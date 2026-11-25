//! Explicit family-resolution planner for the experimental ALB backend.
//! No model is silently loaded from the proprietary `SIMetrix` installation.

use anyhow::{Context, Result, bail};
use model_alb_experimental::{
    AnalogBoundaryPin, AnalogBridgeKind, BridgeCatalog, BridgeModel, DigitalPinDirection,
    LegacyCompatibilityTable, LogicFamily, LogicFamilyAssignment, plan_analog_bridges,
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, env, fs, path::Path};

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PinDirection {
    Input,
    Output,
}

impl From<PinDirection> for DigitalPinDirection {
    fn from(value: PinDirection) -> Self {
        match value {
            PinDirection::Input => Self::Input,
            PinDirection::Output => Self::Output,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum BridgeDirection {
    Adc,
    Dac,
}

impl From<BridgeDirection> for AnalogBridgeKind {
    fn from(value: BridgeDirection) -> Self {
        match value {
            BridgeDirection::Adc => Self::Adc,
            BridgeDirection::Dac => Self::Dac,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    digital_nets: Vec<DigitalNet>,
    analog_pins: Vec<Boundary>,
    bridge_models: Vec<BridgeDefinition>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DigitalNet {
    node: String,
    inputs: Vec<String>,
    outputs: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Boundary {
    id: String,
    analog_net: String,
    direction: PinDirection,
    family: FamilySettings,
    supply_v: f64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FamilySettings {
    family: Option<String>,
    in_family: Option<String>,
    out_family: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BridgeDefinition {
    name: String,
    kind: BridgeDirection,
    family: String,
    nominal_supply_v: f64,
}

#[derive(Debug, Serialize)]
struct DigitalResult {
    node: String,
    input_base: Option<String>,
    output_base: Option<String>,
    warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
struct AnalogResult {
    analog_net: String,
    model: String,
    kind: &'static str,
    family: String,
    supply_v: f64,
    nominal_supply_v: f64,
    pins: Vec<String>,
}

#[derive(Debug, Serialize)]
struct Output {
    digital_nets: Vec<DigitalResult>,
    analog_bridges: Vec<AnalogResult>,
}

fn resolve(config: Config, table: &LegacyCompatibilityTable) -> Result<Output> {
    let mut seen_nets = BTreeSet::new();
    let mut digital_nets = Vec::new();
    for net in config.digital_nets {
        if net.node.is_empty()
            || net.node.chars().any(char::is_whitespace)
            || !seen_nets.insert(net.node.to_ascii_uppercase())
        {
            bail!("duplicate or invalid digital net {:?}", net.node);
        }
        if net.inputs.is_empty() && net.outputs.is_empty() {
            bail!("digital net {:?} has no connected pins", net.node);
        }
        let inputs = net
            .inputs
            .iter()
            .map(|f| LogicFamily::parse(f))
            .collect::<Result<Vec<_>>>()?;
        let outputs = net
            .outputs
            .iter()
            .map(|f| LogicFamily::parse(f))
            .collect::<Result<Vec<_>>>()?;
        let result = table
            .resolve_digital_net(&inputs, &outputs)
            .with_context(|| format!("resolving digital net {}", net.node))?;
        digital_nets.push(DigitalResult {
            node: net.node.to_ascii_uppercase(),
            input_base: result.input_base,
            output_base: result.output_base,
            warnings: result.warnings,
        });
    }
    digital_nets.sort_by(|a, b| a.node.cmp(&b.node));

    let mut catalog = BridgeCatalog::default();
    for model in config.bridge_models {
        catalog.add(BridgeModel {
            name: model.name,
            kind: model.kind.into(),
            family: LogicFamily::parse(&model.family)?,
            nominal_supply_v: model.nominal_supply_v,
        })?;
    }
    let pins = config
        .analog_pins
        .into_iter()
        .map(|p| {
            let assignment = LogicFamilyAssignment {
                family: p.family.family,
                in_family: p.family.in_family,
                out_family: p.family.out_family,
            };
            Ok(AnalogBoundaryPin {
                id: p.id,
                analog_net: p.analog_net,
                direction: p.direction.into(),
                family: assignment.for_pin(p.direction.into())?,
                supply_v: p.supply_v,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let analog_bridges = plan_analog_bridges(&pins, &catalog)?
        .into_iter()
        .map(|plan| AnalogResult {
            analog_net: plan.analog_net,
            model: plan.model_name,
            kind: match plan.kind {
                AnalogBridgeKind::Adc => "adc",
                AnalogBridgeKind::Dac => "dac",
            },
            family: plan.family.full_name().to_owned(),
            supply_v: plan.supply_v,
            nominal_supply_v: plan.nominal_supply_v,
            pins: plan.physical_pins,
        })
        .collect::<Vec<_>>();
    Ok(Output {
        digital_nets,
        analog_bridges,
    })
}

fn main() -> Result<()> {
    let arguments = env::args_os().skip(1).collect::<Vec<_>>();
    if arguments.len() != 3 {
        bail!("usage: alb-family-resolve EXPLICIT_COMPAT.txt PLAN.json OUTPUT.json");
    }
    let rules_path = Path::new(&arguments[0]);
    let config_path = Path::new(&arguments[1]);
    let output_path = Path::new(&arguments[2]);

    let rules = LegacyCompatibilityTable::parse(
        &fs::read_to_string(rules_path)
            .with_context(|| format!("reading {}", rules_path.display()))?,
    )?;
    let config = serde_json::from_slice::<Config>(
        &fs::read(config_path).with_context(|| format!("reading {}", config_path.display()))?,
    )
    .context("decoding explicit experimental family-planning JSON")?;
    let plan = resolve(config, &rules)?;
    let mut encoded = serde_json::to_vec_pretty(&plan)?;
    encoded.push(b'\n');
    fs::write(output_path, encoded)
        .with_context(|| format!("writing {}", output_path.display()))?;
    Ok(())
}
