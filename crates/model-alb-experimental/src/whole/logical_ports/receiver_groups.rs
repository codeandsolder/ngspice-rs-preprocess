//! Separate ADC routes by actual input occurrences without duplicating
//! receiver loads or specializing reusable X definitions.
use super::{
    AlbInputReceiver, DigitalPinDirection, Graph, LogicalAlbBoundary, ProducerSlot, Result,
    ScopeRenames, SourceOccurrence, allocate_bundle, key, new_promoted_name, reserved,
    valid_analog_node, valid_node,
};
use anyhow::{Context, ensure};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(super) struct ReceiverElectrical {
    family: String,
    supply: Option<u64>,
}

pub(super) struct ResolvedReceiver {
    pub(super) identity: AlbInputReceiver,
    pub(super) model: usize,
}

impl ResolvedReceiver {
    fn electrical(&self, graph: &Graph) -> ReceiverElectrical {
        let model = &graph.models[self.model];
        ReceiverElectrical {
            family: model.in_family.clone(),
            supply: model.supply_v.map(f64::to_bits),
        }
    }
}

/// Every input bit, even repeated input nodes on one A instance, owns
/// exactly one physical metadata load. Retain actual X occurrence paths.
pub(super) fn receiver_paths(
    graph: &Graph,
    scope: &[String],
    node: &str,
) -> Result<Vec<ResolvedReceiver>> {
    let mut found = graph
        .pin_paths(scope, node, DigitalPinDirection::Input, &mut Vec::new())?
        .into_iter()
        .map(|pin| ResolvedReceiver {
            identity: AlbInputReceiver {
                path: pin.path,
                input_pin: pin.bit,
                analog_hops: pin.analog_hops,
            },
            model: pin.model,
        })
        .collect::<Vec<_>>();
    found.sort_by(|a, b| a.identity.cmp(&b.identity));
    ensure!(
        found
            .windows(2)
            .all(|pair| pair[0].identity != pair[1].identity),
        "multiple source paths to the same ALB receiver on {scope:?}/{node}"
    );
    Ok(found)
}

/// Physical wire crossings are explicit in the receiver route. Mark them
/// before logical role propagation; later complete partition validation
/// proves every annotated receiver reachable through those same formals.
pub(super) fn mark_lifted_formals(
    graph: &mut Graph,
    boundaries: &[LogicalAlbBoundary],
) -> Result<()> {
    let mut lifted = BTreeSet::new();
    for boundary in boundaries {
        for receiver in &boundary.receivers {
            if receiver.analog_hops.is_empty() {
                continue;
            }
            let mut scope = boundary
                .scope
                .iter()
                .map(|part| key(part))
                .collect::<Vec<_>>();
            ensure!(
                receiver
                    .analog_hops
                    .keys()
                    .all(|index| *index < receiver.path.len().saturating_sub(1)),
                "ADC analog hop must name an X-interface crossing"
            );
            for (index, instance) in receiver
                .path
                .iter()
                .take(receiver.path.len().saturating_sub(1))
                .enumerate()
            {
                let mut matches = graph
                    .occurrences(&scope)?
                    .calls()
                    .filter(|call| call.name.eq_ignore_ascii_case(instance));
                let matched = matches.next().with_context(|| {
                    format!("missing lifted ADC X instance {instance} in {scope:?}")
                })?;
                ensure!(
                    matches.next().is_none(),
                    "ambiguous lifted ADC X instance {instance} in {scope:?}"
                );
                scope = matched.target.clone();
                if let Some(formal) = receiver.analog_hops.get(&index) {
                    let formal = valid_node(formal)?;
                    let child = graph
                        .scopes
                        .get(&scope)
                        .context("missing lifted ADC child")?;
                    ensure!(
                        child.ports.contains(&formal),
                        "lifted ADC route crosses unknown formal {scope:?}/{formal}"
                    );
                    lifted.insert((scope.clone(), formal));
                }
            }
        }
    }
    for (scope, formal) in lifted {
        ensure!(
            !boundaries
                .iter()
                .any(|b| b.direction == DigitalPinDirection::Input
                    && b.scope
                        .iter()
                        .map(|part| key(part))
                        .eq(scope.iter().cloned())
                    && key(&b.analog_net) == formal),
            "local and lifted ADC both claim {scope:?}/{formal}"
        );
        let child = graph
            .scopes
            .get_mut(&scope)
            .context("missing lifted ADC scope")?;
        child.analog_cut_formals.insert(formal.clone());
        child.lifted_adc_formals.insert(formal);
    }
    Ok(())
}

