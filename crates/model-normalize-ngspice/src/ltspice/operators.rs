//! Locate scalar expressions in source cards before lowering dialect operators.
//!
//! Reuse the parameter-expression parser; never rewrite a positional node,
//! model or device identifier as arithmetic.

use crate::{expression_start, lexical, param_expression::lower_ltspice};
use anyhow::{Result, ensure};
use std::ops::Range;

fn needs_lowering(text: &[u8]) -> bool {
    text.windows(2).any(|pair| matches!(pair, b"**" | b"=="))
        || text.iter().any(|byte| {
            matches!(
                byte,
                b'^' | b'&' | b'|' | b'!' | b'~' | b'%' | b'?' | b'<' | b'>'
            )
        })
}

const fn identifier(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$' | b'#' | b'.')
}

/// Split a scalar/assignment list at top-level assignment keys, not at
/// comparison operators or '=' within a balanced expression.
fn assignment_ranges(text: &[u8], start: usize, end: usize) -> Result<Vec<Range<usize>>> {
    let mut balanced = lexical::Balanced::default();
    let mut boundaries = Vec::new();
    for index in start..end {
        let byte = text[index];
        if byte == b'='
            && balanced.top_level()
            && !matches!(
                text.get(index.wrapping_sub(1)),
                Some(b'=' | b'!' | b'<' | b'>')
            )
            && text.get(index + 1) != Some(&b'=')
        {
            let prefix = text[start..index].trim_ascii_end();
            let key_end = start + prefix.len();
            let key_start = prefix
                .iter()
                .rposition(|byte| !identifier(*byte))
                .map_or(start, |offset| start + offset + 1);
            ensure!(key_start < key_end, "missing LTspice assignment key");
            boundaries.push((key_start, index + 1));
        }
        balanced.advance(byte)?;
    }
    balanced.finish()?;
    let mut ranges = Vec::new();
    let mut value_start = start;
    for (key, value) in boundaries {
        if key > value_start {
            ranges.push(value_start..key);
        }
        value_start = value;
    }
    if value_start < end {
        ranges.push(value_start..end);
    }
    Ok(ranges)
}

fn trimmed_span(text: &[u8], span: Range<usize>) -> Range<usize> {
    let source = &text[span.clone()];
    let start = span.start + source.len() - source.trim_ascii_start().len();
    let end = start + source.trim_ascii().len();
    start..end
}

/// A source field can contain a native waveform or flags alongside a
/// braced parameter expression. Preserve that outer grammar and rewrite
/// only complete balanced parameter groups where possible.
pub(super) fn braced_ranges(text: &[u8], span: Range<usize>) -> Result<Vec<Range<usize>>> {
    let mut groups = Vec::new();
    let mut balanced = lexical::Balanced::default();
    let mut begin = None;
    for index in span {
        let byte = text[index];
        if balanced.top_level() && matches!(byte, b'{' | b'\'') {
            begin = Some(index);
        }
        balanced.advance(byte)?;
        if balanced.top_level()
            && let Some(start) = begin.take()
        {
            groups.push(start..index + 1);
        }
    }
    balanced.finish()?;
    Ok(groups)
}

fn rewrite_expression(
    statement: &lexical::Statement,
    span: Range<usize>,
    edits: &mut Vec<lexical::Edit>,
    mut runtime: bool,
) -> Result<()> {
    let text = &statement.text;
    let mut span = trimmed_span(text, span);
    if span.is_empty() {
        return Ok(());
    }
    // Preserve outer parameter quoting rather than converting its evaluation
    // phase to ordinary runtime parentheses.
    if matches!(text[span.start], b'{' | b'\'')
        && braced_ranges(text, span.clone())?.as_slice() == [span.clone()]
    {
        runtime = false;
        span.start += 1;
        span.end -= 1;
    }
    if !needs_lowering(&text[span.clone()]) {
        return Ok(());
    }
    let raw = std::str::from_utf8(&text[span.clone()])?;
    match lower_ltspice(raw, runtime) {
        Ok(lowered) => lexical::replace_span(statement, span, lowered.as_bytes(), edits),
        Err(error) => {
            let groups = braced_ranges(text, span.clone())?;
            let mut outside = span.start;
            for group in &groups {
                ensure!(
                    !needs_lowering(&text[outside..group.start]),
                    "line {}: LTspice scalar expression {raw:?}: {error}",
                    statement.line
                );
                outside = group.end;
            }
            ensure!(
                !groups.is_empty() && !needs_lowering(&text[outside..span.end]),
                "line {}: LTspice scalar expression {raw:?}: {error}",
                statement.line
            );
            for group in groups {
                rewrite_expression(statement, group, edits, false)?;
            }
        }
    }
    Ok(())
}

