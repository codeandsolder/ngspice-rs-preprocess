//! Explicit multiplicity propagation for native hierarchical circuits.
//! An explicit M formal disables ngspice's automatic (overwriting) native
//! multiplier expansion. Pass the product in every nested call instead.
use super::{command, expression_body, validate_multiplier};
use anyhow::{Context, Result, ensure};
use model_normalize_ngspice::statement_token_spans;
use model_preprocess_simplis::source_dialect::ScopedStatement;

fn multiplier(text: &str) -> Result<Option<std::ops::Range<usize>>> {
    let fields = statement_token_spans(text)?;
    let mut result = None;
    for (index, field) in fields.iter().enumerate().skip(1) {
        if text[field.clone()].eq_ignore_ascii_case("m")
            && fields
                .get(index + 1)
                .is_some_and(|span| &text[span.clone()] == "=")
        {
            let value = fields.get(index + 2).context("missing M value")?.clone();
            ensure!(result.replace(value).is_none(), "duplicate M assignment");
        }
    }
    Ok(result)
}

fn apply_multiplier(text: &mut String, inherited: bool) -> Result<()> {
    if let Some(span) = multiplier(text)? {
        let value = &text[span.clone()];
        validate_multiplier(value)?;
        if inherited {
            let product = format!("{{m*({})}}", expression_body(value));
            text.replace_range(span, &product);
        }
    } else if inherited {
        text.push_str(" m={m}");
    }
    Ok(())
}

fn header(text: &mut String) -> Result<()> {
    ensure!(
        multiplier(text)?.is_none(),
        "explicit M formal requires qualification"
    );
    let fields = statement_token_spans(text)?;
    if !fields
        .iter()
        .any(|span| text[span.clone()].eq_ignore_ascii_case("params:"))
    {
        // Explicit PARAMS: only when no existing formal assignments. Native
        // parser recognizes ordinary name=value header parameters too.
        if !fields.iter().any(|span| &text[span.clone()] == "=") {
            text.push_str(" params:");
        }
    }
    text.push_str(" m=1");
    Ok(())
}

pub(super) fn normalize(
    mut source: Vec<ScopedStatement>,
    deck: bool,
    limit: usize,
) -> Result<Vec<ScopedStatement>> {
    let mut depth = 0usize;
    let mut used = 0usize;
    for statement in source.iter_mut().skip(usize::from(deck)) {
        let name = command(&statement.text);
        if name == ".SUBCKT" {
            header(&mut statement.text)?;
            depth += 1;
        } else if name == ".ENDS" {
            depth = depth.checked_sub(1).context("unmatched .ENDS")?;
        } else if !name.is_empty() && !name.starts_with(['.', '*', '$']) {
            match name.as_bytes()[0] {
                b'R' | b'C' | b'L' | b'D' | b'Q' | b'M' | b'J' | b'Z' | b'I' | b'F' | b'G'
                | b'X' => {
                    apply_multiplier(&mut statement.text, depth != 0)?;
                }
                // Ideal voltage sources do not scale. Inductive coupling
                // coefficients are dimensionless; the inductors scale.
                b'V' | b'E' | b'H' | b'K' => ensure!(
                    multiplier(&statement.text)?.is_none(),
                    "unsupported multiplier on {name}"
                ),
                _ => ensure!(depth == 0, "unqualified hierarchical multiplier on {name}"),
            }
        }
        used = used.saturating_add(statement.text.len().saturating_add(1));
        ensure!(
            used <= limit,
            "hierarchical source expansion byte budget exceeded"
        );
    }
    ensure!(depth == 0, "unterminated .SUBCKT");
    Ok(source)
}
