//! Exact electrical specialization of ALB subcircuit occurrences.
//! Original lexical definitions and their local model contexts stay intact.

mod recursive;

use super::{Graph, Item, Result, key, parse_items};
use crate::alb::{AnalogDigitalConnection, DigitalPinDirection, ScopedAnalogDigitalConnection};
use anyhow::ensure;
use std::collections::BTreeSet;

/// Assign a physical bridge to a path of actual X instances from the root.
#[derive(Clone, Debug)]
pub struct OccurrenceFamilyBoundary {
    pub occurrence: Vec<String>,
    pub scope: Vec<String>,
    pub boundary: AnalogDigitalConnection,
}

/// Specialized electrical deck and its definition-local native bridge plan.
#[derive(Clone, Debug)]
pub struct FamilySpecialization {
    pub deck: String,
    pub connections: Vec<ScopedAnalogDigitalConnection>,
}

type ElectricalKey = (
    String,
    String,
    u8,
    Option<String>,
    Option<String>,
    Option<String>,
    u64,
);
type Signature = Vec<ElectricalKey>;

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct VariantKey {
    local: Option<Signature>,
    children: Vec<(String, Vec<String>, Box<Self>)>,
}

fn signature(entries: &[OccurrenceFamilyBoundary]) -> Result<Signature> {
    let mut values = Vec::with_capacity(entries.len());
    for entry in entries {
        let c = &entry.boundary;
        ensure!(
            c.supply_v.is_finite() && c.supply_v > 0.0,
            "instance supply must be finite and positive"
        );
        let direction = match c.direction {
            DigitalPinDirection::Input => 0,
            DigitalPinDirection::Output => 1,
        };
        values.push((
            key(&c.analog_net),
            key(&c.digital_net),
            direction,
            c.family.family.as_ref().map(|s| key(s)),
            c.family.in_family.as_ref().map(|s| key(s)),
            c.family.out_family.as_ref().map(|s| key(s)),
            c.supply_v.to_bits(),
        ));
    }
    values.sort();
    ensure!(
        values.iter().collect::<BTreeSet<_>>().len() == values.len(),
        "duplicate electrical boundary for one X occurrence"
    );
    let topological = values
        .iter()
        .map(|v| (&v.0, &v.1, v.2))
        .collect::<BTreeSet<_>>();
    ensure!(
        topological.len() == values.len(),
        "two conflicting electrical assignments target the same physical boundary"
    );
    Ok(values)
}

/// Specialize reachable X occurrences by their entire nested electrical
/// signature, preserving lexical nested models and the original ALB library.
///
/// # Errors
/// Rejects unknown or duplicated X paths, recursive subcircuits, unmatched
/// assignments, conflicting physical role signatures or name collisions.
pub fn specialize_family_occurrences(
    deck: &str,
    occurrence_connections: &[OccurrenceFamilyBoundary],
    lexical_connections: &[ScopedAnalogDigitalConnection],
) -> Result<FamilySpecialization> {
    ensure!(
        !occurrence_connections.is_empty(),
        "occurrence specialization requires actual occurrence assignments"
    );
    let items = parse_items(deck)?;
    let mut graph = Graph::default();
    graph.collect(&items, &[], None)?;
    recursive::run(&graph, &items, occurrence_connections, lexical_connections)
}
