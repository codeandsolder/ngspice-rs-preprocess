//! Explicit original logical ALB port expansion into native three-node
//! physical bundles. No production translation path uses this module.

mod code_model_ifspec;
mod discovery;
mod global_nodes;
mod occurrence;
mod opaque_library;
mod receiver_groups;
use super::{
    CompiledSourceAlbModel, Item, Result, bail, compile_explicit_alb_source_models,
    normalized_statement, parse_items, statement_tokens,
};
use crate::alb::DigitalPinDirection;
use anyhow::{Context, ensure};
pub use code_model_ifspec::{CodeModelContract, LookupTableShape, verify_external_code_models};
pub use discovery::{
    DiscoveredAlbBoundary, OpaqueXInterface, OpaqueXPinKind, discover_original_alb_boundaries,
    discover_original_alb_boundaries_with_opaque_x_interfaces,
};
pub use occurrence::{
    DefinitionFamilyBoundary, FamilySpecialization, OccurrenceFamilyBoundary,
    specialize_family_occurrences,
};
pub use opaque_library::verify_external_native_libraries;
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
    /// Exact input occurrences belonging to a heterogeneous physical receiver group.
    pub receivers: Vec<AlbInputReceiver>,
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

/// Exact original ALB input occurrence relative to the boundary scope.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct AlbInputReceiver {
    pub path: Vec<String>,
    pub input_pin: usize,
    /// X-hop indices and scalar analog formals traversed by this ADC route.
    /// The analog wire remains a scalar; a separate event bundle is promoted.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub analog_hops: BTreeMap<usize, String>,
}

impl AlbInputReceiver {
    fn prepend(&mut self, instance: String, analog_formal: Option<String>) {
        self.path.insert(0, instance);
        self.analog_hops = std::mem::take(&mut self.analog_hops)
            .into_iter()
            .map(|(index, formal)| (index + 1, formal))
            .collect();
        if let Some(formal) = analog_formal {
            self.analog_hops.insert(0, formal);
        }
    }
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
    /// Original implicit globals made explicit at the end of the input ABI.
    global_formals: BTreeMap<String, String>,
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
    /// Scalar interfaces whose receiver bundles originate in a caller.
    lifted_adc_formals: BTreeSet<String>,
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
    model: usize,
    /// Immediate source instance, its output formal (or direct ALB net),
    /// and direct ALB output bit. The first divergent interface is split.
    slot: (String, String, usize),
}

/// Resolved pin reached through original lexical X interfaces. Electrical
/// identity and immediate routing slot come from the same source traversal.
struct ResolvedAlbPin {
    path: Vec<String>,
    bit: usize,
    model: usize,
    slot: (String, String, usize),
    analog_hops: BTreeMap<usize, String>,
}

#[derive(Default)]
struct Graph {
    scopes: BTreeMap<Vec<String>, Scope>,
    models: Vec<CompiledSourceAlbModel>,
    globals: BTreeSet<String>,
}

fn key(text: &str) -> String {
    text.to_ascii_uppercase()
}

fn valid_analog_node(text: &str) -> Result<String> {
    crate::validate_node(text)?;
    Ok(key(text))
}

