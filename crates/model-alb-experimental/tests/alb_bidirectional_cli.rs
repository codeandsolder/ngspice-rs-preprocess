//! Original `SIMetrix` ALB with a single analog inout pin and independent
//! native sensing and driving bundles. No pre-split digital source nodes.

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
fn expand(source: &Path, config: &Path, dir: &Path) -> Result<Output> {
    Ok(Command::new(env!("CARGO_BIN_EXE_alb-logical-expand"))
        .arg(source)
        .arg(config)
        .arg(dir.join("lowered.cir"))
        .arg(dir.join("generated.inc"))
        .output()?)
}
fn wire(config: &Path, dir: &Path) -> Result<Output> {
    Ok(Command::new(env!("CARGO_BIN_EXE_alb-family-autowire"))
        .arg(dir.join("lowered.cir"))
        .arg(dir.join("generated.inc"))
        .arg(config)
        .arg(fixtures().join("auto-models.json"))
        .arg(dir.join("circuit.cir"))
        .output()?)
}
#[test]
fn input_and_output_of_one_original_alb_pin_have_distinct_physical_bundles() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config = fixtures().join("bidirectional-port.json");
    let result = expand(
        &fixtures().join("bidirectional-port.cir"),
        &config,
        temp.path(),
    )?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let lowered = fs::read_to_string(temp.path().join("lowered.cir"))?;
    assert!(lowered.contains(".subckt TOP PIN OE MONITOR"));
    assert!(lowered.contains(
        "XSR_A1 D_SENSE D_SENSE_SR_SINK D_SENSE_SR_SOURCE \
D_OE D_OE_SR_SINK D_OE_SR_SOURCE \
D_DRIVE D_DRIVE_SR_SINK D_DRIVE_SR_SOURCE \
D_MON D_MON_SR_SINK D_MON_SR_SOURCE SRALB0"
    ));
    assert!(!lowered.contains("PIN PIN_SR_SINK"));
    let result = wire(&config, temp.path())?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let deck = fs::read_to_string(temp.path().join("circuit.cir"))?;
    assert!(deck.contains("AsrFamily2 %gd(PIN 0) D_SENSE D_SENSE_SR_SINK D_SENSE_SR_SOURCE"));
    assert!(deck.contains("AsrFamily3 D_DRIVE D_DRIVE_SR_SINK D_DRIVE_SR_SOURCE %gd(PIN 0)"));
    assert_eq!(deck.matches("sr_family_adc(").count(), 2);
    assert_eq!(deck.matches("sr_family_dac(").count(), 2);
    assert!(fs::read_to_string(temp.path().join("generated.inc"))?.contains("table_values=\"Z1\""));
    Ok(())
}
#[test]
fn bidirectional_invalid_role_definitions_fail_before_output() -> Result<()> {
    for case in [
        "missing-sense",
        "missing-drive",
        "same-digital",
        "sense-short-analog",
        "drive-short-analog",
        "duplicate-role",
        "other-signal-alias",
        "wrong-direction",
    ] {
        let temp = tempfile::tempdir()?;
        let mut cfg: Value =
            serde_json::from_slice(&fs::read(fixtures().join("bidirectional-port.json"))?)?;
        match case {
            "missing-sense" => {
                cfg["connections"]
                    .as_array_mut()
                    .context("no connections")?
                    .remove(0);
            }
            "missing-drive" => {
                cfg["connections"]
                    .as_array_mut()
                    .context("no connections")?
                    .remove(1);
            }
            "same-digital" => cfg["connections"][1]["digital_net"] = json!("D_SENSE"),
            "sense-short-analog" => cfg["connections"][0]["digital_net"] = json!("PIN"),
            "drive-short-analog" => cfg["connections"][1]["digital_net"] = json!("PIN"),
            "duplicate-role" => cfg["connections"][1]["direction"] = json!("input"),
            "other-signal-alias" => cfg["connections"][3]["digital_net"] = json!("D_SENSE"),
            "wrong-direction" => cfg["connections"][3]["direction"] = json!("input"),
            _ => unreachable!(),
        }
        let config = temp.path().join("config.json");
        fs::write(&config, serde_json::to_vec(&cfg)?)?;
        let result = expand(
            &fixtures().join("bidirectional-port.cir"),
            &config,
            temp.path(),
        )?;
        if result.status.success() {
            let result = wire(&config, temp.path())?;
            assert!(!result.status.success(), "{case} was silently accepted");
            assert!(
                !temp.path().join("circuit.cir").exists(),
                "{case} wrote a native deck"
            );
        } else {
            assert!(
                !temp.path().join("lowered.cir").exists(),
                "{case} wrote lowered source"
            );
            assert!(
                !temp.path().join("generated.inc").exists(),
                "{case} wrote compiled model"
            );
        }
    }
    Ok(())
}
#[test]
fn a_later_digital_state_name_cannot_collide_with_generated_metadata() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut cfg: Value =
        serde_json::from_slice(&fs::read(fixtures().join("bidirectional-port.json"))?)?;
    cfg["connections"][3]["digital_net"] = json!("D_SENSE_SR_SINK");
    let config = temp.path().join("config.json");
    fs::write(&config, serde_json::to_vec(&cfg)?)?;
    let source = fixtures().join("bidirectional-port.cir");
    let result = expand(&source, &config, temp.path())?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let lowered = fs::read_to_string(temp.path().join("lowered.cir"))?;
    assert!(lowered.contains("D_SENSE_SR_SINK"));
    assert!(lowered.contains("D_SENSE SR_META"));
    let result = wire(&config, temp.path())?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    Ok(())
}
#[test]
fn bidirectional_pin_routes_sensing_only_child_to_adc_bundle() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let source = fs::read_to_string(fixtures().join("bidirectional-port.cir"))?;
    let source=source.replace(
        ".ends TOP",
        "Xother pin other CHILD\n.ends TOP\n.subckt CHILD port other\nA2 [port] [other] DCH\n.model DCH d_logic_block out_delay=2n def=\"out[0] = in[0];\"\n.ends CHILD"
    );
    let file = temp.path().join("source.cir");
    fs::write(&file, source)?;
    let result = expand(
        &file,
        &fixtures().join("bidirectional-port.json"),
        temp.path(),
    )?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let lowered = fs::read_to_string(temp.path().join("lowered.cir"))?;
    assert!(
        lowered.contains("Xother D_SENSE D_SENSE_SR_SINK D_SENSE_SR_SOURCE"),
        "{lowered}"
    );
    let wired = wire(&fixtures().join("bidirectional-port.json"), temp.path())?;
    assert!(
        wired.status.success(),
        "{}",
        String::from_utf8_lossy(&wired.stderr)
    );
    Ok(())
}

