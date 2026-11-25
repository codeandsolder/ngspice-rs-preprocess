use anyhow::{Context, Result};
use std::{fs, path::Path, process::Command};

fn run(
    format: &str,
    source: &Path,
    output: &Path,
    options: &[&str],
) -> Result<std::process::Output> {
    Ok(Command::new(env!("CARGO_BIN_EXE_ngspice-rs-preprocess"))
        .arg(format)
        .arg(source)
        .arg(output)
        .args(options)
        .output()?)
}

#[test]
fn cli_and_library_have_identical_artifacts_and_manifest() -> Result<()> {
    use ngspice_rs_preprocess::{
        Cancellation, InputFormat, Options, Request, SourceInput, prepare,
    };
    let dir = tempfile::tempdir()?;
    let source = dir.path().join("input.cir");
    let output = dir.path().join("bundle");
    fs::write(&source, b"* title\nR1 a 0 47\n.end\n")?;
    let execution = run("native-deck", &source, &output, &[])?;
    assert!(execution.status.success(), "{execution:?}");
    let bundle = prepare(
        &Request {
            format: InputFormat::NativeDeck,
            input: SourceInput::File(source.clone()),
            libraries: Vec::new(),
            search_paths: Vec::new(),
        },
        &Options::default(),
        &Cancellation::default(),
    )?;
    assert_eq!(
        fs::read(output.join(&bundle.primary.filename))?,
        bundle.primary.bytes
    );
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("manifest.json"))?)?;
    assert_eq!(manifest["version"], 1);
    assert_eq!(manifest["input_format"], "native-deck");
    assert_eq!(manifest["primary"], "deck.cir");
    assert_eq!(
        manifest["artifacts"][0]["provenance"]["lines"]
            .as_array()
            .context("missing lines")?
            .len(),
        3
    );
    let old = fs::read(output.join("deck.cir"))?;
    fs::write(&source, b"* title\nR1 a 0 99\n.end\n")?;
    assert!(!run("native-deck", &source, &output, &[])?.status.success());
    assert_eq!(fs::read(output.join("deck.cir"))?, old);
    Ok(())
}

#[test]
fn validation_failure_leaves_no_output_directory_or_temporary_files() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let source = dir.path().join("input.cir");
    let output = dir.path().join("bundle");
    fs::write(&source, b"* title\n.inc missing.lib\n.end\n")?;
    let result = run("native-deck", &source, &output, &[])?;
    assert!(!result.status.success());
    assert!(!output.exists());
    assert_eq!(fs::read_dir(dir.path())?.count(), 1);
    fs::write(&source, b"* title\nR1 a 0 47\n.end\n")?;
    for options in [
        vec!["--max-output-bytes", "2"],
        vec!["--command-arg", "literal"],
        vec!["--command", "relative"],
        vec!["--max-input-bytes", "3", "--max-input-bytes", "50"],
    ] {
        assert!(
            !run("native-deck", &source, &output, &options)?
                .status
                .success()
        );
        assert!(!output.exists());
    }
    Ok(())
}

#[test]
fn native_reference_dc_and_ac_through_unified_cli() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("input.cir");
    let output = dir.path().join("bundle");
    fs::write(&root, b"* series RL\n.inc coil.lib\nV1 a 0 dc 1 ac 1\nX1 a 0 COIL\n.control\nset numdgt=17\nset wr_singlescale\nop\nwrdata op.dat i(V1)\nac lin 3 1000 3000\nwrdata ac.dat i(V1)\nquit\n.endc\n.end\n")?;
    fs::write(
        dir.path().join("coil.lib"),
        b".subckt COIL a b\nL1 a b 4u\n+ Rser=40m\n.ends COIL\n.end\n",
    )?;
    let prepared = run("native-deck", &root, &output, &[])?;
    assert!(prepared.status.success(), "{prepared:?}");
    let result = Command::new("ngspice")
        .current_dir(&output)
        .args(["-b", "-n", "deck.cir"])
        .output()?;
    assert!(result.status.success(), "{result:?}");
    let op = fs::read_to_string(output.join("op.dat"))?;
    let current: f64 = op
        .split_ascii_whitespace()
        .nth(1)
        .context("missing DC current")?
        .parse()?;
    assert!((current + 25.).abs() < 1e-10);
    let ac = fs::read_to_string(output.join("ac.dat"))?;
    assert_eq!(ac.lines().count(), 3);
    for line in ac.lines() {
        let values = line
            .split_ascii_whitespace()
            .map(str::parse::<f64>)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let [frequency, real, imaginary] = values.as_slice() else {
            anyhow::bail!("unexpected AC values");
        };
        let r = 0.04;
        let x = std::f64::consts::TAU * frequency * 4e-6;
        let denominator = x.mul_add(x, r * r);
        assert!((real + r / denominator).abs() < 1e-10);
        assert!((imaginary - x / denominator).abs() < 1e-10);
    }
    Ok(())
}

#[test]
fn translated_piecewise_resistor_matches_independent_dc_law() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("input.net");
    let output = dir.path().join("bundle");
    fs::write(
        &root,
        b"V1 a 0 0\n!R1 a 0 NSEG=2 X0=0 Y0=0 X1=1 Y1=1m X2=2 Y2=3m IC=1\n.end\n",
    )?;
    let prepared = run("expanded-deck", &root, &output, &[])?;
    assert!(prepared.status.success(), "{prepared:?}");
    let bytes = fs::read(output.join("deck.cir"))?;
    let body = bytes
        .strip_suffix(b".end\n")
        .context("missing deck terminator")?;
    let mut deck = body.to_vec();
    deck.extend_from_slice(b".control\nset numdgt=17\nset wr_singlescale\ndc V1 0 2 0.25\nwrdata dc.dat i(V1)\nquit\n.endc\n.end\n");
    fs::write(output.join("reference.cir"), deck)?;
    let execution = Command::new("ngspice")
        .current_dir(&output)
        .args(["-b", "-n", "reference.cir"])
        .output()?;
    assert!(execution.status.success(), "{execution:?}");
    let dc = fs::read_to_string(output.join("dc.dat"))?;
    assert_eq!(dc.lines().count(), 9);
    for line in dc.lines() {
        let values = line
            .split_ascii_whitespace()
            .map(str::parse::<f64>)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let [voltage, current] = values.as_slice() else {
            anyhow::bail!("unexpected DC values");
        };
        let expected = if *voltage <= 1.0 {
            voltage * 0.001
        } else {
            (voltage - 1.0).mul_add(0.002, 0.001)
        };
        assert!((current + expected).abs() < 1e-10);
    }
    Ok(())
}
