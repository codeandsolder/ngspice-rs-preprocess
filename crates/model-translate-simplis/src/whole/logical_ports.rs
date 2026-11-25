//! Explicit original logical ALB port expansion into native three-node
//! physical bundles. No production translation path uses this module.

mod occurrence;
use super::{
    CompiledSourceAlbModel, Item, Result, bail, compile_explicit_alb_source_models,
    normalized_statement, parse_items, statement_tokens,
};
use crate::alb::DigitalPinDirection;
use anyhow::{Context, ensure};
pub use occurrence::{
    FamilySpecialization, OccurrenceFamilyBoundary, specialize_family_occurrences,
};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

/// Mixed root analog net and the intended distinct digital state wire.
#[derive(Clone, Debug)]
pub struct LogicalAlbBoundary {
    /// Lexical subcircuit definition path; empty for the root deck.
    pub scope: Vec<String>,
    pub analog_net: String,
    pub digital_net: String,
    /// The physical role at this analog boundary.
    pub direction: DigitalPinDirection,
    /// Exact ALB producer occurrence, for separating output pins that
    /// previously shared one logical state. Absent for existing boundaries.
    pub producer: Option<AlbOutputProducer>,
}

/// Exact source occurrence path relative to the annotated lexical scope.
///
/// All but the final component are X instance names, and the last is the
/// original ALB A instance. Output index identifies its physical bit.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlbOutputProducer {
    pub path: Vec<String>,
    pub output_pin: usize,
}

/// Lowered original source and the compiled code-model subcircuit library.
#[derive(Clone, Debug)]
pub struct LogicalAlbExpansion {
    pub deck: String,
    pub library: String,
}

#[derive(Clone, Debug)]
struct Scope {
    ports: Vec<String>,
    suffix: Vec<String>,
    items: Vec<Item>,
    digital: BTreeSet<String>,
    /// Directional uses reachable through this scope and its X descendants.
    /// These are distinct from the undirected digital connectivity graph:
    /// an upstream driver must not turn a sensing-only child into a driver.
    alb_roles: BTreeMap<String, BTreeSet<DigitalPinDirection>>,
    /// Formal ports whose exported side remains analog while an internal
    /// compiled ALB uses a distinct physical digital bundle.
    analog_cut_formals: BTreeSet<String>,
}

#[derive(Clone)]
struct LogicalInstance {
    name: String,
    inputs: Vec<String>,
    outputs: Vec<String>,
    target: usize,
}

#[derive(Clone)]
struct Call {
    target: Vec<String>,
    actual: Vec<String>,
    suffix: Vec<String>,
    name: String,
}

#[derive(Clone)]
struct ResolvedAlbProducer {
    identity: AlbOutputProducer,
    /// Immediate source instance, its output formal (or direct ALB net),
    /// and direct ALB output bit. The first divergent interface is split.
    slot: (String, String, usize),
}

#[derive(Default)]
struct Graph {
    scopes: BTreeMap<Vec<String>, Scope>,
    models: Vec<CompiledSourceAlbModel>,
}

fn key(text: &str) -> String {
    text.to_ascii_uppercase()
}

fn valid_node(text: &str) -> Result<String> {
    crate::validate_node(text)?;
    ensure!(text != "0", "digital ALB node cannot be analog ground 0");
    Ok(key(text))
}

fn header(header: &str) -> Result<(Vec<String>, Vec<String>)> {
    let normalized = normalized_statement(header);
    let fields = statement_tokens(&normalized)?;
    ensure!(
        fields.len() >= 2 && fields[0].eq_ignore_ascii_case(".subckt"),
        "malformed original .subckt header {header}"
    );
    let mut ports = Vec::new();
    let mut at = 2;
    while let Some(value) = fields.get(at) {
        if value.eq_ignore_ascii_case("params:")
            || value.eq_ignore_ascii_case("params")
            || value.contains('=')
        {
            break;
        }
        ports.push(valid_node(value)?);
        at += 1;
    }
    ensure!(
        ports.iter().collect::<BTreeSet<_>>().len() == ports.len(),
        "duplicate subcircuit formal port in {header}"
    );
    Ok((ports, fields[at..].to_vec()))
}

impl Graph {
    fn collect(&mut self, items: &[Item], scope: &[String], header: Option<&str>) -> Result<()> {
        let (ports, suffix) = if let Some(head) = header {
            self::header(head)?
        } else {
            (Vec::new(), Vec::new())
        };
        ensure!(
            self.scopes
                .insert(
                    scope.to_vec(),
                    Scope {
                        ports,
                        suffix,
                        items: items.to_vec(),
                        digital: BTreeSet::new(),
                        alb_roles: BTreeMap::new(),
                        analog_cut_formals: BTreeSet::new(),
                    }
                )
                .is_none(),
            "duplicate lexical subcircuit scope {scope:?}"
        );
        for item in items {
            if let Item::Subcircuit(child) = item {
                let mut path = scope.to_vec();
                path.push(key(&child.name));
                self.collect(&child.body, &path, Some(&child.header))?;
            }
        }
        Ok(())
    }

    fn lookup_model(&self, scope: &[String], name: &str) -> Option<usize> {
        let mut context = scope.to_vec();
        loop {
            if let Some(index) = self
                .models
                .iter()
                .position(|m| m.scope == context && m.model_name.eq_ignore_ascii_case(name))
            {
                return Some(index);
            }
            context.pop()?;
        }
    }

    fn lookup_subckt(&self, scope: &[String], name: &str) -> Option<Vec<String>> {
        let mut path = scope.to_vec();
        loop {
            let mut target = path.clone();
            target.push(key(name));
            if self.scopes.contains_key(&target) {
                return Some(target);
            }
            path.pop()?;
        }
    }

    fn call(&self, statement: &str, scope: &[String]) -> Result<Option<Call>> {
        let fields = statement_tokens(&normalized_statement(statement))?;
        let Some(instance) = fields.first() else {
            return Ok(None);
        };
        if !instance.starts_with(['X', 'x']) || instance.len() <= 1 {
            return Ok(None);
        }
        let mut found = Vec::new();
        for (idx, token) in fields.iter().enumerate().skip(1) {
            let Some(target) = self.lookup_subckt(scope, token) else {
                continue;
            };
            let model = self.scopes.get(&target).context("missing lexical subckt")?;
            if idx != model.ports.len() + 1 {
                continue;
            }
            let suffix = &fields[idx + 1..];
            if !suffix.first().is_none_or(|x| {
                x.eq_ignore_ascii_case("params:")
                    || x.eq_ignore_ascii_case("params")
                    || x.contains('=')
            }) {
                continue;
            }
            found.push(Call {
                target,
                actual: fields[1..idx].iter().map(|v| key(v)).collect(),
                suffix: suffix.to_vec(),
                name: instance.clone(),
            });
        }
        match found.as_slice() {
            [] => Ok(None),
            [item] => Ok(Some(item.clone())),
            _ => bail!("ambiguous X instance model lookup in {statement}"),
        }
    }

