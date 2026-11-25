//! Shared lexical source syntax for electrical translation and explicit compilers.
use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;

/// Validate a dialect electrical node identifier.
///
/// # Errors
/// Returns an error for an empty or invalid node name.
pub fn validate_node(node: &str) -> Result<()> {
    if node.is_empty()
        || !node.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'_' | b'$' | b'.' | b'#' | b':' | b'+' | b'-' | b'*')
        })
    {
        bail!("invalid SPICE node {node:?}");
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExprToken {
    Number,
    Plus,
    Minus,
    Star,
    Slash,
    LeftParen,
    RightParen,
    End,
}

struct ExprParser<'a> {
    input: &'a [u8],
    offset: usize,
}

impl<'a> ExprParser<'a> {
    const fn new(input: &'a [u8]) -> Self {
        Self { input, offset: 0 }
    }

    fn skip_space(&mut self) {
        while self
            .input
            .get(self.offset)
            .is_some_and(u8::is_ascii_whitespace)
        {
            self.offset += 1;
        }
    }

    fn token(&mut self) -> ExprToken {
        self.skip_space();
        match self.input.get(self.offset) {
            Some(b'+') => ExprToken::Plus,
            Some(b'-') => ExprToken::Minus,
            Some(b'*') => ExprToken::Star,
            Some(b'/') => ExprToken::Slash,
            Some(b'(') => ExprToken::LeftParen,
            Some(b')') => ExprToken::RightParen,
            Some(byte) if byte.is_ascii_digit() || *byte == b'.' => ExprToken::Number,
            None | Some(_) => ExprToken::End,
        }
    }

    fn consume(&mut self, token: ExprToken) -> bool {
        if self.token() != token {
            return false;
        }
        self.offset += 1;
        true
    }

    fn parse(mut self) -> Result<f64> {
        let value = self.parse_sum()?;
        self.skip_space();
        if self.offset != self.input.len() {
            bail!(
                "unsupported SIMPLIS numeric expression {:?}",
                String::from_utf8_lossy(self.input)
            );
        }
        if !value.is_finite() {
            bail!("SIMPLIS numeric expression evaluated to a non-finite value");
        }
        Ok(value)
    }

    fn parse_sum(&mut self) -> Result<f64> {
        let mut value = self.parse_product()?;
        loop {
            if self.consume(ExprToken::Plus) {
                value += self.parse_product()?;
            } else if self.consume(ExprToken::Minus) {
                value -= self.parse_product()?;
            } else {
                return Ok(value);
            }
        }
    }

    fn parse_product(&mut self) -> Result<f64> {
        let mut value = self.parse_unary()?;
        loop {
            if self.consume(ExprToken::Star) {
                value *= self.parse_unary()?;
            } else if self.consume(ExprToken::Slash) {
                let divisor = self.parse_unary()?;
                if divisor == 0.0 {
                    bail!("division by zero in SIMPLIS numeric expression");
                }
                value /= divisor;
            } else {
                return Ok(value);
            }
        }
    }

    fn parse_unary(&mut self) -> Result<f64> {
        if self.consume(ExprToken::Plus) {
            self.parse_unary()
        } else if self.consume(ExprToken::Minus) {
            Ok(-self.parse_unary()?)
        } else {
            self.parse_primary()
        }
    }

    fn parse_primary(&mut self) -> Result<f64> {
        if self.consume(ExprToken::LeftParen) {
            let value = self.parse_sum()?;
            if !self.consume(ExprToken::RightParen) {
                bail!("unterminated parenthesis in SIMPLIS numeric expression");
            }
            return Ok(value);
        }
        self.parse_number()
    }

