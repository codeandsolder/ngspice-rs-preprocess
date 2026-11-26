//! Instance-only positional syntax, independent of model-value resolution.

use crate::{
    lexical,
    model_card::{parse_spice_constant, positional_prefix},
};
use anyhow::{Context, Result, ensure};
use std::ops::Range;

fn scalar(text: &str) -> bool {
    parse_spice_constant(text).is_ok() || text.starts_with(['{', '\'', '('])
}

fn fields(text: &str) -> Result<Vec<(&str, Range<usize>)>> {
    Ok(lexical::comma_tokens(text.as_bytes())?
        .into_iter()
        .map(|span| (&text[span.clone()], span))
        .collect())
}

fn temperature_coefficients(text: &str) -> Result<String> {
    let fields = fields(text)?;
    let mut edits = Vec::new();
    let mut index = 4usize;
    while index < fields.len() {
        let (name, span) = &fields[index];
        if !name.eq_ignore_ascii_case("tc")
            || fields
                .get(index.wrapping_sub(1))
                .is_some_and(|(word, _)| *word == "=")
        {
            index += 1;
            continue;
        }
        let start = span.start;
        index += 1;
        if fields.get(index).is_some_and(|(word, _)| *word == "=") {
            index += 1;
        }
        let (first, first_span) = fields
            .get(index)
            .context("missing first PSpice temperature coefficient")?;
        ensure!(
            !matches!(*first, "," | "="),
            "missing first PSpice temperature coefficient"
        );
        let mut end = first_span.end;
        let mut replacement = format!("tc1={first}");
        index += 1;
        let comma = fields.get(index).is_some_and(|(word, _)| *word == ",");
        if comma {
            index += 1;
        }
        let second = fields.get(index).filter(|(word, _)| {
            !matches!(*word, "," | "=")
                && (comma || scalar(word))
                && fields.get(index + 1).is_none_or(|(next, _)| *next != "=")
        });
        if let Some((second, span)) = second {
            use std::fmt::Write as _;
            write!(replacement, " tc2={second}")?;
            end = span.end;
            index += 1;
        } else {
            ensure!(!comma, "missing second PSpice temperature coefficient");
        }
        edits.push((start..end, replacement.into_bytes()));
    }
    Ok(String::from_utf8(
        lexical::apply_edits(text.as_bytes(), edits).unwrap_or_else(|| text.as_bytes().to_vec()),
    )?)
}

fn area(text: &str, bjt: bool, local: bool, ground: &str) -> Result<String> {
    let fields = fields(text)?;
    let mut model = if bjt { 4 } else { 3 };
    let mut edits = Vec::new();
    if bjt && let Some((node, span)) = fields.get(model) {
        if let Some(node) = node
            .strip_prefix('[')
            .and_then(|word| word.strip_suffix(']'))
        {
            ensure!(
                !node.is_empty() && !node.contains(char::is_whitespace),
                "invalid PSpice BJT substrate node"
            );
            let replacement = if local && node.eq_ignore_ascii_case("gnd") {
                ground
            } else {
                node
            };
            edits.push((span.clone(), replacement.as_bytes().to_vec()));
            model += 1;
        } else if node.bytes().all(|byte| byte.is_ascii_digit()) {
            model += 1;
        }
    }
    if let Some((value, span)) = fields.get(model + 1)
        && scalar(value)
        && fields.get(model + 2).is_none_or(|(next, _)| *next != "=")
    {
        edits.push((span.start..span.start, b"area=".to_vec()));
    }
    Ok(String::from_utf8(
        lexical::apply_edits(text.as_bytes(), edits).unwrap_or_else(|| text.as_bytes().to_vec()),
    )?)
}

