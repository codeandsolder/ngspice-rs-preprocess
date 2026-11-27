//! Compiled, side-effect-free ngspice `numparam` expressions.
//!
//! Source symbols are resolved at model preparation, never while evaluating
//! a device. Lazy conditional evaluation and explicit errors for stochastic
//! functions avoid silently substituting deterministic model parameters.

use crate::model_card::parse_spice_constant;
use crate::native_scalar::{BinaryOp, ConditionalChain, Function, UnaryOp};
use anyhow::{Context, Result, bail, ensure};
use model_preprocess_command::{Resource, WorkBudget};
mod rational;
pub(crate) use rational::coefficients as rational_coefficients;

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Value(f64),
    Name(String),
    Op(&'static str),
    Open,
    Close,
    Comma,
    Question,
    Colon,
    End,
}

fn number_end(bytes: &[u8], byte: u8, cursor: &mut usize) {
    if byte == b'.' {
        *cursor += 1;
    }
    while bytes.get(*cursor).is_some_and(u8::is_ascii_digit) {
        *cursor += 1;
    }
    if bytes.get(*cursor) == Some(&b'.') {
        *cursor += 1;
        while bytes.get(*cursor).is_some_and(u8::is_ascii_digit) {
            *cursor += 1;
        }
    }
    if matches!(bytes.get(*cursor), Some(b'e' | b'E' | b'd' | b'D')) {
        let exponent = *cursor + 1;
        let offset = exponent + usize::from(matches!(bytes.get(exponent), Some(b'+' | b'-')));
        if bytes.get(offset).is_some_and(u8::is_ascii_digit) {
            *cursor = offset + 1;
            while bytes.get(*cursor).is_some_and(u8::is_ascii_digit) {
                *cursor += 1;
            }
        }
    }
    while bytes.get(*cursor).is_some_and(u8::is_ascii_alphabetic) {
        *cursor += 1;
    }
}

fn tokenize(raw: &str) -> Result<Vec<Token>> {
    ensure!(raw.is_ascii(), "non-ASCII ngspice parameter expression");
    let bytes = raw.as_bytes();
    let mut cursor = 0;
    let mut tokens = Vec::new();
    while let Some(&byte) = bytes.get(cursor) {
        if byte.is_ascii_whitespace() {
            cursor += 1;
            continue;
        }
        let start = cursor;
        if byte.is_ascii_digit()
            || (byte == b'.' && bytes.get(cursor + 1).is_some_and(u8::is_ascii_digit))
        {
            number_end(bytes, byte, &mut cursor);
            tokens.push(Token::Value(parse_spice_constant(&raw[start..cursor])?));
            continue;
        }
        if byte.is_ascii_alphabetic() || byte == b'_' {
            cursor += 1;
            while bytes.get(cursor).is_some_and(|b| {
                b.is_ascii_alphanumeric() || matches!(*b, b'_' | b'#' | b'$' | b'[' | b']')
            }) {
                cursor += 1;
            }
            let name = &raw[start..cursor];
            match name.to_ascii_lowercase().as_str() {
                "not" => tokens.push(Token::Op("!")),
                "and" => tokens.push(Token::Op("&&")),
                "or" => tokens.push(Token::Op("||")),
                "mod" => tokens.push(Token::Op("%")),
                "div" => tokens.push(Token::Op("\\")),
                _ => tokens.push(Token::Name(name.to_ascii_lowercase())),
            }
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
        tokens.push(match byte {
            b'(' => Token::Open,
            b')' => Token::Close,
            b',' => Token::Comma,
            b'?' => Token::Question,
            b':' => Token::Colon,
            b'+' => Token::Op("+"),
            b'-' => Token::Op("-"),
            b'*' => Token::Op("*"),
            b'/' => Token::Op("/"),
            b'%' => Token::Op("%"),
            b'\\' => Token::Op("\\"),
            b'^' => Token::Op("**"),
            b'!' => Token::Op("!"),
            b'<' => Token::Op("<"),
            b'>' => Token::Op(">"),
            b'=' => Token::Op("="),
            _ => bail!(
                "unsupported ngspice expression character {:?}",
                char::from(byte)
            ),
        });
    }
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
    Symbol(String),
    Unary(UnaryOp, Box<Self>),
    Binary(BinaryOp, Box<Self>, Box<Self>),
    Conditional(Box<Self>, Box<Self>, Box<Self>),
    Call(String, Vec<Self>),
}

struct Parsed {
    tree: Expr,
    height: usize,
}

struct Parser<'a> {
    tokens: Vec<Token>,
    offset: usize,
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

    fn node(&self, tree: Expr, height: usize) -> Result<Parsed> {
        self.budget.check_depth(height)?;
        self.budget.charge(Resource::GeneratedNodes, 1)?;
        Ok(Parsed { tree, height })
    }

    fn prefix(&mut self, minimum: u8) -> Result<Parsed> {
        match self.take() {
            Token::Value(value) => self.node(Expr::Number(value), 1),
            Token::Name(name) => {
                if self.peek() != &Token::Open {
                    return self.node(Expr::Symbol(name), 1);
                }
                self.take();
                let mut args = Vec::new();
                let mut height = 0;
                if self.peek() != &Token::Close {
                    loop {
                        let arg = self.expression(0)?;
                        height = height.max(arg.height);
                        args.push(arg.tree);
                        if self.peek() != &Token::Comma {
                            break;
                        }
                        self.take();
                    }
                }
                self.expect(&Token::Close)?;
                self.node(Expr::Call(name, args), height + 1)
            }
            Token::Op(symbol @ ("+" | "-" | "!")) => {
                let op = UnaryOp::from_symbol(symbol).context("invalid prefix operator")?;
                // A sign at the beginning of a native expression binds below
                // products; after an operator a signed literal binds atomically.
                let precedence = match op {
                    UnaryOp::Plus | UnaryOp::Minus if minimum == 0 => {
                        BinaryOp::Add.precedence() + 1
                    }
                    _ => BinaryOp::Pow.precedence() + 1,
                };
                let arg = self.expression(precedence)?;
                self.node(Expr::Unary(op, Box::new(arg.tree)), arg.height + 1)
            }
            Token::Open => {
                let value = self.expression(0)?;
                self.expect(&Token::Close)?;
                Ok(value)
            }
            other => bail!("expected ngspice expression atom, got {other:?}"),
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
                return self.conditional(left);
            }
            let Token::Op(op) = self.peek() else { break };
            let Some(op) = BinaryOp::from_symbol(op) else {
                break;
            };
            let precedence = op.precedence();
            if precedence < minimum {
                break;
            }
            self.take();
            let right = self.expression(precedence + 1)?;
            let height = left.height.max(right.height) + 1;
            left = self.node(
                Expr::Binary(op, Box::new(left.tree), Box::new(right.tree)),
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
                                    Box::new(condition.tree),
                                    Box::new(positive.tree),
                                    Box::new(negative.tree),
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
            tokens: tokenize(inner)?,
            offset: 0,
            budget,
        };
        let expr = parser.expression(0)?;
        ensure!(
            parser.peek() == &Token::End,
            "trailing ngspice expression tokens: {:?}",
            parser.peek()
        );
        Ok(Self(expr.tree))
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
