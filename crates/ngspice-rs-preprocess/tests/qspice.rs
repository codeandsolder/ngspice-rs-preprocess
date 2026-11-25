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
        (&b"* comment\n.tran -1m\n"[..], Feature::TransientTiming),
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

#[test]
fn numeric_spelling_preserves_names_and_laws() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("deck.cir");
    fs::write(&root, b"* 3k4 title\n.param gain={1mho+2mho}\nV1 3k4 0 1\nR1 3k4 out 3k4\nR2 out 0 1k7\nC1 out 0 10\xb5\nD1 out 0 1k5\n.model 1k5 D(IS=1f5)\nB1 result 0 V={V(3k4)*2k5 + gain + 1e-3mho}\nE1 linear 0 out 0 1mho\n.dc V1 0 1 0.5\n.end\n")?;
    let bundle = prepare_file(&root, InputFormat::QspiceDeck, Vec::new())?;
    let text = std::str::from_utf8(&bundle.primary.bytes)?;
    for expected in [
        "* 3k4 title",
        ".param gain={1+2}",
        "V1 3k4 0 1",
        "R1 3k4 out 3.4k",
        "R2 out 0 1.7k",
        "C1 out 0 10u",
        "D1 out 0 1k5",
        ".model 1k5 D(IS=1.5f)",
        "V(3k4)*2.5k + gain + 1e-3",
        "E1 linear 0 out 0 1",
    ] {
        ensure!(text.contains(expected), "missing {expected:?} in {text}");
    }
    let rows = reference(
        dir.path(),
        "* numeric witness\n.param gain={1mho+2mho}\nV1 3k4 0 1\nR1 3k4 out 3k4\nR2 out 0 1k7\nB1 result 0 V={V(3k4)*2k5 + gain + 1e-3mho}\nE1 linear 0 out 0 1mho\n.dc V1 0 1 0.5\n.end\n",
        "run\nwrdata witness.dat v(out) v(result) v(linear)",
    )?;
    assert_eq!(rows.len(), 3);
    for row in rows {
        let [input, divider, result, linear] = row.as_slice() else {
            bail!("unexpected numeric witness shape");
        };
        ensure!((divider - input / 3.).abs() < 1e-10, "{row:?}");
        ensure!(
            (result - input.mul_add(2500., 3.001)).abs() < 1e-9,
            "{row:?}"
        );
        ensure!((linear - divider).abs() < 1e-10, "{row:?}");
    }
    Ok(())
}

#[test]
fn numeric_spelling_covers_continuations_and_analysis_values() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("deck.cir");
    fs::write(&root, b"Title\nV1 a 0 PULSE(0 1 1u5 1n 1n 2u5 1m)\nR1 a b 2Meg8\nC1 b 0 1n\n+ Rser=0mho\n.tran 0 1m5 0 1u5 UIC\n.end\n")?;
    let bundle = prepare_file(&root, InputFormat::QspiceDeck, Vec::new())?;
    let text = std::str::from_utf8(&bundle.primary.bytes)?;
    ensure!(text.contains("PULSE(0 1 1.5u 1n 1n 2.5u 1m)"), "{text}");
    ensure!(text.contains("R1 a b 2.8Meg"), "{text}");
    ensure!(text.contains("1.5m 0 1.5u UIC"), "{text}");
    ensure!(!text.contains("Rser"), "{text}");
    fs::write(&root, b"Title\nR1 a b 1.2k3\n.end\n")?;
    let error = prepare_file(&root, InputFormat::QspiceDeck, Vec::new())
        .err()
        .context("ambiguous literal accepted")?;
    let diagnostic = error
        .downcast_ref::<Unsupported>()
        .context("missing numeric diagnostic")?;
    assert_eq!(diagnostic.feature, Feature::NumericLiteral);
    assert_eq!(diagnostic.line, 2);
    Ok(())
}

#[test]
fn relative_pwl_is_cumulative_and_preserves_value_signs() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let source = "* PWL witness\nV1 a 0 PWL 0 0 1m 0 +1u +1 +2m 1 +1u 0\nI1 0 b PWL(0 0, 1m 0, +1u +1,\n+ +2m 1, +1u 0)\nR1 b 0 1\n.tran 0 4m 0 100n\n.end\n";
    let root = dir.path().join("source.cir");
    fs::write(&root, source)?;
    let bundle = prepare_file(&root, InputFormat::QspiceDeck, Vec::new())?;
    let output = std::str::from_utf8(&bundle.primary.bytes)?;
    assert!(output.contains("V1 a 0 PWL("));
    assert!(output.contains("+1"));
    let Provenance::Native { lines } = &bundle.primary.provenance else {
        bail!("missing source map");
    };
    assert_eq!(lines.len(), source.lines().count());
    let rows = reference(dir.path(), source, "run\nwrdata witness.dat v(a) v(b)")?;
    let mut count = 0;
    for row in rows {
        let [time, voltage, current] = row.as_slice() else {
            bail!("invalid waveform trace");
        };
        let expected = if *time <= 0.001 {
            0.
        } else if *time < 0.001_001 {
            (*time - 0.001) / 1e-6
        } else if *time < 0.003_001 {
            1.
        } else if *time < 0.003_002 {
            (0.003_002 - *time) / 1e-6
        } else {
            0.
        };
        ensure!(
            (voltage - expected).abs() < 1e-8 && (current - expected).abs() < 1e-8,
            "{row:?}, expected {expected}"
        );
        count += 1;
    }
    ensure!(count > 100, "missing waveform observations");
    Ok(())
}