    fn alb_instance(&self, statement: &str, scope: &[String]) -> Result<Option<LogicalInstance>> {
        let normalized = normalized_statement(statement);
        let fields = statement_tokens(&normalized)?;
        let Some(inst) = fields.first() else {
            return Ok(None);
        };
        if !inst.starts_with(['A', 'a']) || inst.len() <= 1 {
            return Ok(None);
        }
        let Some(last) = fields.last() else {
            return Ok(None);
        };
        let model_name = last
            .strip_prefix("MODEL=")
            .or_else(|| last.strip_prefix("model="))
            .unwrap_or(last);
        let Some(target) = self.lookup_model(scope, model_name) else {
            return Ok(None);
        };
        let model = &self.models[target];
        let raw = normalized[inst.len()..].trim();
        let ports = if raw.starts_with('[') {
            let mut groups = Vec::new();
            let mut remaining = raw;
            while let Some(stripped) = remaining.trim_start().strip_prefix('[') {
                let close = stripped
                    .find(']')
                    .context("unterminated original XSPICE ALB node vector")?;
                let content = stripped[..close]
                    .split_ascii_whitespace()
                    .map(valid_node)
                    .collect::<Result<Vec<_>>>()?;
                groups.push(content);
                remaining = &stripped[close + 1..];
            }
            ensure!(
                groups.len() == 2,
                "ALB {inst} must have input and output port vectors"
            );
            let tail = remaining.trim();
            ensure!(
                tail.eq_ignore_ascii_case(model_name)
                    || tail.to_ascii_uppercase() == format!("MODEL={}", key(model_name)),
                "unsupported ALB {inst} instance parameters {tail}"
            );
            groups
        } else {
            let tokens = raw.split_ascii_whitespace().collect::<Vec<_>>();
            ensure!(
                tokens.len() == model.input_width + model.output_width + 1,
                "ALB {inst} logical port count does not match model"
            );
            vec![
                tokens[..model.input_width]
                    .iter()
                    .map(|s| valid_node(s))
                    .collect::<Result<Vec<_>>>()?,
                tokens[model.input_width..tokens.len() - 1]
                    .iter()
                    .map(|s| valid_node(s))
                    .collect::<Result<Vec<_>>>()?,
            ]
        };
        ensure!(
            ports[0].len() == model.input_width && ports[1].len() == model.output_width,
            "ALB {inst} pin arity differs from compiled model {}",
            model.model_name
        );
        Ok(Some(LogicalInstance {
            name: inst.clone(),
            inputs: ports[0].clone(),
            outputs: ports[1].clone(),
            target,
        }))
    }

    /// Mark intentional analog-side exported formal ports before digital
    /// role propagation. This severs only the graph edge at the annotated
    /// analog/digital boundary, not adjacent unrelated digital connections.
    fn mark_exported_cuts(&mut self, boundaries: &[LogicalAlbBoundary]) -> Result<()> {
        for boundary in boundaries {
            let scope = boundary
                .scope
                .iter()
                .map(|name| key(name))
                .collect::<Vec<_>>();
            let analog = valid_node(&boundary.analog_net)?;
            let digital = valid_node(&boundary.digital_net)?;
            let owner = self
                .scopes
                .get_mut(&scope)
                .with_context(|| format!("unknown mixed boundary scope {scope:?}"))?;
            if owner.ports.contains(&analog) {
                ensure!(
                    analog != digital,
                    "exported analog formal {analog} requires a different internal digital state"
                );
                ensure!(
                    !owner.ports.contains(&digital),
                    "exported analog boundary {analog} cannot map to exported digital formal {digital}"
                );
                owner.analog_cut_formals.insert(analog);
            }
        }
        Ok(())
    }

    fn discover_roles(&mut self) -> Result<()> {
        let scopes = self.scopes.keys().cloned().collect::<Vec<_>>();
        for scope in &scopes {
            let items = self
                .scopes
                .get(scope)
                .context("missing scope")?
                .items
                .clone();
            for item in items {
                if let Item::Statement(statement) = item
                    && let Some(alb) = self.alb_instance(&statement, scope)?
                {
                    let info = self
                        .scopes
                        .get_mut(scope)
                        .context("missing mutable scope")?;
                    info.digital
                        .extend(alb.inputs.into_iter().chain(alb.outputs));
                }
            }
        }
        loop {
            let mut changes = BTreeMap::<Vec<String>, BTreeSet<String>>::new();
            for scope in &scopes {
                let info = self.scopes.get(scope).context("missing scope")?;
                for item in &info.items {
                    if let Item::Statement(stmt) = item
                        && let Some(call) = self.call(stmt, scope)?
                    {
                        let child = self.scopes.get(&call.target).context("missing child")?;
                        for (formal, actual) in child.ports.iter().zip(&call.actual) {
                            if child.analog_cut_formals.contains(formal) {
                                continue;
                            }
                            if info.digital.contains(actual) || child.digital.contains(formal) {
                                changes
                                    .entry(scope.clone())
                                    .or_default()
                                    .insert(actual.clone());
                                changes
                                    .entry(call.target.clone())
                                    .or_default()
                                    .insert(formal.clone());
                            }
                        }
                    }
                }
            }
            let mut any_added = false;
            for (scope, nodes) in changes {
                let current = &mut self
                    .scopes
                    .get_mut(&scope)
                    .context("missing scope")?
                    .digital;
                for n in nodes {
                    any_added |= current.insert(n);
                }
            }
            if !any_added {
                break;
            }
        }
        Ok(())
    }
    /// Resolve every physical ALB output occurrence attached to an original
    /// source node, preserving X instance identity (not merely the reusable
    /// child definition identity). No implied driver ordering or selection.
    fn producer_paths(
        &self,
        scope: &[String],
        node: &str,
        stack: &mut Vec<Vec<String>>,
    ) -> Result<Vec<ResolvedAlbProducer>> {
        ensure!(stack.len() < 256, "ALB X hierarchy exceeds 256 levels");
        ensure!(
            !stack.iter().any(|active| active == scope),
            "recursive ALB producer hierarchy at {scope:?}"
        );
        stack.push(scope.to_vec());
        let info = self
            .scopes
            .get(scope)
            .context("missing ALB producer scope")?;
        let mut result = Vec::new();
        for item in &info.items {
            let Item::Statement(statement) = item else {
                continue;
            };
            if let Some(alb) = self.alb_instance(statement, scope)? {
                for (bit, output) in alb.outputs.iter().enumerate() {
                    if output == node {
                        result.push(ResolvedAlbProducer {
                            identity: AlbOutputProducer {
                                path: vec![key(&alb.name)],
                                output_pin: bit,
                            },
                            slot: (key(&alb.name), node.to_owned(), bit),
                        });
                    }
                }
            } else if let Some(call) = self.call(statement, scope)? {
                let child = self
                    .scopes
                    .get(&call.target)
                    .context("missing producer child")?;
                for (formal, actual) in child.ports.iter().zip(&call.actual) {
                    if actual != node || child.analog_cut_formals.contains(formal) {
                        continue;
                    }
                    for mut resolved in self.producer_paths(&call.target, formal, stack)? {
                        resolved.identity.path.insert(0, key(&call.name));
                        resolved.slot = (key(&call.name), formal.clone(), 0);
                        result.push(resolved);
                    }
                }
            }
        }
        stack.pop();
        Ok(result)
    }