/// All domains on a physically lifted wire require explicit routes, even
/// a domain whose particular receiver happens to reside in the owner.
pub(super) fn lifted_nodes(boundaries: &[LogicalAlbBoundary]) -> BTreeSet<(Vec<String>, String)> {
    boundaries
        .iter()
        .filter(|b| b.receivers.iter().any(|r| !r.analog_hops.is_empty()))
        .map(|b| {
            (
                b.scope.iter().map(|part| key(part)).collect(),
                key(&b.analog_net),
            )
        })
        .collect()
}

#[derive(Default)]
pub(super) struct ReceiverPlan {
    /// Each selected group shares a single digital state and metadata pair.
    pub(super) states: BTreeSet<String>,
    pub(super) routes: BTreeMap<(String, String, usize), String>,
    /// Stable ordered electrical input groups promoted through X formals.
    pub(super) formals: BTreeMap<String, BTreeMap<ReceiverElectrical, String>>,
    /// Analog cut nodes with more than one receiver group.
    pub(super) cuts: BTreeSet<String>,
}

#[derive(Clone, Default)]
pub(super) struct ReceiverBundles {
    pub(super) routes: BTreeMap<ProducerSlot, [String; 3]>,
    pub(super) formals: BTreeMap<String, Vec<[String; 3]>>,
}

