//! Source-file language context, resolved before inclusion or specialization.
//!
//! See `SIMetrix` Simulator Reference, Netlist Format and Language Differences.
//! Behavioral source language is selected by its implementation syntax;
//! parameter expressions use the containing source's declared language.

use anyhow::{Context, Result, bail, ensure};
use model_preprocess_command::{Resource, WorkBudget};
use serde::{Deserialize, Serialize};

/// Default bound for the standalone source projection.
pub const DEFAULT_PROJECTION_BYTES: usize = 128 * 1024 * 1024;
mod identity;
pub mod model_mapping;
mod power;
pub use identity::{
    DeviceIdentity, ResolvedDeviceSource, ScopedStatement, resolve_device_identities,
};

/// Explicit power-expression capability of the consumer. Native output never
/// relies on solver-specific intrinsics. The prepared target selects the
/// typed SIMetrix scalar/runtime operations (version 1 of that contract).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PowerTarget {
    /// Portable native ngspice output, including the existing guarded laws.
    #[default]
    NativeNgspice,
    /// Consumer implements the explicit SIMetrix power intrinsic contract.
    PreparedSolverV1,
}

/// Parameter expression and inline-comment language.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceLanguage {
    #[default]
    Simetrix,
    Hspice,
    Pspice,
}

/// Effective language of a source resource, inherited by its children.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DialectContext {
    pub language: SourceLanguage,
    /// Device type/name separator; absent in compatibility languages.
    pub separator: Option<char>,
}

impl Default for DialectContext {
    fn default() -> Self {
        Self {
            language: SourceLanguage::Simetrix,
            separator: Some('$'),
        }
    }
}

/// Placement rules differ between a complete deck and an included resource.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceKind {
    Deck,
    Include,
}

impl DialectContext {
    /// Resolve the legal header declaration; declarations later in the
    /// resource are comments and cannot change the active language.
    #[must_use = "the resolved context must be applied to this resource"]
    pub fn resolve(input: &str, inherited: Self, kind: SourceKind) -> Self {
        let mut lines = input.lines();
        let first = lines.next().unwrap_or_default();
        let second = lines.next().unwrap_or_default();
        let declaration = match kind {
            SourceKind::Deck => parse_declaration(second),
            SourceKind::Include => {
                let first_declaration = parse_declaration(first);
                if first_declaration.is_some() {
                    first_declaration
                } else if first.trim_start().starts_with('*') || first.trim().is_empty() {
                    parse_declaration(second)
                } else {
                    None
                }
            }
        };
        declaration.unwrap_or(inherited)
    }

    /// Split the physical device token into its type and displayed identity.
    ///
    /// # Errors
    /// A separated device must have a nonempty identity.
    pub fn device_identity(self, token: &str) -> Result<&str> {
        let mut chars = token.chars();
        let _ = chars.next().context("empty device token")?;
        let Some(separator) = self.separator else {
            return Ok(token);
        };
        if chars.next() != Some(separator) {
            return Ok(token);
        }
        let identity = chars.as_str();
        ensure!(
            !identity.is_empty(),
            "empty separated device identity {token}"
        );
        Ok(identity)
    }

    /// Remove an inline comment outside quoted strings and expression groups.
    #[must_use = "the active text is the source to be interpreted"]
    pub fn active_prefix(self, line: &str) -> &str {
        let marker = if self.language == SourceLanguage::Hspice {
            '$'
        } else {
            ';'
        };
        active_prefix(line, marker)
    }
}

fn parse_declaration(line: &str) -> Option<DialectContext> {
    let mut fields = line.split_ascii_whitespace();
    let header = fields.next()?;
    let language = match header.to_ascii_uppercase().as_str() {
        "*#SIMETRIX" => SourceLanguage::Simetrix,
        "*#HSPICE" => SourceLanguage::Hspice,
        "*#PSPICE" => SourceLanguage::Pspice,
        _ => return None,
    };
    let mut context = DialectContext {
        language,
        separator: (language == SourceLanguage::Simetrix).then_some('$'),
    };
    for field in fields {
        let Some((name, value)) = field.split_once('=') else {
            continue;
        };
        if language == SourceLanguage::Simetrix && name.eq_ignore_ascii_case("sep") {
            if value.eq_ignore_ascii_case("none") {
                context.separator = None;
            } else {
                let mut chars = value.chars();
                if let Some(character) = chars.next()
                    && chars.next().is_none()
                {
                    context.separator = Some(character);
                }
                // The reference ignores multi-character separator options.
            }
        }
    }
    Some(context)
}

