//! Experimental exported analog-formal boundary cuts over original
//! `SIMetrix` ALB A devices and nested mixed-signal hierarchy.

use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::{fs, path::Path, process::Output};

#[path = "support/family.rs"]
mod harness;
use harness::fixtures;
fn expand(source: &Path, config: &Path, dir: &Path) -> Result<Output> {
    harness::run(
        env!("CARGO_BIN_EXE_alb-logical-expand"),
        &[source, config],
        dir,
        &["lowered.cir", "generated.inc"],
    )
}
fn autowire(config: &Path, dir: &Path) -> Result<Output> {
    harness::run(
        env!("CARGO_BIN_EXE_alb-family-autowire"),
        &[
            &dir.join("lowered.cir"),
            &dir.join("generated.inc"),
            config,
            &fixtures().join("auto-models.json"),
        ],
        dir,
        &["circuit.cir"],
    )
}

#[test]
fn exported_analog_ports_are_not_rewritten_into_digital_triples() -> Result<()> {
    for (fixture, config, header, adc_count, dac_count) in [
        (
            "exported-original.cir",
            "exported-original-3v3.json",
            ".subckt TOP ANALOG_IN ANALOG_OUT",
            1,
            1,
        ),
        (
            "exported-original.cir",
            "exported-original-5v.json",
            ".subckt TOP ANALOG_IN ANALOG_OUT",
            1,
            1,
        ),
        (
            "exported-nested.cir",
            "exported-nested-3v3.json",
            ".subckt INNER PIN_IN PIN_OUT",
            1,
            1,
        ),
        (
            "exported-shared.cir",
            "exported-shared.json",
            ".subckt TOP INPUT",
            1,
            0,
        ),
    ] {
        let temp = tempfile::tempdir()?;
        let cfg = fixtures().join(config);
        let result = expand(&fixtures().join(fixture), &cfg, temp.path())?;
        assert!(
            result.status.success(),
            "{fixture}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let wiring = autowire(&cfg, temp.path())?;
        assert!(
            wiring.status.success(),
            "{fixture}: {}",
            String::from_utf8_lossy(&wiring.stderr)
        );
        let deck = fs::read_to_string(temp.path().join("circuit.cir"))?;
        assert!(
            deck.contains(header),
            "{fixture} formal ports were changed:\n{deck}"
        );
        assert_eq!(deck.matches("sr_family_adc(").count(), adc_count);
        assert_eq!(deck.matches("sr_family_dac(").count(), dac_count);
        assert!(deck.contains("AsrFamily0 %gd("));
        assert!(deck.contains("D_IN D_IN_SR_SINK D_IN_SR_SOURCE"));
        assert!(!deck.contains("d_logic_block"));
        if fixture == "exported-original.cir" {
            assert!(deck.contains("Xone IN_A OUT_A TOP"));
            assert!(deck.contains("Xtwo IN_B OUT_B TOP"));
            assert!(
                deck.contains("AsrFamily1 D_OUT D_OUT_SR_SINK D_OUT_SR_SOURCE %gd(ANALOG_OUT 0)")
            );
        } else if fixture == "exported-nested.cir" {
            assert!(deck.contains("Xinner PUBLIC_IN PUBLIC_OUT INNER"));
            assert!(deck.contains("Xtop ANALOG_IN ANALOG_OUT OUTER"));
            assert!(deck.contains("AsrFamily1 D_OUT D_OUT_SR_SINK D_OUT_SR_SOURCE %gd(PIN_OUT 0)"));
        } else if fixture == "exported-shared.cir" {
            assert_eq!(
                deck.matches(" D_IN D_IN_SR_SINK D_IN_SR_SOURCE ").count(),
                3
            );
        }
    }
    Ok(())
}

#[test]
fn exported_analog_boundaries_cannot_reuse_exported_digital_formals() -> Result<()> {
    let src = fixtures().join("exported-original.cir");
    for case in [
        "self-alias",
        "other-exported",
        "unknown-scope",
        "duplicate",
        "no-input-cut",
        "bad-family",
        "bad-supply",
    ] {
        let temp = tempfile::tempdir()?;
        let mut cfg: Value =
            serde_json::from_slice(&fs::read(fixtures().join("exported-original-3v3.json"))?)?;
        match case {
            "self-alias" => cfg["connections"][0]["digital_net"] = json!("analog_in"),
            "other-exported" => cfg["connections"][0]["digital_net"] = json!("analog_out"),
            "unknown-scope" => cfg["connections"][0]["scope"] = json!(["UNKNOWN"]),
            "duplicate" => {
                let original = cfg["connections"][0].clone();
                cfg["connections"]
                    .as_array_mut()
                    .context("missing connections")?
                    .push(original);
            }
            "no-input-cut" => {
                cfg["connections"]
                    .as_array_mut()
                    .context("missing connections")?
                    .remove(0);
            }
            "bad-family" => cfg["connections"][0]["in_family"] = json!("MISSING"),
            "bad-supply" => cfg["connections"][0]["supply_v"] = json!(0),
            _ => unreachable!(),
        }
        let config = temp.path().join("config.json");
        fs::write(&config, serde_json::to_vec(&cfg)?)?;
        let first = expand(&src, &config, temp.path())?;
        if first.status.success() {
            let second = autowire(&config, temp.path())?;
            assert!(!second.status.success(), "{case} should reject");
            assert!(!temp.path().join("circuit.cir").exists());
        } else {
            assert!(!temp.path().join("lowered.cir").exists());
            assert!(!temp.path().join("generated.inc").exists());
        }
    }
    Ok(())
}

#[test]
fn unannotated_input_cannot_silently_propagate_digital_role_to_analog_parent() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut config: Value =
        serde_json::from_slice(&fs::read(fixtures().join("exported-original-3v3.json"))?)?;
    // Only the output port is explicitly analog. Input remains propagated
    // digital formal and collides with the original root Vin sources.
    config["connections"]
        .as_array_mut()
        .context("missing connections")?
        .remove(0);
    let cfg = temp.path().join("config.json");
    fs::write(&cfg, serde_json::to_vec(&config)?)?;
    let result = expand(&fixtures().join("exported-original.cir"), &cfg, temp.path())?;
    assert!(
        !result.status.success(),
        "input analog/digital crossing silently guessed"
    );
    Ok(())
}

