//! Bottom-up instance signatures and scope-preserving lexical cloning.

use super::super::super::Subcircuit;
use super::super::{Call, normalized_statement, statement_tokens};
use super::{
    DefinitionFamilyBoundary, FamilySpecialization, Graph, Item, LogicalCut,
    OccurrenceFamilyBoundary, Result, Signature, VariantKey, key, signature,
};
use crate::{LogicalAlbBoundary, alb::ScopedAnalogDigitalConnection};
use anyhow::{Context, ensure};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

type Annotations = BTreeMap<Vec<String>, Vec<OccurrenceFamilyBoundary>>;
type Variants = BTreeMap<Vec<String>, BTreeMap<VariantKey, String>>;

#[derive(Clone)]
struct Occurrence {
    definition: Vec<String>,
    path: Vec<String>,
    local: Option<Signature>,
    children: BTreeMap<String, Self>,
    variant: Option<VariantKey>,
}

fn collect_annotations(entries: &[OccurrenceFamilyBoundary]) -> Result<Annotations> {
    let mut result = Annotations::new();
    for entry in entries {
        ensure!(
            !entry.occurrence.is_empty(),
            "family occurrence path must include a root X instance"
        );
        let path = entry
            .occurrence
            .iter()
            .map(|name| {
                ensure!(
                    name.starts_with(['X', 'x']) && name.len() > 1,
                    "invalid X occurrence path element {name}"
                );
                Ok(key(name))
            })
            .collect::<Result<Vec<_>>>()?;
        result.entry(path).or_default().push(entry.clone());
    }
    Ok(result)
}

fn collect_calls(graph: &Graph, items: &[Item], scope: &[String]) -> Result<Vec<Call>> {
    let mut names = BTreeSet::new();
    let mut calls = Vec::new();
    for item in items {
        let Item::Statement(statement) = item else {
            continue;
        };
        let fields = statement_tokens(&normalized_statement(statement))?;
        let Some(first) = fields.first() else {
            continue;
        };
        if !first.starts_with(['X', 'x']) || first.len() <= 1 {
            continue;
        }
        ensure!(
            names.insert(key(first)),
            "duplicate X instance name {first} in lexical scope {scope:?}"
        );
        if let Some(call) = graph.call(statement, scope)? {
            calls.push(call);
        }
    }
    Ok(calls)
}

fn build_occurrence(
    graph: &Graph,
    call: &Call,
    path: Vec<String>,
    annotations: &Annotations,
    seen: &mut BTreeSet<Vec<String>>,
    stack: &mut Vec<Vec<String>>,
) -> Result<Occurrence> {
    ensure!(stack.len() < 256, "X hierarchy exceeds 256 levels");
    ensure!(
        !stack.contains(&call.target),
        "recursive X subcircuit definition {:?} on occurrence {path:?}",
        call.target
    );
    stack.push(call.target.clone());
    let scope = graph
        .scopes
        .get(&call.target)
        .context("missing original lexical X definition")?;
    let mut children = BTreeMap::new();
    for child in collect_calls(graph, &scope.items, &call.target)? {
        let mut child_path = path.clone();
        let name = key(&child.name);
        child_path.push(name.clone());
        children.insert(
            name,
            build_occurrence(graph, &child, child_path, annotations, seen, stack)?,
        );
    }
    stack.pop();
    let local = if let Some(entries) = annotations.get(&path) {
        seen.insert(path.clone());
        for entry in entries {
            ensure!(
                entry.scope.iter().map(|s| key(s)).collect::<Vec<_>>() == call.target,
                "occurrence {path:?} resolves to {:?}, not annotated scope {:?}",
                call.target,
                entry.scope
            );
        }
        Some(signature(entries)?)
    } else {
        None
    };
    let changed = local.is_some() || children.values().any(|node| node.variant.is_some());
    let variant = changed.then(|| VariantKey {
        local: local.clone(),
        children: children
            .iter()
            .filter_map(|(name, node)| {
                node.variant.as_ref().map(|variant| {
                    (
                        name.clone(),
                        node.definition.clone(),
                        Box::new(variant.clone()),
                    )
                })
            })
            .collect(),
    });
    Ok(Occurrence {
        definition: call.target.clone(),
        path,
        local,
        children,
        variant,
    })
}

