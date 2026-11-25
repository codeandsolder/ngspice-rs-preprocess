//! Bind reachable native subcircuit occurrences before numerical preparation.
//!
//! Declaration visibility is lexical; numeric parameter inheritance follows
//! actual callers. Nodes are structured identities, never concatenated names.

use super::{
    ModelLibrary, ScopeResolver, SourceCard, SourceOrigin, SpiceExpression, SubcircuitIndex,
    assignments, lexical_lookup,
};
use crate::{
    model_card::{ModelParameter, instance_fields, positional_prefix},
    vdmos_card::{VdmosLiterals, VdmosPolarity, vdmos_resolved},
};
use anyhow::{Context, Result, bail, ensure};
use std::collections::{BTreeMap, BTreeSet};

/// Resource limits for recursive occurrence expansion.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub occurrences: usize,
    pub depth: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            occurrences: 1_000_000,
            depth: 128,
        }
    }
}

/// Original source position, including file provenance when linked.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Location<'a> {
    pub linked_line: usize,
    pub origin: Option<&'a SourceOrigin>,
}

/// Disjoint node identities, including implicit VDMOS thermal terminals.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum Node {
    /// Top-level or explicitly global node (including ground).
    Global(String),
    /// Local node owned by an actual subcircuit occurrence.
    Local { frame: usize, name: String },
    /// Thermal terminal allocated for a device without an explicit node.
    DeviceThermal {
        device: usize,
        terminal: ThermalTerminal,
    },
}

/// Thermal terminal identity in descriptor order.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum ThermalTerminal {
    Junction,
    Case,
}

/// Actual occurrence of a lexical subcircuit declaration; root is index zero.
#[derive(Debug)]
pub struct Frame<'a> {
    pub parent: Option<usize>,
    pub declaration: usize,
    pub name: String,
    pub location: Location<'a>,
    pub definition_location: Location<'a>,
    pub ports: BTreeMap<String, Node>,
    /// Call-site values, evaluated in the calling frame before binding.
    pub overrides: BTreeMap<String, f64>,
}

/// Evaluated native VDMOS instance inputs. Temperature values retain their
/// source distinction so a later ambient sweep can rebind the same model.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VdmosInstance {
    pub multiplier: f64,
    pub temp_celsius: Option<f64>,
    pub dtemp_kelvin: Option<f64>,
    pub thermal_requested: bool,
    pub off: bool,
    /// Initial drain-source and gate-source voltages, respectively.
    pub initial: [Option<f64>; 2],
}

impl Default for VdmosInstance {
    fn default() -> Self {
        Self {
            multiplier: 1.,
            temp_celsius: None,
            dtemp_kelvin: None,
            thermal_requested: false,
            off: false,
            initial: [None; 2],
        }
    }
}

impl VdmosInstance {
    /// Compute the effective instance temperature in kelvin. Native VDMOS
    /// setup prefers DTEMP when both TEMP and DTEMP are given.
    /// # Errors
    /// Invalid absolute circuit or instance temperature.
    pub fn ambient_kelvin(self, circuit_kelvin: f64) -> Result<f64> {
        ensure!(
            circuit_kelvin.is_finite() && circuit_kelvin > 0.,
            "invalid circuit temperature"
        );
        let value = self.dtemp_kelvin.map_or_else(
            || self.temp_celsius.map_or(circuit_kelvin, |c| c + 273.15),
            |delta| circuit_kelvin + delta,
        );
        ensure!(
            value.is_finite() && value > 0.,
            "invalid VDMOS instance temperature"
        );
        Ok(value)
    }
}

/// Source-side preparation of a VDMOS occurrence. No circuit equation IDs
/// or mutable numerical history are assigned here.
#[derive(Debug)]
pub struct VdmosBinding {
    /// Index in `Topology::models`.
    pub model: usize,
    pub instance: VdmosInstance,
    /// Actual descriptor terminals in D, G, S[, Tj, Tc] order.
    pub terminals: Vec<Node>,
    /// Effective self-heating, requiring both the instance flag and RTHJC.
    pub self_heating: bool,
}

impl VdmosBinding {
    /// Local identity of the immutable model and structural configuration.
    /// Numeric multiplicity and temperature are prepared separately.
    #[must_use = "the key selects the shared immutable model artifact"]
    pub const fn artifact_key(&self) -> (usize, bool) {
        (self.model, self.self_heating)
    }
}