#[test]
fn nested_one_way_children_of_an_analog_inout_port_keep_separate_bundles() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config = fixtures().join("bidirectional-port.json");
    let source = fixtures().join("nested-bidirectional-port.cir");
    let result = expand(&source, &config, temp.path())?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let lowered = fs::read_to_string(temp.path().join("lowered.cir"))?;
    assert!(lowered.contains(".subckt TOP PIN OE MONITOR"));
    assert!(lowered.contains(
        "Xsense D_SENSE D_SENSE_SR_SINK D_SENSE_SR_SOURCE D_MON D_MON_SR_SINK D_MON_SR_SOURCE SENSE_WRAPPER"
    ), "{lowered}");
    assert!(lowered.contains(
        "Xdriver D_OE D_OE_SR_SINK D_OE_SR_SOURCE D_DRIVE D_DRIVE_SR_SINK D_DRIVE_SR_SOURCE DRIVE_WRAPPER"
    ), "{lowered}");
    assert!(!lowered.contains("Xsense PIN PIN_SR_SINK"));
    assert!(!lowered.contains("Xdriver PIN PIN_SR_SINK"));
    let result = wire(&config, temp.path())?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let deck = fs::read_to_string(temp.path().join("circuit.cir"))?;
    assert!(deck.contains("AsrFamily2 %gd(PIN 0) D_SENSE D_SENSE_SR_SINK D_SENSE_SR_SOURCE"));
    assert!(deck.contains("AsrFamily3 D_DRIVE D_DRIVE_SR_SINK D_DRIVE_SR_SOURCE %gd(PIN 0)"));
    assert_eq!(deck.matches("sr_family_adc(").count(), 2);
    assert_eq!(deck.matches("sr_family_dac(").count(), 2);
    let compiled = fs::read_to_string(temp.path().join("generated.inc"))?;
    assert_eq!(compiled.matches("sr_bundle_load(").count(), 1);
    for instance in ["XSR_A1 ", "XSR_A2 "] {
        assert!(
            lowered
                .lines()
                .any(|line| line.starts_with(instance) && line.ends_with(" SRALB0")),
            "{lowered}"
        );
    }
    Ok(())
}

