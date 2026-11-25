//! Source-level topology discovery CLI tests. Physical pin triples are
//! recovered from compiled ALB X instance ports, never supplied in JSON.

use anyhow::{Context, Result};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../runtime/xspice/fixtures")
}
fn compile_alb(root: &Path) -> Result<PathBuf> {
    let output = root.join("generated.inc");
    let result = Command::new(env!("CARGO_BIN_EXE_alb-xspice-compile"))
        .arg(fixtures().join("bundle-buffer.alb"))
        .arg(fixtures().join("family/buffer-40r.json"))
        .arg(&output)
        .arg("ALB_BUFFER")
        .output()?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    Ok(output)
}
fn autowire(deck: &Path, defs: &Path, bindings: &Path, out: &Path) -> Result<Output> {
    Ok(Command::new(env!("CARGO_BIN_EXE_alb-family-autowire"))
        .arg(deck)
        .arg(defs)
        .arg(bindings)
        .arg(fixtures().join("family/auto-models.json"))
        .arg(out)
        .output()?)
}

#[test]
fn real_compiled_alb_direct_and_nested_auto_bridge() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let defs = compile_alb(dir.path())?;
    for (case, binding, expected_id) in [
        ("auto-direct.cir", "auto-bindings-3v3.json", "Xalb"),
        ("auto-nested.cir", "auto-bindings-3v3.json", "Xtop"),
        ("auto-direct.cir", "auto-bindings-5v.json", "Xalb"),
    ] {
        let source = fixtures().join("family").join(case);
        let mapping = fixtures().join("family").join(binding);
        let output = dir.path().join(format!("{case}.{binding}.cir"));
        let result = autowire(&source, &defs, &mapping, &output)?;
        assert!(
            result.status.success(),
            "{case} {expected_id}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let generated = fs::read_to_string(&output)?;
        assert!(generated.contains("sr_family_adc("));
        assert!(generated.contains("sr_family_dac("));
        assert!(generated.contains("g_pullup=2.4e-2 g_pulldown=3.4e-2"));
        assert_eq!(generated.matches("sr_family_adc(").count(), 1);
        assert_eq!(generated.matches("sr_family_dac(").count(), 1);
        assert!(generated.trim_end().to_ascii_lowercase().ends_with(".end"));
        if binding == "auto-bindings-3v3.json" {
            assert!(generated.contains("in_low=1.386e0"));
            assert!(generated.contains("out_high=3.3e0"));
        } else {
            assert!(generated.contains("in_low=2.1e0"));
            assert!(generated.contains("out_high=5e0"));
        }
    }
    Ok(())
}

#[test]
fn unmatched_or_private_digital_nets_cannot_generate_bridges() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let defs = compile_alb(dir.path())?;
    let valid = fs::read_to_string(fixtures().join("family/auto-direct.cir"))?;
    let mappings = fixtures().join("family/auto-bindings-3v3.json");
    for (case, deck) in [
        (
            "missing",
            valid.replace(
                "d_in d_in_sink d_in_source",
                "other other_sink other_source",
            ),
        ),
        (
            "recursive",
            String::from(
                ".subckt WRAP a b c d e f\nXloop a b c d e f WRAP\n.ends WRAP\nXtop d_in d_in_sink d_in_source d_out d_out_sink d_out_source WRAP\n.end\n",
            ),
        ),
        (
            "invalid-port",
            valid.replace(
                "d_in d_in_sink d_in_source d_out d_out_sink d_out_source ALB_BUFFER",
                "d_in d_in_sink d_in_source d_out d_out_sink ALB_BUFFER",
            ),
        ),
        (
            "prebridged",
            valid.replace(
                ".end\n",
                ".model PRIOR adc_bridge(in_low=.2 in_high=.8)\nAprior %gd(ANALOG_INPUT 0) d_in d_in_sink d_in_source PRIOR\n.end\n",
            ),
        ),
        (
            "private",
            String::from(
                "Xtop out out_sink out_source WRAP\n.subckt WRAP a b c\nXcore local local_sink local_source a b c ALB_BUFFER\n.ends WRAP\n.end\n",
            ),
        ),
    ] {
        let input = dir.path().join(format!("{case}.cir"));
        let out = dir.path().join(format!("{case}-generated.cir"));
        fs::write(&input, deck)?;
        let result = autowire(&input, &defs, &mappings, &out)?;
        assert!(!result.status.success(), "{case} incorrectly accepted");
        assert!(!out.exists(), "failure {case} wrote partial source");
    }
    Ok(())
}