/// PSpice's one-dimensional, one-coefficient shorthand is P1 with P0=0.
/// Preserve native POLY identity and every coefficient spelling; only make
/// the dialect-specific implicit constant explicit before native parsing.
fn poly_shorthand(text: &str, voltage: bool) -> Result<String> {
    let (fields, _) = positional_prefix(text);
    if !fields
        .get(3)
        .is_some_and(|(word, _)| word.eq_ignore_ascii_case("poly"))
    {
        return Ok(text.to_owned());
    }
    let dimension = fields
        .get(4)
        .context("PSpice POLY lacks dimension")?
        .0
        .parse::<usize>()
        .context("invalid PSpice POLY dimension")?;
    ensure!(dimension > 0, "PSpice POLY dimension must be positive");
    if dimension != 1 {
        return Ok(text.to_owned());
    }
    let (_, end) = fields
        .get(if voltage { 6 } else { 5 })
        .context("PSpice POLY lacks controls")?;
    let suffix = &text[*end..];
    let tail =
        suffix.trim_start_matches(|c: char| c.is_ascii_whitespace() || matches!(c, ')' | ','));
    let start = text.len() - tail.len();
    let tokens = lexical::waveform_tokens(tail.as_bytes())?;
    let first = tokens.first().context("PSpice POLY lacks coefficients")?;
    // Remaining named setters do not count as polynomial coefficients.
    if tokens.len() > 1 && !tokens.get(2).is_some_and(|span| &tail[span.clone()] == "=") {
        return Ok(text.to_owned());
    }
    let spelling = &tail[first.clone()];
    let insert = if let Some(inner) = spelling
        .strip_prefix('(')
        .and_then(|value| value.strip_suffix(')'))
    {
        let operands = lexical::waveform_tokens(inner.as_bytes())?;
        if operands.len() != 1 {
            return Ok(text.to_owned());
        }
        start + first.start + 1
    } else {
        start + first.start
    };
    let mut output = String::new();
    output.try_reserve(
        text.len()
            .checked_add(2)
            .context("PSpice POLY output length overflow")?,
    )?;
    output.push_str(&text[..insert]);
    output.push_str("0 ");
    output.push_str(&text[insert..]);
    Ok(output)
}

pub(super) fn rewrite(text: &str, local: bool, ground: &str) -> Result<String> {
    match text.as_bytes().first().map(u8::to_ascii_lowercase) {
        Some(b'r' | b'l' | b'c') => temperature_coefficients(text),
        Some(b'q') => area(text, true, local, ground),
        Some(b'd') => area(text, false, local, ground),
        Some(b'e' | b'g') => poly_shorthand(text, true),
        Some(b'f' | b'h') => poly_shorthand(text, false),
        _ => Ok(text.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poly_shorthand_preserves_controls_identity_and_other_coefficients() -> Result<()> {
        for (input, expected) in [
            ("E1 out 0 POLY(1) in 0 7", "E1 out 0 POLY(1) in 0 0 7"),
            (
                "G1 0 out POLY (1) (in,0) (7)",
                "G1 0 out POLY (1) (in,0) (0 7)",
            ),
            ("F1 out 0 POLY 1 VS -2", "F1 out 0 POLY 1 VS 0 -2"),
            (
                "H1 out 0 POLY(1) VS {gain + 1}",
                "H1 out 0 POLY(1) VS 0 {gain + 1}",
            ),
            ("E1 out 0 POLY(1) in 0 (7,0)", "E1 out 0 POLY(1) in 0 (7,0)"),
            ("G1 out 0 POLY(1) in 0 7 0", "G1 out 0 POLY(1) in 0 7 0"),
            ("F1 out 0 POLY(2) V1 V2 7", "F1 out 0 POLY(2) V1 V2 7"),
            ("G1 out 0 in 0 7", "G1 out 0 in 0 7"),
            ("F1 out 0 POLY(1) VS 7 m=2", "F1 out 0 POLY(1) VS 0 7 m=2"),
        ] {
            assert_eq!(rewrite(input, false, "0")?, expected);
        }
        assert!(rewrite("E1 out 0 POLY(0) 7", false, "0").is_err());
        assert!(rewrite("F1 out 0 POLY(1) VS", false, "0").is_err());
        Ok(())
    }
}
