//! Compiled, side-effect-free ngspice `numparam` expressions.
//!
//! Source symbols are resolved at model preparation, never while evaluating
//! a device. Lazy conditional evaluation and explicit errors for stochastic
//! functions avoid silently substituting deterministic model parameters.

use crate::model_card::parse_spice_constant;
use anyhow::{Context, Result, bail, ensure};
use std::cmp::Ordering;
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

impl Dialect {
    fn precedence(self, op: &str) -> Option<u8> {
        match op {
            "||" | "&&" | "^" if matches!(self, Self::Ltspice) => Some(2),
            "||" => Some(2),
            "&&" => Some(3),
            "==" | "!=" | "<=" | ">=" | "<" | ">" => Some(4),
            "+" | "-" => Some(5),
            "*" | "/" | "%" | "\\" => Some(6),
            "**" => Some(8),
            _ => None,
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
        if byte.is_ascii_alphabetic() || byte == b'_' {
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
    Unary(&'static str, Box<Self>),
    Binary(&'static str, Box<Self>, Box<Self>),
    Conditional(Box<Self>, Box<Self>, Box<Self>),
    Call(String, Vec<Self>),
}

// Align the source syntax envelope with the native solver. Bound both
// nesting (including right-associative powers) and flat left-deep trees.
const MAX_NESTING: usize = 64;
const MAX_TREE_DEPTH: usize = 4 * MAX_NESTING;
const MAX_TREE_NODES: usize = 100_000;

struct Parsed {
    expr: Expr,
    height: usize,
}

impl Parsed {
    const fn atom(expr: Expr) -> Self {
        Self { expr, height: 1 }
    }

    fn parent(expr: Expr, child_height: usize) -> Result<Self> {
        let height = child_height + 1;
        ensure!(
            height <= MAX_TREE_DEPTH,
            "parameter expression structural depth limit ({MAX_TREE_DEPTH}) exceeded"
        );
        Ok(Self { expr, height })
    }
}

struct Parser {
    tokens: Vec<Token>,
    offset: usize,
    dialect: Dialect,
}
impl Parser {
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
    fn call(&mut self, name: String, depth: usize) -> Result<Parsed> {
        self.take();
        let mut args = Vec::new();
        let mut height = 0;
        if self.peek() != &Token::Close {
            loop {
                let parsed = self.expression(0, depth + 1)?;
                height = height.max(parsed.height);
                args.push(parsed.expr);
                if self.peek() != &Token::Comma {
                    break;
                }
                self.take();
            }
        }
        self.expect(&Token::Close)?;
        Parsed::parent(Expr::Call(name, args), height)
    }

    fn prefix(&mut self, depth: usize) -> Result<Parsed> {
        match self.take() {
            Token::Value(value) => Ok(Parsed::atom(Expr::Number(value))),
            Token::SourceAtom(value) => Ok(Parsed::atom(Expr::SourceAtom(value))),
            Token::Name(name) => {
                if self.peek() == &Token::Open {
                    self.call(name, depth)
                } else {
                    Ok(Parsed::atom(Expr::Symbol(name)))
                }
            }
            Token::Op(op @ ("+" | "-" | "!")) => {
                let binding = if self.dialect == Dialect::Ltspice {
                    9
                } else {
                    8
                };
                let child = self.expression(binding, depth + 1)?;
                Parsed::parent(Expr::Unary(op, Box::new(child.expr)), child.height)
            }
            Token::Open => {
                let value = self.expression(0, depth + 1)?;
                self.expect(&Token::Close)?;
                Ok(value)
            }
            Token::BraceOpen => {
                let value = self.expression(0, depth + 1)?;
                self.expect(&Token::BraceClose)?;
                Parsed::parent(Expr::ParameterGroup(Box::new(value.expr)), value.height)
            }
            other => bail!("expected SPICE expression atom, got {other:?}"),
        }
    }

    fn expression(&mut self, minimum: u8, depth: usize) -> Result<Parsed> {
        ensure!(
            depth <= MAX_NESTING,
            "parameter expression nesting limit ({MAX_NESTING}) exceeded"
        );
        let mut left = self.prefix(depth)?;
        loop {
            if *self.peek() == Token::Question {
                if minimum > 1 {
                    break;
                }
                self.take();
                let then_branch = self.expression(0, depth + 1)?;
                self.expect(&Token::Colon)?;
                let else_branch = self.expression(1, depth + 1)?;
                let height = left.height.max(then_branch.height).max(else_branch.height);
                left = Parsed::parent(
                    Expr::Conditional(
                        Box::new(left.expr),
                        Box::new(then_branch.expr),
                        Box::new(else_branch.expr),
                    ),
                    height,
                )?;
                continue;
            }
            let Token::Op(op) = self.peek() else { break };
            let Some(precedence) = self.dialect.precedence(op) else {
                break;
            };
            if precedence < minimum {
                break;
            }
            let Token::Op(op) = self.take() else {
                unreachable!()
            };
            let right_binding = if self.dialect == Dialect::Ltspice && op == "**" {
                precedence
            } else {
                precedence + 1
            };
            let right = self.expression(right_binding, depth + 1)?;
            let height = left.height.max(right.height);
            left = Parsed::parent(
                Expr::Binary(op, Box::new(left.expr), Box::new(right.expr)),
                height,
            )?;
        }
        Ok(left)
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
        };
        let expr = parser.expression(0, 0)?.expr;
        ensure!(
            parser.peek() == &Token::End,
            "trailing ngspice expression tokens: {:?}",
            parser.peek()
        );
        Ok(Self(expr))
    }

    /// Evaluate constants without requiring occurrence-scoped parameters.
    /// Invalid constant expressions remain errors.
    pub(crate) fn known_value(&self) -> Result<Option<f64>> {
        self.0.known_value()
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
        self.0.evaluate(context)
    }
}

/// Lower `LTspice` operator precedence and Boolean coercion before native
/// function-name lowering. Numeric spellings and circuit probe names remain
/// untouched. The ordinary native evaluator never receives source atoms.
pub(crate) fn lower_ltspice(raw: &str, runtime: bool) -> Result<String> {
    let mut parser = Parser {
        tokens: tokenize(raw, Dialect::Ltspice, runtime)?,
        offset: 0,
        dialect: Dialect::Ltspice,
    };
    let expression = parser.expression(0, 0)?.expr;
    ensure!(
        parser.peek() == &Token::End,
        "trailing LTspice expression tokens: {:?}",
        parser.peek()
    );
    let mut output = String::new();
    expression.render_ltspice(&mut output)?;
    Ok(output)
}

impl Expr {
    fn render_ltspice(&self, out: &mut String) -> Result<()> {
        use std::fmt::Write as _;
        match self {
            Self::Number(value) => write!(out, "{value}")?,
            Self::SourceAtom(value) | Self::Symbol(value) => out.push_str(value),
            Self::ParameterGroup(value) => {
                out.push('{');
                value.render_ltspice(out)?;
                out.push('}');
            }
            Self::Unary(op, child) => {
                if *op == "!" {
                    out.push_str("inv(");
                } else {
                    write!(out, "({op}")?;
                }
                child.render_ltspice(out)?;
                out.push(')');
            }
            Self::Binary(op, lhs, rhs) => {
                match *op {
                    "**" | "^" | "%" => {
                        let function = match *op {
                            "**" => "pow",
                            "^" => "xor",
                            _ => "mod",
                        };
                        write!(out, "{function}(")?;
                        lhs.render_ltspice(out)?;
                        out.push(',');
                        rhs.render_ltspice(out)?;
                    }
                    "&&" | "||" => {
                        out.push_str("((");
                        lhs.render_ltspice(out)?;
                        write!(out, ">0.5){op}(")?;
                        rhs.render_ltspice(out)?;
                        out.push_str(">0.5)");
                    }
                    _ => {
                        out.push('(');
                        lhs.render_ltspice(out)?;
                        out.push_str(op);
                        rhs.render_ltspice(out)?;
                    }
                }
                out.push(')');
            }
            Self::Conditional(condition, yes, no) => {
                out.push_str("if(");
                condition.render_ltspice(out)?;
                out.push(',');
                yes.render_ltspice(out)?;
                out.push(',');
                no.render_ltspice(out)?;
                out.push(')');
            }
            Self::Call(name, arguments) => {
                write!(out, "{name}(")?;
                for (index, argument) in arguments.iter().enumerate() {
                    if index != 0 {
                        out.push(',');
                    }
                    argument.render_ltspice(out)?;
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
const fn bool_value(value: bool) -> f64 {
    if value { 1.0 } else { 0.0 }
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
    fn known_value(&self) -> Result<Option<f64>> {
        match self.evaluate(&mut RequiresBinding) {
            Ok(value) => Ok(Some(value)),
            Err(error) if error.downcast_ref::<RequiresBinding>().is_some() => Ok(None),
            Err(error) => Err(error),
        }
    }
}

impl Expr {
    fn evaluate(&self, resolve: &mut impl SpiceExpressionContext) -> Result<f64> {
        let value = match self {
            Self::Number(value) => *value,
            Self::SourceAtom(_) | Self::ParameterGroup(_) => {
                bail!("source-only expression cannot be evaluated as native numparam")
            }
            Self::Symbol(name) => resolve
                .symbol(name)
                .with_context(|| format!("resolving parameter {name}"))?,
            Self::Unary(op, arg) => {
                let arg = arg.evaluate(resolve)?;
                match *op {
                    "+" => arg,
                    "-" => -arg,
                    "!" => bool_value(!truthy(arg)),
                    _ => unreachable!(),
                }
            }
            Self::Conditional(condition, positive, negative) => {
                let selected = if truthy(condition.evaluate(resolve)?) {
                    positive
                } else {
                    negative
                };
                return selected.evaluate(resolve);
            }
            Self::Binary(op, lhs, rhs) => {
                let l = lhs.evaluate(resolve)?;
                match *op {
                    "&&" if !truthy(l) => return Ok(0.0),
                    "||" if truthy(l) => return Ok(1.0),
                    _ => {}
                }
                let r = rhs.evaluate(resolve)?;
                match *op {
                    "+" => l + r,
                    "-" => l - r,
                    "*" => l * r,
                    "/" => l / r,
                    "%" => l % r,
                    "\\" => (l / r).trunc(),
                    "**" => l.powf(r),
                    "==" => bool_value(l.partial_cmp(&r) == Some(Ordering::Equal)),
                    "!=" => bool_value(l.partial_cmp(&r) != Some(Ordering::Equal)),
                    ">" => bool_value(l > r),
                    "<" => bool_value(l < r),
                    ">=" => bool_value(l >= r),
                    "<=" => bool_value(l <= r),
                    "&&" | "||" => bool_value(truthy(r)),
                    _ => unreachable!(),
                }
            }
            Self::Call(name, args) => {
                if name == "ternary_fcn" {
                    ensure!(args.len() == 3, "ternary_fcn expects 3 arguments");
                    let branch = if truthy(args[0].evaluate(resolve)?) {
                        &args[1]
                    } else {
                        &args[2]
                    };
                    return branch.evaluate(resolve);
                }
                if let Some(builtin) = built_in(name) {
                    let values = args
                        .iter()
                        .map(|arg| arg.evaluate(resolve))
                        .collect::<Result<Vec<_>>>()?;
                    builtin.evaluate(name, &values)?
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

#[derive(Clone, Copy)]
enum BuiltinFunction {
    Unary(fn(f64) -> f64),
    Binary(fn(f64, f64) -> f64),
}
impl BuiltinFunction {
    fn evaluate(self, name: &str, args: &[f64]) -> Result<f64> {
        match self {
            Self::Unary(f) => {
                ensure!(args.len() == 1, "{name} expects one argument");
                finite(f(args[0]))
            }
            Self::Binary(f) => {
                ensure!(args.len() == 2, "{name} expects two arguments");
                finite(f(args[0], args[1]))
            }
        }
    }
}
fn pwr(base: f64, power: f64) -> f64 {
    base.abs().powf(power)
}
fn sgn(value: f64) -> f64 {
    if value.partial_cmp(&0.0) == Some(Ordering::Equal) {
        0.0
    } else {
        value.signum()
    }
}
fn built_in(name: &str) -> Option<BuiltinFunction> {
    let unary: Option<fn(f64) -> f64> = match name {
        "sqrt" => Some(f64::sqrt),
        "sin" => Some(f64::sin),
        "cos" => Some(f64::cos),
        "tan" => Some(f64::tan),
        "sinh" => Some(f64::sinh),
        "cosh" => Some(f64::cosh),
        "tanh" => Some(f64::tanh),
        "asin" => Some(f64::asin),
        "acos" => Some(f64::acos),
        "atan" | "arctan" => Some(f64::atan),
        "asinh" => Some(f64::asinh),
        "acosh" => Some(f64::acosh),
        "atanh" => Some(f64::atanh),
        "exp" => Some(f64::exp),
        "ln" | "log" => Some(f64::ln),
        "log10" => Some(f64::log10),
        "abs" => Some(f64::abs),
        "nint" => Some(f64::round_ties_even),
        "int" => Some(f64::trunc),
        "floor" => Some(f64::floor),
        "ceil" => Some(f64::ceil),
        "sgn" => Some(sgn),
        _ => None,
    };
    if let Some(f) = unary {
        return Some(BuiltinFunction::Unary(f));
    }
    let binary: Option<fn(f64, f64) -> f64> = match name {
        "min" => Some(f64::min),
        "max" => Some(f64::max),
        "pow" => Some(f64::powf),
        "pwr" => Some(pwr),
        _ => None,
    };
    binary.map(BuiltinFunction::Binary)
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
    fn ngspice_precedence_literals_logic_and_lazy_branches() -> Result<()> {
        for (input, expected) in [
            ("{2**3^2}", 64.0),
            ("-2^2", -4.0),
            ("2^-2", 0.25),
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