    /// Count distinct ALB output producers at a logical net, across actual
    /// X call sites. One DAC bridge per analog output boundary cannot safely
    /// collapse two drivers into one digital triple: that would resolve
    /// contention digitally rather than through the physical analog pin.
    fn output_producers(
        &self,
        scope: &[String],
        node: &str,
        stack: &mut Vec<Vec<String>>,
    ) -> Result<usize> {
        ensure!(stack.len() < 256, "ALB X hierarchy exceeds 256 levels");
        ensure!(
            !stack.iter().any(|active| active == scope),
            "recursive ALB output producer graph at {scope:?}"
        );
        stack.push(scope.to_vec());
        let info = self.scopes.get(scope).context("missing output scope")?;
        let mut producers = 0usize;
        for item in &info.items {
            let Item::Statement(statement) = item else {
                continue;
            };
            if let Some(alb) = self.alb_instance(statement, scope)? {
                producers += alb.outputs.iter().filter(|output| *output == node).count();
            } else if let Some(call) = self.call(statement, scope)? {
                let child = self
                    .scopes
                    .get(&call.target)
                    .context("missing output child")?;
                for (formal, actual) in child.ports.iter().zip(&call.actual) {
                    if actual == node && !child.analog_cut_formals.contains(formal) {
                        producers += self.output_producers(&call.target, formal, stack)?;
                        if producers > 1 {
                            break;
                        }
                    }
                }
            }
            if producers > 1 {
                break;
            }
        }
        stack.pop();
        Ok(producers.min(2))
    }

    /// Propagate the actual ALB input/output roles *up* through lexical X
    /// interfaces. Unlike the physical connectivity flood-fill, directions
    /// must not propagate down from arbitrary callers: a child that only
    /// senses a pin must remain an input even when its parent also drives it.
    fn discover_alb_roles(&mut self) -> Result<()> {
        let scopes = self.scopes.keys().cloned().collect::<Vec<_>>();
        for scope in &scopes {
            let items = self
                .scopes
                .get(scope)
                .context("missing scope")?
                .items
                .clone();
            for item in items {
                if let Item::Statement(statement) = item
                    && let Some(alb) = self.alb_instance(&statement, scope)?
                {
                    let roles = &mut self
                        .scopes
                        .get_mut(scope)
                        .context("missing ALB scope")?
                        .alb_roles;
                    for input in alb.inputs {
                        roles
                            .entry(input)
                            .or_default()
                            .insert(DigitalPinDirection::Input);
                    }
                    for output in alb.outputs {
                        roles
                            .entry(output)
                            .or_default()
                            .insert(DigitalPinDirection::Output);
                    }
                }
            }
        }
        loop {
            let mut changes =
                BTreeMap::<Vec<String>, BTreeMap<String, BTreeSet<DigitalPinDirection>>>::new();
            for scope in &scopes {
                let parent = self.scopes.get(scope).context("missing parent")?;
                for item in &parent.items {
                    if let Item::Statement(statement) = item
                        && let Some(call) = self.call(statement, scope)?
                    {
                        let child = self.scopes.get(&call.target).context("missing child")?;
                        for (formal, actual) in child.ports.iter().zip(&call.actual) {
                            if child.analog_cut_formals.contains(formal) {
                                continue;
                            }
                            if let Some(directions) = child.alb_roles.get(formal) {
                                changes
                                    .entry(scope.clone())
                                    .or_default()
                                    .entry(actual.clone())
                                    .or_default()
                                    .extend(directions);
                            }
                        }
                    }
                }
            }
            let mut any_added = false;
            for (scope, per_node) in changes {
                let roles = &mut self
                    .scopes
                    .get_mut(&scope)
                    .context("missing directional scope")?
                    .alb_roles;
                for (node, directions) in per_node {
                    let current = roles.entry(node).or_default();
                    for direction in directions {
                        any_added |= current.insert(direction);
                    }
                }
            }
            if !any_added {
                break;
            }
        }
        Ok(())
    }
}

/// Direction-aware source-node renames. Most logical signals have exactly
/// one state triple; an analog-facing node used as both an ALB input and
/// output has two independent digital state triples instead.
type ProducerSlot = (String, String, usize);

#[derive(Clone)]
struct PromotedOutput {
    identity: AlbOutputProducer,
    slot: ProducerSlot,
}

#[derive(Default)]
struct ScopeRenames {
    states: BTreeMap<String, String>,
    output_states: BTreeMap<String, String>,
    /// (immediate instance, its formal/ALB node, output bit) specifies
    /// a separately wired physical producer; definitions remain reusable.
    producer_states: BTreeMap<ProducerSlot, String>,
    /// Additional exported physical output bundles per original X formal.
    /// Every lexical call site must supply these in the same stable order.
    promoted_formals: BTreeMap<String, Vec<PromotedOutput>>,
}
impl ScopeRenames {
    fn contains_key(&self, original: &str) -> bool {
        self.states.contains_key(original)
    }
}