#[test]
fn nested_inout_child_formal_cannot_short_parent_adc_and_dac() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let original = fs::read_to_string(fixtures().join("nested-bidirectional-port.cir"))?;
    // Making one nested child port an ALB input *and* output is not a
    // one-way interface. Its one physical triple cannot connect both
    // independent parent-side bundles, so fail before emitting a deck.
    let broken = original.replace("A1 [pin] [observer] DS", "A1 [pin] [pin] DS");
    let file = temp.path().join("broken.cir");
    fs::write(&file, broken)?;
    let result = expand(
        &file,
        &fixtures().join("bidirectional-port.json"),
        temp.path(),
    )?;
    assert!(
        !result.status.success(),
        "ambiguous inout child silently shorted"
    );
    assert!(!temp.path().join("lowered.cir").exists());
    assert!(!temp.path().join("generated.inc").exists());
    Ok(())
}

#[test]
fn multiple_nested_alb_drivers_need_separate_dac_bridges() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let original = fs::read_to_string(fixtures().join("nested-bidirectional-port.cir"))?;
    let broken = original.replace(
        "Xdriver oe pin DRIVE_WRAPPER",
        "Xdriver oe pin DRIVE_WRAPPER\nXdriver2 oe pin DRIVE_WRAPPER",
    );
    let source = temp.path().join("multi-driver.cir");
    fs::write(&source, broken)?;
    let result = expand(
        &source,
        &fixtures().join("bidirectional-port.json"),
        temp.path(),
    )?;
    assert!(
        !result.status.success(),
        "multiple analog drivers collapsed to one DAC"
    );
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("multiple ALB output drivers"),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(!temp.path().join("lowered.cir").exists());
    assert!(!temp.path().join("generated.inc").exists());
    Ok(())
}

#[test]
fn distinct_nested_alb_output_states_get_separate_dacs_on_one_analog_net() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config = fixtures().join("multi-physical-drivers.json");
    let result = expand(
        &fixtures().join("multi-physical-drivers.cir"),
        &config,
        temp.path(),
    )?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let lowered = fs::read_to_string(temp.path().join("lowered.cir"))?;
    assert!(lowered.contains(".subckt TOP PIN A B"), "{lowered}");
    assert!(
        lowered.contains(
            "Xleft D_A D_A_SR_SINK D_A_SR_SOURCE D_LEFT D_LEFT_SR_SINK D_LEFT_SR_SOURCE OUTER"
        ),
        "{lowered}"
    );
    assert!(
        lowered.contains(
            "Xright D_B D_B_SR_SINK D_B_SR_SOURCE D_RIGHT D_RIGHT_SR_SINK D_RIGHT_SR_SOURCE OUTER"
        ),
        "{lowered}"
    );

    let result = wire(&config, temp.path())?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let deck = fs::read_to_string(temp.path().join("circuit.cir"))?;
    assert_eq!(deck.matches("sr_family_adc(").count(), 2, "{deck}");
    assert_eq!(deck.matches("sr_family_dac(").count(), 2, "{deck}");
    assert!(
        deck.lines().any(|line| line.starts_with("AsrFamily")
            && line.contains("D_LEFT D_LEFT_SR_SINK D_LEFT_SR_SOURCE %gd(PIN 0)")),
        "{deck}"
    );
    assert!(
        deck.lines().any(|line| line.starts_with("AsrFamily")
            && line.contains("D_RIGHT D_RIGHT_SR_SINK D_RIGHT_SR_SOURCE %gd(PIN 0)")),
        "{deck}"
    );
    assert_eq!(
        fs::read_to_string(temp.path().join("generated.inc"))?
            .matches("sr_bundle_load(")
            .count(),
        1,
        "one shared ALB definition has one load-model declaration"
    );
    Ok(())
}

