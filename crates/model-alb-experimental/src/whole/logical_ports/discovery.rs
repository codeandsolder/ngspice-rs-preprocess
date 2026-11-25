//! Discover mixed ALB pin boundaries from actual SPICE device terminals.
//! Electrical supply is deliberately not guessed from a logic-family name.

use super::{
    DigitalPinDirection, Graph, Item, LogicalAlbBoundary, Result,
    compile_explicit_alb_source_models, key, normalized_statement, parse_items,
    receiver_groups::receiver_paths, reserved, statement_tokens,
};
use anyhow::{Context, bail, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Physical pin classification supplied by an external X subcircuit's
/// verified interface signature. No electrical type is guessed from its name.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OpaqueXPinKind {
    /// SPICE analog terminal; may witness an ADC/DAC cut.
    Analog,
    /// Externally managed digital terminal; never silently bridged.
    Digital,
}

/// Reusable externally defined X-subcircuit signature, not a per-net mapping.
///
/// Every terminal is listed in the original .SUBCKT order, including
/// digital-only pins. This is typically obtained from an external model
/// library's verified electrical interface documentation.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OpaqueXInterface {
    /// External subcircuit model name, matched case-insensitively.
    pub model: String,
    /// Complete port types in model definition order.
    pub pins: Vec<OpaqueXPinKind>,
    /// Optional native subcircuit replacement for the original X model.
    /// Each digital terminal expands into a verified event-state triple;
    /// analog terminals remain physical single wires. Analog-only native
    /// replacements are also supported without inventing digital nodes.
    #[serde(default)]
    pub native_model: Option<String>,
    /// Exact formal names of the supplied native subcircuit, in original
    /// analog-single / digital-state-sink-source terminal order.
    /// Required when `native_model` is specified; verified against the real
    /// .subckt declaration in the original included library.
    #[serde(default)]
    pub native_formals: Vec<String>,
}

pub(super) fn opaque_x_interfaces(
    definitions: &[OpaqueXInterface],
) -> Result<BTreeMap<String, &OpaqueXInterface>> {
    let mut names = BTreeMap::new();
    for interface in definitions {
        crate::validate_node(&interface.model)?;
        if let Some(native) = &interface.native_model {
            crate::validate_node(native)?;
            let width = interface
                .pins
                .iter()
                .map(|kind| {
                    if *kind == OpaqueXPinKind::Digital {
                        3
                    } else {
                        1
                    }
                })
                .sum::<usize>();
            ensure!(
                interface.native_formals.len() == width,
                "native X replacement {} requires {width} named event/analog formals, got {}",
                interface.model,
                interface.native_formals.len()
            );
            let mut names = BTreeSet::new();
            for formal in &interface.native_formals {
                crate::validate_node(formal)?;
                ensure!(
                    names.insert(key(formal)),
                    "native X replacement {} repeats formal {formal}",
                    interface.model
                );
            }
        } else {
            ensure!(
                interface.native_formals.is_empty(),
                "native X formal names require an explicit native_model for {}",
                interface.model
            );
        }
        ensure!(
            !interface.pins.is_empty(),
            "opaque X model {} must declare its complete pin interface",
            interface.model
        );
        let name = key(&interface.model);
        ensure!(
            names.insert(name.clone(), interface).is_none(),
            "duplicate opaque X interface {name}"
        );
    }
    Ok(names)
}

/// Recognize a model-typed original X instance using only a verified
/// complete port signature, never its suffix parameter expressions.
pub(super) fn external_x_pins(
    statement: &str,
    definitions: &BTreeMap<String, &OpaqueXInterface>,
) -> Result<Vec<(String, OpaqueXPinKind)>> {
    let fields = statement_tokens(&normalized_statement(statement))?;
    let Some(instance) = fields.first() else {
        return Ok(Vec::new());
    };
    if !instance.starts_with(['X', 'x']) || instance.len() <= 1 {
        return Ok(Vec::new());
    }
    let candidates = fields
        .iter()
        .enumerate()
        .skip(1)
        .filter(|(_, token)| definitions.contains_key(&key(token)))
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    ensure!(
        candidates.len() == 1,
        "ambiguous external X model signatures in original {statement}"
    );
    let (position, model) = candidates[0];
    let definition = definitions
        .get(&key(model))
        .context("missing opaque external model")?;
    ensure!(
        position == definition.pins.len() + 1,
        "external X instance {instance} of {} has {} actual pins but its verified interface requires {}",
        definition.model,
        position.saturating_sub(1),
        definition.pins.len()
    );
    ensure!(
        fields.get(position + 1).is_none_or(|first| {
            first.eq_ignore_ascii_case("params:")
                || first.eq_ignore_ascii_case("params")
                || first.contains('=')
        }),
        "unsupported external X parameter suffix in {statement}"
    );
    definition
        .pins
        .iter()
        .enumerate()
        .map(|(index, kind)| {
            let node = key(&fields[index + 1]);
            crate::validate_node(&node)?;
            Ok((node, *kind))
        })
        .collect()
}

/// An inferred analog/digital ALB connection.
///
/// Its physical analog terminal, direction, and family are witnessed by
/// actual original SPICE devices. Supply voltage follows lexical models
/// or the explicit caller default.
#[derive(Clone, Debug)]
pub struct DiscoveredAlbBoundary {
    pub boundary: LogicalAlbBoundary,
    pub family: String,
    /// Effective source supply, with the caller default resolved.
    pub supply_v: f64,
}

/// Return the actual electrical terminals of known SPICE primitives, not
/// arbitrary tokens (which can also denote model names and parameter values).
/// X calls are represented by their resolved lexical connection graph.
fn analog_terminals(statement: &str) -> Result<Vec<String>> {
    let normalized = normalized_statement(statement);
    let fields = statement_tokens(&normalized)?;
    let Some(name) = fields.first() else {
        return Ok(Vec::new());
    };
    if name.starts_with(['.', '*', ';']) || name.len() < 2 {
        return Ok(Vec::new());
    }
    let terminals = match name.as_bytes()[0].to_ascii_uppercase() {
        b'R' | b'C' | b'L' | b'V' | b'I' | b'D' | b'B' | b'F' | b'H' | b'W' => 2,
        b'Q' | b'J' | b'Z' => 3,
        b'M' | b'E' | b'G' | b'S' | b'T' => 4,
        _ => return Ok(Vec::new()),
    };
    if fields.len() <= terminals {
        return Ok(Vec::new());
    }
    Ok(fields[1..=terminals]
        .iter()
        .filter(|field| *field != "0")
        .map(|field| key(field))
        .collect())
}

