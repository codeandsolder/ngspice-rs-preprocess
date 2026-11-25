//! Independent circuit-law witnesses for the QSPICE source adapter.
use anyhow::{Context, Result, bail, ensure};
use model_normalize_ngspice::qspice::{Feature, Unsupported};
use ngspice_rs_preprocess::{
    Cancellation, InputFormat, Options, PreparedBundle, Provenance, Request, SourceInput, prepare,
};
use std::{fs, path::Path, process::Command};

fn prepare_file(
    path: &Path,
    format: InputFormat,
    search_paths: Vec<std::path::PathBuf>,
) -> Result<PreparedBundle> {
    prepare(
        &Request {
            format,
            input: SourceInput::File(path.to_owned()),
            libraries: Vec::new(),
            search_paths,
        },
        &Options::default(),
        &Cancellation::default(),
    )
}

#[test]
fn whole_file_nested_section_and_physical_provenance() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("deck.cir");
    let models = dir.path().join("models");
    fs::create_dir(&models)?;
    let part = models.join("part.lib");
    fs::write(
        &part,
        b".lib slow\nRbad a b 9k\n.endl slow\n.lib fast\n.lib nested.lib\n.endl fast\n",
    )?;
    fs::write(models.join("nested.lib"), b".subckt coil a b\r\nL1 a b 1m\r\n+ Rpar = 0\r\n+ Rser=0 ; ideal winding\r\n.ends coil\r\n.end\r\n")?;
    fs::write(&root, b"* QSPICE circuit\n.lib models\\part.lib fast\nV1 a 0 1\nX1 a b coil\nR1 b 0 10\n.libpath models\n.op\n.end\n")?;
    let bundle = prepare_file(&root, InputFormat::QspiceDeck, Vec::new())?;
    let text = std::str::from_utf8(&bundle.primary.bytes)?;
    assert!(!text.contains("Rbad"));
    assert!(!text.contains("Rpar"));
    assert!(!text.contains("Rser"));
    assert!(text.contains("ideal winding"));
    assert!(text.contains("L1 a b 1m"));
    let Provenance::Native { lines } = &bundle.primary.provenance else {
        bail!("missing source map");
    };
    assert_eq!(lines.len(), text.lines().count());
    let nested = models.join("nested.lib").canonicalize()?;
    for (line, origin) in text.lines().zip(lines) {
        if line.contains("L1 a b") {
            assert_eq!(origin.source, nested);
            assert_eq!(origin.line, 2);
        }
        if line.contains("ideal winding") {
            assert_eq!(origin.source, nested);
            assert_eq!(origin.line, 4);
        }
    }
    assert!(prepare_file(&root, InputFormat::NativeDeck, Vec::new()).is_err());
    Ok(())
}

#[test]
fn diagnostics_preserve_original_resource_and_line() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("deck.cir");
    let part = dir.path().join("part.lib");
    fs::write(&root, b"Title\n.lib part.lib\n.end\n")?;
    for (content, feature) in [
        (&b"* comment\nL1 a b 1u\n"[..], Feature::PassiveParasitics),
        (
            &b"* comment\nL1 a b 1u Rpar=1k\n"[..],
            Feature::PassiveParasitics,
        ),
        (
            &b"* comment\n.model d1 D(Ron=1 Vfwd=0.3)\n"[..],
            Feature::ModelLaw,
        ),
        (
            &b"* comment\n.model sw1 SW(Ron=1 Vh=-0.3)\n"[..],
            Feature::ModelLaw,
        ),
        (
            &b"* comment\n\xa5dev a b model\n"[..],
            Feature::SpecialDevice,
        ),
        (
            "* comment\n¥dev a b model\n".as_bytes(),
            Feature::SpecialDevice,
        ),
        (&b"* comment\n.libpath models\n"[..], Feature::Directive),
        (
            &b"* comment\n* undefined \x81\n"[..],
            Feature::SourceEncoding,
        ),
        (&b"* comment\n.tran 1m\n"[..], Feature::TransientTiming),
    ] {
        fs::write(&part, content)?;
        let error = prepare_file(&root, InputFormat::QspiceDeck, Vec::new())
            .err()
            .context("unsupported input accepted")?;
        let diagnostic = error
            .downcast_ref::<Unsupported>()
            .with_context(|| format!("missing structured diagnostic: {error:#}"))?;
        assert_eq!(diagnostic.feature, feature);
        assert_eq!(diagnostic.path, part.canonicalize()?);
        assert_eq!(diagnostic.line, 2);
    }
    let schematic = dir.path().join("project.qsch");
    fs::write(&schematic, b"not a circuit deck")?;
    let error = prepare_file(&schematic, InputFormat::QspiceDeck, Vec::new())
        .err()
        .context("schematic accepted")?;
    assert_eq!(
        error
            .downcast_ref::<Unsupported>()
            .context("missing resource diagnostic")?
            .feature,
        Feature::NonTextResource
    );
    Ok(())
}