#[test]
fn mixed_output_boundary_must_resolve_to_real_independent_alb_pin() -> Result<()> {
    for case in ["duplicate", "wrong-direction", "missing-state"] {
        let temp = tempfile::tempdir()?;
        let mut config: Value =
            serde_json::from_slice(&fs::read(fixtures().join("multi-physical-drivers.json"))?)?;
        match case {
            "duplicate" => config["connections"][1]["digital_net"] = json!("D_LEFT"),
            "wrong-direction" => config["connections"][1]["direction"] = json!("input"),
            "missing-state" => config["connections"][1]["digital_net"] = json!("NONEXISTENT"),
            _ => unreachable!(),
        }
        let path = temp.path().join("config.json");
        fs::write(&path, serde_json::to_vec(&config)?)?;
        let result = expand(
            &fixtures().join("multi-physical-drivers.cir"),
            &path,
            temp.path(),
        )?;
        assert!(
            !result.status.success(),
            "{case} accepted without a verified ALB producer"
        );
        assert!(!temp.path().join("lowered.cir").exists());
        assert!(!temp.path().join("generated.inc").exists());
    }
    Ok(())
}

#[test]
fn three_independent_nested_drivers_each_get_their_own_physical_dac() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let input = fs::read_to_string(fixtures().join("multi-physical-drivers.cir"))?;
    let source = input.replace(
        "Xright b d_right OUTER",
        "Xright b d_right OUTER\nXthird a d_third OUTER",
    );
    let path = temp.path().join("three-drivers.cir");
    fs::write(&path, source)?;

    let mut config: Value =
        serde_json::from_slice(&fs::read(fixtures().join("multi-physical-drivers.json"))?)?;
    config["connections"]
        .as_array_mut()
        .context("missing annotations")?
        .push(json!({
            "scope": ["TOP"], "analog_net": "pin",
            "digital_net": "D_THIRD", "direction": "output",
            "out_family": "HC", "supply_v": 3.3
        }));
    let annotations = temp.path().join("three-drivers.json");
    fs::write(&annotations, serde_json::to_vec(&config)?)?;
    let result = expand(&path, &annotations, temp.path())?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let result = wire(&annotations, temp.path())?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let deck = fs::read_to_string(temp.path().join("circuit.cir"))?;
    assert_eq!(deck.matches("sr_family_dac(").count(), 3, "{deck}");
    for state in ["D_LEFT", "D_RIGHT", "D_THIRD"] {
        assert!(
            deck.lines().any(|line| line.starts_with("AsrFamily")
                && line.contains(&format!(
                    "{state} {state}_SR_SINK {state}_SR_SOURCE %gd(PIN 0)"
                ))),
            "Missing independent native DAC for {state}: {deck}"
        );
    }
    Ok(())
}