/// Identify analog voltage measurements in expressions and simulator
/// output directives. A voltage probe is a genuine analog consumer even
/// without a resistor or another physical component attached to that net.
fn measured_voltage_nodes(statement: &str) -> Vec<String> {
    let bytes = statement.as_bytes();
    let mut nodes = Vec::new();
    let mut index = 0;
    while index + 2 < bytes.len() {
        let voltage = bytes[index].eq_ignore_ascii_case(&b'v');
        let previous_is_word =
            index > 0 && (bytes[index - 1].is_ascii_alphanumeric() || bytes[index - 1] == b'_');
        if voltage && !previous_is_word && bytes[index + 1] == b'(' {
            let content = &statement[index + 2..];
            if let Some(end) = content.find(')') {
                let inner = &content[..end];
                for name in inner.split(',') {
                    let name = name.trim();
                    if !name.is_empty()
                        && name != "0"
                        && name.bytes().all(|byte| {
                            byte.is_ascii_alphanumeric()
                                || matches!(byte, b'_' | b'.' | b'$' | b'#')
                        })
                    {
                        nodes.push(key(name));
                    }
                }
                index += end + 3;
                continue;
            }
        }
        index += 1;
    }
    nodes
}

pub(super) fn observed_analog_nodes(
    graph: &Graph,
    scope: &[String],
    items: &[Item],
    external: &BTreeMap<String, &OpaqueXInterface>,
) -> Result<BTreeSet<String>> {
    let mut inside_alb_file = false;
    let mut inside_control = false;
    let mut observed = BTreeSet::new();
    for item in items {
        let Item::Statement(statement) = item else {
            continue;
        };
        let normalized = normalized_statement(statement);
        let first = normalized
            .split_ascii_whitespace()
            .next()
            .unwrap_or_default();
        if first.eq_ignore_ascii_case(".file") {
            ensure!(!inside_alb_file, "nested embedded ALB .file");
            inside_alb_file = true;
        } else if first.eq_ignore_ascii_case(".endf") {
            ensure!(inside_alb_file, "orphan embedded ALB .endf");
            inside_alb_file = false;
        } else if !inside_alb_file {
            if first.eq_ignore_ascii_case(".control") {
                ensure!(!inside_control, "nested .control in original ALB deck");
                inside_control = true;
            } else if first.eq_ignore_ascii_case(".endc") {
                ensure!(inside_control, "orphan .endc in original ALB deck");
                inside_control = false;
            } else if inside_control
                || matches!(
                    first.to_ascii_uppercase().as_str(),
                    ".PRINT" | ".SAVE" | ".PROBE" | ".MEAS" | ".MEASURE"
                )
            {
                observed.extend(measured_voltage_nodes(&normalized));
            } else if !first.starts_with('.') {
                // A lexical X definition is traversed by graph wiring;
                // only an *unresolved* X can use an independently verified
                // external model signature to witness analog pins.
                let terminals = if graph.call(statement, scope)?.is_some() {
                    Vec::new()
                } else if first.starts_with(['X', 'x']) {
                    let pins = external_x_pins(statement, external)?;
                    let roles = &graph
                        .scopes
                        .get(scope)
                        .context("missing original external X scope")?
                        .alb_roles;
                    let fields = statement_tokens(&normalized)?;
                    let has_native = fields
                        .iter()
                        .filter_map(|name| external.get(&key(name)))
                        .any(|interface| interface.native_model.is_some());
                    for (node, kind) in &pins {
                        ensure!(
                            *kind != OpaqueXPinKind::Digital
                                || !roles.contains_key(node)
                                || has_native,
                            "verified external X digital pin {node} on {first} touches ALB digital state; native digital X interface requires an explicit compatible bundle"
                        );
                    }
                    pins.into_iter()
                        .filter_map(|(node, kind)| {
                            (kind == OpaqueXPinKind::Analog && node != "0").then_some(node)
                        })
                        .collect()
                } else {
                    analog_terminals(statement)?
                };
                if !terminals.is_empty() {
                    observed.extend(terminals);
                    observed.extend(measured_voltage_nodes(&normalized));
                }
            }
        }
    }
    ensure!(!inside_alb_file, "unterminated embedded ALB .file");
    ensure!(
        !inside_control,
        "unterminated .control in original ALB deck"
    );
    Ok(observed)
}

fn fresh_state_name(
    analog: &str,
    direction: DigitalPinDirection,
    reserved: &mut BTreeSet<String>,
) -> Result<String> {
    let suffix = match direction {
        DigitalPinDirection::Input => "ADC",
        DigitalPinDirection::Output => "DAC",
    };
    let base = format!("SR_AUTO_{analog}_{suffix}");
    for number in 0..u32::MAX {
        let candidate = if number == 0 {
            base.clone()
        } else {
            format!("{base}_{number}")
        };
        if reserved.insert(candidate.clone()) {
            return Ok(candidate);
        }
    }
    bail!("exhausted generated ALB state names for {analog}")
}

/// Process actual referenced X definitions before their callers even when
/// lexical scope names sort in the opposite order of electrical hierarchy.
pub(super) fn dependency_order(graph: &Graph) -> Result<Vec<Vec<String>>> {
    fn visit(
        graph: &Graph,
        scope: &[String],
        active: &mut Vec<Vec<String>>,
        visited: &mut BTreeSet<Vec<String>>,
        ordered: &mut Vec<Vec<String>>,
    ) -> Result<()> {
        if visited.contains(scope) {
            return Ok(());
        }
        ensure!(
            active.len() < 256 && !active.contains(&scope.to_vec()),
            "recursive original X circuit at {scope:?}"
        );
        active.push(scope.to_vec());
        for call in graph.occurrences(scope)?.calls() {
            visit(graph, &call.target, active, visited, ordered)?;
        }
        active.pop();
        if visited.insert(scope.to_vec()) {
            ordered.push(scope.to_vec());
        }
        Ok(())
    }
    let mut visited = BTreeSet::new();
    let mut ordered = Vec::new();
    for scope in graph.scopes.keys() {
        visit(graph, scope, &mut Vec::new(), &mut visited, &mut ordered)?;
    }
    Ok(ordered)
}

