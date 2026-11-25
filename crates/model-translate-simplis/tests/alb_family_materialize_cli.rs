//! End-to-end explicit family resolution → native XSPICE emission tests.

use anyhow::{Context, Result};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../runtime/xspice/fixtures/family")
}

fn plan(supply: &str, root: &Path) -> Result<PathBuf> {
    let path = root.join(format!("{supply}-resolved.json"));
    let result = Command::new(env!("CARGO_BIN_EXE_alb-family-resolve"))
        .arg(fixtures().join("compat-synthetic.txt"))
        .arg(fixtures().join(format!("buffer-plan-{supply}.json")))
        .arg(&path)
        .output()?;
    assert!(
        result.status.success(),
        "family resolver: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    Ok(path)
}

fn emit(plan_path: &Path, spec: &Path, out: &Path) -> Result<std::process::Output> {
    Ok(Command::new(env!("CARGO_BIN_EXE_alb-family-materialize"))
        .arg(plan_path)
        .arg(spec)
        .arg(out)
        .output()?)
}

#[test]
fn generates_supply_scaled_native_instances_with_existing_three_node_bundles() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let spec = fixtures().join("buffer-native.json");
    for (supply, input, output) in [
        ("3v3", "in_low=1.386", "out_high=3.3"),
        ("5v", "in_low=2.1", "out_high=5e0"),
    ] {
        let resolved = plan(supply, temp.path())?;
        let native = temp.path().join(format!("{supply}.inc"));
        let status = emit(&resolved, &spec, &native)?;
        assert!(
            status.status.success(),
            "native emitter: {}",
            String::from_utf8_lossy(&status.stderr)
        );
        let text = fs::read_to_string(native)?;
        assert!(text.contains(input), "{text}");
        assert!(text.contains(output), "{text}");
        assert_eq!(text.matches("sr_family_adc(").count(), 1);
        assert_eq!(text.matches("sr_family_dac(").count(), 1);
        assert!(text.contains("%gd(ANALOG_INPUT 0) D_IN D_IN_SINK D_IN_SOURCE"));
        assert!(text.contains("D_OUT D_OUT_SINK D_OUT_SOURCE %gd(ANALOG_OUTPUT 0)"));
        assert!(text.contains("g_pullup=2.5e-2"), "{text}");
        assert!(text.contains("g_pulldown=2.5e-2"), "{text}");
        assert!(text.contains("knee_high=2.75e0"), "{text}");
    }
    Ok(())
}

#[test]
fn rejects_unwired_missing_or_aliased_physical_pin() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let resolved = plan("3v3", temp.path())?;
    let fixture: Value = serde_json::from_slice(&fs::read(fixtures().join("buffer-native.json"))?)?;
    for (name, change) in [
        ("missing-model", "missing-model"),
        ("missing-pin", "missing-pin"),
        ("alias", "alias"),
        ("bad-supply", "bad-supply"),
        ("bad-clamp", "bad-clamp"),
        ("bad-g", "bad-g"),
        ("bad-current-polarity", "bad-current-polarity"),
        ("adc-out-res", "adc-out-res"),
        ("unused-pin", "unused-pin"),
    ] {
        let mut broken = fixture.clone();
        match change {
            "missing-model" => broken["models"] = serde_json::json!([broken["models"][0]]),
            "missing-pin" => broken["pins"] = serde_json::json!([broken["pins"][0]]),
            "alias" => broken["pins"][1]["bundle"] = broken["pins"][0]["bundle"].clone(),
            "bad-supply" => broken["models"][0]["nominal_supply_v"] = serde_json::json!(4.5),
            "bad-clamp" => {
                broken["models"][0]["native"]["parameters"]["in_high"] = serde_json::json!(1.0);
            }
            "bad-g" => {
                broken["models"][1]["native"]["parameters"]["g_pullup"] = serde_json::json!(-1);
            }
            "bad-current-polarity" => {
                broken["models"][1]["native"]["parameters"]["sink_current"] = serde_json::json!(1);
            }
            "adc-out-res" => {
                broken["pins"][0]["out_res_ohm"] = serde_json::json!(100);
            }
            "unused-pin" => {
                broken["pins"]
                    .as_array_mut()
                    .context("pins not array")?
                    .push(serde_json::json!({"id":"Unused.pin","bundle":{
                        "state":"unused_state", "sink_meta":"unused_sink",
                        "source_meta":"unused_source"
                    },"out_res_ohm":null}));
            }
            _ => unreachable!(),
        }
        let input = temp.path().join(format!("{name}.json"));
        let output = temp.path().join(format!("{name}.inc"));
        fs::write(&input, serde_json::to_vec(&broken)?)?;
        let result = emit(&resolved, &input, &output)?;
        assert!(!result.status.success(), "{name} unexpectedly accepted");
        assert!(!output.exists(), "{name} wrote invalid native output");
    }
    Ok(())
}

#[test]
fn rejects_invalid_shared_adc_wiring_without_duplicate_input_loads() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let resolved = temp.path().join("group.json");
    let group = serde_json::json!({
        "digital_nets": [],
        "analog_bridges": [{
            "analog_net":"ANALOG_INPUT", "model":"HC_ADC", "kind":"adc",
            "family":"HC", "supply_v":3.3, "nominal_supply_v":5.0,
            "pins":["A.in","B.in"]
        }]
    });
    fs::write(&resolved, serde_json::to_vec(&group)?)?;
    let native = fixtures().join("buffer-native.json");
    let mut spec: Value = serde_json::from_slice(&fs::read(native)?)?;
    spec["pins"] = serde_json::json!([
        {"id":"A.in","bundle":{"state":"d_in","sink_meta":"d_sink","source_meta":"d_source"},"out_res_ohm":null},
        {"id":"B.in","bundle":{"state":"d_other","sink_meta":"d_other_sink","source_meta":"d_other_source"},"out_res_ohm":null}
    ]);
    let spec_path = temp.path().join("spec.json");
    fs::write(&spec_path, serde_json::to_vec(&spec)?)?;
    let out = temp.path().join("out.inc");
    assert!(!emit(&resolved, &spec_path, &out)?.status.success());
    spec["pins"][1]["bundle"] = spec["pins"][0]["bundle"].clone();
    fs::write(&spec_path, serde_json::to_vec(&spec)?)?;
    assert!(emit(&resolved, &spec_path, &out)?.status.success());
    let text = fs::read_to_string(&out).context("read generated shared ADC")?;
    assert_eq!(text.matches("sr_family_adc(").count(), 1);
    assert!(!text.contains("sr_bundle_load"));
    Ok(())
}

#[test]
fn physical_bundle_nodes_support_hierarchical_spice_identifiers() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let resolved = plan("3v3", temp.path())?;
    let mut fixture: Value =
        serde_json::from_slice(&fs::read(fixtures().join("buffer-native.json"))?)?;
    fixture["pins"][0]["bundle"]["state"] = serde_json::json!("x1.d_in");
    fixture["pins"][0]["bundle"]["sink_meta"] = serde_json::json!("x1.d_sink");
    fixture["pins"][0]["bundle"]["source_meta"] = serde_json::json!("x1.d_source");
    let spec = temp.path().join("hierarchical.json");
    fs::write(&spec, serde_json::to_vec(&fixture)?)?;
    let output = temp.path().join("hierarchical.inc");
    let result = emit(&resolved, &spec, &output)?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let text = fs::read_to_string(output)?;
    assert!(text.contains("X1.D_IN X1.D_SINK X1.D_SOURCE"));
    Ok(())
}
