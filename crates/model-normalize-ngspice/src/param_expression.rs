//! Compiled, side-effect-free ngspice `numparam` expressions.
//!
//! Source symbols are resolved at model preparation, never while evaluating
//! a device. Lazy conditional evaluation and explicit errors for stochastic
//! functions avoid silently substituting deterministic model parameters.

use crate::model_card::{identifier_start, parse_spice_constant};
use crate::native_scalar::{BinaryOp, ConditionalChain, Function, UnaryOp};
use anyhow::{Context, Result, bail, ensure};
use model_preprocess_command::{Resource, WorkBudget};
mod rational;
pub(crate) use rational::coefficients as rational_coefficients;

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Value(f64),
    SourceAtom(String),
    Name(String),
    Op(&'static str),
    Open,
    Close,
    BraceOpen,
    BraceClose,
    Comma,
    Question,
    Colon,
    End,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Dialect {
    Native,
    Ltspice,
}

/// Source operators reuse the native spelling type for shared syntax, not
/// its evaluation or associativity. `LTspice`'s caret denotes XOR, not power.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SourceBinaryOp {
    Shared(BinaryOp),
    Xor,
}

impl SourceBinaryOp {
    fn from_symbol(symbol: &str) -> Option<Self> {
        if symbol == "^" {
            Some(Self::Xor)
        } else {
            BinaryOp::from_symbol(symbol).map(Self::Shared)
        }
    }

    const fn precedence(self) -> u8 {
        match self {
            Self::Xor | Self::Shared(BinaryOp::And | BinaryOp::Or) => 2,
            Self::Shared(op) => op.precedence(),
        }
    }
}

/// A parsed binary operator with its syntax domain attached.
#[derive(Clone, Copy)]
enum ParsedBinaryOp {
    Native(BinaryOp),
    Source(SourceBinaryOp),
}

impl ParsedBinaryOp {
    fn from_symbol(dialect: Dialect, symbol: &str) -> Option<Self> {
        match dialect {
            Dialect::Native => BinaryOp::from_symbol(symbol).map(Self::Native),
            Dialect::Ltspice => SourceBinaryOp::from_symbol(symbol).map(Self::Source),
        }
    }

    const fn binding(self) -> (u8, u8) {
        let left = match self {
            Self::Native(op) => op.precedence(),
            Self::Source(op) => op.precedence(),
        };
        let right = if matches!(self, Self::Source(SourceBinaryOp::Shared(BinaryOp::Pow))) {
            left
        } else {
            left + 1
        };
        (left, right)
    }

    const fn expression(self, lhs: Box<Expr>, rhs: Box<Expr>) -> Expr {
        match self {
            Self::Native(op) => Expr::Binary(op, lhs, rhs),
            Self::Source(op) => Expr::SourceBinary(op, lhs, rhs),
        }
    }
}

fn source_probe(raw: &str, name_end: usize) -> Result<Option<usize>> {
    let tail = raw[name_end..].trim_start();
    if !tail.starts_with('(') {
        return Ok(None);
    }
    let end = tail
        .find(')')
        .context("unterminated LTspice circuit probe")?;
    let body = &tail[1..end];
    ensure!(!body.contains('('), "nested circuit probe identifier");
    Ok(Some(raw.len() - tail.len() + end + 1))
}

fn number_end(bytes: &[u8], mut cursor: usize) -> usize {
    if bytes[cursor] == b'.' {
        cursor += 1;
    }
    while bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
        cursor += 1;
    }
    if bytes.get(cursor) == Some(&b'.') {
        cursor += 1;
        while bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
            cursor += 1;
        }
    }
    if matches!(bytes.get(cursor), Some(b'e' | b'E' | b'd' | b'D')) {
        let exponent = cursor + 1;
        let offset = exponent + usize::from(matches!(bytes.get(exponent), Some(b'+' | b'-')));
        if bytes.get(offset).is_some_and(u8::is_ascii_digit) {
            cursor = offset + 1;
            while bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
                cursor += 1;
            }
        }
    }
    while bytes.get(cursor).is_some_and(u8::is_ascii_alphabetic) {
        cursor += 1;
    }
    cursor
}