#[derive(Clone)]
struct ScopeBundles {
    ports: BTreeMap<String, [String; 3]>,
    alb_outputs: BTreeMap<String, [String; 3]>,
    producers: BTreeMap<ProducerSlot, [String; 3]>,
    promoted_formals: BTreeMap<String, Vec<PromotedOutput>>,
}
impl ScopeBundles {
    fn triple(&self, name: &str) -> Result<&[String; 3]> {
        self.ports
            .get(&key(name))
            .with_context(|| format!("digital node {name} lacks physical metadata"))
    }
    fn rendered(&self, name: &str) -> Result<Vec<String>> {
        Ok(self.triple(name)?.to_vec())
    }
    fn alb_output(&self, name: &str) -> Result<Vec<String>> {
        let name = key(name);
        Ok(self
            .alb_outputs
            .get(&name)
            .or_else(|| self.ports.get(&name))
            .with_context(|| format!("ALB output {name} lacks physical metadata"))?
            .to_vec())
    }
    fn producer(&self, name: &str, node: &str, output_bit: usize) -> Option<Vec<String>> {
        self.producers
            .get(&(key(name), key(node), output_bit))
            .map(|bundle| bundle.to_vec())
    }
}
fn reserved(scope: &Scope) -> BTreeSet<String> {
    let mut names = scope.ports.iter().cloned().collect::<BTreeSet<_>>();
    for item in &scope.items {
        if let Item::Statement(stmt) = item {
            for field in stmt.split(|ch: char| {
                !ch.is_ascii_alphanumeric()
                    && !matches!(ch, '_' | '$' | '.' | '#' | ':' | '+' | '-' | '*')
            }) {
                if !field.is_empty() {
                    names.insert(key(field));
                }
            }
        }
    }
    names
}
fn build_bundles(scope: &Scope, renames: &ScopeRenames) -> Result<ScopeBundles> {
    let mut reserved = reserved(scope);
    // Protect every chosen state name *before* allocating any metadata.
    // Otherwise a generated _SR_SINK name may collide with a later
    // separately selected input/output state.
    for original in &scope.digital {
        let state = renames.states.get(original).unwrap_or(original);
        reserved.insert(state.clone());
        if let Some(output) = renames.output_states.get(original) {
            reserved.insert(output.clone());
        }
    }
    for state in renames.producer_states.values() {
        reserved.insert(state.clone());
    }
    let mut used_states = BTreeSet::new();
    let mut physical = BTreeMap::new();
    let mut alb_outputs = BTreeMap::new();
    let mut ordinal = 0usize;
    for original in &scope.digital {
        let state = renames
            .states
            .get(original)
            .cloned()
            .unwrap_or_else(|| original.clone());
        let state = valid_node(&state)?;
        ensure!(
            used_states.insert(state.clone()),
            "multiple logical roles map to one digital state {state}"
        );
        let primary = allocate_bundle(&state, &mut reserved, ordinal)?;
        ordinal += 1;
        physical.insert(original.clone(), primary);
        if let Some(output) = renames.output_states.get(original) {
            let output = valid_node(output)?;
            ensure!(
                used_states.insert(output.clone()),
                "multiple logical roles map to one digital state {output}"
            );
            let triple = allocate_bundle(&output, &mut reserved, ordinal)?;
            ordinal += 1;
            alb_outputs.insert(original.clone(), triple);
        }
    }
    let mut producers = BTreeMap::new();
    for (identity, state) in &renames.producer_states {
        let state = valid_node(state)?;
        ensure!(
            used_states.insert(state.clone()),
            "producer state {state} overlaps an existing digital state"
        );
        let bundle = allocate_bundle(&state, &mut reserved, ordinal)?;
        ordinal += 1;
        producers.insert(identity.clone(), bundle);
    }
    Ok(ScopeBundles {
        ports: physical,
        alb_outputs,
        producers,
        promoted_formals: renames.promoted_formals.clone(),
    })
}

fn allocate_bundle(
    state: &str,
    reserved: &mut BTreeSet<String>,
    ordinal: usize,
) -> Result<[String; 3]> {
    let mut suffix = 0u32;
    let (sink, source) = loop {
        let stem = if suffix == 0 {
            format!("{state}_SR")
        } else {
            format!("SR_META{ordinal}_{suffix}")
        };
        let sink = format!("{stem}_SINK");
        let source = format!("{stem}_SOURCE");
        if !reserved.contains(&sink) && !reserved.contains(&source) {
            break (sink, source);
        }
        suffix = suffix
            .checked_add(1)
            .context("exhausted unique ALB metadata names")?;
    };
    reserved.insert(sink.clone());
    reserved.insert(source.clone());
    Ok([state.to_owned(), sink, source])
}
fn expanded_header(scope: &[String], info: &Scope, nodes: &ScopeBundles) -> Result<String> {
    let name = scope.last().context("cannot render root as subcircuit")?;
    let mut output = format!(".subckt {name}");
    for formal in &info.ports {
        if info.digital.contains(formal) && !info.analog_cut_formals.contains(formal) {
            for part in nodes.rendered(formal)? {
                write!(output, " {part}")?;
            }
        } else {
            write!(output, " {formal}")?;
        }
    }
    for promoted in nodes.promoted_formals.values().flatten() {
        let slot = &promoted.slot;
        let parts = nodes
            .producer(&slot.0, &slot.1, slot.2)
            .context("promoted output formal lacks a physical bundle")?;
        for part in parts {
            write!(output, " {part}")?;
        }
    }
    for tail in &info.suffix {
        write!(output, " {tail}")?;
    }
    Ok(output)
}
fn guard_unknown_device(
    statement: &str,
    digital: &BTreeSet<String>,
    renames: &ScopeRenames,
) -> Result<()> {
    let fields = statement_tokens(&normalized_statement(statement))?;
    let Some(inst) = fields.first() else {
        return Ok(());
    };
    if inst.starts_with(['.', '*', ';']) {
        return Ok(());
    }
    for part in statement.split(|ch: char| {
        !ch.is_ascii_alphanumeric() && !matches!(ch, '_' | '$' | '.' | '#' | ':' | '+' | '-' | '*')
    }) {
        let name = key(part);
        if digital.contains(&name) {
            if inst.starts_with(['A', 'a']) && inst.len() > 1 {
                bail!(
                    "unverified original A-device {inst} touches mixed digital/analog node {name}"
                );
            }
            if !renames.contains_key(&name) {
                bail!(
                    "untranslated device {inst} connects digital net {name}; physical interface unknown"
                );
            }
        }
    }
    Ok(())
}
struct RenderContext<'a> {
    scope: &'a [String],
    maps: &'a BTreeMap<Vec<String>, ScopeBundles>,
    renames: &'a ScopeRenames,
    compiled_file: &'a str,
}