/// Physical evidence follows the actual X wiring regardless of whether a
/// child owns any ALB pins. Walk leaves first so a passive-only forwarding
/// chain contributes its contacts to the caller in the same pass.
pub(super) fn witnessed_analog_contacts(
    graph: &Graph,
    external: &BTreeMap<String, &OpaqueXInterface>,
    scopes: &[Vec<String>],
) -> Result<BTreeMap<Vec<String>, BTreeSet<String>>> {
    let mut contacts = graph
        .scopes
        .iter()
        .map(|(path, scope)| {
            Ok((
                path.clone(),
                observed_analog_nodes(graph, path, &scope.items, external)?,
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    super::global_nodes::project_physical_contacts(graph, &mut contacts)?;
    let callers = index_call_sites(graph);
    for scope in scopes {
        let Some(calls) = callers.get(scope) else {
            continue;
        };
        let owner = graph.scopes.get(scope).context("missing witness scope")?;
        let witnessed = contacts.get(scope).context("missing witness contacts")?;
        let indices = owner
            .ports
            .iter()
            .enumerate()
            .filter_map(|(index, formal)| witnessed.contains(formal).then_some(index))
            .collect::<Vec<_>>();
        for (parent, call) in calls {
            let actual = contacts
                .get_mut(*parent)
                .context("missing witness caller")?;
            actual.extend(indices.iter().map(|index| call.actual[*index].clone()));
        }
    }
    super::global_nodes::project_physical_contacts(graph, &mut contacts)?;
    Ok(contacts)
}

/// In addition to actual terminal evidence, classify a mixed-role ALB
/// formal as analog if all callers connect it to witnessed physical nets.
/// Mixed analog/digital uses require occurrence-specific specialization.
fn physical_analog_contacts(
    graph: &Graph,
    external: &BTreeMap<String, &OpaqueXInterface>,
    scopes: &[Vec<String>],
) -> Result<BTreeMap<Vec<String>, BTreeSet<String>>> {
    let mut contacts = witnessed_analog_contacts(graph, external, scopes)?;
    loop {
        let mut occurrences =
            BTreeMap::<(Vec<String>, String), Vec<(Vec<String>, String, bool)>>::new();
        let mut parent_contacts = BTreeSet::new();
        for (path, index) in &graph.occurrences {
            for call in index.calls() {
                let child = graph
                    .scopes
                    .get(&call.target)
                    .context("unknown child of physical ALB X call")?;
                for (formal, actual) in child.ports.iter().zip(&call.actual) {
                    if contacts
                        .get(&call.target)
                        .is_some_and(|known| known.contains(formal))
                    {
                        parent_contacts.insert((path.clone(), actual.clone()));
                    }
                    if child
                        .alb_roles
                        .get(formal)
                        .is_some_and(|roles| roles.len() == 2)
                    {
                        occurrences
                            .entry((call.target.clone(), formal.clone()))
                            .or_default()
                            .push((
                                path.clone(),
                                key(&call.name),
                                contacts
                                    .get(path)
                                    .is_some_and(|known| known.contains(actual)),
                            ));
                    }
                }
            }
        }
        let mut changed = false;
        for (path, actual) in parent_contacts {
            changed |= contacts.entry(path).or_default().insert(actual);
        }
        let mut conflicts = Vec::new();
        for ((child, formal), uses) in occurrences {
            if uses.iter().all(|(_, _, analog)| *analog) {
                changed |= contacts.entry(child).or_default().insert(formal);
            } else if uses.iter().any(|(_, _, analog)| *analog) {
                conflicts.push((child, formal, uses));
            }
        }
        if !changed {
            ensure!(
                conflicts.is_empty(),
                "reusable ALB X formal has both analog and digital occurrences {conflicts:?}; requires occurrence-specific specialization"
            );
            return Ok(contacts);
        }
    }
}

/// Compile a witnessed original analog node into separately family-typed
/// ADC inputs and individually identified physical DAC output producers.
fn boundaries_for_analog_node(
    graph: &Graph,
    scope: &[String],
    analog: &str,
    roles: &BTreeSet<DigitalPinDirection>,
    occupied: &mut BTreeSet<String>,
    default_supply: f64,
) -> Result<Vec<DiscoveredAlbBoundary>> {
    let producers = if roles.contains(&DigitalPinDirection::Output) {
        graph.producer_paths(scope, analog)?
    } else {
        Vec::new()
    };
    if roles.contains(&DigitalPinDirection::Output) {
        ensure!(
            !producers.is_empty(),
            "ALB output {analog} in {scope:?} has no producer"
        );
    }
    let mut discovered = Vec::new();
    if roles.contains(&DigitalPinDirection::Input) {
        let mut groups = BTreeMap::<(String, u64), Vec<super::AlbInputReceiver>>::new();
        for receiver in receiver_paths(graph, scope, analog)? {
            let model = &graph.models[receiver.model];
            groups
                .entry((
                    model.in_family.clone(),
                    model.supply_v.unwrap_or(default_supply).to_bits(),
                ))
                .or_default()
                .push(receiver.identity);
        }
        ensure!(!groups.is_empty(), "missing ALB input receiver");
        for ((family, supply_bits), receivers) in groups {
            discovered.push(DiscoveredAlbBoundary {
                boundary: LogicalAlbBoundary {
                    scope: scope.to_vec(),
                    analog_net: analog.to_owned(),
                    digital_net: fresh_state_name(analog, DigitalPinDirection::Input, occupied)?,
                    direction: DigitalPinDirection::Input,
                    producer: None,
                    receivers,
                },
                family,
                supply_v: f64::from_bits(supply_bits),
            });
        }
    }
    for original in &producers {
        let model = &graph.models[original.model];
        discovered.push(DiscoveredAlbBoundary {
            boundary: LogicalAlbBoundary {
                scope: scope.to_vec(),
                analog_net: analog.to_owned(),
                digital_net: fresh_state_name(analog, DigitalPinDirection::Output, occupied)?,
                direction: DigitalPinDirection::Output,
                producer: (producers.len() > 1).then(|| original.identity.clone()),
                receivers: Vec::new(),
            },
            family: model.out_family.clone(),
            supply_v: model.supply_v.unwrap_or(default_supply),
        });
    }
    Ok(discovered)
}

type Callers<'a> = BTreeMap<Vec<String>, Vec<(&'a [String], &'a super::Call)>>;

fn index_call_sites(graph: &Graph) -> Callers<'_> {
    let mut callers = Callers::new();
    for (path, index) in &graph.occurrences {
        for call in index.calls() {
            callers
                .entry(call.target.clone())
                .or_default()
                .push((path, call));
        }
    }
    callers
}

fn lift_adc_to_call(
    graph: &Graph,
    found: &mut Vec<DiscoveredAlbBoundary>,
    entry: &DiscoveredAlbBoundary,
    parent: &[String],
    call: &super::Call,
    index: usize,
) -> Result<()> {
    let actual = &call.actual[index];
    let formal = &entry.boundary.analog_net;
    let mut receivers = entry.boundary.receivers.clone();
    for receiver in &mut receivers {
        receiver.prepend(key(&call.name), Some(formal.clone()));
    }
    if let Some(existing) = found.iter_mut().find(|candidate| {
        candidate.boundary.scope == parent
            && candidate.boundary.analog_net == *actual
            && candidate.boundary.direction == DigitalPinDirection::Input
            && candidate.family == entry.family
            && candidate.supply_v.to_bits() == entry.supply_v.to_bits()
    }) {
        existing.boundary.receivers.extend(receivers);
    } else {
        let mut occupied = reserved(
            graph
                .scopes
                .get(parent)
                .context("missing ADC caller scope")?,
        );
        occupied.extend(
            found
                .iter()
                .filter(|candidate| candidate.boundary.scope == parent)
                .map(|candidate| key(&candidate.boundary.digital_net)),
        );
        found.push(DiscoveredAlbBoundary {
            boundary: LogicalAlbBoundary {
                scope: parent.to_vec(),
                analog_net: actual.clone(),
                digital_net: fresh_state_name(actual, DigitalPinDirection::Input, &mut occupied)?,
                direction: DigitalPinDirection::Input,
                producer: None,
                receivers,
            },
            family: entry.family.clone(),
            supply_v: entry.supply_v,
        });
    }
    Ok(())
}

fn omit_implicit_receiver_routes(found: &mut [DiscoveredAlbBoundary]) {
    // The local homogeneous case uses its existing implicit single primary
    // state. All physical cross-cut domains keep complete explicit routes.
    let mut counts = BTreeMap::new();
    let lifted = found
        .iter()
        .filter(|entry| {
            entry
                .boundary
                .receivers
                .iter()
                .any(|receiver| !receiver.analog_hops.is_empty())
        })
        .map(|entry| {
            (
                entry.boundary.scope.clone(),
                entry.boundary.analog_net.clone(),
            )
        })
        .collect::<BTreeSet<_>>();
    for entry in found.iter() {
        if entry.boundary.direction == DigitalPinDirection::Input {
            *counts
                .entry((
                    entry.boundary.scope.clone(),
                    entry.boundary.analog_net.clone(),
                ))
                .or_insert(0usize) += 1;
        }
    }
    for entry in found {
        let node = (
            entry.boundary.scope.clone(),
            entry.boundary.analog_net.clone(),
        );
        if counts.get(&node) == Some(&1) && !lifted.contains(&node) {
            entry.boundary.receivers.clear();
        }
    }
}

/// Lift ADCs from scalar exported ports to the actual caller's physical
/// net. Processing the call graph from leaves to roots aggregates every
/// receiver occurrence without cloning reusable subcircuit definitions.
/// DACs remain at their existing independent driver cuts.
fn consolidate_adc_boundaries(
    graph: &Graph,
    scopes: &[Vec<String>],
    mut found: Vec<DiscoveredAlbBoundary>,
) -> Result<Vec<DiscoveredAlbBoundary>> {
    let callers = index_call_sites(graph);
    for scope in scopes {
        let Some(calls) = callers.get(scope) else {
            continue;
        };
        let owner = graph
            .scopes
            .get(scope)
            .context("missing ADC consolidation scope")?;
        let (lift, keep): (Vec<_>, Vec<_>) = found.into_iter().partition(|entry| {
            entry.boundary.scope == *scope
                && entry.boundary.direction == DigitalPinDirection::Input
                && owner.ports.contains(&entry.boundary.analog_net)
        });
        found = keep;
        for entry in lift {
            let index = owner
                .ports
                .iter()
                .position(|port| port == &entry.boundary.analog_net)
                .context("missing lifted ADC formal position")?;
            for (parent, call) in calls {
                lift_adc_to_call(graph, &mut found, &entry, parent, call, index)?;
            }
        }
    }
    omit_implicit_receiver_routes(&mut found);
    Ok(found)
}

/// Discover electrical ALB boundaries in the original hierarchy.
///
/// Actual analog component terminals determine the physical side. Traverse
/// reusable X definitions from deepest to shallowest; an inner analog
/// terminal stays analog even when an outer X is connected to a driver.
/// Independent ALB output producers receive distinct DAC state wires.
///
/// Effective supplies are resolved from lexical models or the explicit
/// default before grouping input receivers. Bridge selection remains a
/// separate electrical catalog decision.
///
/// # Errors
/// Invalid original ALB/primitive syntax, unsupported or ambiguous interfaces,
/// recursive hierarchy, or unavailable ALB pin ownership.
pub fn discover_original_alb_boundaries(
    source: &str,
    default_supply: f64,
) -> Result<Vec<DiscoveredAlbBoundary>> {
    discover_original_alb_boundaries_with_opaque_x_interfaces(source, &[], default_supply)
}

/// Discover physical boundaries including verified external X interface pins.
///
/// Each supplied model signature must list every analog/digital terminal,
/// while all undefined external interfaces remain untrusted and fail closed
/// when connected to an untranslated ALB digital state.
///
/// # Errors
/// Ambiguous external signatures, pin-count mismatches, unsupported
/// electrical ownership or any original-source compilation error.
pub fn discover_original_alb_boundaries_with_opaque_x_interfaces(
    source: &str,
    external: &[OpaqueXInterface],
    default_supply: f64,
) -> Result<Vec<DiscoveredAlbBoundary>> {
    ensure!(
        default_supply.is_finite() && default_supply > 0.0,
        "ALB discovery requires positive finite supply"
    );
    let definitions = opaque_x_interfaces(external)?;
    let items = parse_items(source)?;
    let models = compile_explicit_alb_source_models(source)?;
    let mut graph = Graph::new(&items, Some(models))?;
    let scopes = dependency_order(&graph)?;
    super::global_nodes::prepare(&mut graph, &scopes)?;
    let mut found = Vec::new();
    graph.discover_alb_roles()?;
    let contacts = physical_analog_contacts(&graph, &definitions, &scopes)?;
    for scope in &scopes {
        let owner = graph.scopes.get(scope).context("missing discovery scope")?;
        let observed = contacts
            .get(scope)
            .context("missing physical analog contact scope")?;
        let relevant = observed
            .iter()
            .cloned()
            .filter_map(|analog| {
                owner
                    .alb_roles
                    .get(&analog)
                    .map(|roles| (analog, roles.clone()))
            })
            .collect::<Vec<_>>();
        let mut occupied = reserved(owner);
        for item in found
            .iter()
            .filter(|item: &&DiscoveredAlbBoundary| item.boundary.scope == *scope)
        {
            occupied.insert(key(&item.boundary.digital_net));
        }
        let mut discovered_here = Vec::new();
        for (analog, roles) in relevant {
            discovered_here.extend(boundaries_for_analog_node(
                &graph,
                scope,
                &analog,
                &roles,
                &mut occupied,
                default_supply,
            )?);
        }
        if !discovered_here.is_empty() {
            graph.mark_exported_cuts(
                &discovered_here
                    .iter()
                    .map(|entry| entry.boundary.clone())
                    .collect::<Vec<_>>(),
            )?;
            graph.propagate_scalar_ports()?;
            for owner in graph.scopes.values_mut() {
                owner.alb_roles.clear();
            }
            graph.discover_alb_roles()?;
        }
        found.extend(discovered_here);
    }
    consolidate_adc_boundaries(&graph, &scopes, found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::whole::expand_logical_alb_source;

    #[test]
    fn implicit_ground_and_global_connections_are_projected_to_root() -> Result<()> {
        for (source, net, domains, receivers) in [
            (
                include_str!(
                    "../../../../../runtime/xspice/fixtures/vendor-implicit-globals/ground-nested.net"
                ),
                "0",
                1,
                3,
            ),
            (
                include_str!(
                    "../../../../../runtime/xspice/fixtures/vendor-implicit-globals/ground-shared.net"
                ),
                "0",
                2,
                3,
            ),
            (
                include_str!(
                    "../../../../../runtime/xspice/fixtures/vendor-implicit-globals/global-physical.net"
                ),
                "P",
                1,
                3,
            ),
        ] {
            let discovered = discover_original_alb_boundaries(source, 5.0)?;
            let groups = discovered
                .iter()
                .filter(|entry| {
                    entry.boundary.analog_net == net
                        && entry.boundary.direction == DigitalPinDirection::Input
                })
                .collect::<Vec<_>>();
            assert_eq!(groups.len(), domains);
            assert!(groups.iter().all(|entry| entry.boundary.scope.is_empty()));
            assert_eq!(
                groups
                    .iter()
                    .map(|entry| entry.boundary.receivers.len())
                    .sum::<usize>(),
                receivers
            );
            let boundaries = discovered
                .into_iter()
                .map(|entry| entry.boundary)
                .collect::<Vec<_>>();
            let expanded = expand_logical_alb_source(source, &boundaries, "native.inc")?;
            assert!(expanded.deck.contains(".include \"native.inc\""));
        }
        Ok(())
    }

    #[test]
    fn implicit_global_formals_cannot_shadow_declared_global_nodes() {
        let source = include_str!(
            "../../../../../runtime/xspice/fixtures/vendor-implicit-globals/global-local-shadow.net"
        );
        assert!(discover_original_alb_boundaries(source, 5.0).is_err());
    }

    #[test]
    fn passive_witnesses_discover_original_analog_net_without_voltage_probe() -> Result<()> {
        for source in [
            include_str!(
                "../../../../../runtime/xspice/fixtures/vendor-passive-witness/passive-direct.net"
            ),
            include_str!(
                "../../../../../runtime/xspice/fixtures/vendor-passive-witness/passive-nested.net"
            ),
            include_str!(
                "../../../../../runtime/xspice/fixtures/vendor-passive-witness/passive-shared.net"
            ),
            include_str!(
                "../../../../../runtime/xspice/fixtures/vendor-passive-witness/passive-independent.net"
            ),
        ] {
            assert!(!source.contains("V(p)"));
            let discovered = discover_original_alb_boundaries(source, 5.0)?;
            let actual = discovered
                .iter()
                .map(|entry| (entry.boundary.analog_net.as_str(), entry.boundary.direction))
                .collect::<BTreeSet<_>>();
            assert_eq!(
                actual,
                BTreeSet::from([
                    ("D", DigitalPinDirection::Input),
                    ("P", DigitalPinDirection::Input),
                    ("P", DigitalPinDirection::Output),
                    ("Q", DigitalPinDirection::Output),
                ])
            );
            assert!(
                discovered
                    .iter()
                    .all(|entry| entry.boundary.scope.is_empty())
            );
            let boundaries = discovered
                .into_iter()
                .map(|entry| entry.boundary)
                .collect::<Vec<_>>();
            let expanded = expand_logical_alb_source(source, &boundaries, "native.inc")?;
            assert!(expanded.deck.contains(".subckt LOAD P\n"));
            assert!(expanded.deck.contains("Xload P LOAD\n"));
            if source.contains("LOAD_INNER") {
                assert!(expanded.deck.contains(".subckt LOAD_INNER P\n"));
                assert!(expanded.deck.contains("Xlower P LOAD_INNER\n"));
            }
            if source.contains("Xother") {
                assert!(expanded.deck.contains("Xother X LOAD\n"));
            }
            let missing = boundaries
                .into_iter()
                .filter(|entry| entry.analog_net != "P")
                .collect::<Vec<_>>();
            assert!(expand_logical_alb_source(source, &missing, "native.inc").is_err());
        }
        Ok(())
    }

    #[test]
    fn passive_contact_does_not_convert_unrelated_digital_formal() -> Result<()> {
        let source = r#"
Vinput d 0 3.3
Aproducer [d] [p] M
Asense [p] [q] M
Xload p dsignal doutput LOAD
Ainput [d] [dsignal] M
Aoutput [doutput] [qaux] M
Rout q 0 100k
Raux qaux 0 100k
.model M d_logic_block family="HC" vsupply=3.3 def="out[0]=in[0];"
.subckt LOAD p a b
Rload p 0 100
Alocal [a] [b] M
.ends LOAD
.end
"#;
        let discovered = discover_original_alb_boundaries(source, 5.0)?;
        assert_eq!(discovered.len(), 5);
        assert!(
            !discovered
                .iter()
                .any(|entry| matches!(entry.boundary.analog_net.as_str(), "DSIGNAL" | "DOUTPUT"))
        );
        let boundaries = discovered
            .into_iter()
            .map(|entry| entry.boundary)
            .collect::<Vec<_>>();
        let expanded = expand_logical_alb_source(source, &boundaries, "native.inc")?;
        assert!(
            expanded
                .deck
                .contains(".subckt LOAD P A A_SR_SINK A_SR_SOURCE B B_SR_SINK B_SR_SOURCE\n")
        );
        assert!(expanded.deck.contains("Xload P DSIGNAL DSIGNAL_SR_SINK DSIGNAL_SR_SOURCE DOUTPUT DOUTPUT_SR_SINK DOUTPUT_SR_SOURCE LOAD"));
        Ok(())
    }

    #[test]
    fn discovers_root_mixed_boundaries_through_reusable_nested_x_definitions() -> Result<()> {
        for source in [
            include_str!("../../../../../runtime/xspice/fixtures/family/logical-original.cir"),
            include_str!("../../../../../runtime/xspice/fixtures/family/logical-nested.cir"),
            include_str!(
                "../../../../../runtime/xspice/fixtures/family/logical-device-variants.cir"
            ),
            include_str!("../../../../../runtime/xspice/fixtures/family/logical-device-comb.cir"),
        ] {
            let found = discover_original_alb_boundaries(source, 5.0)?;
            assert_eq!(
                found.len(),
                if source.contains("analog_copy") { 4 } else { 2 }
            );
            assert!(found.iter().all(|item| item.boundary.scope.is_empty()));
            assert!(found.iter().all(|item| item.family == "HC"));
            assert!(found.iter().any(|item| {
                item.boundary.analog_net == "ANALOG_INPUT"
                    && item.boundary.direction == DigitalPinDirection::Input
            }));
            assert!(found.iter().any(|item| {
                item.boundary.analog_net.starts_with("ANALOG_")
                    && item.boundary.direction == DigitalPinDirection::Output
            }));
            let bindings = found
                .into_iter()
                .map(|item| item.boundary)
                .collect::<Vec<_>>();
            let lowered = expand_logical_alb_source(source, &bindings, "generated.inc")?;
            assert!(lowered.deck.contains("SR_AUTO_ANALOG_INPUT_ADC"));
            assert!(lowered.library.contains(".subckt SRALB0"));
        }
        Ok(())
    }

    #[test]
    fn inner_analog_contact_keeps_parent_x_formal_analog() -> Result<()> {
        let source = r#"
* Analog resistor inside a reusable digital ALB wrapper
Vin drive 0 1.5
Xtop drive load TOP
Rload load 0 100k
.subckt TOP din dout
Rin din 0 10k
A1 [din] [dout] DL
.model DL d_logic_block family="HC" def="out[0]=in[0];"
.ends TOP
.end
"#;
        let found = discover_original_alb_boundaries(source, 5.0)?;
        assert_eq!(found.len(), 2, "{found:?}");
        let input = found
            .iter()
            .find(|item| item.boundary.direction == DigitalPinDirection::Input)
            .context("missing physical input contact")?;
        assert_eq!(input.boundary.scope, Vec::<String>::new());
        assert_eq!(input.boundary.analog_net, "DRIVE");
        assert_eq!(input.boundary.receivers[0].path, ["XTOP", "A1"]);
        assert_eq!(
            input.boundary.receivers[0].analog_hops,
            BTreeMap::from([(0, "DIN".to_owned())])
        );
        let output = found
            .iter()
            .find(|item| item.boundary.direction == DigitalPinDirection::Output)
            .context("missing physical output contact")?;
        assert_eq!(output.boundary.scope, Vec::<String>::new());
        assert_eq!(output.boundary.analog_net, "LOAD");
        let rewritten = expand_logical_alb_source(
            source,
            &found
                .into_iter()
                .map(|item| item.boundary)
                .collect::<Vec<_>>(),
            "native.inc",
        )?;
        assert!(rewritten.deck.contains(".subckt TOP DIN"));
        assert!(rewritten.deck.contains("Rin din 0 10k"));
        assert!(rewritten.deck.contains("SR_AUTO_DRIVE_ADC"));
        Ok(())
    }

    #[test]
    fn shared_analog_net_requires_two_independent_dacs() -> Result<()> {
        let source = r#"
Vin d 0 1.5
Rload q 0 100k
A1 [d] [q] DL
A2 [d] [q] DL
.model DL d_logic_block family="HC" def="out[0]=in[0];"
.end
"#;
        let found = discover_original_alb_boundaries(source, 5.0)?;
        assert_eq!(found.len(), 3);
        let outputs = found
            .iter()
            .filter(|item| item.boundary.direction == DigitalPinDirection::Output)
            .collect::<Vec<_>>();
        assert_eq!(outputs.len(), 2);
        assert_eq!(
            outputs[0]
                .boundary
                .producer
                .as_ref()
                .context("first producer")?
                .path,
            ["A1"]
        );
        assert_eq!(
            outputs[1]
                .boundary
                .producer
                .as_ref()
                .context("second producer")?
                .path,
            ["A2"]
        );
        assert_ne!(
            outputs[0].boundary.digital_net,
            outputs[1].boundary.digital_net
        );
        let lowered = expand_logical_alb_source(
            source,
            &found
                .into_iter()
                .map(|item| item.boundary)
                .collect::<Vec<_>>(),
            "native.inc",
        )?;
        assert!(lowered.deck.contains("SR_AUTO_Q_DAC"));
        assert!(lowered.deck.contains("SR_AUTO_Q_DAC_1"));
        Ok(())
    }

    #[test]
    fn different_physical_families_receive_separate_adc_routes() -> Result<()> {
        let source = r#"
Vin d 0 1.5
A1 [d] [q1] DL1
A2 [d] [q2] DL2
.model DL1 d_logic_block family="HC" def="out[0]=in[0];"
.model DL2 d_logic_block family="HCT" def="out[0]=in[0];"
.end
"#;
        let found = discover_original_alb_boundaries(source, 5.0)?;
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].family, "HC");
        assert_eq!(found[1].family, "HCT");
        assert_eq!(found[0].boundary.receivers[0].path, ["A1"]);
        assert_eq!(found[1].boundary.receivers[0].path, ["A2"]);
        let boundaries = found
            .into_iter()
            .map(|item| item.boundary)
            .collect::<Vec<_>>();
        let deck = expand_logical_alb_source(source, &boundaries, "native.inc")?.deck;
        assert!(deck.contains("XSR_A1 SR_AUTO_D_ADC SR_AUTO_D_ADC_SR_SINK"));
        assert!(deck.contains("XSR_A2 SR_AUTO_D_ADC_1 SR_AUTO_D_ADC_1_SR_SINK"));
        let mut incomplete = boundaries;
        incomplete[0].receivers[0].input_pin = 1;
        assert!(expand_logical_alb_source(source, &incomplete, "native.inc").is_err());
        Ok(())
    }

    #[test]
    fn physical_cuts_follow_actual_x_call_order_not_lexical_names() -> Result<()> {
        let source = r#"
Vin drive 0 1.5
Xwrap drive load AA
Rload load 0 100k
.subckt AA din dout
Xinner din dout ZZ
.ends AA
.subckt ZZ p q
Rin p 0 10k
A1 [p] [q] DL
.model DL d_logic_block family="HC" def="out[0]=in[0];"
.ends ZZ
.end
"#;
        let found = discover_original_alb_boundaries(source, 5.0)?;
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found.iter().any(|item| {
            item.boundary.scope.is_empty()
                && item.boundary.analog_net == "DRIVE"
                && item.boundary.direction == DigitalPinDirection::Input
                && item.boundary.receivers[0].analog_hops
                    == BTreeMap::from([(0, "DIN".to_owned()), (1, "P".to_owned())])
        }));
        assert!(found.iter().any(|item| {
            item.boundary.scope == Vec::<String>::new()
                && item.boundary.analog_net == "LOAD"
                && item.boundary.direction == DigitalPinDirection::Output
        }));
        Ok(())
    }

    #[test]
    fn independent_output_producers_can_have_different_physical_families() -> Result<()> {
        let source = r#"
Vin d 0 1.5
Rload q 0 100k
A1 [d] [q] DL1
A2 [d] [q] DL2
.model DL1 d_logic_block family="HC" def="out[0]=in[0];"
.model DL2 d_logic_block family="HC" out_family="HCT" def="out[0]=in[0];"
.end
"#;
        let found = discover_original_alb_boundaries(source, 5.0)?;
        assert_eq!(found.len(), 3);
        let outputs = found
            .iter()
            .filter(|item| item.boundary.direction == DigitalPinDirection::Output)
            .collect::<Vec<_>>();
        assert_eq!(outputs.len(), 2);
        assert_eq!(outputs[0].family, "HC");
        assert_eq!(outputs[1].family, "HCT");
        assert_eq!(
            outputs[0].boundary.producer.as_ref().context("first")?.path,
            ["A1"]
        );
        assert_eq!(
            outputs[1]
                .boundary
                .producer
                .as_ref()
                .context("second")?
                .path,
            ["A2"]
        );
        Ok(())
    }

    #[test]
    fn analog_voltage_observations_are_real_physical_boundaries() -> Result<()> {
        for source in [
            include_str!("../../../../../runtime/xspice/fixtures/family/logical-observe-print.cir"),
            include_str!(
                "../../../../../runtime/xspice/fixtures/family/logical-observe-control.cir"
            ),
        ] {
            let found = discover_original_alb_boundaries(source, 5.0)?;
            assert_eq!(found.len(), 2, "{found:?}");
            assert!(found.iter().any(|item| {
                item.boundary.analog_net == "ANALOG_INPUT"
                    && item.boundary.direction == DigitalPinDirection::Input
            }));
            assert!(found.iter().any(|item| {
                item.boundary.analog_net == "ANALOG_OUTPUT"
                    && item.boundary.direction == DigitalPinDirection::Output
            }));
        }
        assert_eq!(
            measured_voltage_nodes("plot v(a,b) V(c) i(Vd)"),
            ["A", "B", "C"]
        );
        Ok(())
    }
}

#[cfg(test)]
mod bidirectional_automatic_tests {
    use super::*;
    use crate::whole::expand_logical_alb_source;

    #[test]
    fn original_bidirectional_formal_keeps_local_dac_and_lifts_adc() -> Result<()> {
        let source =
            include_str!("../../../../../runtime/xspice/fixtures/family/bidirectional-port.cir");
        let discovered = discover_original_alb_boundaries(source, 5.0)?;
        assert_eq!(discovered.len(), 4, "{discovered:?}");
        assert_eq!(
            discovered
                .iter()
                .filter(|item| item.boundary.scope == ["TOP"])
                .count(),
            1
        );
        assert_eq!(
            discovered
                .iter()
                .filter(|item| item.boundary.scope.is_empty())
                .count(),
            3
        );
        assert_eq!(
            discovered
                .iter()
                .filter(|item| item.boundary.analog_net == "PIN")
                .count(),
            2
        );
        let lowered = expand_logical_alb_source(
            source,
            &discovered
                .into_iter()
                .map(|item| item.boundary)
                .collect::<Vec<_>>(),
            "generated.inc",
        )?;
        assert!(
            lowered
                .deck
                .contains(".subckt TOP PIN OE OE_SR_SINK OE_SR_SOURCE MONITOR")
        );
        assert!(lowered.deck.contains("SR_AUTO_PIN_ADC"));
        assert!(lowered.deck.contains("SR_AUTO_PIN_DAC"));
        assert!(lowered.deck.contains("Xone PIN SR_AUTO_OE_ADC"));
        assert!(lowered.deck.contains("SR_AUTO_OBSERVE_DAC"));
        Ok(())
    }

    #[test]
    fn reusable_mixed_formal_with_different_analog_and_digital_callers_is_ambiguous() -> Result<()>
    {
        let source = r#"
Vdrive drive 0 1.5
Rdrive drive pad 1k
Venable en 0 3.3
Xphysical pad en monitor TOP
Xdigital data en unused TOP
Adigital [drive] [data] BUFFER
.model BUFFER d_logic_block family="HC" def="out[0]=in[0];"
Rmonitor monitor 0 100k
.subckt TOP pin oe monitor
A1 [pin oe] [pin monitor] DL
.model DL d_logic_block file=logic family="HC"
.file logic
PORT PIN in[0] ;
PORT OE in[1] ;
PORT DRIVE out[0] ;
PORT MON out[1] ;
COMB (WIDTH=1, DELAY=1n) D ;
D = PIN ;
DRIVE = 1 ;
DRIVE.En = OE ;
MON = D ;
.endf
.ends TOP
.end
"#;
        let error = discover_original_alb_boundaries(source, 5.0)
            .err()
            .context("mixed physical/digital reusable definition should be specialized")?;
        assert!(
            format!("{error:#}").contains("requires occurrence-specific specialization"),
            "{error:#}"
        );
        Ok(())
    }
}

#[cfg(test)]
mod opaque_interface_tests {
    use super::*;
    use crate::whole::{
        expand_logical_alb_source, expand_logical_alb_source_with_opaque_x_interfaces,
    };

    fn analog_link() -> OpaqueXInterface {
        OpaqueXInterface {
            model: "ANALOG_LINK".to_owned(),
            pins: vec![OpaqueXPinKind::Analog, OpaqueXPinKind::Analog],
            native_model: None,
            native_formals: Vec::new(),
        }
    }

    #[test]
    fn external_analog_x_pins_witness_private_alb_input_and_output() -> Result<()> {
        let original =
            include_str!("../../../../../runtime/xspice/fixtures/family/logical-opaque-x.cir");
        let interfaces = [analog_link()];
        let found =
            discover_original_alb_boundaries_with_opaque_x_interfaces(original, &interfaces, 5.0)?;
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found.iter().any(|contact| {
            contact.boundary.analog_net == "INNER_SIGNAL"
                && contact.boundary.direction == DigitalPinDirection::Input
                && contact.family == "HC"
        }));
        assert!(found.iter().any(|contact| {
            contact.boundary.analog_net == "INNER_OUTPUT"
                && contact.boundary.direction == DigitalPinDirection::Output
                && contact.family == "HC"
        }));
        let lowered = expand_logical_alb_source_with_opaque_x_interfaces(
            original,
            &found
                .into_iter()
                .map(|contact| contact.boundary)
                .collect::<Vec<_>>(),
            "generated.inc",
            &interfaces,
        )?;
        assert!(
            lowered
                .deck
                .contains("Xinput analog_input inner_signal ANALOG_LINK")
        );
        assert!(
            lowered
                .deck
                .contains("Xoutput inner_output analog_output ANALOG_LINK")
        );
        assert!(lowered.deck.contains("SR_AUTO_INNER_SIGNAL_ADC"));
        assert!(lowered.deck.contains("SR_AUTO_INNER_OUTPUT_DAC"));
        Ok(())
    }

    #[test]
    fn undeclared_opaque_x_wiring_never_guesses_analog_ownership() -> Result<()> {
        let source =
            include_str!("../../../../../runtime/xspice/fixtures/family/logical-opaque-x.cir");
        let found = discover_original_alb_boundaries(source, 5.0)?;
        assert!(
            found.iter().all(|item| {
                item.boundary.analog_net != "INNER_SIGNAL"
                    && item.boundary.analog_net != "INNER_OUTPUT"
            }),
            "{found:?}"
        );
        let err = expand_logical_alb_source(
            source,
            &found.into_iter().map(|c| c.boundary).collect::<Vec<_>>(),
            "generated.inc",
        )
        .err()
        .context("unverified external X pin attached to a digital net must fail closed")?;
        assert!(format!("{err:#}").contains("unresolved external X instance"));
        Ok(())
    }

    #[test]
    fn external_x_interface_validates_complete_arity_and_ambiguous_declarations() -> Result<()> {
        let link = analog_link();
        let decoded = external_x_pins(
            "Xsignal in out analog_link params: K=2",
            &opaque_x_interfaces(std::slice::from_ref(&link))?,
        )?;
        assert_eq!(
            decoded,
            vec![
                ("IN".into(), OpaqueXPinKind::Analog),
                ("OUT".into(), OpaqueXPinKind::Analog),
            ]
        );
        let source =
            include_str!("../../../../../runtime/xspice/fixtures/family/logical-opaque-x.cir");
        let bad_arity = source.replace(
            "Xinput analog_input inner_signal ANALOG_LINK",
            "Xinput analog_input inner_signal extra ANALOG_LINK",
        );
        let err = discover_original_alb_boundaries_with_opaque_x_interfaces(
            &bad_arity,
            std::slice::from_ref(&link),
            5.0,
        )
        .err()
        .context("incorrect external pin arity must be diagnosed")?;
        assert!(format!("{err:#}").contains("actual pins"), "{err:#}");
        let err = discover_original_alb_boundaries_with_opaque_x_interfaces(
            source,
            &[link.clone(), link],
            5.0,
        )
        .err()
        .context("ambiguous duplicate external interface must fail")?;
        assert!(format!("{err:#}").contains("duplicate opaque X interface"));
        Ok(())
    }

    #[test]
    fn external_digital_x_pin_does_not_create_fake_analog_contact() -> Result<()> {
        let source = r#"
Vin a 0 1.5
Xexternal a internal MIXED_DEVICE
A1 [internal] [q] DL
Rload q 0 100k
.model DL d_logic_block family="HC" def="out[0]=in[0];"
.end
"#;
        let external = [OpaqueXInterface {
            model: "MIXED_DEVICE".into(),
            pins: vec![OpaqueXPinKind::Analog, OpaqueXPinKind::Digital],
            native_model: None,
            native_formals: Vec::new(),
        }];
        let err = discover_original_alb_boundaries_with_opaque_x_interfaces(source, &external, 5.0)
            .err()
            .context("external digital pin may not be mistaken for an analog witness")?;
        assert!(
            format!("{err:#}").contains("native digital X interface requires"),
            "{err:#}"
        );
        Ok(())
    }
}

