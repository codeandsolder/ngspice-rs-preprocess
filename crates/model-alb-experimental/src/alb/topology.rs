//! Discover actual compiled-ALB pin bundles through nested SPICE X instances.
//! Source annotations need only identify the analog and digital state nets,
//! their family, direction and supply. No physical triples are hand-mapped.

use super::{
    AnalogBoundaryPin, BridgeCatalog, DigitalPinDirection, LogicFamilyAssignment,
    NativeDigitalBundle, NativePinWiring, PlannedAnalogBridge, Result, bail, plan_analog_bridges,
};
use anyhow::{Context, ensure};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug)]
struct Instance {
    name: String,
    fields: Vec<String>,
}
#[derive(Clone, Debug)]
struct Subckt {
    path: Vec<String>,
    ports: Vec<String>,
    instances: Vec<Instance>,
    statements: Vec<String>,
}
#[derive(Default)]
struct Circuit {
    definitions: BTreeMap<Vec<String>, Subckt>,
    root: Vec<Instance>,
    root_statements: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DiscoveredAlbPin {
    pub id: String,
    pub direction: DigitalPinDirection,
    /// None means that at least one part of the bundle is private to
    /// a subcircuit and cannot be bridged at root without rewriting it.
    pub bundle: Option<NativeDigitalBundle>,
    pub output_res_ohm: Option<f64>,
}

/// A public analog net connected to a digital STATE node. Physical pin
/// occurrences and sink/source metadata are discovered from the source.
#[derive(Clone, Debug)]
pub struct AnalogDigitalConnection {
    pub analog_net: String,
    pub digital_net: String,
    pub direction: DigitalPinDirection,
    pub family: LogicFamilyAssignment,
    pub supply_v: f64,
}

#[derive(Clone)]
enum Node {
    Root(String),
    Local,
}

fn key(s: &str) -> String {
    s.to_ascii_uppercase()
}
fn node(s: &str) -> Result<String> {
    crate::validate_node(s)?;
    Ok(key(s))
}

fn statements(input: &str) -> Result<Vec<String>> {
    let mut result = Vec::<String>::new();
    let mut control = false;
    for (num, line) in input.lines().enumerate() {
        let mut quote = None;
        let cutoff = line.char_indices().find_map(|(at, ch)| {
            if ch == char::from(39) || ch == char::from(34) {
                if quote == Some(ch) {
                    quote = None;
                } else if quote.is_none() {
                    quote = Some(ch);
                }
            }
            (ch == ';' && quote.is_none()).then_some(at)
        });
        let s = line[..cutoff.unwrap_or(line.len())].trim();
        if s.is_empty() || s.starts_with(['*', ';']) {
            continue;
        }
        if s.eq_ignore_ascii_case(".control") {
            control = true;
            continue;
        }
        if s.eq_ignore_ascii_case(".endc") {
            ensure!(control, "orphan .endc at line {}", num + 1);
            control = false;
            continue;
        }
        if control {
            continue;
        }
        if let Some(continued) = s.strip_prefix('+') {
            let previous = result
                .last_mut()
                .with_context(|| format!("orphan continuation at line {}", num + 1))?;
            previous.push(' ');
            previous.push_str(continued.trim());
        } else {
            result.push(s.to_owned());
        }
    }
    ensure!(!control, "unterminated .control block");
    Ok(result)
}

fn header_ports(fields: &[&str]) -> Result<Vec<String>> {
    ensure!(fields.len() >= 2, "nameless .subckt");
    let mut ports = Vec::new();
    for s in fields.iter().skip(2) {
        if s.eq_ignore_ascii_case("params:") || s.eq_ignore_ascii_case("params") || s.contains('=')
        {
            break;
        }
        ports.push(node(s)?);
    }
    ensure!(
        ports.iter().collect::<BTreeSet<_>>().len() == ports.len(),
        "duplicate .subckt formal port"
    );
    Ok(ports)
}

fn parse_sources(deck: &str, definitions: &[&str]) -> Result<Circuit> {
    let mut circuit = Circuit::default();
    for (part, source) in std::iter::once(deck)
        .chain(definitions.iter().copied())
        .enumerate()
    {
        let mut scope = Vec::<String>::new();
        for statement in statements(source)? {
            let fields = statement.split_ascii_whitespace().collect::<Vec<_>>();
            let first = fields[0];
            if first.eq_ignore_ascii_case(".subckt") {
                ensure!(fields.len() >= 2, ".subckt without model name");
                let mut path = scope.clone();
                path.push(key(fields[1]));
                let def = Subckt {
                    path: path.clone(),
                    ports: header_ports(&fields)?,
                    instances: Vec::new(),
                    statements: Vec::new(),
                };
                ensure!(
                    circuit.definitions.insert(path.clone(), def).is_none(),
                    "duplicate subcircuit definition {}",
                    path.join(".")
                );
                scope = path;
            } else if first.eq_ignore_ascii_case(".ends") {
                let closing = scope.pop().context("unmatched .ends")?;
                if let Some(name) = fields.get(1) {
                    ensure!(key(name) == closing, ".ends {name} mismatches {closing}");
                }
            } else if first.starts_with(['X', 'x']) && first.len() > 1 {
                let entry = Instance {
                    name: key(first),
                    fields: fields[1..].iter().map(|s| (*s).to_owned()).collect(),
                };
                if scope.is_empty() {
                    ensure!(part == 0, "definitions file contains root-level X instance");
                    circuit.root.push(entry);
                } else {
                    circuit
                        .definitions
                        .get_mut(&scope)
                        .context("missing subcircuit scope")?
                        .instances
                        .push(entry);
                }
            } else if !scope.is_empty() {
                circuit
                    .definitions
                    .get_mut(&scope)
                    .context("missing subcircuit scope")?
                    .statements
                    .push(statement);
            } else if part == 0 {
                circuit.root_statements.push(statement);
            }
        }
        ensure!(scope.is_empty(), "unterminated .subckt in source {part}");
    }
    Ok(circuit)
}

fn lookup<'a>(circuit: &'a Circuit, scope: &[String], name: &str) -> Option<&'a Subckt> {
    let mut context = scope.to_vec();
    loop {
        let mut target = context.clone();
        target.push(key(name));
        if let Some(def) = circuit.definitions.get(&target) {
            return Some(def);
        }
        if context.pop().is_none() {
            break;
        }
    }
    None
}

