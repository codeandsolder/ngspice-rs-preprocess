//! Capture source initialization at the linked-definition boundary.

use super::{Item, parse_items, render_native_subcircuit};
use anyhow::Result;
use model_normalize_ngspice::passive_ic::{InitialConditions, capture_statement};
use model_preprocess_simplis::LinkedPreprocessedNetlist;
use std::fmt::Write as _;

fn capture_items(
    items: &mut [Item],
    artifact: &str,
    scope: &mut Vec<String>,
    conditions: &mut InitialConditions,
) -> Result<()> {
    let mut embedded = false;
    let mut control = false;
    for item in items {
        match item {
            Item::Statement(statement) => {
                if let Some(start) = crate::syntax::file_boundary(statement) {
                    embedded = start;
                    continue;
                }
                let first = statement.split_whitespace().next().unwrap_or_default();
                if first.eq_ignore_ascii_case(".control") {
                    control = true;
                } else if first.eq_ignore_ascii_case(".endc") {
                    control = false;
                } else if !embedded && !control {
                    *statement = capture_statement(conditions, artifact, scope, statement)?;
                }
            }
            Item::Subcircuit(block) => {
                let mut emitted_scope = vec![block.name.clone()];
                capture_items(&mut block.body, artifact, &mut emitted_scope, conditions)?;
            }
        }
    }
    Ok(())
}

fn capture_artifact(
    bytes: &mut Vec<u8>,
    filename: &str,
    conditions: &mut InitialConditions,
) -> Result<()> {
    let mut items = parse_items(std::str::from_utf8(bytes)?)?;
    capture_items(&mut items, filename, &mut Vec::new(), conditions)?;
    let mut output = String::new();
    for item in items {
        match item {
            Item::Statement(statement) => writeln!(output, "{statement}")?,
            Item::Subcircuit(block) => render_native_subcircuit(&block, &mut output)?,
        }
    }
    *bytes = output.into_bytes();
    Ok(())
}

/// Remove source-only IC annotations from the linked electrical text and
/// retain the complete typed, symbolic initialization policy for the solver.
/// Call after source expression normalization and before native translation.
///
/// # Errors
/// Invalid initialization grammar or malformed source scopes.
pub fn capture_passive_initial_conditions(
    linked: &mut LinkedPreprocessedNetlist,
) -> Result<InitialConditions> {
    let mut conditions = InitialConditions::default();
    // Catalog support declarations are visible before root options.
    for library in &mut linked.libraries {
        capture_artifact(&mut library.bytes, &library.filename, &mut conditions)?;
    }
    capture_artifact(&mut linked.root_bytes, "deck.cir", &mut conditions)?;
    Ok(conditions)
}