#[derive(Default)]
struct CommentState {
    quote: Option<char>,
    braces: usize,
}

impl CommentState {
    fn prefix<'a>(&mut self, line: &'a str, marker: char) -> &'a str {
        for (offset, character) in line.char_indices() {
            if let Some(delimiter) = self.quote {
                if character == delimiter {
                    self.quote = None;
                }
                continue;
            }
            match character {
                '\'' | '"' => self.quote = Some(character),
                '{' => self.braces = self.braces.saturating_add(1),
                '}' => self.braces = self.braces.saturating_sub(1),
                _ if character == marker && self.braces == 0 => return &line[..offset],
                _ => {}
            }
        }
        line
    }
}

pub(super) fn active_prefix(line: &str, marker: char) -> &str {
    CommentState::default().prefix(line, marker)
}

/// A projected source statement with its admitted physical source span.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectedStatement {
    /// Joined active source text, or a retained comment.
    pub text: String,
    /// First one-based physical source line.
    pub first_line: usize,
    /// Last one-based physical source line, including continuations.
    pub last_line: usize,
}

/// Opaque file and command bodies have independent closing delimiters.
#[derive(Clone, Copy)]
enum OpaqueBlock {
    File,
    Control,
}

/// Track an opaque body without interpreting directives within its payload.
#[derive(Default)]
pub(super) struct OpaqueSource(Option<OpaqueBlock>);

impl OpaqueSource {
    pub(super) fn consume(&mut self, statement: &str) -> bool {
        let first = statement
            .split_ascii_whitespace()
            .next()
            .unwrap_or_default();
        match self.0 {
            Some(block) => {
                let close = match block {
                    OpaqueBlock::File => ".endf",
                    OpaqueBlock::Control => ".endc",
                };
                if first.eq_ignore_ascii_case(close) {
                    self.0 = None;
                }
                true
            }
            None if first.eq_ignore_ascii_case(".file") => {
                self.0 = Some(OpaqueBlock::File);
                true
            }
            None if first.eq_ignore_ascii_case(".control") => {
                self.0 = Some(OpaqueBlock::Control);
                true
            }
            None => false,
        }
    }

    pub(super) fn finish(&self) -> Result<()> {
        ensure!(self.0.is_none(), "unterminated opaque source block");
        Ok(())
    }
}

/// Join physical continuation lines after stripping source-specific comments.
/// Quoting and brace state persists across continuation boundaries.
pub(super) fn logical_source(
    input: &str,
    context: DialectContext,
) -> Result<Vec<ProjectedStatement>> {
    let mut statements = Vec::<ProjectedStatement>::new();
    let mut active = None;
    let mut state = CommentState::default();
    let mut opaque = OpaqueSource::default();
    let marker = if context.language == SourceLanguage::Hspice {
        '$'
    } else {
        ';'
    };
    for (index, line) in input.lines().enumerate() {
        let line_number = index + 1;
        if opaque.consume(line) {
            statements.push(ProjectedStatement {
                text: line.to_owned(),
                first_line: line_number,
                last_line: line_number,
            });
            active = None;
            continue;
        }
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('*') {
            statements.push(ProjectedStatement {
                text: line.to_owned(),
                first_line: line_number,
                last_line: line_number,
            });
            continue;
        }
        if let Some(tail) = trimmed.strip_prefix('+') {
            let index: usize = active.context("orphan source continuation")?;
            statements[index].text.push(' ');
            statements[index]
                .text
                .push_str(state.prefix(tail.trim_start(), marker));
            statements[index].last_line = line_number;
        } else {
            state = CommentState::default();
            active = Some(statements.len());
            statements.push(ProjectedStatement {
                text: state.prefix(line, marker).to_owned(),
                first_line: line_number,
                last_line: line_number,
            });
        }
    }
    opaque.finish()?;
    Ok(statements)
}

const fn symbol_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$' | b'.')
}

/// Find a complete function call or expression group, retaining nesting.
pub(super) fn group_end(input: &str, start: usize, budget: &WorkBudget) -> Result<usize> {
    let mut stack = Vec::new();
    let mut quote = None;
    for (relative, ch) in input[start..].char_indices() {
        if let Some(delimiter) = quote {
            if ch == delimiter {
                quote = None;
            }
            continue;
        }
        match ch {
            '"' | '\'' => quote = Some(ch),
            '(' | '{' | '[' => {
                budget.check_depth(stack.len().saturating_add(1))?;
                stack.push(match ch {
                    '(' => ')',
                    '{' => '}',
                    _ => ']',
                });
            }
            ')' | '}' | ']' => {
                ensure!(
                    stack.pop() == Some(ch),
                    "mismatched expression delimiters in {input}"
                );
                if stack.is_empty() {
                    return Ok(start + relative);
                }
            }
            _ => {}
        }
    }
    bail!("unterminated expression group in {input}")
}