/// One model-bearing MOS occurrence.
///
/// Other MOS families retain their source
/// fields explicitly and remain unprepared rather than being mistaken for
/// VDMOS. Their constitutive compilation belongs to their own producer.
#[derive(Debug)]
pub struct Mos<'a> {
    pub frame: usize,
    pub name: String,
    pub location: Location<'a>,
    pub model_declaration: usize,
    /// Actual owner of the lexically visible model declaration.
    pub model_frame: usize,
    pub nodes: Vec<Node>,
    pub source_parameters: Vec<ModelParameter>,
    pub vdmos: Option<VdmosBinding>,
}

/// One interned evaluated model declaration. Identical values of the same
/// declaration share an index even across different calling frames.
#[derive(Debug)]
pub struct ModelVariant<'a> {
    pub declaration: usize,
    pub location: Location<'a>,
    pub values: VdmosLiterals<'a>,
}

/// Bound source-side subcircuit and model-bearing MOS topology.
#[derive(Debug)]
pub struct Topology<'a> {
    pub frames: Vec<Frame<'a>>,
    pub mos: Vec<Mos<'a>>,
    pub models: Vec<ModelVariant<'a>>,
}

impl ModelLibrary {
    /// Instantiate actual X-lines and bind all reachable MOS model references.
    /// Repeated declarations are indexed once and equal evaluated VDMOS
    /// models are interned, independent of numeric instance parameters.
    /// The caller still owns other device grammars and solver row binding.
    /// # Errors
    /// Missing/recursive subcircuits or model references, bad arity, invalid
    /// expressions or instance parameters, or exceeded expansion limits.
    pub fn instantiate(&self, limits: Limits) -> Result<Topology<'_>> {
        Builder::new(self, limits)?.build()
    }

    fn location(&self, line: usize) -> Location<'_> {
        Location {
            linked_line: line,
            origin: line
                .checked_sub(1)
                .and_then(|index| self.origins.get(index)),
        }
    }
}

struct Index {
    subcircuits: SubcircuitIndex,
    models: Vec<BTreeMap<String, usize>>,
    implicit_m: BTreeSet<usize>,
}

struct Call<'a> {
    name: &'a str,
    nodes: Vec<&'a str>,
    target: &'a str,
    args: BTreeMap<String, SpiceExpression>,
}

fn call(card: &SourceCard) -> Result<Call<'_>> {
    let (prefix, fields) = positional_prefix(&card.text);
    let (name, remaining) = prefix.split_first().context("empty X-line")?;
    let (target, nodes) = remaining
        .split_last()
        .context("X-line has no subcircuit name")?;
    Ok(Call {
        name: name.0,
        nodes: nodes.iter().map(|&(node, _)| node).collect(),
        target: target.0,
        args: assignments(fields, card.line)?,
    })
}

impl Index {
    fn new(source: &ModelLibrary) -> Result<Self> {
        let mut result = Self {
            subcircuits: SubcircuitIndex::new(source)?,
            models: vec![BTreeMap::new(); source.scopes.len()],
            implicit_m: BTreeSet::new(),
        };
        for (id, model) in source.models.iter().enumerate() {
            // The first matching declaration is visible in its lexical scope.
            result.models[model.scope]
                .entry(model.card.name.to_ascii_lowercase())
                .or_insert(id);
        }
        // Native ngspice's implicit multiplier is a declaration-level
        // transformation. An explicit m formal suppresses that rewrite.
        // Propagate through the static call graph, including other calls of
        // the same declaration which use the synthetic default m=1.
        let mut edges = vec![Vec::new(); source.scopes.len()];
        let mut pending = Vec::new();
        for (owner, scope) in source.scopes.iter().enumerate() {
            for card in &scope.cards {
                if is_x(card)
                    && let Ok(call) = call(card)
                    && let Some(target) = result.subcircuits.resolve(source, owner, call.target)
                    && !source.scopes[target].defaults.contains_key("m")
                {
                    edges[owner].push(target);
                    if call.args.contains_key("m") && result.implicit_m.insert(target) {
                        pending.push(target);
                    }
                }
            }
        }
        while let Some(owner) = pending.pop() {
            for &target in &edges[owner] {
                if result.implicit_m.insert(target) {
                    pending.push(target);
                }
            }
        }
        Ok(result)
    }
}

fn is_x(card: &SourceCard) -> bool {
    card.text
        .as_bytes()
        .first()
        .is_some_and(|b| b.eq_ignore_ascii_case(&b'x'))
}

type VariantKey = (usize, bool, Vec<u64>);

struct Builder<'a> {
    source: &'a ModelLibrary,
    limits: Limits,
    index: Index,
    result: Topology<'a>,
    variants: BTreeMap<VariantKey, usize>,
    // A model's expressions are evaluated once per actual owning frame.
    resolved: BTreeMap<(usize, usize), Option<(usize, VdmosInstance)>>,
    active: Vec<usize>,
}