impl ReceiverPlan {
    pub(super) fn build_bundles(
        &self,
        physical: &BTreeMap<String, [String; 3]>,
        reserved: &mut BTreeSet<String>,
        used_states: &mut BTreeSet<String>,
        mut ordinal: usize,
    ) -> Result<ReceiverBundles> {
        if self.routes.is_empty() && self.formals.is_empty() {
            return Ok(ReceiverBundles::default());
        }
        let mut receiver_bundles = physical
            .values()
            .map(|bundle| (bundle[0].clone(), bundle.clone()))
            .collect::<BTreeMap<_, _>>();
        for state in &self.states {
            let state = valid_node(state)?;
            ensure!(
                used_states.insert(state.clone()),
                "receiver state {state} overlaps another physical bundle"
            );
            receiver_bundles.insert(state.clone(), allocate_bundle(&state, reserved, ordinal)?);
            ordinal += 1;
        }
        let resolve_receiver = |state: &String| {
            receiver_bundles
                .get(state)
                .cloned()
                .with_context(|| format!("missing receiver state {state}"))
        };
        let receiver_routes = self
            .routes
            .iter()
            .map(|(slot, state)| Ok((slot.clone(), resolve_receiver(state)?)))
            .collect::<Result<BTreeMap<_, _>>>()?;
        let promoted_inputs = self
            .formals
            .iter()
            .map(|(formal, groups)| {
                Ok((
                    formal.clone(),
                    groups
                        .values()
                        .map(resolve_receiver)
                        .collect::<Result<Vec<_>>>()?,
                ))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        Ok(ReceiverBundles {
            routes: receiver_routes,
            formals: promoted_inputs,
        })
    }
}

type Assignment = BTreeMap<AlbInputReceiver, String>;

/// Promote reusable input-only formals by their lexical model electrical
/// identity, independent of any occurrence's fallback supply. Multiple
/// promoted formals can share the same actual triple at another call site.
fn promote(
    graph: &Graph,
    scope: &[String],
    formal: &str,
    plans: &mut BTreeMap<Vec<String>, ScopeRenames>,
) -> Result<BTreeMap<ReceiverElectrical, String>> {
    if let Some(previous) = plans
        .get(scope)
        .and_then(|p| p.receivers.formals.get(formal))
    {
        return Ok(previous.clone());
    }
    let owner = graph
        .scopes
        .get(scope)
        .context("missing promoted receiver scope")?;
    let lifted = owner.lifted_adc_formals.contains(formal);
    ensure!(
        owner.ports.iter().any(|p| p == formal)
            && (lifted
                || (!owner.analog_cut_formals.contains(formal)
                    && owner
                        .alb_roles
                        .get(formal)
                        .is_some_and(|r| r.len() == 1 && r.contains(&DigitalPinDirection::Input)))),
        "heterogeneous receiver interface {scope:?}/{formal} is not a pure digital input"
    );
    let found = receiver_paths(graph, scope, formal)?;
    let electrical = found
        .iter()
        .map(|r| r.electrical(graph))
        .collect::<BTreeSet<_>>();
    ensure!(
        !electrical.is_empty() && (lifted || electrical.len() > 1),
        "cannot split receivers with the same lexical electrical identity on {scope:?}/{formal}"
    );
    let plan = plans.entry(scope.to_vec()).or_default();
    let mut groups = BTreeMap::new();
    for (index, kind) in electrical.into_iter().enumerate() {
        let state = if index == 0 && !lifted {
            formal.to_owned()
        } else {
            let state =
                new_promoted_name(owner, plan, "SR_PROMOTED_IN", plan.receivers.states.len())?;
            plan.receivers.states.insert(state.clone());
            state
        };
        groups.insert(kind, state);
    }
    if lifted {
        plan.receivers.cuts.insert(formal.to_owned());
    }
    plan.receivers
        .formals
        .insert(formal.to_owned(), groups.clone());
    let assignment = found
        .into_iter()
        .map(|r| {
            let state = groups
                .get(&r.electrical(graph))
                .context("missing promoted receiver group")?
                .clone();
            Ok((r.identity, state))
        })
        .collect::<Result<Assignment>>()?;
    route(graph, scope, formal, &assignment, plans)?;
    Ok(groups)
}

fn bind(
    plans: &mut BTreeMap<Vec<String>, ScopeRenames>,
    scope: &[String],
    slot: (String, String, usize),
    state: String,
) -> Result<()> {
    let routes = &mut plans.entry(scope.to_vec()).or_default().receivers.routes;
    match routes.entry(slot) {
        std::collections::btree_map::Entry::Vacant(entry) => {
            entry.insert(state);
        }
        std::collections::btree_map::Entry::Occupied(entry) => {
            ensure!(
                entry.get() == &state,
                "conflicting receiver routes for {scope:?}/{:?}",
                entry.key()
            );
        }
    }
    Ok(())
}

/// Route one complete set of ALB input groups through direct pins and
/// nested X interfaces. Only an interface carrying multiple distinct
/// domain states needs promotion.
fn route(
    graph: &Graph,
    scope: &[String],
    node: &str,
    assignment: &Assignment,
    plans: &mut BTreeMap<Vec<String>, ScopeRenames>,
) -> Result<()> {
    for occurrence in &graph.occurrences(scope)?.ordered {
        if let SourceOccurrence::Alb(alb) = occurrence {
            for (bit, input) in alb.inputs.iter().enumerate() {
                if input == node {
                    let identity = AlbInputReceiver {
                        path: vec![key(&alb.name)],
                        input_pin: bit,
                        analog_hops: BTreeMap::new(),
                    };
                    let state = assignment
                        .get(&identity)
                        .context("missing direct ALB receiver assignment")?;
                    bind(
                        plans,
                        scope,
                        (key(&alb.name), input.clone(), bit),
                        state.clone(),
                    )?;
                }
            }
        } else if let SourceOccurrence::Call(call) = occurrence {
            let child = graph
                .scopes
                .get(&call.target)
                .context("missing input route child")?;
            for (formal, actual) in child.ports.iter().zip(&call.actual) {
                let lifted = child.lifted_adc_formals.contains(formal);
                if actual != node || (child.analog_cut_formals.contains(formal) && !lifted) {
                    continue;
                }
                let found = receiver_paths(graph, &call.target, formal)?;
                if found.is_empty() {
                    ensure!(
                        !child.digital.contains(formal)
                            || child
                                .alb_roles
                                .get(formal)
                                .is_some_and(|roles| roles.len() == 1
                                    && roles.contains(&DigitalPinDirection::Output)),
                        "heterogeneous receiver route has an untyped digital formal {formal} of {}",
                        call.name
                    );
                    continue;
                }
                let directions = child.alb_roles.get(formal);
                ensure!(
                    lifted
                        || directions.is_some_and(
                            |r| r.len() == 1 && r.contains(&DigitalPinDirection::Input)
                        ),
                    "heterogeneous receiver route crosses mixed-direction formal {formal} of {}",
                    call.name
                );
                let mut groups = BTreeMap::<ReceiverElectrical, String>::new();
                for receiver in found {
                    let mut identity = receiver.identity.clone();
                    identity.prepend(key(&call.name), lifted.then(|| formal.clone()));
                    let state = assignment
                        .get(&identity)
                        .context("missing nested ALB receiver assignment")?;
                    if let Some(old) = groups.insert(receiver.electrical(graph), state.clone()) {
                        ensure!(
                            old == *state,
                            "identical lexical input group has conflicting states on {}",
                            call.name
                        );
                    }
                }
                let distinct = groups.values().collect::<BTreeSet<_>>();
                if distinct.len() == 1 && !lifted {
                    let state = groups.into_values().next().context("missing input group")?;
                    bind(plans, scope, (key(&call.name), formal.clone(), 0), state)?;
                } else {
                    let promoted = promote(graph, &call.target, formal, plans)?;
                    for (index, kind) in promoted.keys().enumerate() {
                        let state = groups
                            .get(kind)
                            .context("promoted input group is not routed")?;
                        bind(
                            plans,
                            scope,
                            (key(&call.name), formal.clone(), index),
                            state.clone(),
                        )?;
                    }
                }
            }
        }
    }
    Ok(())
}

/// Validate that the selected groups partition all actual receiver
/// occurrences at each analog boundary before constructing any routes.
pub(super) fn plan(
    graph: &Graph,
    boundaries: &[LogicalAlbBoundary],
    plans: &mut BTreeMap<Vec<String>, ScopeRenames>,
) -> Result<()> {
    let mut groups = BTreeMap::<(Vec<String>, String), Vec<&LogicalAlbBoundary>>::new();
    for boundary in boundaries.iter().filter(|b| !b.receivers.is_empty()) {
        ensure!(
            boundary.direction == DigitalPinDirection::Input && boundary.producer.is_none(),
            "receiver occurrences require an ADC boundary"
        );
        groups
            .entry((
                boundary.scope.iter().map(|s| key(s)).collect(),
                valid_analog_node(&boundary.analog_net)?,
            ))
            .or_default()
            .push(boundary);
    }
    let lifted = lifted_nodes(boundaries);
    for ((scope, analog), entries) in groups {
        let cross_cut = lifted.contains(&(scope.clone(), analog.clone()));
        let owner = graph
            .scopes
            .get(&scope)
            .context("missing receiver cut scope")?;
        let primary = if cross_cut {
            None
        } else {
            Some(
                plans
                    .get(&scope)
                    .and_then(|p| p.states.get(&analog))
                    .context("missing primary receiver state")?
                    .clone(),
            )
        };
        let mut assignment = Assignment::new();
        let mut states = BTreeSet::new();
        for entry in entries {
            let state = valid_node(&entry.digital_net)?;
            ensure!(
                state != analog
                    && !reserved(owner).contains(&state)
                    && states.insert(state.clone()),
                "invalid or duplicate selected receiver state {state}"
            );
            for receiver in &entry.receivers {
                let identity = AlbInputReceiver {
                    path: receiver
                        .path
                        .iter()
                        .map(|p| valid_node(p))
                        .collect::<Result<Vec<_>>>()?,
                    input_pin: receiver.input_pin,
                    analog_hops: receiver
                        .analog_hops
                        .iter()
                        .map(|(index, formal)| Ok((*index, valid_node(formal)?)))
                        .collect::<Result<BTreeMap<_, _>>>()?,
                };
                ensure!(
                    assignment.insert(identity.clone(), state.clone()).is_none(),
                    "duplicate selected receiver {identity:?}"
                );
            }
        }
        let actual = receiver_paths(graph, &scope, &analog)?
            .into_iter()
            .map(|r| r.identity)
            .collect::<BTreeSet<_>>();
        ensure!(
            actual == assignment.keys().cloned().collect(),
            "receiver groups do not exactly partition {scope:?}/{analog}"
        );
        if let Some(primary) = primary {
            ensure!(
                states.len() > 1 && states.remove(&primary),
                "receiver cut must select multiple states including its primary"
            );
        }
        let plan = plans.entry(scope.clone()).or_default();
        plan.receivers.cuts.insert(analog.clone());
        for state in states {
            ensure!(
                plan.receivers.states.insert(state),
                "receiver state selected for multiple analog cuts in {scope:?}"
            );
        }
        route(graph, &scope, &analog, &assignment, plans)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{discover_original_alb_boundaries, expand_logical_alb_source};

    #[test]
    fn original_cross_cut_receivers_share_only_actual_analog_nets() -> Result<()> {
        for (source, groups) in [
            (
                include_str!(
                    "../../../../../runtime/xspice/fixtures/vendor-cross-cut-receivers/same-domain.net"
                ),
                vec![3],
            ),
            (
                include_str!(
                    "../../../../../runtime/xspice/fixtures/vendor-cross-cut-receivers/mixed-domain.net"
                ),
                vec![1, 2],
            ),
            (
                include_str!(
                    "../../../../../runtime/xspice/fixtures/vendor-cross-cut-receivers/independent-nets.net"
                ),
                vec![2, 1],
            ),
        ] {
            let discovered = discover_original_alb_boundaries(source, 5.0)?;
            let inputs = discovered
                .iter()
                .filter(|entry| entry.boundary.direction == DigitalPinDirection::Input)
                .collect::<Vec<_>>();
            assert_eq!(
                inputs
                    .iter()
                    .map(|entry| entry.boundary.receivers.len())
                    .collect::<Vec<_>>(),
                groups
            );
            assert!(inputs.iter().all(|entry| entry.boundary.scope.is_empty()));
            let boundaries = discovered
                .into_iter()
                .map(|entry| entry.boundary)
                .collect::<Vec<_>>();
            let expanded = expand_logical_alb_source(source, &boundaries, "native.inc")?;
            assert_eq!(expanded.deck.matches(".subckt INNER ").count(), 1);
            assert!(expanded.deck.contains("Rlocal p 0 1meg"));
            assert!(expanded.deck.contains("XSR_AINNER SR_PROMOTED_IN_"));
            assert!(expanded.deck.contains("Xone P "));
            let mut duplicate = boundaries.clone();
            let mut implicit = duplicate
                .iter()
                .find(|entry| !entry.receivers.is_empty())
                .context("missing selected receiver group")?
                .clone();
            implicit.receivers.clear();
            implicit.digital_net = "UNCLAIMED_ADC".to_owned();
            duplicate.push(implicit);
            assert!(expand_logical_alb_source(source, &duplicate, "native.inc").is_err());
            let mut invalid = boundaries;
            let route = invalid
                .iter_mut()
                .flat_map(|entry| &mut entry.receivers)
                .find(|receiver| !receiver.analog_hops.is_empty())
                .context("missing scalar route")?;
            route.analog_hops.clear();
            assert!(expand_logical_alb_source(source, &invalid, "native.inc").is_err());
        }
        Ok(())
    }

    #[test]
    fn grounded_scalar_occurrence_is_an_analog_contact_not_a_digital_ground() -> Result<()> {
        let source = include_str!(
            "../../../../../runtime/xspice/fixtures/vendor-cross-cut-receivers/same-domain.net"
        )
        .replace("Xtwo p q2 INNER", "Xtwo 0 q2 INNER");
        let discovered = discover_original_alb_boundaries(&source, 5.0)?;
        let ground = discovered
            .iter()
            .find(|entry| entry.boundary.analog_net == "0")
            .context("missing grounded input boundary")?;
        assert_eq!(ground.boundary.receivers[0].path, ["XTWO", "AINNER"]);
        let boundaries = discovered
            .into_iter()
            .map(|entry| entry.boundary)
            .collect::<Vec<_>>();
        let expanded = expand_logical_alb_source(&source, &boundaries, "native.inc")?;
        assert!(expanded.deck.contains("Xtwo 0 "));
        assert!(expanded.deck.contains("SR_AUTO_0_ADC"));
        Ok(())
    }

    #[test]
    fn scalar_output_cut_is_preserved_through_forwarding_only_wrapper() -> Result<()> {
        let source = r#"
Vinput d 0 5
Xwrap d p WRAP
Aroot [p] [q] M
Rout q 0 100k
.model M d_logic_block family="HC" def="out[0]=in[0];"
.subckt WRAP a b
Xinside a b INNER
.ends WRAP
.subckt INNER a b
Ainner [a] [b] M
Rinside b 0 1meg
.ends INNER
.end
"#;
        let discovered = discover_original_alb_boundaries(source, 5.0)?;
        assert!(
            discovered
                .iter()
                .any(|entry| entry.boundary.analog_net == "P"
                    && entry.boundary.direction == DigitalPinDirection::Input)
        );
        let boundaries = discovered
            .into_iter()
            .map(|entry| entry.boundary)
            .collect::<Vec<_>>();
        let expanded = expand_logical_alb_source(source, &boundaries, "native.inc")?;
        assert!(expanded.deck.contains(
            ".subckt WRAP A A_SR_SINK A_SR_SOURCE B
"
        ));
        assert!(
            expanded
                .deck
                .contains("Xinside A A_SR_SINK A_SR_SOURCE B INNER")
        );
        assert!(expanded.deck.contains("XSR_AROOT SR_AUTO_P_ADC"));
        Ok(())
    }

    #[test]
    fn child_physical_witness_requires_dac_for_parent_alb_driver() -> Result<()> {
        let source = r#"
Vinput d 0 5
Adrive [d] [p] M
Xwrap p q WRAP
Rout q 0 100k
.model M d_logic_block family="HC" def="out[0]=in[0];"
.subckt WRAP a b
Xinside a b INNER
.ends WRAP
.subckt INNER a b
Rinside a 0 1meg
Ainner [a] [b] M
.ends INNER
.end
"#;
        let discovered = discover_original_alb_boundaries(source, 5.0)?;
        let shared = discovered
            .iter()
            .filter(|entry| entry.boundary.analog_net == "P")
            .collect::<Vec<_>>();
        assert_eq!(shared.len(), 2);
        assert!(shared.iter().all(|entry| entry.boundary.scope.is_empty()));
        let boundaries = discovered
            .into_iter()
            .map(|entry| entry.boundary)
            .collect::<Vec<_>>();
        let expanded = expand_logical_alb_source(source, &boundaries, "native.inc")?;
        assert!(expanded.deck.contains("Xwrap P "));
        assert!(expanded.deck.contains(
            "XSR_ADRIVE SR_AUTO_D_ADC SR_AUTO_D_ADC_SR_SINK SR_AUTO_D_ADC_SR_SOURCE SR_AUTO_P_DAC"
        ));
        Ok(())
    }

    #[test]
    fn effective_supply_groups_resolve_default_before_partition() -> Result<()> {
        let source = r#"
V1 p 0 1.5
A1 [p] [q1] M1
A2 [p] [q2] M2
.model M1 d_logic_block family="HC" vsupply=5 def="out[0]=in[0];"
.model M2 d_logic_block family="HC" def="out[0]=in[0];"
.end
"#;
        let common = discover_original_alb_boundaries(source, 5.0)?;
        assert_eq!(common.len(), 1);
        assert_eq!(common[0].boundary.receivers, []);
        assert_eq!(common[0].supply_v.to_bits(), 5.0_f64.to_bits());
        let separate = discover_original_alb_boundaries(source, 3.3)?;
        assert_eq!(separate.len(), 2);
        assert!(
            separate
                .iter()
                .all(|group| group.boundary.receivers.len() == 1)
        );
        let boundaries = separate
            .into_iter()
            .map(|entry| entry.boundary)
            .collect::<Vec<_>>();
        assert!(
            expand_logical_alb_source(source, &boundaries, "native.inc")?
                .deck
                .contains("XSR_A1 SR_AUTO_P_ADC_1 ")
        );
        Ok(())
    }

    #[test]
    fn shared_domains_promote_reusable_formals_without_breaking_digital_caller() -> Result<()> {
        let original = include_str!(
            "../../../../../runtime/xspice/fixtures/vendor-receiver-groups/shared-receivers.net"
        );
        let source = original.replace(
            "Xtwo p q4 q5 q6 WRAP",
            "Xtwo p q4 q5 q6 WRAP\nAlogic [p] [d] M5\nXdigital d a b c WRAP",
        );
        let discovered = discover_original_alb_boundaries(&source, 5.0)?;
        let inputs = discovered
            .iter()
            .filter(|entry| entry.boundary.direction == DigitalPinDirection::Input)
            .collect::<Vec<_>>();
        assert_eq!(inputs.len(), 3);
        assert_eq!(
            inputs
                .iter()
                .map(|group| group.boundary.receivers.len())
                .collect::<Vec<_>>(),
            [2, 4, 2]
        );
        let boundaries = discovered
            .into_iter()
            .map(|entry| entry.boundary)
            .collect::<Vec<_>>();
        let expansion = expand_logical_alb_source(&source, &boundaries, "native.inc")?;
        let digital = expansion
            .deck
            .lines()
            .find(|line| line.starts_with("Xdigital "))
            .context("missing digital sibling")?;
        assert_eq!(
            digital.matches("D D_SR_SINK D_SR_SOURCE").count(),
            3,
            "{digital}"
        );
        assert_eq!(expansion.deck.matches(".subckt WRAP ").count(), 1);
        assert_eq!(expansion.deck.matches(".subckt INNER ").count(), 1);
        let mut duplicate = boundaries;
        let receiver = duplicate[0]
            .receivers
            .first()
            .context("missing first receiver")?
            .clone();
        duplicate[1].receivers.push(receiver);
        assert!(expand_logical_alb_source(&source, &duplicate, "native.inc").is_err());
        Ok(())
    }

    #[test]
    fn rejects_shared_extra_state_across_distinct_analog_cuts() -> Result<()> {
        let source = r#"
V1 p 0 1.5
V2 q 0 1.5
A1 [p] [out1] M1
A2 [p] [out2] M2
A3 [q] [out3] M1
A4 [q] [out4] M2
.model M1 d_logic_block family="HC" vsupply=3.3 def="out[0]=in[0];"
.model M2 d_logic_block family="HC" vsupply=5 def="out[0]=in[0];"
.end
"#;
        let mut boundaries = discover_original_alb_boundaries(source, 5.0)?
            .into_iter()
            .map(|entry| entry.boundary)
            .collect::<Vec<_>>();
        assert_eq!(boundaries.len(), 4);
        expand_logical_alb_source(source, &boundaries, "native.inc")?;
        boundaries[1].digital_net = "SHARED_EXTRA".to_owned();
        boundaries[3].digital_net = "SHARED_EXTRA".to_owned();
        assert!(expand_logical_alb_source(source, &boundaries, "native.inc").is_err());
        Ok(())
    }

    #[test]
    fn repeated_input_bits_and_outputs_on_one_physical_node_route_independently() -> Result<()> {
        let source = r#"
Vinput p 0 1.5
Vdata d 0 0
A1 [p p] [q1] M1
A2 [p] [q2] M2
Adriver [d] [p] M2
.model M1 d_logic_block family="HC" vsupply=3.3 def="out[0]=in[0]&in[1];"
.model M2 d_logic_block family="HC" vsupply=5 def="out[0]=in[0];"
.end
"#;
        let found = discover_original_alb_boundaries(source, 5.0)?;
        let group = found
            .iter()
            .find(|entry| {
                entry.boundary.analog_net == "P" && entry.supply_v.to_bits() == 3.3_f64.to_bits()
            })
            .context("missing 3.3V group")?;
        assert_eq!(
            group
                .boundary
                .receivers
                .iter()
                .map(|pin| pin.input_pin)
                .collect::<Vec<_>>(),
            [0, 1]
        );
        let boundaries = found
            .into_iter()
            .map(|entry| entry.boundary)
            .collect::<Vec<_>>();
        let deck = expand_logical_alb_source(source, &boundaries, "native.inc")?.deck;
        let a1 = deck
            .lines()
            .find(|line| line.starts_with("XSR_A1 "))
            .context("missing repeated receiver")?;
        assert_eq!(
            a1.matches("SR_AUTO_P_ADC SR_AUTO_P_ADC_SR_SINK SR_AUTO_P_ADC_SR_SOURCE")
                .count(),
            2
        );
        let driver = deck
            .lines()
            .find(|line| line.starts_with("XSR_ADRIVER "))
            .context("missing independent DAC")?;
        assert!(driver.contains("SR_AUTO_P_DAC SR_AUTO_P_DAC_SR_SINK SR_AUTO_P_DAC_SR_SOURCE"));
        Ok(())
    }
}