#[test]
fn unqualified_waveform_laws_are_source_located() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("source.cir");
    for card in [
        "V1 a 0 PWL REPEAT FOR 2 0 0 1u 1 ENDREPEAT",
        "V1 a 0 PWL(FILE 3k4.csv)",
        "V1 a 0 PWL(0 0 1u 1 1u 2)",
        "V1 a 0 PWL(0 0 1u 1 +{dt} 2)",
        "V1 a 0 PWL(0 0 1u 1) TRIGGER=V(a)",
        "V1 a 0 PULSE(0 1 -1u 1n 1n 1u 2u)",
        "V1 a 0 PULSE(0 1 0 0 0 1u 2u)",
        "V1 a 0 PULSE(0 1 0 1n 1n 1u 2u 3)",
        "V1 a 0 1 Rser=1",
    ] {
        fs::write(&root, format!("Title\n{card}\n.end\n"))?;
        let error = prepare_file(&root, InputFormat::QspiceDeck, Vec::new())
            .err()
            .with_context(|| format!("unsupported waveform accepted: {card}"))?;
        let diagnostic = error
            .downcast_ref::<Unsupported>()
            .with_context(|| format!("missing typed diagnostic for {card}: {error:#}"))?;
        assert_eq!(diagnostic.feature, Feature::Waveform);
        assert_eq!(diagnostic.line, 2);
    }
    Ok(())
}

#[test]
fn bare_parameter_references_bind_per_occurrence() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let source = "* Scoped bare parameters\n.subckt divider a b resistance=1k\nR1 a b resistance\nR2 b 0 base\n.ends divider\nV1 a 0 3\nX1 a b divider resistance=base\nX2 a c divider resistance=other\n.param base=2k other=4k\nE1 output 0 b c gain\n.param gain=3\n.op\n.end\n";
    let root = dir.path().join("source.cir");
    fs::write(&root, source)?;
    let bundle = prepare_file(&root, InputFormat::QspiceDeck, Vec::new())?;
    let text = std::str::from_utf8(&bundle.primary.bytes)?;
    for expected in [
        "R1 a b {resistance}",
        "R2 b 0 {base}",
        "X1 a b divider resistance={base}",
        "X2 a c divider resistance={other}",
        "E1 output 0 b c {gain}",
    ] {
        ensure!(text.contains(expected), "missing {expected:?} in {text}");
    }
    let rows = reference(
        dir.path(),
        source,
        "run\nwrdata witness.dat v(b) v(c) v(output)",
    )?;
    assert_eq!(rows.len(), 1);
    let [_, first, second, difference] = rows[0].as_slice() else {
        bail!("invalid OP trace");
    };
    ensure!((first - 1.5).abs() < 1e-10, "{rows:?}");
    ensure!((second - 1.).abs() < 1e-10, "{rows:?}");
    ensure!((difference - 1.5).abs() < 1e-10, "{rows:?}");
    Ok(())
}