pub(super) fn split_arguments<'a>(input: &'a str, budget: &WorkBudget) -> Result<Vec<&'a str>> {
    let mut args = Vec::new();
    let mut start = 0;
    let mut offset = 0;
    while offset < input.len() {
        let ch = input[offset..]
            .chars()
            .next()
            .context("expression boundary")?;
        match ch {
            '(' | '{' | '[' => offset = group_end(input, offset, budget)? + 1,
            '"' | '\'' => {
                let end = input[offset + 1..]
                    .find(ch)
                    .context("unterminated expression quote")?;
                offset += end + 2;
            }
            ',' => {
                budget.charge(Resource::GeneratedNodes, 1)?;
                args.push(input[start..offset].trim());
                offset += 1;
                start = offset;
            }
            _ => offset += ch.len_utf8(),
        }
    }
    budget.charge(Resource::GeneratedNodes, 1)?;
    args.push(input[start..].trim());
    Ok(args)
}

#[derive(Clone, Copy)]
struct ExpressionProjection {
    language: SourceLanguage,
    target: PowerTarget,
    numeric_suffixes: bool,
}

impl ExpressionProjection {
    const fn native(language: SourceLanguage, numeric_suffixes: bool) -> Self {
        Self {
            language,
            target: PowerTarget::NativeNgspice,
            numeric_suffixes,
        }
    }

    const fn prepared(self) -> bool {
        matches!(self.language, SourceLanguage::Simetrix)
            && matches!(self.target, PowerTarget::PreparedSolverV1)
    }
}

fn lower_call(
    name: &str,
    body: &str,
    projection: ExpressionProjection,
    limit: usize,
    budget: &WorkBudget,
) -> Result<String> {
    let language = projection.language;
    if name.eq_ignore_ascii_case("LOG") {
        let args = split_arguments(body, budget)?;
        ensure!(
            args.len() == 1 && !args[0].is_empty(),
            "LOG requires one argument"
        );
        let target = if language == SourceLanguage::Simetrix {
            "log10"
        } else {
            "ln"
        };
        let argument = rewrite_expression(body, projection, true, limit, budget)?;
        budget.charge(
            Resource::EmittedBytes,
            target
                .len()
                .saturating_add(argument.len())
                .saturating_add(2),
        )?;
        return Ok(format!("{target}({argument})"));
    }
    let args = split_arguments(body, budget)?;
    let [base, exponent] = args.as_slice() else {
        bail!("{name} requires two arguments")
    };
    ensure!(
        !base.is_empty() && !exponent.is_empty(),
        "{name} requires two arguments"
    );
    let base = rewrite_expression(base, projection, true, limit, budget)?;
    let exponent = rewrite_expression(exponent, projection, true, limit, budget)?;
    if projection.prepared() {
        let operation = if name.eq_ignore_ascii_case("PWR") {
            "simetrix_abs_pow"
        } else {
            "simetrix_pow"
        };
        let len = operation
            .len()
            .saturating_add(base.len())
            .saturating_add(exponent.len())
            .saturating_add(3);
        ensure!(
            len <= limit,
            "source expression expansion byte budget exceeded"
        );
        budget.charge(Resource::EmittedBytes, len)?;
        return Ok(format!("{operation}({base},{exponent})"));
    }
    if language == SourceLanguage::Simetrix {
        return power::guarded(&base, &exponent, limit, budget);
    }
    // ngspice's behavioral PWR is signed, but its static PWR is unsigned.
    // Use the same explicit mathematical operators in both contexts.
    let expansion = base
        .len()
        .saturating_mul(if language == SourceLanguage::Hspice {
            2
        } else {
            1
        })
        .saturating_add(exponent.len())
        .saturating_add(32);
    ensure!(
        expansion <= limit,
        "source expression expansion byte budget exceeded"
    );
    budget.charge(Resource::EmittedBytes, expansion)?;
    let power = format!("pow(abs({base}),{exponent})");
    if language == SourceLanguage::Hspice {
        Ok(format!("(sgn({base})*{power})"))
    } else {
        Ok(power)
    }
}