#[test]
fn shared_original_output_net_is_split_by_exact_nested_producer_identity() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config = fixtures().join("producer-identity-drivers.json");
    let result = expand(
        &fixtures().join("producer-identity-drivers.cir"),
        &config,
        temp.path(),
    )?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let lowered = fs::read_to_string(temp.path().join("lowered.cir"))?;
    assert!(lowered.contains(".subckt TOP PIN A B"), "{lowered}");
    assert!(
        lowered.contains(
            "Xleft D_A D_A_SR_SINK D_A_SR_SOURCE D_LEFT D_LEFT_SR_SINK D_LEFT_SR_SOURCE OUTER"
        ),
        "{lowered}"
    );
    assert!(
        lowered.contains(
            "Xright D_B D_B_SR_SINK D_B_SR_SOURCE D_RIGHT D_RIGHT_SR_SINK D_RIGHT_SR_SOURCE OUTER"
        ),
        "{lowered}"
    );
    assert_eq!(lowered.matches(".subckt OUTER ").count(), 1);
    assert_eq!(lowered.matches(".subckt INNER ").count(), 1);
    assert!(!lowered.contains("Xleft D_A D_A_SR_SINK D_A_SR_SOURCE PIN"));
    assert!(!lowered.contains("Xright D_B D_B_SR_SINK D_B_SR_SOURCE PIN"));
    let wired = wire(&config, temp.path())?;
    assert!(
        wired.status.success(),
        "{}",
        String::from_utf8_lossy(&wired.stderr)
    );
    let deck = fs::read_to_string(temp.path().join("circuit.cir"))?;
    assert_eq!(deck.matches("sr_family_dac(").count(), 2, "{deck}");
    assert_eq!(deck.matches("sr_family_adc(").count(), 2, "{deck}");
    for state in ["D_LEFT", "D_RIGHT"] {
        assert!(
            deck.lines().any(|line| line.starts_with("AsrFamily")
                && line.contains(&format!(
                    "{state} {state}_SR_SINK {state}_SR_SOURCE %gd(PIN 0)"
                ))),
            "Missing occurrence-local native DAC for {state}: {deck}"
        );
    }
    Ok(())
}

#[test]
fn direct_original_alb_instances_can_split_their_shared_output_state() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let source = fs::read_to_string(fixtures().join("producer-identity-drivers.cir"))?
        .replace(
            "Xleft a pin OUTER\nXright b pin OUTER",
            "Aleft [a] [pin] DL\nAright [b] [pin] DL\n.model DL d_logic_block input_load=2p out_res=40 out_delay=1n family=\"HC\" def=\"out[0] = in[0];\"",
        );
    let path = temp.path().join("direct.cir");
    fs::write(&path, source)?;
    let mut config: Value = serde_json::from_slice(&fs::read(
        fixtures().join("producer-identity-drivers.json"),
    )?)?;
    config["connections"][0]["producer"]["path"] = json!(["Aleft"]);
    config["connections"][1]["producer"]["path"] = json!(["Aright"]);
    let annotations = temp.path().join("direct.json");
    fs::write(&annotations, serde_json::to_vec(&config)?)?;
    let result = expand(&path, &annotations, temp.path())?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let lowered = fs::read_to_string(temp.path().join("lowered.cir"))?;
    assert!(
        lowered.contains(
            "XSR_ALEFT D_A D_A_SR_SINK D_A_SR_SOURCE D_LEFT D_LEFT_SR_SINK D_LEFT_SR_SOURCE SRALB"
        ),
        "{lowered}"
    );
    assert!(
        lowered.contains("XSR_ARIGHT D_B D_B_SR_SINK D_B_SR_SOURCE D_RIGHT D_RIGHT_SR_SINK D_RIGHT_SR_SOURCE SRALB"),
        "{lowered}"
    );
    let result = wire(&annotations, temp.path())?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        fs::read_to_string(temp.path().join("circuit.cir"))?
            .matches("sr_family_dac(")
            .count(),
        2
    );
    Ok(())
}

#[test]
fn producer_selection_requires_complete_nonambiguous_source_identity() -> Result<()> {
    let source = fs::read_to_string(fixtures().join("producer-identity-drivers.cir"))?;
    for case in [
        "duplicate-producer",
        "unmatched-path",
        "unmatched-bit",
        "missing-producer",
        "input-direction",
        "reused-state",
    ] {
        let temp = tempfile::tempdir()?;
        let mut config: Value = serde_json::from_slice(&fs::read(
            fixtures().join("producer-identity-drivers.json"),
        )?)?;
        let modified = match case {
            "duplicate-producer" => {
                config["connections"][1]["producer"]["path"] = json!(["Xleft", "Xinner", "A1"]);
                source.clone()
            }
            "unmatched-path" => {
                config["connections"][1]["producer"]["path"] = json!(["Xwrong", "Xinner", "A1"]);
                source.clone()
            }
            "unmatched-bit" => {
                config["connections"][1]["producer"]["output_pin"] = json!(1);
                source.clone()
            }
            "missing-producer" => {
                config["connections"]
                    .as_array_mut()
                    .context("no annotations")?
                    .remove(1);
                source.clone()
            }
            "input-direction" => {
                config["connections"][1]["direction"] = json!("input");
                source.clone()
            }
            "reused-state" => {
                config["connections"][1]["digital_net"] = json!("D_A");
                source.clone()
            }
            _ => unreachable!(),
        };
        let path = temp.path().join("source.cir");
        fs::write(&path, modified)?;
        let annotations = temp.path().join("config.json");
        fs::write(&annotations, serde_json::to_vec(&config)?)?;
        let result = expand(&path, &annotations, temp.path())?;
        assert!(
            !result.status.success(),
            "{case} incorrectly emitted a physical deck"
        );
        assert!(!temp.path().join("lowered.cir").exists(), "{case}");
        assert!(!temp.path().join("generated.inc").exists(), "{case}");
    }
    Ok(())
}