    fn parse_number(&mut self) -> Result<f64> {
        self.skip_space();
        let start = self.offset;
        let mut saw_digit = false;
        while self.input.get(self.offset).is_some_and(u8::is_ascii_digit) {
            saw_digit = true;
            self.offset += 1;
        }
        if self.input.get(self.offset) == Some(&b'.') {
            self.offset += 1;
            while self.input.get(self.offset).is_some_and(u8::is_ascii_digit) {
                saw_digit = true;
                self.offset += 1;
            }
        }
        if !saw_digit {
            bail!("expected number in SIMPLIS numeric expression");
        }
        if self
            .input
            .get(self.offset)
            .is_some_and(|byte| matches!(byte, b'e' | b'E'))
        {
            self.offset += 1;
            if self
                .input
                .get(self.offset)
                .is_some_and(|byte| matches!(byte, b'+' | b'-'))
            {
                self.offset += 1;
            }
            let exponent_start = self.offset;
            while self.input.get(self.offset).is_some_and(u8::is_ascii_digit) {
                self.offset += 1;
            }
            if exponent_start == self.offset {
                bail!("invalid exponent in SIMPLIS numeric expression");
            }
        }
        let number_end = self.offset;
        while self
            .input
            .get(self.offset)
            .is_some_and(u8::is_ascii_alphabetic)
        {
            self.offset += 1;
        }
        let number = std::str::from_utf8(&self.input[start..number_end])?.parse::<f64>()?;
        let suffix = self.input[number_end..self.offset].to_ascii_lowercase();
        let multiplier = if suffix.starts_with(b"meg") {
            1.0e6
        } else if suffix.starts_with(b"mil") {
            25.4e-6
        } else {
            match suffix.first().copied() {
                Some(b't') => 1.0e12,
                Some(b'g') => 1.0e9,
                Some(b'k') => 1.0e3,
                Some(b'm') => 1.0e-3,
                Some(b'u') => 1.0e-6,
                Some(b'n') => 1.0e-9,
                Some(b'p') => 1.0e-12,
                Some(b'f') => 1.0e-15,
                None | Some(_) => 1.0,
            }
        };
        Ok(number * multiplier)
    }
}

/// Parse a finite static numeric expression and SPICE scale suffixes.
///
/// # Errors
/// Returns an error for unsupported expressions or nonfinite results.
pub fn parse_numeric(input: &str) -> Result<f64> {
    let trimmed = input.trim();
    let expression = trimmed
        .strip_prefix('{')
        .and_then(|value| value.strip_suffix('}'))
        .unwrap_or(trimmed);
    ExprParser::new(expression.as_bytes()).parse()
}

/// Tokenize an electrical statement, retaining quoted expressions and braces.
///
/// # Errors
/// Returns an error for unbalanced quotes or braces.
pub fn statement_tokens(input: &str) -> Result<Vec<String>> {
    if input.trim_start().starts_with('*') {
        return Ok(Vec::new());
    }
    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut brace_depth = 0usize;
    let mut quote = false;
    for character in input.chars() {
        match character {
            '\'' => {
                quote = !quote;
                token.push(character);
            }
            '{' if !quote => {
                brace_depth = brace_depth
                    .checked_add(1)
                    .ok_or_else(|| anyhow::anyhow!("SIMPLIS brace nesting overflow"))?;
                token.push(character);
            }
            '}' if !quote => {
                brace_depth = brace_depth
                    .checked_sub(1)
                    .ok_or_else(|| anyhow::anyhow!("unmatched SIMPLIS closing brace"))?;
                token.push(character);
            }
            ';' if !quote && brace_depth == 0 => break,
            character if character.is_whitespace() && !quote && brace_depth == 0 => {
                if !token.is_empty() {
                    tokens.push(std::mem::take(&mut token));
                }
            }
            _ => token.push(character),
        }
    }
    if quote || brace_depth != 0 {
        bail!("unterminated quote or brace in SIMPLIS statement");
    }
    if !token.is_empty() {
        tokens.push(token);
    }
    Ok(tokens)
}