// Return the complete literal and its engineering suffix. A D/E is an
// exponent marker only when followed by a signed or unsigned decimal integer.
fn numeric_literal(bytes: &[u8], start: usize) -> (usize, usize) {
    let mut end = start;
    while bytes.get(end).is_some_and(u8::is_ascii_digit) {
        end += 1;
    }
    if bytes.get(end) == Some(&b'.') {
        end += 1;
        while bytes.get(end).is_some_and(u8::is_ascii_digit) {
            end += 1;
        }
    }
    if matches!(bytes.get(end), Some(b'e' | b'E' | b'd' | b'D')) {
        let mut exponent = end + 1;
        exponent += usize::from(matches!(bytes.get(exponent), Some(b'+' | b'-')));
        let digits = exponent;
        while bytes.get(exponent).is_some_and(u8::is_ascii_digit) {
            exponent += 1;
        }
        if exponent > digits {
            end = exponent;
        }
    }
    let suffix = end;
    while bytes.get(end).is_some_and(u8::is_ascii_alphabetic) {
        end += 1;
    }
    (end, suffix)
}

fn append_hspice_number(
    input: &str,
    start: usize,
    output: &mut String,
    budget: &WorkBudget,
) -> Result<usize> {
    let bytes = input.as_bytes();
    let (end, suffix) = numeric_literal(bytes, start);
    if matches!(bytes.get(suffix), Some(b'x' | b'X')) {
        budget.charge(Resource::EmittedBytes, 2)?;
        output.push_str(&input[start..suffix]);
        output.push_str("meg");
        output.push_str(&input[suffix + 1..end]);
    } else {
        output.push_str(&input[start..end]);
    }
    Ok(end)
}

/// Lower one expression region of a standalone SPICE dialect.
pub(super) fn lower_expression(
    input: &str,
    language: SourceLanguage,
    limit: usize,
    budget: &WorkBudget,
) -> Result<String> {
    rewrite_expression(
        input,
        ExpressionProjection::native(language, true),
        false,
        limit,
        budget,
    )
}

fn append_expression_identifier(
    input: &str,
    start: usize,
    output: &mut String,
    projection: ExpressionProjection,
    limit: usize,
    budget: &WorkBudget,
) -> Result<usize> {
    let bytes = input.as_bytes();
    let mut offset = start + 1;
    while bytes.get(offset).copied().is_some_and(symbol_byte) {
        offset += 1;
    }
    let name = &input[start..offset];
    let mut open = offset;
    while bytes.get(open).is_some_and(u8::is_ascii_whitespace) {
        open += 1;
    }
    if bytes.get(open) == Some(&b'(')
        && (["LOG", "PWR", "V", "I"]
            .iter()
            .any(|candidate| name.eq_ignore_ascii_case(candidate))
            || projection.prepared() && name.eq_ignore_ascii_case("POW"))
    {
        let end = group_end(input, open, budget)?;
        if name.eq_ignore_ascii_case("V") || name.eq_ignore_ascii_case("I") {
            // Electrical names, not parameter expressions.
            output.push_str(&input[start..=end]);
        } else {
            let lowered = lower_call(
                name,
                &input[open + 1..end],
                projection,
                limit.saturating_sub(output.len()),
                budget,
            )?;
            budget.charge(
                Resource::EmittedBytes,
                lowered.len().saturating_sub(end + 1 - start),
            )?;
            output.push_str(&lowered);
        }
        offset = end + 1;
    } else {
        output.push_str(name);
    }
    Ok(offset)
}

