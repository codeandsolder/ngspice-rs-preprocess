//! Regression for original logical `SIMetrix` ALB models and physical
//! three-node propagation, without manually supplying digital bundles.

use anyhow::Result;
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

#[path = "support/family.rs"]
mod harness;
use harness::fixtures as source_dir;
fn expand(source: &Path, config: &Path, dir: &Path) -> Result<Output> {
    harness::run(
        env!("CARGO_BIN_EXE_alb-logical-expand"),
        &[source, config],
        dir,
        &["lowered.cir", "generated.inc"],
    )
}
fn copy_binding(tmp: &Path, modifier: impl FnOnce(&mut Value)) -> Result<PathBuf> {
    let mut value: Value =
        serde_json::from_slice(&fs::read(source_dir().join("auto-bindings-3v3.json"))?)?;
    modifier(&mut value);
    let path = tmp.join("config.json");
    fs::write(&path, serde_json::to_vec(&value)?)?;
    Ok(path)
}
#[test]
fn file_and_inline_models_emit_source_tree_with_shared_physical_triples() -> Result<()> {
    for (fixture, expected) in [
        (
            "logical-original.cir",
            "XSR_A1 DIN DIN_SR_SINK DIN_SR_SOURCE DOUT DOUT_SR_SINK DOUT_SR_SOURCE SRALB0",
        ),
        (
            "logical-nested.cir",
            "XSR_A1 P P_SR_SINK P_SR_SOURCE Q Q_SR_SINK Q_SR_SOURCE SRALB0",
        ),
        (
            "logical-shared.cir",
            "XSR_A2 INPUT INPUT_SR_SINK INPUT_SR_SOURCE RIGHT RIGHT_SR_SINK RIGHT_SR_SOURCE SRALB0",
        ),
    ] {
        let temp = tempfile::tempdir()?;
        let config = source_dir().join(if fixture == "logical-shared.cir" {
            "auto-bindings-shared.json"
        } else {
            "auto-bindings-3v3.json"
        });
        let result = expand(&source_dir().join(fixture), &config, temp.path())?;
        assert!(
            result.status.success(),
            "{fixture}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let deck = fs::read_to_string(temp.path().join("lowered.cir"))?;
        let native = fs::read_to_string(temp.path().join("generated.inc"))?;
        assert!(deck.contains(expected), "{deck}");
        assert!(!deck.to_ascii_lowercase().contains("d_logic_block"));
        assert!(!deck.to_ascii_lowercase().contains(".file logic"));
        assert!(deck.contains(".include \"generated.inc\""));
        assert!(native.to_ascii_uppercase().contains(
            ".SUBCKT SRALB0 SR_IN0 SR_IN0_SINK SR_IN0_SOURCE SR_OUT0 SR_OUT0_SINK SR_OUT0_SOURCE"
        ));
        assert!(native.contains("out_res_pos=4e1"));
        assert!(native.contains("capacitance=2e-12"));
        if fixture == "logical-shared.cir" {
            assert_eq!(
                deck.matches("INPUT INPUT_SR_SINK INPUT_SR_SOURCE").count(),
                3
            );
        }
    }
    Ok(())
}

#[test]
fn mismatched_or_ambiguous_boundaries_fail_before_writing() -> Result<()> {
    let source = source_dir().join("logical-original.cir");
    for case in ["duplicate", "conflict", "preexisting", "unbound", "missing"] {
        let temp = tempfile::tempdir()?;
        let config = copy_binding(temp.path(), |v| match case {
            "duplicate" => {
                let entry = v["connections"][0].clone();
                if let Some(items) = v["connections"].as_array_mut() {
                    items.push(entry);
                }
            }
            "conflict" => v["connections"][1]["digital_net"] = json!("d_in"),
            "preexisting" => v["connections"][0]["digital_net"] = json!("vin"),
            "unbound" => v["connections"][0]["analog_net"] = json!("something_else"),
            "missing" => v["connections"] = json!([]),
            _ => unreachable!(),
        })?;
        let result = expand(&source, &config, temp.path())?;
        assert!(!result.status.success(), "{case} incorrectly accepted");
        assert!(!temp.path().join("lowered.cir").exists());
        assert!(!temp.path().join("generated.inc").exists());
    }
    Ok(())
}

#[test]
fn unsupported_model_forms_and_hierarchy_are_not_guessed() -> Result<()> {
    let input = fs::read_to_string(source_dir().join("logical-original.cir"))?;
    let binding = source_dir().join("auto-bindings-3v3.json");
    for (case, source) in [
        (
            "unmatched-output",
            input.replace("[din] [dout]", "[din] [dout other]"),
        ),
        (
            "wrong-model",
            input.replace("A1 [din] [dout] DL", "A1 [din] [dout] MISSING"),
        ),
        (
            "unknown-digital-device",
            input.replace(
                "A1 [din] [dout] DL",
                "A1 [din] [dout] DL\nA2 [din] [dout] MISSING",
            ),
        ),
        (
            "ambiguous-physical-instance",
            input.replace(
                "A1 [din] [dout] DL",
                "XSR_A1 din dout OTHER\nA1 [din] [dout] DL",
            ),
        ),
        ("missing-end", input.replace(".end\n", "")),
    ] {
        let temp = tempfile::tempdir()?;
        let file = temp.path().join(format!("{case}.cir"));
        fs::write(&file, source)?;
        let result = expand(&file, &binding, temp.path())?;
        assert!(!result.status.success(), "{case} accepted");
        assert!(!temp.path().join("lowered.cir").exists());
    }
    Ok(())
}

#[test]
fn compiled_model_parameters_are_extracted_from_inline_truth_table() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let result = expand(
        &source_dir().join("logical-nested.cir"),
        &source_dir().join("auto-bindings-3v3.json"),
        temp.path(),
    )?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let text = fs::read_to_string(temp.path().join("generated.inc"))?;
    assert!(
        text.contains("sr_alb_lut_bit(table_values=\"01\""),
        "{text}"
    );
    assert!(text.contains("delay=2e-9"), "{text}");
    assert!(text.contains("out_res_pos=4e1"), "{text}");
    Ok(())
}