#[test]
fn invalid_mappings_and_duplicate_boundaries_rejected() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let defs = compile_alb(dir.path())?;
    let source = fixtures().join("family/auto-direct.cir");
    let original: serde_json::Value =
        serde_json::from_slice(&fs::read(fixtures().join("family/auto-bindings-3v3.json"))?)
            .context("decode binding fixture")?;
    for case in ["duplicate", "unregistered", "invalid-net", "bad-supply"] {
        let mut data = original.clone();
        match case {
            "duplicate" => data["connections"]
                .as_array_mut()
                .context("missing connections")?
                .push(original["connections"][0].clone()),
            "unregistered" => {
                data["connections"][0]["in_family"] = serde_json::json!("NO_SUCH_FAMILY");
            }
            "invalid-net" => data["connections"][0]["digital_net"] = serde_json::json!("IN BAD"),
            "bad-supply" => data["connections"][0]["supply_v"] = serde_json::json!(0),
            "analog-digital-alias" => {
                data["connections"][0]["analog_net"] = serde_json::json!("d_out_sink");
            }
            _ => unreachable!(),
        }
        let input = dir.path().join(format!("{case}.json"));
        let out = dir.path().join(format!("{case}-out.cir"));
        fs::write(&input, serde_json::to_vec(&data)?)?;
        let status = autowire(&source, &defs, &input, &out)?;
        assert!(!status.status.success(), "{case} unexpectedly accepted");
        assert!(!out.exists(), "failed {case} wrote source");
    }
    Ok(())
}

#[test]
fn preexisting_a_device_on_native_bundle_metadata_is_rejected() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let defs = compile_alb(dir.path())?;
    let original = fs::read_to_string(fixtures().join("family/auto-direct.cir"))?;
    let mappings = fixtures().join("family/auto-bindings-3v3.json");
    for metadata in ["d_in_sink", "d_in_source", "d_out_sink", "d_out_source"] {
        let source = dir.path().join(format!("{metadata}.cir"));
        let output = dir.path().join(format!("{metadata}.generated.cir"));
        fs::write(
            &source,
            original.replace(
                ".end\n",
                &format!("Arogue {metadata} UNKNOWN_MODEL\n.end\n"),
            ),
        )?;
        let result = autowire(&source, &defs, &mappings, &output)?;
        assert!(
            !result.status.success(),
            "accepted unverified A device on physical bundle metadata {metadata}"
        );
        assert!(!output.exists(), "failed {metadata} wrote source");
    }
    Ok(())
}

#[test]
fn compiled_alb_must_not_hide_unverified_physical_a_or_x_devices() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let defs = compile_alb(dir.path())?;
    let valid = fs::read_to_string(&defs)?;
    assert!(valid.contains(".ends ALB_BUFFER"));
    let deck = fixtures().join("family/auto-direct.cir");
    let mapping = fixtures().join("family/auto-bindings-3v3.json");
    for (name, addition) in [
        ("input", "Arogue sr_in0 UNKNOWN_MODEL\n"),
        ("output", "Arogue sr_out0_source UNKNOWN_MODEL\n"),
        ("nested", "Xrogue sr_in0 PROBE\n"),
    ] {
        let mut corrupted =
            valid.replace(".ends ALB_BUFFER", &format!("{addition}.ends ALB_BUFFER"));
        if name == "nested" {
            corrupted.push_str(".subckt PROBE tap\nArogue tap UNKNOWN_MODEL\n.ends PROBE\n");
        }
        fs::write(&defs, &corrupted)?;
        let output = dir.path().join(format!("{name}.generated.cir"));
        let result = autowire(&deck, &defs, &mapping, &output)?;
        assert!(
            !result.status.success(),
            "accepted an extra {name} code model inside compiled ALB implementation"
        );
        assert!(!output.exists(), "{name} produced invalid generated deck");
    }
    Ok(())
}