fn rewrite_expression(
    input: &str,
    projection: ExpressionProjection,
    grouped: bool,
    limit: usize,
    budget: &WorkBudget,
) -> Result<String> {
    let language = projection.language;
    if grouped && projection.prepared() {
        let lowered = power::lower_runtime(input, limit, budget)?;
        return rewrite_expression(
            &lowered,
            ExpressionProjection::native(language, projection.numeric_suffixes),
            true,
            limit,
            budget,
        );
    }
    let _frame = budget.enter()?;
    budget.charge(Resource::EvaluationSteps, input.len())?;
    ensure!(
        input.len() <= limit,
        "source expression byte budget exceeded"
    );
    budget.charge(Resource::EmittedBytes, input.len())?;
    let mut output = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut offset = 0;
    while offset < bytes.len() {
        let ch = input[offset..]
            .chars()
            .next()
            .context("expression boundary")?;
        if ch == '"' {
            let end = input[offset + 1..]
                .find('"')
                .context("unterminated string")?
                + offset
                + 2;
            output.push_str(&input[offset..end]);
            offset = end;
        } else if ch == '\'' || ch == '{' {
            let end = if ch == '\'' {
                input[offset + 1..]
                    .find(ch)
                    .context("unterminated expression quote")?
                    + offset
                    + 1
            } else {
                group_end(input, offset, budget)?
            };
            // A nested braced expression is a mathematical group, not a
            // second ngspice substitution. Canonicalize mixed quote forms.
            output.push(if grouped { '(' } else { '{' });
            let lowered = rewrite_expression(
                &input[offset + 1..end],
                projection,
                true,
                limit.saturating_sub(output.len()),
                budget,
            )?;
            budget.charge(
                Resource::EmittedBytes,
                lowered.len().saturating_sub(end - offset - 1),
            )?;
            output.push_str(&lowered);
            output.push(if grouped { ')' } else { '}' });
            offset = end + 1;
        } else if projection.numeric_suffixes
            && language == SourceLanguage::Hspice
            && (offset == 0 || !symbol_byte(bytes[offset - 1]))
            && (ch.is_ascii_digit()
                || (ch == '.' && bytes.get(offset + 1).is_some_and(u8::is_ascii_digit)))
        {
            offset = append_hspice_number(input, offset, &mut output, budget)?;
        } else if (ch.is_ascii_alphabetic() || ch == '_')
            && (offset == 0 || !symbol_byte(bytes[offset - 1]))
        {
            offset = append_expression_identifier(
                input,
                offset,
                &mut output,
                projection,
                limit,
                budget,
            )?;
        } else {
            output.push(ch);
            offset += ch.len_utf8();
        }
        ensure!(
            output.len() <= limit,
            "source expression expansion byte budget exceeded"
        );
    }
    if grouped && language == SourceLanguage::Simetrix {
        power::lower_with_budget(&output, limit, budget)
    } else {
        Ok(output)
    }
}

fn token_end(input: &str, count: usize) -> usize {
    let mut rest = input;
    for _ in 0..count {
        rest = rest.trim_start();
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        rest = &rest[end..];
    }
    input.len() - rest.len()
}

/// Lower language-dependent expression functions in a joined statement.
/// Source/path strings, declarations and probe identifiers are not expressions.
///
/// # Errors
/// Invalid function arity or unbalanced expression delimiters.
#[cfg(test)]
fn lower_statement(statement: &str, context: DialectContext, limit: usize) -> Result<String> {
    lower_statement_with_budget(statement, context, limit, &WorkBudget::default())
}

pub(super) fn lower_statement_with_budget(
    statement: &str,
    context: DialectContext,
    limit: usize,
    budget: &WorkBudget,
) -> Result<String> {
    lower_statement_targeted(
        statement,
        context,
        PowerTarget::NativeNgspice,
        limit,
        budget,
    )
}

/// The prepared target projects source runtime laws before portable guarded
/// power lowering can erase their operation identity. Scalar expressions
/// and shared function bodies carry explicit operation names, interpreted
/// by the consumer in their actual evaluation phase.
pub(super) fn lower_statement_targeted(
    statement: &str,
    context: DialectContext,
    target: PowerTarget,
    limit: usize,
    budget: &WorkBudget,
) -> Result<String> {
    let trimmed = statement.trim_start();
    let Some(first) = trimmed.split_whitespace().next() else {
        return Ok(statement.to_owned());
    };
    let upper = first.to_ascii_uppercase();
    let (prefix_end, language) = if upper.starts_with('.') {
        let prefix = match upper.as_str() {
            ".PARAM" | ".TRAN" | ".AC" | ".TEMP" => token_end(trimmed, 1),
            ".MODEL" => {
                let start = token_end(trimmed, 2);
                // Stop after the model family, including the compact D(...).
                let tail = trimmed[start..].trim_start();
                start + trimmed[start..].len() - tail.len()
                    + tail
                        .find(|ch: char| ch.is_whitespace() || ch == '(')
                        .unwrap_or(tail.len())
            }
            ".FUNC" => {
                let open = trimmed.find('(').context("missing function arguments")?;
                group_end(trimmed, open, budget)? + 1
            }
            ".SUBCKT" => token_end(trimmed, 2),
            _ => return Ok(statement.to_owned()),
        };
        (prefix, context.language)
    } else if first.starts_with('*') {
        return Ok(statement.to_owned());
    } else {
        let prefix = token_end(trimmed, 1);
        let device = upper.as_bytes().first().copied().unwrap_or_default();
        let behavior = &trimmed[token_end(trimmed, 3)..];
        let assignment = behavior
            .trim_start()
            .split(|ch: char| ch.is_whitespace() || ch == '=' || ch == '{')
            .next()
            .unwrap_or_default();
        let language = match (device, assignment.to_ascii_uppercase().as_str()) {
            (b'B', _) => SourceLanguage::Simetrix,
            (b'E'..=b'H', "VALUE") => SourceLanguage::Pspice,
            (b'E' | b'G', "VOL" | "CUR") => SourceLanguage::Hspice,
            _ => context.language,
        };
        (prefix, language)
    };
    let projection = ExpressionProjection {
        language,
        target,
        numeric_suffixes: false,
    };
    if projection.prepared()
        && let Some(output) =
            prepared_statement(trimmed, &upper, prefix_end, projection, limit, budget)?
    {
        return Ok(output);
    }
    let mut output = trimmed[..prefix_end].to_owned();
    output.push_str(&rewrite_expression(
        &trimmed[prefix_end..],
        projection,
        false,
        limit.saturating_sub(prefix_end),
        budget,
    )?);
    if language == SourceLanguage::Simetrix && output.contains("**") {
        let start = match upper.as_str() {
            ".PARAM" | ".MODEL" | ".FUNC" => prefix_end,
            _ if matches!(upper.as_bytes()[0], b'R' | b'C' | b'L' | b'V' | b'I' | b'B') => {
                token_end(&output, 3)
            }
            _ => output.find('=').map_or(output.len(), |index| index + 1),
        };
        let lowered =
            power::lower_with_budget(&output[start..], limit.saturating_sub(start), budget)?;
        output.truncate(start);
        output.push_str(&lowered);
    }
    Ok(output)
}

