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
        for (key, expected) in [
            ("knee_high", if supply == "3v3" { 1.815 } else { 2.75 }),
            ("knee_low", if supply == "3v3" { 1.32 } else { 2.0 }),
            ("v_smooth", if supply == "3v3" { 0.33 } else { 0.5 }),
            ("clamp_high", if supply == "3v3" { 3.3 } else { 5.0 }),
        ] {
            let prefix = format!("{key}=");
            let value: f64 = text
                .split_ascii_whitespace()
                .find_map(|field| field.strip_prefix(&prefix))
                .context("missing scaled physical voltage")?
                .parse()?;
            assert!((value - expected).abs() < 1e-12, "{key}: {text}");
        }
    }
    Ok(())
}

#[test]
fn explicit_dac_metadata_allows_reverse_signed_demands() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let resolved = plan("3v3", temp.path())?;
    let mut spec: Value =
        serde_json::from_slice(&fs::read(fixtures().join("buffer-native.json"))?)?;
    spec["models"][1]["native"]["parameters"]["sink_current"] = serde_json::json!(0.0005);
    spec["models"][1]["native"]["parameters"]["source_current"] = serde_json::json!(-0.0005);
    let input = temp.path().join("signed.json");
    let out = temp.path().join("signed.inc");
    fs::write(&input, serde_json::to_vec(&spec)?)?;
    let result = emit(&resolved, &input, &out)?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(fs::read_to_string(out)?.contains("sink_current=5e-4 source_current=-5e-4"));
    Ok(())
}

#[test]
fn adc_signed_parameters_and_subpicosecond_values_are_emitted_without_clipping() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let resolved = plan("5v", temp.path())?;
    let mut spec: Value =
        serde_json::from_slice(&fs::read(fixtures().join("buffer-native.json"))?)?;
    let params = &mut spec["models"][0]["native"]["parameters"]["adc"];
    params["rise_delay"] = serde_json::json!(-1e-9);
    params["fall_delay"] = serde_json::json!(5e-13);
    params["time_tol"] = serde_json::json!(0.0);
    params["clamp_res"] = serde_json::json!(-1e-7);
    params["clamp_bias"] = serde_json::json!(-0.1);
    let input = temp.path().join("signed-adc.json");
    let out = temp.path().join("signed-adc.inc");
    fs::write(&input, serde_json::to_vec(&spec)?)?;
    let result = emit(&resolved, &input, &out)?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let text = fs::read_to_string(&out)?;
    assert!(
        text.contains("rise_delay=-1e-9 fall_delay=5e-13 time_tol=0e0"),
        "{text}"
    );
    assert!(text.contains("clamp_res=-1e-7 clamp_bias=-1e-1"), "{text}");
    spec["models"][0]["native"]["parameters"]["adc"]["clamp_res"] = serde_json::json!(0);
    fs::write(&input, serde_json::to_vec(&spec)?)?;
    let invalid = temp.path().join("zero-clamp.inc");
    assert!(!emit(&resolved, &input, &invalid)?.status.success());
    assert!(!invalid.exists());
    Ok(())
}