fn collect_occurrences(
    graph: &Graph,
    items: &[Item],
    annotations: &Annotations,
) -> Result<BTreeMap<String, Occurrence>> {
    let mut roots = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for call in collect_calls(graph, items, &[])? {
        let name = key(&call.name);
        roots.insert(
            name.clone(),
            build_occurrence(
                graph,
                &call,
                vec![name],
                annotations,
                &mut seen,
                &mut Vec::new(),
            )?,
        );
    }
    for path in annotations.keys() {
        ensure!(
            seen.contains(path),
            "unresolved/unused X occurrence path {path:?}"
        );
    }
    Ok(roots)
}

/// A wholly unannotated occurrence retains the common logical cut layout
/// only when every explicitly annotated occurrence agrees on it. It gets
/// no electrical bridges. Heterogeneous cuts have no implicit default:
/// an unchanged occurrence must be valid with its actual digital ports.
type CommonCuts = BTreeMap<Vec<String>, Option<Vec<LogicalCut>>>;

fn collect_common_cuts(node: &Occurrence, common: &mut CommonCuts) {
    if let Some(local) = &node.local {
        let cuts = local
            .iter()
            .map(|entry| entry.cut.clone())
            .collect::<Vec<_>>();
        common
            .entry(node.definition.clone())
            .and_modify(|previous| {
                if previous.as_ref() != Some(&cuts) {
                    *previous = None;
                }
            })
            .or_insert(Some(cuts));
    }
    for child in node.children.values() {
        collect_common_cuts(child, common);
    }
}

fn gather_variants(node: &Occurrence, groups: &mut BTreeMap<Vec<String>, BTreeSet<VariantKey>>) {
    if let Some(variant) = &node.variant {
        groups
            .entry(node.definition.clone())
            .or_default()
            .insert(variant.clone());
    }
    for child in node.children.values() {
        gather_variants(child, groups);
    }
}

fn allocate_variants(graph: &Graph, roots: &BTreeMap<String, Occurrence>) -> Result<Variants> {
    let mut signatures = BTreeMap::new();
    for node in roots.values() {
        gather_variants(node, &mut signatures);
    }
    let mut variants = Variants::new();
    for (scope, values) in signatures {
        let (name, parent) = scope.split_last().context("empty definition scope")?;
        let siblings = graph
            .scopes
            .keys()
            .filter_map(|path| {
                if path.len() == scope.len() && path[..path.len() - 1] == *parent {
                    path.last()
                } else {
                    None
                }
            })
            .collect::<BTreeSet<_>>();
        let mut mapping = BTreeMap::new();
        for (index, key) in values.into_iter().enumerate() {
            let generated = format!("{name}_SRV{index}");
            ensure!(
                !siblings.contains(&generated),
                "generated specialization model {generated} collides with lexical declaration"
            );
            mapping.insert(key, generated);
        }
        variants.insert(scope, mapping);
    }
    Ok(variants)
}

fn matching_occurrences<'a>(
    node: &'a Occurrence,
    target: &[String],
    out: &mut Vec<&'a Occurrence>,
) {
    if node.definition == target {
        out.push(node);
    }
    for child in node.children.values() {
        matching_occurrences(child, target, out);
    }
}

fn variant_header(block: &Subcircuit, replacement: &str) -> Result<String> {
    let normalized = normalized_statement(&block.header);
    let mut fields = normalized.splitn(3, char::is_whitespace);
    ensure!(
        fields
            .next()
            .is_some_and(|f| f.eq_ignore_ascii_case(".subckt")),
        "not a subcircuit header"
    );
    fields.next().context("missing model name")?;
    let rest = fields.next().unwrap_or_default();
    Ok(format!(".subckt {replacement} {rest}"))
}

fn rewrite_call(call: &Call, name: &str) -> Result<String> {
    let mut output = call.name.clone();
    for actual in &call.actual {
        write!(output, " {actual}")?;
    }
    write!(output, " {name}")?;
    for suffix in &call.suffix {
        write!(output, " {suffix}")?;
    }
    Ok(output)
}