fn resolve<'a>(
    circuit: &'a Circuit,
    scope: &[String],
    instance: &Instance,
) -> Result<(&'a Subckt, Vec<String>)> {
    let mut choices = Vec::new();
    for (i, word) in instance.fields.iter().enumerate() {
        let Some(def) = lookup(circuit, scope, word) else {
            continue;
        };
        if i != def.ports.len() {
            continue;
        }
        if instance.fields.get(i + 1).is_none_or(|s| {
            s.eq_ignore_ascii_case("params:") || s.eq_ignore_ascii_case("params") || s.contains('=')
        }) {
            choices.push((
                def,
                instance.fields[..i]
                    .iter()
                    .map(|s| key(s))
                    .collect::<Vec<_>>(),
            ));
        }
    }
    match choices.as_slice() {
        [(def, pins)] => Ok((def, pins.clone())),
        [] => bail!(
            "cannot resolve X{} in scope {scope:?}; missing or invalid subckt/port layout",
            instance.name
        ),
        _ => bail!("ambiguous X{} subcircuit resolution", instance.name),
    }
}

fn alb_signature(ports: &[String]) -> Option<(usize, usize)> {
    let mut at = 0;
    let mut counts = [0usize; 2];
    for (i, prefix) in ["SR_IN", "SR_OUT"].iter().enumerate() {
        while ports
            .get(at)
            .is_some_and(|s| s == &format!("{prefix}{}", counts[i]))
        {
            let n = counts[i];
            if ports.get(at + 1)? != &format!("{prefix}{n}_SINK")
                || ports.get(at + 2)? != &format!("{prefix}{n}_SOURCE")
            {
                return None;
            }
            at += 3;
            counts[i] += 1;
        }
    }
    (at == ports.len() && counts[1] > 0).then_some(counts.into())
}