fn prepared_statement(
    statement: &str,
    directive: &str,
    prefix_end: usize,
    projection: ExpressionProjection,
    limit: usize,
    budget: &WorkBudget,
) -> Result<Option<String>> {
    if directive == ".PARAM" {
        return prepared_parameters(statement, prefix_end, limit, budget).map(Some);
    }
    if directive == ".FUNC" {
        let body = statement[prefix_end..].trim_start();
        let body = body.strip_prefix('=').unwrap_or(body);
        let mut output = statement[..prefix_end].to_owned();
        power::append(&mut output, " ", limit, budget)?;
        let expression = prepared_expression(body, limit.saturating_sub(output.len()), budget)?;
        power::append(&mut output, &expression, limit, budget)?;
        return Ok(Some(output));
    }
    let Some((start, end)) = runtime_law(statement, directive, budget)? else {
        return Ok(None);
    };
    let mut output = statement[..start].to_owned();
    let expression = prepared_expression(
        &statement[start..end],
        limit.saturating_sub(output.len()),
        budget,
    )?;
    power::append(&mut output, &expression, limit, budget)?;
    let setters = rewrite_expression(
        &statement[end..],
        projection,
        false,
        limit.saturating_sub(output.len()),
        budget,
    )?;
    power::append(&mut output, &setters, limit, budget)?;
    Ok(Some(output))
}

/// Static parameter assignments use the same explicit source operation names,
/// but the consumer evaluates them with its separate scalar grammar and
/// arithmetic contract. Do not copy the behavioural zero guard into numparam.
fn prepared_parameters(
    statement: &str,
    prefix_end: usize,
    limit: usize,
    budget: &WorkBudget,
) -> Result<String> {
    let tail = &statement[prefix_end..];
    budget.charge(Resource::ScannedBytes, tail.len())?;
    let assignments = crate::parameter_assignments(tail)?;
    let mut output = statement[..prefix_end].to_owned();
    for (name, value) in assignments {
        budget.charge(Resource::GeneratedNodes, 1)?;
        for part in [" ", name, "="] {
            power::append(&mut output, part, limit, budget)?;
        }
        let value = prepared_expression(value, limit.saturating_sub(output.len()), budget)?;
        power::append(&mut output, &value, limit, budget)?;
    }
    Ok(output)
}