#[test]
fn deeply_promoted_x_output_interfaces_keep_distinct_physical_dacs() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config = fixtures().join("promoted-x-formal-drivers.json");
    let source = fixtures().join("promoted-x-formal-drivers.cir");
    let result = expand(&source, &config, temp.path())?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let lowered = fs::read_to_string(temp.path().join("lowered.cir"))?;
    for definition in ["GATE", "GROUP", "OUTER", "INNER"] {
        assert_eq!(
            lowered.matches(&format!(".subckt {definition} ")).count(),
            1,
            "Subcircuit {definition} should remain reusable: {lowered}"
        );
    }
    for definition in ["GATE", "GROUP"] {
        let header = lowered
            .lines()
            .find(|line| line.starts_with(&format!(".subckt {definition} ")))
            .context("missing promoted definition")?;
        assert!(header.contains("SR_PROMOTED_OUT_0"), "{header}");
        assert!(header.contains("SR_PROMOTED_OUT_1"), "{header}");
    }
    let top_call = lowered
        .lines()
        .find(|line| line.starts_with("Xgateway "))
        .context("missing top gateway")?;
    assert!(
        top_call.contains("D_LEFT D_LEFT_SR_SINK D_LEFT_SR_SOURCE")
            && top_call.contains("D_RIGHT D_RIGHT_SR_SINK D_RIGHT_SR_SOURCE"),
        "{top_call}"
    );
    let idle = lowered
        .lines()
        .find(|line| line.starts_with("Xidle "))
        .context("missing reused unselected definition")?;
    assert!(
        idle.matches("SPARE SPARE_SR_SINK SPARE_SR_SOURCE").count() == 3,
        "unselected instance must preserve its original shared digital net: {idle}"
    );
    let result = wire(&config, temp.path())?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let deck = fs::read_to_string(temp.path().join("circuit.cir"))?;
    assert_eq!(deck.matches("sr_family_dac(").count(), 2, "{deck}");
    assert_eq!(deck.matches("sr_family_adc(").count(), 2, "{deck}");
    for state in ["D_LEFT", "D_RIGHT"] {
        assert!(
            deck.lines().any(|line| line.starts_with("AsrFamily")
                && line.contains(&format!(
                    "{state} {state}_SR_SINK {state}_SR_SOURCE %gd(PIN 0)"
                ))),
            "No independently routed DAC for {state}"
        );
    }
    Ok(())
}