fn output_resistance(def: &Subckt, pin: usize) -> Result<f64> {
    let formal = format!("SR_OUT{pin}");
    let drivers = def
        .statements
        .iter()
        .filter(|s| {
            let f = s.split_ascii_whitespace().collect::<Vec<_>>();
            f.first()
                .is_some_and(|x| x.to_ascii_uppercase().starts_with("ASR_"))
                && f.iter().any(|x| x.eq_ignore_ascii_case(&formal))
        })
        .collect::<Vec<_>>();
    ensure!(
        drivers.len() == 1,
        "ALB {} has {} physical drivers for {formal}",
        def.path.join("."),
        drivers.len()
    );
    let model = drivers[0]
        .split_ascii_whitespace()
        .last()
        .context("missing ALB model ref")?;
    let matching = def
        .statements
        .iter()
        .filter(|s| {
            let f = s.split_ascii_whitespace().collect::<Vec<_>>();
            f.len() >= 3
                && f[0].eq_ignore_ascii_case(".model")
                && f[1].eq_ignore_ascii_case(model)
                && f[2].to_ascii_uppercase().starts_with("SR_ALB_LUT_BIT")
        })
        .collect::<Vec<_>>();
    ensure!(
        matching.len() == 1,
        "missing unique ALB output model {model}"
    );
    let upper = matching[0].to_ascii_uppercase();
    let at = upper
        .find("OUT_RES=")
        .context("compiled output model missing OUT_RES")?
        + 8;
    let token = matching[0][at..]
        .split(|c: char| c.is_ascii_whitespace() || c == ')')
        .next()
        .context("empty OUT_RES")?;
    let value = crate::parse_numeric(token).context("invalid output resistance")?;
    ensure!(
        value.is_finite() && value >= 0.0,
        "invalid compiled ALB OUT_RES {value}"
    );
    Ok(value)
}

struct Walker<'a> {
    circuit: &'a Circuit,
    pins: Vec<DiscoveredAlbPin>,
    instance_paths: BTreeSet<String>,
}

impl Walker<'_> {
    fn visit(
        &mut self,
        scope: &[String],
        items: &[Instance],
        inherited: &BTreeMap<String, Node>,
        path: &str,
        recursion: &mut Vec<Vec<String>>,
    ) -> Result<()> {
        for instance in items {
            let (def, actual) = resolve(self.circuit, scope, instance)?;
            ensure!(
                !recursion.contains(&def.path),
                "recursive subcircuit model {} at {path}",
                def.path.join(".")
            );
            let instance_path = if path.is_empty() {
                instance.name.clone()
            } else {
                format!("{path}.{}", instance.name)
            };
            ensure!(
                self.instance_paths.insert(instance_path.clone()),
                "duplicate instance name {instance_path}"
            );
            let mut mapping = BTreeMap::new();
            for (formal, actual) in def.ports.iter().zip(&actual) {
                mapping.insert(
                    formal.clone(),
                    inherited.get(actual).cloned().unwrap_or(Node::Local),
                );
            }
            if let Some((inputs, outputs)) = alb_signature(&def.ports) {
                for (direction, count, prefix, idprefix) in [
                    (DigitalPinDirection::Input, inputs, "SR_IN", "in"),
                    (DigitalPinDirection::Output, outputs, "SR_OUT", "out"),
                ] {
                    for index in 0..count {
                        let nodes = [
                            format!("{prefix}{index}"),
                            format!("{prefix}{index}_SINK"),
                            format!("{prefix}{index}_SOURCE"),
                        ]
                        .map(|name| mapping.get(&name).cloned().unwrap_or(Node::Local));
                        let bundle = match nodes {
                            [
                                Node::Root(state),
                                Node::Root(sink_meta),
                                Node::Root(source_meta),
                            ] => Some(NativeDigitalBundle {
                                state,
                                sink_meta,
                                source_meta,
                            }),
                            _ => None,
                        };
                        self.pins.push(DiscoveredAlbPin {
                            id: format!("{instance_path}.{idprefix}{index}"),
                            direction,
                            bundle,
                            output_res_ohm: if direction == DigitalPinDirection::Output {
                                Some(output_resistance(def, index)?)
                            } else {
                                None
                            },
                        });
                    }
                }
            } else {
                recursion.push(def.path.clone());
                self.visit(
                    &def.path,
                    &def.instances,
                    &mapping,
                    &instance_path,
                    recursion,
                )?;
                recursion.pop();
            }
        }
        Ok(())
    }
}

