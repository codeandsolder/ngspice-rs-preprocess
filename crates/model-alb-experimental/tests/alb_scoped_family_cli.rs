//! End-to-end private subcircuit boundary planning tests.
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

fn run_expand(source: &Path, config: &Path, directory: &Path) -> Result<Output> {
    Ok(Command::new(env!("CARGO_BIN_EXE_alb-logical-expand"))
        .arg(source)
        .arg(config)
        .arg(directory.join("lowered.cir"))
        .arg(directory.join("generated.inc"))
        .output()?)
}

fn run_autowire(config: &Path, directory: &Path) -> Result<Output> {
    Ok(Command::new(env!("CARGO_BIN_EXE_alb-family-autowire"))
        .arg(directory.join("lowered.cir"))
        .arg(directory.join("generated.inc"))
        .arg(config)
        .arg(fixtures().join("auto-models.json"))
        .arg(directory.join("circuit.cir"))
        .output()?)
}

#[test]
fn native_bridges_are_inserted_in_their_actual_private_definition() -> Result<()> {
    for (fixture, config, scope, adc_count, dac_count) in [
        ("scoped-private.cir", "scoped-private-3v3.json", "TOP", 1, 1),
        ("scoped-private.cir", "scoped-private-5v.json", "TOP", 1, 1),
        ("scoped-nested.cir", "scoped-nested-3v3.json", "INNER", 1, 1),
        ("scoped-shared.cir", "scoped-shared.json", "TOP", 1, 0),
    ] {
        let directory = tempfile::tempdir()?;
        let config = fixtures().join(config);
        let expanded = run_expand(&fixtures().join(fixture), &config, directory.path())?;
        assert!(
            expanded.status.success(),
            "{}",
            String::from_utf8_lossy(&expanded.stderr)
        );
        let result = run_autowire(&config, directory.path())?;
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let netlist = fs::read_to_string(directory.path().join("circuit.cir"))?;
        let start = netlist
            .find(&format!(".subckt {scope} "))
            .context("missing owner")?;
        let end = netlist[start..]
            .find(&format!(".ends {scope}"))
            .context("missing closing .ends")?
            + start;
        let inner = &netlist[start..end];
        assert_eq!(inner.matches("sr_family_adc(").count(), adc_count);
        assert_eq!(inner.matches("sr_family_dac(").count(), dac_count);
        assert_eq!(netlist.matches("sr_family_adc(").count(), adc_count);
        assert_eq!(netlist.matches("sr_family_dac(").count(), dac_count);
        assert!(inner.contains("AsrFamily0 %gd(LOCAL_IN 0) D_IN D_IN_SR_SINK D_IN_SR_SOURCE"));
        if fixture == "scoped-shared.cir" {
            assert_eq!(
                inner.matches(" D_IN D_IN_SR_SINK D_IN_SR_SOURCE ").count(),
                3
            );
        }
        if fixture == "scoped-private.cir" {
            assert!(netlist.contains("Xone O1 TOP"));
            assert!(netlist.contains("Xtwo O2 TOP"));
        }
        assert!(netlist.ends_with(".end\n"));
    }
    Ok(())
}

#[test]
fn invalid_and_exported_private_scopes_fail_before_output() -> Result<()> {
    let source = fixtures().join("scoped-private.cir");
    for case in [
        "unknown",
        "exported-analog",
        "exported-digital",
        "duplicate",
        "invalid-family",
        "invalid-voltage",
        "unmatched-digital",
    ] {
        let temp = tempfile::tempdir()?;
        let mut mapping: Value =
            serde_json::from_slice(&fs::read(fixtures().join("scoped-private-3v3.json"))?)?;
        match case {
            "unknown" => mapping["connections"][0]["scope"] = json!(["MISSING"]),
            "exported-analog" => mapping["connections"][0]["analog_net"] = json!("output"),
            "exported-digital" => mapping["connections"][0]["digital_net"] = json!("output"),
            "duplicate" => {
                let duplicate = mapping["connections"][0].clone();
                mapping["connections"]
                    .as_array_mut()
                    .context("missing mapping list")?
                    .push(duplicate);
            }
            "invalid-family" => mapping["connections"][0]["in_family"] = json!("MISSING"),
            "invalid-voltage" => mapping["connections"][0]["supply_v"] = json!(0),
            "unmatched-digital" => {
                mapping["connections"][0]["analog_net"] = json!("NOT_CONNECTED");
                mapping["connections"][0]["digital_net"] = json!("OTHER");
            }
            _ => unreachable!(),
        }
        let input = temp.path().join("config.json");
        fs::write(&input, serde_json::to_vec(&mapping)?)?;
        let first = run_expand(&source, &input, temp.path())?;
        if first.status.success() {
            let next = run_autowire(&input, temp.path())?;
            assert!(
                !next.status.success(),
                "{case}: autowire unexpectedly accepted"
            );
            assert!(!temp.path().join("circuit.cir").exists());
        } else {
            assert!(!temp.path().join("lowered.cir").exists());
            assert!(!temp.path().join("generated.inc").exists());
        }
    }
    Ok(())
}

