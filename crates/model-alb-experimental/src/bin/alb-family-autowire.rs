//! Derive physical ALB bundle wiring directly from hierarchical SPICE
//! instance connectivity, then insert explicit native family bridges.
//! Production `D_LOGIC_BLOCK` translation is not enabled.

use anyhow::{Context, Result, bail, ensure};
use model_alb_experimental::{
    AlbOutputProducer, AnalogBridgeKind, AnalogDigitalConnection, BridgeCatalog, BridgeModel,
    DigitalPinDirection, LogicFamily, LogicFamilyAssignment, NativeFamilySpecification,
    OccurrenceFamilyBoundary, ScopedAnalogDigitalConnection, materialize_scoped_family_bridges,
    specialize_family_occurrences,
};
use serde::Deserialize;
use std::{collections::BTreeSet, env, fs, path::Path};

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Direction {
    Input,
    Output,
}
impl From<Direction> for DigitalPinDirection {
    fn from(value: Direction) -> Self {
        match value {
            Direction::Input => Self::Input,
            Direction::Output => Self::Output,
        }
    }
}
#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum BridgeKind {
    Adc,
    Dac,
}
impl From<BridgeKind> for AnalogBridgeKind {
    fn from(value: BridgeKind) -> Self {
        match value {
            BridgeKind::Adc => Self::Adc,
            BridgeKind::Dac => Self::Dac,
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Mapping {
    #[serde(default)]
    scope: Vec<String>,
    occurrence: Option<Vec<String>>,
    producer: Option<AlbOutputProducer>,
    analog_net: String,
    digital_net: String,
    direction: Direction,
    #[serde(default)]
    family: Option<String>,
    #[serde(default)]
    in_family: Option<String>,
    #[serde(default)]
    out_family: Option<String>,
    supply_v: f64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Bridge {
    name: String,
    kind: BridgeKind,
    family: String,
    nominal_supply_v: f64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    connections: Vec<Mapping>,
    bridge_models: Vec<Bridge>,
}

/// A preexisting analog/digital bridge on a selected root digital state
/// would duplicate the very physical interface this pass is inserting.
/// Recognize the known XSPICE ADC/DAC model families rather than assuming
/// every A device is a bridge.
fn reject_existing_bridges(source: &str, connections: &[AnalogDigitalConnection]) -> Result<()> {
    let selected = connections
        .iter()
        .map(|c| c.digital_net.to_ascii_uppercase())
        .collect::<BTreeSet<_>>();
    let bridge_models = source
        .lines()
        .filter_map(|line| {
            let tokens = line
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .split_ascii_whitespace()
                .collect::<Vec<_>>();
            if tokens.len() < 3 || !tokens[0].eq_ignore_ascii_case(".model") {
                return None;
            }
            let kind = tokens[2].split('(').next()?.to_ascii_uppercase();
            matches!(
                kind.as_str(),
                "ADC_BRIDGE" | "DAC_BRIDGE" | "ADC_SCHMITT" | "SR_FAMILY_ADC" | "SR_FAMILY_DAC"
            )
            .then(|| tokens[1].to_ascii_uppercase())
        })
        .collect::<BTreeSet<_>>();
    if bridge_models.is_empty() {
        return Ok(());
    }
    for line in source.lines() {
        let tokens = line
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .split_ascii_whitespace()
            .collect::<Vec<_>>();
        if tokens.len() < 3 || !tokens[0].to_ascii_uppercase().starts_with('A') {
            continue;
        }
        if !tokens
            .last()
            .is_some_and(|model| bridge_models.contains(&model.to_ascii_uppercase()))
        {
            continue;
        }
        for item in tokens.iter().skip(1) {
            let name = item
                .trim_matches(['[', ']', '(', ')', ','])
                .to_ascii_uppercase();
            if selected.contains(&name) {
                bail!(
                    "source already contains a physical analog/digital bridge on {name}; refusing duplicate insertion"
                );
            }
        }
    }
    Ok(())
}

fn run() -> Result<()> {
    let args = env::args_os().skip(1).collect::<Vec<_>>();
    if args.len() != 5 {
        bail!(
            "usage: alb-family-autowire SOURCE.cir COMPILED_ALB.inc BOUNDARIES.json NATIVE_MODELS.json OUTPUT.cir"
        );
    }
    let source = fs::read_to_string(&args[0]).context("reading complete source SPICE deck")?;
    let compiled = fs::read_to_string(&args[1]).context("reading compiled ALB declarations")?;
    let config: Config = serde_json::from_slice(&fs::read(&args[2])?)
        .context("decoding source analog/digital boundary annotations")?;
    let native: NativeFamilySpecification = serde_json::from_slice(&fs::read(&args[3])?)
        .context("decoding exact native family electrical models")?;
    ensure!(
        native.pins.is_empty(),
        "automatic wiring accepts model parameters only, not handwritten physical pin triples"
    );
    let mut connections = Vec::new();
    let mut occurrences = Vec::new();
    for c in config.connections {
        if let Some(producer) = &c.producer {
            ensure!(
                !producer.path.is_empty() && matches!(c.direction, Direction::Output),
                "physical ALB producer requires an output boundary and nonempty path"
            );
        }
        let boundary = AnalogDigitalConnection {
            analog_net: c.analog_net,
            digital_net: c.digital_net,
            direction: c.direction.into(),
            supply_v: c.supply_v,
            family: LogicFamilyAssignment {
                family: c.family,
                in_family: c.in_family,
                out_family: c.out_family,
            },
        };
        if let Some(occurrence) = c.occurrence {
            occurrences.push(OccurrenceFamilyBoundary {
                occurrence,
                scope: c.scope,
                boundary,
            });
        } else {
            connections.push(ScopedAnalogDigitalConnection {
                scope: c.scope,
                boundary,
            });
        }
    }
    let specialized = if occurrences.is_empty() {
        None
    } else {
        Some(specialize_family_occurrences(
            &source,
            &occurrences,
            &connections,
        )?)
    };
    let source = if let Some(result) = specialized {
        connections = result.connections;
        result.deck
    } else {
        source
    };
    let roots = connections
        .iter()
        .filter(|entry| entry.scope.is_empty())
        .map(|entry| entry.boundary.clone())
        .collect::<Vec<_>>();
    reject_existing_bridges(&source, &roots)?;
    let mut catalog = BridgeCatalog::default();
    for model in config.bridge_models {
        catalog.add(BridgeModel {
            name: model.name,
            kind: model.kind.into(),
            family: LogicFamily::parse(&model.family)?,
            nominal_supply_v: model.nominal_supply_v,
        })?;
    }
    let assembled =
        materialize_scoped_family_bridges(&source, &[&compiled], &connections, &catalog, &native)?;
    fs::write(Path::new(&args[4]), assembled).context("writing complete root native SPICE deck")?;
    Ok(())
}
fn main() -> Result<()> {
    run()
}