#[test]
fn root_search_order_and_legacy_encoding() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("deck.cir");
    let first = dir.path().join("first dir");
    let second = dir.path().join("second");
    fs::create_dir(&first)?;
    fs::create_dir(&second)?;
    fs::write(
        first.join("part.lib"),
        b"* legacy \x95\n.model d\x95x D(IS=1e-18)\n",
    )?;
    fs::write(second.join("part.lib"), b".model wrong D\n")?;
    fs::write(&root, b"* legacy title \x95\n.lib part.lib\nV1 a 0 1\nD1 a 0 d\x95x\n.libpath \"first dir\"\n.libpath second\n.op\n.end\n")?;
    let bundle = prepare_file(&root, InputFormat::QspiceDeck, Vec::new())?;
    let text = std::str::from_utf8(&bundle.primary.bytes)?;
    assert!(text.starts_with("* legacy title •\n"));
    assert!(text.contains(".model d•x D(IS=1e-18)"));
    assert!(text.contains("D1 a 0 d•x"));
    assert!(!text.contains(".model wrong"));
    assert!(!text.lines().any(|line| line.starts_with(".libpath")));
    let Provenance::Native { lines } = &bundle.primary.provenance else {
        bail!("missing source map");
    };
    assert_eq!(lines.len(), text.lines().count());
    fs::write(
        dir.path().join("part.lib"),
        b"\xef\xbb\xbf.model d\xe2\x80\xa2x D(IS=2e-18)\n",
    )?;
    let bundle = prepare_file(&root, InputFormat::QspiceDeck, Vec::new())?;
    let text = std::str::from_utf8(&bundle.primary.bytes)?;
    assert!(text.contains(".model d•x D(IS=2e-18)"));
    Ok(())
}

fn reference(dir: &Path, circuit: &str, control: &str) -> Result<Vec<Vec<f64>>> {
    let source = dir.join("source.cir");
    fs::write(&source, circuit)?;
    let bundle = prepare_file(&source, InputFormat::QspiceDeck, Vec::new())?;
    let deck = std::str::from_utf8(&bundle.primary.bytes)?
        .strip_suffix(".end\n")
        .context("missing end")?;
    fs::write(
        dir.join("reference.cir"),
        format!(
            "{deck}.control\nset numdgt=17\nset wr_singlescale\n{control}\nquit\n.endc\n.end\n"
        ),
    )?;
    let output = Command::new("ngspice")
        .current_dir(dir)
        .args(["-b", "-n", "reference.cir"])
        .output()?;
    let log = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    ensure!(
        output.status.success() && !log.to_ascii_lowercase().contains("error:"),
        "{log}"
    );
    fs::read_to_string(dir.join("witness.dat"))?
        .lines()
        .map(|line| {
            line.split_ascii_whitespace()
                .map(|field| Ok(field.parse::<f64>()?))
                .collect()
        })
        .collect()
}

#[test]
fn native_reference_matches_dc_ac_and_transient_laws() -> Result<()> {
    let dir = tempfile::tempdir()?;
    fs::write(
        dir.path().join("divider.lib"),
        b".subckt divider a b\nR1 a b 1k\nR2 b 0 1k\n.ends divider\n",
    )?;
    let rows = reference(
        dir.path(),
        "* divider\n.lib divider.lib\nV1 a 0 2\nX1 a b divider\n.dc V1 0 2 0.5\n.end\n",
        "run\nwrdata witness.dat v(b)",
    )?;
    assert_eq!(rows.len(), 5);
    for row in rows {
        let [voltage, output] = row.as_slice() else {
            bail!("invalid DC trace");
        };
        ensure!((output - voltage * 0.5).abs() < 1e-10, "DC: {row:?}");
    }
    let rows = reference(
        dir.path(),
        "* RL lowpass\nV1 a 0 AC 1\nL1 a b 1m Rpar=0 Rser=0\nR1 b 0 10\n.ac lin 9 100 8100\n.end\n",
        "run\nwrdata witness.dat v(b)",
    )?;
    assert_eq!(rows.len(), 9);
    for row in rows {
        let [frequency, real, imag] = row.as_slice() else {
            bail!("invalid AC trace");
        };
        let x = std::f64::consts::TAU * frequency * 0.001 / 10.;
        let denominator = x.mul_add(x, 1.);
        ensure!(
            (real - 1. / denominator).abs() < 1e-10 && (imag + x / denominator).abs() < 1e-10,
            "AC: {row:?}"
        );
    }
    let rows = reference(
        dir.path(),
        "* RC step\nV1 a 0 1\nR1 a b 1k\nC1 b 0 1u Rser=0\n.tran 5m 4m 0 2u UIC\n.end\n",
        "run\nwrdata witness.dat v(b)",
    )?;
    ensure!(rows.len() > 100, "missing transient samples");
    for row in &rows {
        let [time, output] = row.as_slice() else {
            bail!("invalid transient trace");
        };
        let expected = -(-time / 0.001).exp_m1();
        ensure!(
            (output - expected).abs() < 2e-4,
            "transient: {row:?}, expected {expected}"
        );
    }
    let last = rows.last().context("missing end time")?;
    ensure!(
        (last[0] - 0.005).abs() < 1e-12,
        "wrong QSPICE stop time: {last:?}"
    );
    Ok(())
}
