use anyhow::{Context, Result, ensure};
use model_alb_experimental::{
    AlbInputReceiver, AlbOutputProducer, AnalogBridgeKind, AnalogDigitalConnection, BridgeCatalog,
    BridgeModel, CodeModelContract, DefinitionFamilyBoundary, DigitalPinDirection, LogicFamily,
    LogicFamilyAssignment, NativeFamilySpecification, OccurrenceFamilyBoundary, OpaqueXInterface,
    ScopedAnalogDigitalConnection, materialize_scoped_family_bridges, verify_external_code_models,
    verify_external_native_libraries,
};
use serde::Deserialize;
use std::{collections::BTreeMap, fmt::Write as _, fs, path::Path};

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
    #[serde(default)]
    receivers: Vec<AlbInputReceiver>,
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
    /// Model-level interface data is consumed by discovery and logical
    /// lowering; preserve its validated presence without interpreting it as
    /// family bridge configuration in this final analog wiring stage.
    #[serde(default)]
    opaque_x_interfaces: Vec<OpaqueXInterface>,
    #[serde(default)]
    code_model_contracts: Vec<CodeModelContract>,
}

/// The X wiring resolver needs only the verified external model port
/// layout; these topology-only declarations never enter the output deck.
fn opaque_x_topology(interfaces: &[OpaqueXInterface]) -> Result<String> {
    let mut source = String::new();
    let mut names = BTreeMap::<String, Vec<model_alb_experimental::OpaqueXPinKind>>::new();
    for interface in interfaces {
        ensure!(
            !interface.pins.is_empty()
                && interface.model.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$' | b'.')
                }),
            "invalid external X topology interface {}",
            interface.model
        );
        let topology_model = interface.native_model.as_ref().unwrap_or(&interface.model);
        ensure!(
            !topology_model.is_empty()
                && topology_model.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$' | b'.')
                }),
            "invalid native external X topology model {topology_model}"
        );
        let name = topology_model.to_ascii_uppercase();
        if let Some(previous) = names.get(&name) {
            ensure!(
                previous == &interface.pins,
                "conflicting external X topology for shared native model {topology_model}"
            );
            continue;
        }
        names.insert(name, interface.pins.clone());
        write!(&mut source, ".subckt {topology_model}")?;
        for (index, kind) in interface.pins.iter().enumerate() {
            write!(&mut source, " SR_OPAQUE_PORT{index}")?;
            if interface.native_model.is_some()
                && *kind == model_alb_experimental::OpaqueXPinKind::Digital
            {
                write!(
                    &mut source,
                    " SR_OPAQUE_PORT{index}_SINK SR_OPAQUE_PORT{index}_SOURCE"
                )?;
            }
        }
        writeln!(&mut source)?;
        writeln!(&mut source, ".ends {topology_model}")?;
    }
    Ok(source)
}

pub struct FamilyConfig {
    pub definitions: Vec<DefinitionFamilyBoundary>,
    pub occurrences: Vec<OccurrenceFamilyBoundary>,
    pub interfaces: Vec<OpaqueXInterface>,
    contracts: Vec<CodeModelContract>,
    catalog: BridgeCatalog,
    native: NativeFamilySpecification,
}

impl FamilyConfig {
    pub fn read(bindings: &Path, models: &Path) -> Result<Self> {
        let config: Config = serde_json::from_slice(&fs::read(bindings)?)
            .context("decoding source analog/digital boundary annotations")?;
        let native: NativeFamilySpecification = serde_json::from_slice(&fs::read(models)?)
            .context("decoding exact native family electrical models")?;
        ensure!(
            native.pins.is_empty(),
            "automatic wiring accepts model parameters only, not handwritten physical pin triples"
        );
        let mut definitions = Vec::new();
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
                    producer: c.producer,
                    receivers: c.receivers,
                });
            } else {
                definitions.push(DefinitionFamilyBoundary {
                    scope: c.scope,
                    boundary,
                    producer: c.producer,
                    receivers: c.receivers,
                });
            }
        }
        let mut catalog = BridgeCatalog::default();
        for model in config.bridge_models {
            catalog.add(BridgeModel {
                name: model.name,
                kind: model.kind.into(),
                family: LogicFamily::parse(&model.family)?,
                nominal_supply_v: model.nominal_supply_v,
            })?;
        }
        Ok(Self {
            definitions,
            occurrences,
            interfaces: config.opaque_x_interfaces,
            contracts: config.code_model_contracts,
            catalog,
            native,
        })
    }

    pub fn verify(&self, path: &Path, source: &str) -> Result<()> {
        verify_external_native_libraries(path, source, &self.interfaces)?;
        verify_external_code_models(path, source, &self.contracts, &self.interfaces)
    }

    pub fn wire(
        &self,
        source: &str,
        compiled: &str,
        connections: &[ScopedAnalogDigitalConnection],
    ) -> Result<String> {
        let external = opaque_x_topology(&self.interfaces)?;
        let mut topology_sources = vec![compiled];
        if !external.is_empty() {
            topology_sources.push(&external);
        }
        materialize_scoped_family_bridges(
            source,
            &topology_sources,
            connections,
            &self.catalog,
            &self.native,
        )
    }
}
