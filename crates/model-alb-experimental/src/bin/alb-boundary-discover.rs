//! Discover original logical ALB mixed-signal sites and family assignments
//! directly from electrical SPICE terminals, without hand-naming digital
//! state wires or handwritten A-instance pin-to-bridge mappings.
//! Nominal supply and available family bridges remain explicit electrical data.

use anyhow::{Context, Result, bail, ensure};
use model_alb_experimental::{
    CodeModelContract, DigitalPinDirection, OpaqueXInterface,
    discover_original_alb_boundaries_with_opaque_x_interfaces, verify_external_code_models,
    verify_external_native_libraries,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{env, fs};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Options {
    supply_v: f64,
    bridge_models: Vec<Value>,
    #[serde(default)]
    opaque_x_interfaces: Vec<OpaqueXInterface>,
    #[serde(default)]
    code_model_contracts: Vec<CodeModelContract>,
}

fn run() -> Result<()> {
    let args = env::args_os().skip(1).collect::<Vec<_>>();
    if args.len() != 3 {
        bail!(
            "usage: alb-boundary-discover ORIGINAL.cir ELECTRICAL.json GENERATED-BOUNDARIES.json"
        );
    }
    let source = fs::read_to_string(&args[0]).context("reading original SPICE ALB source")?;
    let options: Options = serde_json::from_slice(&fs::read(&args[1])?)
        .context("reading explicit family supply and native bridge catalog")?;
    ensure!(
        options.supply_v.is_finite() && options.supply_v > 0.0,
        "ALB discovery requires a positive finite family supply voltage"
    );
    verify_external_native_libraries(
        std::path::Path::new(&args[0]),
        &source,
        &options.opaque_x_interfaces,
    )?;
    verify_external_code_models(
        std::path::Path::new(&args[0]),
        &source,
        &options.code_model_contracts,
        &options.opaque_x_interfaces,
    )?;
    let discovered = discover_original_alb_boundaries_with_opaque_x_interfaces(
        &source,
        &options.opaque_x_interfaces,
        options.supply_v,
    )?;
    ensure!(
        !discovered.is_empty(),
        "no original mixed ALB analog contacts discovered"
    );
    let connections = discovered
        .into_iter()
        .map(|entry| {
            let direction = match entry.boundary.direction {
                DigitalPinDirection::Input => "input",
                DigitalPinDirection::Output => "output",
            };
            let family_key = if direction == "input" {
                "in_family"
            } else {
                "out_family"
            };
            ensure!(
                options.bridge_models.iter().any(|item| {
                    item.get("family")
                        .and_then(Value::as_str)
                        .is_some_and(|v| v.eq_ignore_ascii_case(&entry.family))
                        && item.get("kind").and_then(Value::as_str).is_some_and(|v| {
                            v.eq_ignore_ascii_case(if direction == "input" { "adc" } else { "dac" })
                        })
                }),
                "no available {direction} bridge model for original ALB family {}",
                entry.family
            );
            let mut value = json!({
                "scope":entry.boundary.scope,
                "analog_net":entry.boundary.analog_net,
                "digital_net":entry.boundary.digital_net,
                "direction":direction,
                "supply_v":entry.supply_v,
            });
            value[family_key] = json!(entry.family);
            if let Some(producer) = entry.boundary.producer {
                value["producer"] = json!({
                    "path": producer.path,
                    "output_pin": producer.output_pin
                });
            }
            if !entry.boundary.receivers.is_empty() {
                value["receivers"] = json!(entry.boundary.receivers);
            }
            Ok(value)
        })
        .collect::<Result<Vec<_>>>()?;
    let text = serde_json::to_vec_pretty(&json!({
        "connections":connections,
        "bridge_models":options.bridge_models,
        "opaque_x_interfaces":options.opaque_x_interfaces,
        "code_model_contracts":options.code_model_contracts
    }))?;
    fs::write(&args[2], text).context("writing discovered ALB physical boundary config")?;
    Ok(())
}
fn main() -> Result<()> {
    run()
}