fn rewrite_card(
    statement: &lexical::Statement,
    first: &[u8],
    edits: &mut Vec<lexical::Edit>,
) -> Result<()> {
    let text = &statement.text;
    let Some(start) = expression_start(text, first)? else {
        return Ok(());
    };
    if !needs_lowering(&text[start..]) {
        return Ok(());
    }
    let lower = first.to_ascii_lowercase();
    match lower.as_slice() {
        b".func" | b".if" | b".elseif" => {
            rewrite_expression(statement, start..text.len(), edits, false)
        }
        b".model" => {
            // A parenthesized model parameter list is an assignment list,
            // not a function call.
            let model = &text[start..];
            let open = model.iter().position(|byte| *byte == b'(');
            let (start, end) = if let Some(open) = open
                && model.last() == Some(&b')')
                && !model[..open].contains(&b'=')
            {
                (start + open + 1, text.len() - 1)
            } else {
                (start, text.len())
            };
            for span in assignment_ranges(text, start, end)? {
                rewrite_expression(statement, span, edits, false)?;
            }
            Ok(())
        }
        _ => {
            for span in assignment_ranges(text, start, text.len())? {
                rewrite_expression(
                    statement,
                    span,
                    edits,
                    !lower.starts_with(b".") && !lower.starts_with(b"x"),
                )?;
            }
            Ok(())
        }
    }
}

pub(super) fn rewrite(body: &[u8]) -> Result<Option<Vec<u8>>> {
    let mut edits = Vec::new();
    let mut control = false;
    for statement in lexical::statements(body)? {
        let first = statement
            .text
            .split(u8::is_ascii_whitespace)
            .next()
            .unwrap_or_default();
        if first.eq_ignore_ascii_case(b".control") {
            control = true;
        } else if first.eq_ignore_ascii_case(b".endc") {
            control = false;
            continue;
        }
        if !control {
            rewrite_card(&statement, first, &mut edits)?;
        }
    }
    Ok(lexical::apply_edits(body, edits))
}

/// Classify a table call by the containing evaluation phase. A behavioral
/// brace group containing circuit probes or runtime symbols remains dynamic.
/// Function definitions are macro bodies: the call site determines phase.
pub(super) fn static_table(text: &[u8], start: usize, call: usize) -> Result<bool> {
    let first = text
        .split(u8::is_ascii_whitespace)
        .next()
        .unwrap_or_default();
    if [
        b".param".as_slice(),
        b".subckt",
        b".model",
        b".if",
        b".elseif",
    ]
    .iter()
    .any(|keyword| first.eq_ignore_ascii_case(keyword))
        || first
            .first()
            .is_some_and(|byte| byte.eq_ignore_ascii_case(&b'x'))
    {
        return Ok(true);
    }
    if first.eq_ignore_ascii_case(b".func") {
        return Ok(false);
    }
    for group in braced_ranges(text, start..text.len())? {
        if group.contains(&call) {
            let expression = &text[group];
            let runtime = [b"v".as_slice(), b"i"]
                .iter()
                .any(|name| !lexical::function_names(expression, name, 0).is_empty())
                || expression.split(|byte| !identifier(*byte)).any(|word| {
                    [b"time".as_slice(), b"hertz", b"frequency", b"temper"]
                        .iter()
                        .any(|name| word.eq_ignore_ascii_case(name))
                });
            return Ok(!runtime);
        }
    }
    Ok(false)
}