impl<'a> Builder<'a> {
    fn new(source: &'a ModelLibrary, limits: Limits) -> Result<Self> {
        ensure!(
            limits.depth > 0 && limits.depth <= 128,
            "topology depth must be in 1..=128"
        );
        ensure!(limits.occurrences > 0, "topology occurrence budget is zero");
        Ok(Self {
            source,
            limits,
            index: Index::new(source)?,
            result: Topology {
                frames: Vec::new(),
                mos: Vec::new(),
                models: Vec::new(),
            },
            variants: BTreeMap::new(),
            resolved: BTreeMap::new(),
            active: Vec::new(),
        })
    }

    fn build(mut self) -> Result<Topology<'a>> {
        self.visit(Frame {
            parent: None,
            declaration: 0,
            name: String::new(),
            location: self.source.location(0),
            definition_location: self.source.location(0),
            ports: BTreeMap::new(),
            overrides: BTreeMap::new(),
        })?;
        Ok(self.result)
    }

    fn room(&self) -> Result<()> {
        ensure!(
            self.result
                .frames
                .len()
                .saturating_add(self.result.mos.len())
                < self.limits.occurrences,
            "topology occurrence budget exceeded"
        );
        Ok(())
    }

    fn visit(&mut self, frame: Frame<'a>) -> Result<()> {
        self.room()?;
        let scope = frame.declaration;
        ensure!(
            self.active.len() < self.limits.depth,
            "subcircuit depth exceeded"
        );
        ensure!(
            !self.active.contains(&scope),
            "recursive subcircuit {}",
            frame.name
        );
        let id = self.result.frames.len();
        self.result.frames.push(frame);
        self.active.push(scope);
        let mut resolver = self.resolver(id)?;
        let mut names = BTreeSet::new();
        for card in &self.source.scopes[scope].cards {
            let name = card
                .text
                .split_whitespace()
                .next()
                .context("empty device line")?;
            ensure!(
                names.insert(name.to_ascii_lowercase()),
                "line {}: duplicate device instance {name}",
                card.line
            );
            let result = if is_x(card) {
                self.visit_call(id, card, &mut resolver)
            } else {
                self.visit_mos(id, card, &mut resolver)
            };
            result.with_context(|| {
                self.source.location(card.line).origin.map_or_else(
                    || format!("line {}: {name}", card.line),
                    |origin| format!("{}:{}: {name}", origin.path.display(), origin.line),
                )
            })?;
        }
        self.active.pop();
        Ok(())
    }

    fn resolver(&self, frame: usize) -> Result<ScopeResolver<'a>> {
        let mut current = Some(frame);
        let mut overrides = Vec::new();
        let mut parents = Vec::new();
        while let Some(id) = current {
            let item = &self.result.frames[id];
            let scope = item.declaration;
            overrides.extend(
                item.overrides
                    .iter()
                    .map(|(key, &value)| (scope, key.as_str(), value)),
            );
            parents.push((
                scope,
                item.parent.map(|p| self.result.frames[p].declaration),
            ));
            current = item.parent;
        }
        let mut resolver = ScopeResolver::new(self.source, &overrides)?;
        for (scope, parent) in parents {
            resolver.parameter_parents.insert(scope, parent);
        }
        Ok(resolver)
    }

    fn node(&self, frame: usize, name: &str) -> Node {
        let name = if name.eq_ignore_ascii_case("gnd") {
            "0".to_owned()
        } else {
            name.to_ascii_lowercase()
        };
        if frame == 0 || name == "0" || self.source.globals.contains(&name) {
            Node::Global(name)
        } else if let Some(bound) = self.result.frames[frame].ports.get(&name) {
            bound.clone()
        } else {
            Node::Local { frame, name }
        }
    }

    fn visit_call(
        &mut self,
        parent: usize,
        card: &SourceCard,
        resolver: &mut ScopeResolver<'a>,
    ) -> Result<()> {
        let call = call(card)?;
        let owner = self.result.frames[parent].declaration;
        let target = self
            .index
            .subcircuits
            .resolve(self.source, owner, call.target)
            .with_context(|| format!("unknown lexical subcircuit {}", call.target))?;
        let definition = &self.source.scopes[target];
        ensure!(
            call.nodes.len() == definition.ports.len(),
            "subcircuit {} expects {} terminals, got {}",
            call.target,
            definition.ports.len(),
            call.nodes.len()
        );
        let mut ports = BTreeMap::new();
        for (formal, actual) in definition.ports.iter().zip(call.nodes) {
            ensure!(
                ports
                    .insert(formal.clone(), self.node(parent, actual))
                    .is_none(),
                "duplicate formal terminal {formal}"
            );
        }
        let mut overrides = BTreeMap::new();
        for (key, expr) in call.args {
            ensure!(
                definition.defaults.contains_key(&key)
                    || (key == "m" && self.index.implicit_m.contains(&target)),
                "unknown actual parameter {key} for {}",
                call.target
            );
            // In native mode a rewritten implicit multiplier replaces the
            // original instance multiplier, including nested X calls.
            if key != "m" || !self.index.implicit_m.contains(&owner) {
                overrides.insert(key, resolver.evaluate(owner, &expr, None)?);
            }
        }
        if self.index.implicit_m.contains(&owner) {
            overrides.insert("m".into(), resolver.symbol(owner, "m")?);
        } else if self.index.implicit_m.contains(&target) {
            overrides.entry("m".into()).or_insert(1.);
        }
        self.visit(Frame {
            parent: Some(parent),
            declaration: target,
            name: call.name.to_ascii_lowercase(),
            location: self.source.location(card.line),
            definition_location: self.source.location(definition.source_line),
            ports,
            overrides,
        })
    }

    fn model_owner(&self, mut frame: usize, declaration: usize) -> Result<usize> {
        loop {
            let item = &self.result.frames[frame];
            if item.declaration == declaration {
                return Ok(frame);
            }
            frame = item
                .parent
                .context("lexical model has no active declaring frame")?;
        }
    }

    fn model(
        &mut self,
        declaration: usize,
        owner: usize,
    ) -> Result<Option<(usize, VdmosInstance)>> {
        if let Some(&value) = self.resolved.get(&(declaration, owner)) {
            return Ok(value);
        }
        let model = &self.source.models[declaration];
        let mut resolver = self.resolver(owner)?;
        let value = vdmos_resolved(&model.card, |expr| {
            resolver.evaluate(model.scope, &SpiceExpression::parse(expr)?, None)
        })?;
        let variant = value
            .map(|values| -> Result<_> {
                let mut instance = VdmosInstance::default();
                // Native model instance defaults are prepended, so the first
                // declaration wins; explicit device fields subsequently override.
                for field in values.instance_defaults.iter().rev() {
                    apply_instance(&mut instance, field, |expr| {
                        resolver.evaluate(model.scope, &SpiceExpression::parse(expr)?, None)
                    })?;
                }
                let key = (
                    declaration,
                    values.polarity == VdmosPolarity::P,
                    values
                        .parameters
                        .iter()
                        .map(|(_, value)| value.to_bits())
                        .collect(),
                );
                let id = *self.variants.entry(key).or_insert_with(|| {
                    let id = self.result.models.len();
                    self.result.models.push(ModelVariant {
                        declaration,
                        location: self.source.location(model.card.source_line),
                        values,
                    });
                    id
                });
                Ok((id, instance))
            })
            .transpose()?;
        self.resolved.insert((declaration, owner), variant);
        Ok(variant)
    }

    fn visit_mos(
        &mut self,
        frame: usize,
        card: &SourceCard,
        resolver: &mut ScopeResolver<'a>,
    ) -> Result<()> {
        self.room()?;
        let scope = self.result.frames[frame].declaration;
        let (tokens, _) = positional_prefix(&card.text);
        let (name, _) = tokens.first().context("empty MOS line")?;
        // INP2M checks for an existing model after at least three terminals.
        let (at, declaration) = tokens.iter().enumerate().skip(4).take(5).find_map(|(i, (token, _))| {
            lexical_lookup(self.source, scope, token, &self.index.models).map(|model| (i, model))
        }).context("no visible exact MOS model name (model bin selection belongs to the family binder)")?;
        let model_frame = self.model_owner(frame, self.source.models[declaration].scope)?;
        let nodes = tokens[1..at]
            .iter()
            .map(|&(node, _)| self.node(frame, node))
            .collect::<Vec<_>>();
        let fields = instance_fields(&card.text[tokens[at].1..])?;
        let vdmos = if let Some((model, defaults)) = self.model(declaration, model_frame)? {
            Some(self.bind_vdmos(model, defaults, &nodes, &fields, resolver, scope)?)
        } else {
            None
        };
        self.result.mos.push(Mos {
            frame,
            name: name.to_ascii_lowercase(),
            location: self.source.location(card.line),
            model_declaration: declaration,
            model_frame,
            nodes,
            source_parameters: fields,
            vdmos,
        });
        Ok(())
    }

    fn bind_vdmos(
        &self,
        model: usize,
        mut instance: VdmosInstance,
        nodes: &[Node],
        fields: &[ModelParameter],
        resolver: &mut ScopeResolver<'a>,
        scope: usize,
    ) -> Result<VdmosBinding> {
        ensure!(
            (3..=5).contains(&nodes.len()),
            "VDMOS needs three electrical and at most two thermal terminals"
        );
        for field in fields {
            if field.name.eq_ignore_ascii_case("m") && self.index.implicit_m.contains(&scope) {
                continue;
            }
            apply_instance(&mut instance, field, |expr| {
                resolver.evaluate(scope, &SpiceExpression::parse(expr)?, None)
            })?;
        }
        if self.index.implicit_m.contains(&scope) {
            instance.multiplier = resolver.symbol(scope, "m")?;
        }
        ensure!(
            instance.multiplier.is_finite() && instance.multiplier > 0.,
            "invalid VDMOS multiplier"
        );
        let self_heating = instance.thermal_requested
            && self.result.models[model]
                .values
                .parameters
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case("rthjc"));
        let mut terminals = nodes[..3].to_vec();
        if self_heating {
            for (index, terminal) in [ThermalTerminal::Junction, ThermalTerminal::Case]
                .into_iter()
                .enumerate()
            {
                terminals.push(
                    nodes
                        .get(index + 3)
                        .cloned()
                        .unwrap_or(Node::DeviceThermal {
                            device: self.result.mos.len(),
                            terminal,
                        }),
                );
            }
        }
        Ok(VdmosBinding {
            model,
            instance,
            terminals,
            self_heating,
        })
    }
}

