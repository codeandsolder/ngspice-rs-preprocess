//! Source-file language context, resolved before inclusion or specialization.
//!
//! See `SIMetrix` Simulator Reference, Netlist Format and Language Differences.
//! Behavioral source language is selected by its implementation syntax;
//! parameter expressions use the containing source's declared language.

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};

/// Default bound for the standalone source projection.
pub const DEFAULT_PROJECTION_BYTES: usize = 128 * 1024 * 1024;
mod identity;
pub mod model_mapping;
mod power;
pub use identity::{
    DeviceIdentity, ResolvedDeviceSource, ScopedStatement, resolve_device_identities,
};

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

/// Join physical continuation lines after stripping source-specific comments.
/// Quoting and brace state persists across continuation boundaries.
pub(super) fn logical_source(input: &str, context: DialectContext) -> Result<Vec<String>> {
    let mut statements = Vec::<String>::new();
    let mut active = None;
    let mut state = CommentState::default();
    let marker = if context.language == SourceLanguage::Hspice {
        '$'
    } else {
        ';'
    };
    for line in input.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('*') {
            statements.push(line.to_owned());
            continue;
        }
        if let Some(tail) = trimmed.strip_prefix('+') {
            let index: usize = active.context("orphan source continuation")?;
            statements[index].push(' ');
            statements[index].push_str(state.prefix(tail.trim_start(), marker));
        } else {
            state = CommentState::default();
            active = Some(statements.len());
            statements.push(state.prefix(line, marker).to_owned());
        }
    }
    Ok(statements)
}

const fn symbol_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$' | b'.')
}

/// Find a complete function call or expression group, retaining nesting.
fn group_end(input: &str, start: usize) -> Result<usize> {
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
            '(' => stack.push(')'),
            '{' => stack.push('}'),
            '[' => stack.push(']'),
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

fn split_arguments(input: &str) -> Result<Vec<&str>> {
    let mut args = Vec::new();
    let mut start = 0;
    let mut offset = 0;
    while offset < input.len() {
        let ch = input[offset..]
            .chars()
            .next()
            .context("expression boundary")?;
        match ch {
            '(' | '{' | '[' => offset = group_end(input, offset)? + 1,
            '"' | '\'' => {
                let end = input[offset + 1..]
                    .find(ch)
                    .context("unterminated expression quote")?;
                offset += end + 2;
            }
            ',' => {
                args.push(input[start..offset].trim());
                offset += 1;
                start = offset;
            }
            _ => offset += ch.len_utf8(),
        }
    }
    args.push(input[start..].trim());
    Ok(args)
}

fn lower_call(
    name: &str,
    body: &str,
    language: SourceLanguage,
    depth: usize,
    limit: usize,
) -> Result<String> {
    if name.eq_ignore_ascii_case("LOG") {
        let args = split_arguments(body)?;
        ensure!(
            args.len() == 1 && !args[0].is_empty(),
            "LOG requires one argument"
        );
        let target = if language == SourceLanguage::Simetrix {
            "log10"
        } else {
            "ln"
        };
        return Ok(format!(
            "{target}({})",
            rewrite_expression(body, language, depth + 1, true, limit)?
        ));
    }
    let args = split_arguments(body)?;
    let [base, exponent] = args.as_slice() else {
        bail!("PWR requires two arguments")
    };
    ensure!(
        !base.is_empty() && !exponent.is_empty(),
        "PWR requires two arguments"
    );
    let base = rewrite_expression(base, language, depth + 1, true, limit)?;
    let exponent = rewrite_expression(exponent, language, depth + 1, true, limit)?;
    if language == SourceLanguage::Simetrix {
        return power::guarded(&base, &exponent, limit);
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
    let power = format!("pow(abs({base}),{exponent})");
    if language == SourceLanguage::Hspice {
        Ok(format!("(sgn({base})*{power})"))
    } else {
        Ok(power)
    }
}

fn rewrite_expression(
    input: &str,
    language: SourceLanguage,
    depth: usize,
    grouped: bool,
    limit: usize,
) -> Result<String> {
    ensure!(depth < 128, "source expression nesting exceeds 128");
    ensure!(
        input.len() <= limit,
        "source expression byte budget exceeded"
    );
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
                group_end(input, offset)?
            };
            // A nested braced expression is a mathematical group, not a
            // second ngspice substitution. Canonicalize mixed quote forms.
            output.push(if grouped { '(' } else { '{' });
            output.push_str(&rewrite_expression(
                &input[offset + 1..end],
                language,
                depth + 1,
                true,
                limit.saturating_sub(output.len()),
            )?);
            output.push(if grouped { ')' } else { '}' });
            offset = end + 1;
        } else if (ch.is_ascii_alphabetic() || ch == '_')
            && (offset == 0 || !symbol_byte(bytes[offset - 1]))
        {
            let start = offset;
            offset += 1;
            while bytes.get(offset).copied().is_some_and(symbol_byte) {
                offset += 1;
            }
            let name = &input[start..offset];
            let mut open = offset;
            while bytes.get(open).is_some_and(u8::is_ascii_whitespace) {
                open += 1;
            }
            if bytes.get(open) == Some(&b'(')
                && ["LOG", "PWR", "V", "I"]
                    .iter()
                    .any(|candidate| name.eq_ignore_ascii_case(candidate))
            {
                let end = group_end(input, open)?;
                if name.eq_ignore_ascii_case("V") || name.eq_ignore_ascii_case("I") {
                    // Electrical names, not parameter expressions.
                    output.push_str(&input[start..=end]);
                } else {
                    output.push_str(&lower_call(
                        name,
                        &input[open + 1..end],
                        language,
                        depth,
                        limit.saturating_sub(output.len()),
                    )?);
                }
                offset = end + 1;
            } else {
                output.push_str(name);
            }
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
        power::lower(&output, limit)
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
pub(super) fn lower_statement(
    statement: &str,
    context: DialectContext,
    limit: usize,
) -> Result<String> {
    let trimmed = statement.trim_start();
    let Some(first) = trimmed.split_whitespace().next() else {
        return Ok(statement.to_owned());
    };
    let upper = first.to_ascii_uppercase();
    let (prefix_end, language) = if upper.starts_with('.') {
        let prefix = match upper.as_str() {
            ".PARAM" => token_end(trimmed, 1),
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
                group_end(trimmed, open)? + 1
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
    let mut output = trimmed[..prefix_end].to_owned();
    output.push_str(&rewrite_expression(
        &trimmed[prefix_end..],
        language,
        0,
        false,
        limit.saturating_sub(prefix_end),
    )?);
    if language == SourceLanguage::Simetrix && output.contains("**") {
        let start = match upper.as_str() {
            ".PARAM" | ".MODEL" | ".FUNC" => prefix_end,
            _ if matches!(upper.as_bytes()[0], b'R' | b'C' | b'L' | b'V' | b'I' | b'B') => {
                token_end(&output, 3)
            }
            _ => output.find('=').map_or(output.len(), |index| index + 1),
        };
        let lowered = power::lower(&output[start..], limit.saturating_sub(start))?;
        output.truncate(start);
        output.push_str(&lowered);
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

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
