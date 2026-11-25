//! Instance-only positional syntax, independent of model-value resolution.

use crate::{lexical, model_card::parse_spice_constant};
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

pub(super) fn rewrite(text: &str, local: bool, ground: &str) -> Result<String> {
    match text.as_bytes().first().map(u8::to_ascii_lowercase) {
        Some(b'r' | b'l' | b'c') => temperature_coefficients(text),
        Some(b'q') => area(text, true, local, ground),
        Some(b'd') => area(text, false, local, ground),
        _ => Ok(text.to_owned()),
    }
}