/// Traverse an expanded SPICE deck and its supplied subcircuit definitions.
///
/// Source/library references are not silently invented; every X instance
/// must resolve to an available declaration. Local, non-exported nodes are
/// reported but cannot be bridged from the parent circuit.
///
/// # Errors
/// Malformed hierarchy, duplicate instances, unresolved or recursive
/// subcircuits, or invalid compiled-ALB port models.
pub fn discover_alb_pins(deck: &str, definitions: &[&str]) -> Result<Vec<DiscoveredAlbPin>> {
    let circuit = parse_sources(deck, definitions)?;
    let mut walker = Walker {
        circuit: &circuit,
        pins: Vec::new(),
        instance_paths: BTreeSet::new(),
    };
    let mut root = BTreeMap::new();
    for instance in &circuit.root {
        for item in &instance.fields {
            if let Ok(name) = node(item) {
                root.insert(name.clone(), Node::Root(name));
            }
        }
    }
    walker.visit(&[], &circuit.root, &root, "", &mut Vec::new())?;
    Ok(walker.pins)
}

/// Match each public digital-state node to every physical ALB input/output.
///
/// Each instance at that node. The existing family planner groups analog inputs;
/// actual sink/source metadata is recovered from the SPICE hierarchy.
///
/// # Errors
/// Unmatched state nodes, overlapping annotations or incompatible groups.
pub fn derive_family_wiring(
    pins: &[DiscoveredAlbPin],
    connections: &[AnalogDigitalConnection],
    catalog: &BridgeCatalog,
) -> Result<(Vec<PlannedAnalogBridge>, Vec<NativePinWiring>)> {
    let all_digital_nodes = pins
        .iter()
        .filter_map(|pin| pin.bundle.as_ref())
        .flat_map(|bundle| {
            [
                bundle.state.as_str(),
                bundle.sink_meta.as_str(),
                bundle.source_meta.as_str(),
            ]
        })
        .collect::<BTreeSet<_>>();
    let mut selected = BTreeSet::new();
    let mut physical = Vec::new();
    let mut wires = Vec::new();
    for entry in connections {
        let state = node(&entry.digital_net)?;
        let analog = node(&entry.analog_net)?;
        ensure!(
            !all_digital_nodes.contains(analog.as_str()),
            "analog boundary {analog} collides with a physical digital bundle node"
        );
        let family = entry.family.for_pin(entry.direction)?;
        let found = pins
            .iter()
            .filter(|p| {
                p.direction == entry.direction
                    && p.bundle.as_ref().is_some_and(|b| b.state == state)
            })
            .collect::<Vec<_>>();
        ensure!(
            !found.is_empty(),
            "no root-visible ALB {:?} physical pin on digital state {state}",
            entry.direction
        );
        for p in found {
            ensure!(
                selected.insert(p.id.clone()),
                "physical pin {} mapped to multiple mixed-signal boundaries",
                p.id
            );
            physical.push(AnalogBoundaryPin {
                id: p.id.clone(),
                analog_net: entry.analog_net.clone(),
                direction: p.direction,
                family: family.clone(),
                supply_v: entry.supply_v,
            });
            wires.push(NativePinWiring {
                id: p.id.clone(),
                bundle: p
                    .bundle
                    .clone()
                    .context("internal ALB bundle cannot connect at root")?,
                out_res_ohm: p.output_res_ohm,
            });
        }
    }
    Ok((plan_analog_bridges(&physical, catalog)?, wires))
}