#[test]
fn actual_compiled_lut_cannot_read_unconnected_external_state() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let defs = compile_alb(dir.path())?;
    let valid = fs::read_to_string(&defs)?;
    let output_device = valid
        .lines()
        .find(|line| line.starts_with("Asr_o"))
        .context("compiled ALB contains no physical output driver")?;
    let (prefix, source_and_tail) = output_device
        .split_once('[')
        .context("compiled ALB output lacks source vector")?;
    let (_, suffix) = source_and_tail
        .split_once(']')
        .context("compiled ALB output source vector is unterminated")?;
    let corrupted = valid.replacen(output_device, &format!("{prefix}[UNBOUND]{suffix}"), 1);
    fs::write(&defs, corrupted)?;
    let output = dir.path().join("unbound-lut-source.cir");
    let result = autowire(
        &fixtures().join("family/auto-direct.cir"),
        &defs,
        &fixtures().join("family/auto-bindings-3v3.json"),
        &output,
    )?;
    assert!(
        !result.status.success(),
        "accepted compiled ALB LUT reading an undeclared source"
    );
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("LUT source"),
        "missing malformed source diagnostic: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(!output.exists(), "invalid autowired deck was written");
    Ok(())
}

#[test]
fn generated_bridge_names_are_checked_in_their_own_lexical_scope() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let defs = compile_alb(dir.path())?;
    let original = fs::read_to_string(fixtures().join("family/auto-direct.cir"))?;
    let mapping = fixtures().join("family/auto-bindings-3v3.json");

    // Each subcircuit owns its own model/device namespace. An unrelated
    // private declaration must not veto a root-level native bridge.
    for (case, addition) in [
        (
            "nested-model",
            ".subckt UNUSED tap\n.model MsrFamily0 d_buffer()\n.ends UNUSED\n",
        ),
        (
            "nested-device",
            ".subckt UNUSED tap\nAsrFamily0 tap 0 private_model\n.ends UNUSED\n",
        ),
        (
            "control-command",
            ".control\n.model MsrFamily0 nonspice_value\n.endc\n",
        ),
    ] {
        let source = dir.path().join(format!("{case}.cir"));
        let output = dir.path().join(format!("{case}.autowired.cir"));
        fs::write(
            &source,
            original.replace(".tran 20p", &format!("{addition}.tran 20p")),
        )?;
        let result = autowire(&source, &defs, &mapping, &output)?;
        assert!(
            result.status.success(),
            "{case} wrongly treated as a root name collision: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let generated = fs::read_to_string(&output)?;
        assert!(generated.contains(".model MsrFamily0 sr_family_adc("));
        assert!(generated.contains("AsrFamily0 %gd("));
    }

    // The same spelling in the root scope really does conflict and must
    // remain a hard error. Also check both device and .model symbols.
    for (case, addition) in [
        ("root-model", ".model MsrFamily0 d_buffer()\n"),
        ("root-device", "AsrFamily0 elsewhere other_model\n"),
    ] {
        let source = dir.path().join(format!("{case}.cir"));
        let output = dir.path().join(format!("{case}.autowired.cir"));
        fs::write(
            &source,
            original.replace(".tran 20p", &format!("{addition}.tran 20p")),
        )?;
        let result = autowire(&source, &defs, &mapping, &output)?;
        assert!(
            !result.status.success(),
            "{case} overwrote an existing root symbol"
        );
        assert!(!output.exists(), "{case} wrote invalid output");
    }
    Ok(())
}

#[test]
fn a_device_model_identifier_is_not_a_physical_node() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let defs = compile_alb(dir.path())?;
    let original = fs::read_to_string(fixtures().join("family/auto-direct.cir"))?;
    let source = dir.path().join("unrelated-model-identifier.cir");
    fs::write(
        &source,
        original.replace(
            ".tran 20p",
            "Aunrelated %gd(isolated 0) D_IN_SINK\n\
             .model D_IN_SINK d_pulldown(load=0)\n\
             .tran 20p",
        ),
    )?;
    let output = dir.path().join("unrelated-model-identifier.generated.cir");
    let result = autowire(
        &source,
        &defs,
        &fixtures().join("family/auto-bindings-3v3.json"),
        &output,
    )?;
    assert!(
        result.status.success(),
        "rejected model identifier sharing a protected node's name: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(output.exists());
    Ok(())
}

