//! Standalone HSPICE source observations from the public HSPICE user
//! guide, with independent analytic circuit expectations in native ngspice.
use anyhow::{Context, Result, ensure};
use ngspice_rs_preprocess::{
    Cancellation, InputFormat, Options, PreparedBundle, Provenance, Request, SourceInput, prepare,
};
use std::{fs, path::Path, process::Command};

fn prepared(path: &Path, format: InputFormat) -> Result<PreparedBundle> {
    prepare(
        &Request {
            format,
            input: SourceInput::File(path.to_owned()),
            libraries: Vec::new(),
            search_paths: Vec::new(),
        },
        &Options::default(),
        &Cancellation::default(),
    )
}

fn execute(bundle: &PreparedBundle, script: &str) -> Result<Vec<f64>> {
    let dir = tempfile::tempdir()?;
    let mut deck = bundle
        .primary
        .bytes
        .strip_suffix(b".end\n")
        .context("missing terminator")?
        .to_vec();
    deck.extend_from_slice(
        format!(".control\nset numdgt=17\nset wr_singlescale\n{script}\nquit\n.endc\n.end\n")
            .as_bytes(),
    );
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
    Ok(fs::read_to_string(dir.path().join("values.dat"))?
        .split_whitespace()
        .map(str::parse::<f64>)
        .collect::<std::result::Result<Vec<_>, _>>()?)
}

fn op(bundle: &PreparedBundle, vectors: &str, expected: &[f64]) -> Result<()> {
    let rows = execute(bundle, &format!("op\nwrdata values.dat {vectors}"))?;
    let actual = rows.get(1..).context("missing operating point")?;
    ensure!(
        actual.len() == expected.len(),
        "shape mismatch: {actual:?} != {expected:?}"
    );
    for (&actual, &expected) in actual.iter().zip(expected) {
        let scale = expected.abs().max(1e-21);
        ensure!(
            (actual - expected).abs() <= 1e-9 * scale,
            "{actual} != {expected}"
        );
    }
    Ok(())
}

#[test]
fn scalar_semantics_are_not_ngspice_global_compatibility() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("root.sp");
    fs::write(&path, b"standalone scalar semantics\n.param x=2X y=3a z=4D-3\n.param l='LOG(-2)' s='SQRT(-9)' d='DB(-10)'\n.param p='(-2)**2.9' n='(-2)**(-2.9)' q='0**(-3)'\n.param k='POW(-2,2.9)' f='PWR(-4,0.5)' t='INT(-2.9)'\nV1 2X 0 'x'\nV2 b 0 'y'\nV3 c 0 'z'\nV4 d 0 'l'\nV5 e 0 's'\nV6 f 0 'd'\nV7 g 0 'p'\nV8 h 0 'n'\nV9 j 0 'q'\nV10 k 0 'k'\nV11 m 0 'f'\nV12 n 0 't'\n.end\n")?;
    let bundle = prepared(&path, InputFormat::HspiceDeck)?;
    op(
        &bundle,
        "v(2x) v(b) v(c) v(d) v(e) v(f) v(g) v(h) v(j) v(k) v(m) v(n)",
        &[
            2e6,
            3e-18,
            4e-3,
            -2.0_f64.ln(),
            -3.,
            -20.,
            4.,
            0.25,
            0.,
            4.,
            -2.,
            -2.,
        ],
    )?;
    let text = std::str::from_utf8(&bundle.primary.bytes)?;
    ensure!(text.starts_with("standalone scalar semantics\n"), "{text}");
    ensure!(
        text.contains(".options tnom=25") && text.contains(".temp 25"),
        "{text}"
    );
    Ok(())
}

#[test]
fn named_sections_and_resource_language_preserve_spans() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("root.sp");
    let library = dir.path().join("sections.lib");
    let child = dir.path().join("foreign.lib");
    fs::write(&library, b".lib tt\n.param gain=2X\n+ factor='SQRT(-9)' $ comment\n.endl tt\n.lib ss\n.param gain=99 factor=99\n.endl ss\n")?;
    fs::write(&child, b"*#PSPICE\n.param foreign='PWR(-2,2)' ; comment\n")?;
    fs::write(&root, b"title containing .param x=1\n.lib 'sections.lib' tt\n.inc 'foreign.lib'\n.param own='PWR(-2,2)'\nV1 a 0 'gain'\nV2 b 0 'factor'\nV3 c 0 'foreign'\nV4 d 0 'own'\n.end\n")?;
    let bundle = prepared(&root, InputFormat::HspiceDeck)?;
    op(&bundle, "v(a) v(b) v(c) v(d)", &[2e6, -3., 4., -4.])?;
    let Provenance::Native { lines } = &bundle.primary.provenance else {
        anyhow::bail!("missing exact source spans");
    };
    ensure!(
        lines
            .iter()
            .any(|line| line.source == library && line.line == 2 && line.last_line == 3),
        "{lines:?}"
    );
    ensure!(
        lines
            .iter()
            .any(|line| line.source == child && line.line == 2),
        "{lines:?}"
    );
    Ok(())
}

