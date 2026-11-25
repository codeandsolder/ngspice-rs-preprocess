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
        ".lib is a title, not a directive\n.inc 'child.lib' $ comment\nV1 a 0 'hs+ps+again'\n.end\n",
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
            "{deck}.control\nset numdgt=17\nset wr_singlescale\nop\nwrdata op.dat v(a)\nquit\n.endc\n.end\n"
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
    let actual: f64 = data
        .split_ascii_whitespace()
        .nth(1)
        .context("missing voltage")?
        .parse()?;
    ensure!((actual - 2. * 100_f64.ln()).abs() < 1e-10, "{actual}");
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
        (".subckt cell a b r=1k\n.ends cell", "hierarchy lowering"),
        ("X1 a b cell m=4", "hierarchy lowering"),
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