#[cfg(test)]
mod external_native_digital_tests {
    use super::*;
    use crate::whole::expand_logical_alb_source_with_opaque_x_interfaces;

    fn model(native: Option<&str>) -> OpaqueXInterface {
        OpaqueXInterface {
            model: "LEGACY_DIGITAL_BUFFER".into(),
            pins: vec![OpaqueXPinKind::Digital, OpaqueXPinKind::Digital],
            native_model: native.map(str::to_owned),
            native_formals: if native.is_some() {
                ["D", "D_SINK", "D_SOURCE", "Q", "Q_SINK", "Q_SOURCE"]
                    .into_iter()
                    .map(str::to_owned)
                    .collect()
            } else {
                Vec::new()
            },
        }
    }

    #[test]
    fn verified_native_x_rewrites_each_digital_pin_as_full_event_bundle() -> Result<()> {
        let source = include_str!(
            "../../../../../runtime/xspice/fixtures/family/logical-native-digital-x.cir"
        );
        let interfaces = [model(Some("SR_NATIVE_DIGITAL_BUFFER"))];
        let found =
            discover_original_alb_boundaries_with_opaque_x_interfaces(source, &interfaces, 5.0)?;
        assert_eq!(found.len(), 2, "{found:?}");
        let rendered = expand_logical_alb_source_with_opaque_x_interfaces(
            source,
            &found.into_iter().map(|x| x.boundary).collect::<Vec<_>>(),
            "native.inc",
            &interfaces,
        )?;
        assert!(
            rendered.deck.contains(
                "Xexternal FIRST FIRST_SR_SINK FIRST_SR_SOURCE SECOND SECOND_SR_SINK SECOND_SR_SOURCE SR_NATIVE_DIGITAL_BUFFER"
            ),
            "{}",
            rendered.deck
        );
        assert!(
            rendered
                .deck
                .contains(".include \"native-external-buffer.lib\"")
        );
        Ok(())
    }

