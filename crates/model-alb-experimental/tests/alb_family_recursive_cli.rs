//! Real original-SIMetrix ALB nested occurrence specialization,
//! including lexical model and parameter scopes.

use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::{fs, path::Path, process::Output};

#[path = "support/family.rs"]
mod harness;
use harness::fixtures;
fn compile(source: &Path, config: &Path, dir: &Path) -> Result<Output> {
    harness::run(
        env!("CARGO_BIN_EXE_alb-family-compile"),
        &[source, config, &fixtures().join("auto-models.json")],
        dir,
        &["final.cir", "generated.inc"],
    )
}

#[test]
fn recursive_occurrence_variants_preserve_actual_model_hierarchy() -> Result<()> {
    for variant in ["root", "lexical", "fanout"] {
        let dir = tempfile::tempdir()?;
        let source = fixtures().join(format!("occurrence-nested-{variant}.cir"));
        let config = fixtures().join(format!("occurrence-nested-{variant}.json"));
        let output = compile(&source, &config, dir.path())?;
        assert!(
            output.status.success(),
            "{variant}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let deck = fs::read_to_string(dir.path().join("final.cir"))?;
        assert_eq!(deck.matches(".subckt INNER_SRV").count(), 2);
        assert_eq!(deck.matches("sr_family_adc(").count(), 2);
        assert_eq!(deck.matches("sr_family_dac(").count(), 2);
        assert!(!deck.contains(".subckt INNER PIN_IN PIN_OUT"));
        if variant == "fanout" {
            assert_eq!(deck.matches(".subckt OUTER_SRV").count(), 1);
            assert!(deck.contains("Xleft INPUT_A OUTPUT_A INNER_SRV0"));
            assert!(deck.contains("Xright INPUT_B OUTPUT_B INNER_SRV1"));
        } else {
            assert_eq!(deck.matches(".subckt OUTER_SRV").count(), 2);
            assert!(deck.contains("Xone IN_A OUT_A OUTER_SRV0"));
            assert!(deck.contains("Xtwo IN_B OUT_B OUTER_SRV1"));
            assert!(deck.contains("Xthree IN_C OUT_C OUTER_SRV0"));
        }
        if variant == "lexical" {
            assert_eq!(deck.matches(".param RMON=100meg").count(), 2);
            let begin = deck.find(".subckt OUTER_SRV0").context("missing OUTER")?;
            let inner = deck[begin..]
                .find(".subckt INNER_SRV0")
                .context("missing INNER")?
                + begin;
            let inner_end = deck[inner..]
                .find(".ends INNER_SRV0")
                .context("missing INNER end")?
                + inner;
            let outer_end = deck[begin..]
                .find(".ends OUTER_SRV0")
                .context("missing OUTER end")?
                + begin;
            assert!(begin < inner && inner < inner_end && inner_end < outer_end);
            assert!(
                deck[inner..inner_end]
                    .to_ascii_uppercase()
                    .contains("RMONITOR PIN_OUT 0 {RMON}")
            );
        }
    }
    Ok(())
}
#[test]
fn nested_variant_assignment_is_independent_of_source_call_order() -> Result<()> {
    let src = fixtures().join("occurrence-nested-root.cir");
    let cfg = fixtures().join("occurrence-nested-root.json");
    let original = fs::read_to_string(&src)?;
    for order in [false, true] {
        let dir = tempfile::tempdir()?;
        let content = if order {
            original.replace(
                "Xone in_a out_a OUTER\nXtwo in_b out_b OUTER\nXthree in_c out_c OUTER",
                "Xthree in_c out_c OUTER\nXtwo in_b out_b OUTER\nXone in_a out_a OUTER",
            )
        } else {
            original.clone()
        };
        let source = dir.path().join("source.cir");
        fs::write(&source, content)?;
        let output = compile(&source, &cfg, dir.path())?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let deck = fs::read_to_string(dir.path().join("final.cir"))?;
        assert!(deck.contains("Xone IN_A OUT_A OUTER_SRV0"));
        assert!(deck.contains("Xtwo IN_B OUT_B OUTER_SRV1"));
        assert!(deck.contains("Xthree IN_C OUT_C OUTER_SRV0"));
    }
    Ok(())
}
#[test]
fn partially_annotated_nested_occurrences_preserve_original_wrappers() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let source = fixtures().join("occurrence-nested-root.cir");
    let mut config: Value =
        serde_json::from_slice(&fs::read(fixtures().join("occurrence-nested-root.json"))?)?;
    config["connections"]
        .as_array_mut()
        .context("missing nested mapping entries")?
        .retain(|entry| entry["occurrence"] != json!(["XTWO", "XINNER"]));
    let config_file = temp.path().join("partial-nested.json");
    fs::write(&config_file, serde_json::to_vec(&config)?)?;
    let wired = compile(&source, &config_file, temp.path())?;
    assert!(
        wired.status.success(),
        "{}",
        String::from_utf8_lossy(&wired.stderr)
    );
    let deck = fs::read_to_string(temp.path().join("final.cir"))?;
    assert!(deck.contains("Xone IN_A OUT_A OUTER_SRV0"), "{deck}");
    assert!(deck.contains("Xtwo IN_B OUT_B OUTER"), "{deck}");
    assert!(deck.contains("Xthree IN_C OUT_C OUTER_SRV0"), "{deck}");
    assert!(
        deck.contains(".subckt OUTER ANALOG_IN ANALOG_OUT"),
        "{deck}"
    );
    assert!(deck.contains(".subckt INNER PIN_IN PIN_OUT"), "{deck}");
    assert_eq!(deck.matches(".subckt OUTER_SRV").count(), 1);
    assert_eq!(deck.matches(".subckt INNER_SRV").count(), 1);
    assert_eq!(deck.matches("sr_family_adc(").count(), 1);
    assert_eq!(deck.matches("sr_family_dac(").count(), 1);
    Ok(())
}

