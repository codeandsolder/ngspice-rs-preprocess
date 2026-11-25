//! Bounded SIMPLIS netlist-preprocessor semantics.
//!
//! This crate intentionally implements only semantics verified against real SIMPLIS
//! source and oracle-expanded decks. Unsupported control flow or expression features
//! fail closed instead of being approximated.

use anyhow::{Context, Result, bail};
use model_preprocess_command::{Resource, WorkBudget};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

mod hierarchy;
pub mod source_dialect;
pub use hierarchy::{
    LibrarySimulator, LinkedGeneratedLibrary, LinkedPreprocessedNetlist, NetlistPreprocessStats,
    ParsedLibrary, PreprocessedNetlist, SubcircuitCatalog, active_statement_prefix,
    capacitor_charge_assignments, linked_library_filename, lower_simetrix_source_view,
    parse_init_statement, parse_init_target_path, parse_library, parse_library_file,
    parse_library_for_simulator, parse_library_with_budget, parse_scoped_library,
    preprocess_netlist, preprocess_netlist_file, preprocess_netlist_linked,
    preprocess_netlist_linked_file, project_resource, project_simetrix_source, project_source,
    render_projection, source_include_target,
};

fn decode_simplis_text(input: &[u8]) -> Result<String> {
    anyhow::ensure!(!input.contains(&0), "SIMPLIS source contains a NUL byte");
    if let Ok(text) = std::str::from_utf8(input) {
        return Ok(text.to_owned());
    }
    let mut output = String::with_capacity(input.len());
    for (line_index, line) in input.split_inclusive(|byte| *byte == b'\n').enumerate() {
        let has_non_ascii = line.iter().any(|byte| !byte.is_ascii());
        if has_non_ascii {
            let first = line
                .iter()
                .copied()
                .find(|byte| !matches!(*byte, b' ' | b'\t' | b'\r' | b'\n'));
            if first != Some(b'*') {
                bail!(
                    "non-ASCII byte in active SIMPLIS syntax at line {}",
                    line_index + 1
                );
            }
        }
        for byte in line {
            output.push(if byte.is_ascii() {
                char::from(*byte)
            } else {
                '?'
            });
        }
    }
    Ok(output)
}

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Number(f64),
    Text(String),
    Vector(Vec<Self>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ScalarType {
    Number,
    Text,
}

impl Value {
    fn charge_copy(&self, budget: &WorkBudget) -> Result<()> {
        let _depth = budget.enter()?;
        budget.charge(Resource::EvaluationSteps, 1)?;
        match self {
            Self::Number(_) => Ok(()),
            Self::Text(text) => budget.charge(Resource::EmittedBytes, text.len()),
            Self::Vector(values) => {
                budget.charge(Resource::GeneratedNodes, values.len())?;
                for value in values {
                    value.charge_copy(budget)?;
                }
                Ok(())
            }
        }
    }

    fn charged_render(&self, budget: &WorkBudget) -> Result<String> {
        self.charge_copy(budget)?;
        self.render()
    }

    // In the reference expression language, scalar and vector are shapes,
    // not distinct element types. Empty vectors have no inspectable type.
    fn scalar_type(&self) -> Result<Option<ScalarType>> {
        match self {
            Self::Number(_) => Ok(Some(ScalarType::Number)),
            Self::Text(_) => Ok(Some(ScalarType::Text)),
            Self::Vector(values) => {
                let mut kind = None;
                for value in values {
                    if let Some(next) = value.scalar_type()? {
                        anyhow::ensure!(
                            kind.is_none_or(|prior| prior == next),
                            "SIMPLIS vector elements must have the same type"
                        );
                        kind = Some(next);
                    }
                }
                Ok(kind)
            }
        }
    }

    fn render(&self) -> Result<String> {
        match self {
            Self::Number(value) => {
                if !value.is_finite() {
                    bail!("cannot render non-finite SIMPLIS value");
                }
                if value.fract() == 0.0 && value.abs() <= 9_007_199_254_740_992.0 {
                    Ok(format!("{value:.0}"))
                } else {
                    Ok(format!("{value:?}"))
                }
            }
            Self::Text(value) => Ok(value.clone()),
            Self::Vector(_) => bail!("vector cannot be rendered into a netlist scalar"),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Environment {
    values: BTreeMap<String, Value>,
    options: BTreeMap<String, Value>,
    budget: WorkBudget,
}

impl PartialEq for Environment {
    fn eq(&self, other: &Self) -> bool {
        self.values == other.values && self.options == other.options
    }
}

impl Environment {
    #[must_use = "the returned environment owns parameter and option bindings"]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create bindings that participate in a caller's cumulative budget.
    #[must_use]
    pub fn with_budget(budget: WorkBudget) -> Self {
        Self {
            budget,
            ..Self::default()
        }
    }

    fn charged_clone(&self) -> Result<Self> {
        for value in self.values.values().chain(self.options.values()) {
            value.charge_copy(&self.budget)?;
        }
        Ok(self.clone())
    }

    pub fn set(&mut self, name: impl AsRef<str>, value: Value) {
        self.values
            .insert(name.as_ref().to_ascii_uppercase(), value);
    }

    #[must_use = "the returned value is the requested parameter binding"]
    pub fn get(&self, name: &str) -> Option<&Value> {
        self.values.get(&name.to_ascii_uppercase())
    }

    #[must_use = "the result reports whether the parameter is bound"]
    pub fn contains(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    pub fn set_option(&mut self, name: impl AsRef<str>, value: Value) {
        self.options
            .insert(name.as_ref().to_ascii_uppercase(), value);
    }

    #[must_use = "the returned value is the requested option binding"]
    pub fn get_option(&self, name: &str) -> Option<&Value> {
        self.options.get(&name.to_ascii_uppercase())
    }

    #[must_use = "the result reports whether any bindings exist"]
    pub(crate) fn is_empty(&self) -> bool {
        self.values.is_empty() && self.options.is_empty()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PreprocessStats {
    pub assigned_variables: usize,
    pub evaluated_conditionals: usize,
    pub substituted_expressions: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Preprocessed {
    pub bytes: Vec<u8>,
    pub environment: Environment,
    pub stats: PreprocessStats,
}

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Number(f64),
    Text(String),
    Ident(String),
    Plus,
    Minus,
    Star,
    Slash,
    Caret,
    Amp,
    Bang,
    EqEq,
    NotEq,
    Less,
    LessEq,
    Greater,
    GreaterEq,
    AndAnd,
    OrOr,
    LParen,
    RParen,
    LBracket,
    RBracket,
    Comma,
    End,
}

/// A numeric token may carry a unit label after its engineering multiplier.
fn engineering_multiplier(suffix: &str) -> (f64, usize) {
    let upper = suffix.to_ascii_uppercase();
    if upper.starts_with("MEG") {
        return (1e6, 3);
    }
    if upper.starts_with("MIL") {
        return (25.4e-6, 3);
    }
    let multiplier = match upper.as_bytes().first() {
        Some(b'T') => 1e12,
        Some(b'G') => 1e9,
        Some(b'K') => 1e3,
        Some(b'M') => 1e-3,
        Some(b'U') => 1e-6,
        Some(b'N') => 1e-9,
        Some(b'P') => 1e-12,
        Some(b'F') => 1e-15,
        Some(b'A') => 1e-18,
        _ => 1.0,
    };
    (multiplier, usize::from(!suffix.is_empty()))
}

fn decimal_end(bytes: &[u8]) -> usize {
    let mut offset = 0usize;
    while bytes
        .get(offset)
        .is_some_and(|byte| byte.is_ascii_digit() || *byte == b'.')
    {
        offset += 1;
    }
    if bytes
        .get(offset)
        .is_some_and(|byte| matches!(*byte, b'e' | b'E'))
    {
        let exponent_marker = offset;
        offset += 1;
        if bytes
            .get(offset)
            .is_some_and(|byte| matches!(*byte, b'+' | b'-'))
        {
            offset += 1;
        }
        let exponent_start = offset;
        while bytes.get(offset).is_some_and(u8::is_ascii_digit) {
            offset += 1;
        }
        if exponent_start == offset {
            offset = exponent_marker;
        }
    }
    offset
}

fn parse_number_literal(input: &str) -> Result<f64> {
    let (number, suffix) = input.split_at(decimal_end(input.as_bytes()));
    let base: f64 = number
        .parse()
        .with_context(|| format!("invalid SIMPLIS number {input:?}"))?;
    anyhow::ensure!(
        suffix.is_empty() || suffix.starts_with(|c: char| c.is_ascii_alphabetic()),
        "invalid SIMPLIS numeric suffix {suffix:?}"
    );
    anyhow::ensure!(
        suffix.bytes().all(|byte| byte.is_ascii_alphanumeric()),
        "invalid SIMPLIS numeric suffix {suffix:?}"
    );
    let (multiplier, prefix) = engineering_multiplier(suffix);
    // SIMetrix accepts engineering decimal notation (4k7, 2R2)
    // and ignores trailing unit text (2kOhm, 3V, 1xx). Only digits
    // immediately following the scale marker extend the mantissa.
    let fractional = suffix[prefix..]
        .bytes()
        .take_while(u8::is_ascii_digit)
        .count();
    // The vendor's engineering-decimal expansion leaves an exactly zero
    // leading value unchanged, including 0k2 and 0.0x2.
    let value = if fractional == 0 || base == 0.0 {
        base
    } else {
        let digits = &suffix[prefix..prefix + fractional];
        base + format!("0.{digits}").parse::<f64>()?
    } * multiplier;
    anyhow::ensure!(value.is_finite(), "non-finite SIMPLIS number {input:?}");
    Ok(value)
}

fn two_byte_token(bytes: &[u8], offset: usize) -> Option<Token> {
    match bytes.get(offset..offset.saturating_add(2))? {
        b"==" => Some(Token::EqEq),
        b"!=" | b"<>" => Some(Token::NotEq),
        b"<=" => Some(Token::LessEq),
        b">=" => Some(Token::GreaterEq),
        b"&&" => Some(Token::AndAnd),
        b"||" => Some(Token::OrOr),
        _ => None,
    }
}

fn tokenize(input: &str) -> Result<Vec<Token>> {
    let bytes = input.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if let Some(token) = two_byte_token(bytes, i) {
            out.push(token);
            i += 2;
            continue;
        }
        match b {
            b'+' => out.push(Token::Plus),
            b'-' => out.push(Token::Minus),
            b'*' => out.push(Token::Star),
            b'/' => out.push(Token::Slash),
            b'^' => out.push(Token::Caret),
            b'&' => out.push(Token::Amp),
            b'!' => out.push(Token::Bang),
            b'<' => out.push(Token::Less),
            b'>' => out.push(Token::Greater),
            b'(' | b'{' => out.push(Token::LParen),
            b')' | b'}' => out.push(Token::RParen),
            b'[' => out.push(Token::LBracket),
            b']' => out.push(Token::RBracket),
            b',' => out.push(Token::Comma),
            b'\'' => {
                let start = i + 1;
                i = start;
                while i < bytes.len() && bytes[i] != b'\'' {
                    i += 1;
                }
                if i == bytes.len() {
                    bail!("unterminated SIMPLIS string literal");
                }
                out.push(Token::Text(input[start..i].to_owned()));
            }
            _ if b.is_ascii_digit() || b == b'.' => {
                let start = i;
                i += decimal_end(&bytes[i..]);
                while bytes.get(i).is_some_and(u8::is_ascii_alphanumeric) {
                    i += 1;
                }
                let literal = &input[start..i];
                out.push(Token::Number(parse_number_literal(literal)?));
                continue;
            }
            _ if b.is_ascii_alphabetic() || b == b'_' => {
                let start = i;
                i += 1;
                while i < bytes.len()
                    && (bytes[i].is_ascii_alphanumeric() || matches!(bytes[i], b'_' | b'$' | b'.'))
                {
                    i += 1;
                }
                out.push(Token::Ident(input[start..i].to_owned()));
                continue;
            }
            _ => bail!("unsupported SIMPLIS expression byte 0x{b:02x}"),
        }
        i += 1;
    }
    out.push(Token::End);
    Ok(out)
}

#[derive(Clone, Copy)]
struct Evaluation<'a> {
    env: &'a Environment,
    conversion_depth: usize,
}

impl<'a> Evaluation<'a> {
    const fn new(env: &'a Environment) -> Self {
        Self {
            env,
            conversion_depth: 0,
        }
    }

    fn expression(self, input: &str) -> Result<Value> {
        self.env
            .budget
            .charge(Resource::EvaluationSteps, input.len().max(1))?;
        let trimmed = input.trim();
        let inner = if trimmed.starts_with('{') && trimmed.ends_with('}') {
            &trimmed[1..trimmed.len().saturating_sub(1)]
        } else {
            trimmed
        };
        Parser::new(inner, self)?.parse()
    }

    fn number(self, value: &Value) -> Result<f64> {
        match value {
            Value::Number(number) => Ok(*number),
            Value::Text(text) => {
                anyhow::ensure!(
                    self.conversion_depth < 32,
                    "SIMPLIS numeric string conversion depth exceeded"
                );
                let nested = Self {
                    conversion_depth: self.conversion_depth + 1,
                    ..self
                };
                let evaluated = nested
                    .expression(text)
                    .with_context(|| format!("SIMPLIS numeric conversion of {text:?}"))?;
                nested.number(&evaluated)
            }
            Value::Vector(_) => bail!("numeric value required"),
        }
    }

    fn truthy(self, value: &Value) -> Result<bool> {
        Ok(self.number(value)? != 0.0)
    }
}

struct Parser<'a> {
    tokens: Vec<Token>,
    offset: usize,
    eval: Evaluation<'a>,
}

impl<'a> Parser<'a> {
    fn new(input: &str, eval: Evaluation<'a>) -> Result<Self> {
        Ok(Self {
            tokens: tokenize(input)?,
            offset: 0,
            eval,
        })
    }

    fn current(&self) -> &Token {
        &self.tokens[self.offset]
    }

    fn bump(&mut self) -> Token {
        let token = self.tokens[self.offset].clone();
        self.offset = self.offset.saturating_add(1);
        token
    }

    fn parse(mut self) -> Result<Value> {
        let value = self.parse_or()?;
        if self.current() != &Token::End {
            bail!("trailing tokens in SIMPLIS expression");
        }
        Ok(value)
    }

    fn parse_or(&mut self) -> Result<Value> {
        let mut left = self.parse_and()?;
        while self.current() == &Token::OrOr {
            self.bump();
            let right = self.parse_and()?;
            left = Value::Number(f64::from(
                self.eval.truthy(&left)? | self.eval.truthy(&right)?,
            ));
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Value> {
        let mut left = self.parse_compare()?;
        while self.current() == &Token::AndAnd {
            self.bump();
            let right = self.parse_compare()?;
            left = Value::Number(f64::from(
                self.eval.truthy(&left)? & self.eval.truthy(&right)?,
            ));
        }
        Ok(left)
    }

    fn parse_compare(&mut self) -> Result<Value> {
        let left = self.parse_concat()?;
        let op = self.current().clone();
        if !matches!(
            op,
            Token::EqEq
                | Token::NotEq
                | Token::Less
                | Token::LessEq
                | Token::Greater
                | Token::GreaterEq
        ) {
            return Ok(left);
        }
        self.bump();
        let right = self.parse_concat()?;
        let result = match (&left, &right) {
            (Value::Number(a), Value::Number(b)) => match op {
                Token::EqEq => a.partial_cmp(b).is_some_and(std::cmp::Ordering::is_eq),
                Token::NotEq => !a.partial_cmp(b).is_some_and(std::cmp::Ordering::is_eq),
                Token::Less => a < b,
                Token::LessEq => a <= b,
                Token::Greater => a > b,
                Token::GreaterEq => a >= b,
                _ => unreachable!(),
            },
            (Value::Text(a), Value::Text(b)) => match op {
                Token::EqEq => a == b,
                Token::NotEq => a != b,
                _ => bail!("ordered comparison of SIMPLIS strings is unsupported"),
            },
            _ => bail!("SIMPLIS comparison type mismatch"),
        };
        Ok(Value::Number(f64::from(result)))
    }

    fn parse_concat(&mut self) -> Result<Value> {
        let mut left = self.parse_add()?;
        while self.current() == &Token::Amp {
            self.bump();
            let right = self.parse_add()?;
            let left_text = left.charged_render(&self.eval.env.budget)?;
            let right_text = right.charged_render(&self.eval.env.budget)?;
            self.eval.env.budget.charge(
                Resource::EmittedBytes,
                left_text.len().saturating_add(right_text.len()),
            )?;
            left = Value::Text(left_text + &right_text);
        }
        Ok(left)
    }

    fn parse_add(&mut self) -> Result<Value> {
        let mut left = self.parse_mul()?;
        loop {
            let add = self.current() == &Token::Plus;
            let sub = self.current() == &Token::Minus;
            if !add && !sub {
                return Ok(left);
            }
            self.bump();
            let right = self.parse_mul()?;
            left = vector_add_sub(left, right, add, self.eval)?;
        }
    }

    fn parse_mul(&mut self) -> Result<Value> {
        let mut left = self.parse_unary()?;
        loop {
            let mul = self.current() == &Token::Star;
            let div = self.current() == &Token::Slash;
            if !mul && !div {
                return Ok(left);
            }
            self.bump();
            let right = self.parse_unary()?;
            let value = if mul {
                self.eval.number(&left)? * self.eval.number(&right)?
            } else {
                self.eval.number(&left)? / self.eval.number(&right)?
            };
            if !value.is_finite() {
                bail!("non-finite SIMPLIS arithmetic result");
            }
            left = Value::Number(value);
        }
    }

    fn parse_unary(&mut self) -> Result<Value> {
        let _depth = self.eval.env.budget.enter()?;
        match self.current() {
            Token::Plus => {
                self.bump();
                let operand = self.parse_unary()?;
                Ok(Value::Number(self.eval.number(&operand)?))
            }
            Token::Minus => {
                self.bump();
                let operand = self.parse_unary()?;
                Ok(Value::Number(-self.eval.number(&operand)?))
            }
            Token::Bang => {
                self.bump();
                let operand = self.parse_unary()?;
                Ok(Value::Number(f64::from(!self.eval.truthy(&operand)?)))
            }
            _ => self.parse_power(),
        }
    }

    fn parse_power(&mut self) -> Result<Value> {
        let left = self.parse_postfix()?;
        if self.current() != &Token::Caret {
            return Ok(left);
        }
        self.bump();
        let operand = self.parse_unary()?;
        let exponent = self.eval.number(&operand)?;
        let value = self.eval.number(&left)?.powf(exponent);
        if !value.is_finite() {
            bail!("non-finite SIMPLIS exponentiation result");
        }
        Ok(Value::Number(value))
    }

    fn parse_postfix(&mut self) -> Result<Value> {
        let mut value = self.parse_primary()?;
        while self.current() == &Token::LBracket {
            self.bump();
            let operand = self.parse_or()?;
            let index = self.eval.number(&operand)?;
            if self.current() != &Token::RBracket {
                bail!("missing ] after SIMPLIS vector index");
            }
            self.bump();
            if index.fract() != 0.0 || index < 0.0 {
                bail!("SIMPLIS vector index must be a non-negative integer");
            }
            let index: usize = format!("{index:.0}")
                .parse()
                .context("SIMPLIS vector index does not fit usize")?;
            let Value::Vector(values) = value else {
                bail!("SIMPLIS indexing requires a vector");
            };
            value = values
                .get(index)
                .cloned()
                .with_context(|| format!("SIMPLIS vector index {index} out of range"))?;
        }
        Ok(value)
    }

    fn parse_primary(&mut self) -> Result<Value> {
        match self.bump() {
            Token::Number(value) => Ok(Value::Number(value)),
            Token::Text(value) => Ok(Value::Text(value)),
            Token::Ident(name) => {
                if self.current() == &Token::LParen {
                    self.bump();
                    self.parse_function(&name)
                } else if name.eq_ignore_ascii_case("PI") {
                    Ok(Value::Number(std::f64::consts::PI))
                } else {
                    let value = self
                        .eval
                        .env
                        .get(&name)
                        .with_context(|| format!("undefined SIMPLIS variable {name}"))?;
                    value.charge_copy(&self.eval.env.budget)?;
                    Ok(value.clone())
                }
            }
            Token::LParen => {
                let value = self.parse_or()?;
                if self.current() != &Token::RParen {
                    bail!("missing ) in SIMPLIS expression");
                }
                self.bump();
                Ok(value)
            }
            Token::LBracket => {
                let mut values = Vec::new();
                if self.current() != &Token::RBracket {
                    loop {
                        self.eval.env.budget.charge(Resource::GeneratedNodes, 1)?;
                        values.push(self.parse_or()?);
                        if self.current() != &Token::Comma {
                            break;
                        }
                        self.bump();
                    }
                }
                if self.current() != &Token::RBracket {
                    bail!("missing ] in SIMPLIS vector literal");
                }
                self.bump();
                let vector = Value::Vector(values);
                vector.scalar_type()?;
                Ok(vector)
            }
            other => bail!("unexpected SIMPLIS expression token {other:?}"),
        }
    }

    fn parse_arguments(&mut self) -> Result<Vec<Value>> {
        let mut args = Vec::new();
        if self.current() != &Token::RParen {
            loop {
                args.push(self.parse_or()?);
                if self.current() != &Token::Comma {
                    break;
                }
                self.bump();
            }
        }
        if self.current() != &Token::RParen {
            bail!("missing ) after SIMPLIS function arguments");
        }
        self.bump();
        Ok(args)
    }

    fn parse_function(&mut self, name: &str) -> Result<Value> {
        if name.eq_ignore_ascii_case("ExistVec") {
            let ident = match self.bump() {
                Token::Text(value) | Token::Ident(value) => value,
                token => bail!("ExistVec requires a variable name, got {token:?}"),
            };
            if self.current() != &Token::RParen {
                bail!("ExistVec accepts exactly one argument");
            }
            self.bump();
            return Ok(Value::Number(f64::from(self.eval.env.contains(&ident))));
        }
        let args = self.parse_arguments()?;
        match name.to_ascii_uppercase().as_str() {
            "IFF" => {
                if args.len() != 3 {
                    bail!("IFF requires exactly three arguments");
                }
                let left_type = args[1].scalar_type()?;
                let right_type = args[2].scalar_type()?;
                anyhow::ensure!(
                    left_type.is_none() || right_type.is_none() || left_type == right_type,
                    "IFF arguments 2 and 3 must be of the same type"
                );
                if self.eval.truthy(&args[0])? {
                    args[1].charge_copy(&self.eval.env.budget)?;
                    Ok(args[1].clone())
                } else {
                    args[2].charge_copy(&self.eval.env.budget)?;
                    Ok(args[2].clone())
                }
            }
            "ABS" => unary_numeric(name, &args, f64::abs, self.eval),
            "SQRT" => unary_numeric(name, &args, f64::sqrt, self.eval),
            "SIN" => unary_numeric(name, &args, f64::sin, self.eval),
            "LN" => unary_numeric(name, &args, f64::ln, self.eval),
            "LOG10" => unary_numeric(name, &args, f64::log10, self.eval),
            "FLOOR" => unary_numeric(name, &args, f64::floor, self.eval),
            "SIGN" => unary_numeric(name, &args, f64::signum, self.eval),
            "VECTOR" => vector_function(name, &args, self.eval),
            "SEARCH" => search_function(name, &args),
            "GETOPTION" => get_option_function(name, &args, self.eval.env),
            "FORMATNUMBER" => format_number_function(name, &args, self.eval),
            "SUBSTSTRING" => subst_string_function(name, &args, &self.eval.env.budget),
            _ => bail!("unsupported SIMPLIS function {name}"),
        }
    }
}

const fn engineering_suffix(exponent: i32) -> Option<&'static str> {
    match exponent {
        -18 => Some("a"),
        -15 => Some("f"),
        -12 => Some("p"),
        -9 => Some("n"),
        -6 => Some("u"),
        -3 => Some("m"),
        0 => Some(""),
        3 => Some("k"),
        6 => Some("Meg"),
        9 => Some("G"),
        12 => Some("T"),
        _ => None,
    }
}

fn trim_decimal_zeros(mut value: String) -> String {
    if value.contains('.') {
        while value.ends_with('0') {
            value.pop();
        }
        if value.ends_with('.') {
            value.pop();
        }
    }
    if value == "-0" { "0".to_owned() } else { value }
}

fn format_engineering(value: f64, significant_digits: u32) -> Result<String> {
    if !value.is_finite() {
        bail!("FormatNumber requires a finite value");
    }
    if value == 0.0 {
        return Ok("0".to_owned());
    }
    let magnitude = value.abs().log10().floor();
    let exponent = (magnitude / 3.0).floor() * 3.0;
    let exponent: i32 = format!("{exponent:.0}")
        .parse()
        .context("FormatNumber engineering exponent does not fit i32")?;
    let suffix = engineering_suffix(exponent)
        .with_context(|| format!("FormatNumber engineering exponent {exponent} is unsupported"))?;
    let scaled = value / 10_f64.powi(exponent);
    let digits_before_decimal = if scaled.abs() < 1.0 {
        0
    } else {
        let digits = scaled.abs().log10().floor() + 1.0;
        format!("{digits:.0}")
            .parse::<u32>()
            .context("FormatNumber digit count does not fit u32")?
    };
    let decimal_digits = significant_digits.saturating_sub(digits_before_decimal);
    let decimal_digits: usize = usize::try_from(decimal_digits)
        .context("FormatNumber decimal digit count does not fit usize")?;
    let rendered = trim_decimal_zeros(format!("{scaled:.decimal_digits$}"));
    Ok(format!("{rendered}{suffix}"))
}

fn subst_string_function(name: &str, args: &[Value], budget: &WorkBudget) -> Result<Value> {
    if args.len() != 4 {
        bail!("{name} requires exactly four arguments");
    }
    let [
        Value::Text(input),
        Value::Text(needle),
        Value::Text(replacement),
        Value::Text(mode),
    ] = args
    else {
        bail!("{name} requires four string arguments");
    };
    if needle.is_empty() {
        bail!("{name} search string cannot be empty");
    }
    if !mode.eq_ignore_ascii_case("all") {
        bail!("{name} currently supports only mode 'all'");
    }
    let matches = input.matches(needle).count();
    let length = input
        .len()
        .saturating_sub(matches.saturating_mul(needle.len()))
        .saturating_add(matches.saturating_mul(replacement.len()));
    budget.charge(Resource::EmittedBytes, length)?;
    Ok(Value::Text(input.replace(needle, replacement)))
}

fn format_number_function(name: &str, args: &[Value], eval: Evaluation<'_>) -> Result<Value> {
    if args.len() != 2 {
        bail!("{name} currently supports exactly two arguments");
    }
    let value = eval.number(&args[0])?;
    let significant_digits = eval.number(&args[1])?;
    if significant_digits < 1.0 || significant_digits > 17.0 || significant_digits.fract() != 0.0 {
        bail!("{name} significant digits must be an integer from 1 through 17");
    }
    let significant_digits: u32 = format!("{significant_digits:.0}")
        .parse()
        .context("FormatNumber significant digits do not fit u32")?;
    Ok(Value::Text(format_engineering(value, significant_digits)?))
}

fn get_option_function(name: &str, args: &[Value], env: &Environment) -> Result<Value> {
    if args.len() != 1 {
        bail!("{name} requires exactly one argument");
    }
    let Value::Text(option_name) = &args[0] else {
        bail!("{name} option name must be a string");
    };
    Ok(env
        .get_option(option_name)
        .cloned()
        .unwrap_or_else(|| Value::Text("FALSE".to_owned())))
}

fn vector_add_sub(left: Value, right: Value, add: bool, eval: Evaluation<'_>) -> Result<Value> {
    fn apply(left: f64, right: f64, add: bool) -> Result<f64> {
        let value = if add { left + right } else { left - right };
        if !value.is_finite() {
            bail!("non-finite SIMPLIS vector arithmetic result");
        }
        Ok(value)
    }

    let elements = match (&left, &right) {
        (Value::Vector(values), _) | (_, Value::Vector(values)) => values.len(),
        _ => 0,
    };
    eval.env.budget.charge(Resource::GeneratedNodes, elements)?;
    match (left, right) {
        (Value::Vector(left), right @ (Value::Number(_) | Value::Text(_))) => {
            let right = eval.number(&right)?;
            left.into_iter()
                .map(|value| Ok(Value::Number(apply(eval.number(&value)?, right, add)?)))
                .collect::<Result<Vec<_>>>()
                .map(Value::Vector)
        }
        (left @ (Value::Number(_) | Value::Text(_)), Value::Vector(right)) => {
            let left = eval.number(&left)?;
            right
                .into_iter()
                .map(|value| Ok(Value::Number(apply(left, eval.number(&value)?, add)?)))
                .collect::<Result<Vec<_>>>()
                .map(Value::Vector)
        }
        (Value::Vector(left), Value::Vector(right)) => {
            if left.len() != right.len() {
                bail!("SIMPLIS vector arithmetic requires equal lengths");
            }
            left.into_iter()
                .zip(right)
                .map(|(left, right)| {
                    Ok(Value::Number(apply(
                        eval.number(&left)?,
                        eval.number(&right)?,
                        add,
                    )?))
                })
                .collect::<Result<Vec<_>>>()
                .map(Value::Vector)
        }
        (left, right) => Ok(Value::Number(apply(
            eval.number(&left)?,
            eval.number(&right)?,
            add,
        )?)),
    }
}

fn vector_function(name: &str, args: &[Value], eval: Evaluation<'_>) -> Result<Value> {
    if args.len() != 1 {
        bail!("{name} requires exactly one argument");
    }
    let length = eval.number(&args[0])?;
    if length < 0.0 || length.fract() != 0.0 {
        bail!("{name} length must be a non-negative integer");
    }
    let length: u32 = format!("{length:.0}")
        .parse()
        .context("SIMPLIS Vector length does not fit u32")?;
    eval.env
        .budget
        .charge(Resource::GeneratedNodes, usize::try_from(length)?)?;
    let values = (0..length)
        .map(|index| Value::Number(f64::from(index)))
        .collect();
    Ok(Value::Vector(values))
}

fn search_function(name: &str, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        bail!("{name} requires exactly two arguments");
    }
    let Value::Vector(values) = &args[0] else {
        bail!("{name} first argument must be a vector");
    };
    let needle = &args[1];
    let index = values.iter().position(|value| match (value, needle) {
        (Value::Number(left), Value::Number(right)) => left
            .partial_cmp(right)
            .is_some_and(std::cmp::Ordering::is_eq),
        (Value::Text(left), Value::Text(right)) => left.eq_ignore_ascii_case(right),
        _ => false,
    });
    let index = index
        .map(u32::try_from)
        .transpose()
        .context("SIMPLIS Search index does not fit u32")?;
    Ok(Value::Number(index.map_or(-1.0, f64::from)))
}

fn unary_numeric(
    name: &str,
    args: &[Value],
    function: fn(f64) -> f64,
    eval: Evaluation<'_>,
) -> Result<Value> {
    if args.len() != 1 {
        bail!("{name} requires exactly one argument");
    }
    let value = function(eval.number(&args[0])?);
    if !value.is_finite() {
        bail!("{name} produced a non-finite result");
    }
    Ok(Value::Number(value))
}

/// Evaluate one bounded SIMPLIS preprocessing expression.
///
/// # Errors
///
/// Returns an error for unsupported syntax/functions, undefined variables, type
/// mismatches, non-finite arithmetic, or invalid vector access.
pub fn eval_expression(input: &str, env: &Environment) -> Result<Value> {
    let result = Evaluation::new(env).expression(input);
    env.budget.check()?;
    result
}

fn directive<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let trimmed = line.trim_start();
    let prefix = format!(".{name}");
    if trimmed.len() < prefix.len() || !trimmed[..prefix.len()].eq_ignore_ascii_case(&prefix) {
        return None;
    }
    let rest = &trimmed[prefix.len()..];
    rest.is_empty().then_some(rest).or_else(|| {
        rest.chars()
            .next()
            .is_some_and(char::is_whitespace)
            .then_some(rest.trim_start())
    })
}

fn unwrap_then(expression: &str) -> &str {
    let trimmed = expression.trim();
    if trimmed.len() >= 4 && trimmed[trimmed.len() - 4..].eq_ignore_ascii_case("then") {
        trimmed[..trimmed.len() - 4].trim_end()
    } else {
        trimmed
    }
}

fn parse_assignment(input: &str, env: &Environment) -> Result<(String, Value)> {
    let Some((name, expression)) = input.split_once('=') else {
        bail!("SIMPLIS .VAR requires name=value");
    };
    let name = name.trim();
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        bail!("invalid SIMPLIS variable name {name:?}");
    }
    Ok((name.to_owned(), eval_expression(expression, env)?))
}

fn find_closing_brace(input: &str, start: usize) -> Result<usize> {
    let bytes = input.as_bytes();
    let mut quote = false;
    let mut depth = 0usize;
    for (index, byte) in bytes.iter().copied().enumerate().skip(start) {
        if byte == b'\'' {
            quote = !quote;
            continue;
        }
        if quote {
            continue;
        }
        match byte {
            b'{' => depth = depth.saturating_add(1),
            b'}' => {
                depth = depth
                    .checked_sub(1)
                    .context("unmatched } in SIMPLIS line")?;
                if depth == 0 {
                    return Ok(index);
                }
            }
            _ => {}
        }
    }
    bail!("unterminated {{...}} expression in SIMPLIS line")
}

fn brace_belongs_to_runtime_annotation(line: &str, start: usize) -> bool {
    let prefix = line[..start].trim_end();
    let token_start = prefix
        .rfind(char::is_whitespace)
        .map_or(0, |index| index.saturating_add(1));
    matches!(
        prefix[token_start..].to_ascii_uppercase().as_str(),
        "BRANCH=" | "ICMODE="
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Substitution {
    None,
    Known,
    All,
}

fn substitute_line(line: &str, env: &Environment, mode: Substitution) -> Result<(String, usize)> {
    if mode == Substitution::None {
        return Ok((line.to_owned(), 0));
    }
    let mut output = String::new();
    let mut offset = 0usize;
    let mut substitutions = 0usize;
    while let Some(relative) = line[offset..].find('{') {
        let start = offset + relative;
        output.push_str(&line[offset..start]);
        let end = find_closing_brace(line, start)?;
        if brace_belongs_to_runtime_annotation(line, start) {
            output.push_str(&line[start..=end]);
        } else {
            match eval_expression(&line[start..=end], env) {
                Ok(value) => {
                    let value = value.charged_render(&env.budget)?;
                    env.budget.charge(Resource::EmittedBytes, value.len())?;
                    output.push_str(&value);
                    substitutions = substitutions.saturating_add(1);
                }
                // A plain definition may carry runtime electrical
                // expressions. The source preprocessor expands its
                // known compile-time expressions while preserving
                // expressions outside that environment.
                Err(_) if mode == Substitution::Known => {
                    env.budget.check()?;
                    output.push_str(&line[start..=end]);
                }
                Err(error) => return Err(error),
            }
        }
        offset = end.saturating_add(1);
    }
    output.push_str(&line[offset..]);
    Ok((output.replace("%%", "%"), substitutions))
}

// Elements 9.40d uses 1000, despite the reference manual listing 100.
// The source cap and the independent implementation resource ceiling are
// different contracts. A source cap is fixed when entering its loop.
const DEFAULT_WHILE_ITERATIONS: usize = 1_000;
const MAX_WHILE_ITERATIONS: usize = 100_000;

fn while_header(header: &str) -> Result<(&str, Option<&str>)> {
    let header = active_statement_prefix(header).trim();
    anyhow::ensure!(!header.is_empty(), "SIMPLIS .WHILE requires a condition");
    let end = if header.starts_with('{') {
        find_closing_brace(header, 0)? + 1
    } else {
        // The vendor also accepts an unbraced, whitespace-delimited
        // condition. Expressions containing spaces belong in braces.
        header.find(char::is_whitespace).unwrap_or(header.len())
    };
    let condition = &header[..end];
    let remaining = header[end..].trim();
    if remaining.is_empty() {
        return Ok((condition, None));
    }
    let limit_end = if remaining.starts_with('{') {
        find_closing_brace(remaining, 0)? + 1
    } else {
        remaining
            .find(char::is_whitespace)
            .unwrap_or(remaining.len())
    };
    Ok((condition, Some(&remaining[..limit_end])))
}

fn while_limit(expression: Option<&str>, env: &Environment) -> Result<usize> {
    let Some(expression) = expression else {
        return Ok(DEFAULT_WHILE_ITERATIONS);
    };
    // Unlike truth testing, the limit requires a numeric value rather
    // than numeric conversion of a string. Invalid expressions must not
    // accidentally disable the independent execution ceiling.
    let Ok(Value::Number(value)) = eval_expression(expression, env) else {
        // The source tool falls back to its default for unresolved
        // names, invalid expressions and nonnumeric values (including
        // quoted numeric strings).
        env.budget.check()?;
        return Ok(DEFAULT_WHILE_ITERATIONS);
    };
    anyhow::ensure!(
        value.is_finite() && value <= f64::from(i32::MAX),
        "SIMPLIS .WHILE limit exceeds signed 32-bit range"
    );
    if value < 1.0 {
        return Ok(0);
    }
    let count: usize = format!("{:.0}", value.trunc())
        .parse()
        .context("SIMPLIS .WHILE limit does not fit usize")?;
    Ok(count)
}

fn find_if_bounds(lines: &[&str], start: usize, end: usize) -> Result<(Option<usize>, usize)> {
    let mut depth = 1usize;
    let mut else_index = None;
    for (index, line) in lines
        .iter()
        .enumerate()
        .take(end)
        .skip(start.saturating_add(1))
    {
        if directive(line, "IF").is_some() {
            depth = depth.saturating_add(1);
        } else if directive(line, "ENDIF").is_some() {
            depth = depth
                .checked_sub(1)
                .context("SIMPLIS IF nesting underflow")?;
            if depth == 0 {
                return Ok((else_index, index));
            }
        } else if depth == 1
            && directive(line, "ELSE").is_some()
            && else_index.replace(index).is_some()
        {
            bail!("duplicate SIMPLIS .ELSE for .IF at line {}", start + 1);
        }
    }
    bail!("unterminated SIMPLIS .IF at line {}", start + 1)
}

fn find_while_end(lines: &[&str], start: usize, end: usize) -> Result<usize> {
    let mut depth = 1usize;
    for (index, line) in lines
        .iter()
        .enumerate()
        .take(end)
        .skip(start.saturating_add(1))
    {
        if directive(line, "WHILE").is_some() {
            depth = depth.saturating_add(1);
        } else if directive(line, "ENDWHILE").is_some() {
            depth = depth
                .checked_sub(1)
                .context("SIMPLIS WHILE nesting underflow")?;
            if depth == 0 {
                return Ok(index);
            }
        }
    }
    bail!("unterminated SIMPLIS .WHILE at line {}", start + 1)
}

struct PreprocessState<'a> {
    env: &'a mut Environment,
    output: &'a mut String,
    assigned_variables: &'a mut usize,
    evaluated_conditionals: &'a mut usize,
    substituted_expressions: &'a mut usize,
    parameter_names: &'a mut BTreeSet<String>,
    substitution: Substitution,
    evaluate_parameters: bool,
}

fn process_while(
    lines: &[&str],
    start: usize,
    end: usize,
    header: &str,
    state: &mut PreprocessState<'_>,
) -> Result<()> {
    let line = start + 1;
    let (condition, source_limit) =
        while_header(header).with_context(|| format!("SIMPLIS .WHILE at line {line}"))?;
    let mut limit = None;
    let mut iterations = 0usize;
    loop {
        state.env.budget.charge(Resource::Statements, 1)?;
        let active = eval_expression(condition, state.env)
            .and_then(|value| Evaluation::new(state.env).truthy(&value))
            .with_context(|| format!("SIMPLIS .WHILE condition at line {line}"))?;
        *state.evaluated_conditionals = state.evaluated_conditionals.saturating_add(1);
        if !active {
            return Ok(());
        }
        // An inactive loop never evaluates its count. An entered loop
        // computes it only once, before executing the first body.
        let limit = if let Some(limit) = limit {
            limit
        } else {
            let value = while_limit(source_limit, state.env)
                .with_context(|| format!("SIMPLIS .WHILE limit at line {line}"))?;
            limit = Some(value);
            value
        };
        // The vendor emits a partial deck after an overrun, but reports
        // an error. Our API is transactional: never execute or return the
        // extra iteration once the condition remains true at the cap.
        anyhow::ensure!(
            iterations < limit,
            "SIMPLIS .WHILE at line {line} exceeded source limit {limit} iterations"
        );
        anyhow::ensure!(
            iterations < MAX_WHILE_ITERATIONS,
            "SIMPLIS .WHILE at line {line} exceeded implementation ceiling {MAX_WHILE_ITERATIONS} iterations"
        );
        process_range(lines, start + 1, end, state)?;
        iterations += 1;
    }
}

fn process_range(
    lines: &[&str],
    start: usize,
    end: usize,
    state: &mut PreprocessState<'_>,
) -> Result<()> {
    let _depth = state.env.budget.enter()?;
    let mut index = start;
    while index < end {
        let line = lines[index];
        state.env.budget.charge(Resource::Statements, 1)?;
        state
            .env
            .budget
            .charge(Resource::ScannedBytes, line.len())?;
        if let Some(condition) = directive(line, "IF") {
            let (else_index, endif_index) = find_if_bounds(lines, index, end)?;
            state
                .env
                .budget
                .charge(Resource::Statements, endif_index - index)?;
            let active = Evaluation::new(state.env)
                .truthy(&eval_expression(unwrap_then(condition), state.env)?)?;
            *state.evaluated_conditionals = state.evaluated_conditionals.saturating_add(1);
            if active {
                process_range(
                    lines,
                    index.saturating_add(1),
                    else_index.unwrap_or(endif_index),
                    state,
                )?;
            } else if let Some(else_index) = else_index {
                process_range(lines, else_index.saturating_add(1), endif_index, state)?;
            }
            index = endif_index.saturating_add(1);
            continue;
        }
        if directive(line, "ELSE").is_some() || directive(line, "ENDIF").is_some() {
            bail!("unexpected SIMPLIS IF terminator at line {}", index + 1);
        }
        if let Some(header) = directive(line, "WHILE") {
            let endwhile_index = find_while_end(lines, index, end)?;
            state
                .env
                .budget
                .charge(Resource::Statements, endwhile_index - index)?;
            process_while(lines, index, endwhile_index, header, state)?;
            index = endwhile_index.saturating_add(1);
            continue;
        }
        if directive(line, "ENDWHILE").is_some() {
            bail!("unexpected SIMPLIS .ENDWHILE at line {}", index + 1);
        }
        if line.trim_start().starts_with('*') {
            state
                .env
                .budget
                .charge(Resource::EmittedBytes, line.len().saturating_add(1))?;
            state.output.push_str(line);
            state.output.push('\n');
            index = index.saturating_add(1);
            continue;
        }
        if let Some(assignment) = directive(line, "VAR").or_else(|| directive(line, "GLOBALVAR")) {
            let (name, value) = parse_assignment(assignment, state.env)
                .with_context(|| format!("SIMPLIS assignment at line {}", index + 1))?;
            state.env.set(name, value);
            *state.assigned_variables = state.assigned_variables.saturating_add(1);
            index = index.saturating_add(1);
            continue;
        }
        if state.evaluate_parameters
            && let Some(assignment) = directive(line, "PARAM")
        {
            let (name, value) = parse_assignment(assignment, state.env)
                .with_context(|| format!("SIMPLIS .PARAM assignment at line {}", index + 1))?;
            state.env.set(&name, value);
            state.parameter_names.insert(name);
            *state.assigned_variables = state.assigned_variables.saturating_add(1);
            index = index.saturating_add(1);
            continue;
        }
        if let Some(message) = directive(line, "ERROR") {
            let (message, _) = substitute_line(message, state.env, Substitution::All)?;
            bail!("SIMPLIS .ERROR at line {}: {}", index + 1, message.trim());
        }
        for unsupported in ["FOR", "ENDFOR"] {
            if directive(line, unsupported).is_some() {
                bail!("unsupported SIMPLIS .{unsupported} at line {}", index + 1);
            }
        }
        // A runtime parameter declaration must retain its expression and
        // binding; control-flow expansion does not evaluate runtime parameters.
        let mode = if !state.evaluate_parameters && directive(line, "PARAM").is_some() {
            Substitution::None
        } else {
            state.substitution
        };
        let (rendered, count) = substitute_line(line, state.env, mode)
            .with_context(|| format!("SIMPLIS substitution at line {}", index + 1))?;
        *state.substituted_expressions = state.substituted_expressions.saturating_add(count);
        state
            .env
            .budget
            .charge(Resource::EmittedBytes, rendered.len().saturating_add(1))?;
        state.output.push_str(&rendered);
        state.output.push('\n');
        index = index.saturating_add(1);
    }
    Ok(())
}

/// Evaluate the bounded, line-oriented SIMPLIS preprocessing subset used by real
/// model-library bodies. The caller provides the subcircuit parameter environment.
///
/// Supported today: `.VAR`, `.GLOBALVAR`, `.IF/.ELSE/.ENDIF`, bounded
/// `.WHILE/.ENDWHILE` (numeric source count, default 1000 as observed in
/// Elements 9.40d, independent implementation ceiling), arithmetic, strings,
/// vectors/indexing, `IFF`, `ExistVec`,
/// and selected functions. `.FOR`, library lookup, and unsupported expressions fail closed.
///
/// # Errors
///
/// Returns an error on unsupported directives or expressions, malformed control
/// flow, undefined variables, invalid arithmetic, or an active `.ERROR` directive.
pub fn preprocess_body(input: &[u8], initial: &Environment) -> Result<Preprocessed> {
    preprocess_body_with_mode(input, initial, true)
}

fn run_preprocess_body(
    input: &[u8],
    initial: &Environment,
    substitution: Substitution,
    evaluate_parameters: bool,
    parameter_names: &mut BTreeSet<String>,
) -> Result<Preprocessed> {
    initial.budget.charge(Resource::ScannedBytes, input.len())?;
    let text = decode_simplis_text(input)?;
    let lines = text.lines().collect::<Vec<_>>();
    let mut env = initial.charged_clone()?;
    let mut output = String::new();
    let mut assigned_variables = 0usize;
    let mut evaluated_conditionals = 0usize;
    let mut substituted_expressions = 0usize;
    process_range(
        &lines,
        0,
        lines.len(),
        &mut PreprocessState {
            env: &mut env,
            output: &mut output,
            assigned_variables: &mut assigned_variables,
            evaluated_conditionals: &mut evaluated_conditionals,
            substituted_expressions: &mut substituted_expressions,
            parameter_names,
            substitution,
            evaluate_parameters,
        },
    )?;
    Ok(Preprocessed {
        bytes: output.into_bytes(),
        environment: env,
        stats: PreprocessStats {
            assigned_variables,
            evaluated_conditionals,
            substituted_expressions,
        },
    })
}

// Execute source controls and substitute known compile-time expressions
// without consuming native runtime .PARAM declarations or electrical
// expressions. Used for root statements and plain subcircuits.
fn preprocess_control_flow(input: &[u8], initial: &Environment) -> Result<Preprocessed> {
    run_preprocess_body(
        input,
        initial,
        Substitution::Known,
        false,
        &mut BTreeSet::new(),
    )
}

fn preprocess_body_with_mode(
    input: &[u8],
    initial: &Environment,
    substitute_active_lines: bool,
) -> Result<Preprocessed> {
    if !substitute_active_lines {
        return preprocess_control_flow(input, initial);
    }

    let mut discovered_names = BTreeSet::new();
    let discovery = run_preprocess_body(
        input,
        initial,
        Substitution::None,
        true,
        &mut discovered_names,
    )?;
    let mut seeded = initial.charged_clone()?;
    for name in discovered_names {
        let value = discovery
            .environment
            .get(&name)
            .with_context(|| format!("discovered SIMPLIS .PARAM {name} has no value"))?
            .clone();
        seeded.set(name, value);
    }
    run_preprocess_body(
        input,
        &seeded,
        Substitution::All,
        true,
        &mut BTreeSet::new(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(value: f64) -> Value {
        Value::Number(value)
    }

    #[test]
    fn numeric_choice_and_engineering_unit_notation_matches_reference() -> Result<()> {
        let env = Environment::new();
        for count in 1..=10 {
            assert_eq!(
                eval_expression(&format!("{count}x"), &env)?,
                n(f64::from(count))
            );
        }
        for (expression, expected) in [
            ("3X", 3.0),
            ("1xx", 1.0),
            ("2.5x", 2.5),
            ("1.5e2x", 150.0),
            ("4foo", 4e-15),
            ("2kOhm", 2000.0),
            ("6megOhm", 6e6),
            ("3V", 3.0),
            ("4x2", 4.2),
            ("4k7", 4700.0),
            ("2R2", 2.2),
            ("1meg5", 1.5e6),
            ("1u25", 1.25e-6),
            ("2k7Ohm", 2700.0),
            ("2xyz", 2.0),
            ("1foo2", 1e-15),
            ("1.5k2", 1700.0),
            ("1x2x3", 1.2),
            ("1abc", 1e-18),
            ("1abc2", 1e-18),
            ("3mV", 0.003),
            ("1mil", 25.4e-6),
            ("2e", 2.0),
            ("3efoo", 3.0),
            ("1e3k", 1e6),
            ("1e-3k", 1.0),
            ("2e3x", 2000.0),
            ("1e+3V", 1000.0),
            ("3e-2V", 0.03),
            ("4e2x3", 400.3),
            ("1e2e3", 100.3),
            ("1.2e2k3", 120_300.0),
            ("0.5x1", 0.6),
            ("0.01k2", 210.0),
            ("0.0001k2", 200.1),
            ("0.1e-2k3", 301.0),
            ("0k2", 0.0),
            ("0.0x2", 0.0),
            ("0e2x3", 0.0),
            ("4k7+1", 4701.0),
            ("3e-2V-1", -0.97),
        ] {
            let actual = Evaluation::new(&env).number(&eval_expression(expression, &env)?)?;
            assert!(
                (actual - expected).abs() <= expected.abs() * 4.0 * f64::EPSILON,
                "{expression}: {actual} != {expected}"
            );
        }
        for invalid in [".", "1.2.3", "1e999", "1k.2", "1x+unknown"] {
            assert!(eval_expression(invalid, &env).is_err(), "{invalid}");
        }
        Ok(())
    }

    #[test]
    fn numeric_string_conversion_uses_the_current_environment() -> Result<()> {
        let mut env = Environment::new();
        env.set("X", n(5.0));
        for (expression, expected) in [
            ("'2x'+1", 3.0),
            ("'2+3'*4", 20.0),
            ("'2k'+1", 2001.0),
            ("'X+2'*3", 21.0),
            ("'2+3'+1", 6.0),
            ("'2'+'3'", 5.0),
            ("ABS('-2')", 2.0),
            ("IFF('0',2,3)", 3.0),
            ("IFF('1',2,3)", 2.0),
            ("!'0'", 1.0),
        ] {
            assert_eq!(
                eval_expression(expression, &env)?,
                n(expected),
                "{expression}"
            );
        }
        assert_eq!(
            eval_expression("'2' & 3", &env)?,
            Value::Text("23".to_owned())
        );
        assert_eq!(eval_expression("'01' == '1'", &env)?, n(0.0));
        for invalid in ["'2' == 2", "'2' < 3", "2 == '2'", "!'foo'"] {
            assert!(eval_expression(invalid, &env).is_err(), "{invalid}");
        }
        env.set("A", Value::Text("2".to_owned()));
        env.set("B", Value::Text("A".to_owned()));
        env.set("C", Value::Text("B+1".to_owned()));
        assert_eq!(eval_expression("'A'+1", &env)?, n(3.0));
        assert_eq!(eval_expression("'B'+1", &env)?, n(3.0));
        assert_eq!(eval_expression("'C'+1", &env)?, n(4.0));
        env.set("LOOP", Value::Text("LOOP+1".to_owned()));
        assert!(eval_expression("LOOP+1", &env).is_err());
        Ok(())
    }

    #[test]
    fn preprocessing_branches_are_lazy_but_operators_and_iff_are_eager() -> Result<()> {
        let env = Environment::new();
        for invalid in [
            "IFF(1,2,UNDEFINED)",
            "IFF(0,UNDEFINED,3)",
            "IFF(0,1/0,3)",
            "IFF(1,2,'abc')",
            "0 && UNDEFINED",
            "1 || UNDEFINED",
            "0 && 'undefined'",
            "1 || 'undefined'",
        ] {
            assert!(eval_expression(invalid, &env).is_err(), "{invalid}");
        }
        assert_eq!(eval_expression("IFF(1,2,[3,4])", &env)?, n(2.0));
        assert_eq!(eval_expression("IFF(0,[2,3],4)", &env)?, n(4.0));
        assert_eq!(
            eval_expression("IFF(1,'hi',['a','b'])", &env)?,
            Value::Text("hi".to_owned())
        );
        assert_eq!(
            eval_expression("IFF(0,['a','b'],'hi')", &env)?,
            Value::Text("hi".to_owned())
        );
        assert!(eval_expression("IFF(1,2,['a','b'])", &env).is_err());
        assert!(eval_expression("IFF(1,[2,'a'],[3,4])", &env).is_err());
        let source = b".IF {0}\nR1 1 0 {UNDEFINED}\n.ELSE\nR2 1 0 3\n.ENDIF\n";
        assert_eq!(preprocess_body(source, &env)?.bytes, b"R2 1 0 3\n");
        let source = b".IF {'0'}\nR1 1 0 {UNDEFINED}\n.ELSE\nR2 1 0 3\n.ENDIF\n";
        assert_eq!(preprocess_body(source, &env)?.bytes, b"R2 1 0 3\n");
        Ok(())
    }

    #[test]
    fn quantity_defaults_overrides_and_inactive_branches_specialize() -> Result<()> {
        let source = b".SUBCKT CTEST A B VARS: USE_QUANTITY=0 QUANTITY=1x\nC1 A B 1u\n.IF {USE_QUANTITY}\nF1 A B V1 {QUANTITY}\n.ELSE\nR1 A B 1k\n.ENDIF\n.ENDS\n";
        let mut overrides = Environment::new();
        assert_eq!(
            specialize_subcircuit(source, &overrides)?
                .environment
                .get("QUANTITY"),
            Some(&n(1.0))
        );
        overrides.set("USE_QUANTITY", n(1.0));
        overrides.set("QUANTITY", eval_expression("3x", &Environment::new())?);
        let specialized = specialize_subcircuit(source, &overrides)?;
        let output = std::str::from_utf8(&specialized.bytes)?;
        assert!(output.contains("F1 A B V1 3"));
        assert!(!output.contains("R1 A B 1k"));
        Ok(())
    }

    #[test]
    fn full_preprocessing_resolves_forward_active_params() -> Result<()> {
        let mut env = Environment::new();
        env.set("SELECT", n(1.0));
        env.set("BASE", n(2.0));
        let input = br"R1 1 0 {LATE}
.if {SELECT}
.PARAM EARLY = {BASE + 1}
.else
.PARAM EARLY = 99
.endif
.PARAM LATE = {EARLY + 1}
";
        let processed = preprocess_body(input, &env)?;
        let text = std::str::from_utf8(&processed.bytes)?;
        assert_eq!(
            text,
            "R1 1 0 4
"
        );
        assert_eq!(processed.environment.get("EARLY"), Some(&n(3.0)));
        assert_eq!(processed.environment.get("LATE"), Some(&n(4.0)));
        Ok(())
    }

    #[test]
    fn control_flow_only_preserves_runtime_params() -> Result<()> {
        let mut env = Environment::new();
        env.set("A", n(2.0));
        let input = br".PARAM RUNTIME = {A + 1}
R1 1 0 {RUNTIME}
";
        let processed = preprocess_body_with_mode(input, &env, false)?;
        assert_eq!(
            std::str::from_utf8(&processed.bytes)?,
            std::str::from_utf8(input)?
        );
        assert!(!processed.environment.contains("RUNTIME"));
        Ok(())
    }

    #[test]
    fn supports_angle_bracket_not_equal() -> Result<()> {
        let mut env = Environment::new();
        env.set("RB", n(0.0));
        assert_eq!(eval_expression("RB<>0", &env)?, n(0.0));
        env.set("RB", n(10.0));
        assert_eq!(eval_expression("RB<>0", &env)?, n(1.0));
        Ok(())
    }

    #[test]
    fn exponentiation_is_right_associative_and_binds_inside_unary_minus() -> Result<()> {
        let env = Environment::new();
        assert_eq!(eval_expression("10^11", &env)?, n(1.0e11));
        assert_eq!(eval_expression("2^3^2", &env)?, n(512.0));
        assert_eq!(eval_expression("-2^2", &env)?, n(-4.0));
        assert_eq!(eval_expression("2^-2", &env)?, n(0.25));
        let sine = Evaluation::new(&env).number(&eval_expression("SIN(PI/2)", &env)?)?;
        assert!((sine - 1.0).abs() < f64::EPSILON);
        Ok(())
    }

    #[test]
    fn evaluates_real_source_expression_shapes() -> Result<()> {
        let mut env = Environment::new();
        env.set("_V1", n(0.0));
        env.set("_V2", n(5.0));
        env.set("_USEPHASE", n(0.0));
        env.set("_PHASE", n(90.0));
        env.set("_FREQ", n(100_000.0));
        env.set("_DELAY", n(2e-6));
        assert_eq!(eval_expression("{_V2>_V1}", &env)?, n(1.0));
        assert_eq!(eval_expression("{(_V1+_V2)/2}", &env)?, n(2.5));
        assert_eq!(
            eval_expression("{IFF(_USEPHASE, _PHASE/360/_FREQ, _DELAY)}", &env)?,
            n(2e-6)
        );
        Ok(())
    }

    #[test]
    fn substitution_preserves_runtime_only_export_annotations() -> Result<()> {
        let mut env = Environment::new();
        env.set("C", Value::Number(1.0e-6));
        let (line, count) = substitute_line(
            "C1 P N {C} BRANCH={IF(ANALYSIS=2,1,0)} ICMODE={RUNTIME_MODE}",
            &env,
            Substitution::All,
        )?;
        assert_eq!(count, 1);
        assert!(line.contains("C1 P N 1e-6"));
        assert!(line.contains("BRANCH={IF(ANALYSIS=2,1,0)}"));
        assert!(line.contains("ICMODE={RUNTIME_MODE}"));
        Ok(())
    }

    #[test]
    fn substitutes_string_placeholders() -> Result<()> {
        let mut env = Environment::new();
        env.set("GRAPH_NAME_I", Value::Text(r"Input\sCurrent".to_owned()));
        assert_eq!(
            eval_expression(r"{SubStString(GRAPH_NAME_I,'\s',' ','all')}", &env,)?,
            Value::Text("Input Current".to_owned())
        );
        Ok(())
    }

    #[test]
    fn tolerates_non_ascii_only_in_comment_lines() -> Result<()> {
        let source = b"* junction temp 25\xb0C\nR1 1 0 1k\n";
        let result = preprocess_body(source, &Environment::new())?;
        let text = std::str::from_utf8(&result.bytes)?;
        assert!(text.contains("* junction temp 25?C"));
        assert!(text.contains("R1 1 0 1k"));
        Ok(())
    }

    #[test]
    fn rejects_non_ascii_in_active_syntax() {
        let error = preprocess_body(b"R\xb01 1 0 1k\n", &Environment::new())
            .err()
            .map(|value| value.to_string());
        assert!(error.is_some_and(|value| value.contains("non-ASCII byte in active")));
    }

    #[test]
    fn format_number_matches_ota_oracle_engineering_values() -> Result<()> {
        let env = Environment::new();
        assert_eq!(
            eval_expression("FormatNumber(-10.1, 16)", &env)?,
            Value::Text("-10.1".to_owned())
        );
        assert_eq!(
            eval_expression("FormatNumber(-100010, 16)", &env)?,
            Value::Text("-100.01k".to_owned())
        );
        assert_eq!(
            eval_expression("FormatNumber(100010, 16)", &env)?,
            Value::Text("100.01k".to_owned())
        );
        assert_eq!(
            eval_expression("FormatNumber(0.0012, 6)", &env)?,
            Value::Text("1.2m".to_owned())
        );
        Ok(())
    }

    #[test]
    fn get_option_defaults_false_and_accepts_injected_values() -> Result<()> {
        let mut env = Environment::new();
        assert_eq!(
            eval_expression("GetOption('Missing')", &env)?,
            Value::Text("FALSE".to_owned())
        );
        env.set_option("Feature", Value::Text("TRUE".to_owned()));
        assert_eq!(
            eval_expression("GetOption('feature')", &env)?,
            Value::Text("TRUE".to_owned())
        );
        Ok(())
    }

    #[test]
    fn supports_vector_and_search_preprocessor_idioms() -> Result<()> {
        let env = Environment::new();
        assert_eq!(
            eval_expression("Vector(4)", &env)?,
            Value::Vector(vec![n(0.0), n(1.0), n(2.0), n(3.0)])
        );
        assert_eq!(eval_expression("Search(Vector(4), 2)", &env)?, n(2.0));
        assert_eq!(eval_expression("Search(Vector(4), 8)", &env)?, n(-1.0));
        assert_eq!(
            eval_expression("Vector(3)+2", &env)?,
            Value::Vector(vec![n(2.0), n(3.0), n(4.0)])
        );
        Ok(())
    }

    #[test]
    fn exposes_pi_builtin_constant() -> Result<()> {
        let env = Environment::new();
        assert_eq!(eval_expression("PI", &env)?, n(std::f64::consts::PI));
        assert_eq!(
            eval_expression("2*pi", &env)?,
            n(2.0 * std::f64::consts::PI)
        );
        Ok(())
    }

    #[test]
    fn renders_numbers_as_shortest_round_trip_text() -> Result<()> {
        assert_eq!(n(0.0012).render()?, "0.0012");
        assert_eq!(n(2e-12).render()?, "2e-12");
        assert_eq!(n(0.66).render()?, "0.66");
        assert_eq!(n(200_000.0).render()?, "200000");
        Ok(())
    }

    #[test]
    fn parses_scientific_notation_and_engineering_suffixes() -> Result<()> {
        let env = Environment::new();
        assert_eq!(eval_expression("1e-6", &env)?, n(1e-6));
        assert_eq!(eval_expression("1E+3", &env)?, n(1e3));
        assert_eq!(eval_expression("100k", &env)?, n(100_000.0));
        assert_eq!(eval_expression("10Meg", &env)?, n(10_000_000.0));
        Ok(())
    }

    #[test]
    fn unescapes_preprocessor_percent_literals() -> Result<()> {
        let source = b"A_BUF %%gnd_ref 0 [ 1 ] [ 2 ] MODEL=M\n";
        let result = preprocess_body(source, &Environment::new())?;
        assert_eq!(
            std::str::from_utf8(&result.bytes)?.trim(),
            "A_BUF %gnd_ref 0 [ 1 ] [ 2 ] MODEL=M"
        );
        Ok(())
    }

    #[test]
    fn literal_comments_do_not_evaluate_braces() -> Result<()> {
        let source = b"* documentation {UNDEFINED}
R1 1 0 1k
";
        let result = preprocess_body(source, &Environment::new())?;
        assert!(std::str::from_utf8(&result.bytes)?.contains("* documentation {UNDEFINED}"));
        Ok(())
    }

    #[test]
    fn supports_diode_vector_indexing() -> Result<()> {
        let mut env = Environment::new();
        env.set("IC", n(2.0));
        assert_eq!(eval_expression("{([1,2,3,3])[IC]}", &env)?, n(3.0));
        Ok(())
    }

    #[test]
    fn evaluates_existvec_without_eager_identifier_lookup() -> Result<()> {
        let mut env = Environment::new();
        assert_eq!(
            eval_expression("{!ExistVec('IDEAL_CLAMPING')}", &env)?,
            n(1.0)
        );
        env.set("ideal_clamping", n(0.0));
        assert_eq!(
            eval_expression("{!ExistVec('IDEAL_CLAMPING')}", &env)?,
            n(0.0)
        );
        Ok(())
    }

    #[test]
    fn preprocesses_real_conditional_source_shape() -> Result<()> {
        let source = br".if {_V2>_V1}
V1 1 2 SIN
+ VOFFSET={(_V1+_V2)/2}
+ DELAY={IFF(_USEPHASE, _PHASE/360/_FREQ, _DELAY)}
.else
V1 2 1 SIN
.endif
";
        let mut env = Environment::new();
        env.set("_V1", n(0.0));
        env.set("_V2", n(5.0));
        env.set("_USEPHASE", n(0.0));
        env.set("_PHASE", n(0.0));
        env.set("_FREQ", n(100_000.0));
        env.set("_DELAY", n(0.0));
        let result = preprocess_body(source, &env)?;
        let text = std::str::from_utf8(&result.bytes)?;
        assert!(text.contains("V1 1 2 SIN"));
        assert!(!text.contains("V1 2 1 SIN"));
        assert!(text.contains("VOFFSET=2.5"));
        assert_eq!(result.stats.evaluated_conditionals, 1);
        Ok(())
    }

    #[test]
    fn preprocesses_diode_variable_program() -> Result<()> {
        let source = br".VAR ICINT = {([1,2,3,3])[IC]}
.if {ICINT > NUMSEG} then
.VAR IC = NUMSEG
.VAR ICINT = NUMSEG
.endif
.VAR MYCOMMENT = '*'
!R_DBODY 1 2 MRDM IC={ICINT}
{MYCOMMENT}RIC 1 2 {IC}
";
        let mut env = Environment::new();
        env.set("IC", n(3.0));
        env.set("NUMSEG", n(2.0));
        let result = preprocess_body(source, &env)?;
        let text = std::str::from_utf8(&result.bytes)?;
        assert!(text.contains("!R_DBODY 1 2 MRDM IC=2"));
        assert!(text.contains("*RIC 1 2 2"));
        assert_eq!(result.stats.assigned_variables, 4);
        Ok(())
    }

    #[test]
    fn inactive_error_is_not_raised() -> Result<()> {
        let source = br".if {0}
.ERROR 'bad'
.else
R1 1 0 1k
.endif
";
        let result = preprocess_body(source, &Environment::new())?;
        assert_eq!(std::str::from_utf8(&result.bytes)?.trim(), "R1 1 0 1k");
        Ok(())
    }

    #[test]
    fn expands_bounded_while_with_nested_vector_index_expression() -> Result<()> {
        let source = br".VAR Xs = {[10,20,30]}
.VAR SEG_CNT = 0
.WHILE {SEG_CNT < 3}
{ '+' } { 'X' & SEG_CNT & '=' } { Xs[{SEG_CNT}] }
.VAR SEG_CNT = {SEG_CNT + 1}
.ENDWHILE
";
        let result = preprocess_body(source, &Environment::new())?;
        let text = std::str::from_utf8(&result.bytes)?;
        assert!(text.contains("+ X0= 10"));
        assert!(text.contains("+ X1= 20"));
        assert!(text.contains("+ X2= 30"));
        assert_eq!(result.environment.get("SEG_CNT"), Some(&n(3.0)));
        Ok(())
    }

    #[test]
    fn source_while_count_matches_vendor_boundaries() -> Result<()> {
        for (limit, bound) in [
            ("", 1000),
            ("garbage", 1000),
            ("{UNKNOWN}", 1000),
            ("'2'", 1000),
            ("{'2'}", 1000),
            (r#""2""#, 1000),
            ("3", 3),
            ("1002", 1002),
            ("2.9", 2),
            ("2e+0", 2),
            ("2x", 2),
            ("LIMIT", 2),
            ("{LIMIT}", 2),
            ("2+1", 3),
            ("{LIMIT + 1}", 3),
            ("3 4", 3),
            ("0", 0),
            ("-1", 0),
            ("-0.5", 0),
            ("0.5", 0),
        ] {
            for count in [bound, bound + 1] {
                let input = format!(
                    ".VAR I=0\n.VAR LIMIT=2\n.WHILE {{I<{count}}} {limit}\n.VAR I={{I+1}}\n.ENDWHILE\nR1 1 0 {{I+1}}\n"
                );
                let result = preprocess_body(input.as_bytes(), &Environment::new());
                if count == bound {
                    let result = result?;
                    assert_eq!(result.environment.get("I"), Some(&n(f64::from(count))));
                    assert_eq!(
                        result.stats.evaluated_conditionals,
                        usize::try_from(count + 1)?
                    );
                } else {
                    let error = result.err().context("expected source count error")?;
                    assert!(
                        format!("{error:#}").contains(&format!("source limit {bound} iterations")),
                        "{error:#}"
                    );
                }
            }
        }
        Ok(())
    }

    #[test]
    fn while_source_limit_does_not_disable_the_implementation_ceiling() -> Result<()> {
        let overflow = preprocess_body(b".WHILE {1} 2147483648\n.ENDWHILE\n", &Environment::new())
            .err()
            .context("expected out-of-range limit error")?;
        assert!(format!("{overflow:#}").contains("signed 32-bit range"));
        let ceiling = preprocess_body(b".WHILE {1} 100001\n.ENDWHILE\n", &Environment::new())
            .err()
            .context("expected implementation ceiling error")?;
        assert!(format!("{ceiling:#}").contains("implementation ceiling 100000"));
        Ok(())
    }

    #[test]
    fn while_counts_are_entry_local_and_inactive_bodies_are_lazy() -> Result<()> {
        let source = br".VAR I=0
.VAR SUM=0
.WHILE {I<3} 3
.VAR J=0
.WHILE {J<2} {2}
.IF {I==1}
.VAR SUM={SUM+10}
.ELSE
.VAR SUM={SUM+1}
.ENDIF
.VAR J={J+1}
.ENDWHILE
.VAR I={I+1}
.ENDWHILE
.IF {0}
.WHILE {UNDEFINED} UNKNOWN
.ERROR 'unreachable'
.ENDWHILE
.ENDIF
.WHILE {0} UNKNOWN
.ERROR 'unreachable'
.ENDWHILE
R1 1 0 {SUM}
";
        let result = preprocess_body(source, &Environment::new())?;
        assert_eq!(std::str::from_utf8(&result.bytes)?.trim(), "R1 1 0 24");
        for (limit, next) in [(2, 10), (3, 0)] {
            let input = format!(
                ".VAR I=0\n.VAR LIMIT={limit}\n.WHILE {{I<5}} {{LIMIT}}\n.VAR I={{I+1}}\n.VAR LIMIT={next}\n.ENDWHILE\n"
            );
            let error = preprocess_body(input.as_bytes(), &Environment::new())
                .err()
                .context("entry count must not change")?;
            assert!(format!("{error:#}").contains(&format!("source limit {limit} iterations")));
        }
        let naked = preprocess_body(
            b".VAR I=0\n.WHILE I<3 3\n.VAR I={I+1}\n.ENDWHILE\nR1 1 0 {I+1}\n",
            &Environment::new(),
        )?;
        assert_eq!(std::str::from_utf8(&naked.bytes)?.trim(), "R1 1 0 4");
        Ok(())
    }

    #[test]
    fn for_remains_fail_closed() {
        let error = preprocess_body(b".for I=0 to 1\n.enfor\n", &Environment::new())
            .err()
            .map(|value| value.to_string());
        assert!(error.is_some_and(|value| value.contains("unsupported SIMPLIS .FOR")));
    }
}

fn find_ascii_case_insensitive(haystack: &str, needle: &str) -> Option<usize> {
    let needle = needle.as_bytes();
    haystack
        .as_bytes()
        .windows(needle.len())
        .position(|window| window.eq_ignore_ascii_case(needle))
}

/// Historical fixed nominal `SIMetrix` constants, as decimal source literals.
#[must_use = "the result determines whether the source symbol is a fixed constant"]
pub fn simetrix_constant(name: &str) -> Option<&'static str> {
    match name.to_ascii_uppercase().as_str() {
        "PI" => Some("3.141592653589793"),
        "E" => Some("2.718281828459045"),
        "BOLTZ" => Some("1.3806226e-23"),
        "ECHARGE" => Some("1.6021918e-19"),
        _ => None,
    }
}

/// Reject attempts to bind a predefined nominal parameter as an ordinary one.
///
/// # Errors
/// The name is reserved by the source simulator.
pub fn validate_simetrix_parameter_name(name: &str) -> Result<()> {
    anyhow::ensure!(
        simetrix_constant(name).is_none() && !name.eq_ignore_ascii_case("TEMP"),
        "SIMetrix predefined parameter {name} has already been defined"
    );
    Ok(())
}

/// Parse ordered parameter assignments without evaluating their expressions.
///
/// # Errors
/// Rejects malformed names, missing values and unbalanced expression delimiters.
pub fn parameter_assignments(input: &str) -> Result<Vec<(&str, &str)>> {
    named_assignments(input, false)
}

fn named_assignments(input: &str, init: bool) -> Result<Vec<(&str, &str)>> {
    let bytes = input.as_bytes();
    let mut offset = 0usize;
    let mut assignments = Vec::new();
    while offset < bytes.len() {
        while bytes
            .get(offset)
            .is_some_and(|byte| byte.is_ascii_whitespace() || *byte == b'+')
        {
            offset += 1;
        }
        if offset >= bytes.len() {
            break;
        }
        let name_start = offset;
        while bytes.get(offset).is_some_and(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(*byte, b'_' | b'$' | b'.')
                || init && matches!(*byte, b'!' | b':' | b'(' | b')')
        }) {
            offset += 1;
        }
        if offset == name_start {
            bail!("invalid SIMPLIS parameter list near {:?}", &input[offset..]);
        }
        let name = &input[name_start..offset];
        while bytes.get(offset).is_some_and(u8::is_ascii_whitespace) {
            offset += 1;
        }
        if bytes.get(offset) != Some(&b'=') {
            bail!("SIMPLIS parameter {name} is missing '='");
        }
        offset += 1;
        while bytes.get(offset).is_some_and(u8::is_ascii_whitespace) {
            offset += 1;
        }
        let value_start = offset;
        let mut quote = false;
        let mut brace_depth = 0usize;
        let mut bracket_depth = 0usize;
        let mut paren_depth = 0usize;
        while offset < bytes.len() {
            let byte = bytes[offset];
            if byte == b'\'' {
                quote = !quote;
                offset += 1;
                continue;
            }
            if !quote {
                match byte {
                    b'{' => brace_depth = brace_depth.saturating_add(1),
                    b'}' => {
                        brace_depth = brace_depth
                            .checked_sub(1)
                            .context("unmatched } in SIMPLIS parameter")?;
                    }
                    b'[' => bracket_depth = bracket_depth.saturating_add(1),
                    b']' => {
                        bracket_depth = bracket_depth
                            .checked_sub(1)
                            .context("unmatched ] in SIMPLIS parameter")?;
                    }
                    b'(' => paren_depth = paren_depth.saturating_add(1),
                    b')' => {
                        paren_depth = paren_depth
                            .checked_sub(1)
                            .context("unmatched ) in SIMPLIS parameter")?;
                    }
                    _ => {}
                }
                if byte.is_ascii_whitespace()
                    && brace_depth == 0
                    && bracket_depth == 0
                    && paren_depth == 0
                {
                    break;
                }
            }
            offset += 1;
        }
        if quote || brace_depth != 0 || bracket_depth != 0 || paren_depth != 0 {
            bail!("unterminated SIMPLIS parameter value for {name}");
        }
        let expression = input[value_start..offset].trim();
        if expression.is_empty() {
            bail!("SIMPLIS parameter {name} has an empty value");
        }
        assignments.push((name, expression));
    }
    Ok(assignments)
}

fn parse_parameter_list(input: &str, env: &mut Environment) -> Result<usize> {
    let assignments = parameter_assignments(input)?;
    for (name, expression) in &assignments {
        let value = eval_expression(expression, env)
            .with_context(|| format!("evaluating default for SIMPLIS parameter {name}"))?;
        env.set(name, value);
    }
    Ok(assignments.len())
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SpecializedSubcircuitStats {
    pub default_parameters: usize,
    pub overridden_parameters: usize,
    pub body: PreprocessStats,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SpecializedSubcircuit {
    pub bytes: Vec<u8>,
    pub environment: Environment,
    pub stats: SpecializedSubcircuitStats,
}

/// Specialize one standalone SIMPLIS `.SUBCKT ... VARS:` definition to concrete
/// parameter values, then run the bounded body preprocessor over it.
///
/// This is deliberately not hierarchy expansion: nested `.SUBCKT` definitions and
/// unresolved child-instance preprocessing remain errors until oracle fixtures pin
/// their semantics.
///
/// # Errors
///
/// Returns an error for malformed headers, missing `.ENDS`, nested subcircuits,
/// unsupported body preprocessing, or invalid parameter overrides.
pub fn specialize_subcircuit(
    input: &[u8],
    overrides: &Environment,
) -> Result<SpecializedSubcircuit> {
    specialize_subcircuit_with_base(
        input,
        &Environment::with_budget(overrides.budget.clone()),
        overrides,
    )
}

fn specialize_subcircuit_with_base(
    input: &[u8],
    inherited: &Environment,
    overrides: &Environment,
) -> Result<SpecializedSubcircuit> {
    specialize_subcircuit_with_base_mode(input, inherited, overrides, true)
}

fn specialize_subcircuit_control_flow_with_base(
    input: &[u8],
    inherited: &Environment,
    overrides: &Environment,
) -> Result<SpecializedSubcircuit> {
    specialize_subcircuit_with_base_mode(input, inherited, overrides, false)
}

fn specialize_subcircuit_with_base_mode(
    input: &[u8],
    inherited: &Environment,
    overrides: &Environment,
    substitute_active_lines: bool,
) -> Result<SpecializedSubcircuit> {
    inherited
        .budget
        .charge(Resource::ScannedBytes, input.len())?;
    let text = decode_simplis_text(input)?;
    let lines: Vec<&str> = text.lines().collect();
    let first = lines
        .iter()
        .position(|line| directive(line, "SUBCKT").is_some())
        .context("SIMPLIS specialization found no .SUBCKT")?;
    let mut header_end = first + 1;
    while lines
        .get(header_end)
        .is_some_and(|line| line.trim_start().starts_with('+'))
    {
        header_end += 1;
    }
    let mut header = lines[first].trim().to_owned();
    for line in &lines[first + 1..header_end] {
        header.push(' ');
        header.push_str(
            line.trim_start_matches(char::is_whitespace)
                .trim_start_matches('+')
                .trim(),
        );
    }
    let subckt_rest = directive(&header, "SUBCKT").context("invalid .SUBCKT header")?;
    let vars_offset = find_ascii_case_insensitive(subckt_rest, "vars:");
    let (identity, parameter_text) = vars_offset.map_or((subckt_rest, ""), |index| {
        (&subckt_rest[..index], &subckt_rest[index + "vars:".len()..])
    });
    let identity = identity.trim();
    let mut identity_fields = identity.split_whitespace();
    let name = identity_fields
        .next()
        .context("SIMPLIS .SUBCKT has no name")?;
    let nodes = identity_fields.collect::<Vec<_>>().join(" ");
    if nodes.is_empty() {
        bail!("SIMPLIS .SUBCKT {name} has no external nodes");
    }

    let mut env = inherited.charged_clone()?;
    env.options.extend(overrides.options.clone());
    let default_parameters = parse_parameter_list(parameter_text, &mut env)?;
    let mut overridden_parameters = 0usize;
    for (name, value) in &overrides.values {
        env.values.insert(name.clone(), value.clone());
        overridden_parameters = overridden_parameters.saturating_add(1);
    }

    let mut body_end = None;
    for (index, line) in lines.iter().enumerate().skip(header_end) {
        if directive(line, "SUBCKT").is_some() {
            bail!("nested .SUBCKT remains unsupported during specialization");
        }
        if directive(line, "ENDS").is_some() {
            body_end = Some(index);
            break;
        }
    }
    let body_end = body_end.context("SIMPLIS specialization found no matching .ENDS")?;
    let mut body_source = String::new();
    for line in &lines[header_end..body_end] {
        body_source.push_str(line);
        body_source.push('\n');
    }
    let body = preprocess_body_with_mode(body_source.as_bytes(), &env, substitute_active_lines)?;

    let mut output = format!(".SUBCKT {name} {nodes}\n");
    output.push_str(std::str::from_utf8(&body.bytes)?);
    writeln_string(&mut output, &format!(".ENDS {name}"));
    Ok(SpecializedSubcircuit {
        bytes: output.into_bytes(),
        environment: body.environment,
        stats: SpecializedSubcircuitStats {
            default_parameters,
            overridden_parameters,
            body: body.stats,
        },
    })
}

fn writeln_string(output: &mut String, line: &str) {
    output.push_str(line);
    output.push('\n');
}

#[cfg(test)]
mod specialization_tests {
    use super::*;

    #[test]
    fn specializes_real_sin_source_shape() -> Result<()> {
        let source = br".SUBCKT SIN_SOURCE 1 2 vars: _V1=0 _V2=1 _FREQ=100k _DELAY=0 _OFF_UNTIL_DELAY=0 _IDLE_IN_POP=0 _USEPHASE=0 _PHASE=0 _DAMP_COEF=0
.NODE_MAP P 1
.NODE_MAP N 2
.if {_V2>_V1}
V1 1 2 SIN
+ VOFFSET={(_V1+_V2)/2}
+ APEAK={(_V2-_V1)/2}
+ FREQ={_FREQ}
+ TDELAY={IFF(_USEPHASE, _PHASE/360/_FREQ, _DELAY)}
.else
V1 2 1 SIN
.endif
.ENDS
";
        let specialized = specialize_subcircuit(source, &Environment::new())?;
        let text = std::str::from_utf8(&specialized.bytes)?;
        assert!(text.starts_with(".SUBCKT SIN_SOURCE 1 2\n"));
        assert!(text.contains("V1 1 2 SIN"));
        assert!(text.contains("VOFFSET=0.5"));
        assert!(text.contains("FREQ=100000"));
        assert!(!text.to_ascii_lowercase().contains("vars:"));
        assert!(!text.to_ascii_lowercase().contains(".if"));
        assert_eq!(specialized.stats.default_parameters, 9);
        Ok(())
    }

    #[test]
    fn overrides_defaults_case_insensitively() -> Result<()> {
        let source = b".SUBCKT T 1 2 VARS: A=1 B=2\nR1 1 2 {A+B}\n.ENDS\n";
        let mut overrides = Environment::new();
        overrides.set("a", Value::Number(3.0));
        let specialized = specialize_subcircuit(source, &overrides)?;
        assert!(std::str::from_utf8(&specialized.bytes)?.contains("R1 1 2 5"));
        assert_eq!(specialized.stats.overridden_parameters, 1);
        Ok(())
    }
}