#[test]
fn adc_threshold_clamp_and_output_order_follow_vendor_domains() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let resolved = plan("5v", temp.path())?;
    let mut spec: Value =
        serde_json::from_slice(&fs::read(fixtures().join("buffer-native.json"))?)?;
    for (label, in_low, in_high, out_low, out_high, clamp_low, clamp_high) in [
        ("input-reversed", 2.2, 2.1, 0.0, 5.0, 0.0, 5.0),
        ("input-equal", 2.1, 2.1, 0.0, 5.0, 0.0, 5.0),
        ("output-reversed", 2.1, 2.2, 5.0, 0.0, 0.0, 5.0),
        ("clamp-reversed", 2.1, 2.2, 0.0, 5.0, 5.0, 0.0),
        ("clamp-equal", 2.1, 2.2, 0.0, 5.0, 2.5, 2.5),
        ("all-reversed", 2.2, 2.1, 5.0, 0.0, 5.0, 0.0),
    ] {
        let params = &mut spec["models"][0]["native"]["parameters"];
        params["adc"]["in_low"] = serde_json::json!(in_low);
        params["adc"]["in_high"] = serde_json::json!(in_high);
        params["adc"]["out_low"] = serde_json::json!(out_low);
        params["adc"]["out_high"] = serde_json::json!(out_high);
        params["clamp_low"] = serde_json::json!(clamp_low);
        params["clamp_high"] = serde_json::json!(clamp_high);
        let input = temp.path().join(format!("{label}.json"));
        let out = temp.path().join(format!("{label}.inc"));
        fs::write(&input, serde_json::to_vec(&spec)?)?;
        let result = emit(&resolved, &input, &out)?;
        assert!(
            result.status.success(),
            "{label}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let native = fs::read_to_string(out)?;
        for expected in [
            format!("in_low={in_low:e} in_high={in_high:e}"),
            format!("out_low={out_low:e} out_high={out_high:e}"),
            format!("clamp_low={clamp_low:e} clamp_high={clamp_high:e}"),
        ] {
            assert!(
                native.contains(&expected),
                "{label}: expected {expected} in {native}"
            );
        }
    }
    spec["models"][0]["native"]["parameters"]["adc"]["out_low"] = serde_json::json!(2.5);
    spec["models"][0]["native"]["parameters"]["adc"]["out_high"] = serde_json::json!(2.5);
    let input = temp.path().join("equal-output.json");
    let out = temp.path().join("equal-output.inc");
    fs::write(&input, serde_json::to_vec(&spec)?)?;
    assert!(!emit(&resolved, &input, &out)?.status.success());
    assert!(!out.exists());
    // Test unsupported IC independently of the equal-rail rejection.
    spec["models"][0]["native"]["parameters"]["adc"]["out_low"] = serde_json::json!(0.0);
    spec["models"][0]["native"]["parameters"]["adc"]["out_high"] = serde_json::json!(5.0);
    spec["models"][0]["native"]["parameters"]["adc"]["ic"] = serde_json::json!(1);
    let input = temp.path().join("unsupported-ic.json");
    let out = temp.path().join("unsupported-ic.inc");
    fs::write(&input, serde_json::to_vec(&spec)?)?;
    let result = emit(&resolved, &input, &out)?;
    assert!(!result.status.success());
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("unknown field `ic`"),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(!out.exists());
    Ok(())
}

#[test]
fn schmitt_native_metadata_excludes_vendor_ignored_clamp_and_initial_state() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let resolved = plan("5v", temp.path())?;
    let mut spec: Value =
        serde_json::from_slice(&fs::read(fixtures().join("buffer-native.json"))?)?;
    let adc = spec["models"][0]["native"]["parameters"]["adc"].clone();
    spec["models"][0]["native"] = serde_json::json!({
        "kind": "adc_schmitt", "parameters": adc
    });
    let input = temp.path().join("schmitt.json");
    let output = temp.path().join("schmitt.inc");
    fs::write(&input, serde_json::to_vec(&spec)?)?;
    let result = emit(&resolved, &input, &output)?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let generated = fs::read_to_string(&output)?;
    assert!(generated.contains("schmitt=true)"), "{generated}");
    assert!(!generated.contains("clamp_low="), "{generated}");
    assert!(!generated.contains("clamp_high="), "{generated}");

    // Vendor accepts signed, inverted nondegenerate load levels for
    // Schmitt; both limits remain required and distinct after scaling.
    spec["models"][0]["native"]["parameters"]["out_low"] = serde_json::json!(-2.0);
    spec["models"][0]["native"]["parameters"]["out_high"] = serde_json::json!(-5.0);
    fs::write(&input, serde_json::to_vec(&spec)?)?;
    assert!(emit(&resolved, &input, &output)?.status.success());
    let generated = fs::read_to_string(&output)?;
    assert!(
        generated.contains("out_low=-2e0 out_high=-5e0"),
        "{generated}"
    );

    for field in ["clamp_low", "clamp_high", "ic", "schmitt"] {
        let mut invalid = spec.clone();
        invalid["models"][0]["native"]["parameters"][field] = serde_json::json!(1.0);
        let out = temp.path().join(format!("invalid-schmitt-{field}.inc"));
        fs::write(&input, serde_json::to_vec(&invalid)?)?;
        let result = emit(&resolved, &input, &out)?;
        assert!(
            !result.status.success(),
            "unexpected Schmitt {field} accepted"
        );
        assert!(
            String::from_utf8_lossy(&result.stderr).contains(&format!("unknown field `{field}`")),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(!out.exists());
    }
    let mut invalid = spec.clone();
    invalid["models"][0]["native"]["parameters"]["out_high"] = serde_json::json!(-2.0);
    fs::write(&input, serde_json::to_vec(&invalid)?)?;
    let out = temp.path().join("equal-schmitt-rails.inc");
    assert!(!emit(&resolved, &input, &out)?.status.success());
    assert!(!out.exists());

    // Ordinary ADC requires both independent clamp bounds, and must
    // never silently materialize a Schmitt model from a boolean flag.
    let mut bridge: Value =
        serde_json::from_slice(&fs::read(fixtures().join("buffer-native.json"))?)?;
    for field in ["clamp_low", "clamp_high"] {
        let mut missing = bridge.clone();
        missing["models"][0]["native"]["parameters"]
            .as_object_mut()
            .context("bridge parameter object")?
            .remove(field);
        fs::write(&input, serde_json::to_vec(&missing)?)?;
        let out = temp.path().join(format!("missing-bridge-{field}.inc"));
        let result = emit(&resolved, &input, &out)?;
        assert!(
            !result.status.success(),
            "unexpected missing {field} accepted"
        );
        assert!(
            String::from_utf8_lossy(&result.stderr).contains(&format!("missing field `{field}`")),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(!out.exists());
    }
    bridge["models"][0]["native"]["parameters"]["adc"]["schmitt"] = serde_json::json!(true);
    fs::write(&input, serde_json::to_vec(&bridge)?)?;
    let out = temp.path().join("obsolete-bridge-schmitt.inc");
    let result = emit(&resolved, &input, &out)?;
    assert!(!result.status.success());
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("unknown field `schmitt`"),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(!out.exists());
    Ok(())
}

#[test]
fn dac_signed_electrical_and_slew_parameters_are_emitted_without_clipping() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let resolved = plan("5v", temp.path())?;
    let mut spec: Value =
        serde_json::from_slice(&fs::read(fixtures().join("buffer-native.json"))?)?;
    let params = &mut spec["models"][1]["native"]["parameters"];
    params["g_pullup"] = serde_json::json!(-0.024);
    params["g_pulldown"] = serde_json::json!(-0.034);
    params["g_resistive"] = serde_json::json!(-0.001);
    params["g_hiz"] = serde_json::json!(-0.01);
    params["v_smooth"] = serde_json::json!(-0.5);
    params["t_rise"] = serde_json::json!(-1e-9);
    params["t_fall"] = serde_json::json!(5e-13);
    let input = temp.path().join("signed-dac.json");
    let out = temp.path().join("signed-dac.inc");
    fs::write(&input, serde_json::to_vec(&spec)?)?;
    let result = emit(&resolved, &input, &out)?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let text = fs::read_to_string(&out)?;
    assert!(
        text.contains("g_pullup=-2.4e-2 g_pulldown=-3.4e-2"),
        "{text}"
    );
    assert!(text.contains("g_resistive=-1e-3 g_hiz=-1e-2"), "{text}");
    assert!(text.contains("v_smooth=-5e-1"), "{text}");
    assert!(text.contains("t_rise=-1e-9 t_fall=5e-13"), "{text}");
    for parameter in ["t_rise", "t_fall"] {
        let mut invalid_spec = spec.clone();
        invalid_spec["models"][1]["native"]["parameters"][parameter] = serde_json::json!(0);
        fs::write(&input, serde_json::to_vec(&invalid_spec)?)?;
        let invalid = temp.path().join(format!("zero-{parameter}.inc"));
        assert!(!emit(&resolved, &input, &invalid)?.status.success());
        assert!(!invalid.exists());
    }
    Ok(())
}

