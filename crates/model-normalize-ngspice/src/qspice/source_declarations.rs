//! Shared inspection of source-only QSPICE declaration families.

use crate::{
    lexical::{Statement, statements},
    linked_source::{LinkedSource, RootKind, SourceDialect, SourceOrigin},
};
use anyhow::{Context, Result};
use model_preprocess_command::{Resource, WorkBudget};

pub(super) fn inspect<T>(
    linked: &LinkedSource,
    kind: RootKind,
    budget: &WorkBudget,
    matches: impl Fn(&[u8]) -> bool,
    parse: impl Fn(&Statement, &SourceOrigin, &WorkBudget) -> Result<T>,
) -> Result<Vec<T>> {
    let skip_title = kind == RootKind::Deck;
    let body = if skip_title {
        linked
            .bytes
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(&[][..], |end| &linked.bytes[end + 1..])
    } else {
        &linked.bytes
    };
    budget.charge(Resource::ScannedBytes, body.len())?;
    let mut declarations = Vec::new();
    for statement in statements(body)? {
        budget.charge(Resource::Statements, 1)?;
        let line = statement.line + usize::from(skip_title);
        if matches(&statement.text) && linked.dialect_at(line) == Some(SourceDialect::Qspice) {
            let origin = linked
                .origin(line)
                .context("missing QSPICE source location")?;
            declarations.push(parse(&statement, origin, budget)?);
        }
    }
    Ok(declarations)
}