/// Join continuation lines into a single logical statement.
#[must_use = "the normalized statement is needed for subsequent parsing"]
pub fn normalized_statement(input: &str) -> String {
    input
        .lines()
        .map(str::trim)
        .map(|line| line.strip_prefix('+').map_or(line, str::trim_start))
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// A logical source statement or nested subcircuit definition.
pub enum Item {
    /// Unmodified logical statement including continuation lines.
    Statement(String),
    /// Lexically nested definition.
    Subcircuit(Subcircuit),
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// Parsed lexical subcircuit, before electrical lowering.
pub struct Subcircuit {
    /// Original header.
    pub header: String,
    /// Declared name.
    pub name: String,
    /// Statements in source order.
    pub body: Vec<Item>,
}

fn bare_model_header(statement: &str) -> bool {
    let fields = statement.split_whitespace().collect::<Vec<_>>();
    fields.len() == 3 && fields[0].eq_ignore_ascii_case(".model")
}

fn assignment_only_continuation(line: &str) -> bool {
    let Some((name, value)) = line.trim().split_once('=') else {
        return false;
    };
    !name.trim().is_empty()
        && name
            .trim()
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        && !value.trim().is_empty()
}

/// Identify embedded file delimiters without tokenizing file contents.
/// True opens a file, false closes it.
#[must_use = "the boundary determines whether subsequent text is embedded data"]
pub fn file_boundary(statement: &str) -> Option<bool> {
    let trimmed = statement.trim_start();
    for (name, starts) in [(".file", true), (".endf", false)] {
        if trimmed
            .get(..name.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(name))
            && trimmed
                .get(name.len()..name.len().saturating_add(1))
                .is_none_or(|value| value.as_bytes().iter().all(u8::is_ascii_whitespace))
        {
            return Some(starts);
        }
    }
    None
}

/// Borrow the electrical statements in one lexical scope, excluding
/// embedded file payloads and subcircuit definitions.
#[must_use = "the returned statements exclude embedded file content"]
pub fn electrical_statements(items: &[Item]) -> Vec<&str> {
    let mut statements = Vec::new();
    let mut embedded = false;
    for item in items {
        if let Item::Statement(statement) = item {
            if let Some(start) = file_boundary(statement) {
                embedded = start;
            } else if !embedded {
                statements.push(statement.as_str());
            }
        }
    }
    statements
}

/// Collect embedded source files in one lexical scope. Only delimiters
/// are interpreted as electrical statements; file contents remain opaque.
///
/// # Errors
/// Returns an error for malformed, duplicate, nested or unmatched file delimiters.
pub fn embedded_files(items: &[Item]) -> Result<BTreeMap<String, Vec<&str>>> {
    let mut files = BTreeMap::new();
    let mut current: Option<(String, Vec<&str>)> = None;
    for item in items {
        let Item::Statement(statement) = item else {
            continue;
        };
        match file_boundary(statement) {
            Some(true) => {
                if current.is_some() {
                    bail!("nested SIMetrix embedded .file blocks are not supported");
                }
                let normalized = normalized_statement(statement);
                let tokens = statement_tokens(&normalized)?;
                let name = tokens
                    .get(1)
                    .context("SIMetrix embedded .file directive has no name")?
                    .trim_matches(['\'', '"'])
                    .to_ascii_uppercase();
                if name.is_empty() || tokens.len() != 2 {
                    bail!("malformed SIMetrix embedded .file directive {statement}");
                }
                current = Some((name, Vec::new()));
            }
            Some(false) => {
                let (name, body) = current
                    .take()
                    .context("SIMetrix embedded .endf without matching .file")?;
                if statement_tokens(&normalized_statement(statement))?.len() != 1 {
                    bail!("malformed SIMetrix embedded .endf directive {statement}");
                }
                if files.insert(name.clone(), body).is_some() {
                    bail!("duplicate SIMetrix embedded .file block {name}");
                }
            }
            None => {
                if let Some((_, body)) = &mut current {
                    body.push(statement.as_str());
                }
            }
        }
    }
    if let Some((name, _)) = current {
        bail!("unterminated embedded SIMetrix .file block {name}");
    }
    Ok(files)
}

/// Split physical deck lines into logical statements, preserving embedded payloads.
#[must_use = "the returned statements combine physical continuation lines"]
pub fn logical_statements(input: &str) -> Vec<String> {
    let lines = input.lines().map(str::to_owned).collect::<Vec<_>>();
    let mut statements = Vec::<String>::new();
    let mut embedded = false;
    for line in &lines {
        if let Some(start) = file_boundary(line) {
            embedded = start;
            statements.push(line.clone());
            continue;
        }
        if embedded {
            statements.push(line.clone());
            continue;
        }
        let trimmed = line.trim_start();
        if trimmed.starts_with('+')
            && let Some(statement) = statements.iter_mut().rev().find(|statement| {
                let candidate = statement.trim_start();
                !candidate.is_empty() && !candidate.starts_with('*')
            })
        {
            statement.push('\n');
            statement.push_str(line);
            continue;
        }
        if assignment_only_continuation(trimmed)
            && let Some(previous) = statements.last_mut()
            && bare_model_header(previous)
        {
            previous.push('\n');
            previous.push('+');
            previous.push_str(trimmed);
            continue;
        }
        statements.push(line.clone());
    }
    statements
}

fn subcircuit_name(statement: &str) -> Result<String> {
    let normalized = normalized_statement(statement);
    let mut fields = normalized.split_whitespace();
    let directive = fields.next().context("empty .subckt statement")?;
    if !directive.eq_ignore_ascii_case(".subckt") {
        bail!("expected .subckt statement");
    }
    fields
        .next()
        .map(str::to_owned)
        .context(".subckt statement has no name")
}

/// Parse lexical definitions while retaining source statements.
///
/// # Errors
/// Returns an error for unmatched or malformed subcircuit delimiters.
pub fn parse_items(input: &str) -> Result<Vec<Item>> {
    let mut roots = Vec::new();
    let mut stack: Vec<Subcircuit> = Vec::new();
    let mut embedded = false;
    for statement in logical_statements(input) {
        let boundary = file_boundary(&statement);
        let opaque = embedded || boundary.is_some();
        if let Some(start) = boundary {
            embedded = start;
        }
        let trimmed = statement.trim_start();
        if !opaque
            && trimmed
                .get(..7)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case(".subckt"))
            && trimmed
                .get(7..8)
                .is_some_and(|value| value.as_bytes().iter().all(u8::is_ascii_whitespace))
        {
            let name = subcircuit_name(&statement)?;
            stack.push(Subcircuit {
                header: statement,
                name,
                body: Vec::new(),
            });
            continue;
        }
        if !opaque
            && trimmed
                .get(..5)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case(".ends"))
            && trimmed
                .get(5..6)
                .is_none_or(|value| value.as_bytes().iter().all(u8::is_ascii_whitespace))
        {
            let block = stack.pop().context("unmatched .ends in SIMPLIS model")?;
            embedded_files(&block.body)?;
            if let Some(parent) = stack.last_mut() {
                parent.body.push(Item::Subcircuit(block));
            } else {
                roots.push(Item::Subcircuit(block));
            }
            continue;
        }
        if let Some(parent) = stack.last_mut() {
            parent.body.push(Item::Statement(statement));
        } else {
            roots.push(Item::Statement(statement));
        }
    }
    if let Some(block) = stack.last() {
        bail!("unterminated .subckt {}", block.name);
    }
    embedded_files(&roots)?;
    Ok(roots)
}