struct Renderer<'a> {
    graph: &'a Graph,
    variants: &'a Variants,
    annotations: &'a Annotations,
    lexical: &'a [DefinitionFamilyBoundary],
    common: &'a CommonCuts,
    emitted: BTreeSet<Vec<String>>,
    output: String,
    connections: Vec<ScopedAnalogDigitalConnection>,
    logical_boundaries: Vec<LogicalAlbBoundary>,
}

impl Renderer<'_> {
    fn copy_connections(
        &mut self,
        original: &[String],
        physical: &[String],
        representative: Option<&Occurrence>,
    ) {
        for entry in self.lexical {
            if entry.scope.iter().map(|s| key(s)).collect::<Vec<_>>() == original {
                self.connections.push(ScopedAnalogDigitalConnection {
                    scope: physical.to_vec(),
                    boundary: entry.boundary.clone(),
                });
                self.logical_boundaries.push(
                    LogicalCut::new(&entry.boundary, entry.producer.as_ref(), &entry.receivers)
                        .at(physical),
                );
            }
        }
        if let Some(node) = representative
            && let Some(entries) = self.annotations.get(&node.path)
        {
            for entry in entries {
                self.connections.push(ScopedAnalogDigitalConnection {
                    scope: physical.to_vec(),
                    boundary: entry.boundary.clone(),
                });
                self.logical_boundaries.push(
                    LogicalCut::new(&entry.boundary, entry.producer.as_ref(), &entry.receivers)
                        .at(physical),
                );
            }
        } else if let Some(Some(cuts)) = self.common.get(original) {
            self.logical_boundaries
                .extend(cuts.iter().map(|cut| cut.at(physical)));
        }
    }

    fn statement(
        &mut self,
        original: &[String],
        statement: &str,
        representative: Option<&Occurrence>,
    ) -> Result<()> {
        if let Some(node) = representative
            && let Some(call) = self.graph.call(statement, original)?
            && let Some(child) = node.children.get(&key(&call.name))
            && let Some(variant) = &child.variant
        {
            let target = self
                .variants
                .get(&child.definition)
                .and_then(|variants| variants.get(variant))
                .context("missing nested electrical specialization")?;
            self.output.push_str(&rewrite_call(&call, target)?);
        } else {
            self.output.push_str(statement);
        }
        self.output.push('\n');
        Ok(())
    }

    fn definition(
        &mut self,
        block: &Subcircuit,
        original: &[String],
        physical: &[String],
        new_name: &str,
        representative: Option<&Occurrence>,
    ) -> Result<()> {
        ensure!(
            self.emitted.insert(physical.to_vec()),
            "duplicate specialized lexical definition {physical:?}"
        );
        if new_name.eq_ignore_ascii_case(&block.name) {
            self.output.push_str(&block.header);
        } else {
            self.output.push_str(&variant_header(block, new_name)?);
        }
        self.output.push('\n');
        for item in &block.body {
            match item {
                Item::Statement(statement) => {
                    self.statement(original, statement.as_str(), representative)?;
                }
                Item::Subcircuit(child) => {
                    let mut child_scope = original.to_vec();
                    child_scope.push(key(&child.name));
                    self.subdefinition(child, &child_scope, physical, representative)?;
                }
            }
        }
        writeln!(self.output, ".ends {new_name}")?;
        self.copy_connections(original, physical, representative);
        Ok(())
    }

    fn subdefinition(
        &mut self,
        block: &Subcircuit,
        original: &[String],
        parent_physical: &[String],
        parent: Option<&Occurrence>,
    ) -> Result<()> {
        let mut matches = Vec::new();
        if let Some(context) = parent {
            matching_occurrences(context, original, &mut matches);
        }
        let mut requested = BTreeMap::<VariantKey, &Occurrence>::new();
        for occurrence in &matches {
            if let Some(variant) = &occurrence.variant {
                requested.entry(variant.clone()).or_insert(occurrence);
            }
        }
        let original_occurrence = matches.iter().find(|node| node.variant.is_none()).copied();
        if requested.is_empty() || original_occurrence.is_some() {
            let mut path = parent_physical.to_vec();
            path.push(key(&block.name));
            self.definition(
                block,
                original,
                &path,
                &block.name,
                original_occurrence.or_else(|| matches.first().copied()),
            )?;
        }
        // A specialized child and an unchanged child can coexist under the
        // same lexical parent. Keep the original definition for unchanged X
        // calls rather than leaving them pointing to a removed subcircuit.
        for (variant, occurrence) in requested {
            let generated = self
                .variants
                .get(original)
                .and_then(|variants| variants.get(&variant))
                .context("missing nested variant name")?
                .clone();
            let mut path = parent_physical.to_vec();
            path.push(generated.clone());
            self.definition(block, original, &path, &generated, Some(occurrence))?;
        }
        Ok(())
    }
}

