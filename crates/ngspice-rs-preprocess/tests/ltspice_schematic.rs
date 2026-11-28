//! Headless schematic import through the ordinary source preparation API.
use anyhow::{Context, Result, ensure};
use ngspice_rs_preprocess::{
    Cancellation, InputFormat, Options, PreparedBundle, Provenance, Request, SourceInput, prepare,
};
use std::{fs, path::Path, process::Command};

const RES: &str = "Version 4\nSymbolType CELL\nSYMATTR Prefix R\nSYMATTR Value 1k\nPIN 0 16 NONE 0\nPINATTR SpiceOrder 2\nPIN 0 0 NONE 0\nPINATTR SpiceOrder 1\n";
const CHILD: &str = "Version 4\nSymbolType BLOCK\nPIN 0 0 NONE 0\nPINATTR PinName in\nPINATTR SpiceOrder 1\nPIN 0 16 NONE 0\nPINATTR PinName out\nPINATTR SpiceOrder 2\n";

fn prepared(path: &Path) -> Result<PreparedBundle> {
    prepare(
        &Request {
            format: InputFormat::LtspiceSchematic,
            input: SourceInput::File(path.to_owned()),
            libraries: Vec::new(),
            search_paths: Vec::new(),
        },
        &Options::default(),
        &Cancellation::default(),
    )
}

#[test]
fn source_maps_retain_child_and_library_lines() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("top.asc");
    let child = dir.path().join("child.asc");
    let lib = dir.path().join("mod.lib");
    fs::write(dir.path().join("RES.ASY"), RES)?;
    fs::write(dir.path().join("child.asy"), CHILD)?;
    fs::write(&lib, ".subckt ext a b\nRext a b 2k\n.ends ext\n")?;
    fs::write(
        dir.path().join("mod.asy"),
        RES.replace("Prefix R", "Prefix X")
            .replace("Value 1k", "Value ext\nSYMATTR ModelFile mod.lib"),
    )?;
    fs::write(
        &child,
        "Version 4\nSHEET 1 100 100\nFLAG 0 0 in\nIOPIN 0 0 In\nFLAG 0 16 out\nIOPIN 0 16 Out\nSYMBOL res 0 0 R0\nSYMATTR InstName R1\nSYMATTR Value 2k\n",
    )?;
    fs::write(
        &root,
        "Version 4\nSHEET 1 100 100\nFLAG 0 0 a\nFLAG 0 16 b\nFLAG 16 0 b\nFLAG 16 16 0\nSYMBOL child 0 0 R0\nSYMATTR InstName X1\nSYMBOL mod 16 0 R0\nSYMATTR InstName X2\nTEXT 0 0 Left 2 !.op\n",
    )?;
    let bundle = prepared(&root)?;
    let output = std::str::from_utf8(&bundle.primary.bytes)?;
    let Provenance::Native { lines } = &bundle.primary.provenance else {
        anyhow::bail!("missing provenance")
    };
    assert_eq!(lines.len(), output.lines().count());
    for (needle, source, expected) in [
        ("XX1 a b __lt_sheet_1", &root, 7),
        ("XX2 b 0 ext", &root, 9),
        ("R1 in out 2k", &child, 7),
        ("Rext a b 2k", &lib, 2),
    ] {
        let index = output
            .lines()
            .position(|line| line == needle)
            .with_context(|| format!("missing {needle} in {output}"))?;
        assert_eq!(lines[index].source, source.canonicalize()?);
        assert_eq!(lines[index].line, expected);
    }
    assert!(output.lines().any(|line| line == ".op"));
    Ok(())
}

fn numerical_data(dir: &Path, deck: &[u8], name: &str) -> Result<Vec<Vec<f64>>> {
    let mut deck = deck
        .strip_suffix(b".end\n")
        .context("missing deck terminator")?
        .to_vec();
    deck.extend_from_slice(format!(".control\nset numdgt=17\nset wr_singlescale\nop\nwrdata {name}-op.dat v(out)\ndc V1 0 2 0.125\nwrdata {name}-dc.dat v(out)\nac lin 3 100 300\nwrdata {name}-ac.dat v(out)\ntran 1u 4m 0 10u\nwrdata {name}-tran.dat v(out)\nquit\n.endc\n.end\n").as_bytes());
    let filename = format!("{name}.cir");
    fs::write(dir.join(&filename), deck)?;
    let result = Command::new("ngspice")
        .current_dir(dir)
        .args(["-b", "-n", &filename])
        .output()?;
    let log = format!(
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    ensure!(
        result.status.success() && !log.to_ascii_lowercase().contains("error"),
        "{log}"
    );
    ["op", "dc", "ac", "tran"]
        .into_iter()
        .map(|analysis| {
            let text = fs::read_to_string(dir.join(format!("{name}-{analysis}.dat")))?;
            text.split_whitespace()
                .map(|value| Ok(value.parse::<f64>()?))
                .collect()
        })
        .collect()
}

#[test]
fn imported_hierarchical_rc_matches_independent_native_deck() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("filter.asc");
    fs::write(dir.path().join("res.asy"), RES)?;
    fs::write(
        dir.path().join("cap.asy"),
        RES.replace("Prefix R", "Prefix C")
            .replace("Value 1k", "Value 1u"),
    )?;
    fs::write(
        dir.path().join("voltage.asy"),
        RES.replace("Prefix R", "Prefix V")
            .replace("Value 1k", "Value PULSE(0 1 1u 1u 1u 2m 4m) AC 1"),
    )?;
    fs::write(dir.path().join("child.asy"), CHILD)?;
    fs::write(
        dir.path().join("child.asc"),
        "Version 4\nSHEET 1 100 100\nFLAG 0 0 in\nIOPIN 0 0 In\nFLAG 0 16 out\nIOPIN 0 16 Out\nSYMBOL res 0 0 R0\nSYMATTR InstName R1\n",
    )?;
    fs::write(
        &root,
        "Version 4\nSHEET 1 100 100\nFLAG 0 0 in\nFLAG 0 16 0\nFLAG 32 0 in\nFLAG 32 16 out\nFLAG 64 0 out\nFLAG 64 16 0\nSYMBOL voltage 0 0 R0\nSYMATTR InstName V1\nSYMBOL child 32 0 R0\nSYMATTR InstName X1\nSYMBOL cap 64 0 M0\nSYMATTR InstName C1\nTEXT 0 0 Left 2 !.tran 1u 4m 0 10u\n",
    )?;
    let bundle = prepared(&root)?;
    let reference = b"reference\nV1 in 0 PULSE(0 1 1u 1u 1u 2m 4m) AC 1\nX1 in out cell\nC1 out 0 1u\n.subckt cell in out\nR1 in out 1k\n.ends cell\n.end\n";
    let measured = numerical_data(dir.path(), &bundle.primary.bytes, "imported")?;
    let expected = numerical_data(dir.path(), reference, "reference")?;
    for (observed, expected) in measured.iter().zip(&expected) {
        assert!(!observed.is_empty());
        assert_eq!(observed.len(), expected.len());
        for (&a, &b) in observed.iter().zip(expected) {
            ensure!((a - b).abs() <= 1e-10 * b.abs().max(1.), "{a} != {b}");
        }
    }
    Ok(())
}
