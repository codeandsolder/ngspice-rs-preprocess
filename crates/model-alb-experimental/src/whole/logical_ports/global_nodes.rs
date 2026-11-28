//! Project implicit global connectivity into explicit, collision-free
//! lexical formals. Physical devices still use their original global net;
//! only ALB-connected paths acquire generated formal aliases.
use super::{
    Graph, Item, Scope, key, normalized_statement, reserved, statement_tokens, valid_analog_node,
};
use anyhow::{Context, Result, ensure};
use std::collections::{BTreeMap, BTreeSet};

impl Scope {
    pub(super) fn mapped_node(&self, node: &str) -> String {
        let original = key(node);
        self.global_formals
            .get(&original)
            .cloned()
            .unwrap_or(original)
    }
}

fn declarations(graph: &Graph) -> Result<BTreeSet<String>> {
    let mut globals = BTreeSet::from(["0".to_owned()]);
    for owner in graph.scopes.values() {
        let mut in_file = false;
        let mut in_control = false;
        for item in &owner.items {
            let Item::Statement(statement) = item else {
                continue;
            };
            let normalized = normalized_statement(statement);
            let first = normalized
                .split_ascii_whitespace()
                .next()
                .unwrap_or_default();
            match key(first).as_str() {
                ".FILE" => in_file = true,
                ".ENDF" => in_file = false,
                ".CONTROL" if !in_file => in_control = true,
                ".ENDC" if !in_file => in_control = false,
                ".GLOBAL" if !in_file && !in_control => {
                    let fields = statement_tokens(&normalized)?;
                    ensure!(fields.len() > 1, "empty original .GLOBAL declaration");
                    for name in &fields[1..] {
                        globals.insert(valid_analog_node(name)?);
                    }
                }
                _ => {}
            }
        }
    }
    Ok(globals)
}

fn required_globals(graph: &Graph, scope: &[String]) -> Result<BTreeSet<String>> {
    let owner = graph
        .scopes
        .get(scope)
        .context("missing global alias scope")?;
    let mut result = owner
        .alb_roles
        .keys()
        .filter(|node| graph.globals.contains(*node))
        .cloned()
        .collect::<BTreeSet<_>>();
    for call in graph.occurrences(scope)?.calls() {
        let child = graph
            .scopes
            .get(&call.target)
            .context("missing global alias child")?;
        result.extend(child.global_formals.keys().cloned());
    }
    Ok(result)
}

fn allocate_formals(
    owner: &mut Scope,
    globals: &BTreeSet<String>,
    required: BTreeSet<String>,
) -> Result<()> {
    let mut occupied = reserved(owner);
    occupied.extend(globals.iter().cloned());
    let mut next = 0usize;
    for global in required {
        let alias = loop {
            let candidate = format!("SR_GLOBAL_{next}");
            next = next
                .checked_add(1)
                .context("exhausted original global aliases")?;
            if occupied.insert(candidate.clone()) {
                break candidate;
            }
        };
        owner.ports.push(alias.clone());
        owner.global_formals.insert(global, alias);
    }
    Ok(())
}

/// Discover original ALB uses before adding explicit global aliases.
/// The leaf-first call order propagates implicit requirements into callers
/// even through a definition that contains no ALB itself.
pub(super) fn prepare(graph: &mut Graph, scopes: &[Vec<String>]) -> Result<()> {
    graph.globals = declarations(graph)?;
    for (path, owner) in &graph.scopes {
        ensure!(
            owner
                .ports
                .iter()
                .all(|formal| !graph.globals.contains(formal)),
            "global node used as argument in subcircuit definition {path:?}"
        );
    }
    graph.discover_alb_roles()?;
    for scope in scopes.iter().filter(|scope| !scope.is_empty()) {
        let required = required_globals(graph, scope)?;
        let owner = graph
            .scopes
            .get_mut(scope)
            .context("missing mutable global alias scope")?;
        allocate_formals(owner, &graph.globals, required)?;
    }
    graph.rebind_global_occurrences()?;
    for owner in graph.scopes.values_mut() {
        owner.alb_roles.clear();
    }
    Ok(())
}

/// Ground is inherently analog. Other declared globals become analog
/// only when a genuine physical contact witnesses that same net in any
/// lexical scope. The original global node and its aliases denote one
/// physical identity, not separate receiver domains.
pub(super) fn project_physical_contacts(
    graph: &Graph,
    contacts: &mut BTreeMap<Vec<String>, BTreeSet<String>>,
) -> Result<()> {
    let mut physical = BTreeSet::from(["0".to_owned()]);
    for (scope, owner) in &graph.scopes {
        let known = contacts
            .get(scope)
            .context("missing global witness contacts")?;
        physical.extend(known.intersection(&graph.globals).cloned());
        physical.extend(
            owner
                .global_formals
                .iter()
                .filter(|(_, alias)| known.contains(*alias))
                .map(|(global, _)| global.clone()),
        );
    }
    for (scope, owner) in &graph.scopes {
        let known = contacts
            .get_mut(scope)
            .context("missing mutable global witness contacts")?;
        known.extend(physical.iter().map(|global| owner.mapped_node(global)));
    }
    Ok(())
}