/// Select the physical state for the original X formal. Any additional
/// promoted outputs travel on appended triples, never on this original bus.
fn call_original_bundle(
    call: &Call,
    formal: &str,
    original: &str,
    child: &Scope,
    child_bundles: &ScopeBundles,
    physical: &ScopeBundles,
    renames: &ScopeRenames,
) -> Result<Vec<String>> {
    if child_bundles.promoted_formals.contains_key(formal) {
        // The original formal remains the sensing input when this exported
        // node was bidirectional. When it is output-only, no internal ALB
        // consumes it: reuse the first selected producer triple rather than
        // exposing a fake physical wire named like the analog pin.
        if !child
            .alb_roles
            .get(formal)
            .is_some_and(|roles| roles.contains(&DigitalPinDirection::Input))
            && let Some(parts) = physical.producer(&call.name, formal, 0)
        {
            return Ok(parts);
        }
        return physical.rendered(original);
    }
    if let Some(bundle) = physical.producer(&call.name, formal, 0) {
        ensure!(
            child.alb_roles.get(formal).is_some_and(|roles| {
                roles.len() == 1 && roles.contains(&DigitalPinDirection::Output)
            }),
            "selected producer {} has a mixed-role formal {formal}; its outputs require a promoted X interface",
            call.name
        );
        return Ok(bundle);
    }
    if renames.output_states.contains_key(original) {
        let directions = child.alb_roles.get(formal);
        ensure!(
            directions.is_some_and(|roles| roles.len() == 1),
            "ambiguous nested role for {formal} of {} on split analog pin {original}",
            call.name
        );
        if directions.is_some_and(|roles| roles.contains(&DigitalPinDirection::Output)) {
            return physical.alb_output(original);
        }
    }
    physical.rendered(original)
}

/// Append separately promoted output formals in exactly the same order as
/// the extended .subckt declaration. Unselected calls preserve their
/// preexisting logical digital bus by passing its original triple.
fn render_promoted_call_outputs(
    call: &Call,
    child: &Scope,
    child_bundles: &ScopeBundles,
    physical: &ScopeBundles,
    renames: &ScopeRenames,
    output: &mut String,
) -> Result<()> {
    for (formal, outputs) in &child_bundles.promoted_formals {
        let position = child
            .ports
            .iter()
            .position(|port| port == formal)
            .context("promoted output does not belong to the child formal interface")?;
        let actual = &call.actual[position];
        for index in 0..outputs.len() {
            let parts = if let Some(selected) = physical.producer(&call.name, formal, index) {
                selected
            } else if renames.output_states.contains_key(actual) {
                physical.alb_output(actual)?
            } else {
                physical.rendered(actual)?
            };
            for part in parts {
                write!(output, " {part}")?;
            }
        }
    }
    Ok(())
}

impl Graph {
    fn render_call(&self, call: &Call, ctx: &RenderContext<'_>, output: &mut String) -> Result<()> {
        let child = self
            .scopes
            .get(&call.target)
            .context("missing X subcircuit")?;
        let parent = self.scopes.get(ctx.scope).context("missing parent scope")?;
        let physical = ctx
            .maps
            .get(ctx.scope)
            .context("missing physical node map")?;
        let child_bundles = ctx
            .maps
            .get(&call.target)
            .context("missing promoted child physical mapping")?;
        write!(output, "{}", call.name)?;
        for (idx, original) in call.actual.iter().enumerate() {
            let formal = &child.ports[idx];
            if child.analog_cut_formals.contains(formal) {
                ensure!(
                    !parent.digital.contains(original)
                        || ctx
                            .renames
                            .states
                            .get(original)
                            .is_some_and(|digital| digital != original),
                    "analog-facing formal {formal} of {} connects to unsplit digital node {original} in {:?}",
                    call.name,
                    ctx.scope
                );
                write!(output, " {original}")?;
            } else if child.digital.contains(formal) {
                let parts = call_original_bundle(
                    call,
                    formal,
                    original,
                    child,
                    child_bundles,
                    physical,
                    ctx.renames,
                )?;
                for part in parts {
                    write!(output, " {part}")?;
                }
            } else {
                write!(output, " {original}")?;
            }
        }
        render_promoted_call_outputs(call, child, child_bundles, physical, ctx.renames, output)?;
        write!(
            output,
            " {}",
            call.target.last().context("unnamed subcircuit")?
        )?;
        for tail in &call.suffix {
            write!(output, " {tail}")?;
        }
        output.push('\n');
        Ok(())
    }

    fn render_statement(
        &self,
        statement: &str,
        ctx: &RenderContext<'_>,
        in_file: &mut bool,
        names: &mut BTreeSet<String>,
        output: &mut String,
    ) -> Result<()> {
        let scope = ctx.scope;
        let maps = ctx.maps;
        let renames = ctx.renames;
        let compiled_file = ctx.compiled_file;
        let info = self
            .scopes
            .get(scope)
            .context("missing statement render scope")?;
        let physical = maps
            .get(scope)
            .context("missing statement physical mapping")?;
        let normalized = normalized_statement(statement);
        let first = normalized
            .split_ascii_whitespace()
            .next()
            .unwrap_or_default();
        if first.eq_ignore_ascii_case(".file") {
            ensure!(!*in_file, "nested .file in ALB source");
            *in_file = true;
            return Ok(());
        }
        if first.eq_ignore_ascii_case(".endf") {
            ensure!(*in_file, "orphan .endf in ALB source");
            *in_file = false;
            return Ok(());
        }
        if *in_file {
            return Ok(());
        }
        if let Some((_, kind)) = super::model_header(&normalized)
            && kind == "D_LOGIC_BLOCK"
        {
            return Ok(());
        }
        if let Some(alb) = self.alb_instance(statement, scope)? {
            let device = format!("XSR_{}", key(&alb.name));
            ensure!(
                names.insert(device.clone()),
                "duplicate ALB instance name {device}"
            );
            write!(output, "{device}")?;
            for state in &alb.inputs {
                for part in physical.rendered(state)? {
                    write!(output, " {part}")?;
                }
            }
            for (bit, state) in alb.outputs.iter().enumerate() {
                let triple = physical
                    .producer(&alb.name, state, bit)
                    .map_or_else(|| physical.alb_output(state), Ok)?;
                for part in triple {
                    write!(output, " {part}")?;
                }
            }
            writeln!(output, " {}", self.models[alb.target].subcircuit_name)?;
            return Ok(());
        }
        if let Some(call) = self.call(statement, scope)? {
            self.render_call(&call, ctx, output)?;
            return Ok(());
        }
        if first.starts_with(['X', 'x']) && first.len() > 1 {
            let fields = statement_tokens(&normalized)?;
            if fields
                .iter()
                .skip(1)
                .any(|n| info.digital.contains(&key(n)))
            {
                bail!("unresolved external X instance {first} touches digital state");
            }
        }
        guard_unknown_device(statement, &info.digital, renames)?;
        if scope.is_empty() && first.eq_ignore_ascii_case(".end") {
            writeln!(output, ".include \"{compiled_file}\"")?;
        }
        output.push_str(statement);
        output.push('\n');
        Ok(())
    }

