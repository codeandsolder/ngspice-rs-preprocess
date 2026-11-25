//! Physical-source termination of the proprietary inline ALB definition.
//! DEF is the final recognized parameter even if its closing quote is
//! followed by other text. Continuations belonging to the definition
//! are legal; continuations following the closing quote are not.

use super::model_header;
use anyhow::{Context, Result, ensure};
use std::ops::Range;

/// Locate the assignment token outside quoted values. Alphabetic model
/// identifiers or quoted strings containing DEF are not parameter names.
fn parameter_offset(statement: &str) -> Option<usize> {
    let mut tail = statement.trim_start();
    for _ in 0..2 {
        tail = tail.get(tail.find(char::is_whitespace)?..)?.trim_start();
    }
    let kind_width = tail
        .find(|ch: char| ch.is_whitespace() || ch == '(')
        .unwrap_or(tail.len());
    Some(statement.len() - tail.len() + kind_width)
}

fn definition_start(statement: &str) -> Option<(usize, usize)> {
    let mut quote = None;
    let start = parameter_offset(statement)?;
    for (index, byte) in statement.bytes().enumerate().skip(start) {
        if let Some(delimiter) = quote {
            if byte == delimiter {
                quote = None;
            }
            continue;
        }
        match byte {
            b'\'' | b'"' => quote = Some(byte),
            b';' => return None,
            b'd' | b'D'
                if index == 0
                    || statement.as_bytes()[index - 1].is_ascii_whitespace()
                    || matches!(statement.as_bytes()[index - 1], b'(' | b',') =>
            {
                let Some(name) = statement.get(index..index + 3) else {
                    continue;
                };
                if !name.eq_ignore_ascii_case("DEF") {
                    continue;
                }
                let tail = &statement[index + 3..];
                if tail.is_empty() || tail.starts_with('=') || tail.starts_with(char::is_whitespace)
                {
                    let tail = tail.trim_start();
                    let value = tail.strip_prefix('=').unwrap_or(tail).trim_start();
                    return Some((index, statement.len() - value.len()));
                }
            }
            _ => {}
        }
    }
    None
}

pub(super) struct InlineDefinition {
    pub(super) parameter_end: usize,
    pub(super) body: Range<usize>,
}

/// The caller supplies the same normalized text used by model semantics.
/// Offsets refer to that text, not to the original physical lines.
pub(super) fn definition(statement: &str) -> Result<Option<InlineDefinition>> {
    let Some((parameter_end, start)) = definition_start(statement) else {
        return Ok(None);
    };
    let quote = statement
        .as_bytes()
        .get(start)
        .copied()
        .filter(|byte| matches!(*byte, b'\'' | b'"'))
        .context("SIMetrix ALB DEF must be quoted")?;
    let body_start = start + 1;
    let body_end = statement.as_bytes()[body_start..]
        .iter()
        .position(|byte| *byte == quote)
        .map(|end| body_start + end)
        .context("unterminated quoted SIMetrix ALB DEF")?;
    Ok(Some(InlineDefinition {
        parameter_end,
        body: body_start..body_end,
    }))
}

/// Remove SPICE comments per physical line, respecting strings spanning
/// continuations. Track the last non-comment, nonempty continuation even
/// if it follows text which DEF will otherwise discard.
fn normalize_header(statement: &str) -> (String, usize) {
    let mut normalized = String::with_capacity(statement.len());
    let mut last_content_start = 0;
    let mut quote = None;
    for line in statement.lines() {
        let line = line.trim();
        let line = line.strip_prefix('+').map_or(line, str::trim_start);
        if !normalized.is_empty() {
            normalized.push(' ');
        }
        if !line.is_empty() && !line.starts_with(';') {
            last_content_start = normalized.len();
        }
        let mut end = line.len();
        for (index, byte) in line.bytes().enumerate() {
            if let Some(delimiter) = quote {
                if byte == delimiter {
                    quote = None;
                }
            } else {
                match byte {
                    b'\'' | b'"' => quote = Some(byte),
                    b';' => {
                        end = index;
                        break;
                    }
                    _ => {}
                }
            }
        }
        normalized.push_str(line[..end].trim_end());
    }
    normalized.truncate(normalized.trim_end().len());
    (normalized, last_content_start)
}

/// Canonicalize only ALB model statements, before unrelated statement
/// tokenizers can interpret text the vendor ignores. Preserve physical
/// line boundaries until after checking the continuation rule.
pub(super) fn canonicalize(statement: String) -> Result<String> {
    if !statement
        .split_ascii_whitespace()
        .next()
        .is_some_and(|token| token.eq_ignore_ascii_case(".model"))
        || model_header(&statement).is_none_or(|(_, kind)| kind != "D_LOGIC_BLOCK")
    {
        return Ok(statement);
    }
    let (mut normalized, last_line_start) = normalize_header(&statement);
    if let Some(inline) = definition(&normalized)? {
        let quote_end = inline.body.end + 1;
        ensure!(
            quote_end > last_line_start,
            "SIMetrix ALB DEF cannot be followed by a nonempty parameter continuation"
        );
        normalized.truncate(quote_end);
    }
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiline_definition_ends_at_closing_quote() -> Result<()> {
        for (raw, expected) in [
            (
                ".model M d_logic_block DEF=\"out[0]=in[0];\" garbage='",
                ".model M d_logic_block DEF=\"out[0]=in[0];\"",
            ),
            (
                ".model M d_logic_block\n+ family=\"HC\"\n+def =\n+ 'out[0] =\n+ in[0];' trailing",
                ".model M d_logic_block family=\"HC\" def = 'out[0] = in[0];'",
            ),
            (
                ".model M d_logic_block(def=\"out[0]=in[0];\" vsupply=3.3)",
                ".model M d_logic_block(def=\"out[0]=in[0];\"",
            ),
        ] {
            assert_eq!(canonicalize(raw.to_owned())?, expected);
        }
        assert!(
            canonicalize(
                ".model M d_logic_block def=\"out[0]=in[0];\"\n+ family=\"HC\"".to_owned()
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn model_comments_and_blank_continuations_preserve_def_boundary() -> Result<()> {
        for (raw, expected) in [
            (
                ".model M d_logic_block family=\"HC\" ; vsupply=9\n+ vsupply=3.3 DEF \"out[0]=in[0];\"",
                ".model M d_logic_block family=\"HC\" vsupply=3.3 DEF \"out[0]=in[0];\"",
            ),
            (
                ".model M d_logic_block file=LOGIC ; DEF=\"out[0]=in[0];\"",
                ".model M d_logic_block file=LOGIC",
            ),
            (
                ".model M d_logic_block def=\"out[0]=in[0];\"\n+",
                ".model M d_logic_block def=\"out[0]=in[0];\"",
            ),
            (
                ".model M d_logic_block def=\"out[0]=in[0];\"\n+ ; comment",
                ".model M d_logic_block def=\"out[0]=in[0];\"",
            ),
        ] {
            assert_eq!(canonicalize(raw.to_owned())?, expected);
        }
        Ok(())
    }

    #[test]
    fn definition_token_does_not_match_quoted_or_commented_text() -> Result<()> {
        for raw in [
            ".model DEF d_logic_block file=LOGIC",
            ".model M d_logic_block file=LOGIC family=\"DEF=ignored\"",
            ".model M d_logic_block file=LOGIC nodef=\"unused\"",
        ] {
            assert!(definition(raw)?.is_none(), "{raw}");
            assert_eq!(canonicalize(raw.to_owned())?, raw);
        }
        Ok(())
    }
}