#[test]
fn accepts_equal_and_reversed_dac_rails() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let resolved = plan("5v", temp.path())?;
    let mut spec: Value =
        serde_json::from_slice(&fs::read(fixtures().join("buffer-native.json"))?)?;
    for (label, low, high) in [
        ("equal", 2.5, 2.5),
        ("reversed", 5.0, 0.0),
        ("negative-reversed", -1.0, -2.0),
    ] {
        let params = &mut spec["models"][1]["native"]["parameters"];
        params["out_low"] = serde_json::json!(low);
        params["out_high"] = serde_json::json!(high);
        let input = temp.path().join(format!("{label}.json"));
        let out = temp.path().join(format!("{label}.inc"));
        fs::write(&input, serde_json::to_vec(&spec)?)?;
        let result = emit(&resolved, &input, &out)?;
        assert!(
            result.status.success(),
            "{label}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let text = fs::read_to_string(&out)?;
        let expected_low = format!("out_low={low:e}");
        let expected_high = format!("out_high={high:e}");
        assert!(text.contains(&expected_low), "{label}: {text}");
        assert!(text.contains(&expected_high), "{label}: {text}");
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
        ("adc-out-res", "adc-out-res"),
        ("unused-pin", "unused-pin"),
    ] {
        let mut broken = fixture.clone();
        match change {
            "missing-model" => broken["models"] = serde_json::json!([broken["models"][0]]),
            "missing-pin" => broken["pins"] = serde_json::json!([broken["pins"][0]]),
            "alias" => broken["pins"][1]["bundle"] = broken["pins"][0]["bundle"].clone(),
            "bad-supply" => broken["models"][0]["nominal_supply_v"] = serde_json::json!(4.5),
            "adc-out-res" => {
                broken["pins"][0]["out_res_ohm"] =
                    serde_json::json!({"positive":100,"negative":100});
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
