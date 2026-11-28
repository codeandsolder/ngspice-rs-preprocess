//! Lower active source aliases after compile-time execution. Indexed aliases
//! remain catalog declarations, whereas an explicit import contributes only
//! the alias declarations that were actually executed in this scope.

use super::{Alias, SubcircuitCatalog, is_directive, parse_library_root};
use anyhow::{Context, Result, ensure};
use model_preprocess_command::{Resource, WorkBudget};
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn indexed_aliases(
    aliases: &[Alias],
    budget: &WorkBudget,
) -> Result<BTreeMap<String, Alias>> {
    let mut result: BTreeMap<String, Alias> = BTreeMap::new();
    for alias in aliases {
        budget.charge(Resource::GeneratedNodes, 1)?;
        budget.charge(
            Resource::EmittedBytes,
            alias
                .name
                .len()
                .saturating_add(alias.target.len())
                .saturating_add(alias.kind.len()),
        )?;
        let key = alias.name.to_ascii_uppercase();
        if let Some(previous) = result.get(&key) {
            ensure!(
                previous.target.eq_ignore_ascii_case(&alias.target)
                    && previous.kind.eq_ignore_ascii_case(&alias.kind),
                "conflicting active SIMPLIS alias {}",
                alias.name
            );
        } else {
            result.insert(key, alias.clone());
        }
    }
    Ok(result)
}

pub(super) fn resolve_alias_target<'a>(
    aliases: &'a BTreeMap<String, Alias>,
    name: &'a str,
    kind: Option<&str>,
    budget: &WorkBudget,
) -> Result<&'a str> {
    let mut current = name;
    let mut seen = BTreeSet::new();
    while let Some(alias) = aliases.get(&current.to_ascii_uppercase()) {
        budget.charge(Resource::EvaluationSteps, 1)?;
        if let Some(kind) = kind {
            ensure!(
                alias.kind.eq_ignore_ascii_case(kind),
                "SIMPLIS alias {} has incompatible type {}",
                alias.name,
                alias.kind
            );
        }
        ensure!(
            seen.insert(current.to_ascii_uppercase()),
            "recursive SIMPLIS alias {name}"
        );
        current = &alias.target;
    }
    Ok(current)
}

fn model_name(statement: &str) -> Option<&str> {
    let mut fields = statement.split_whitespace();
    if fields.next()?.eq_ignore_ascii_case(".MODEL") {
        fields.next()
    } else {
        None
    }
}

pub(super) fn model_alias_card(
    alias: &Alias,
    target: &str,
    source_family: Option<&str>,
) -> Result<String> {
    let mut fields = target.split_whitespace().collect::<Vec<_>>();
    ensure!(fields.len() >= 3, "malformed SIMPLIS alias target model");
    let model_type = source_family.unwrap_or_else(|| {
        fields[2]
            .split_once('(')
            .map_or(fields[2], |(head, _)| head)
    });
    ensure!(
        model_type.eq_ignore_ascii_case(&alias.kind),
        "SIMPLIS alias {} expects {} but target {} is model type {model_type}",
        alias.name,
        alias.kind,
        alias.target
    );
    fields[1] = &alias.name;
    Ok(fields.join(" "))
}

pub(super) fn materialize(
    statements: &mut Vec<String>,
    external: &SubcircuitCatalog,
) -> Result<BTreeMap<String, Alias>> {
    let budget = &external.budget;
    if !statements
        .iter()
        .any(|statement| is_directive(statement, "ALIAS"))
    {
        return Ok(BTreeMap::new());
    }
    let (_, declarations) = parse_library_root(statements)?;
    let aliases = indexed_aliases(&declarations, budget)?;
    if aliases.is_empty() {
        return Ok(aliases);
    }
    let mut generated = Vec::new();
    let mut models = BTreeMap::<String, &str>::new();
    for statement in statements.iter() {
        budget.charge(Resource::ScannedBytes, statement.len())?;
        if let Some(name) = model_name(statement) {
            let key = name.to_ascii_uppercase();
            if let Some(previous) = models.insert(key, statement) {
                ensure!(
                    previous == statement,
                    "conflicting active SIMPLIS model {name}"
                );
            }
        }
    }
    for alias in aliases.values() {
        if alias.kind.eq_ignore_ascii_case("subckt") {
            continue;
        }
        let target = resolve_alias_target(&aliases, &alias.name, Some(&alias.kind), budget)?;
        let card = if let Some(card) = models.get(&target.to_ascii_uppercase()) {
            model_alias_card(alias, card, None)?
        } else {
            let source = external
                .source_for_model(target)?
                .with_context(|| format!("SIMPLIS model alias target {target} was not found"))?;
            let (card, family) = external.resolve_model_statement(target, &source)?;
            model_alias_card(alias, &card, Some(&family))?
        };
        budget.charge(Resource::EmittedBytes, card.len().saturating_add(1))?;
        generated.push(card);
    }
    // Emit aliases after their target declarations have been selected and
    // evaluated; keep the enclosing .END or .ENDS last.
    statements.retain(|statement| !is_directive(statement, "ALIAS"));
    let end = statements
        .iter()
        .rposition(|statement| is_directive(statement, "END") || is_directive(statement, "ENDS"))
        .unwrap_or(statements.len());
    statements.splice(end..end, generated);
    Ok(aliases
        .into_iter()
        .filter(|(_, alias)| alias.kind.eq_ignore_ascii_case("subckt"))
        .collect())
}