    #[test]
    fn verified_analog_only_native_x_preserves_physical_terminal_arity() -> Result<()> {
        let source =
            include_str!("../../../../../runtime/xspice/fixtures/family/logical-opaque-x.cir");
        let interfaces = [OpaqueXInterface {
            model: "ANALOG_LINK".into(),
            pins: vec![OpaqueXPinKind::Analog, OpaqueXPinKind::Analog],
            native_model: Some("SR_NATIVE_ANALOG_LINK".into()),
            native_formals: vec!["IN".into(), "OUT".into()],
        }];
        let found =
            discover_original_alb_boundaries_with_opaque_x_interfaces(source, &interfaces, 5.0)?;
        let rendered = expand_logical_alb_source_with_opaque_x_interfaces(
            source,
            &found.into_iter().map(|x| x.boundary).collect::<Vec<_>>(),
            "native.inc",
            &interfaces,
        )?;
        assert!(
            rendered
                .deck
                .contains("Xinput ANALOG_INPUT INNER_SIGNAL SR_NATIVE_ANALOG_LINK"),
            "{}",
            rendered.deck
        );
        assert!(
            rendered
                .deck
                .contains("Xoutput INNER_OUTPUT ANALOG_OUTPUT SR_NATIVE_ANALOG_LINK"),
            "{}",
            rendered.deck
        );
        assert!(
            !rendered
                .deck
                .contains("Xinput ANALOG_INPUT ANALOG_INPUT_SR"),
            "analog X formal must never be expanded to event triple"
        );
        Ok(())
    }