fn name_token(raw: &str, start: usize, dialect: Dialect) -> Result<(Token, usize)> {
    let mut cursor = start + 1;
    while raw.as_bytes().get(cursor).is_some_and(|b| {
        b.is_ascii_alphanumeric() || matches!(*b, b'_' | b'#' | b'$' | b'[' | b']')
    }) {
        cursor += 1;
    }
    let name = &raw[start..cursor];
    if dialect == Dialect::Ltspice {
        if (name.eq_ignore_ascii_case("v") || name.eq_ignore_ascii_case("i"))
            && let Some(end) = source_probe(raw, cursor)?
        {
            return Ok((Token::SourceAtom(raw[start..end].to_owned()), end));
        }
        return Ok((Token::Name(name.to_ascii_lowercase()), cursor));
    }
    let token = match name.to_ascii_lowercase().as_str() {
        "not" => Token::Op("!"),
        "and" => Token::Op("&&"),
        "or" => Token::Op("||"),
        "mod" => Token::Op("%"),
        "div" => Token::Op("\\"),
        _ => Token::Name(name.to_ascii_lowercase()),
    };
    Ok((token, cursor))
}

/// Runtime SPICE numeric literals use percent as an engineering suffix;
/// the parameter language instead recognizes a remainder operator.
fn percent_literal(raw: &str, start: usize, percent: usize) -> Result<(String, usize)> {
    let bytes = raw.as_bytes();
    let whole = &raw[start..percent];
    let mut end = percent + 1;
    while bytes.get(end).is_some_and(u8::is_ascii_digit) {
        end += 1;
    }
    let fraction = &raw[percent + 1..end];
    let number = if fraction.is_empty() {
        while bytes.get(end).is_some_and(u8::is_ascii_alphabetic) {
            end += 1;
        }
        format!("{:.17e}", parse_spice_constant(whole)? * 0.01)
    } else {
        ensure!(
            whole.bytes().all(|byte| byte.is_ascii_digit()),
            "fractional percent suffix requires an integer prefix"
        );
        format!("{whole}.{fraction}e-2")
    };
    Ok((number, end))
}

fn single_token(
    byte: u8,
    dialect: Dialect,
    runtime: bool,
    brace_depth: &mut usize,
) -> Result<Token> {
    Ok(match byte {
        b'(' => Token::Open,
        b')' => Token::Close,
        b'{' if dialect == Dialect::Ltspice => {
            *brace_depth += 1;
            Token::BraceOpen
        }
        b'}' if dialect == Dialect::Ltspice => {
            *brace_depth = brace_depth
                .checked_sub(1)
                .context("unmatched parameter brace")?;
            Token::BraceClose
        }
        b',' => Token::Comma,
        b'?' => Token::Question,
        b':' => Token::Colon,
        b'+' => Token::Op("+"),
        b'-' => Token::Op("-"),
        b'*' => Token::Op("*"),
        b'/' => Token::Op("/"),
        b'%' if dialect == Dialect::Ltspice && runtime && *brace_depth == 0 => {
            bail!("runtime LTspice percent suffix must follow a numeric literal")
        }
        b'%' => Token::Op("%"),
        b'\\' => Token::Op("\\"),
        b'^' if dialect == Dialect::Ltspice => Token::Op("^"),
        b'&' if dialect == Dialect::Ltspice => Token::Op("&&"),
        b'|' if dialect == Dialect::Ltspice => Token::Op("||"),
        b'~' if dialect == Dialect::Ltspice => Token::Op("!"),
        b'^' => Token::Op("**"),
        b'!' => Token::Op("!"),
        b'<' => Token::Op("<"),
        b'>' => Token::Op(">"),
        b'=' if dialect == Dialect::Native => Token::Op("="),
        _ => bail!(
            "unsupported ngspice expression character {:?}",
            char::from(byte)
        ),
    })
}