#[test]
fn bridge_model_names_on_unrelated_devices_are_not_physical_contacts() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let defs = compile_alb(dir.path())?;
    let original = fs::read_to_string(fixtures().join("family/auto-direct.cir"))?;
    for (case, device, acceptable) in [
        (
            "root",
            "Aunrelated %gd(isolated 0) other other_sink other_source D_IN\n",
            true,
        ),
        (
            "nested",
            "Xunrelated isolated PROBE\n\
             .subckt PROBE input\n\
             Aunrelated %gd(input 0) other other_sink other_source D_IN\n\
             .ends PROBE\n",
            true,
        ),
        (
            "connected",
            "Aexisting %gd(isolated 0) d_in d_in_sink d_in_source D_IN\n",
            false,
        ),
    ] {
        let source = dir.path().join(format!("{case}-model-name.cir"));
        fs::write(
            &source,
            original.replace(
                ".tran 20p",
                &format!("{device}.model D_IN sr_family_adc(in_low=1 in_high=2)\n.tran 20p"),
            ),
        )?;
        let output = dir.path().join(format!("{case}-out.cir"));
        let result = autowire(
            &source,
            &defs,
            &fixtures().join("family/auto-bindings-3v3.json"),
            &output,
        )?;
        assert_eq!(
            result.status.success(),
            acceptable,
            "{case}: incorrect model-name versus physical-terminal collision: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(output.exists(), acceptable);
    }
    Ok(())
}

#[test]
fn nested_a_device_on_native_bundle_metadata_is_rejected() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let defs = compile_alb(dir.path())?;
    let original = fs::read_to_string(fixtures().join("family/auto-direct.cir"))?;
    let source = dir.path().join("nested-metadata.cir");
    fs::write(
        &source,
        original.replace(
            ".tran 20p",
            "Xrogue d_in_sink PROBE\n\
             .subckt PROBE tap\n\
             Arogue tap UNKNOWN_MODEL\n\
             .ends PROBE\n\
             .tran 20p",
        ),
    )?;
    let output = dir.path().join("nested-metadata.generated.cir");
    let result = autowire(
        &source,
        &defs,
        &fixtures().join("family/auto-bindings-3v3.json"),
        &output,
    )?;
    assert!(
        !result.status.success(),
        "accepted A device through a nested X port"
    );
    assert!(!output.exists(), "invalid output was emitted");
    Ok(())
}

#[test]
fn multilevel_x_devices_only_block_their_actual_physical_boundary() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let defs = compile_alb(dir.path())?;
    let original = fs::read_to_string(fixtures().join("family/auto-direct.cir"))?;
    for (physical, must_reject) in [("d_out_source", true), ("unrelated", false)] {
        let source = dir.path().join(format!("{physical}.cir"));
        fs::write(
            &source,
            original.replace(
                ".tran 20p",
                &format!(
                    "Xrogue {physical} PROBE\n\
                     .subckt PROBE tap\n\
                     Xdeep tap SENSE\n\
                     .ends PROBE\n\
                     .subckt SENSE sensed\n\
                     Arogue %gd(sensed 0) unrelated UNKNOWN_MODEL\n\
                     .ends SENSE\n\
                     .tran 20p"
                ),
            ),
        )?;
        let output = dir.path().join(format!("{physical}.generated.cir"));
        let result = autowire(
            &source,
            &defs,
            &fixtures().join("family/auto-bindings-3v3.json"),
            &output,
        )?;
        assert_eq!(
            result.status.success(),
            !must_reject,
            "incorrect A-device collision decision on {physical}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(output.exists(), !must_reject);
    }
    Ok(())
}