    fn render(
        &self,
        items: &[Item],
        scope: &[String],
        maps: &BTreeMap<Vec<String>, ScopeBundles>,
        scoped_renames: &BTreeMap<Vec<String>, ScopeRenames>,
        compiled_file: &str,
        output: &mut String,
    ) -> Result<()> {
        let empty = ScopeRenames::default();
        let renames = scoped_renames.get(scope).unwrap_or(&empty);
        let mut in_file = false;
        let mut names = items
            .iter()
            .filter_map(|item| {
                if let Item::Statement(stmt) = item {
                    let first = stmt.trim().split_ascii_whitespace().next()?;
                    if first.len() > 1 && first.starts_with(['X', 'x']) {
                        return Some(key(first));
                    }
                }
                None
            })
            .collect::<BTreeSet<_>>();
        for item in items {
            match item {
                Item::Subcircuit(child) => {
                    let mut path = scope.to_vec();
                    path.push(key(&child.name));
                    let child_info = self.scopes.get(&path).context("missing child scope")?;
                    let child_nodes = maps.get(&path).context("missing child node mapping")?;
                    writeln!(
                        output,
                        "{}",
                        expanded_header(&path, child_info, child_nodes)?
                    )?;
                    self.render(
                        &child.body,
                        &path,
                        maps,
                        scoped_renames,
                        compiled_file,
                        output,
                    )?;
                    writeln!(output, ".ends {}", key(&child.name))?;
                }
                Item::Statement(statement) => {
                    self.render_statement(
                        statement,
                        &RenderContext {
                            scope,
                            maps,
                            renames,
                            compiled_file,
                        },
                        &mut in_file,
                        &mut names,
                        output,
                    )?;
                }
            }
        }
        ensure!(!in_file, "unterminated ALB embedded .file in render");
        Ok(())
    }
}

/// Split a single original ALB net into independent ADC and DAC state
/// bundles, with the only feedback path running through the analog pin.
/// A purely digital X child needs an additional explicit interface boundary
/// before this split can be represented safely.
fn plan_original_node_roles(
    graph: &Graph,
    scope: &Scope,
    path: &[String],
    analog: &str,
    roles: &BTreeMap<DigitalPinDirection, String>,
) -> Result<(String, Option<String>)> {
    let used = scope.alb_roles.get(analog).cloned().unwrap_or_default();
    if used.contains(&DigitalPinDirection::Output) {
        ensure!(
            graph.output_producers(path, analog, &mut Vec::new())? == 1,
            "analog boundary {analog} in {path:?} has multiple ALB output drivers; each needs a separate physical DAC"
        );
    }
    if used.len() != 2 {
        // A source node with one actual ALB direction needs one triple.
        // An annotation with an unproved direction may remain a separate
        // explicitly supplied boundary; it must not imply an extra driver.
        ensure!(
            roles.len() == 1,
            "multiple physical directions on {analog} in {path:?} require verifiable ALB input and output roles"
        );
        if let Some(role) = used.iter().next() {
            ensure!(
                roles.contains_key(role),
                "boundary direction for {analog} in {path:?} differs from its ALB pin direction"
            );
        }
        let value = roles.values().next().context("missing selected boundary")?;
        return Ok((value.clone(), None));
    }
    let input = roles.get(&DigitalPinDirection::Input).with_context(|| {
        format!("bidirectional ALB node {analog} in {path:?} lacks ADC boundary")
    })?;
    let output = roles.get(&DigitalPinDirection::Output).with_context(|| {
        format!("bidirectional ALB node {analog} in {path:?} lacks DAC boundary")
    })?;
    ensure!(
        input != output && input != analog && output != analog,
        "bidirectional ALB node {analog} requires distinct analog/input/output nodes"
    );
    ensure!(
        !scope.ports.contains(&analog.to_owned()) || scope.analog_cut_formals.contains(analog),
        "bidirectional exported node {analog} has no analog-facing interface cut"
    );
    for item in &scope.items {
        if let Item::Statement(stmt) = item
            && let Some(call) = graph.call(stmt, path)?
        {
            let child = graph
                .scopes
                .get(&call.target)
                .context("missing child scope")?;
            for (formal, actual) in child.ports.iter().zip(&call.actual) {
                if actual == analog {
                    if child.analog_cut_formals.contains(formal) {
                        continue;
                    }
                    let directions = child.alb_roles.get(formal);
                    ensure!(
                        directions.is_some_and(|roles| roles.len() == 1),
                        "bidirectional ALB node {analog} connects to ambiguous digital formal {formal} of {}; a single X port cannot represent both physical directions",
                        call.name
                    );
                }
            }
        }
    }
    Ok((input.clone(), Some(output.clone())))
}

/// A boundary selects either an existing physical ALB state (false) or a
/// source node that needs a new private digital state (true).
/// Distinct existing output states may share one analog net only electrically.
fn needs_original_node_rename(
    scope: &Scope,
    path: &[String],
    analog: &str,
    digital: &str,
    direction: DigitalPinDirection,
) -> Result<bool> {
    let source_role = scope
        .alb_roles
        .get(analog)
        .is_some_and(|roles| roles.contains(&direction));
    let existing_role = digital != analog
        && scope
            .alb_roles
            .get(digital)
            .is_some_and(|roles| roles.contains(&direction));
    if existing_role {
        // The selected state belongs to a different ALB pin. Multiple
        // independent DACs can share an analog net, but not a digital state.
        ensure!(
            !source_role,
            "ambiguous {direction:?} mapping for {analog} in {path:?}: original and selected state {digital} are both ALB pins"
        );
        return Ok(false);
    }
    if scope.digital.contains(analog) {
        ensure!(
            digital == analog || !reserved(scope).contains(digital),
            "chosen split digital state {digital} already exists in {path:?}"
        );
    } else {
        ensure!(
            scope.digital.contains(digital),
            "boundary {path:?}/{analog}/{digital} has no ALB digital state"
        );
        ensure!(
            scope
                .alb_roles
                .get(digital)
                .is_some_and(|roles| roles.contains(&direction)),
            "boundary {path:?}/{analog}/{digital} has no {direction:?} ALB pin"
        );
    }
    Ok(true)
}

/// Find the nearest independently routable physical output interface.
///
/// When one X formal carries several original ALB outputs, recursively
/// promote its per-producer bundles through the child definition first.
fn producer_slot(
    graph: &Graph,
    scope: &[String],
    producer: &ResolvedAlbProducer,
    plans: &mut BTreeMap<Vec<String>, ScopeRenames>,
    active: &mut Vec<(Vec<String>, String)>,
) -> Result<ProducerSlot> {
    if producer.identity.path.len() == 1 {
        return Ok(producer.slot.clone());
    }
    let instance_name = &producer.identity.path[0];
    let formal = &producer.slot.1;
    let info = graph
        .scopes
        .get(scope)
        .context("missing producer parent scope")?;
    let child = info
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Statement(statement) => Some(statement),
            Item::Subcircuit(_) => None,
        })
        .map(|statement| graph.call(statement, scope))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .find(|call| key(&call.name) == *instance_name)
        .with_context(|| format!("missing selected X instance {instance_name} in {scope:?}"))?;
    let child_outputs = graph.producer_paths(&child.target, formal, &mut Vec::new())?;
    if child_outputs.len() == 1 {
        return Ok((instance_name.clone(), formal.clone(), 0));
    }
    let promoted = promote_formal(graph, &child.target, formal, plans, active)?;
    let wanted = AlbOutputProducer {
        path: producer.identity.path[1..].to_vec(),
        output_pin: producer.identity.output_pin,
    };
    let position = promoted
        .iter()
        .position(|candidate| candidate.identity == wanted)
        .with_context(|| {
            format!(
                "producer {wanted:?} is not exposed by {formal} of {:?}",
                child.target
            )
        })?;
    Ok((instance_name.clone(), formal.clone(), position))
}