#[expect(clippy::float_cmp, reason = "native flags accept exact zero or one")]
fn apply_instance(
    instance: &mut VdmosInstance,
    field: &ModelParameter,
    mut evaluate: impl FnMut(&str) -> Result<f64>,
) -> Result<()> {
    let name = field.name.to_ascii_lowercase();
    let expr = field.expression.as_deref();
    if matches!(name.as_str(), "thermal" | "off") {
        let flag = expr.map_or(Ok(1.), &mut evaluate)?;
        ensure!(flag == 0. || flag == 1., "VDMOS flag {name} must be 0 or 1");
        if name == "thermal" {
            instance.thermal_requested = flag != 0.;
        } else {
            instance.off = flag != 0.;
        }
        return Ok(());
    }
    let expr = expr.with_context(|| format!("VDMOS instance parameter {name} requires a value"))?;
    if name == "ic" {
        let vector = vector_fields(expr)?;
        ensure!(
            (1..=2).contains(&vector.len()),
            "VDMOS IC requires one or two voltages"
        );
        for (index, field) in vector.iter().enumerate() {
            instance.initial[index] = Some(evaluate(field)?);
        }
        return Ok(());
    }
    let value = evaluate(expr)?;
    match name.as_str() {
        "m" => instance.multiplier = value,
        "temp" => instance.temp_celsius = Some(value),
        "dtemp" => instance.dtemp_kelvin = Some(value),
        "icvds" => instance.initial[0] = Some(value),
        "icvgs" => instance.initial[1] = Some(value),
        _ => bail!("unknown VDMOS instance parameter {name}"),
    }
    Ok(())
}