#[test]
fn nested_unknown_occurrences_reject_without_partial_output() -> Result<()> {
    let source = fixtures().join("occurrence-nested-root.cir");
    for kind in [
        "unknown-inner",
        "wrong-lexical-scope",
        "duplicate-boundary",
        "zero-supply",
        "bad-path",
    ] {
        let dir = tempfile::tempdir()?;
        let mut cfg: Value =
            serde_json::from_slice(&fs::read(fixtures().join("occurrence-nested-root.json"))?)?;
        match kind {
            "unknown-inner" => cfg["connections"][0]["occurrence"] = json!(["XONE", "XNOTHERE"]),
            "wrong-lexical-scope" => cfg["connections"][0]["scope"] = json!(["OUTER", "INNER"]),
            "duplicate-boundary" => {
                let entry = cfg["connections"][0].clone();
                cfg["connections"]
                    .as_array_mut()
                    .context("missing mappings")?
                    .push(entry);
            }
            "zero-supply" => cfg["connections"][0]["supply_v"] = json!(0),
            "bad-path" => cfg["connections"][0]["occurrence"] = json!(["XONE", "XINNER", "XOTHER"]),
            _ => unreachable!(),
        }
        let config = dir.path().join("config.json");
        fs::write(&config, serde_json::to_vec(&cfg)?)?;
        let result = compile(&source, &config, dir.path())?;
        assert!(!result.status.success(), "{kind} unexpectedly accepted");
        assert!(!dir.path().join("final.cir").exists());
        assert!(!dir.path().join("generated.inc").exists());
    }
    Ok(())
}
#[test]
fn reachable_recursive_model_calls_fail_before_native_output() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let cfg = fixtures().join("occurrence-nested-root.json");
    let source = dir.path().join("source.cir");
    let deck = fs::read_to_string(fixtures().join("occurrence-nested-root.cir"))?;
    let modified = deck.replace(
        ".tran 20p 15n",
        "Xcycle 0 0 LOOP\n.subckt LOOP p q\nXself p q LOOP\n.ends LOOP\n.tran 20p 15n",
    );
    fs::write(&source, modified)?;
    let result = compile(&source, &cfg, dir.path())?;
    assert!(
        !result.status.success(),
        "reachable recursive X model silently accepted"
    );
    assert!(!dir.path().join("final.cir").exists());
    Ok(())
}
