//! Numeric raw-file presentation settings, distinct from electrical options.
//!
//! The consumer resolves their values in the ordinary option scope. In
//! particular, an undefined parameter must not disappear during normalization.

use anyhow::{Context, Result, ensure};
use model_preprocess_command::{Resource, WorkBudget};

use crate::{
    SourceDialects,
    lexical::{self, Edit, Statement},
    linked_source::SourceDialect,
};

/// Source raw-file presentation fields with required scalar values.
pub const SOURCE_NAMES: &[&str] = &[
    "plotwinsize",
    "numdgt",
    "plotreltol",
    "plotvntol",
    "plotabstol",
];
const PREFIX: &str = "ltspice_output_";

/// Whether a native option represents a qualified `LTspice` raw-file setting.
///
/// Only the five explicitly projected names are recognized.
#[must_use = "only explicitly projected raw-file options have no electrical effect"]
pub fn is_projected(name: &str) -> bool {
    name.strip_prefix(PREFIX)
        .is_some_and(|name| SOURCE_NAMES.contains(&name))
}

/// Parse one logical LTspice option card, retaining exact source field spans.
/// The qualified presentation controls accept both assignments and bare values.
///
/// # Errors
/// Malformed fields or missing presentation values.
pub fn source_fields(text: &str) -> Result<Vec<crate::model_card::ModelParameter>> {
    let head = lexical::tokens(text.as_bytes())?
        .into_iter()
        .next()
        .context("missing LTspice options directive")?;
    let offset = head.end;
    let mut fields = crate::model_card::parse_parameter_fields(&text[offset..], SOURCE_NAMES)?;
    for field in &mut fields {
        field.span.name = field.span.name.start + offset..field.span.name.end + offset;
        if let Some(span) = &mut field.span.value {
            *span = span.start + offset..span.end + offset;
        }
        if SOURCE_NAMES
            .iter()
            .any(|name| field.name.eq_ignore_ascii_case(name))
        {
            let value = field
                .expression
                .as_deref()
                .context("missing LTspice output option value")?;
            ensure!(
                field.span.explicit_equals
                    || !SOURCE_NAMES
                        .iter()
                        .any(|name| value.eq_ignore_ascii_case(name)),
                "LTspice output option {} requires a numeric value",
                field.name
            );
        }
    }
    Ok(fields)
}

fn project_card(statement: &Statement, edits: &mut Vec<Edit>) -> Result<()> {
    let source = &statement.text;
    let text = std::str::from_utf8(source)?;
    for field in source_fields(text)? {
        let Some(name) = SOURCE_NAMES
            .iter()
            .find(|name| field.name.eq_ignore_ascii_case(name))
        else {
            continue;
        };
        let value = field
            .span
            .value
            .context("missing LTspice output option value span")?;
        let raw = &text[value.clone()];
        let marker = format!(
            "{PREFIX}{name}{}",
            if field.span.explicit_equals { "" } else { "=" }
        );
        lexical::replace_span(statement, field.span.name, marker.as_bytes(), edits);
        if crate::model_card::parse_spice_constant(raw).is_err() && !raw.starts_with(['{', '\'']) {
            // Bare scalar parameters remain expressions until occurrence binding.
            lexical::replace_span(statement, value, format!("{{{raw}}}").as_bytes(), edits);
        }
    }
    for span in lexical::comma_tokens(source)? {
        if &source[span.clone()] == b"," {
            lexical::replace_span(statement, span, b" ", edits);
        }
    }
    Ok(())
}

pub(super) fn rewrite(
    body: &[u8],
    origins: &[usize],
    dialects: SourceDialects<'_>,
    budget: &WorkBudget,
) -> Result<Option<Vec<u8>>> {
    budget.charge(Resource::ScannedBytes, body.len())?;
    let mut edits = Vec::new();
    let mut control = false;
    for statement in lexical::statements(body)? {
        budget.charge(Resource::Statements, 1)?;
        let first = statement
            .text
            .split(u8::is_ascii_whitespace)
            .next()
            .unwrap_or_default();
        if first.eq_ignore_ascii_case(b".control") {
            control = true;
        } else if first.eq_ignore_ascii_case(b".endc") {
            control = false;
        } else if !control
            && dialects.at_mapped(origins, statement.line)? == SourceDialect::Ltspice
            && [b".options".as_slice(), b".option", b".opt"]
                .iter()
                .any(|keyword| first.eq_ignore_ascii_case(keyword))
        {
            project_card(&statement, &mut edits)
                .with_context(|| dialects.describe_mapped(origins, statement.line))?;
        }
    }
    lexical::apply_budgeted_edits(body, edits, budget)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NormalizationPass, linked_source::RootKind, normalize_mapped};

    #[test]
    fn projection_keeps_order_scope_and_physical_lines() -> Result<()> {
        let input = b"title\n.param pw=6\n.OPTION ReLToL=1m,PlotWinSize pw\n+ numdgt={7+8} plotreltol=1m ; untouched\n+ plotvntol=-1 plotabstol=1n plotwinsize=0\n.control\necho plotwinsize\n.endc\n.end\n";
        let result = normalize_mapped(
            input,
            RootKind::Deck,
            SourceDialect::Ltspice,
            &WorkBudget::default(),
        )?;
        let actual = std::str::from_utf8(&result.model.bytes)?;
        assert!(actual.contains(".OPTION ReLToL=1m ltspice_output_plotwinsize= {pw}"));
        assert!(
            actual
                .contains("+ ltspice_output_numdgt={7+8} ltspice_output_plotreltol=1m ; untouched")
        );
        assert!(actual.contains("+ ltspice_output_plotvntol=-1 ltspice_output_plotabstol=1n ltspice_output_plotwinsize=0"));
        assert!(actual.contains("echo plotwinsize"));
        assert!(
            result
                .model
                .passes
                .contains(&NormalizationPass::LtspiceOutputOptions)
        );
        assert_eq!(result.input_lines, (1..=9).collect::<Vec<_>>());
        let native = normalize_mapped(
            input,
            RootKind::Deck,
            SourceDialect::Native,
            &WorkBudget::default(),
        )?;
        assert!(
            !native
                .model
                .passes
                .contains(&NormalizationPass::LtspiceOutputOptions)
        );
        assert!(!std::str::from_utf8(&native.model.bytes)?.contains(PREFIX));
        Ok(())
    }

    #[test]
    fn missing_values_are_errors_and_unknown_controls_are_not_removed() -> Result<()> {
        for input in [
            b".options plotwinsize".as_slice(),
            b".options plotwinsize numdgt",
            b".options plotwinsize=,numdgt=15",
            b".options plotwinsize reltol=1m",
        ] {
            assert!(
                rewrite(
                    input,
                    &[1],
                    SourceDialects::Uniform(SourceDialect::Ltspice),
                    &WorkBudget::default()
                )
                .is_err()
            );
        }
        let input = b".options method=plotwinsize unknown=1 plotwinsize=0\n";
        assert_eq!(
            rewrite(
                input,
                &[1],
                SourceDialects::Uniform(SourceDialect::Ltspice),
                &WorkBudget::default()
            )?
            .context("expected projection")?,
            b".options method=plotwinsize unknown=1 ltspice_output_plotwinsize=0\n"
        );
        Ok(())
    }
}
