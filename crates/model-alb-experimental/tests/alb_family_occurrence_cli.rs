//! Explicit root-occurrence electrical specialization of native ALB bridges.

use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../runtime/xspice/fixtures/family")
}
fn compile(src: &Path, cfg: &Path, dir: &Path) -> Result<Output> {
    Ok(Command::new(env!("CARGO_BIN_EXE_alb-family-compile"))
        .arg(src)
        .arg(cfg)
        .arg(fixtures().join("auto-models.json"))
        .arg(dir.join("final.cir"))
        .arg(dir.join("generated.inc"))
        .output()?)
}

#[test]
fn electrical_variants_are_shared_by_signature_and_independent_of_order() -> Result<()> {
    let src = fixtures().join("occurrence-3calls.cir");
    let cfg = fixtures().join("occurrence-3calls.json");
    let mut reference = None;
    for reverse in [false, true] {
        let temp = tempfile::tempdir()?;
        let source = if reverse {
            let original = fs::read_to_string(&src)?;
            let reordered = original.replace(
                "Xone in_a out_a TOP\nXtwo in_b out_b TOP\nXthree in_c out_c TOP",
                "Xthree in_c out_c TOP\nXone in_a out_a TOP\nXtwo in_b out_b TOP",
            );
            let path = temp.path().join("reordered.cir");
            fs::write(&path, reordered)?;
            path
        } else {
            src.clone()
        };
        let result = compile(&source, &cfg, temp.path())?;
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let deck = fs::read_to_string(temp.path().join("final.cir"))?;
        assert_eq!(deck.matches(".subckt TOP_SRV").count(), 2);
        let library = fs::read_to_string(temp.path().join("generated.inc"))?;
        assert_eq!(library.matches(".subckt SRALB").count(), 1, "{library}");
        assert_eq!(
            deck.lines()
                .filter(|line| line.starts_with("XSR_A1 ") && line.ends_with(" SRALB0"))
                .count(),
            2
        );
        assert_eq!(deck.matches("sr_family_adc(").count(), 2);
        assert_eq!(deck.matches("sr_family_dac(").count(), 2);
        assert!(!deck.contains(".subckt TOP ANALOG_IN ANALOG_OUT"));
        let calls = deck
            .lines()
            .filter(|line| {
                line.starts_with("Xone ")
                    || line.starts_with("Xtwo ")
                    || line.starts_with("Xthree ")
            })
            .map(str::to_owned)
            .collect::<Vec<_>>();
        assert_eq!(calls.len(), 3);
        let mut sorted = calls;
        sorted.sort();
        assert!(sorted.iter().any(|s| s == "Xone IN_A OUT_A TOP_SRV0"));
        assert!(sorted.iter().any(|s| s == "Xtwo IN_B OUT_B TOP_SRV1"));
        assert!(sorted.iter().any(|s| s == "Xthree IN_C OUT_C TOP_SRV0"));
        assert!(deck.contains("out_high=3.3") || deck.contains("out_high=3.3e0"));
        if let Some(previous) = &reference {
            assert_eq!(
                previous, &sorted,
                "variant assignment depends on source X order"
            );
        } else {
            reference = Some(sorted);
        }
    }
    Ok(())
}

#[test]
fn partially_annotated_occurrences_keep_the_original_definition() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let original: Value =
        serde_json::from_slice(&fs::read(fixtures().join("occurrence-3calls.json"))?)?;
    let mut config = original;
    config["connections"]
        .as_array_mut()
        .context("missing connections")?
        .retain(|entry| entry["occurrence"] != json!(["XTHREE"]));
    let config_path = temp.path().join("partial.json");
    fs::write(&config_path, serde_json::to_vec(&config)?)?;
    let result = compile(
        &fixtures().join("occurrence-3calls.cir"),
        &config_path,
        temp.path(),
    )?;
    assert!(
        result.status.success(),
        "mixed annotated/unannotated X occurrences rejected: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let deck = fs::read_to_string(temp.path().join("final.cir"))?;
    assert!(deck.contains(".subckt TOP ANALOG_IN ANALOG_OUT"), "{deck}");
    assert!(deck.contains("Xone IN_A OUT_A TOP_SRV0"), "{deck}");
    assert!(deck.contains("Xtwo IN_B OUT_B TOP_SRV1"), "{deck}");
    assert!(deck.contains("Xthree IN_C OUT_C TOP"), "{deck}");
    assert_eq!(deck.matches(".subckt TOP_SRV").count(), 2);
    assert_eq!(deck.matches("sr_family_adc(").count(), 2);
    assert_eq!(deck.matches("sr_family_dac(").count(), 2);
    Ok(())
}

#[test]
fn occurrence_specialization_rejects_unknown_definition_boundaries() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let cfg = fixtures().join("occurrence-3calls.json");
    let mut config: Value = serde_json::from_slice(&fs::read(cfg)?)?;
    let mut unknown = config["connections"][0].clone();
    unknown["scope"] = json!(["MISSING"]);
    unknown
        .as_object_mut()
        .context("boundary is not an object")?
        .remove("occurrence");
    config["connections"]
        .as_array_mut()
        .context("missing connections")?
        .push(unknown);
    let invalid = temp.path().join("missing-scope.json");
    fs::write(&invalid, serde_json::to_vec(&config)?)?;
    let result = compile(
        &fixtures().join("occurrence-3calls.cir"),
        &invalid,
        temp.path(),
    )?;
    assert!(
        !result.status.success(),
        "specialization silently discarded a definition boundary"
    );
    assert!(!temp.path().join("final.cir").exists());
    Ok(())
}