// Split an initial-condition vector without splitting expressions or
// function arguments. Braces and apostrophes quote scalar expressions.
fn vector_fields(input: &str) -> Result<Vec<&str>> {
    let input = input.trim();
    let input = input
        .strip_prefix('(')
        .and_then(|s| s.strip_suffix(')'))
        .unwrap_or(input);
    let mut fields = Vec::new();
    let mut start = 0;
    let mut depth = 0usize;
    let mut quoted = false;
    for (index, ch) in input.char_indices() {
        match ch {
            '\'' => quoted = !quoted,
            '(' | '{' if !quoted => depth += 1,
            ')' | '}' if !quoted => {
                depth = depth.checked_sub(1).context("unbalanced IC expression")?;
            }
            ',' if !quoted && depth == 0 => {
                let field = input[start..index].trim();
                if !field.is_empty() {
                    fields.push(field);
                }
                start = index + 1;
            }
            c if c.is_whitespace() && !quoted && depth == 0 => {
                let field = input[start..index].trim();
                if !field.is_empty() {
                    fields.push(field);
                }
                start = index + c.len_utf8();
            }
            _ => {}
        }
    }
    let tail = input[start..].trim();
    if !tail.is_empty() {
        fields.push(tail);
    }
    ensure!(
        depth == 0 && !quoted && fields.iter().all(|f| !f.is_empty()),
        "invalid VDMOS IC vector"
    );
    Ok(fields)
}

#[cfg(test)]
mod tests;