fn new_promoted_name(info: &Scope, plan: &ScopeRenames, ordinal: usize) -> Result<String> {
    let mut occupied = reserved(info);
    occupied.extend(info.digital.iter().cloned());
    occupied.extend(plan.states.values().cloned());
    occupied.extend(plan.output_states.values().cloned());
    occupied.extend(plan.producer_states.values().cloned());
    for suffix in 0..u32::MAX {
        let name = if suffix == 0 {
            format!("SR_PROMOTED_OUT_{ordinal}")
        } else {
            format!("SR_PROMOTED_OUT_{ordinal}_{suffix}")
        };
        if !occupied.contains(&name) {
            return valid_node(&name);
        }
    }
    bail!("no unique internal ALB promoted output state available")
}

/// Add physical output formals to a reusable X definition exactly once.
/// Per-instance callers can bind these same formals to different analog DACs.
fn promote_formal(
    graph: &Graph,
    scope: &[String],
    formal: &str,
    plans: &mut BTreeMap<Vec<String>, ScopeRenames>,
    active: &mut Vec<(Vec<String>, String)>,
) -> Result<Vec<PromotedOutput>> {
    if let Some(previous) = plans
        .get(scope)
        .and_then(|plan| plan.promoted_formals.get(formal))
    {
        return Ok(previous.clone());
    }
    let here = (scope.to_vec(), formal.to_owned());
    ensure!(
        !active.contains(&here) && active.len() < 256,
        "recursive ALB promoted output interface at {scope:?}/{formal}"
    );
    active.push(here);
    let found = graph.producer_paths(scope, formal, &mut Vec::new())?;
    ensure!(
        found.len() > 1,
        "promoted output formal {formal} in {scope:?} has fewer than two actual ALB drivers"
    );
    let mut promoted = Vec::new();
    let mut slots = BTreeSet::new();
    for item in found {
        let slot = producer_slot(graph, scope, &item, plans, active)?;
        ensure!(
            slots.insert(slot.clone()),
            "ALB physical outputs still collide at {slot:?} in {scope:?}"
        );
        promoted.push(PromotedOutput {
            identity: item.identity,
            slot,
        });
    }
    promoted.sort_by(|left, right| left.identity.cmp(&right.identity));
    let info = graph
        .scopes
        .get(scope)
        .context("missing promoted output scope")?;
    ensure!(
        info.ports.contains(&formal.to_owned()) && info.digital.contains(formal),
        "promoted ALB node {formal} is not a digital exported formal in {scope:?}"
    );
    let plan = plans.entry(scope.to_vec()).or_default();
    for promoted_output in &promoted {
        let state = new_promoted_name(info, plan, plan.producer_states.len())?;
        ensure!(
            plan.producer_states
                .insert(promoted_output.slot.clone(), state)
                .is_none(),
            "promoted output conflicts with another physical producer in {scope:?}"
        );
    }
    ensure!(
        plan.promoted_formals
            .insert(formal.to_owned(), promoted.clone())
            .is_none(),
        "duplicate promoted ALB formal {formal} in {scope:?}"
    );
    active.pop();
    Ok(promoted)
}

/// Prove that each original producer of the annotated logical net has
/// exactly one identity-aware physical DAC, without choosing a subset of the
/// drivers or inventing a priority. A reusable X definition is not cloned:
/// the first caller-side output interface is distinct per X occurrence.
fn plan_selected_producers(
    graph: &Graph,
    requests: BTreeMap<(Vec<String>, String), BTreeMap<AlbOutputProducer, String>>,
    plans: &mut BTreeMap<Vec<String>, ScopeRenames>,
) -> Result<()> {
    for ((scope, analog), selected) in requests {
        let owner = graph.scopes.get(&scope).context("lost producer scope")?;
        let discovered = graph.producer_paths(&scope, &analog, &mut Vec::new())?;
        ensure!(
            !discovered.is_empty(),
            "no original ALB output producer on {analog} in {scope:?}"
        );
        if owner
            .alb_roles
            .get(&analog)
            .is_some_and(|roles| roles.contains(&DigitalPinDirection::Input))
        {
            ensure!(
                plans
                    .get(&scope)
                    .is_some_and(|plan| plan.states.contains_key(&analog)),
                "original ALB node {analog} in {scope:?} is also sensed; provide a separate ADC boundary"
            );
        }
        let mut identities = BTreeSet::new();
        let mut slots = BTreeSet::new();
        let mut bindings = Vec::new();
        for producer in discovered {
            ensure!(
                identities.insert(producer.identity.clone()),
                "original producer {:?} of {analog} has multiple source paths",
                producer.identity
            );
            let state = selected.get(&producer.identity).with_context(|| {
                format!(
                    "missing physical DAC for original producer {:?} of {analog} in {scope:?}",
                    producer.identity
                )
            })?;
            let slot = producer_slot(graph, &scope, &producer, plans, &mut Vec::new())?;
            ensure!(
                slots.insert(slot.clone()),
                "multiple producers share physical output interface {slot:?} of {analog} in {scope:?}"
            );
            bindings.push((slot, state.clone()));
        }
        ensure!(
            identities.len() == selected.len(),
            "unmatched physical ALB producer annotation on {analog} in {scope:?}"
        );
        let plan = plans.entry(scope.clone()).or_default();
        for (slot, state) in bindings {
            ensure!(
                plan.producer_states.insert(slot, state).is_none(),
                "duplicate physical producer state for {analog} in {scope:?}"
            );
        }
    }
    Ok(())
}

type RequestedNodeRoles =
    BTreeMap<Vec<String>, BTreeMap<String, BTreeMap<DigitalPinDirection, String>>>;
type RequestedOutputProducers =
    BTreeMap<(Vec<String>, String), BTreeMap<AlbOutputProducer, String>>;