#[test]
fn promoted_x_formals_support_three_producers_without_definition_cloning() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let original = fs::read_to_string(fixtures().join("promoted-x-formal-drivers.cir"))?;
    let source = original.replace(
        "Xright b pin OUTER",
        "Xright b pin OUTER\nXthird a pin OUTER",
    );
    let source_path = temp.path().join("three.cir");
    fs::write(&source_path, source)?;
    let mut config: Value = serde_json::from_slice(&fs::read(
        fixtures().join("promoted-x-formal-drivers.json"),
    )?)?;
    config["connections"]
        .as_array_mut()
        .context("missing connections")?
        .push(json!({
            "scope": ["TOP"], "analog_net": "PIN", "digital_net": "D_THIRD",
            "direction": "output",
            "producer": {
                "path": ["Xgateway", "Xgroup", "Xthird", "Xinner", "A1"],
                "output_pin": 0
            },
            "out_family": "HC", "supply_v": 3.3
        }));
    let annotations = temp.path().join("three.json");
    fs::write(&annotations, serde_json::to_vec(&config)?)?;
    let result = expand(&source_path, &annotations, temp.path())?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let lowered = fs::read_to_string(temp.path().join("lowered.cir"))?;
    assert_eq!(lowered.matches(".subckt GROUP ").count(), 1);
    assert_eq!(lowered.matches(".subckt GATE ").count(), 1);
    assert!(lowered.contains("SR_PROMOTED_OUT_2"));
    let result = wire(&annotations, temp.path())?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let deck = fs::read_to_string(temp.path().join("circuit.cir"))?;
    assert_eq!(deck.matches("sr_family_dac(").count(), 3, "{deck}");
    for state in ["D_LEFT", "D_RIGHT", "D_THIRD"] {
        assert!(
            deck.lines().any(|line| line.starts_with("AsrFamily")
                && line.contains(&format!(
                    "{state} {state}_SR_SINK {state}_SR_SOURCE %gd(PIN 0)"
                ))),
            "Missing selected native physical DAC for {state}: {deck}"
        );
    }
    Ok(())
}

#[test]
fn promoted_bidirectional_child_keeps_sensing_on_the_adc_bundle() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let source = fs::read_to_string(fixtures().join("promoted-x-formal-drivers.cir"))?
        .replace(
            "Xright b pin OUTER\n.ends GROUP",
            "Xright b pin OUTER\nAmonitor [pin] [sense_internal] DM\n.model DM d_logic_block input_load=2p out_res=40 out_delay=1n family=\"HC\" def=\"out[0] = in[0];\"\n.ends GROUP",
        );
    let source_path = temp.path().join("sensing.cir");
    fs::write(&source_path, source)?;
    let mut config: Value = serde_json::from_slice(&fs::read(
        fixtures().join("promoted-x-formal-drivers.json"),
    )?)?;
    config["connections"]
        .as_array_mut()
        .context("missing connections")?
        .push(json!({
            "scope": ["TOP"], "analog_net": "pin", "digital_net": "D_SENSE",
            "direction": "input", "in_family": "HC", "supply_v": 3.3
        }));
    let annotations = temp.path().join("sensing.json");
    fs::write(&annotations, serde_json::to_vec(&config)?)?;
    let result = expand(&source_path, &annotations, temp.path())?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let lowered = fs::read_to_string(temp.path().join("lowered.cir"))?;
    let gateway = lowered
        .lines()
        .find(|line| line.starts_with("Xgateway "))
        .context("missing Xgateway")?;
    assert!(
        gateway.contains("D_SENSE D_SENSE_SR_SINK D_SENSE_SR_SOURCE"),
        "{gateway}"
    );
    assert!(
        gateway.contains("D_LEFT D_LEFT_SR_SINK D_LEFT_SR_SOURCE"),
        "{gateway}"
    );
    assert!(
        gateway.contains("D_RIGHT D_RIGHT_SR_SINK D_RIGHT_SR_SOURCE"),
        "{gateway}"
    );
    let result = wire(&annotations, temp.path())?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let deck = fs::read_to_string(temp.path().join("circuit.cir"))?;
    assert_eq!(deck.matches("sr_family_adc(").count(), 3, "{deck}");
    assert_eq!(deck.matches("sr_family_dac(").count(), 2, "{deck}");
    assert!(deck.lines().any(|line| line.starts_with("AsrFamily")
        && line.contains("%gd(PIN 0) D_SENSE D_SENSE_SR_SINK D_SENSE_SR_SOURCE")));
    Ok(())
}

