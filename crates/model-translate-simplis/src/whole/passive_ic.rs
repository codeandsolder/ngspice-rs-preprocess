//! Capture source initialization at the linked-definition boundary.

use super::{Item, capture_artifact_items};
use anyhow::Result;
use model_normalize_ngspice::passive_ic::{InitialConditions, capture_statement};
use model_preprocess_command::{Resource, WorkBudget};
use model_preprocess_simplis::LinkedPreprocessedNetlist;
use std::fmt::Write as _;

fn apply_local_reactive_overrides(items: &mut [Item]) -> Result<bool> {
    use super::statement_with_init_override;
    use model_preprocess_simplis::parse_init_statement;
    use std::collections::{BTreeMap, BTreeSet};
    let mut overrides = BTreeMap::new();
    for statement in super::electrical_statements(items) {
        if let Some(assignments) = parse_init_statement(&super::normalized_statement(statement))? {
            for (path, value) in assignments {
                if let [name] = path.as_slice()
                    && name
                        .as_bytes()
                        .first()
                        .map(u8::to_ascii_uppercase)
                        .is_some_and(|kind| matches!(kind, b'C' | b'L'))
                {
                    let name = name.to_ascii_uppercase();
                    anyhow::ensure!(
                        overrides.insert(name.clone(), value).is_none(),
                        "duplicate reactive initialization {name}"
                    );
                }
            }
        }
    }
    if overrides.is_empty() {
        return Ok(false);
    }
    let mut applied = BTreeSet::new();
    let mut embedded = false;
    for item in &mut *items {
        let Item::Statement(statement) = item else {
            continue;
        };
        if let Some(start) = crate::syntax::file_boundary(statement) {
            embedded = start;
        } else if !embedded
            && let Some(name) = statement
                .split_ascii_whitespace()
                .next()
                .map(str::to_ascii_uppercase)
            && overrides.contains_key(&name)
        {
            *statement = statement_with_init_override(statement, &overrides)?;
            applied.insert(name);
        }
    }
    if applied.is_empty() {
        return Ok(false);
    }
    for item in items {
        let Item::Statement(statement) = item else {
            continue;
        };
        if let Some(assignments) = parse_init_statement(&super::normalized_statement(statement))? {
            let mut retained = String::new();
            for (path, value) in assignments {
                let name = path.join(".");
                if !applied.contains(&name.to_ascii_uppercase()) {
                    write!(retained, " {name}={value}")?;
                }
            }
            *statement = if retained.is_empty() {
                "* source reactive snapshot recorded in typed policy".to_owned()
            } else {
                format!(".INIT{retained}")
            };
        }
    }
    Ok(true)
}

fn capture_items(
    items: &mut [Item],
    artifact: &str,
    scope: &[String],
    conditions: &mut InitialConditions,
    budget: &WorkBudget,
) -> Result<bool> {
    let _depth = budget.enter()?;
    let mut changed = apply_local_reactive_overrides(items)?;
    let mut embedded = false;
    let mut control = false;
    for item in items {
        match item {
            Item::Statement(statement) => {
                budget.charge(Resource::Statements, 1)?;
                budget.charge(Resource::ScannedBytes, statement.len())?;
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
                    let rewritten = capture_statement(conditions, artifact, scope, statement)?;
                    if rewritten != *statement {
                        *statement = rewritten;
                        changed = true;
                    }
                }
            }
            Item::Subcircuit(block) => {
                changed |= capture_items(
                    &mut block.body,
                    artifact,
                    std::slice::from_ref(&block.name),
                    conditions,
                    budget,
                )?;
            }
        }
    }
    Ok(changed)
}

/// Capture passive initial conditions in an emitted electrical artifact.
///
/// # Errors
/// Malformed source fields or scope declarations.
pub fn capture_artifact_initial_conditions(
    bytes: &mut Vec<u8>,
    filename: &str,
    conditions: &mut InitialConditions,
    budget: &WorkBudget,
) -> Result<()> {
    capture_artifact_items(bytes, budget, |items| {
        capture_items(items, filename, &[], conditions, budget)
    })
}

/// Remove source-only IC annotations from the linked electrical text and
/// retain the complete typed, symbolic initialization policy for the solver.
///
/// Call after source expression normalization and before native translation.
///
/// # Errors
/// Invalid initialization grammar or malformed source scopes.
pub fn capture_passive_initial_conditions(
    linked: &mut LinkedPreprocessedNetlist,
    budget: &WorkBudget,
) -> Result<InitialConditions> {
    let mut conditions = InitialConditions::default();
    // Catalog support declarations are visible before root options.
    for library in &mut linked.libraries {
        capture_artifact_initial_conditions(
            &mut library.bytes,
            &library.filename,
            &mut conditions,
            budget,
        )?;
    }
    capture_artifact_initial_conditions(
        &mut linked.root_bytes,
        "deck.cir",
        &mut conditions,
        budget,
    )?;
    Ok(conditions)
}