/// Detect the actual electrical law rather than rewrite the whole device
/// card (which also contains node names and scalar instance setters).
fn runtime_law(
    statement: &str,
    first: &str,
    budget: &WorkBudget,
) -> Result<Option<(usize, usize)>> {
    let Some(kind) = first.as_bytes().first() else {
        return Ok(None);
    };
    if !matches!(kind, b'B' | b'E' | b'G' | b'F' | b'H' | b'R' | b'C' | b'L') {
        return Ok(None);
    }
    let tail = token_end(statement, 3);
    let remaining = &statement[tail..];
    let Some((key, _)) = remaining.split_once('=') else {
        return Ok(None);
    };
    let key = key.trim();
    let valid = match kind {
        b'B' | b'E' | b'G' | b'F' | b'H' => {
            ["V", "I"].iter().any(|name| key.eq_ignore_ascii_case(name))
        }
        b'R' => key.eq_ignore_ascii_case("R"),
        b'C' => key.eq_ignore_ascii_case("C") || key.eq_ignore_ascii_case("Q"),
        b'L' => key.eq_ignore_ascii_case("L"),
        _ => false,
    };
    if !valid {
        return Ok(None);
    }
    let start = tail
        + remaining
            .find('=')
            .context("missing runtime law assignment")?
        + 1;
    let body = &statement[start..];
    let mut offset = 0;
    while offset < body.len() {
        let ch = body[offset..]
            .chars()
            .next()
            .context("runtime expression boundary")?;
        match ch {
            '(' | '{' | '[' => offset = group_end(body, offset, budget)? + 1,
            '\'' | '"' => {
                offset += body[offset + 1..]
                    .find(ch)
                    .context("unterminated runtime law quote")?
                    + 2;
            }
            ch if ch.is_ascii_whitespace() => {
                let next = body[offset..].trim_start();
                // Only a known scalar setter delimits an unquoted expression;
                // equality inside the electrical law stays arithmetic.
                let key_end = next.bytes().take_while(|byte| symbol_byte(*byte)).count();
                let name = &next[..key_end];
                let setter = next[key_end..].trim_start();
                if [
                    "M",
                    "TC1",
                    "TC2",
                    "TEMP",
                    "DTEMP",
                    "RECIPROCTC",
                    "IC",
                    "SCALE",
                    "NOISE",
                    "NOISY",
                ]
                .iter()
                .any(|candidate| name.eq_ignore_ascii_case(candidate))
                    && setter.starts_with('=')
                    && !setter.starts_with("==")
                {
                    return Ok(Some((start, start + offset)));
                }
                offset += body[offset..].len() - next.len();
            }
            _ => offset += ch.len_utf8(),
        }
    }
    Ok(Some((start, statement.len())))
}