fn tokenize(raw: &str, dialect: Dialect, runtime: bool) -> Result<Vec<Token>> {
    ensure!(raw.is_ascii(), "non-ASCII ngspice parameter expression");
    let bytes = raw.as_bytes();
    let mut cursor = 0;
    let mut tokens = Vec::new();
    let mut brace_depth = 0_usize;
    while let Some(&byte) = bytes.get(cursor) {
        ensure!(
            tokens.len() < MAX_TREE_NODES,
            "parameter expression token budget ({MAX_TREE_NODES}) exceeded"
        );
        if byte.is_ascii_whitespace() {
            cursor += 1;
            continue;
        }
        let start = cursor;
        if byte.is_ascii_digit()
            || (byte == b'.' && bytes.get(cursor + 1).is_some_and(u8::is_ascii_digit))
        {
            cursor = number_end(bytes, cursor);
            if dialect == Dialect::Ltspice
                && runtime
                && brace_depth == 0
                && bytes.get(cursor) == Some(&b'%')
            {
                let (number, end) = percent_literal(raw, start, cursor)?;
                tokens.push(Token::SourceAtom(number));
                cursor = end;
                continue;
            }
            let literal = &raw[start..cursor];
            let value = parse_spice_constant(literal)?;
            tokens.push(if dialect == Dialect::Ltspice {
                Token::SourceAtom(literal.to_owned())
            } else {
                Token::Value(value)
            });
            continue;
        }
        if identifier_start(byte) {
            let (token, end) = name_token(raw, cursor, dialect)?;
            cursor = end;
            tokens.push(token);
            continue;
        }
        let op = match bytes.get(cursor..cursor.saturating_add(2)) {
            Some(b"**") => Some("**"),
            Some(b"==") => Some("=="),
            Some(b"!=" | b"<>") => Some("!="),
            Some(b"<=") => Some("<="),
            Some(b">=") => Some(">="),
            Some(b"&&") => Some("&&"),
            Some(b"||") => Some("||"),
            _ => None,
        };
        if let Some(op) = op {
            cursor += 2;
            tokens.push(Token::Op(op));
            continue;
        }
        cursor += 1;
        tokens.push(single_token(byte, dialect, runtime, &mut brace_depth)?);
    }
    ensure!(
        tokens.len() <= MAX_TREE_NODES,
        "parameter expression token budget ({MAX_TREE_NODES}) exceeded"
    );
    tokens.push(Token::End);
    Ok(tokens)
}

/// External parameter resolution context.
///
/// Source functions receive unevaluated argument syntax trees to preserve
/// lazy conditional behavior and lexical caller context.
pub trait SpiceExpressionContext {
    /// Resolve a lexical source symbol into a scalar value.
    ///
    /// # Errors
    ///
    /// Rejects undefined symbols or invalid scalar values.
    fn symbol(&mut self, name: &str) -> Result<f64>;

    /// Resolve a source-defined function at model preparation time.
    ///
    /// # Errors
    ///
    /// The default rejects unknown functions; source linkers can override it.
    fn function(&mut self, name: &str, _args: &[SpiceExpression]) -> Result<f64> {
        bail!("unsupported ngspice parameter function {name}")
    }
}

impl<F: FnMut(&str) -> Result<f64>> SpiceExpressionContext for F {
    fn symbol(&mut self, name: &str) -> Result<f64> {
        self(name)
    }
}

/// Immutable syntax tree compiled once from an ngspice front-end scalar expression.
#[derive(Clone, Debug, PartialEq)]
pub struct SpiceExpression(Expr);

#[derive(Clone, Debug, PartialEq)]
enum Expr {
    Number(f64),
    SourceAtom(String),
    ParameterGroup(Box<Self>),
    Symbol(String),
    Unary(UnaryOp, Box<Self>),
    Binary(BinaryOp, Box<Self>, Box<Self>),
    SourceBinary(SourceBinaryOp, Box<Self>, Box<Self>),
    Conditional(Box<Self>, Box<Self>, Box<Self>),
    Call(String, Vec<Self>),
}

const MAX_TREE_NODES: usize = 100_000;

struct Parsed {
    expr: Expr,
    height: usize,
}

struct Parser<'a> {
    tokens: Vec<Token>,
    offset: usize,
    dialect: Dialect,
    budget: &'a WorkBudget,
}