pub(crate) fn matching_behavioral_delimiter(
    input: &str,
    open: usize,
    opener: char,
    closer: char,
    underflow: &'static str,
    unterminated: &'static str,
) -> Result<usize> {
    let mut depth = 0usize;
    let mut quote = false;
    for (relative, character) in input[open..].char_indices() {
        if character == '\'' {
            quote = !quote;
            continue;
        }
        if quote {
            continue;
        }
        if character == opener {
            depth = depth.saturating_add(1);
        } else if character == closer {
            depth = depth.checked_sub(1).context(underflow)?;
            if depth == 0 {
                return Ok(open.saturating_add(relative));
            }
        }
    }
    bail!(unterminated)
}

/// Return the case-normalized model name and kind, if present.
#[must_use = "the model name and type identify the declaration"]
pub fn model_header(statement: &str) -> Option<(String, String)> {
    let normalized = normalized_statement(statement);
    let fields = normalized.split_whitespace().collect::<Vec<_>>();
    (fields.len() >= 3 && fields[0].eq_ignore_ascii_case(".MODEL")).then(|| {
        let kind = fields[2]
            .split_once('(')
            .map_or(fields[2], |(kind, _)| kind)
            .trim_start_matches('(');
        (fields[1].to_ascii_uppercase(), kind.to_ascii_uppercase())
    })
}

