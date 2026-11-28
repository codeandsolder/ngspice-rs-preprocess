//! Emit experimental native XSPICE ADC/DAC instances from an already
//! verified family-resolution plan, explicit model data, and digital wiring.

use anyhow::{Context, Result, bail};
use model_alb_experimental::{
    AnalogBridgeKind, LogicFamily, NativeFamilySpecification, PlannedAnalogBridge,
    render_native_family_bridges,
};
use serde::Deserialize;
use std::{env, fs, path::Path};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolvedPlan {
    #[serde(rename = "digital_nets")]
    _digital_nets: Vec<serde_json::Value>,
    analog_bridges: Vec<ResolvedBridge>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolvedBridge {
    analog_net: String,
    model: String,
    kind: String,
    family: String,
    supply_v: f64,
    nominal_supply_v: f64,
    pins: Vec<String>,
}

fn run() -> Result<()> {
    let args = env::args_os().skip(1).collect::<Vec<_>>();
    if args.len() != 3 {
        bail!("usage: alb-family-materialize RESOLVED.json NATIVE.json OUTPUT.inc");
    }
    let plan_path = Path::new(&args[0]);
    let native_path = Path::new(&args[1]);
    let output_path = Path::new(&args[2]);
    let resolved: ResolvedPlan = serde_json::from_slice(
        &fs::read(plan_path).with_context(|| format!("reading {}", plan_path.display()))?,
    )
    .context("decoding resolved family grouping")?;
    // The family resolver has already checked digital compatibility.
    // This stage is deliberately downstream: never silently redo it using
    // guessed built-in rules or the modern physical-level algorithm.
    let plans = resolved
        .analog_bridges
        .into_iter()
        .map(|plan| {
            let kind = match plan.kind.as_str() {
                "adc" => AnalogBridgeKind::Adc,
                "dac" => AnalogBridgeKind::Dac,
                other => bail!("unsupported native family bridge kind {other}"),
            };
            Ok(PlannedAnalogBridge {
                analog_net: plan.analog_net,
                model_name: plan.model,
                kind,
                family: LogicFamily::parse(&plan.family)?,
                supply_v: plan.supply_v,
                nominal_supply_v: plan.nominal_supply_v,
                physical_pins: plan.pins,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let native: NativeFamilySpecification = serde_json::from_slice(
        &fs::read(native_path).with_context(|| format!("reading {}", native_path.display()))?,
    )
    .context("decoding native physical bridge models and digital bundles")?;
    let generated = render_native_family_bridges(&plans, &native)?;
    fs::write(output_path, generated)
        .with_context(|| format!("writing {}", output_path.display()))?;
    Ok(())
}

fn main() -> Result<()> {
    run()
}