impl Parser<'_> {
    fn take(&mut self) -> Token {
        let token = self.tokens[self.offset].clone();
        if token != Token::End {
            self.offset += 1;
        }
        token
    }
    fn peek(&self) -> &Token {
        &self.tokens[self.offset]
    }
    fn expect(&mut self, required: &Token) -> Result<()> {
        let got = self.take();
        ensure!(
            &got == required,
            "expected {required:?} in parameter expression, got {got:?}"
        );
        Ok(())
    }
    fn node(&self, expr: Expr, height: usize) -> Result<Parsed> {
        self.budget.check_depth(height)?;
        self.budget.charge(Resource::GeneratedNodes, 1)?;
        Ok(Parsed { expr, height })
    }
    fn call(&mut self, name: String) -> Result<Parsed> {
        self.take();
        let mut args = Vec::new();
        let mut height = 0;
        if self.peek() != &Token::Close {
            loop {
                let parsed = self.expression(0)?;
                height = height.max(parsed.height);
                args.push(parsed.expr);
                if self.peek() != &Token::Comma {
                    break;
                }
                self.take();
            }
        }
        self.expect(&Token::Close)?;
        self.node(Expr::Call(name, args), height + 1)
    }

    fn prefix(&mut self, minimum: u8) -> Result<Parsed> {
        match self.take() {
            Token::Value(value) => self.node(Expr::Number(value), 1),
            Token::SourceAtom(value) => self.node(Expr::SourceAtom(value), 1),
            Token::Name(name) => {
                if self.peek() == &Token::Open {
                    self.call(name)
                } else {
                    self.node(Expr::Symbol(name), 1)
                }
            }
            Token::Op(symbol @ ("+" | "-" | "!")) => {
                let op = UnaryOp::from_symbol(symbol).context("invalid prefix operator")?;
                // Leading native signs bind below products, while signs
                // following an operator bind atomically. Source signs always
                // bind above power; source NOT is lowered rather than evaluated.
                let binding = match (self.dialect, op, minimum) {
                    (Dialect::Native, UnaryOp::Plus | UnaryOp::Minus, 0) => {
                        BinaryOp::Add.precedence() + 1
                    }
                    _ => BinaryOp::Pow.precedence() + 1,
                };
                let child = self.expression(binding)?;
                self.node(Expr::Unary(op, Box::new(child.expr)), child.height + 1)
            }
            Token::Open => {
                let value = self.expression(0)?;
                self.expect(&Token::Close)?;
                Ok(value)
            }
            Token::BraceOpen => {
                let value = self.expression(0)?;
                self.expect(&Token::BraceClose)?;
                self.node(Expr::ParameterGroup(Box::new(value.expr)), value.height + 1)
            }
            other => bail!("expected SPICE expression atom, got {other:?}"),
        }
    }

    fn expression(&mut self, minimum: u8) -> Result<Parsed> {
        let _depth = self.budget.enter()?;
        let mut left = self.prefix(minimum)?;
        loop {
            if *self.peek() == Token::Question {
                if minimum > 1 {
                    break;
                }
                self.take();
                if self.dialect == Dialect::Native {
                    return self.conditional(left);
                }
                let then_branch = self.expression(0)?;
                self.expect(&Token::Colon)?;
                let else_branch = self.expression(1)?;
                let height = left.height.max(then_branch.height).max(else_branch.height) + 1;
                left = self.node(
                    Expr::Conditional(
                        Box::new(left.expr),
                        Box::new(then_branch.expr),
                        Box::new(else_branch.expr),
                    ),
                    height,
                )?;
                continue;
            }
            let Token::Op(symbol) = self.peek() else {
                break;
            };
            let Some(op) = ParsedBinaryOp::from_symbol(self.dialect, symbol) else {
                break;
            };
            let (precedence, right_binding) = op.binding();
            if precedence < minimum {
                break;
            }
            self.take();
            let right = self.expression(right_binding)?;
            let height = left.height.max(right.height) + 1;
            left = self.node(
                op.expression(Box::new(left.expr), Box::new(right.expr)),
                height,
            )?;
        }
        Ok(left)
    }
    fn conditional(&mut self, condition: Parsed) -> Result<Parsed> {
        let mut chain = ConditionalChain::new(condition, ());
        loop {
            let operand = self.expression(2)?;
            match self.peek() {
                Token::Question => {
                    self.take();
                    chain.question(operand, ());
                }
                Token::Colon => {
                    self.take();
                    chain
                        .colon(operand)
                        .map_err(|_| anyhow::anyhow!("unmatched ':'"))?;
                }
                _ => {
                    return chain.finish(
                        operand,
                        |_| anyhow::anyhow!("unmatched '?'"),
                        |condition, positive, negative, ()| {
                            let height =
                                condition.height.max(positive.height).max(negative.height) + 1;
                            self.node(
                                Expr::Conditional(
                                    Box::new(condition.expr),
                                    Box::new(positive.expr),
                                    Box::new(negative.expr),
                                ),
                                height,
                            )
                        },
                    );
                }
            }
        }
    }
}

impl SpiceExpression {
    /// Compile a scalar expression; single wrapping braces or apostrophes are
    /// syntax-level quoting, and have no effect on mathematical precedence.
    ///
    /// # Errors
    ///
    /// Rejects invalid numbers, unknown tokens, mismatched delimiters and
    /// incomplete expressions before any symbol evaluation.
    pub fn parse(raw: &str) -> Result<Self> {
        Self::parse_with_budget(raw, &WorkBudget::default())
    }

