//! QSPICE number spelling, projected into native scalar syntax without
//! changing node, branch, model, parameter or function names.
//!
//! Numeric suffix exceptions documented by the simulator author:
//! <https://forum.qorvo.com/t/bug-unit-prefixes/14163>

use super::{Feature, reject};
use crate::{
    expression_start,
    lexical::{Edit, Statement, replace_span, tokens},
    linked_source::SourceOrigin,
    model_card::parse_model_card,
};
use anyhow::{Result, ensure};
use std::ops::Range;

fn decimal_prefix(raw: &str) -> usize {
    let mut end = 0;
    for byte in raw.bytes() {
        if byte.is_ascii_digit() || byte == b'.' {
            end += 1;
        } else {
            break;
        }
    }
    end
}

fn scale_prefix(suffix: &str) -> Option<usize> {
    for prefix in ["meg", "mil"] {
        if suffix
            .get(..prefix.len())
            .is_some_and(|s| s.eq_ignore_ascii_case(prefix))
        {
            return Some(prefix.len());
        }
    }
    match suffix.chars().next()? {
        'µ' | 'μ' => Some(2),
        c if matches!(
            c.to_ascii_lowercase(),
            't' | 'g' | 'k' | 'm' | 'u' | 'n' | 'p' | 'f' | 'a'
        ) =>
        {
            Some(1)
        }
        _ => None,
    }
}

// The caller identifies a complete numeric token, not a prefix of a name.
// Only spellings whose numerical interpretation differs from native SPICE
// need rewriting. Keep decimal precision by moving the separator, not
// evaluating and formatting a floating-point approximation.
fn native_spelling(raw: &str) -> Result<Option<String>> {
    let end = decimal_prefix(raw);
    let (mantissa, suffix) = raw.split_at(end);
    let exponent_end = if matches!(suffix.as_bytes().first(), Some(b'e' | b'E' | b'd' | b'D')) {
        let bytes = suffix.as_bytes();
        let start = 1 + usize::from(matches!(bytes.get(1), Some(b'+' | b'-')));
        let count = bytes[start..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count();
        if count == 0 { 0 } else { start + count }
    } else {
        0
    };
    let (exponent, suffix) = suffix.split_at(exponent_end);
    if suffix.eq_ignore_ascii_case("mho") {
        return Ok(Some(format!("{mantissa}{exponent}")));
    }
    ensure!(
        !suffix
            .get(..3)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("mho"))
            || !suffix[3..].bytes().any(|byte| byte.is_ascii_digit()),
        "unqualified embedded decimal conductance spelling: {raw}"
    );
    let Some(scale_end) = scale_prefix(suffix) else {
        return Ok(None);
    };
    let (scale, tail) = suffix.split_at(scale_end);
    let scale = if matches!(scale, "µ" | "μ") {
        "u"
    } else {
        scale
    };
    let embedded_digits = tail.bytes().take_while(u8::is_ascii_digit).count();
    if embedded_digits != 0 {
        ensure!(
            exponent.is_empty() && !mantissa.contains('.'),
            "embedded decimal following a fractional or exponential mantissa: {raw}"
        );
        let (digits, units) = tail.split_at(embedded_digits);
        ensure!(
            units.chars().all(char::is_alphabetic),
            "invalid embedded decimal suffix: {raw}"
        );
        return Ok(Some(format!("{mantissa}.{digits}{scale}{units}")));
    }
    if suffix.starts_with(['µ', 'μ']) {
        return Ok(Some(format!("{mantissa}{exponent}{scale}{tail}")));
    }
    Ok(None)
}

const fn identifier(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || !byte.is_ascii()
        || matches!(byte, b'_' | b'$' | b'#' | b'.' | b'[' | b']')
}

fn token_end(text: &str, start: usize) -> usize {
    let tail = &text[start..];
    let mut end = decimal_prefix(tail);
    let bytes = tail.as_bytes();
    if matches!(bytes.get(end), Some(b'e' | b'E' | b'd' | b'D')) {
        let exponent = end + 1;
        let first = exponent + usize::from(matches!(bytes.get(exponent), Some(b'+' | b'-')));
        let digits = bytes[first..]
            .iter()
            .take_while(|byte| byte.is_ascii_digit())
            .count();
        if digits != 0 {
            end = first + digits;
        }
    }
    end += tail[end..]
        .chars()
        .take_while(|c| c.is_alphanumeric() || matches!(c, 'µ' | 'μ' | '°'))
        .map(char::len_utf8)
        .sum::<usize>();
    start + end
}