#[test]
fn bjt_assertions_use_visible_model_and_preserve_terminals() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("source.cir");
    let source = "* Scoped bipolar polarity\n.model polarity NPN(IS=1e-15 BF=100)\n.subckt highside c b e\nQinner c b e 0 polarity PNP\n.model polarity PNP(IS=1e-15 BF=100)\n.ends highside\nQouter c b 0 polarity\n+ NPN ; assert NPN\nQfour c b 0 0 polarity NPN\nV1 c 0 2\nV2 b 0 0.6\n.op\n.end\n";
    fs::write(&root, source)?;
    let bundle = prepare_file(&root, InputFormat::QspiceDeck, Vec::new())?;
    let text = std::str::from_utf8(&bundle.primary.bytes)?;
    assert!(text.contains("Qinner c b e 0 polarity"));
    assert!(text.contains("Qouter c b 0 polarity"));
    assert!(text.contains("Qfour c b 0 0 polarity"));
    assert!(!text.contains("polarity PNP\n.model"));
    assert!(!text.contains("Qfour c b 0 0 polarity NPN"));
    assert!(text.contains("assert NPN"));
    let Provenance::Native { lines } = &bundle.primary.provenance else {
        bail!("missing source map");
    };
    assert_eq!(lines.len(), source.lines().count());
    for malformed in [
        source.replace("0 polarity PNP", "0 polarity NPN"),
        source.replace("+ NPN", "+ PNP"),
        source.replace("Qfour c b 0 0 polarity NPN", "Qfour c b 0 0 absent NPN"),
        source.replace(
            ".model polarity NPN(IS=1e-15 BF=100)",
            ".model polarity NPN\n.model polarity PNP",
        ),
    ] {
        fs::write(&root, malformed)?;
        let error = prepare_file(&root, InputFormat::QspiceDeck, Vec::new())
            .err()
            .context("contradictory or unresolved polarity accepted")?;
        let diagnostic = error
            .downcast_ref::<Unsupported>()
            .with_context(|| format!("{error:#}"))?;
        assert_eq!(diagnostic.feature, Feature::Binding);
    }
    Ok(())
}

#[test]
fn transient_window_defaults_and_symbolic_options() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("deck.cir");
    let source = "* timing\nV1 a 0 1\nR1 a b 1k\nC1 b 0 1u\n.tran 2m UIC\n.end\n";
    fs::write(&root, source)?;
    let bundle = prepare_file(&root, InputFormat::QspiceDeck, Vec::new())?;
    let output = std::str::from_utf8(&bundle.primary.bytes)?;
    let tran = output
        .lines()
        .find(|line| line.starts_with(".tran"))
        .context("missing transient")?;
    let fields = tran.split_ascii_whitespace().collect::<Vec<_>>();
    assert_eq!(&fields[2..], ["2m", "0", fields[1], "UIC"]);
    ensure!(
        (fields[1].parse::<f64>()? - 0.002 / 1024.0).abs() < 1e-15,
        "{tran}"
    );

    let source = "* timing\n.param duration=2m window=1m frequency=1000\nV1 a 0 1\nR1 a b 1k\nC1 b 0 1u\n.tran 0 duration window UIC\n.option reltol=1e-5\n+ maxstep=1/frequency/1000\n.option ABSOLUTETIME\n.end\n";
    fs::write(&root, source)?;
    let bundle = prepare_file(&root, InputFormat::QspiceDeck, Vec::new())?;
    let output = std::str::from_utf8(&bundle.primary.bytes)?;
    ensure!(output.contains("{window}"), "{output}");
    ensure!(output.contains("{1/frequency/1000}"), "{output}");
    ensure!(output.contains(".option reltol=1e-5"), "{output}");
    ensure!(
        !output.to_ascii_lowercase().contains("maxstep="),
        "{output}"
    );
    let Provenance::Native { lines } = &bundle.primary.provenance else {
        bail!("missing source map");
    };
    assert_eq!(lines.len(), source.lines().count());

    let rows = reference(dir.path(), source, "run\nwrdata witness.dat v(b)")?;
    let first = rows.first().context("no transient samples")?;
    let last = rows.last().context("no transient samples")?;
    ensure!(first[0] >= 0.001 && first[0] <= 0.001_001_1, "{first:?}");
    ensure!((last[0] - 0.002).abs() < 1e-12, "{last:?}");
    for row in rows {
        let [time, voltage] = row.as_slice() else {
            bail!("invalid transient trace");
        };
        ensure!((voltage + (-time / 0.001).exp_m1()).abs() < 2e-4, "{row:?}");
    }
    Ok(())
}

#[test]
fn transient_rejects_unqualified_output_time_and_option_precedence() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("deck.cir");
    for (source, line) in [
        ("Title\n.tran 0 2m 1m\n.end\n", 2),
        ("Title\n.tran 0 2m 0 2u\n.option maxstep=1u\n.end\n", 2),
        ("Title\n.option maxstep=0\n.tran 2m\n.end\n", 2),
        ("Title\n.option maxstep=1u\n.end\n", 2),
        (
            "Title\n.if (1)\n.option maxstep=1u\n.endif\n.tran 2m\n.end\n",
            3,
        ),
    ] {
        fs::write(&root, source)?;
        let error = prepare_file(&root, InputFormat::QspiceDeck, Vec::new())
            .err()
            .context("unqualified timing accepted")?;
        let diagnostic = error
            .downcast_ref::<Unsupported>()
            .with_context(|| format!("missing structured diagnostic: {error:#}"))?;
        assert_eq!(diagnostic.feature, Feature::TransientTiming);
        assert_eq!(diagnostic.line, line);
    }
    Ok(())
}