    #[test]
    fn native_event_pin_with_unbridged_physical_resistor_fails_closed() -> Result<()> {
        let source = include_str!(
            "../../../../../runtime/xspice/fixtures/family/logical-native-digital-x.cir"
        )
        .replace(
            "Xexternal first second LEGACY_DIGITAL_BUFFER",
            "Xexternal orphan second LEGACY_DIGITAL_BUFFER\nRorphan orphan 0 1k",
        );
        let interfaces = [model(Some("SR_NATIVE_DIGITAL_BUFFER"))];
        let found =
            discover_original_alb_boundaries_with_opaque_x_interfaces(&source, &interfaces, 5.0)?;
        let error = expand_logical_alb_source_with_opaque_x_interfaces(
            &source,
            &found
                .into_iter()
                .map(|item| item.boundary)
                .collect::<Vec<_>>(),
            "native.inc",
            &interfaces,
        )
        .err()
        .context("a native event triple must not be shorted to an analog resistor")?;
        assert!(
            format!("{error:#}").contains("unsplit physical analog contact"),
            "{error:#}"
        );
        Ok(())
    }

    #[test]
    fn no_verified_native_x_abi_cannot_use_digital_single_wire_ports() -> Result<()> {
        let source = include_str!(
            "../../../../../runtime/xspice/fixtures/family/logical-native-digital-x.cir"
        );
        let err =
            discover_original_alb_boundaries_with_opaque_x_interfaces(source, &[model(None)], 5.0)
                .err()
                .context("digital external ports require an explicit native triple ABI")?;
        assert!(format!("{err:#}").contains("requires an explicit compatible bundle"));
        Ok(())
    }
}