#[test]
fn promoted_nested_output_selectors_must_remain_complete_and_exact() -> Result<()> {
    let source = fixtures().join("promoted-x-formal-drivers.cir");
    for defect in ["missing", "wrong-path", "wrong-bit", "duplicate-state"] {
        let temp = tempfile::tempdir()?;
        let mut config: Value = serde_json::from_slice(&fs::read(
            fixtures().join("promoted-x-formal-drivers.json"),
        )?)?;
        match defect {
            "missing" => {
                config["connections"]
                    .as_array_mut()
                    .context("connections")?
                    .remove(1);
            }
            "wrong-path" => {
                config["connections"][1]["producer"]["path"] =
                    json!(["Xgateway", "Xgroup", "Xwrong", "Xinner", "A1"]);
            }
            "wrong-bit" => {
                config["connections"][1]["producer"]["output_pin"] = json!(99);
            }
            "duplicate-state" => {
                config["connections"][1]["digital_net"] = json!("D_LEFT");
            }
            _ => unreachable!(),
        }
        let annotations = temp.path().join("config.json");
        fs::write(&annotations, serde_json::to_vec(&config)?)?;
        let result = expand(&source, &annotations, temp.path())?;
        assert!(!result.status.success(), "{defect} unexpectedly accepted");
        assert!(!temp.path().join("lowered.cir").exists(), "{defect}");
        assert!(!temp.path().join("generated.inc").exists(), "{defect}");
    }
    Ok(())
}

#[test]
fn dynamically_loaded_bidirectional_shared_pin_has_independent_physical_ports() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config = fixtures().join("dynamic-sensed-contention.json");
    let result = expand(
        &fixtures().join("dynamic-sensed-contention.cir"),
        &config,
        temp.path(),
    )?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let lowered = fs::read_to_string(temp.path().join("lowered.cir"))?;
    assert_eq!(lowered.matches(".subckt GROUP ").count(), 1);
    assert_eq!(lowered.matches(".subckt GATE ").count(), 1);
    assert!(lowered.contains("Cpin pin 0 80p"), "{lowered}");
    assert!(
        lowered.contains("Sload pin shunt gate 0 SHUNTSW"),
        "{lowered}"
    );
    let gateway = lowered
        .lines()
        .find(|line| line.starts_with("Xgateway "))
        .context("missing dynamically sensed gateway")?;
    for state in ["D_SENSE", "D_OBSERVE", "D_LEFT", "D_RIGHT"] {
        assert!(
            gateway.contains(&format!("{state} {state}_SR_SINK {state}_SR_SOURCE")),
            "{gateway}"
        );
    }
    let idle = lowered
        .lines()
        .find(|line| line.starts_with("Xidle "))
        .context("missing unselected reused GROUP caller")?;
    assert_eq!(
        idle.matches("SPARE SPARE_SR_SINK SPARE_SR_SOURCE").count(),
        3,
        "{idle}"
    );

    let result = wire(&config, temp.path())?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let deck = fs::read_to_string(temp.path().join("circuit.cir"))?;
    assert_eq!(deck.matches("sr_family_adc(").count(), 3, "{deck}");
    assert_eq!(deck.matches("sr_family_dac(").count(), 3, "{deck}");
    for state in ["D_LEFT", "D_RIGHT"] {
        assert!(
            deck.lines().any(|line| line.starts_with("AsrFamily")
                && line.contains(&format!(
                    "{state} {state}_SR_SINK {state}_SR_SOURCE %gd(PIN 0)"
                ))),
            "missing independent current-source DAC {state}"
        );
    }
    assert!(
        deck.lines().any(|line| line.starts_with("AsrFamily")
            && line.contains("%gd(PIN 0) D_SENSE D_SENSE_SR_SINK D_SENSE_SR_SOURCE")),
        "missing independent analog PIN sensing ADC"
    );
    assert!(
        deck.lines().any(|line| line.starts_with("AsrFamily")
            && line.contains("D_OBSERVE D_OBSERVE_SR_SINK D_OBSERVE_SR_SOURCE %gd(OBSERVE 0)")),
        "missing independent analog readback DAC"
    );
    assert_eq!(
        fs::read_to_string(temp.path().join("generated.inc"))?
            .matches("sr_bundle_load(")
            .count(),
        1,
        "identical gate and monitor kernels share a load-model declaration"
    );
    for instance in ["XSR_A1 ", "XSR_AMONITOR "] {
        assert!(
            lowered
                .lines()
                .any(|line| line.starts_with(instance) && line.ends_with(" SRALB0")),
            "{lowered}"
        );
    }
    Ok(())
}
