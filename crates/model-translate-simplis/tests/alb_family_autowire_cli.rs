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
        assert!(generated.contains("g_pullup=2.5e-2 g_pulldown=2.5e-2"));
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
fn ideal_compiled_output_only_requires_finite_conductance_when_not_overridden() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let mut electrical: serde_json::Value =
        serde_json::from_slice(&fs::read(fixtures().join("family/buffer-40r.json"))?)?;
    electrical["output_out_res_ohm"] = serde_json::json!(0);
    let settings = dir.path().join("ideal.json");
    fs::write(&settings, serde_json::to_vec(&electrical)?)?;
    let model = dir.path().join("ideal-alb.inc");
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
    let source = fixtures().join("family/auto-direct.cir");
    let mappings = fixtures().join("family/auto-bindings-3v3.json");
    let models: serde_json::Value =
        serde_json::from_slice(&fs::read(fixtures().join("family/auto-models.json"))?)?;
    let mut explicit = models.clone();
    explicit["models"][1]["native"]["parameters"]["g_pullup"] = serde_json::json!(0.024);
    explicit["models"][1]["native"]["parameters"]["g_pulldown"] = serde_json::json!(0.034);
    let override_file = dir.path().join("explicit.json");
    fs::write(&override_file, serde_json::to_vec(&explicit)?)?;
    let good = dir.path().join("good.cir");
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
    let original_file = dir.path().join("non-explicit.json");
    fs::write(&original_file, serde_json::to_vec(&models)?)?;
    let invalid_output = dir.path().join("bad.cir");
    let bad = Command::new(env!("CARGO_BIN_EXE_alb-family-autowire"))
        .arg(&source)
        .arg(&model)
        .arg(&mappings)
        .arg(&original_file)
        .arg(&invalid_output)
        .output()?;
    assert!(
        !bad.status.success(),
        "zero OUT_RES was incorrectly inverted to infinity"
    );
    assert!(!invalid_output.exists());
    Ok(())
}