pub(crate) fn simple_assignment_span(statement: &str, name: &str) -> Option<(usize, usize, usize)> {
    let upper = statement.to_ascii_uppercase();
    let needle = name.to_ascii_uppercase();
    let mut offset = 0usize;
    while let Some(found) = upper[offset..].find(&needle) {
        let start = offset + found;
        let boundary = start == 0
            || upper.as_bytes()[start - 1].is_ascii_whitespace()
            || matches!(upper.as_bytes()[start - 1], b'(' | b',');
        let mut cursor = start + needle.len();
        if !boundary
            || upper
                .as_bytes()
                .get(cursor)
                .is_some_and(|byte| !byte.is_ascii_whitespace() && *byte != b'=')
        {
            offset = cursor;
            continue;
        }
        while upper
            .as_bytes()
            .get(cursor)
            .is_some_and(u8::is_ascii_whitespace)
        {
            cursor += 1;
        }
        if upper.as_bytes().get(cursor) != Some(&b'=') {
            offset = cursor;
            continue;
        }
        cursor += 1;
        while upper
            .as_bytes()
            .get(cursor)
            .is_some_and(u8::is_ascii_whitespace)
        {
            cursor += 1;
        }
        let value_start = cursor;
        if upper.as_bytes().get(cursor) == Some(&b'{') {
            cursor = matching_behavioral_delimiter(
                statement,
                cursor,
                '{',
                '}',
                "assignment brace underflow",
                "unterminated assignment expression",
            )
            .ok()?
                + 1;
        } else {
            while upper
                .as_bytes()
                .get(cursor)
                .is_some_and(|byte| !byte.is_ascii_whitespace() && !matches!(*byte, b')' | b','))
            {
                cursor += 1;
            }
        }
        if cursor == value_start {
            return None;
        }
        return Some((start, value_start, cursor));
    }
    None
}

/// Return a named scalar assignment, including balanced braces.
#[must_use = "the returned value is the requested assignment"]
pub fn simple_assignment_value(statement: &str, name: &str) -> Option<String> {
    let normalized = normalized_statement(statement);
    let (_, value_start, value_end) = simple_assignment_span(&normalized, name)?;
    Some(normalized[value_start..value_end].to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_payload_is_opaque_and_retains_physical_lines() -> Result<()> {
        let source = ".subckt outer a b\n.file data\n.subckt not_a_circuit\n.model imaginary D_LOGIC_BLOCK\n+ data\nunterminated ' text\n.ends not_a_circuit\n.endf\nR1 a b 1k\n.ends outer\n";
        let roots = parse_items(source)?;
        let [Item::Subcircuit(block)] = roots.as_slice() else {
            bail!("expected exactly one electrical subcircuit");
        };
        assert_eq!(block.name, "outer");
        let files = embedded_files(&block.body)?;
        assert_eq!(
            files.get("DATA").map(Vec::as_slice),
            Some(
                [
                    ".subckt not_a_circuit",
                    ".model imaginary D_LOGIC_BLOCK",
                    "+ data",
                    "unterminated ' text",
                    ".ends not_a_circuit",
                ]
                .as_slice()
            )
        );
        assert_eq!(electrical_statements(&block.body), ["R1 a b 1k"]);
        Ok(())
    }

    #[test]
    fn malformed_embedded_file_structure_fails_in_lexical_parser() {
        for source in [
            ".file\n.endf\n",
            ".file a b\n.endf\n",
            ".file a\n",
            ".endf\n",
            ".file a\n.endf more\n",
            ".file a\n.file b\n.endf\n.endf\n",
            ".file a\n.endf\n.file A\n.endf\n",
        ] {
            assert!(parse_items(source).is_err(), "{source}");
        }
    }

    #[test]
    fn file_identity_is_lexically_scoped() -> Result<()> {
        let source =
            ".file data\nroot\n.endf\n.subckt one a b\n.file data\nchild\n.endf\n.ends one\n";
        let roots = parse_items(source)?;
        assert_eq!(embedded_files(&roots)?.get("DATA"), Some(&vec!["root"]));
        let Some(Item::Subcircuit(child)) = roots.last() else {
            bail!("expected child definition");
        };
        assert_eq!(
            embedded_files(&child.body)?.get("DATA"),
            Some(&vec!["child"])
        );
        Ok(())
    }
}
