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

const NAMES: &[&str] = &[
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
        .is_some_and(|name| NAMES.contains(&name))
}

fn project_card(statement: &Statement, edits: &mut Vec<Edit>) -> Result<()> {
    let source = &statement.text;
    let spans = lexical::comma_tokens(source)?;
    let tokens: Vec<_> = spans.iter().map(|span| &source[span.clone()]).collect();
    let mut index = 1;
    while let Some(&key) = tokens.get(index) {
        if key == b"," {
            lexical::replace_span(statement, spans[index].clone(), b" ", edits);
            index += 1;
            continue;
        }
        let assigned = tokens.get(index + 1) == Some(&b"=".as_slice());
        let Some(name) = NAMES
            .iter()
            .find(|name| key.eq_ignore_ascii_case(name.as_bytes()))
        else {
            // Do not mistake the value of an unrelated option for a key.
            index += if assigned { 3 } else { 1 };
            continue;
        };
        let value_index = index + if assigned { 2 } else { 1 };
        let value = tokens
            .get(value_index)
            .context("missing LTspice output option value")?;
        ensure!(
            !matches!(*value, b"=" | b",")
                && tokens.get(value_index + 1) != Some(&b"=".as_slice())
                && (assigned
                    || !NAMES
                        .iter()
                        .any(|name| value.eq_ignore_ascii_case(name.as_bytes()))),
            "line {}: LTspice output option {name} requires a numeric value",
            statement.line
        );
        let marker = format!("{PREFIX}{name}{}", if assigned { "" } else { "=" });
        lexical::replace_span(statement, spans[index].clone(), marker.as_bytes(), edits);
        let raw = std::str::from_utf8(value)?;
        if crate::model_card::parse_spice_constant(raw).is_err()
            && !matches!(value.first(), Some(b'{' | b'\''))
        {
            // LTspice also accepts bare scalar parameters in this position.
            // Retain them as expressions for occurrence-specific validation.
            lexical::replace_span(
                statement,
                spans[value_index].clone(),
                format!("{{{raw}}}").as_bytes(),
                edits,
            );
        }
        index = value_index + 1;
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