fn valid_node(text: &str) -> Result<String> {
    ensure!(text != "0", "digital ALB node cannot be analog ground 0");
    valid_analog_node(text)
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
                        global_formals: BTreeMap::new(),
                        suffix,
                        items: items.to_vec(),
                        digital: BTreeSet::new(),
                        alb_roles: BTreeMap::new(),
                        analog_cut_formals: BTreeSet::new(),
                        lifted_adc_formals: BTreeSet::new(),
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

    fn lookup_model(
        &self,
        scope: &[String],
        name: &str,
        device: Option<&[f64]>,
    ) -> Result<Option<usize>> {
        let mut context = scope.to_vec();
        loop {
            if self
                .models
                .iter()
                .any(|m| m.scope == context && m.model_name.eq_ignore_ascii_case(name))
            {
                let selected = self.models.iter().position(|m| {
                    m.scope == context
                        && m.model_name.eq_ignore_ascii_case(name)
                        && super::same_device_values(m.device_values.as_deref(), device)
                });
                return selected
                    .with_context(|| {
                        format!("ALB {name} in scope {context:?} has no compiled DEVICE={device:?} specialization")
                    })
                    .map(Some);
            }
            if context.pop().is_none() {
                return Ok(None);
            }
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
            if idx != model.ports.len() - model.global_formals.len() + 1 {
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
            let owner = self
                .scopes
                .get(scope)
                .context("missing original call scope")?;
            let actual = fields[1..idx]
                .iter()
                .chain(model.global_formals.keys())
                .map(|node| owner.mapped_node(node))
                .collect();
            found.push(Call {
                target,
                actual,
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
        let Some((model_name, device_values)) = super::original_alb_instance_signature(statement)?
        else {
            return Ok(None);
        };
        let Some(target) = self.lookup_model(scope, &model_name, device_values.as_deref())? else {
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
                    .map(valid_analog_node)
                    .collect::<Result<Vec<_>>>()?;
                groups.push(content);
                remaining = &stripped[close + 1..];
            }
            ensure!(
                groups.len() == 2,
                "ALB {inst} must have input and output port vectors"
            );
            let tail = remaining
                .trim()
                .split_once(':')
                .map_or_else(|| remaining.trim(), |(model, _)| model.trim());
            ensure!(
                tail.eq_ignore_ascii_case(&model_name)
                    || tail.to_ascii_uppercase() == format!("MODEL={model_name}"),
                "unsupported ALB {inst} instance model reference {tail}"
            );
            groups
        } else {
            let node_and_model = raw.split_once(':').map_or(raw, |(head, _)| head.trim());
            let tokens = node_and_model.split_ascii_whitespace().collect::<Vec<_>>();
            ensure!(
                tokens.len() == model.kernel.input_width + model.kernel.output_width + 1,
                "ALB {inst} logical port count does not match model"
            );
            vec![
                tokens[..model.kernel.input_width]
                    .iter()
                    .map(|s| valid_analog_node(s))
                    .collect::<Result<Vec<_>>>()?,
                tokens[model.kernel.input_width..tokens.len() - 1]
                    .iter()
                    .map(|s| valid_analog_node(s))
                    .collect::<Result<Vec<_>>>()?,
            ]
        };
        ensure!(
            ports[0].len() == model.kernel.input_width
                && ports[1].len() == model.kernel.output_width,
            "ALB {inst} pin arity differs from compiled model {}",
            model.model_name
        );
        let owner = self.scopes.get(scope).context("missing ALB source scope")?;
        Ok(Some(LogicalInstance {
            name: inst.clone(),
            inputs: ports[0]
                .iter()
                .map(|node| owner.mapped_node(node))
                .collect(),
            outputs: ports[1]
                .iter()
                .map(|node| owner.mapped_node(node))
                .collect(),
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
            let analog = valid_analog_node(&boundary.analog_net)?;
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

    /// A known physical terminal without any ALB pin role must not be
    /// rewritten as a digital triple simply because its caller also owns
    /// a compiled ALB on the same physical wire. ALB-bearing interfaces
    /// still require an explicit ADC/DAC boundary annotation.
    fn mark_passive_scalar_ports(
        &mut self,
        contacts: &BTreeMap<Vec<String>, BTreeSet<String>>,
    ) -> Result<()> {
        for (path, owner) in &mut self.scopes {
            let witnessed = contacts.get(path).context("missing physical contacts")?;
            owner.analog_cut_formals.extend(
                owner
                    .ports
                    .iter()
                    .filter(|formal| {
                        witnessed.contains(*formal) && !owner.alb_roles.contains_key(*formal)
                    })
                    .cloned(),
            );
        }
        Ok(())
    }

    /// A scalar analog formal also makes a forwarding parent formal
    /// scalar, even when that intermediate definition has no ALB of its own.
    /// Never infer this port property from a caller's net name.
    fn propagate_scalar_ports(&mut self) -> Result<()> {
        loop {
            let mut scalar_parents = BTreeSet::new();
            for (path, owner) in &self.scopes {
                for item in &owner.items {
                    if let Item::Statement(statement) = item
                        && let Some(call) = self.call(statement, path)?
                    {
                        let child = self
                            .scopes
                            .get(&call.target)
                            .context("missing scalar child")?;
                        for (formal, actual) in child.ports.iter().zip(&call.actual) {
                            if child.analog_cut_formals.contains(formal)
                                && owner.ports.contains(actual)
                            {
                                scalar_parents.insert((path.clone(), actual.clone()));
                            }
                        }
                    }
                }
            }
            let mut changed = false;
            for (path, formal) in scalar_parents {
                changed |= self
                    .scopes
                    .get_mut(&path)
                    .context("missing scalar parent")?
                    .analog_cut_formals
                    .insert(formal);
            }
            if !changed {
                return Ok(());
            }
        }
    }

    /// External native X implementations opt into the same three-node
    /// digital event ABI as compiled ALBs. Their original logical pins
    /// are digital even when no local ALB happens to touch that net.
    fn mark_native_external_digital(
        &mut self,
        external: &BTreeMap<String, &OpaqueXInterface>,
    ) -> Result<()> {
        let scopes = self.scopes.keys().cloned().collect::<Vec<_>>();
        for scope in scopes {
            let items = self
                .scopes
                .get(&scope)
                .context("missing scope")?
                .items
                .clone();
            for item in items {
                let Item::Statement(statement) = item else {
                    continue;
                };
                if self.call(&statement, &scope)?.is_some() {
                    continue;
                }
                let pins = discovery::external_x_pins(&statement, external)?;
                if pins.is_empty() {
                    continue;
                }
                let tokens = statement_tokens(&normalized_statement(&statement))?;
                let native = tokens
                    .iter()
                    .filter_map(|name| external.get(&key(name)))
                    .any(|entry| entry.native_model.is_some());
                if native {
                    let owner = self
                        .scopes
                        .get_mut(&scope)
                        .context("missing mutable scope")?;
                    owner.digital.extend(
                        pins.into_iter()
                            .filter(|(_, kind)| *kind == OpaqueXPinKind::Digital)
                            .map(|(name, _)| name),
                    );
                }
            }
        }
        Ok(())
    }

    /// A verified native digital event node cannot simultaneously be a raw
    /// SPICE analog voltage/current contact unless an independently proven
    /// ADC/DAC cut actually replaces that original logical wire.
    fn validate_native_external_contacts(
        &self,
        external: &BTreeMap<String, &OpaqueXInterface>,
        renames: &BTreeMap<Vec<String>, ScopeRenames>,
        contacts: &BTreeMap<Vec<String>, BTreeSet<String>>,
    ) -> Result<()> {
        for (path, scope) in &self.scopes {
            let analog = contacts
                .get(path)
                .context("missing native X physical contacts")?;
            let empty = ScopeRenames::default();
            let cut = renames.get(path).unwrap_or(&empty);
            for item in &scope.items {
                let Item::Statement(statement) = item else {
                    continue;
                };
                if self.call(statement, path)?.is_some() {
                    continue;
                }
                let pins = discovery::external_x_pins(statement, external)?;
                if pins.is_empty() {
                    continue;
                }
                let tokens = statement_tokens(&normalized_statement(statement))?;
                let native = tokens
                    .iter()
                    .filter_map(|name| external.get(&key(name)))
                    .any(|model| model.native_model.is_some());
                if !native {
                    continue;
                }
                for (node, kind) in pins {
                    ensure!(
                        kind != OpaqueXPinKind::Digital
                            || (!cut.receivers.cuts.contains(&node)
                                && !cut.receivers.formals.contains_key(&node)),
                        "external native digital X pin {node} in {path:?} has ambiguous heterogeneous receiver state"
                    );
                    ensure!(
                        kind != OpaqueXPinKind::Digital
                            || !analog.contains(&node)
                            || cut.contains_key(&node)
                            || cut.producer_analog_nodes.contains(&node),
                        "external native digital X pin {node} in {path:?} has an unsplit physical analog contact; requires an explicit ADC/DAC cut"
                    );
                }
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
    /// Resolve physical ALB pin occurrences, preserving actual X instance
    /// identity and model binding rather than merely a reusable definition.
    fn pin_paths(
        &self,
        scope: &[String],
        node: &str,
        direction: DigitalPinDirection,
        stack: &mut Vec<Vec<String>>,
    ) -> Result<Vec<ResolvedAlbPin>> {
        ensure!(stack.len() < 256, "ALB X hierarchy exceeds 256 levels");
        ensure!(
            !stack.iter().any(|active| active == scope),
            "recursive ALB pin hierarchy at {scope:?}"
        );
        stack.push(scope.to_vec());
        let info = self.scopes.get(scope).context("missing ALB pin scope")?;
        let mut result = Vec::new();
        for item in &info.items {
            let Item::Statement(statement) = item else {
                continue;
            };
            if let Some(alb) = self.alb_instance(statement, scope)? {
                let pins = match direction {
                    DigitalPinDirection::Input => &alb.inputs,
                    DigitalPinDirection::Output => &alb.outputs,
                };
                for (bit, pin) in pins.iter().enumerate() {
                    if pin == node {
                        result.push(ResolvedAlbPin {
                            path: vec![key(&alb.name)],
                            bit,
                            model: alb.target,
                            slot: (key(&alb.name), node.to_owned(), bit),
                            analog_hops: BTreeMap::new(),
                        });
                    }
                }
            } else if let Some(call) = self.call(statement, scope)? {
                let child = self
                    .scopes
                    .get(&call.target)
                    .context("missing ALB pin child")?;
                for (formal, actual) in child.ports.iter().zip(&call.actual) {
                    let lifted = direction == DigitalPinDirection::Input
                        && child.lifted_adc_formals.contains(formal);
                    if actual != node || (child.analog_cut_formals.contains(formal) && !lifted) {
                        continue;
                    }
                    for mut resolved in self.pin_paths(&call.target, formal, direction, stack)? {
                        resolved.path.insert(0, key(&call.name));
                        resolved.analog_hops = resolved
                            .analog_hops
                            .into_iter()
                            .map(|(index, formal)| (index + 1, formal))
                            .collect();
                        if lifted {
                            resolved.analog_hops.insert(0, formal.clone());
                        }
                        resolved.slot = (key(&call.name), formal.clone(), 0);
                        result.push(resolved);
                    }
                }
            }
        }
        stack.pop();
        Ok(result)
    }

    fn producer_paths(&self, scope: &[String], node: &str) -> Result<Vec<ResolvedAlbProducer>> {
        Ok(self
            .pin_paths(scope, node, DigitalPinDirection::Output, &mut Vec::new())?
            .into_iter()
            .map(|pin| ResolvedAlbProducer {
                identity: AlbOutputProducer {
                    path: pin.path,
                    output_pin: pin.bit,
                },
                slot: pin.slot,
                model: pin.model,
            })
            .collect())
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
    /// Original analog nodes with every independent output producer bound
    /// to a separate physical DAC; these do not have one digital driver.
    producer_analog_nodes: BTreeSet<String>,
    /// Additional exported physical output bundles per original X formal.
    /// Every lexical call site must supply these in the same stable order.
    promoted_formals: BTreeMap<String, Vec<PromotedOutput>>,
    receivers: receiver_groups::ReceiverPlan,
}
impl ScopeRenames {
    fn contains_key(&self, original: &str) -> bool {
        self.states.contains_key(original) || self.receivers.cuts.contains(original)
    }
}

#[derive(Clone)]
struct ScopeBundles {
    ports: BTreeMap<String, [String; 3]>,
    alb_outputs: BTreeMap<String, [String; 3]>,
    producers: BTreeMap<ProducerSlot, [String; 3]>,
    promoted_formals: BTreeMap<String, Vec<PromotedOutput>>,
    receivers: receiver_groups::ReceiverBundles,
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
    fn receiver(&self, name: &str, node: &str, input_bit: usize) -> Option<Vec<String>> {
        self.receivers
            .routes
            .get(&(key(name), key(node), input_bit))
            .map(|b| b.to_vec())
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
fn needs_primary_bundle(scope: &Scope, renames: &ScopeRenames, node: &str) -> bool {
    if renames.states.contains_key(node) {
        return true;
    }
    let routed_input = renames.receivers.cuts.contains(node);
    let routed_output = renames.producer_analog_nodes.contains(node);
    if !routed_input && !routed_output {
        return true;
    }
    scope.alb_roles.get(node).is_some_and(|roles| {
        roles.iter().any(|direction| match direction {
            DigitalPinDirection::Input => !routed_input,
            DigitalPinDirection::Output => !routed_output,
        })
    })
}

fn build_bundles(scope: &Scope, renames: &ScopeRenames) -> Result<ScopeBundles> {
    let mut reserved = reserved(scope);
    // Protect every chosen state name *before* allocating any metadata.
    // Otherwise a generated _SR_SINK name may collide with a later
    // separately selected input/output state.
    for original in &scope.digital {
        if !needs_primary_bundle(scope, renames, original) {
            continue;
        }
        let state = renames.states.get(original).unwrap_or(original);
        reserved.insert(state.clone());
        if let Some(output) = renames.output_states.get(original) {
            reserved.insert(output.clone());
        }
    }
    for state in renames.producer_states.values() {
        reserved.insert(state.clone());
    }
    reserved.extend(renames.receivers.states.iter().cloned());
    let mut used_states = BTreeSet::new();
    let mut physical = BTreeMap::new();
    let mut alb_outputs = BTreeMap::new();
    let mut ordinal = 0usize;
    for original in &scope.digital {
        if !needs_primary_bundle(scope, renames, original) {
            continue;
        }
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
    let receivers =
        renames
            .receivers
            .build_bundles(&physical, &mut reserved, &mut used_states, ordinal)?;
    Ok(ScopeBundles {
        ports: physical,
        alb_outputs,
        producers,
        promoted_formals: renames.promoted_formals.clone(),
        receivers,
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
            let parts = if let Some(groups) = nodes.receivers.formals.get(formal) {
                groups
                    .first()
                    .context("empty promoted input groups")?
                    .to_vec()
            } else {
                nodes.rendered(formal)?
            };
            for part in parts {
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
    for (formal, groups) in &nodes.receivers.formals {
        let skip = usize::from(!info.lifted_adc_formals.contains(formal));
        for parts in groups.iter().skip(skip) {
            for part in parts {
                write!(output, " {part}")?;
            }
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
            if !renames.contains_key(&name) && !renames.producer_analog_nodes.contains(&name) {
                bail!(
                    "untranslated device {inst} connects digital net {name}; physical interface unknown"
                );
            }
        }
    }
    Ok(())
}
struct RenderEnvironment<'a> {
    maps: &'a BTreeMap<Vec<String>, ScopeBundles>,
    scoped_renames: &'a BTreeMap<Vec<String>, ScopeRenames>,
    compiled_file: &'a str,
    external: &'a BTreeMap<String, &'a OpaqueXInterface>,
}

struct RenderContext<'a> {
    scope: &'a [String],
    maps: &'a BTreeMap<Vec<String>, ScopeBundles>,
    renames: &'a ScopeRenames,
    compiled_file: &'a str,
    external: &'a BTreeMap<String, &'a OpaqueXInterface>,
}

/// Rewrite only externally verified native X replacements, preserving their
/// analog terminals and expanding digital terminals into event triples.
/// Opaque models without a replacement retain their original SPICE X call.
fn render_external_x(
    statement: &str,
    ctx: &RenderContext<'_>,
    info: &Scope,
    physical: &ScopeBundles,
    output: &mut String,
) -> Result<bool> {
    let fields = statement_tokens(&normalized_statement(statement))?;
    let Some(first) = fields.first() else {
        return Ok(false);
    };
    if !first.starts_with(['X', 'x']) || first.len() < 2 {
        return Ok(false);
    }
    let pins = discovery::external_x_pins(statement, ctx.external)?;
    if pins.is_empty() {
        ensure!(
            !fields
                .iter()
                .skip(1)
                .any(|node| info.digital.contains(&key(node))),
            "unresolved external X instance {first} touches digital state"
        );
        return Ok(false);
    }
    let model = fields
        .iter()
        .find_map(|name| ctx.external.get(&key(name)))
        .context("verified external model missing")?;
    if let Some(native) = &model.native_model {
        write!(output, "{first}")?;
        for (node, kind) in &pins {
            match kind {
                OpaqueXPinKind::Digital => {
                    for part in physical.rendered(node)? {
                        write!(output, " {part}")?;
                    }
                }
                OpaqueXPinKind::Analog => {
                    ensure!(
                        !info.digital.contains(node)
                            || ctx.renames.contains_key(node)
                            || ctx.renames.producer_analog_nodes.contains(node),
                        "external X {first} analog pin {node} touches unsplit ALB state"
                    );
                    write!(output, " {node}")?;
                }
            }
        }
        write!(output, " {native}")?;
        for suffix in fields.iter().skip(pins.len() + 2) {
            write!(output, " {suffix}")?;
        }
    } else {
        for (node, kind) in &pins {
            if info.digital.contains(node) {
                ensure!(
                    *kind == OpaqueXPinKind::Analog,
                    "external X {first} digital pin {node} touches ALB state without a compatible native digital bundle"
                );
                ensure!(
                    ctx.renames.contains_key(node)
                        || ctx.renames.producer_analog_nodes.contains(node),
                    "external X {first} analog pin {node} touches unsplit ALB digital state"
                );
            }
        }
        output.push_str(statement);
    }
    output.push('\n');
    Ok(true)
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
    if let Some(bundle) = physical.receiver(&call.name, formal, 0) {
        return Ok(bundle);
    }
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

fn render_promoted_call_inputs(
    call: &Call,
    child: &Scope,
    child_bundles: &ScopeBundles,
    physical: &ScopeBundles,
    output: &mut String,
) -> Result<()> {
    for (formal, groups) in &child_bundles.receivers.formals {
        let index = child
            .ports
            .iter()
            .position(|port| port == formal)
            .context("missing promoted receiver formal")?;
        let actual = &call.actual[index];
        let lifted = child.lifted_adc_formals.contains(formal);
        let start = usize::from(!lifted);
        for group in start..groups.len() {
            let parts = if lifted {
                physical
                    .receiver(&call.name, formal, group)
                    .context("missing caller binding for lifted ADC receiver")?
            } else {
                physical
                    .receiver(&call.name, formal, group)
                    .or_else(|| physical.receiver(&call.name, formal, 0))
                    .map_or_else(|| physical.rendered(actual), Ok)?
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
                            .is_some_and(|digital| digital != original)
                        || ctx.renames.receivers.cuts.contains(original)
                        || ctx.renames.producer_analog_nodes.contains(original),
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
        render_promoted_call_inputs(call, child, child_bundles, physical, output)?;
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
            for (bit, state) in alb.inputs.iter().enumerate() {
                let parts = physical
                    .receiver(&alb.name, state, bit)
                    .map_or_else(|| physical.rendered(state), Ok)?;
                for part in parts {
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
            writeln!(output, " {}", self.models[alb.target].kernel.name)?;
            return Ok(());
        }
        if let Some(call) = self.call(statement, scope)? {
            self.render_call(&call, ctx, output)?;
            return Ok(());
        }
        if render_external_x(statement, ctx, info, physical, output)? {
            return Ok(());
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
        env: &RenderEnvironment<'_>,
        output: &mut String,
    ) -> Result<()> {
        let empty = ScopeRenames::default();
        let renames = env.scoped_renames.get(scope).unwrap_or(&empty);
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
                    let child_nodes = env.maps.get(&path).context("missing child node mapping")?;
                    writeln!(
                        output,
                        "{}",
                        expanded_header(&path, child_info, child_nodes)?
                    )?;
                    self.render(&child.body, &path, env, output)?;
                    writeln!(output, ".ends {}", key(&child.name))?;
                }
                Item::Statement(statement) => {
                    self.render_statement(
                        statement,
                        &RenderContext {
                            scope,
                            maps: env.maps,
                            renames,
                            compiled_file: env.compiled_file,
                            external: env.external,
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
    lifted_input: bool,
) -> Result<(String, Option<String>)> {
    let mut used = scope.alb_roles.get(analog).cloned().unwrap_or_default();
    if lifted_input {
        used.remove(&DigitalPinDirection::Input);
    }
    if used.contains(&DigitalPinDirection::Output) {
        ensure!(
            graph.producer_paths(path, analog)?.len() == 1,
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
    let child_outputs = graph.producer_paths(&child.target, formal)?;
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

fn new_promoted_name(
    info: &Scope,
    plan: &ScopeRenames,
    prefix: &str,
    ordinal: usize,
) -> Result<String> {
    let mut occupied = reserved(info);
    occupied.extend(info.digital.iter().cloned());
    occupied.extend(plan.states.values().cloned());
    occupied.extend(plan.output_states.values().cloned());
    occupied.extend(plan.producer_states.values().cloned());
    occupied.extend(plan.receivers.states.iter().cloned());
    for suffix in 0..u32::MAX {
        let name = if suffix == 0 {
            format!("{prefix}_{ordinal}")
        } else {
            format!("{prefix}_{ordinal}_{suffix}")
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
    let found = graph.producer_paths(scope, formal)?;
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
        let state = new_promoted_name(info, plan, "SR_PROMOTED_OUT", plan.producer_states.len())?;
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
        let discovered = graph.producer_paths(&scope, &analog)?;
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
                    .is_some_and(|plan| plan.contains_key(&analog))
                    || owner.lifted_adc_formals.contains(&analog),
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
        plan.producer_analog_nodes.insert(analog.clone());
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

fn validate_boundary_formals(
    scope: &Scope,
    path: &[String],
    analog: &str,
    digital: &str,
) -> Result<()> {
    if !path.is_empty() {
        ensure!(
            !scope.ports.iter().any(|formal| formal == digital),
            "mixed boundary {path:?} cannot use an exported digital formal {digital}"
        );
        if scope.ports.iter().any(|formal| formal == analog) {
            ensure!(
                scope.analog_cut_formals.contains(analog),
                "exported analog formal {analog} in {path:?} lacks a verified interface cut"
            );
        }
    }
    Ok(())
}

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
    let mut receiver_cuts = BTreeSet::new();
    let lifted_nodes = receiver_groups::lifted_nodes(boundaries);
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
        let analog = valid_analog_node(&boundary.analog_net)?;
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
        validate_boundary_formals(scope, &path, &analog, &digital)?;
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
        if boundary.direction == DigitalPinDirection::Input
            && lifted_nodes.contains(&(path.clone(), analog.clone()))
        {
            ensure!(
                !boundary.receivers.is_empty(),
                "lifted ADC needs explicit receiver identities in {path:?}/{analog}"
            );
            continue;
        }
        if !boundary.receivers.is_empty() {
            ensure!(
                boundary.direction == DigitalPinDirection::Input,
                "receiver group must be an input"
            );
            if !receiver_cuts.insert((path.clone(), analog.clone())) {
                continue;
            }
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
    let lifted_nodes = receiver_groups::lifted_nodes(boundaries);
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
                let (primary, secondary) = plan_original_node_roles(
                    graph,
                    scope,
                    &path,
                    &analog,
                    &roles,
                    lifted_nodes.contains(&(path.clone(), analog.clone()))
                        || scope.lifted_adc_formals.contains(&analog),
                )?;
                planned.states.insert(analog.clone(), primary);
                if let Some(output) = secondary {
                    planned.output_states.insert(analog, output);
                }
            }
        }
        result.insert(path, planned);
    }
    // Allocate requested producer names first so promoted receiver states
    // cannot collide, and tell the producer validator which inputs are owned.
    for (path, analog) in lifted_nodes {
        result
            .entry(path)
            .or_default()
            .receivers
            .cuts
            .insert(analog);
    }
    plan_selected_producers(graph, producers, &mut result)?;
    receiver_groups::plan(graph, boundaries, &mut result)?;
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
    expand_logical_alb_source_with_opaque_x_interfaces(source, boundaries, compiled_file, &[])
}

/// Expand ALB nodes alongside explicitly verified external X-pin interfaces.
///
/// Neither model names nor node names imply analog ownership. An external X
/// digital port touching native ALB state is rejected unless a compatible
/// digital bundle can be provided by a future independent implementation.
///
/// # Errors
/// Invalid signatures, incompatible original X terminals, or any source
/// translation error.
pub fn expand_logical_alb_source_with_opaque_x_interfaces(
    source: &str,
    boundaries: &[LogicalAlbBoundary],
    compiled_file: &str,
    external_interfaces: &[OpaqueXInterface],
) -> Result<LogicalAlbExpansion> {
    let external = discovery::opaque_x_interfaces(external_interfaces)?;
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
        globals: BTreeSet::new(),
    };
    graph.collect(&items, &[], None)?;
    let scopes = discovery::dependency_order(&graph)?;
    global_nodes::prepare(&mut graph, &scopes)?;
    graph.mark_native_external_digital(&external)?;
    graph.mark_exported_cuts(boundaries)?;
    receiver_groups::mark_lifted_formals(&mut graph, boundaries)?;
    graph.propagate_scalar_ports()?;
    graph.discover_alb_roles()?;
    let contacts = discovery::witnessed_analog_contacts(&graph, &external, &scopes)?;
    graph.mark_passive_scalar_ports(&contacts)?;
    graph.discover_roles()?;
    let scoped_renames = plan_scope_renames(&graph, boundaries)?;
    graph.validate_native_external_contacts(&external, &scoped_renames, &contacts)?;
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
        &RenderEnvironment {
            maps: &maps,
            scoped_renames: &scoped_renames,
            compiled_file,
            external: &external,
        },
        &mut output,
    )?;
    let mut library = String::new();
    let mut emitted = BTreeSet::new();
    for model in &graph.models {
        let kernel = &model.kernel;
        if emitted.insert(&kernel.name) {
            writeln!(
                library,
                ".subckt {}{}.ends {}\n",
                kernel.name, kernel.contents, kernel.name
            )?;
        }
    }
    Ok(LogicalAlbExpansion {
        deck: output,
        library,
    })
}