// Probe arguments are opaque identifiers, even if they are entirely numeric.
// Skip balanced parentheses as a whole rather than treating a node label
// like 3k4 as a scalar. This also covers current/power branch references.
fn probe_end(bytes: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (offset, byte) in bytes[open..].iter().enumerate() {
        match byte {
            b'(' => depth += 1,
            b')' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(open + offset + 1);
                }
            }
            _ => {}
        }
    }
    None
}

fn rewrite_span(
    statement: &Statement,
    origin: &SourceOrigin,
    span: Range<usize>,
    edits: &mut Vec<Edit>,
) -> Result<()> {
    let text = std::str::from_utf8(&statement.text[span.clone()])?;
    let bytes = text.as_bytes();
    let mut at = 0;
    while let Some(&byte) = bytes.get(at) {
        if byte == b'"' {
            // Double-quoted strings are paths or literals, not arithmetic.
            at += 1;
            while let Some(&next) = bytes.get(at) {
                at += 1;
                if next == b'"' {
                    break;
                }
                if next == b'\\' && at < bytes.len() {
                    at += 1;
                }
            }
            continue;
        }
        if byte == b'@'
            || byte.is_ascii_alphabetic()
            || matches!(byte, b'_' | b'$' | b'#')
            || !byte.is_ascii()
        {
            let start = at;
            at += 1;
            while bytes.get(at).is_some_and(|b| identifier(*b)) {
                at += 1;
            }
            let name = &bytes[start..at];
            let open = at
                + bytes[at..]
                    .iter()
                    .take_while(|b| b.is_ascii_whitespace())
                    .count();
            if bytes.get(open) == Some(&b'(')
                && [b"v".as_slice(), b"i", b"p", b"q"]
                    .iter()
                    .any(|probe| name.eq_ignore_ascii_case(probe))
            {
                at = probe_end(bytes, open).ok_or_else(|| {
                    reject(
                        origin,
                        Feature::NumericLiteral,
                        "unterminated probe argument",
                    )
                })?;
            }
            continue;
        }
        if byte.is_ascii_digit()
            || (byte == b'.' && bytes.get(at + 1).is_some_and(u8::is_ascii_digit))
        {
            let end = token_end(text, at);
            let raw = &text[at..end];
            if bytes.get(end).is_none_or(|b| !identifier(*b)) {
                // Assignment keys are not numeric values.
                let after = bytes[end..].trim_ascii_start();
                if (!after.starts_with(b"=") || after.starts_with(b"=="))
                    && let Some(native) = native_spelling(raw).map_err(|error| {
                        reject(origin, Feature::NumericLiteral, &error.to_string())
                    })?
                {
                    replace_span(
                        statement,
                        span.start + at..span.start + end,
                        native.as_bytes(),
                        edits,
                    );
                }
            }
            at = end;
        } else {
            at += 1;
        }
    }
    Ok(())
}

pub(super) fn rewrite(
    statement: &Statement,
    origin: &SourceOrigin,
    edits: &mut Vec<Edit>,
) -> Result<()> {
    let text = &statement.text;
    let fields = tokens(text)?;
    let Some(first) = fields.first() else {
        return Ok(());
    };
    let name = &text[first.clone()];
    if name.eq_ignore_ascii_case(b".model") {
        let model = parse_model_card(std::str::from_utf8(text)?, origin.line, &[])?;
        for parameter in model.parameters {
            if let Some(span) = parameter.span.value {
                rewrite_span(statement, origin, span, edits)?;
            }
        }
        return Ok(());
    }
    if name.eq_ignore_ascii_case(b".dc") {
        // Each sweep has an opaque source name followed by start/stop/step.
        for (index, field) in fields.iter().enumerate().skip(1) {
            if (index - 1) % 4 != 0 {
                rewrite_span(statement, origin, field.clone(), edits)?;
            }
        }
        return Ok(());
    }
    if name.eq_ignore_ascii_case(b".ac")
        || name.eq_ignore_ascii_case(b".tran")
        || name.eq_ignore_ascii_case(b".temp")
    {
        let start = if name.eq_ignore_ascii_case(b".ac") {
            2
        } else {
            1
        };
        for field in fields.iter().skip(start) {
            rewrite_span(statement, origin, field.clone(), edits)?;
        }
        return Ok(());
    }
    if name.first().is_some_and(|b| b.eq_ignore_ascii_case(&b'k')) {
        if let Some(field) = fields.last().filter(|_| fields.len() >= 4) {
            rewrite_span(statement, origin, field.clone(), edits)?;
        }
        return Ok(());
    }
    if let Some(start) = expression_start(text, name)? {
        rewrite_span(statement, origin, start..text.len(), edits)?;
    }
    Ok(())
}