/// One electrical boundary owned by a lexical subcircuit definition.
/// Empty scope selects the root deck. Non-root connections name strictly
/// private nets, not formals exported to parent circuits.
#[derive(Clone, Debug)]
pub struct ScopedAnalogDigitalConnection {
    pub scope: Vec<String>,
    pub boundary: AnalogDigitalConnection,
}

/// Existing A devices on an annotated digital state have an unproven
/// physical interface; inserting another converter there would be unsafe.
fn reject_existing_digital_devices(
    statements: &[String],
    entries: &[AnalogDigitalConnection],
    scope: &[String],
) -> Result<()> {
    let states = entries
        .iter()
        .map(|entry| node(&entry.digital_net))
        .collect::<Result<BTreeSet<_>>>()?;
    for statement in statements {
        let fields = statement.split_ascii_whitespace().collect::<Vec<_>>();
        let Some(device) = fields.first() else {
            continue;
        };
        if !device.starts_with(['A', 'a']) || device.len() < 2 {
            continue;
        }
        for field in fields.iter().skip(1) {
            let token = field
                .trim_matches(['[', ']', '(', ')', ','])
                .to_ascii_uppercase();
            if states.contains(&token) {
                bail!(
                    "unverified pre-existing A device {device} on digital node {token} in {scope:?}"
                );
            }
        }
    }
    Ok(())
}

fn pins_in_scope(circuit: &Circuit, scope: &[String]) -> Result<Vec<DiscoveredAlbPin>> {
    let mut walk = Walker {
        circuit,
        pins: Vec::new(),
        instance_paths: BTreeSet::new(),
    };
    let instances = if scope.is_empty() {
        &circuit.root
    } else {
        &circuit
            .definitions
            .get(scope)
            .with_context(|| format!("unknown lexical subcircuit {scope:?}"))?
            .instances
    };
    let mut available = BTreeMap::new();
    for instance in instances {
        let (_, actual) = resolve(circuit, scope, instance)?;
        for name in actual {
            available.insert(name.clone(), Node::Root(name));
        }
    }
    walk.visit(scope, instances, &available, "", &mut Vec::new())?;
    Ok(walk.pins)
}

fn insert_scoped_bridges(deck: &str, generated: &BTreeMap<Vec<String>, String>) -> Result<String> {
    let mut output = String::new();
    let mut path = Vec::<String>::new();
    let mut inserted = BTreeSet::new();
    let mut control = false;
    for line in deck.split_inclusive('\n') {
        let visible = line.split(';').next().unwrap_or_default().trim();
        let words = visible.split_ascii_whitespace().collect::<Vec<_>>();
        let first = words.first().copied().unwrap_or_default();
        if first.eq_ignore_ascii_case(".control") {
            control = true;
        } else if first.eq_ignore_ascii_case(".endc") {
            control = false;
        }
        if !control {
            if first.eq_ignore_ascii_case(".subckt") {
                ensure!(
                    words.len() >= 2,
                    "nameless subcircuit during bridge insertion"
                );
                path.push(key(words[1]));
            } else if first.eq_ignore_ascii_case(".ends") {
                ensure!(!path.is_empty(), "orphan .ends during bridge insertion");
                if let Some(native) = generated.get(&path) {
                    ensure!(
                        inserted.insert(path.clone()),
                        "duplicate bridge scope {path:?}"
                    );
                    output.push_str(native);
                }
                let closed = path.pop().context("no lexical subcircuit")?;
                if let Some(name) = words.get(1) {
                    ensure!(key(name) == closed, "mismatched .ends name {name}/{closed}");
                }
            } else if first.eq_ignore_ascii_case(".end") {
                ensure!(path.is_empty(), "root .end inside a subcircuit");
                if let Some(native) = generated.get(&Vec::new()) {
                    ensure!(
                        inserted.insert(Vec::new()),
                        "duplicate root bridge insertion"
                    );
                    output.push_str(native);
                }
            }
        }
        output.push_str(line);
    }
    ensure!(
        path.is_empty(),
        "unterminated subcircuit during scoped insertion"
    );
    ensure!(
        inserted.len() == generated.len(),
        "requested lexical bridge scope was not encountered in source"
    );
    Ok(output)
}

