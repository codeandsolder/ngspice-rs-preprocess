use anyhow::{Context, Result, ensure};
use ngspice_rs_preprocess::{Cancellation, InputFormat, Options, Request, SourceInput, prepare};
use std::{fs, path::Path, process::Command};

fn run(root: &Path, vectors: &str, expected: &[f64]) -> Result<()> {
    let bundle = prepare(
        &Request {
            format: InputFormat::SimetrixDeck,
            input: SourceInput::File(root.to_path_buf()),
            libraries: Vec::new(),
            search_paths: Vec::new(),
        },
        &Options::default(),
        &Cancellation::default(),
    )?;
    let dir = tempfile::tempdir()?;
    for artifact in &bundle.companions {
        fs::write(dir.path().join(&artifact.filename), &artifact.bytes)?;
    }
    let mut deck = bundle
        .primary
        .bytes
        .strip_suffix(b".end\n")
        .context("missing terminator")?
        .to_vec();
    deck.extend_from_slice(format!(".control\nset numdgt=17\nset wr_singlescale\nop\nwrdata op.dat {vectors}\nquit\n.endc\n.end\n").as_bytes());
    fs::write(dir.path().join("deck.cir"), &deck)?;
    let execution = Command::new("ngspice")
        .current_dir(dir.path())
        .args(["-b", "-n", "deck.cir"])
        .output()?;
    let log = format!(
        "{}\n{}",
        String::from_utf8_lossy(&execution.stdout),
        String::from_utf8_lossy(&execution.stderr)
    );
    ensure!(
        execution.status.success() && !log.to_ascii_lowercase().contains("error:"),
        "{log}\n{}",
        String::from_utf8_lossy(&deck)
    );
    let actual = fs::read_to_string(dir.path().join("op.dat"))?
        .split_ascii_whitespace()
        .skip(1)
        .map(str::parse::<f64>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    ensure!(actual.len() == expected.len(), "shape mismatch");
    for (actual, expected) in actual.into_iter().zip(expected) {
        ensure!(
            (actual - expected).abs() < 1e-10,
            "{actual} != {expected}\n{}",
            String::from_utf8_lossy(&deck)
        );
    }
    Ok(())
}

#[test]
fn nested_source_language_inherits_and_restores_with_named_sections() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path();
    fs::write(path.join("root.net"), b"* root\n*#SIMETRIX\n.param rootlog={LOG(100)} rootpow={PWR(-2,2)}\n.inc 'hs.lib' ; root comment\n.inc 'native.lib'\nV1 a 0 {rootlog+rootpow}\nV2 b 0 {hslog+hspow+grandlog+grandpow}\nV3 c 0 {pslog+pspow+afterlog+afterpow}\nV4 d 0 {native_log+native_pow}\n.end\n")?;
    fs::write(path.join("hs.lib"), b"*#HSPICE\n.param hslog='LOG(100)' hspow='PWR(-2,2)' $ child comment\n.lib 'sections.lib' tt\n.inc 'ps.lib'\n.param afterlog='LOG(100)' afterpow='PWR(-2,2)'\n")?;
    fs::write(path.join("sections.lib"), b".lib tt\n.param grandlog='LOG(100)' grandpow='PWR(-2,2)'\n.endl tt\n.lib ss\n.param grandlog=99 grandpow=99\n.endl ss\n")?;
    fs::write(
        path.join("ps.lib"),
        b"*#PSPICE\n.param pslog={LOG(100)} pspow={PWR(-2,2)} ; comment\n",
    )?;
    fs::write(
        path.join("native.lib"),
        b".param native_log={LOG(100)} native_pow={PWR(-2,2)}\n*#HSPICE\n",
    )?;
    run(
        &path.join("root.net"),
        "v(a) v(b) v(c) v(d)",
        &[
            6.,
            2.0_f64.mul_add(100.0_f64.ln(), -8.),
            2. * 100.0_f64.ln(),
            6.,
        ],
    )
}

