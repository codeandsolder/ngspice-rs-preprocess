use anyhow::{Context, Result, ensure};
use ngspice_rs_preprocess::{
    Cancellation, InputFormat, Options, PreparedBundle, Provenance, Request, SourceInput, prepare,
};
use std::{fs, path::Path, process::Command};

fn compile(path: &Path, format: InputFormat) -> Result<PreparedBundle> {
    prepare(
        &Request {
            format,
            input: SourceInput::File(path.to_path_buf()),
            libraries: Vec::new(),
            search_paths: Vec::new(),
        },
        &Options::default(),
        &Cancellation::default(),
    )
}

#[test]
fn linked_hspice_resource_spans_and_language_overrides() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("root.cir");
    fs::write(
        &root,
        ".lib is a title, not a directive\n.inc 'child.lib' $ comment\nV1 a 0 'hs+ps+again'\nV2 1x 0 1\nR1 1x 0 2X\nB3 b 0 V=2.5X/1meg+V(1x)\n.end\n",
    )?;
    fs::write(
        dir.path().join("child.lib"),
        ".lib 'corners.lib' tt\n.inc 'ps.lib'\n.param again='LOG(100)'\n",
    )?;
    fs::write(
        dir.path().join("corners.lib"),
        ".lib ss\n.param hs=999\n.endl ss\n.lib tt\n.param hs='LOG(100)\n+ +PWR(-2,2)' $ discarded\n.endl tt\n",
    )?;
    fs::write(
        dir.path().join("ps.lib"),
        "*#PSPICE\n.param ps={PWR(-2,2)} ; override\n",
    )?;
    let bundle = compile(&root, InputFormat::HspiceDeck)?;
    let text = std::str::from_utf8(&bundle.primary.bytes)?;
    ensure!(
        text.starts_with("* .lib is a title, not a directive\n"),
        "{text}"
    );
    ensure!(
        text.contains(".options TNOM=25") && text.contains(".temp 25"),
        "{text}"
    );
    ensure!(
        !text.contains("999") && !text.contains("discarded"),
        "{text}"
    );
    let Provenance::Native { lines } = &bundle.primary.provenance else {
        anyhow::bail!("expected mapped artifact");
    };
    let span = lines
        .iter()
        .find(|line| line.source.ends_with("corners.lib"))
        .context("missing selected section origin")?;
    assert_eq!((span.line, span.last_line), (5, 6));
    assert_eq!(lines.len(), text.lines().count());
    assert_eq!(lines.iter().filter(|line| line.line == 0).count(), 2);
    let deck = text
        .strip_suffix(".end\n")
        .context("missing deck terminator")?;
    fs::write(
        dir.path().join("test.cir"),
        format!(
            "{deck}.control\nset numdgt=17\nset wr_singlescale\nop\nwrdata op.dat v(a) v(b) i(v2)\nquit\n.endc\n.end\n"
        ),
    )?;
    let run = Command::new("ngspice")
        .current_dir(dir.path())
        .args(["-b", "-n", "test.cir"])
        .output()?;
    let log = format!(
        "{}\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    ensure!(
        run.status.success() && !log.to_ascii_lowercase().contains("error:"),
        "{log}\n{text}"
    );
    let data = fs::read_to_string(dir.path().join("op.dat"))?;
    let actual = data
        .split_ascii_whitespace()
        .skip(1)
        .map(str::parse::<f64>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(actual.len(), 3);
    for (value, expected) in actual.into_iter().zip([2. * 100_f64.ln(), 3.5, -0.5e-6]) {
        ensure!((value - expected).abs() < 1e-10, "{value} != {expected}");
    }
    assert!(text.contains("R1 1x 0 2meg"), "{text}");
    assert!(text.contains("V(1x)"), "{text}");
    Ok(())
}

#[test]
fn standalone_library_keeps_first_statement_without_deck_defaults() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("lib.sp");
    fs::write(
        &root,
        ".param gain='LOG(100)'\n.subckt test in out\nE1 out 0 VCVS in 0 2\n.ends test\n",
    )?;
    let bundle = compile(&root, InputFormat::HspiceLibrary)?;
    let text = std::str::from_utf8(&bundle.primary.bytes)?;
    assert!(text.starts_with(".param gain={ln(100)}\n"), "{text}");
    assert!(!text.contains("VCVS"), "{text}");
    assert!(!text.contains("TNOM") && !text.contains(".temp"), "{text}");
    Ok(())
}

#[test]
fn unqualified_hspice_semantics_are_errors() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("root.cir");
    for (card, expected) in [
        ("X1 a b cell m=4", "unknown HSPICE subcircuit"),
        (
            ".subckt cell a b\nXrec a b cell\n.ends cell\nX1 a b cell",
            "recursive HSPICE subcircuit",
        ),
        (
            ".subckt cell a b m=4\n.ends cell",
            "multiplier-default qualification",
        ),
        (".model mos NMOS LEVEL=49", "qualified native mapping"),
        (".temp 25 50", "multiple-temperature"),
        (".option post=2", "unqualified HSPICE option"),
    ] {
        fs::write(&root, format!("test\n{card}\n.end\n"))?;
        let error = compile(&root, InputFormat::HspiceDeck).expect_err("unsupported semantics");
        assert!(format!("{error:#}").contains(expected), "{error:#}");
    }
    Ok(())
}