fn prepared_expression(input: &str, limit: usize, budget: &WorkBudget) -> Result<String> {
    let trimmed = input.trim();
    // Keep an outer expression delimiter, which carries source-card syntax.
    let (body, wrapper) =
        if trimmed.starts_with('{') && group_end(trimmed, 0, budget)? + 1 == trimmed.len() {
            (&trimmed[1..trimmed.len() - 1], Some(('{', '}')))
        } else if trimmed.starts_with('\'')
            && trimmed[1..].find('\'') == Some(trimmed.len().saturating_sub(2))
        {
            (&trimmed[1..trimmed.len() - 1], Some(('{', '}')))
        } else {
            (trimmed, None)
        };
    let lowered = power::lower_runtime(body, limit, budget)?;
    let lowered = rewrite_expression(
        &lowered,
        ExpressionProjection::native(SourceLanguage::Simetrix, false),
        true,
        limit,
        budget,
    )?;
    if let Some((open, close)) = wrapper {
        budget.charge(Resource::EmittedBytes, lowered.len().saturating_add(2))?;
        ensure!(
            lowered.len().saturating_add(2) <= limit,
            "source power expansion byte budget exceeded"
        );
        Ok(format!("{open}{lowered}{close}"))
    } else {
        Ok(lowered)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepared_scalar_and_runtime_regions_preserve_phase_specific_names() -> Result<()> {
        let budget = WorkBudget::default();
        let context = DialectContext::default();
        for (input, expected) in [
            (
                ".param a={0**.5} b=-2^3 c={PWR(2,-3)}",
                ".param a={simetrix_abs_pow(0,.5)} b=simetrix_caret_pow(-2,3) c={simetrix_abs_pow(2,-3)}",
            ),
            (
                ".func f(x,p) = {x**p}",
                ".func f(x,p) {simetrix_abs_pow(x,p)}",
            ),
            (
                "B1 a 0 V=0*PWR(V(x),-1) TC1={2^3}",
                "B1 a 0 V=simetrix_abs_pow_product(0,V(x),-1) TC1={simetrix_caret_pow(2,3)}",
            ),
            ("R1 a 0 {PWR(v(x),2)}", "R1 a 0 {simetrix_abs_pow(v(x),2)}"),
            (
                ".model m d(is={0**.5})",
                ".model m d(is={simetrix_abs_pow(0,.5)})",
            ),
        ] {
            let actual = lower_statement_targeted(
                input,
                context,
                PowerTarget::PreparedSolverV1,
                4096,
                &budget,
            )?;
            assert_eq!(actual, expected, "{input}");
        }
        Ok(())
    }

    #[test]
    fn header_overrides_are_local_and_late_declarations_are_comments() -> Result<()> {
        let root = DialectContext::resolve(
            "* title\n*#SIMETRIX sep=:\nR:A a 0 1k\n",
            DialectContext::default(),
            SourceKind::Deck,
        );
        assert_eq!(root.separator, Some(':'));
        let child = DialectContext::resolve("*#HSPICE\n.param x=1\n", root, SourceKind::Include);
        assert_eq!(child.language, SourceLanguage::Hspice);
        assert_eq!(child.separator, None);
        assert_eq!(
            DialectContext::resolve(".param y=2\n*#PSPICE\n", child, SourceKind::Include),
            child
        );
        assert_eq!(
            DialectContext::resolve("*#PSPICE\n", root, SourceKind::Include).language,
            SourceLanguage::Pspice
        );
        assert_eq!(root.device_identity("R:A")?, "A");
        assert_eq!(child.device_identity("R:A")?, "R:A");
        assert!(root.device_identity("R:").is_err());
        Ok(())
    }

    #[test]
    fn absolute_power_is_lowered_only_in_its_source_language() -> Result<()> {
        let native = DialectContext::default();
        let hspice = DialectContext {
            language: SourceLanguage::Hspice,
            separator: None,
        };
        let pspice = DialectContext {
            language: SourceLanguage::Pspice,
            separator: None,
        };
        for source in [
            ".param gain=2**3",
            ".func f(x) x**3",
            "R1 a b 2**3",
            "C1 p n Q=(V(p,n)**3)",
            "B1 out 0 V=V(a)**.5",
        ] {
            let lowered = lower_statement(source, native, 4096)?;
            assert!(!lowered.contains("**"), "{lowered}");
            assert!(lowered.contains("abs("), "{lowered}");
        }
        for context in [hspice, pspice] {
            assert_eq!(
                lower_statement(".param a={-2**3}", context, 4096)?,
                ".param a={-2**3}"
            );
            assert_eq!(
                lower_statement("E1 p n VALUE={V(a)**3}", context, 4096)?,
                "E1 p n VALUE={V(a)**3}"
            );
            assert!(!lower_statement("B1 p n V=V(a)**3", context, 4096)?.contains("**"));
        }
        assert_eq!(
            lower_statement("R1 p**q n 1k", native, 4096)?,
            "R1 p**q n 1k"
        );
        Ok(())
    }

    #[test]
    fn intermediate_power_expansion_consumes_the_request_ledger() -> Result<()> {
        use model_preprocess_command::{Cancellation, WorkLimits};
        let hspice = DialectContext {
            language: SourceLanguage::Hspice,
            separator: None,
        };
        let mut nested = "2".to_owned();
        for _ in 0..10 {
            nested = format!("PWR({nested},1)");
        }
        for (expression, context) in [
            (nested, hspice),
            (format!("{}1", "2**".repeat(10)), DialectContext::default()),
        ] {
            let source = format!(".param gain={{{expression}}}");
            let budget = WorkBudget::new(
                WorkLimits {
                    emitted_bytes: 4096,
                    ..WorkLimits::default()
                },
                Cancellation::default(),
            );
            let error =
                lower_statement_with_budget(&source, context, DEFAULT_PROJECTION_BYTES, &budget)
                    .err()
                    .context("intermediate expression expansion bypassed the request limit")?;
            assert!(
                error.to_string().contains("EmittedBytes budget exceeded"),
                "{error:#}"
            );
            assert!(budget.check().is_err());
            let lowered = lower_statement(&source, context, DEFAULT_PROJECTION_BYTES)?;
            assert!(lowered.len() > source.len());
        }
        Ok(())
    }

    #[test]
    fn comments_respect_dialect_and_quoted_paths() {
        let hs =
            DialectContext::resolve("*#HSPICE", DialectContext::default(), SourceKind::Include);
        assert_eq!(
            hs.active_prefix(".include \"a$b.lib\" $ comment"),
            ".include \"a$b.lib\" "
        );
        assert_eq!(
            DialectContext::default().active_prefix("V$A a 0 1 ; comment"),
            "V$A a 0 1 "
        );
        assert_eq!(
            DialectContext::default().active_prefix(".inc 'a;b.lib' ; comment"),
            ".inc 'a;b.lib' "
        );
        assert_eq!(
            DialectContext::default().active_prefix(".inc \"a;b.lib\" ; comment"),
            ".inc \"a;b.lib\" "
        );
    }
}
