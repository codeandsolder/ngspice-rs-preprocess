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
fn expand(src: &Path, cfg: &Path, dir: &Path) -> Result<Output> {
    Ok(Command::new(env!("CARGO_BIN_EXE_alb-logical-expand"))
        .arg(src)
        .arg(cfg)
        .arg(dir.join("lowered.cir"))
        .arg(dir.join("generated.inc"))
        .output()?)
}
fn wire(cfg: &Path, dir: &Path) -> Result<Output> {
    Ok(Command::new(env!("CARGO_BIN_EXE_alb-family-autowire"))
        .arg(dir.join("lowered.cir"))
        .arg(dir.join("generated.inc"))
        .arg(cfg)
        .arg(fixtures().join("auto-models.json"))
        .arg(dir.join("final.cir"))
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
        let first = expand(&source, &cfg, temp.path())?;
        assert!(
            first.status.success(),
            "{}",
            String::from_utf8_lossy(&first.stderr)
        );
        let result = wire(&cfg, temp.path())?;
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let deck = fs::read_to_string(temp.path().join("final.cir"))?;
        assert_eq!(deck.matches(".subckt TOP_SRV").count(), 2);
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
fn invalid_occurrence_assignments_fail_without_native_deck() -> Result<()> {
    for case in [
        "unknown",
        "nested",
        "wrong-scope",
        "unassigned",
        "duplicate",
        "invalid-supply",
        "lexical-collision",
        "different-structure",
        "conflicting-electrical",
    ] {
        let temp = tempfile::tempdir()?;
        let mut value: Value =
            serde_json::from_slice(&fs::read(fixtures().join("occurrence-3calls.json"))?)?;
        match case {
            "unknown" => value["connections"][0]["occurrence"] = json!(["XNOTTHERE"]),
            "nested" => value["connections"][0]["occurrence"] = json!(["XONE", "XINNER"]),
            "wrong-scope" => value["connections"][0]["scope"] = json!(["MISSING"]),
            "unassigned" => {
                value["connections"]
                    .as_array_mut()
                    .context("no connections")?
                    .retain(|entry| entry["occurrence"] != json!(["XTHREE"]));
            }
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
            "different-structure" => value["connections"][0]["digital_net"] = json!("DIFFERENT"),
            _ => unreachable!(),
        }
        let cfg = temp.path().join("config.json");
        fs::write(&cfg, serde_json::to_vec(&value)?)?;
        let initial = expand(&fixtures().join("occurrence-3calls.cir"), &cfg, temp.path())?;
        if initial.status.success() {
            let result = wire(&cfg, temp.path())?;
            assert!(!result.status.success(), "{case} was accepted");
            assert!(
                !temp.path().join("final.cir").exists(),
                "{case} wrote invalid native circuit"
            );
        } else {
            assert!(!temp.path().join("lowered.cir").exists());
            assert!(!temp.path().join("generated.inc").exists());
        }
    }
    Ok(())
}

#[test]
fn electrical_specialization_preserves_unused_nested_lexical_definitions() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let source = fixtures().join("occurrence-3calls.cir");
    let config = fixtures().join("occurrence-3calls.json");
    let first = expand(&source, &config, temp.path())?;
    assert!(first.status.success());
    let lowered = temp.path().join("lowered.cir");
    let original = fs::read_to_string(&lowered)?;
    fs::write(
        &lowered,
        original.replace(
            ".ends TOP\n",
            ".subckt INNER p q\nRin p q 1k\n.ends INNER\n.ends TOP\n",
        ),
    )?;
    let result = wire(&config, temp.path())?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let deck = fs::read_to_string(temp.path().join("final.cir"))?;
    assert_eq!(deck.matches(".subckt INNER p q").count(), 2);
    assert!(deck.contains(".subckt TOP_SRV0"));
    assert!(deck.contains(".subckt TOP_SRV1"));
    Ok(())
}
