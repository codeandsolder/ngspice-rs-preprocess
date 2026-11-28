//! Exact electrical specialization of ALB subcircuit occurrences.
//! Original lexical definitions and their local model contexts stay intact.

mod recursive;

use super::{
    AlbInputReceiver, AlbOutputProducer, Graph, Item, LogicalAlbBoundary, Result, key, parse_items,
};
use crate::alb::{AnalogDigitalConnection, DigitalPinDirection, ScopedAnalogDigitalConnection};
use anyhow::ensure;
use std::collections::BTreeSet;

/// Assign a physical bridge to a path of actual X instances from the root.
#[derive(Clone, Debug)]
pub struct OccurrenceFamilyBoundary {
    pub occurrence: Vec<String>,
    pub scope: Vec<String>,
    pub boundary: AnalogDigitalConnection,
    pub producer: Option<AlbOutputProducer>,
    pub receivers: Vec<AlbInputReceiver>,
}

/// Definition-local logical and electrical mapping, including any selected
/// original ALB producer. The producer is used during logical lowering.
#[derive(Clone, Debug)]
pub struct DefinitionFamilyBoundary {
    pub scope: Vec<String>,
    pub boundary: AnalogDigitalConnection,
    pub producer: Option<AlbOutputProducer>,
    pub receivers: Vec<AlbInputReceiver>,
}

/// Specialized original deck and its remapped logical and native bridge plans.
#[derive(Clone, Debug)]
pub struct FamilySpecialization {
    pub deck: String,
    pub connections: Vec<ScopedAnalogDigitalConnection>,
    pub logical_boundaries: Vec<LogicalAlbBoundary>,
}

/// Canonical logical cut identity. Producer identity is part of topology,
/// not merely an electrical option.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct LogicalCut {
    analog: String,
    digital: String,
    direction: DigitalPinDirection,
    producer: Option<AlbOutputProducer>,
    receivers: Vec<AlbInputReceiver>,
}

impl LogicalCut {
    fn new(
        boundary: &AnalogDigitalConnection,
        producer: Option<&AlbOutputProducer>,
        receivers: &[AlbInputReceiver],
    ) -> Self {
        let mut receivers = receivers
            .iter()
            .map(|pin| AlbInputReceiver {
                path: pin.path.iter().map(|part| key(part)).collect(),
                input_pin: pin.input_pin,
                analog_hops: pin
                    .analog_hops
                    .iter()
                    .map(|(index, formal)| (*index, key(formal)))
                    .collect(),
            })
            .collect::<Vec<_>>();
        receivers.sort();
        Self {
            analog: key(&boundary.analog_net),
            digital: key(&boundary.digital_net),
            direction: boundary.direction,
            producer: producer.map(|p| AlbOutputProducer {
                path: p.path.iter().map(|part| key(part)).collect(),
                output_pin: p.output_pin,
            }),
            receivers,
        }
    }

    fn at(&self, scope: &[String]) -> LogicalAlbBoundary {
        LogicalAlbBoundary {
            scope: scope.to_vec(),
            analog_net: self.analog.clone(),
            digital_net: self.digital.clone(),
            direction: self.direction,
            producer: self.producer.clone(),
            receivers: self.receivers.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct ElectricalKey {
    cut: LogicalCut,
    family: Option<String>,
    in_family: Option<String>,
    out_family: Option<String>,
    supply_bits: u64,
}

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
        values.push(ElectricalKey {
            cut: LogicalCut::new(c, entry.producer.as_ref(), &entry.receivers),
            family: c.family.family.as_ref().map(|s| key(s)),
            in_family: c.family.in_family.as_ref().map(|s| key(s)),
            out_family: c.family.out_family.as_ref().map(|s| key(s)),
            supply_bits: c.supply_v.to_bits(),
        });
    }
    values.sort();
    ensure!(
        values.iter().collect::<BTreeSet<_>>().len() == values.len(),
        "duplicate electrical boundary for one X occurrence"
    );
    let topological = values
        .iter()
        .map(|v| (&v.cut.analog, &v.cut.digital, v.cut.direction))
        .collect::<BTreeSet<_>>();
    ensure!(
        topological.len() == values.len(),
        "two conflicting electrical assignments target the same physical boundary"
    );
    Ok(values)
}

/// Specialize original logical X occurrences by their complete nested cut
/// and electrical signature, before any physical port expansion.
///
/// # Errors
/// Rejects unknown or duplicated X paths, recursive subcircuits, unmatched
/// assignments, conflicting physical role signatures or name collisions.
pub fn specialize_family_occurrences(
    deck: &str,
    occurrence_connections: &[OccurrenceFamilyBoundary],
    lexical_connections: &[DefinitionFamilyBoundary],
) -> Result<FamilySpecialization> {
    if occurrence_connections.is_empty() {
        return Ok(FamilySpecialization {
            deck: deck.to_owned(),
            connections: lexical_connections
                .iter()
                .map(|entry| ScopedAnalogDigitalConnection {
                    scope: entry.scope.clone(),
                    boundary: entry.boundary.clone(),
                })
                .collect(),
            logical_boundaries: lexical_connections
                .iter()
                .map(|entry| {
                    LogicalCut::new(&entry.boundary, entry.producer.as_ref(), &entry.receivers)
                        .at(&entry.scope)
                })
                .collect(),
        });
    }
    let items = parse_items(deck)?;
    let graph = Graph::new(&items, None)?;
    recursive::run(&graph, &items, occurrence_connections, lexical_connections)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alb::LogicFamilyAssignment;
    use anyhow::Context;

    #[test]
    fn nested_specialization_retains_unannotated_sibling_model() -> Result<()> {
        let deck = "Xtop a b c d TOP\n\
                    .subckt TOP a b c d\n\
                    Xfirst a b INNER\n\
                    Xsecond c d INNER\n\
                    .subckt INNER in out\n\
                    Rinside in out 1k\n\
                    .ends INNER\n\
                    .ends TOP\n\
                    .end\n";
        let boundary = OccurrenceFamilyBoundary {
            occurrence: vec!["XTOP".to_owned(), "XFIRST".to_owned()],
            scope: vec!["TOP".to_owned(), "INNER".to_owned()],
            producer: None,
            receivers: Vec::new(),
            boundary: AnalogDigitalConnection {
                analog_net: "in".to_owned(),
                digital_net: "out".to_owned(),
                direction: DigitalPinDirection::Input,
                family: LogicFamilyAssignment::default(),
                supply_v: 3.3,
            },
        };
        let specialized = specialize_family_occurrences(deck, &[boundary], &[])?;
        let rendered = &specialized.deck;
        assert!(rendered.contains("Xtop A B C D TOP_SRV0"), "{rendered}");
        assert!(rendered.contains("Xfirst A B INNER_SRV0"), "{rendered}");
        assert!(rendered.contains("Xsecond c d INNER"), "{rendered}");
        assert_eq!(rendered.matches(".subckt INNER ").count(), 1);
        assert_eq!(rendered.matches(".subckt INNER_SRV0 ").count(), 1);
        assert!(!rendered.contains(".subckt TOP a b c d"));
        assert_eq!(specialized.connections.len(), 1);
        let generated = specialized
            .connections
            .first()
            .context("missing specialized nested boundary")?;
        assert_eq!(generated.scope, ["TOP_SRV0", "INNER_SRV0"]);
        assert_eq!(generated.boundary.digital_net, "out");
        Ok(())
    }
}