#[test]
fn existing_a_device_cannot_receive_a_second_private_adc() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config = fixtures().join("scoped-private-3v3.json");
    let expanded = run_expand(&fixtures().join("scoped-private.cir"), &config, temp.path())?;
    assert!(expanded.status.success());
    let path = temp.path().join("lowered.cir");
    let content = fs::read_to_string(&path)?;
    fs::write(
        &path,
        content.replace(".ends TOP\n", "Arogue D_IN UNKNOWN_MODEL\n.ends TOP\n"),
    )?;
    let output = run_autowire(&config, temp.path())?;
    assert!(
        !output.status.success(),
        "accepted duplicate private digital source"
    );
    assert!(!temp.path().join("circuit.cir").exists());
    Ok(())
}

#[test]
fn preexisting_a_device_on_private_bundle_metadata_is_rejected() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config = fixtures().join("scoped-private-3v3.json");
    let expanded = run_expand(&fixtures().join("scoped-private.cir"), &config, temp.path())?;
    assert!(expanded.status.success());
    let path = temp.path().join("lowered.cir");
    let original = fs::read_to_string(&path)?;
    for metadata in ["D_IN_SR_SINK", "D_IN_SR_SOURCE"] {
        fs::write(
            &path,
            original.replace(
                ".ends TOP\n",
                &format!("Arogue {metadata} UNKNOWN_MODEL\n.ends TOP\n"),
            ),
        )?;
        let output = run_autowire(&config, temp.path())?;
        assert!(
            !output.status.success(),
            "accepted existing A device on private metadata {metadata}"
        );
        assert!(!temp.path().join("circuit.cir").exists());
    }
    Ok(())
}

#[test]
fn nested_a_device_on_private_bundle_metadata_is_rejected() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config = fixtures().join("scoped-private-3v3.json");
    let expanded = run_expand(&fixtures().join("scoped-private.cir"), &config, temp.path())?;
    assert!(expanded.status.success());
    let path = temp.path().join("lowered.cir");
    let content = fs::read_to_string(&path)?;
    let altered = content.replace(
        ".ends TOP\n",
        "Xrogue D_IN_SR_SOURCE PROBE\n\
         .ends TOP\n\
         .subckt PROBE tap\n\
         Xdeep tap SENSE\n\
         .ends PROBE\n\
         .subckt SENSE sensed\n\
         Arogue %gd(sensed 0) unrelated UNKNOWN_MODEL\n\
         .ends SENSE\n",
    );
    assert_ne!(altered, content);
    fs::write(&path, altered)?;
    let output = run_autowire(&config, temp.path())?;
    assert!(
        !output.status.success(),
        "accepted nested A device on private output source-current metadata"
    );
    assert!(!temp.path().join("circuit.cir").exists());
    Ok(())
}

#[test]
fn lexical_nested_model_scope_is_distinct_from_flat_model_name() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let input = fs::read_to_string(fixtures().join("scoped-nested.cir"))?;
    let input = input
        .replace(
            ".ends OUTER\n.subckt INNER analog_out",
            ".subckt INNER analog_out",
        )
        .replace(".ends INNER\n.tran", ".ends INNER\n.ends OUTER\n.tran");
    let source = temp.path().join("nested.cir");
    fs::write(&source, input)?;
    let mut cfg: Value =
        serde_json::from_slice(&fs::read(fixtures().join("scoped-nested-3v3.json"))?)?;
    for binding in cfg["connections"].as_array_mut().context("missing list")? {
        binding["scope"] = json!(["OUTER", "INNER"]);
    }
    let config = temp.path().join("config.json");
    fs::write(&config, serde_json::to_vec(&cfg)?)?;
    let result = run_expand(&source, &config, temp.path())?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let emitted = run_autowire(&config, temp.path())?;
    assert!(
        emitted.status.success(),
        "{}",
        String::from_utf8_lossy(&emitted.stderr)
    );
    let text = fs::read_to_string(temp.path().join("circuit.cir"))?;
    let inner = text
        .find(".subckt INNER")
        .context("missing inner declaration")?;
    let outer = text.find(".ends OUTER").context("missing closing outer")?;
    let last = text.find("AsrFamily1").context("missing DAC")?;
    assert!(
        inner < last && last < outer,
        "bridge must be lexically inside INNER"
    );
    Ok(())
}

#[test]
fn private_digital_state_names_are_truly_caller_chosen() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut config: Value =
        serde_json::from_slice(&fs::read(fixtures().join("scoped-private-3v3.json"))?)?;
    config["connections"][0]["digital_net"] = json!("arbitrary_private_state");
    let file = temp.path().join("custom.json");
    fs::write(&file, serde_json::to_vec(&config)?)?;
    let expansion = run_expand(&fixtures().join("scoped-private.cir"), &file, temp.path())?;
    assert!(
        expansion.status.success(),
        "{}",
        String::from_utf8_lossy(&expansion.stderr)
    );
    let wiring = run_autowire(&file, temp.path())?;
    assert!(
        wiring.status.success(),
        "{}",
        String::from_utf8_lossy(&wiring.stderr)
    );
    let text = fs::read_to_string(temp.path().join("circuit.cir"))?;
    assert!(text.contains(
        "ARBITRARY_PRIVATE_STATE ARBITRARY_PRIVATE_STATE_SR_SINK ARBITRARY_PRIVATE_STATE_SR_SOURCE"
    ));
    Ok(())
}
