//! Compiled, side-effect-free ngspice `numparam` expressions.
//!
//! Source symbols are resolved at model preparation, never while evaluating
//! a device. Lazy conditional evaluation and explicit errors for stochastic
//! functions avoid silently substituting deterministic model parameters.

use crate::model_card::parse_spice_constant;
use anyhow::{Context, Result, bail, ensure};
use std::cmp::Ordering;

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
            if byte == b'.' {
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
                let offset =
                    exponent + usize::from(matches!(bytes.get(exponent), Some(b'+' | b'-')));
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
    Unary(&'static str, Box<Self>),
    Binary(&'static str, Box<Self>, Box<Self>),
    Conditional(Box<Self>, Box<Self>, Box<Self>),
    Call(String, Vec<Self>),
}

struct Parser {
    tokens: Vec<Token>,
    offset: usize,
}
impl Parser {
    fn take(&mut self) -> Token {
        let token = self.tokens[self.offset].clone();
        self.offset += 1;
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
    fn prefix(&mut self) -> Result<Expr> {
        match self.take() {
            Token::Value(value) => Ok(Expr::Number(value)),
            Token::Name(name) => {
                if self.peek() != &Token::Open {
                    return Ok(Expr::Symbol(name));
                }
                self.take();
                let mut args = Vec::new();
                if self.peek() != &Token::Close {
                    loop {
                        args.push(self.expression(0)?);
                        if self.peek() != &Token::Comma {
                            break;
                        }
                        self.take();
                    }
                }
                self.expect(&Token::Close)?;
                Ok(Expr::Call(name, args))
            }
            Token::Op("+" | "-" | "!") => {
                let Token::Op(op) = self.tokens[self.offset - 1] else {
                    unreachable!()
                };
                Ok(Expr::Unary(op, Box::new(self.expression(8)?)))
            }
            Token::Open => {
                let value = self.expression(0)?;
                self.expect(&Token::Close)?;
                Ok(value)
            }
            other => bail!("expected ngspice expression atom, got {other:?}"),
        }
    }
    fn expression(&mut self, minimum: u8) -> Result<Expr> {
        let mut left = self.prefix()?;
        loop {
            if *self.peek() == Token::Question {
                if minimum > 1 {
                    break;
                }
                self.take();
                let then_branch = self.expression(0)?;
                self.expect(&Token::Colon)?;
                let else_branch = self.expression(1)?;
                left =
                    Expr::Conditional(Box::new(left), Box::new(then_branch), Box::new(else_branch));
                continue;
            }
            let Token::Op(op) = self.peek() else { break };
            let precedence = match *op {
                "||" => 2,
                "&&" => 3,
                "==" | "!=" | "<=" | ">=" | "<" | ">" => 4,
                "+" | "-" => 5,
                "*" | "/" | "%" | "\\" => 6,
                "**" => 8,
                _ => break,
            };
            if precedence < minimum {
                break;
            }
            let Token::Op(op) = self.take() else {
                unreachable!()
            };
            let right = self.expression(precedence + 1)?;
            left = Expr::Binary(op, Box::new(left), Box::new(right));
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
            tokens: tokenize(inner)?,
            offset: 0,
        };
        let expr = parser.expression(0)?;
        ensure!(
            parser.peek() == &Token::End,
            "trailing ngspice expression tokens: {:?}",
            parser.peek()
        );
        Ok(Self(expr))
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

impl Expr {
    fn evaluate(&self, resolve: &mut impl SpiceExpressionContext) -> Result<f64> {
        let value = match self {
            Self::Number(value) => *value,
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