#[test]
fn each_occurrence_must_select_an_actual_physical_producer() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut config: Value =
        serde_json::from_slice(&fs::read(fixtures().join("occurrence-3calls.json"))?)?;
    // The first and second calls have the same lowered output state.
    // The latter cannot secretly select a different source producer simply
    // because the earlier occurrence's annotation was encountered first.
    config["connections"][3]["producer"] = json!({"path": ["A_NONEXISTENT"], "output_pin": 0});
    let cfg = temp.path().join("conflicting-producer.json");
    fs::write(&cfg, serde_json::to_vec(&config)?)?;
    let first = compile(&fixtures().join("occurrence-3calls.cir"), &cfg, temp.path())?;
    assert!(
        !first.status.success(),
        "logical expansion silently discarded a conflicting producer assignment"
    );
    assert!(!temp.path().join("final.cir").exists());
    assert!(!temp.path().join("generated.inc").exists());
    Ok(())
}

#[test]
fn invalid_occurrence_assignments_fail_without_native_deck() -> Result<()> {
    for case in [
        "unknown",
        "nested",
        "wrong-scope",
        "duplicate",
        "invalid-supply",
        "lexical-collision",
        "exported-state",
        "conflicting-electrical",
    ] {
        let temp = tempfile::tempdir()?;
        let mut value: Value =
            serde_json::from_slice(&fs::read(fixtures().join("occurrence-3calls.json"))?)?;
        match case {
            "unknown" => value["connections"][0]["occurrence"] = json!(["XNOTTHERE"]),
            "nested" => value["connections"][0]["occurrence"] = json!(["XONE", "XINNER"]),
            "wrong-scope" => value["connections"][0]["scope"] = json!(["MISSING"]),
            "duplicate" => {
                let entry = value["connections"][0].clone();
                value["connections"]
                    .as_array_mut()
                    .context("no connections")?
                    .push(entry);
            }
            "invalid-supply" => value["connections"][0]["supply_v"] = json!(0),
            "conflicting-electrical" => {
                let mut entry = value["connections"][0].clone();
                entry["supply_v"] = json!(5.0);
                value["connections"]
                    .as_array_mut()
                    .context("no connections")?
                    .push(entry);
            }
            "lexical-collision" => {
                value["connections"][0]
                    .as_object_mut()
                    .context("mapping not object")?
                    .remove("occurrence");
            }
            "exported-state" => value["connections"][0]["digital_net"] = json!("ANALOG_OUT"),
            _ => unreachable!(),
        }
        let cfg = temp.path().join("config.json");
        fs::write(&cfg, serde_json::to_vec(&value)?)?;
        let result = compile(&fixtures().join("occurrence-3calls.cir"), &cfg, temp.path())?;
        assert!(!result.status.success(), "{case} was accepted");
        assert!(
            !temp.path().join("final.cir").exists(),
            "{case} wrote invalid native circuit"
        );
        assert!(
            !temp.path().join("generated.inc").exists(),
            "{case} wrote invalid native library"
        );
    }
    Ok(())
}

#[test]
fn electrical_specialization_preserves_unused_nested_lexical_definitions() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let source = fixtures().join("occurrence-3calls.cir");
    let config = fixtures().join("occurrence-3calls.json");
    let original = fs::read_to_string(source)?;
    let mutated = temp.path().join("source.cir");
    fs::write(
        &mutated,
        original.replace(
            ".ends TOP\n",
            ".subckt INNER p q\nRin p q 1k\n.ends INNER\n.ends TOP\n",
        ),
    )?;
    let result = compile(&mutated, &config, temp.path())?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let deck = fs::read_to_string(temp.path().join("final.cir"))?;
    assert_eq!(deck.matches(".subckt INNER P Q").count(), 2);
    assert!(deck.contains(".subckt TOP_SRV0"));
    assert!(deck.contains(".subckt TOP_SRV1"));
    Ok(())
}

#[test]
fn original_occurrence_variants_have_independent_port_topology() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let result = compile(
        &fixtures().join("occurrence-heterogeneous.cir"),
        &fixtures().join("occurrence-heterogeneous.json"),
        temp.path(),
    )?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let deck = fs::read_to_string(temp.path().join("final.cir"))?;
    assert!(
        deck.contains(".subckt TOP_SRV0 ANALOG_IN ANALOG_OUT"),
        "{deck}"
    );
    assert!(
        deck.contains(
            ".subckt TOP_SRV1 ANALOG_IN ANALOG_IN_SR_SINK ANALOG_IN_SR_SOURCE ANALOG_OUT"
        ),
        "{deck}"
    );
    assert!(deck.contains("Xone IN_A OUT_A TOP_SRV0"), "{deck}");
    assert!(
        deck.contains("Xtwo BUS BUS_SR_SINK BUS_SR_SOURCE OUT_B TOP_SRV1"),
        "{deck}"
    );
    // Root ADC and Xone ADC; neither the root digital state nor Xtwo
    // receives a redundant ADC/DAC round trip.
    assert_eq!(deck.matches("sr_family_adc(").count(), 2);
    assert_eq!(deck.matches("sr_family_dac(").count(), 2);
    Ok(())
}