fn numeric_deck(path: &Path, source: &str, script: &str) -> Result<String> {
    let root = path.join("input.sp");
    fs::write(&root, source)?;
    let bundle = compile(&root, InputFormat::HspiceDeck)?;
    let text = std::str::from_utf8(&bundle.primary.bytes)?;
    let deck = text.strip_suffix(".end\n").context("missing terminator")?;
    fs::write(
        path.join("run.cir"),
        format!("{deck}.control\nset numdgt=17\nset wr_singlescale\n{script}\nquit\n.endc\n.end\n"),
    )?;
    let result = Command::new("ngspice")
        .current_dir(path)
        .args(["-b", "-n", "run.cir"])
        .output()?;
    let log = format!(
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    ensure!(
        result.status.success() && !log.to_ascii_lowercase().contains("error:"),
        "{log}\n{text}"
    );
    Ok(text.to_owned())
}

fn values(path: &Path, file: &str) -> Result<Vec<Vec<f64>>> {
    fs::read_to_string(path.join(file))?
        .lines()
        .map(|line| {
            line.split_ascii_whitespace()
                .map(str::parse::<f64>)
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(Into::into)
        })
        .collect()
}

#[test]
fn documented_hspice_precedence_examples_and_actual_isolation() -> Result<()> {
    // HSPICE User Guide: Simulation and Analysis, B-2008.09, chapter 10,
    // figure 26 and tables 21/22. Independently constructed companion
    // branches exercise call-site actuals and nested inherited symbols.
    for (policy, chain, default, actual) in [
        ("GLOBAL", -3., -1., -1.),
        ("LOCAL", -(1. + 0.5 + 1. / 3.), -0.5, -0.2),
    ] {
        let dir = tempfile::tempdir()?;
        let text = numeric_deck(
            dir.path(),
            &format!(
                r#"Hierarchy
.option parhier={policy}
.param Val=1
Vchain n0 0 1
Vdefault n1 0 1
Vactual n2 0 1
X1 n0 0 Sub1
Xd n1 0 Cell
Xa n2 0 Cell Val=5
.subckt Sub1 a b Val=1
R1 a b Val
X2 a b Sub2
.ends Sub1
.subckt Sub2 a b Val=2
R2 a b Val
X3 a b Sub3
.ends Sub2
.subckt Sub3 a b Val=3
R3 a b Val
.ends Sub3
.subckt Cell a b Val=2
R1 a b Val
.ends Cell
.op
.end
"#
            ),
            "op\nwrdata op.dat i(vchain) i(vdefault) i(vactual)",
        )?;
        let actual_values = values(dir.path(), "op.dat")?;
        let row = actual_values.first().context("missing OP data")?;
        assert_eq!(row.len(), 4);
        for (&measured, expected) in row[1..].iter().zip([chain, default, actual]) {
            ensure!(
                (measured - expected).abs() < 1e-10,
                "{policy}: {measured} != {expected}\n{text}"
            );
        }
    }
    Ok(())
}

#[test]
fn nested_multiplier_composes_existing_element_values_in_dc_and_ac() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let text = numeric_deck(
        dir.path(),
        r#"Nested multiplier
V1 a 0 dc 1 ac 1
.param factor=2
X1 a 0 outer m={factor}
.subckt outer p n
X2 p n inner m=3
.ends outer
.subckt inner p n
R1 p n 12 m=5
C1 p n 2u m=4
.ends inner
.dc V1 0 2 1
.ac lin 2 1000 2000
.end
"#,
        "dc V1 0 2 1\nwrdata dc.dat i(v1)\nac lin 2 1000 2000\nwrdata ac.dat i(v1)",
    )?;
    for row in values(dir.path(), "dc.dat")? {
        ensure!((row[1] + row[0] * 2.5).abs() < 1e-10, "DC {row:?}\n{text}");
    }
    for row in values(dir.path(), "ac.dat")? {
        ensure!((row[1] + 2.5).abs() < 1e-10, "AC {row:?}");
        ensure!(
            (row[2] + std::f64::consts::TAU * row[0] * 48e-6).abs() < 1e-10,
            "AC {row:?}\n{text}"
        );
    }
    Ok(())
}

#[test]
fn nested_multiplier_and_quoted_values_preserve_transient_time_constant() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let text = numeric_deck(
        dir.path(),
        r#"Multiplied capacitor transient
V1 src 0 PULSE(0 1 0 1n 1n 10m 20m)
R1 src out 1k
X1 out 0 outer m=2
.subckt outer p n
X2 p n inner m=3
.ends outer
.subckt inner p n cval=1u
C1 p n 'cval/2' m=2
.ends inner
.tran 10u 6m
.end
"#,
        "tran 10u 6m 0 10u\nwrdata tran.dat v(out)",
    )?;
    let rows = values(dir.path(), "tran.dat")?;
    let row = rows
        .iter()
        .find(|row| row[0] >= 5e-3)
        .context("missing transient data")?;
    let expected = 1. - (-row[0] / 6e-3).exp();
    ensure!(
        (row[1] - expected).abs() < 5e-5,
        "{row:?} != {expected}\n{text}"
    );
    Ok(())
}