fn render_specialized(
    graph: &Graph,
    items: &[Item],
    roots: &BTreeMap<String, Occurrence>,
    variants: &Variants,
    annotations: &Annotations,
    lexical: &[DefinitionFamilyBoundary],
    common: &CommonCuts,
) -> Result<FamilySpecialization> {
    let mut renderer = Renderer {
        graph,
        variants,
        annotations,
        lexical,
        common,
        emitted: BTreeSet::new(),
        output: String::new(),
        connections: Vec::new(),
        logical_boundaries: Vec::new(),
    };
    for item in items {
        match item {
            Item::Statement(statement) => {
                if let Some(call) = graph.call(statement, &[])?
                    && let Some(node) = roots.get(&key(&call.name))
                    && let Some(variant) = &node.variant
                {
                    let target = variants
                        .get(&node.definition)
                        .and_then(|variants| variants.get(variant))
                        .context("missing root specialized subcircuit")?;
                    renderer.output.push_str(&rewrite_call(&call, target)?);
                } else {
                    renderer.output.push_str(statement);
                }
                renderer.output.push('\n');
            }
            Item::Subcircuit(def) => {
                let scope = vec![key(&def.name)];
                let mut matches = Vec::new();
                for node in roots.values() {
                    matching_occurrences(node, &scope, &mut matches);
                }
                let mut requested = BTreeMap::<VariantKey, &Occurrence>::new();
                for occurrence in &matches {
                    if let Some(variant) = &occurrence.variant {
                        requested.entry(variant.clone()).or_insert(occurrence);
                    }
                }
                let original_occurrence =
                    matches.iter().find(|node| node.variant.is_none()).copied();
                if requested.is_empty() || original_occurrence.is_some() {
                    renderer.definition(
                        def,
                        &scope,
                        &scope,
                        &def.name,
                        original_occurrence.or_else(|| matches.first().copied()),
                    )?;
                }
                for (variant, occurrence) in requested {
                    let name = variants
                        .get(&scope)
                        .and_then(|variants| variants.get(&variant))
                        .context("missing root electrical variant")?;
                    renderer.definition(
                        def,
                        &scope,
                        std::slice::from_ref(name),
                        name,
                        Some(occurrence),
                    )?;
                }
            }
        }
    }
    renderer.copy_connections(&[], &[], None);
    Ok(FamilySpecialization {
        deck: renderer.output,
        connections: renderer.connections,
        logical_boundaries: renderer.logical_boundaries,
    })
}

pub(super) fn run(
    graph: &Graph,
    items: &[Item],
    occurrence_connections: &[OccurrenceFamilyBoundary],
    lexical_connections: &[DefinitionFamilyBoundary],
) -> Result<FamilySpecialization> {
    let annotations = collect_annotations(occurrence_connections)?;
    let roots = collect_occurrences(graph, items, &annotations)?;
    let mut targeted = BTreeSet::new();
    for entries in annotations.values() {
        let first = entries.first().context("empty occurrence annotation")?;
        targeted.insert(first.scope.iter().map(|s| key(s)).collect::<Vec<_>>());
    }
    let mut common = CommonCuts::new();
    for node in roots.values() {
        collect_common_cuts(node, &mut common);
    }
    for entry in lexical_connections {
        let scope = entry.scope.iter().map(|s| key(s)).collect::<Vec<_>>();
        ensure!(
            graph.scopes.contains_key(&scope),
            "unknown lexical family boundary scope {scope:?}"
        );
        ensure!(
            !targeted.contains(&scope),
            "per-definition family boundary conflicts with per-occurrence assignment in {scope:?}"
        );
    }
    let variants = allocate_variants(graph, &roots)?;
    render_specialized(
        graph,
        items,
        &roots,
        &variants,
        &annotations,
        lexical_connections,
        &common,
    )
}
