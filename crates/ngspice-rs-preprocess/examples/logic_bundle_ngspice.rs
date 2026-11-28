//! Independent native reference verification of runtime discovery from a
//! complete linked preparation bundle, including companion-only requirements.
use anyhow::{Context, Result, ensure};
use ngspice_rs_preprocess::{
    Cancellation, CodeModelRuntime, InputFormat, LibraryInput, Options, Request, SourceInput,
    prepare,
};
use std::{fmt::Write as _, fs, path::Path, process::Command};

fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let runtime_directory = fs::canonicalize(
        args.next()
            .context("usage: logic_bundle_ngspice RUNTIME_DIRECTORY")?,
    )?;
    ensure!(
        args.next().is_none(),
        "usage: logic_bundle_ngspice RUNTIME_DIRECTORY"
    );
    let directory = tempfile::tempdir()?;
    let root = directory.path().join("root.net");
    let library = directory.path().join("gate.lb");
    fs::write(&root, b".include gate.lb\nVref rtn 0 0.75\nV1 a rtn PWL(0 0 1u 0 1.01u 5 1.99u 5 2u 0 3u 0)\nV2 b rtn 5\nX1 a b q rtn GATE\nR1 q rtn 100\n.end\n")?;
    fs::write(&library, b".subckt GATE a b q rtn\n!D1 q rtn a b M IC=0\n.MODEL M AND2 RIN=10Meg ROUT=10 VOL=0.5 VOH=4.5 HYSTWD=1 TH=2.5 DELAY=500n LOGIC=POS\n.ends GATE\n")?;
    let bundle = prepare(
        &Request {
            format: InputFormat::SimplisDeck,
            input: SourceInput::File(root),
            libraries: vec![LibraryInput::Source(SourceInput::File(library))],
            search_paths: Vec::new(),
        },
        &Options::default(),
        &Cancellation::default(),
    )?;
    ensure!(
        bundle.primary.required_code_models.is_empty(),
        "runtime must originate in the companion library"
    );
    ensure!(
        bundle.required_code_models() == [CodeModelRuntime::Logic],
        "missing or duplicate runtime requirement"
    );
    let mut startup = String::new();
    for runtime in bundle.required_code_models() {
        let path = runtime_directory.join(runtime.reference_module());
        let path = path.to_str().context("non-UTF-8 runtime path")?;
        ensure!(
            Path::new(path).is_file() && !path.contains(char::is_whitespace),
            "invalid reference module pathname {path}"
        );
        writeln!(startup, "codemodel {path}")?;
    }
    fs::write(directory.path().join(".spiceinit"), startup)?;
    for artifact in bundle.artifacts() {
        fs::write(directory.path().join(&artifact.filename), &artifact.bytes)?;
    }
    let mut deck = bundle
        .primary
        .bytes
        .strip_suffix(b".end\n")
        .context("missing deck terminator")?
        .to_vec();
    deck.extend_from_slice(b".control\ntran 10n 3u\nmeas tran low find v(q) at=1.3u\nmeas tran high find v(q) at=1.8u\nmeas tran recovered find v(q) at=2.7u\nquit\n.endc\n.end\n");
    fs::write(directory.path().join(&bundle.primary.filename), deck)?;
    let output = Command::new("ngspice")
        .current_dir(directory.path())
        .args(["-b", &bundle.primary.filename])
        .output()?;
    let stdout = String::from_utf8(output.stdout)?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    ensure!(
        output.status.success() && stdout.contains("ngspice-47 done") && !stderr.contains("Error:"),
        "{stdout}\n{stderr}"
    );
    for (name, expected) in [
        ("low", 0.75 + 0.5 / 1.1),
        ("high", 0.75 + 4.5 / 1.1),
        ("recovered", 0.75 + 0.5 / 1.1),
    ] {
        let value = stdout
            .lines()
            .filter_map(|line| line.split_once('='))
            .find_map(|(key, value)| (key.trim() == name).then_some(value.trim()))
            .with_context(|| format!("missing measurement {name}: {stdout}"))?
            .split_ascii_whitespace()
            .next()
            .context("empty measurement")?
            .parse::<f64>()?;
        ensure!(
            (value - expected).abs() < 5e-5,
            "{name}: {value} != {expected}"
        );
    }
    println!("Linked companion logic runtime and native loaded transient passed");
    Ok(())
}