#[test]
fn nested_multipliers_preserve_explicit_products() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("root.sp");
    fs::write(&root, b"nested product\n.param factor=3\nV1 a 0 1\nX1 a 0 outer M=2\n.subckt outer p n\nX2 p n leaf M='factor'\n.ends\n.subckt leaf p n\nR1 p n 1k M=4\n.ends\n.end\n")?;
    let bundle = prepared(&root, InputFormat::HspiceDeck)?;
    op(&bundle, "i(v1)", &[-24e-3])
}

#[test]
fn behavioral_controlled_sources_are_native() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("root.sp");
    fs::write(&root, b"controlled sources\nV1 input 0 -9\nE1 output 0 VOL='SQRT(V(input))'\nR1 output 0 1k\nG1 0 load CUR='PWR(V(input),0.5)'\nR2 load 0 1\nE2 linear 0 VCVS input 0 2\n.end\n")?;
    let bundle = prepared(&root, InputFormat::HspiceDeck)?;
    op(
        &bundle,
        "v(output) v(load) v(linear) i(e1)",
        &[-3., -3., -18., 3e-3],
    )
}

#[test]
fn standalone_library_is_include_safe_and_unknown_options_fail() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("library.sp");
    fs::write(
        &root,
        b".param small=1a\n.subckt rpair a b\nR1 a b 2X\n.ends\n.end\n",
    )?;
    let bundle = prepared(&root, InputFormat::HspiceLibrary)?;
    let text = std::str::from_utf8(&bundle.primary.bytes)?;
    ensure!(
        bundle.primary.filename == "model.lib"
            && !text
                .lines()
                .any(|line| line.trim().eq_ignore_ascii_case(".end")),
        "{text}"
    );
    ensure!(text.contains("2e6") && text.contains("1e-18"), "{text}");
    fs::write(
        &root,
        b"unqualified geometry policy\n.options scale=1u\n.end\n",
    )?;
    ensure!(
        prepared(&root, InputFormat::HspiceDeck).is_err(),
        "unsupported geometry option was accepted"
    );
    Ok(())
}

#[test]
fn parhier_resolves_global_and_local_parameter_precedence() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("root.sp");
    for (option, current) in [("GLOBAL", -1.), ("LOCAL", -0.2)] {
        fs::write(
            &root,
            format!(
                "hierarchical precedence\n.options parhier={option}\n.param resistance=1\nV1 a 0 1\nX1 a 0 outer resistance=2\n.subckt outer p n resistance=3\n.param resistance=4\nX2 p n leaf resistance=5\n.ends\n.subckt leaf p n resistance=6\nR1 p n 'resistance'\n.ends\n.end\n"
            ),
        )?;
        op(
            &prepared(&root, InputFormat::HspiceDeck)?,
            "i(v1)",
            &[current],
        )?;
    }
    Ok(())
}

#[test]
fn parhier_specializes_inherited_names_not_actual_values() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("root.sp");
    fs::write(&root, b"scope specialization\nV1 a 0 1\nV2 b 0 1\nV3 c 0 1\nX1 a 0 outer resistance=4\nX2 b 0 leaf\nX3 c 0 outer resistance=8\n.subckt outer p n resistance=2\nXchild p n leaf resistance=9\n.ends\n.subckt leaf p n resistance=10\nR1 p n resistance\n.ends\n.end\n")?;
    op(
        &prepared(&root, InputFormat::HspiceDeck)?,
        "i(v1) i(v2) i(v3)",
        &[-0.25, -0.1, -0.125],
    )
}

#[test]
fn rc_analyses_preserve_dc_ac_and_transient() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("rc.sp");
    fs::write(&root, b"RC response\nV1 a 0 DC 1 AC 1 PULSE(0 1 0 1n 1n 1 2)\nR1 a b 1k\nC1 b 0 1u\n.dc V1 0 1 0.5\n.ac lin 1 159.154943091895 159.154943091895\n.tran 10u 1m\n.end\n")?;
    let bundle = prepared(&root, InputFormat::HspiceDeck)?;
    let dc = execute(&bundle, "dc v1 0 1 0.5\nwrdata values.dat v(b)")?;
    ensure!(dc == [0., 0., 0.5, 0.5, 1., 1.], "{dc:?}");
    let ac = execute(
        &bundle,
        "ac lin 1 159.154943091895 159.154943091895\nwrdata values.dat v(b)",
    )?;
    ensure!(
        ac.len() == 3 && (ac[1] - 0.5).abs() < 1e-8 && (ac[2] + 0.5).abs() < 1e-8,
        "{ac:?}"
    );
    let tr = execute(&bundle, "tran 10u 1m\nwrdata values.dat v(b)")?;
    let last = tr.last().context("missing transient")?;
    ensure!((last - (1. - (-1_f64).exp())).abs() < 2e-4, "{last}");
    Ok(())
}