    /// Compile while sharing the caller's cumulative work counters.
    /// # Errors
    /// Malformed syntax, cancellation, or exhausted work budget.
    pub fn parse_with_budget(raw: &str, budget: &WorkBudget) -> Result<Self> {
        budget.charge(Resource::EvaluationSteps, raw.len().max(1))?;
        let raw = raw.trim();
        let inner = if (raw.starts_with('{') && raw.ends_with('}'))
            || (raw.starts_with('\'') && raw.ends_with('\''))
        {
            &raw[1..raw.len() - 1]
        } else {
            raw
        };
        let mut parser = Parser {
            tokens: tokenize(inner, Dialect::Native, false)?,
            offset: 0,
            dialect: Dialect::Native,
            budget,
        };
        let expr = parser.expression(0)?.expr;
        ensure!(
            parser.peek() == &Token::End,
            "trailing ngspice expression tokens: {:?}",
            parser.peek()
        );
        Ok(Self(expr))
    }

    /// Evaluate constants without requiring occurrence-scoped parameters.
    /// Invalid constant expressions remain errors.
    pub(crate) fn known_value(&self, budget: &WorkBudget) -> Result<Option<f64>> {
        self.0.known_value(budget)
    }

    /// Evaluate a compiled expression against the provided symbol resolver.
    /// The callback is never asked to resolve a symbol in an unselected lazy
    /// branch (`? :`, `&&`, `||` or `ternary_fcn`).
    ///
    /// # Errors
    ///
    /// Rejects undefined symbols, unsupported functions, non-finite arithmetic
    /// and invalid real-valued function domains.
    pub fn evaluate(&self, mut resolve: impl FnMut(&str) -> Result<f64>) -> Result<f64> {
        self.evaluate_with(&mut resolve)
    }

    /// Evaluate with both scalar symbols and source-defined functions.
    ///
    /// # Errors
    ///
    /// Rejects unresolved names/functions and invalid expressions.
    pub fn evaluate_with(&self, context: &mut impl SpiceExpressionContext) -> Result<f64> {
        self.evaluate_with_budget(context, &WorkBudget::default())
    }

    /// Evaluate while sharing the caller's cumulative work counters.
    /// # Errors
    /// Invalid evaluation, cancellation, or exhausted work budget.
    pub fn evaluate_with_budget(
        &self,
        context: &mut impl SpiceExpressionContext,
        budget: &WorkBudget,
    ) -> Result<f64> {
        self.0.evaluate(context, budget)
    }
}

/// Lower `LTspice` operator precedence and Boolean coercion before native
/// function-name lowering. Numeric spellings and circuit probe names remain
/// untouched. The ordinary native evaluator never receives source atoms.
pub(crate) fn lower_ltspice(raw: &str, runtime: bool, budget: &WorkBudget) -> Result<String> {
    budget.charge(Resource::EvaluationSteps, raw.len().max(1))?;
    let mut parser = Parser {
        tokens: tokenize(raw, Dialect::Ltspice, runtime)?,
        offset: 0,
        dialect: Dialect::Ltspice,
        budget,
    };
    let expression = parser.expression(0)?.expr;
    ensure!(
        parser.peek() == &Token::End,
        "trailing LTspice expression tokens: {:?}",
        parser.peek()
    );
    let mut output = String::new();
    expression.render_ltspice(&mut output, budget)?;
    Ok(output)
}

impl Expr {
    fn render_ltspice(&self, out: &mut String, budget: &WorkBudget) -> Result<()> {
        use std::fmt::Write as _;
        let _depth = budget.enter()?;
        budget.charge(Resource::EvaluationSteps, 1)?;
        budget.charge(
            Resource::EmittedBytes,
            match self {
                Self::SourceAtom(value) | Self::Symbol(value) => value.len(),
                Self::Call(name, args) => name.len().saturating_add(args.len()).saturating_add(2),
                _ => 32,
            },
        )?;
        match self {
            Self::Number(value) => write!(out, "{value}")?,
            Self::SourceAtom(value) | Self::Symbol(value) => out.push_str(value),
            Self::ParameterGroup(value) => {
                out.push('{');
                value.render_ltspice(out, budget)?;
                out.push('}');
            }
            Self::Unary(op, child) => {
                if *op == UnaryOp::Not {
                    out.push_str("inv(");
                } else {
                    write!(out, "({}", op.symbol())?;
                }
                child.render_ltspice(out, budget)?;
                out.push(')');
            }
            Self::SourceBinary(op, lhs, rhs) => {
                match op {
                    SourceBinaryOp::Xor | SourceBinaryOp::Shared(BinaryOp::Pow | BinaryOp::Rem) => {
                        let function = match op {
                            SourceBinaryOp::Shared(BinaryOp::Pow) => "pow",
                            SourceBinaryOp::Xor => "xor",
                            SourceBinaryOp::Shared(_) => "mod",
                        };
                        write!(out, "{function}(")?;
                        lhs.render_ltspice(out, budget)?;
                        out.push(',');
                        rhs.render_ltspice(out, budget)?;
                    }
                    SourceBinaryOp::Shared(op @ (BinaryOp::And | BinaryOp::Or)) => {
                        out.push_str("((");
                        lhs.render_ltspice(out, budget)?;
                        write!(out, ">0.5){}(", op.symbol())?;
                        rhs.render_ltspice(out, budget)?;
                        out.push_str(">0.5)");
                    }
                    SourceBinaryOp::Shared(op) => {
                        out.push('(');
                        lhs.render_ltspice(out, budget)?;
                        out.push_str(op.symbol());
                        rhs.render_ltspice(out, budget)?;
                    }
                }
                out.push(')');
            }
            Self::Binary(_, _, _) => bail!("native expression cannot be lowered as LTspice source"),
            Self::Conditional(condition, yes, no) => {
                out.push_str("if(");
                condition.render_ltspice(out, budget)?;
                out.push(',');
                yes.render_ltspice(out, budget)?;
                out.push(',');
                no.render_ltspice(out, budget)?;
                out.push(')');
            }
            Self::Call(name, arguments) => {
                write!(out, "{name}(")?;
                for (index, argument) in arguments.iter().enumerate() {
                    if index != 0 {
                        out.push(',');
                    }
                    argument.render_ltspice(out, budget)?;
                }
                out.push(')');
            }
        }
        Ok(())
    }
}

