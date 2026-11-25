//! Roundtrip the explicit family CLI using only synthetic test tables.

use anyhow::{Context, Result};
use serde_json::Value;
use std::{fs, path::PathBuf, process::Command};

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../runtime/xspice/fixtures/family")
}

#[test]
fn family_plan_cli_groups_only_exact_family_and_supply() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let output = tmp.path().join("plan.json");
    let status = Command::new(env!("CARGO_BIN_EXE_alb-family-resolve"))
        .arg(fixtures().join("compat-synthetic.txt"))
        .arg(fixtures().join("plan-synthetic.json"))
        .arg(&output)
        .status()?;
    assert!(status.success(), "explicit family CLI failed");
    let plan: Value = serde_json::from_slice(&fs::read(output)?)?;
    let digital = plan["digital_nets"]
        .as_array()
        .context("missing digital nets")?;
    assert_eq!(digital.len(), 2);
    assert_eq!(digital[0]["node"], "DIG_COMPAT");
    assert_eq!(digital[0]["warnings"].as_array().map(Vec::len), Some(0));
    assert_eq!(digital[1]["warnings"].as_array().map(Vec::len), Some(1));
    let analog = plan["analog_bridges"]
        .as_array()
        .context("missing analog bridges")?;
    assert_eq!(analog.len(), 5);
    assert_eq!(analog.iter().filter(|b| b["kind"] == "dac").count(), 2);
    assert_eq!(analog.iter().filter(|b| b["kind"] == "adc").count(), 3);
    let shared = analog
        .iter()
        .find(|b| b["pins"].as_array().is_some_and(|pins| pins.len() == 2))
        .context("missing shared HC ADC")?;
    assert_eq!(shared["family"], "HC");
    assert_eq!(shared["pins"], serde_json::json!(["X1.in", "X2.in"]));
    assert_eq!(shared["nominal_supply_v"], 5.0);
    Ok(())
}

#[test]
fn family_cli_rejects_missing_specific_schmitt_bridge_model() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let fixture: Value =
        serde_json::from_slice(&fs::read(fixtures().join("plan-synthetic.json"))?)?;
    let mut edited = fixture;
    edited["bridge_models"] = serde_json::json!([
        {"name": "HC_ADC", "kind": "adc", "family": "HC", "nominal_supply_v": 5.0},
        {"name": "HC_DAC", "kind": "dac", "family": "HC", "nominal_supply_v": 5.0}
    ]);
    let input = tmp.path().join("missing.json");
    fs::write(&input, serde_json::to_vec(&edited)?)?;
    let output = tmp.path().join("result.json");
    let result = Command::new(env!("CARGO_BIN_EXE_alb-family-resolve"))
        .arg(fixtures().join("compat-synthetic.txt"))
        .arg(&input)
        .arg(&output)
        .output()?;
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("HC_SCHMITT"));
    assert!(!output.exists(), "invalid plan must never write output");
    Ok(())
}