#[test]
fn original_clocked_alb_discovery_and_native_bundle_wiring() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let input = source_dir().join("../vendor-stateful/edge-basic.net");
    let original = fs::read_to_string(input)?;
    let source = temp.path().join("clocked.cir");
    fs::write(&source, original)?;
    let config = temp.path().join("config.json");
    let result = Command::new(env!("CARGO_BIN_EXE_alb-boundary-discover"))
        .arg(&source)
        .arg(source_dir().join("auto-discovery-univ-5v.json"))
        .arg(&config)
        .output()?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let result = expand(&source, &config, temp.path())?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let library = temp.path().join("generated.inc");
    let native = fs::read_to_string(&library)?;
    assert!(
        native.contains(
            "Asr_stateful [sr_in0 sr_in1] [sr_out0] 0 [sr_out0_sink] [sr_out0_source] Msr_stateful"
        ),
        "{native}"
    );
    let wire = || {
        Command::new(env!("CARGO_BIN_EXE_alb-family-autowire"))
            .arg(temp.path().join("lowered.cir"))
            .arg(&library)
            .arg(&config)
            .arg(source_dir().join("synthetic-univ-wiring.json"))
            .arg(temp.path().join("final.cir"))
            .output()
    };
    let result = wire()?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let deck = fs::read_to_string(temp.path().join("final.cir"))?;
    assert_eq!(deck.matches("sr_family_adc(").count(), 2);
    assert_eq!(deck.matches("sr_family_dac(").count(), 1);
    fs::remove_file(temp.path().join("final.cir"))?;
    for broken in [
        native.replace("[sr_in0 sr_in1] [sr_out0]", "[sr_in1 sr_in0] [sr_out0]"),
        native.replace(
            "[sr_out0_sink] [sr_out0_source]",
            "[sr_out0_source] [sr_out0_sink]",
        ),
        native.replace("[sr_out0_sink]", "[unbound]"),
        native.replace("input_load=0)", "input_load=1e-12)"),
        native.replace("out_res_pos=1e2 ", "wrong_out_res_pos=1e2 "),
        native.replace("out_res_neg=1e2 ", "out_res_neg=NaN "),
        native.replace("hold_time=0e0 ", "hold_time=NaN "),
        native.replace("setup_time=0e0 ", "setup_time=0e0 setup_time=1e-9 "),
        native.replace("sr_alb_stateful(", "sr_alb_probe("),
    ] {
        fs::write(&library, &broken)?;
        let result = wire()?;
        assert!(
            !result.status.success(),
            "invalid stateful ABI accepted: {broken}"
        );
        assert!(!temp.path().join("final.cir").exists());
    }
    Ok(())
}

#[test]
fn annotation_document_rejects_unknown_fields_before_lowering() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config = copy_binding(temp.path(), |doc| {
        doc["connections"][0]["direciton"] = json!("input");
    })?;
    let result = expand(
        &source_dir().join("logical-original.cir"),
        &config,
        temp.path(),
    )?;
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("unknown field"));
    assert!(!temp.path().join("lowered.cir").exists());
    assert!(!temp.path().join("generated.inc").exists());
    Ok(())
}