/// Resolve and emit native family bridges into their owning subcircuits.
///
/// Every instance of a definition gets its own local analog/digital bridge.
/// This does not give different electrical parameter sets to individual
/// instances of the *same* definition.
///
/// # Errors
/// Invalid scopes, exported private nets, physical mapping conflicts,
/// incompatible family models or duplicate model/device names.
pub fn materialize_scoped_family_bridges(
    deck: &str,
    compiled: &[&str],
    connections: &[ScopedAnalogDigitalConnection],
    catalog: &BridgeCatalog,
    native: &super::NativeFamilySpecification,
) -> Result<String> {
    ensure!(
        native.pins.is_empty(),
        "automatic scoped wiring rejects handwritten physical pin triples"
    );
    let circuit = parse_sources(deck, compiled)?;
    let mut by_scope = BTreeMap::<Vec<String>, Vec<AnalogDigitalConnection>>::new();
    for entry in connections {
        let path = entry.scope.iter().map(|s| key(s)).collect::<Vec<_>>();
        for item in &path {
            node(item)?;
        }
        if !path.is_empty() {
            let owner = circuit
                .definitions
                .get(&path)
                .with_context(|| format!("unknown private boundary scope {path:?}"))?;
            node(&entry.boundary.analog_net)?;
            let digital = node(&entry.boundary.digital_net)?;
            ensure!(
                !owner.ports.contains(&digital),
                "boundary in {path:?} cannot connect an exported digital formal {digital}"
            );
        }
        by_scope
            .entry(path)
            .or_default()
            .push(entry.boundary.clone());
    }
    let mut emitted = BTreeMap::new();
    for (path, entries) in by_scope {
        let statements = if path.is_empty() {
            &circuit.root_statements
        } else {
            &circuit
                .definitions
                .get(&path)
                .context("missing private definition")?
                .statements
        };
        reject_existing_digital_devices(statements, &entries, &path)?;
        let pins = pins_in_scope(&circuit, &path)?;
        let (plans, wires) = derive_family_wiring(&pins, &entries, catalog)
            .with_context(|| format!("resolving scoped family bridges in {path:?}"))?;
        let spec = super::NativeFamilySpecification {
            models: native.models.clone(),
            pins: wires,
        };
        let text = super::render_native_family_bridges(&plans, &spec)?;
        // Native model/device identifiers are intentionally local to the
        // enclosing subcircuit. Do not shadow an existing declaration.
        let existing = if path.is_empty() {
            deck.lines().map(str::to_owned).collect::<Vec<_>>()
        } else {
            circuit
                .definitions
                .get(&path)
                .context("lost definition scope")?
                .statements
                .clone()
        };
        for line in text.lines() {
            let words = line.split_ascii_whitespace().collect::<Vec<_>>();
            let Some(first) = words.first() else {
                continue;
            };
            let declared = if first.eq_ignore_ascii_case(".model") {
                words.get(1).copied()
            } else if first.starts_with(['A', 'a']) {
                Some(*first)
            } else {
                None
            };
            if let Some(name) = declared {
                ensure!(
                    !existing.iter().any(|line| {
                        let original = line.split_ascii_whitespace().collect::<Vec<_>>();
                        original
                            .first()
                            .is_some_and(|word| word.eq_ignore_ascii_case(name))
                            || (original
                                .first()
                                .is_some_and(|word| word.eq_ignore_ascii_case(".model"))
                                && original
                                    .get(1)
                                    .is_some_and(|word| word.eq_ignore_ascii_case(name)))
                    }),
                    "native family identifier {name} already exists in scope {path:?}"
                );
            }
        }
        emitted.insert(path, text);
    }
    insert_scoped_bridges(deck, &emitted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AnalogBridgeKind, BridgeModel, LogicFamily};

    const ALB: &str = "
.subckt ALB_BUFFER sr_in0 sr_in0_sink sr_in0_source sr_out0 sr_out0_sink sr_out0_source
Asr_load0 sr_in0 sr_in0_sink sr_in0_source Msr_input_load
.model Msr_input_load sr_bundle_load(capacitance=2p sink_current=0 source_current=0)
Asr_o0 [sr_in0] sr_out0 sr_out0_sink sr_out0_source 0 Msr_o0
.model Msr_o0 sr_alb_lut_bit(table_values=\"01\" delay=2n input_load=0 out_res=40e0)
.ends ALB_BUFFER
";

    fn catalog() -> Result<BridgeCatalog> {
        let mut c = BridgeCatalog::default();
        for (name, kind) in [
            ("HC_ADC", AnalogBridgeKind::Adc),
            ("HC_DAC", AnalogBridgeKind::Dac),
        ] {
            c.add(BridgeModel {
                name: name.to_owned(),
                kind,
                family: LogicFamily::parse("HC")?,
                nominal_supply_v: 5.0,
            })?;
        }
        Ok(c)
    }
    fn mapping(digital: &str, direction: DigitalPinDirection) -> AnalogDigitalConnection {
        AnalogDigitalConnection {
            analog_net: match direction {
                DigitalPinDirection::Input => "AIN",
                DigitalPinDirection::Output => "AOUT",
            }
            .to_owned(),
            digital_net: digital.to_owned(),
            direction,
            family: LogicFamilyAssignment {
                in_family: Some("HC".to_owned()),
                out_family: Some("HC".to_owned()),
                family: None,
            },
            supply_v: 3.3,
        }
    }

    #[test]
    fn direct_and_nested_public_bundles_and_output_resistance() -> Result<()> {
        let deck = "
Xtop input input_sink input_source output output_sink output_source OUTER
.subckt OUTER ain ain_sink ain_source aout aout_sink aout_source
Xmiddle ain ain_sink ain_source aout aout_sink aout_source INNER gain=1
.subckt INNER din din_sink din_source dout dout_sink dout_source
Xcore din din_sink din_source dout dout_sink dout_source ALB_BUFFER
.ends INNER
.ends OUTER
.end
";
        let pins = discover_alb_pins(deck, &[ALB])?;
        assert_eq!(pins.len(), 2);
        assert_eq!(pins[0].id, "XTOP.XMIDDLE.XCORE.in0");
        assert_eq!(pins[1].id, "XTOP.XMIDDLE.XCORE.out0");
        let input = pins[0].bundle.as_ref().context("missing input bundle")?;
        assert_eq!(
            (&*input.state, &*input.sink_meta, &*input.source_meta),
            ("INPUT", "INPUT_SINK", "INPUT_SOURCE")
        );
        assert_eq!(pins[1].output_res_ohm, Some(40.0));
        let (plans, wires) = derive_family_wiring(
            &pins,
            &[
                mapping("input", DigitalPinDirection::Input),
                mapping("OUTPUT", DigitalPinDirection::Output),
            ],
            &catalog()?,
        )?;
        assert_eq!(plans.len(), 2);
        assert_eq!(wires.len(), 2);
        assert_eq!(wires[1].out_res_ohm, Some(40.0));
        Ok(())
    }

    #[test]
    fn shared_input_maps_two_physical_loads_into_one_adc_group() -> Result<()> {
        let deck = "
Xleft d d_sink d_source y1 y1_sink y1_source ALB_BUFFER
Xright d d_sink d_source y2 y2_sink y2_source ALB_BUFFER
.end
";
        let pins = discover_alb_pins(deck, &[ALB])?;
        assert_eq!(pins.len(), 4);
        let (plans, wires) = derive_family_wiring(
            &pins,
            &[mapping("d", DigitalPinDirection::Input)],
            &catalog()?,
        )?;
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].physical_pins, vec!["XLEFT.in0", "XRIGHT.in0"]);
        assert_eq!(wires.len(), 2);
        assert_eq!(wires[0].bundle, wires[1].bundle);
        Ok(())
    }

    #[test]
    fn private_node_never_aliases_public_boundary() -> Result<()> {
        let deck = "
Xwrap exported exp_sink exp_source WRAP
.subckt WRAP out out_sink out_source
Xcore private private_sink private_source out out_sink out_source ALB_BUFFER
.ends WRAP
.end";
        let pins = discover_alb_pins(deck, &[ALB])?;
        assert_eq!(pins.len(), 2);
        assert!(pins[0].bundle.is_none());
        assert!(pins[1].bundle.is_some());
        assert!(
            derive_family_wiring(
                &pins,
                &[mapping("PRIVATE", DigitalPinDirection::Input)],
                &catalog()?
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn malformed_or_ambiguous_hierarchies_fail() {
        for deck in [
            "Xbad in in_sink in_source out out_sink out_source UNRESOLVED\n.end",
            "Xbad in in_sink in_source out out_sink ALB_BUFFER\n.end",
            "Xdup a b c d e f ALB_BUFFER\nXdup x y z p q r ALB_BUFFER\n.end",
            "Xloop a b c RECURSE\n.subckt RECURSE a b c\nXloop a b c RECURSE\n.ends RECURSE\n.end",
            "Xx a b c ALB_BUFFER\n.subckt ALB_BUFFER a b c\n.ends ALB_BUFFER\n.end",
            "Xx a b c d e f ALB_BUFFER\n.subckt ALB_BUFFER a b c\n.ends ALB_BUFFER\n.end",
        ] {
            assert!(discover_alb_pins(deck, &[ALB]).is_err(), "accepted {deck}");
        }
    }

    #[test]
    fn mixed_family_annotations_must_not_overlap_same_physical_port() -> Result<()> {
        let pins = discover_alb_pins(
            "Xone a a_sink a_source y y_sink y_source ALB_BUFFER\n.end",
            &[ALB],
        )?;
        assert!(
            derive_family_wiring(
                &pins,
                &[
                    mapping("a", DigitalPinDirection::Input),
                    mapping("A", DigitalPinDirection::Input),
                ],
                &catalog()?
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn malformed_compiled_output_model_is_rejected() {
        let broken = ALB.replace("out_res=40e0", "out_res=bad");
        assert!(discover_alb_pins("Xone a b c y z q ALB_BUFFER\n.end", &[&broken]).is_err());
    }
    #[test]
    fn comments_and_zero_resistance_remain_valid_source_semantics() -> Result<()> {
        let ideal = ALB.replace("out_res=40e0", "out_res=0e0");
        let deck = "Xone in in_sink in_source out out_sink out_source ALB_BUFFER ; comment\n.end\n";
        let pins = discover_alb_pins(deck, &[&ideal])?;
        assert_eq!(pins.len(), 2);
        assert_eq!(pins[1].output_res_ohm, Some(0.0));
        Ok(())
    }
}