#[test]
fn sibling_digital_formal_propagation_survives_an_unrelated_analog_cut() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut src = fs::read_to_string(fixtures().join("exported-original.cir"))?;
    src = src.replace(
        "Xtwo in_b out_b TOP",
        "Xtwo in_b out_b TOP\nXdigital source_n sink_n SIBLING",
    );
    src=src.replace(".tran 20p 15n",
        ".subckt SIBLING digital_in digital_out\nA1 [digital_in] [digital_out] DL\n.model DL d_logic_block input_load=2p out_res=40 out_delay=2n def=\"out[0] = in[0];\"\n.ends SIBLING\n.tran 20p 15n");
    let deck = temp.path().join("sibling.cir");
    fs::write(&deck, src)?;
    let cfg = fixtures().join("exported-original-3v3.json");
    let result = expand(&deck, &cfg, temp.path())?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let text = fs::read_to_string(temp.path().join("lowered.cir"))?;
    assert!(text.contains("Xdigital SOURCE_N SOURCE_N_SR_SINK SOURCE_N_SR_SOURCE SINK_N SINK_N_SR_SINK SINK_N_SR_SOURCE SIBLING"));
    assert!(text.contains(".subckt SIBLING DIGITAL_IN DIGITAL_IN_SR_SINK DIGITAL_IN_SR_SOURCE DIGITAL_OUT DIGITAL_OUT_SR_SINK DIGITAL_OUT_SR_SOURCE"));
    assert!(text.contains(".subckt TOP ANALOG_IN ANALOG_OUT"));
    Ok(())
}

#[test]
fn analog_and_digital_exported_formals_coexist_on_one_netlist_graph() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let cfg = fixtures().join("exported-mixed.json");
    let result = expand(&fixtures().join("exported-mixed.cir"), &cfg, temp.path())?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let wired = autowire(&cfg, temp.path())?;
    assert!(
        wired.status.success(),
        "{}",
        String::from_utf8_lossy(&wired.stderr)
    );
    let text = fs::read_to_string(temp.path().join("circuit.cir"))?;
    assert!(
        text.contains(
            ".subckt PRODUCER PIN_IN PIN_DIGITAL PIN_DIGITAL_SR_SINK PIN_DIGITAL_SR_SOURCE"
        ),
        "{text}"
    );
    assert!(
        text.contains(
            ".subckt CONSUMER PIN_DIGITAL PIN_DIGITAL_SR_SINK PIN_DIGITAL_SR_SOURCE PIN_OUT"
        ),
        "{text}"
    );
    assert!(
        text.contains("Xproducer ANALOG_IN BUS BUS_SR_SINK BUS_SR_SOURCE PRODUCER"),
        "{text}"
    );
    assert!(text.contains("Xconsumer_a BUS BUS_SR_SINK BUS_SR_SOURCE OUT_A CONSUMER"));
    assert!(text.contains("Xconsumer_b BUS BUS_SR_SINK BUS_SR_SOURCE OUT_B CONSUMER"));
    assert_eq!(text.matches("sr_family_adc(").count(), 1);
    assert_eq!(text.matches("sr_family_dac(").count(), 1);
    let before = text.find(".subckt CONSUMER").context("missing consumer")?;
    let after = text
        .find(".ends CONSUMER")
        .context("missing consumer ends")?;
    let consumer = &text[before..after];
    assert!(consumer.contains("AsrFamily0 D_OUT D_OUT_SR_SINK D_OUT_SR_SOURCE %gd(PIN_OUT 0)"));
    assert!(consumer.contains("XSR_A1 PIN_DIGITAL PIN_DIGITAL_SR_SINK PIN_DIGITAL_SR_SOURCE"));
    Ok(())
}

#[test]
fn unrecognized_a_device_on_an_exported_analog_cut_fails_closed() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let original = fs::read_to_string(fixtures().join("exported-original.cir"))?;
    let altered = original.replace(
        "A1 [analog_in] [analog_out] DL",
        "A1 [analog_in] [analog_out] DL\nArogue [analog_in] [other] UNKNOWN",
    );
    let file = temp.path().join("rogue.cir");
    fs::write(&file, altered)?;
    let cfg = fixtures().join("exported-original-3v3.json");
    let result = expand(&file, &cfg, temp.path())?;
    assert!(
        !result.status.success(),
        "unknown XSPICE A device was accepted on the bridge boundary"
    );
    assert!(!temp.path().join("lowered.cir").exists());
    assert!(!temp.path().join("generated.inc").exists());
    Ok(())
}

#[test]
fn exporting_an_analog_formal_onto_an_unsplit_digital_bus_is_rejected() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let original = fs::read_to_string(fixtures().join("exported-mixed.cir"))?;
    let broken = original.replace(
        "Xconsumer_a bus out_a CONSUMER",
        "Xconsumer_a bus bus CONSUMER",
    );
    let src = temp.path().join("collided.cir");
    fs::write(&src, broken)?;
    let config = fixtures().join("exported-mixed.json");
    let result = expand(&src, &config, temp.path())?;
    assert!(
        !result.status.success(),
        "unsplit physical digital bus was exported as an analog output"
    );
    assert!(!temp.path().join("lowered.cir").exists());
    Ok(())
}