#[test]
fn behavioral_source_grammar_controls_function_semantics() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("root.net");
    fs::write(&root, b"* behavioral\n*#HSPICE\nB1 a 0 V=LOG(100)+PWR(-2,2)\nE1 b 0 VALUE={LOG(100)+PWR(-2,2)}\n.end\n")?;
    run(&root, "v(a) v(b)", &[6., 100.0_f64.ln() + 4.])
}

#[test]
fn mixed_expression_quoting_and_bare_bias_are_not_global_compatibility_modes() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("root.net");
    fs::write(&root,b"* bias\n*#HSPICE\n.param WIDTH=2 MULT=4 dc_bias=15\nV1 a 0 dc_bias\nV2 b 0 DC dc_bias\nV3 c 0 DC=dc_bias\nV4 d 0 '{MULT}'\nV5 e 0 '6*{WIDTH}'\n.end\n")?;
    run(&root, "v(a) v(b) v(c) v(d) v(e)", &[15., 15., 15., 4., 12.])
}

#[test]
fn identity_resolution_crosses_includes_but_not_subcircuit_scopes() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("root.net");
    fs::write(&root, b"* identity\n*#SIMETRIX sep=:\nV1 in 0 1\nR1 in sense 1k\n.inc 'sensor.lib'\nB1 out 0 V=I(sense_source)*1000\nH1 controlled 0 sense_source 2000\nX1 sub 0 cell\n.end\n")?;
    fs::write(dir.path().join("sensor.lib"), b"V:sense_source sense 0 0\n.inc 'literal.lib'\n.subckt cell out rtn\nV:sense_source n 0 0\nV2 supply 0 3\nR2 supply n 1k\nB1 out 0 V=I(sense_source)*1000\n.ends cell\n")?;
    fs::write(dir.path().join("literal.lib"), b"*#SIMETRIX sep=none\nV$literal lit 0 0\nV3 sup 0 4\nR3 sup lit 1k\nB2 other 0 V=I(V$literal)*1000\n")?;
    run(
        &root,
        "v(out) v(controlled) v(other) v(sub)",
        &[1., 2., 4., 3.],
    )?;
    let bundle = prepare(
        &Request {
            format: InputFormat::SimetrixDeck,
            input: SourceInput::File(root),
            libraries: Vec::new(),
            search_paths: Vec::new(),
        },
        &Options::default(),
        &Cancellation::default(),
    )?;
    let names = bundle
        .device_identities
        .iter()
        .filter(|identity| identity.displayed == "sense_source")
        .collect::<Vec<_>>();
    assert_eq!(names.len(), 2);
    assert!(names.iter().any(|identity| identity.scope.is_empty()));
    assert!(names.iter().any(|identity| identity.scope == ["cell"]));
    assert!(
        names
            .iter()
            .all(|identity| identity.linked.starts_with("V__src_"))
    );
    let literal = bundle
        .device_identities
        .iter()
        .find(|identity| identity.declared == "V$literal")
        .context("missing literal device")?;
    assert_eq!(literal.displayed, "V$literal");
    assert!(literal.linked.starts_with("V__src_"));
    Ok(())
}

#[test]
fn duplicate_displayed_names_fail_even_when_types_differ() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("root.net");
    fs::write(
        &root,
        b"* duplicate\n*#SIMETRIX\nV$device a 0 1\nR$device a 0 1k\n.end\n",
    )?;
    let error = prepare(
        &Request {
            format: InputFormat::SimetrixDeck,
            input: SourceInput::File(root),
            libraries: Vec::new(),
            search_paths: Vec::new(),
        },
        &Options::default(),
        &Cancellation::default(),
    )
    .err()
    .context("duplicate accepted")?;
    ensure!(
        format!("{error:#}").contains("duplicate device identity"),
        "{error:#}"
    );
    Ok(())
}