/// Collect and validate annotations before allocating any physical state.
fn collect_boundary_requests(
    graph: &Graph,
    boundaries: &[LogicalAlbBoundary],
) -> Result<(RequestedNodeRoles, RequestedOutputProducers)> {
    let mut requested =
        BTreeMap::<Vec<String>, BTreeMap<String, BTreeMap<DigitalPinDirection, String>>>::new();
    let mut producers =
        BTreeMap::<(Vec<String>, String), BTreeMap<AlbOutputProducer, String>>::new();
    let mut seen = BTreeSet::new();
    for boundary in boundaries {
        let path = boundary
            .scope
            .iter()
            .map(|name| key(name))
            .collect::<Vec<_>>();
        let scope = graph
            .scopes
            .get(&path)
            .with_context(|| format!("unknown lexical boundary subcircuit scope {path:?}"))?;
        let analog = valid_node(&boundary.analog_net)?;
        let digital = valid_node(&boundary.digital_net)?;
        ensure!(
            seen.insert((
                path.clone(),
                analog.clone(),
                boundary.direction,
                digital.clone(),
            )),
            "duplicate {:?} mixed boundary in {path:?}/{analog}/{digital}",
            boundary.direction
        );
        if !path.is_empty() {
            ensure!(
                !scope.ports.contains(&digital),
                "mixed boundary {path:?} cannot use an exported digital formal {digital}"
            );
            if scope.ports.contains(&analog) {
                ensure!(
                    scope.analog_cut_formals.contains(&analog),
                    "exported analog formal {analog} in {path:?} lacks a verified interface cut"
                );
            }
        }
        if let Some(producer) = &boundary.producer {
            ensure!(
                boundary.direction == DigitalPinDirection::Output,
                "selected ALB producer is only valid on a DAC output boundary"
            );
            ensure!(
                scope.digital.contains(&analog) && digital != analog,
                "producer {producer:?} must split an actual original ALB node {analog}"
            );
            ensure!(
                !reserved(scope).contains(&digital),
                "selected producer state {digital} already exists in {path:?}"
            );
            let identity = AlbOutputProducer {
                path: producer
                    .path
                    .iter()
                    .map(|segment| valid_node(segment))
                    .collect::<Result<Vec<_>>>()?,
                output_pin: producer.output_pin,
            };
            ensure!(!identity.path.is_empty(), "ALB producer path is empty");
            let previous = producers
                .entry((path, analog))
                .or_default()
                .insert(identity.clone(), digital);
            ensure!(
                previous.is_none(),
                "duplicate selected ALB producer {identity:?}"
            );
            continue;
        }
        if !needs_original_node_rename(scope, &path, &analog, &digital, boundary.direction)? {
            continue;
        }
        let previous = requested
            .entry(path.clone())
            .or_default()
            .entry(analog.clone())
            .or_default()
            .insert(boundary.direction, digital);
        ensure!(
            previous.is_none(),
            "multiple {:?} renames for original ALB node {analog} in {path:?} need distinct identified physical drivers",
            boundary.direction
        );
    }

    Ok((requested, producers))
}

fn plan_scope_renames(
    graph: &Graph,
    boundaries: &[LogicalAlbBoundary],
) -> Result<BTreeMap<Vec<String>, ScopeRenames>> {
    // A physical analog pin is allowed to have both an ADC and a DAC,
    // with *different* digital state triples, even when the original
    // ALB spells the input and output pins identically.
    let (requested, producers) = collect_boundary_requests(graph, boundaries)?;
    let mut result = BTreeMap::new();
    for (path, per_analog) in requested {
        let scope = graph.scopes.get(&path).context("lost lexical scope")?;
        let mut planned = ScopeRenames::default();
        for (analog, roles) in per_analog {
            if !scope.digital.contains(&analog) {
                continue;
            }
            if producers.contains_key(&(path.clone(), analog.clone())) {
                ensure!(
                    roles.len() == 1 && roles.contains_key(&DigitalPinDirection::Input),
                    "producer-split ALB node {analog} requires separate ADC and identified DAC boundaries"
                );
                ensure!(
                    scope
                        .alb_roles
                        .get(&analog)
                        .is_some_and(|used| used.contains(&DigitalPinDirection::Input)),
                    "producer-split ALB node {analog} has no ALB sensing input"
                );
                let input = roles
                    .get(&DigitalPinDirection::Input)
                    .context("missing producer-split ADC state")?;
                planned.states.insert(analog, input.clone());
            } else {
                let (primary, secondary) =
                    plan_original_node_roles(graph, scope, &path, &analog, &roles)?;
                planned.states.insert(analog.clone(), primary);
                if let Some(output) = secondary {
                    planned.output_states.insert(analog, output);
                }
            }
        }
        result.insert(path, planned);
    }
    plan_selected_producers(graph, producers, &mut result)?;
    Ok(result)
}

/// Rewrite logical ALB A devices to physical three-node X instances, and
/// propagate digital metadata across every connected subcircuit interface.
///
/// # Errors
/// Conflicting or unsupported digital node roles, models or source topology.
pub fn expand_logical_alb_source(
    source: &str,
    boundaries: &[LogicalAlbBoundary],
    compiled_file: &str,
) -> Result<LogicalAlbExpansion> {
    ensure!(
        !compiled_file.is_empty()
            && compiled_file
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-')),
        "invalid emitted compiled model include name"
    );
    let items = parse_items(source)?;
    let terminators = items
        .iter()
        .filter(|item| {
            if let Item::Statement(stmt) = item {
                normalized_statement(stmt)
                    .split_ascii_whitespace()
                    .next()
                    .is_some_and(|first| first.eq_ignore_ascii_case(".end"))
            } else {
                false
            }
        })
        .count();
    ensure!(
        terminators == 1,
        "original logical deck requires exactly one root .end"
    );
    let models = compile_explicit_alb_source_models(source)?;
    ensure!(
        !models.is_empty(),
        "source has no compilable D_LOGIC_BLOCK models"
    );
    let mut graph = Graph {
        models,
        scopes: BTreeMap::new(),
    };
    graph.collect(&items, &[], None)?;
    graph.mark_exported_cuts(boundaries)?;
    graph.discover_roles()?;
    graph.discover_alb_roles()?;
    let scoped_renames = plan_scope_renames(&graph, boundaries)?;
    let maps = graph
        .scopes
        .iter()
        .map(|(path, info)| {
            let empty = ScopeRenames::default();
            let renames = scoped_renames.get(path).unwrap_or(&empty);
            Ok((path.clone(), build_bundles(info, renames)?))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let mut output = String::new();
    graph.render(
        &items,
        &[],
        &maps,
        &scoped_renames,
        compiled_file,
        &mut output,
    )?;
    let mut library = String::new();
    for m in &graph.models {
        library.push_str(&m.source);
        library.push('\n');
    }
    Ok(LogicalAlbExpansion {
        deck: output,
        library,
    })
}