#[test]
fn terminal_end_with_inline_comment_is_accepted() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let defs = compile_alb(dir.path())?;
    let original = fs::read_to_string(fixtures().join("family/auto-direct.cir"))?;
    let source = dir.path().join("commented-end.cir");
    fs::write(
        &source,
        original.replace(".end\n", ".END ; this is a valid terminal directive\n"),
    )?;
    let target = dir.path().join("commented-end-out.cir");
    let result = autowire(
        &source,
        &defs,
        &fixtures().join("family/auto-bindings-3v3.json"),
        &target,
    )?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let rendered = fs::read_to_string(target)?;
    assert!(rendered.contains("sr_family_adc("));
    assert!(rendered.contains(".END ; this is a valid terminal directive"));
    Ok(())
}

#[test]
fn full_source_must_have_single_terminal_end() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let defs = compile_alb(dir.path())?;
    let valid = fs::read_to_string(fixtures().join("family/auto-direct.cir"))?;
    for (suffix, deck) in [
        ("missing", valid.replace(".end\n", "")),
        ("duplicate", format!("{valid}.end\n")),
    ] {
        let source = dir.path().join(format!("{suffix}.cir"));
        let out = dir.path().join(format!("{suffix}.out.cir"));
        fs::write(&source, deck)?;
        let status = autowire(
            &source,
            &defs,
            &fixtures().join("family/auto-bindings-3v3.json"),
            &out,
        )?;
        assert!(
            !status.status.success(),
            "{suffix} .end unexpectedly accepted"
        );
        assert!(!out.exists());
    }
    Ok(())
}

#[test]
fn nonpositive_timing_resistance_requires_explicit_analog_conductance() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let mut electrical: serde_json::Value =
        serde_json::from_slice(&fs::read(fixtures().join("family/buffer-40r.json"))?)?;
    let settings = dir.path().join("electrical.json");
    let model = dir.path().join("alb.inc");
    let source = fixtures().join("family/auto-direct.cir");
    let mappings = fixtures().join("family/auto-bindings-3v3.json");
    let mut models: serde_json::Value =
        serde_json::from_slice(&fs::read(fixtures().join("family/auto-models.json"))?)?;
    let override_file = dir.path().join("explicit.json");
    fs::write(&override_file, serde_json::to_vec(&models)?)?;
    models["models"][1]["native"]["parameters"]["g_pullup"] = serde_json::Value::Null;
    models["models"][1]["native"]["parameters"]["g_pulldown"] = serde_json::Value::Null;
    let original_file = dir.path().join("non-explicit.json");
    fs::write(&original_file, serde_json::to_vec(&models)?)?;
    for resistance in [0, -1000] {
        electrical["output_out_res_ohm"] =
            serde_json::json!({"positive":resistance,"negative":resistance});
        fs::write(&settings, serde_json::to_vec(&electrical)?)?;
        let compiled = Command::new(env!("CARGO_BIN_EXE_alb-xspice-compile"))
            .arg(fixtures().join("bundle-buffer.alb"))
            .arg(&settings)
            .arg(&model)
            .arg("ALB_BUFFER")
            .output()?;
        assert!(
            compiled.status.success(),
            "{}",
            String::from_utf8_lossy(&compiled.stderr)
        );
        let good = dir.path().join(format!("good-{resistance}.cir"));
        let result = Command::new(env!("CARGO_BIN_EXE_alb-family-autowire"))
            .arg(&source)
            .arg(&model)
            .arg(&mappings)
            .arg(&override_file)
            .arg(&good)
            .output()?;
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let rendered = fs::read_to_string(good)?;
        assert!(rendered.contains("g_pullup=2.4e-2 g_pulldown=3.4e-2"));
        let invalid_output = dir.path().join(format!("bad-{resistance}.cir"));
        let bad = Command::new(env!("CARGO_BIN_EXE_alb-family-autowire"))
            .arg(&source)
            .arg(&model)
            .arg(&mappings)
            .arg(&original_file)
            .arg(&invalid_output)
            .output()?;
        assert!(
            !bad.status.success(),
            "nonpositive OUT_RES {resistance} cannot supply an implicit conductance"
        );
        assert!(!invalid_output.exists());
    }
    Ok(())
}
