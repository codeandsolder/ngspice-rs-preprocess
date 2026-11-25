//! Regression for original logical `SIMetrix` ALB models and physical
//! three-node propagation, without manually supplying digital bundles.

use anyhow::Result;
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

fn source_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../runtime/xspice/fixtures/family")
}
fn expand(source: &Path, binding: &Path, dir: &Path) -> Result<Output> {
    Ok(Command::new(env!("CARGO_BIN_EXE_alb-logical-expand"))
        .arg(source)
        .arg(binding)
        .arg(dir.join("lowered.cir"))
        .arg(dir.join("generated.inc"))
        .output()?)
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
        assert!(native.contains("out_res=4e1"));
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
            "unequal-out-res",
            input.replace("out_res=40", "out_res_pos=40 out_res_neg=20"),
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
    assert!(text.contains("out_res=4e1"), "{text}");
    Ok(())
}