fn truthy(value: f64) -> bool {
    value != 0.0
}
fn finite(value: f64) -> Result<f64> {
    ensure!(value.is_finite(), "non-finite ngspice expression result");
    Ok(value)
}

#[derive(Debug)]
struct RequiresBinding;

impl std::fmt::Display for RequiresBinding {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("expression requires occurrence binding")
    }
}

impl std::error::Error for RequiresBinding {}

impl SpiceExpressionContext for RequiresBinding {
    fn symbol(&mut self, _name: &str) -> Result<f64> {
        Err(Self.into())
    }

    fn function(&mut self, _name: &str, _args: &[SpiceExpression]) -> Result<f64> {
        Err(Self.into())
    }
}

impl Expr {
    fn known_value(&self, budget: &WorkBudget) -> Result<Option<f64>> {
        match self.evaluate(&mut RequiresBinding, budget) {
            Ok(value) => Ok(Some(value)),
            Err(error) if error.downcast_ref::<RequiresBinding>().is_some() => Ok(None),
            Err(error) => Err(error),
        }
    }
}

impl Expr {
    fn evaluate(
        &self,
        resolve: &mut impl SpiceExpressionContext,
        budget: &WorkBudget,
    ) -> Result<f64> {
        let _depth = budget.enter()?;
        budget.charge(Resource::EvaluationSteps, 1)?;
        let value = match self {
            Self::Number(value) => *value,
            Self::SourceAtom(_) | Self::ParameterGroup(_) | Self::SourceBinary(_, _, _) => {
                bail!("source-only expression cannot be evaluated as native numparam")
            }
            Self::Symbol(name) => resolve
                .symbol(name)
                .with_context(|| format!("resolving parameter {name}"))?,
            Self::Unary(op, arg) => op.evaluate(arg.evaluate(resolve, budget)?),
            Self::Conditional(condition, positive, negative) => {
                let selected = if truthy(condition.evaluate(resolve, budget)?) {
                    positive
                } else {
                    negative
                };
                return selected.evaluate(resolve, budget);
            }
            Self::Binary(op, lhs, rhs) => {
                let l = lhs.evaluate(resolve, budget)?;
                if let Some(value) = op.short_circuit(l) {
                    return Ok(value);
                }
                op.evaluate(l, rhs.evaluate(resolve, budget)?)
            }
            Self::Call(name, args) => {
                if name == "ternary_fcn" {
                    ensure!(args.len() == 3, "ternary_fcn expects 3 arguments");
                    let branch = if truthy(args[0].evaluate(resolve, budget)?) {
                        &args[1]
                    } else {
                        &args[2]
                    };
                    return branch.evaluate(resolve, budget);
                }
                if let Some(builtin) = Function::from_name(name) {
                    let values = args
                        .iter()
                        .map(|arg| arg.evaluate(resolve, budget))
                        .collect::<Result<Vec<_>>>()?;
                    builtin
                        .evaluate(&values)
                        .with_context(|| format!("{name} expects {} arguments", builtin.arity()))?
                } else {
                    let source_args = args
                        .iter()
                        .cloned()
                        .map(SpiceExpression)
                        .collect::<Vec<_>>();
                    resolve.function(name, &source_args)?
                }
            }
        };
        finite(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn evaluate(text: &str) -> Result<f64> {
        SpiceExpression::parse(text)?.evaluate(|name| match name {
            "a" => Ok(3.0),
            "b" => Ok(5.0),
            _ => bail!("undefined {name}"),
        })
    }

    #[test]
    fn expression_depth_and_cumulative_steps_are_bounded() -> Result<()> {
        use model_preprocess_command::{Cancellation, WorkLimits};
        for expression in [
            format!("{}1{}", "(".repeat(1000), ")".repeat(1000)),
            format!("{}1", "1+".repeat(1000)),
            format!("{}1", "-".repeat(1000)),
        ] {
            let error = SpiceExpression::parse(&expression)
                .err()
                .context("unbounded expression accepted")?;
            ensure!(format!("{error:#}").contains("depth budget exceeded"));
        }
        let budget = WorkBudget::new(
            WorkLimits {
                evaluation_steps: 12,
                ..WorkLimits::default()
            },
            Cancellation::default(),
        );
        let expression = SpiceExpression::parse_with_budget("1+2", &budget)?;
        for _ in 0..3 {
            let value = expression.evaluate_with_budget(&mut |_: &str| Ok(0.0), &budget)?;
            ensure!((value - 3.0).abs() < f64::EPSILON);
        }
        ensure!(
            expression
                .evaluate_with_budget(&mut |_: &str| Ok(0.0), &budget)
                .is_err()
        );
        ensure!(budget.check().is_err());
        Ok(())
    }

    #[test]
    fn ngspice_precedence_literals_logic_and_lazy_branches() -> Result<()> {
        for (input, expected) in [
            ("{2**3^2}", 64.0),
            ("-2^2", -4.0),
            ("2^-2", 0.25),
            ("(-2)^3", 8.0),
            ("pow(-2,3)", -8.0),
            ("0^0", 1.0),
            ("2==3<1", 1.0),
            ("1=1", 1.0),
            ("!2^0", 1.0),
            ("-2*3^2", -18.0),
            ("2*-3^2", 18.0),
            ("1?0?3:4:5", 3.0),
            ("1?(0?3:4):5", 4.0),
            ("0?3:1?4:5", 4.0),
            ("0?3:0?4:5", 5.0),
            ("1?3:missing?missing:missing", 3.0),
            ("sqr(4)", 16.0),
            ("(-8)%3", -2.0),
            ("(-8)\\3", 2.0),
            ("{2 * 3 + 4}", 10.0),
            ("{a>b ? 7 : min(a,b)}", 3.0),
            ("{(a<b) ? 3 : 1/0}", 3.0),
            ("{(b>a) && (2==2)}", 1.0),
            ("{0 && missing}", 0.0),
            ("{1 || missing}", 1.0),
            ("{8\\3}", 2.0),
            ("{8%3}", 2.0),
            ("{ternary_fcn(1,1k,missing)}", 1000.0),
            ("{3.2d-2u}", 3.2e-8),
            ("-2MEG", -2e6),
            ("2.5MIL", 63.5e-6),
        ] {
            let actual = evaluate(input)?;
            ensure!(
                (actual - expected).abs() <= expected.abs().max(1e-14) * 1e-12,
                "{input}: actual {actual:e} expected {expected:e}"
            );
        }
        Ok(())
    }

    #[test]
    fn explicit_prepared_power_has_a_separate_scalar_phase() -> Result<()> {
        for (input, expected) in [
            ("simetrix_abs_pow(-2,3)", 8.),
            ("simetrix_abs_pow(0,0)", 1.),
            ("simetrix_abs_pow(0,0.5)", 1e-154),
            ("simetrix_abs_pow(2,-3)", 0.125),
            ("simetrix_caret_pow(-2,3)", -8.),
            ("simetrix_caret_pow(simetrix_abs_pow(2,3),2)", 64.),
            ("simetrix_pow(-2,3)", -8.),
            ("simetrix_abs_pow_product(0,0,-1)", 0.),
            ("simetrix_abs_pow_product(2,-2,3)", 16.),
            ("(-2)^3", 8.),
            ("0^0.5", 0.),
        ] {
            let actual = evaluate(input)?;
            ensure!(
                (actual - expected).abs() <= expected.abs() * 1e-12,
                "{input}: {actual:e} != {expected:e}"
            );
        }
        assert!(evaluate("simetrix_abs_pow(0,-1)").is_err());
        assert!(evaluate("simetrix_caret_pow(-2,0.5)").is_err());
        Ok(())
    }

    #[test]
    fn native_and_ltspice_operator_domains_remain_distinct() -> Result<()> {
        let budget = WorkBudget::default();
        for (input, lowered) in [
            ("2**3**2", "pow(2,pow(3,2))"),
            ("2^3", "xor(2,3)"),
            ("-2**2", "pow((-2),2)"),
            ("1?0?3:4:5", "if(1,if(0,3,4),5)"),
            ("1||0&&0", "((((1>0.5)||(0>0.5))>0.5)&&(0>0.5))"),
        ] {
            let actual = lower_ltspice(input, false, &budget)?;
            ensure!(actual == lowered, "{input}: {actual} != {lowered}");
        }
        ensure!(evaluate("2^3^2")?.to_bits() == 64_f64.to_bits());
        ensure!(evaluate("1?0?3:4:5")?.to_bits() == 3_f64.to_bits());
        ensure!(evaluate("1||0&&0")?.to_bits() == 1_f64.to_bits());
        Ok(())
    }

    #[test]
    fn native_scalar_semantics_match_ngspice() -> Result<()> {
        use std::fmt::Write as _;
        use std::process::Command;

        let cases = [
            "(-2)^3",
            "pow(-2,3)",
            "0^0",
            "2==3<1",
            "1=1",
            "!2^0",
            "-2*3^2",
            "2*-3^2",
            "1?0?3:4:5",
            "1?(0?3:4):5",
            "0?3:1?4:5",
            "0?3:0?4:5",
            "sqr(4)",
            "(-8)%3",
            "(-8)\\3",
            "nint(2.5)",
            "nint(3.5)",
            "floor(-1.2)",
            "ceil(-1.2)",
            "pwr(-2,3)",
            "log(2)",
        ];
        let directory = tempfile::tempdir()?;
        let circuit = directory.path().join("native-scalar.cir");
        let mut deck = String::from("Native scalar comparison\n");
        for (index, expression) in cases.iter().enumerate() {
            writeln!(deck, "V{index} scalar_{index} 0 {{{expression}}}")?;
        }
        deck.push_str(".control\nset numdgt=16\nop\n");
        for index in 0..cases.len() {
            writeln!(deck, "print v(scalar_{index})")?;
        }
        deck.push_str("quit\n.endc\n.end\n");
        std::fs::write(&circuit, deck)?;
        let execution = Command::new("ngspice")
            .args(["-n", "-b"])
            .arg(&circuit)
            .current_dir(directory.path())
            .output()?;
        ensure!(
            execution.status.success(),
            "ngspice failed: {}",
            String::from_utf8_lossy(&execution.stderr)
        );
        let output = String::from_utf8(execution.stdout)?;
        for (index, expression) in cases.iter().enumerate() {
            let label = format!("v(scalar_{index})");
            let expected: f64 = output
                .lines()
                .filter_map(|line| line.split_once('='))
                .find(|(name, _)| name.trim() == label)
                .with_context(|| format!("missing {label} in {output}"))?
                .1
                .trim()
                .parse()?;
            let actual = evaluate(expression)?;
            ensure!(
                (actual - expected).abs() <= expected.abs().max(1.0) * 1e-12,
                "{expression}: actual {actual:e}, ngspice {expected:e}"
            );
        }
        Ok(())
    }

    #[test]
    fn full_frontend_function_grammar_and_errors() -> Result<()> {
        for (input, expected) in [
            ("{sin(acos(0))*4 + 2**3^2}", 68.0),
            ("{max(3,5) + min(2,4)+abs(-3)}", 10.0),
            ("{sqrt(9)+pwr(-2,3)+pow(-2,3)}", 3.0),
            (
                "{nint(2.5)+nint(3.5)+int(-1.9)+floor(-1.2)+ceil(-1.2)}",
                2.0,
            ),
            ("{sgn(0)+sgn(-2)+sgn(3)}", 0.0),
        ] {
            let actual = evaluate(input)?;
            ensure!(
                (actual - expected).abs() < 1e-10,
                "{input}: {actual} != {expected}"
            );
        }
        for invalid in [
            "",
            "{1/0}",
            "{sqrt(-1)}",
            "{unknown+1}",
            "{gauss(1,1,2)}",
            "{if(1,2,3)}",
            "{1 +}",
            "{1 2}",
            "{1/}",
            "{a ? b}",
            "{min(1)}",
            "{2e}",
        ] {
            ensure!(evaluate(invalid).is_err(), "accepted unsupported {invalid}");
        }
        Ok(())
    }
}
