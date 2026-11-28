use anyhow::{Context, Result, ensure};
use ngspice_rs_preprocess::{
    Cancellation, InputFormat, LibraryInput, LibrarySimulator, Options, ParsedLibrary,
    PreparedBundle, Provenance, Request, SourceInput, prepare,
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
    let Provenance::Native { lines, .. } = &bundle.primary.provenance else {
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
fn standalone_library_keeps_neutral_source_and_binds_in_consuming_deck() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let lib = dir.path().join("lib.sp");
    let root = dir.path().join("root.sp");
    let child = dir.path().join("child.sp");
    fs::write(
        &lib,
        ".param gain='LOG(100)'\n.inc child.sp\n.subckt test in out scale=1\nE1 out 0 VCVS in 0 'gain*scale'\n.ends test\n",
    )?;
    fs::write(&child, "*#PSPICE\n.param other={PWR(-2,2)}\n")?;
    fs::write(
        &root,
        "Catalog consumption\n.option parhier=local\nV1 in 0 1\nX1 in out test scale=2\nR1 out 0 1k\n.end\n",
    )?;
    let bundle = compile(&lib, InputFormat::HspiceLibrary)?;
    let parsed = ParsedLibrary::from_json(&bundle.primary.bytes)?;
    assert_eq!(parsed.simulator(), LibrarySimulator::Hspice);
    assert_eq!(parsed.definitions(), 1);
    assert_eq!(parsed.scoped_statements()[0].text, ".param gain='LOG(100)'");
    let raw_request = Request {
        format: InputFormat::HspiceDeck,
        input: SourceInput::File(root.clone()),
        libraries: vec![LibraryInput::Source(SourceInput::File(lib.clone()))],
        search_paths: Vec::new(),
    };
    let raw = prepare(&raw_request, &Options::default(), &Cancellation::default())?;
    fs::remove_file(lib)?;
    fs::remove_file(child)?;
    let cached_request = Request {
        libraries: vec![LibraryInput::Parsed(parsed)],
        ..raw_request
    };
    let cached = prepare(
        &cached_request,
        &Options::default(),
        &Cancellation::default(),
    )?;
    assert_eq!(raw.primary.bytes, cached.primary.bytes);
    let text = std::str::from_utf8(&cached.primary.bytes)?;
    measure_native(dir.path(), text, "op\nwrdata op.dat v(out)")?;
    let actual = values(dir.path(), "op.dat")?[0][1];
    ensure!(
        (actual - 2. * 100_f64.ln()).abs() < 1e-10,
        "{actual} {text}"
    );
    let Provenance::Native { lines, .. } = &cached.primary.provenance else {
        anyhow::bail!("missing native spans");
    };
    assert!(
        lines
            .iter()
            .any(|line| line.source.ends_with("child.sp") && line.line == 2)
    );
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
        (".option XMULT_IN_EXP=maybe", "invalid HSPICE XMULT_IN_EXP"),
        (".model mos NMOS LEVEL=49", "qualified native mapping"),
        (".temp 25 50", "multiple-temperature"),
        (
            ".tran 1n 10n 2n 8n START=0",
            "invalid or non-progressing transient timing segment",
        ),
        (".dc gain 1 2 1", "parameter/temperature sweep"),
        (
            ".ac lin 1 1k 2k SWEEP gain 1 2",
            "unsupported HSPICE SWEEP coordinate syntax",
        ),
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
    measure_native(path, text, script)?;
    Ok(text.to_owned())
}

fn measure_native(path: &Path, text: &str, script: &str) -> Result<()> {
    let deck = text.strip_suffix(".end\n").context("missing terminator")?;
    fs::write(
        path.join("run.cir"),
        format!(
            "{deck}.control\nset numdgt=17\nset num_threads=1\nset wr_singlescale\n{script}\nquit\n.endc\n.end\n"
        ),
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
    Ok(())
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

#[test]
fn standalone_diode_breakdown_and_area_scaling() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let text = numeric_deck(
        dir.path(),
        r#"HSPICE dimensionless junction
V1 a 0 -5
V2 b 0 -5
D1 a 0 junction AREA=1
D2 b 0 junction AREA=7 M=2
.model junction D (IS=1p BV=5 IBV=100u)
.end
"#,
        "op\nwrdata op.dat i(v1) i(v2)",
    )?;
    let row = &values(dir.path(), "op.dat")?[0];
    ensure!((row[1] - 100e-6).abs() < 2e-7, "{row:?}\n{text}");
    ensure!((row[2] / row[1] - 14.).abs() < 0.002, "{row:?}\n{text}");
    Ok(())
}

#[test]
fn depletion_modes_and_lexical_model_occurrences() -> Result<()> {
    for mode in [1, 2] {
        let dir = tempfile::tempdir()?;
        let text = numeric_deck(
            dir.path(),
            &format!(
                r#"HSPICE junction charge
.option parhier=local
V1 a 0 dc 0.2 ac 1
V2 b 0 dc 0.2 ac 1
X1 a 0 outer cap=2n
X2 b 0 outer cap=5n
.subckt outer p n cap=1n
.model junction D (IS=1p CJ='cap' PB=0.8 M=0.5)
.subckt inner q r
D1 q r junction AREA=3 M=2
.ends inner
Xinside p n inner
.ends outer
.option dcap={mode}
.end
"#
            ),
            "ac lin 1 1000 1000\nwrdata ac.dat i(v1) i(v2)",
        )?;
        let row = &values(dir.path(), "ac.dat")?[0];
        let factor = if mode == 1 {
            (1.0_f64 - 0.2 / 0.8).powf(-0.5)
        } else {
            1. + 0.5 * 0.2 / 0.8
        };
        for (actual, cap) in [(row[2], 2e-9), (row[4], 5e-9)] {
            let expected = -std::f64::consts::TAU * 1000. * 6. * cap * factor;
            ensure!(
                (actual / expected - 1.).abs() < 1e-5,
                "{row:?} expected {expected}\n{text}"
            );
        }
    }
    Ok(())
}

#[test]
fn transient_start_is_bound_without_reinterpreting_schedules() -> Result<()> {
    use model_normalize_ngspice::model_card::parse_spice_constant;
    use ngspice_rs_preprocess::analysis_plan::{Directive, timing::IntegrationCeiling};
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("tran.sp");
    for (source, count, start, cap, uic) in [
        (".tran 1n 10n START=2n UIC", 1, 2e-9, 0.25e-9, true),
        (".tran 1n 10n 2n 0.25n", 1, 2e-9, 0.25e-9, false),
        (".tran 1n 10n 2n 20n START=2n", 2, 2e-9, 0.25e-9, false),
    ] {
        fs::write(
            &root,
            format!(
                "Transient\nV1 a 0 1\nR1 a 0 1k\n{source}\n.option DELMAX=.25n RELTOL=1e-4\n.end\n"
            ),
        )?;
        let bundle = compile(&root, InputFormat::HspiceDeck)?;
        let native = std::str::from_utf8(&bundle.primary.bytes)?;
        ensure!(
            !native
                .lines()
                .any(|line| line.to_ascii_lowercase().starts_with(".tran")),
            "{native}"
        );
        ensure!(
            !native
                .lines()
                .any(|line| line.starts_with(".options")
                    && line.to_ascii_uppercase().contains("DELMAX")),
            "{native}"
        );
        ensure!(
            native.to_ascii_lowercase().contains("reltol=1e-4"),
            "{native}"
        );
        let plan = bundle.analysis_plan.context("missing HSPICE plan")?;
        assert_eq!(plan.entries.len(), 1);
        assert_eq!(plan.entries[0].statement, source);
        let Directive::ScheduledTransient { intent } = &plan.entries[0].directive else {
            anyhow::bail!("missing schedule");
        };
        let timing = intent.bind(parse_spice_constant)?;
        assert_eq!(timing.output.len(), count);
        assert!((timing.start - start).abs() < 1e-22);
        assert_eq!(timing.uic, uic);
        let IntegrationCeiling::Explicit(ceiling) = timing.ceiling else {
            anyhow::bail!("missing explicit integration ceiling");
        };
        assert!((ceiling[0].step - cap).abs() < 1e-22);
    }
    Ok(())
}

#[test]
fn segmented_transient_preserves_continuation_origins_and_candidate_axes() -> Result<()> {
    use model_normalize_ngspice::model_card::parse_spice_constant;
    use model_preprocess_command::WorkBudget;
    use ngspice_rs_preprocess::analysis_plan::{
        Directive, DirectiveOrigin, sweep::SweepTarget, timing::IntegrationCeiling,
    };
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("tran.sp");
    let child = dir.path().join("analysis.sp");
    fs::write(
        &root,
        "Timing\n.param duration=10n\n.param gain=1\nV1 a 0 1\nR1 a 0 1k\n.inc 'analysis.sp'\n.op\n.end\n",
    )?;
    fs::write(
        &child,
        ".tran 1n 'duration'\n+ 2n 30n START=2n SWEEP gain LIN 3 1 3 SWEEP TEMP 25 75 25\n",
    )?;
    let bundle = compile(&root, InputFormat::HspiceDeck)?;
    let plan = bundle.analysis_plan.context("missing plan")?;
    assert_eq!(plan.entries.len(), 2);
    assert_eq!(plan.native_requests().count(), 1);
    assert_eq!(plan.title.as_deref(), Some("Timing"));
    let entry = &plan.entries[0];
    assert!(
        matches!(&entry.origin, DirectiveOrigin::Source { path, first_line: 1, last_line: 2 } if path == &child)
    );
    assert_eq!(entry.independent_axes.len(), 2);
    assert_eq!(
        entry.independent_axes[0].target,
        SweepTarget::Parameter {
            name: "gain".into()
        }
    );
    assert_eq!(entry.independent_axes[1].target, SweepTarget::Temperature);
    let bound = plan.bind_independent_sweeps(
        0,
        |_, _, raw| parse_spice_constant(raw),
        &WorkBudget::default(),
    )?;
    assert_eq!(bound.count(), 9);
    let Directive::ScheduledTransient { intent } = &entry.directive else {
        anyhow::bail!("missing schedule");
    };
    let timing = intent.bind(|raw| {
        if raw
            .trim_matches(['{', '}', '\''])
            .eq_ignore_ascii_case("duration")
        {
            Ok(10e-9)
        } else {
            parse_spice_constant(raw)
        }
    })?;
    assert_eq!(timing.output.len(), 2);
    assert_eq!(timing.ceiling, IntegrationCeiling::Automatic);
    let native = std::str::from_utf8(&bundle.primary.bytes)?;
    assert!(
        !native
            .lines()
            .any(|line| matches!(line.split_whitespace().next(), Some(".tran" | ".op")))
    );
    Ok(())
}

#[test]
fn zero_breakdown_sentinel_resolves_in_each_occurrence() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let text = numeric_deck(
        dir.path(),
        r#"Instance-specific breakdown
.option parhier=local
V1 a 0 -5
V2 b 0 -5
X1 a 0 junction bv=0.0
X2 b 0 junction bv=5
.subckt junction p n bv=0
.model dm D(IS=1p BV='bv' IBV=100u)
D1 p n dm
.ends junction
.end
"#,
        "op\nwrdata op.dat i(v1) i(v2)",
    )?;
    let row = &values(dir.path(), "op.dat")?[0];
    ensure!(row[1].abs() < 1e-9, "{row:?}\n{text}");
    ensure!((row[2] - 100e-6).abs() < 2e-7, "{row:?}\n{text}");
    Ok(())
}

#[test]
fn bsim4_occurrence_bins_match_independent_native_reference() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let reference = tempfile::tempdir()?;
    // HSPICE MOS reference J-2014.09, chapter 1: half-open geometry,
    // first matching declaration, per-finger W by default, lexical fallback.
    let source = r#"MOS bins
.option parhier=local
.param edge=1u
Vg g 0 dc 1 ac 1
Va a 0 0.8
Vb b 0 0.8
Vc c 0 0.8
Vd d 0 0.8
Ve e 0 0.8
Vf f 0 0.8
X1 a g cell ww=2u ll=edge
X2 b g cell ww=2u ll=0.9u
X3 c g cell ww=6u ll=edge
M4 d g 0 0 n W=2u L=edge NF=2 WNFLAG=0
M5 e g 0 0 n.early L=0.9u W=2u NF=2
M6 f g 0 0 n L=0.9u W=2u NF=2
.subckt cell d g ww=1u ll=1u
.model n.local NMOS LEVEL=54 VERSION=4.7 VTH0=0.6 LMIN=0.5u LMAX=1u WMIN=0.5u WMAX=2u
M1 d g 0 0 n L=ll W=ww NF=2
.ends cell
.model n.early NMOS LEVEL=54 VERSION=4.7.0 VTH0=0.35 LMIN=0.5u LMAX=edge WMIN=0.5u WMAX=2u
.model n.next NMOS LEVEL=54 VERSION=4.7.0 VTH0=0.45 LMIN=edge LMAX=2u WMIN=0.5u WMAX=2u
.model n.wide NMOS LEVEL=54 VERSION=4.7.0 VTH0=0.55 LMIN=0.5u LMAX=2u WMIN=2u WMAX=4u
.model n.overlap NMOS LEVEL=54 VERSION=4.7.0 VTH0=0.8 LMIN=0.5u LMAX=2u WMIN=0.5u WMAX=4u
.end
"#;
    let script = "dc Vg 0.5 1.5 0.1\nwrdata dc.dat i(va) i(vb) i(vc) i(vd) i(ve) i(vf)\nop\nac lin 2 1k 1meg\nwrdata ac.dat i(va) i(vb) i(vc) i(vd) i(ve) i(vf)";
    let text = numeric_deck(dir.path(), source, script)?;
    let expected = r#"Explicit model selection
.options TNOM=25
.temp 25
Vg g 0 dc 1 ac 1
Va a 0 0.8
Vb b 0 0.8
Vc c 0 0.8
Vd d 0 0.8
Ve e 0 0.8
Vf f 0 0.8
Ma a g 0 0 next W=2u L=1u NF=2
Mb b g 0 0 local W=2u L=0.9u NF=2
Mc c g 0 0 wide W=6u L=1u NF=2
Md d g 0 0 wide W=2u L=1u NF=2
Me e g 0 0 early W=2u L=0.9u NF=2
Mf f g 0 0 early W=2u L=0.9u NF=2
.model local NMOS LEVEL=14 VERSION=4.70 VTH0=0.6
.model early NMOS LEVEL=14 VERSION=4.70 VTH0=0.35
.model next NMOS LEVEL=14 VERSION=4.70 VTH0=0.45
.model wide NMOS LEVEL=14 VERSION=4.70 VTH0=0.55
.end
"#;
    measure_native(reference.path(), expected, script)?;
    for file in ["dc.dat", "ac.dat"] {
        let actual = values(dir.path(), file)?;
        let reference = values(reference.path(), file)?;
        assert_eq!(actual.len(), reference.len());
        for (actual, expected) in actual.iter().zip(reference) {
            assert_eq!(actual.len(), expected.len());
            for (&a, e) in actual.iter().zip(expected) {
                ensure!(
                    (a - e).abs() <= e.abs() * 1e-7 + 1e-13,
                    "{file}: {a:e} != {e:e}\n{text}"
                );
            }
        }
    }
    assert!(!text.contains("LMIN") && !text.contains("WMIN"), "{text}");
    Ok(())
}

#[test]
fn bsim4_reference_offsets_and_global_finger_option() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let text = numeric_deck(
        dir.path(),
        r#"MOS offset bins
.option WNFLAG=0 DEFL=0.9u DEFW=2u
Vg g 0 1
Vd d 0 0.8
M1 d g 0 0 n NF=2
.model n.low NMOS LEVEL=54 VERSION=4.64 VTH0=0.25 LMIN=0.5u LMAX=1u WMIN=1u WMAX=3u XL=0.1u XLREF=0
.model n.high NMOS LEVEL=54 VERSION=4.64 VTH0=0.6 LMIN=1u LMAX=2u WMIN=1u WMAX=3u XL=0.1u XLREF=0
.end
"#,
        "op\nwrdata op.dat i(vd)",
    )?;
    let expected = tempfile::tempdir()?;
    measure_native(
        expected.path(),
        r#"Selected upper bin
.options TNOM=25
.temp 25
Vg g 0 1
Vd d 0 0.8
M1 d g 0 0 selected L=0.9u W=2u NF=2
.model selected NMOS LEVEL=14 VERSION=4.64 VTH0=0.6 XL=0.1u
.end
"#,
        "op\nwrdata op.dat i(vd)",
    )?;
    let a = values(dir.path(), "op.dat")?[0][1];
    let e = values(expected.path(), "op.dat")?[0][1];
    ensure!(
        (a - e).abs() <= e.abs() * 1e-7 + 1e-13,
        "{a:e} != {e:e}\n{text}"
    );
    Ok(())
}

#[test]
fn bsim4_unqualified_versions_parameters_and_bin_gaps_reject() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("mos.sp");
    for (model, instance, expected) in [
        ("LEVEL=54", "L=1u W=2u", "exact supported VERSION"),
        (
            "LEVEL=54 VERSION=4.6.3",
            "L=1u W=2u",
            "exact supported VERSION",
        ),
        (
            "LEVEL=54 VERSION=4.7 ACM=2",
            "L=1u W=2u",
            "distinct junction kernel",
        ),
        (
            "LEVEL=54 VERSION=4.7 HSPICEONLY=1",
            "L=1u W=2u",
            "unqualified HSPICE BSIM4 model parameter",
        ),
        (
            "LEVEL=54 VERSION=4.7 LMIN=1u LMAX=2u WMIN=1u WMAX=3u",
            "L=2u W=2u",
            "no HSPICE MOS bin",
        ),
        (
            "LEVEL=54 VERSION=4.7 LMIN=1u LMAX=2u WMIN=1u WMAX=3u",
            "L=1u W=2u WNFLAG=2",
            "invalid HSPICE WNFLAG",
        ),
        (
            "LEVEL=54 VERSION=4.7 LMIN=1u LMAX=2u WMIN=1u WMAX=3u",
            "L=1u W=2u HSPICEONLY=3",
            "unqualified HSPICE BSIM4 instance parameter",
        ),
    ] {
        fs::write(
            &root,
            format!(
                "MOS rejection\nV1 d 0 1\nM1 d d 0 0 n {instance}\n.model n.1 NMOS ({model})\n.end\n"
            ),
        )?;
        let error = compile(&root, InputFormat::HspiceDeck).expect_err("unqualified MOS semantics");
        ensure!(
            format!("{error:#}").contains(expected),
            "{model} {instance}: {error:#}"
        );
    }
    Ok(())
}

#[test]
fn multiplier_and_expression_parameter_have_independent_semantics() -> Result<()> {
    for (option, expected) in [
        ("", -0.002),
        (".option XMULT_IN_EXP=NO", -0.002),
        (".option XMULT_IN_EXP=YES", -0.001),
    ] {
        let dir = tempfile::tempdir()?;
        let source = format!(
            "Independent M semantics\n{option}\nV1 a 0 1\nX1 a 0 cell M=2\n.subckt cell p n M=1\nR1 p n 'M*1k'\n.ends cell\n.end\n"
        );
        let text = numeric_deck(dir.path(), &source, "op\nwrdata op.dat i(v1)")?;
        let rows = values(dir.path(), "op.dat")?;
        let current = rows.first().context("missing operating point")?[1];
        ensure!(
            (current - expected).abs() < 1e-12,
            "{option}: {current} != {expected}\n{text}"
        );
    }
    Ok(())
}

#[test]
fn polynomial_controls_are_not_scalar_parameters() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let text = numeric_deck(
        dir.path(),
        r#"Structural controls
.option parhier=local
V1 ctrl 0 2
V2 ctrl2 0 3
X1 ctrl ctrl2 result cell
R1 result 0 1k
.subckt cell a b out a=17 b=19 coefficient=2
E1 out 0 VCVS POLY(2) a 0 b 0 1 coefficient 3 4 5 6
.ends cell
.end
"#,
        "op\nwrdata op.dat v(result)",
    )?;
    // 1 + 2*2 + 3*3 + 4*2^2 + 5*2*3 + 6*3^2
    let row = &values(dir.path(), "op.dat")?[0];
    ensure!((row[1] - 114.).abs() < 1e-10, "{row:?}\n{text}");
    Ok(())
}

#[test]
fn dc_sweep_target_must_be_an_independent_source() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("sweep.sp");
    fs::write(
        &root,
        "Parameter sweep\n.param vgain=1\nV1 a 0 1\nR1 a 0 1k\n.dc vgain 1 2 1\n.end\n",
    )?;
    let error = compile(&root, InputFormat::HspiceDeck).expect_err("parameter sweep");
    ensure!(
        format!("{error:#}").contains("not a root independent source"),
        "{error:#}"
    );
    Ok(())
}

#[test]
fn bsim4_initial_vector_preserves_all_three_startup_voltages() -> Result<()> {
    // HSPICE User Guide: Simulation and Analysis, B-2008.09, active
    // elements, specifies IC in external VDS, VGS, VBS order. Native
    // ngspice 47 interprets a parenthesized vector differently, so the
    // preparation stage must emit a comma vector of individual scalars.
    let reference = tempfile::tempdir()?;
    let circuit = r#"MOS vector initial condition
Vdd supply 0 1
Vg g 0 PULSE(0 1 2n 1n 1n 5n 20n)
R1 supply d 1k
C1 d 0 1p IC=0.15
M1 d g 0 0 selected L=1u W=2u IC=0.15,0.85,-0.05
.model selected NMOS LEVEL=14 VERSION=4.70 VTH0=0.4
.end
"#;
    let script = "tran 0.1n 12n uic\nwrdata tran.dat v(d) i(vdd)";
    let expected = circuit.replacen('\n', "\n.options TNOM=25\n.temp 25\n", 1);
    measure_native(reference.path(), &expected, script)?;
    let expected = values(reference.path(), "tran.dat")?;
    ensure!(expected.len() > 100, "missing reference trajectory");
    for vector in [
        "0.15,0.85,-0.05",
        "0.15 0.85 -0.05",
        "(0.15,0.85,-0.05)",
        "(0.15),(0.85),(-0.05)",
        "'vd0','vg0','vb0'",
        "('vd0','vg0','vb0')",
    ] {
        let dir = tempfile::tempdir()?;
        let source = circuit
            .replacen('\n', "\n.param vd0=0.15 vg0=0.85 vb0=-0.05\n", 1)
            .replace("LEVEL=14 VERSION=4.70", "LEVEL=54 VERSION=4.7")
            .replace("IC=0.15,0.85,-0.05", &format!("IC={vector}"));
        let text = numeric_deck(dir.path(), &source, script)?;
        let actual = values(dir.path(), "tran.dat")?;
        assert_eq!(actual.len(), expected.len(), "{vector}\n{text}");
        for (actual, expected) in actual.iter().zip(&expected) {
            assert_eq!(actual.len(), expected.len());
            for (&a, &e) in actual.iter().zip(expected) {
                ensure!(
                    (a - e).abs() <= e.abs() * 1e-9 + 1e-15,
                    "{vector}: {a:e} != {e:e}\n{text}"
                );
            }
        }
    }
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("root.cir");
    for vector in ["(0.15,0.85,-0.05,1)", "(0.15,,0.85)", "0.15,,0.85"] {
        let source = circuit
            .replace("LEVEL=14 VERSION=4.70", "LEVEL=54 VERSION=4.7")
            .replace("IC=0.15,0.85,-0.05", &format!("IC={vector}"));
        fs::write(&root, source)?;
        assert!(compile(&root, InputFormat::HspiceDeck).is_err(), "{vector}");
    }
    Ok(())
}

#[test]
fn hspice_parameter_functions_bind_to_each_occurrence() -> Result<()> {
    for (policy, expected) in [
        ("GLOBAL", [15., 12., 12., 12., 7.]),
        ("LOCAL", [15., 29., 41., 29., 16.]),
    ] {
        let dir = tempfile::tempdir()?;
        let text = numeric_deck(
            dir.path(),
            &format!(
                r#"Function occurrences
.option parhier={policy}
.param gain=3 f (x)= 'gain*x' g(x)='f(x)+1'
+ rootval='g(4)+2'
.param f(x)='gain*x+0'
Vroot root 0 'rootval'
X1 first fromcaller stage gain=7
X2 second unused stage gain=10
.subckt stage p q gain=5 actual='g(2)'
.param f(x)='gain*x+1'
.param shadow(gain)='gain*2'
V1 p 0 'g(4)+shadow(1)-3'
V2 q 0 'actual'
.ends stage
X3 third ignored stage gain=7
.op
.end
"#
            ),
            "op\nwrdata op.dat v(root) v(first) v(second) v(third) v(fromcaller)",
        )?;
        let rows = values(dir.path(), "op.dat")?;
        let row = rows.first().context("missing function OP data")?;
        assert_eq!(row.len(), 6);
        // g(4)+shadow(1)-3 = f(4), with root or occurrence f.
        for (&measured, target) in row[1..].iter().zip(expected) {
            ensure!(
                (measured - target).abs() < 1e-10,
                "{policy}: {measured} != {target}\n{text}"
            );
        }
    }
    Ok(())
}

#[test]
fn hspice_function_actuals_and_runtime_probes_preserve_caller_context() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let text = numeric_deck(
        dir.path(),
        r#"Function runtime
.option parhier=local
.param gain=2 f(x)='gain*x'
V1 in 0 dc 1 ac 1 PULSE(0 1 0 1n 1n 10m 20m)
Vmeas in p 0
R1 p 0 1k
X1 p out stage gain=4 av='f(3)'
.subckt stage a b gain=8 av=0
.param fun(x)='f(x)+av'
B1 b 0 V='fun(V(a))'
.ends stage
.op
.ac lin 2 1000 2000
.tran 10u 1m
.end
"#,
        "op\nwrdata op.dat v(out) v(p)\nac lin 2 1000 2000\nwrdata ac.dat v(out)\ntran 10u 1m 0 10u\nwrdata tran.dat v(out) v(p)",
    )?;
    let rows = values(dir.path(), "op.dat")?;
    ensure!(
        (rows[0][1] - (4. * rows[0][2] + 6.)).abs() < 1e-10,
        "{rows:?}\n{text}"
    );
    for row in values(dir.path(), "ac.dat")? {
        ensure!(
            (row[1] - 4.).abs() < 1e-10 && row[2].abs() < 1e-10,
            "{row:?}\n{text}"
        );
    }
    for row in values(dir.path(), "tran.dat")? {
        ensure!(
            (row[1] - (4. * row[2] + 6.)).abs() < 1e-8,
            "{row:?}\n{text}"
        );
    }
    Ok(())
}

#[test]
fn hspice_function_malformed_and_cyclic_declarations_are_rejected() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("functions.sp");
    for (source, expected) in [
        (".param f(x,x)='x'", "repeated function formal"),
        (".param f(x)='x'\nV1 a 0 'f(1,2)'", "expects 1 arguments"),
        (
            ".param f(x)='g(x)' g(x)='f(x)'",
            "recursive HSPICE function",
        ),
        (".param f(x)='f(x)'", "recursive HSPICE function"),
    ] {
        fs::write(&root, format!("Function errors\n{source}\n.end\n"))?;
        let error = compile(&root, InputFormat::HspiceDeck).expect_err("malformed function");
        assert!(format!("{error:#}").contains(expected), "{error:#}");
    }
    Ok(())
}

#[test]
fn hspice_function_geometry_and_model_parameters_match_explicit_native() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let reference = tempfile::tempdir()?;
    let script = "dc Vg 0.5 1.5 0.1\nwrdata dc.dat i(va) i(vb)";
    let text = numeric_deck(
        dir.path(),
        r#"Function-based bins
.option parhier=local
.param width(x)='x*1u' length(x)='x*1u'
.param edge=1u threshold(x)='x/10'
Vg g 0 1
Va a 0 0.8
Vb b 0 0.8
X1 a g cell ll=0.9
X2 b g cell ll=1.1
.subckt cell d g ll=1
M1 d g 0 0 n W='width(2)' L='length(ll)'
.model n.a NMOS LEVEL=54 VERSION=4.7 VTH0='threshold(3)' LMIN=0.5u LMAX=edge WMIN=1u WMAX=3u
.model n.b NMOS LEVEL=54 VERSION=4.7 VTH0='threshold(6)' LMIN=edge LMAX=2u WMIN=1u WMAX=3u
.ends cell
.end
"#,
        script,
    )?;
    measure_native(
        reference.path(),
        r#"Explicit bins
.options TNOM=25
.temp 25
Vg g 0 1
Va a 0 0.8
Vb b 0 0.8
Ma a g 0 0 low W=2u L=0.9u
Mb b g 0 0 high W=2u L=1.1u
.model low NMOS LEVEL=14 VERSION=4.70 VTH0=0.3
.model high NMOS LEVEL=14 VERSION=4.70 VTH0=0.6
.end
"#,
        script,
    )?;
    let actual = values(dir.path(), "dc.dat")?;
    let expected = values(reference.path(), "dc.dat")?;
    assert_eq!(actual.len(), expected.len());
    for (actual, expected) in actual.iter().zip(expected) {
        assert_eq!(actual.len(), expected.len());
        for (&a, e) in actual.iter().zip(expected) {
            ensure!(
                (a - e).abs() <= e.abs() * 1e-7 + 1e-13,
                "{a:e} != {e:e}\n{text}"
            );
        }
    }
    Ok(())
}

#[test]
fn hspice_function_linked_continuations_and_scope_isolation() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("root.sp");
    fs::write(
        &root,
        r#"Linked functions
.option parhier=local
.inc 'functions.lib'
V1 a 0 dc 1 ac 1
X1 a b cell
X2 a c other
.end
"#,
    )?;
    fs::write(
        dir.path().join("functions.lib"),
        r#".subckt cell a b
.param twice (x)='2*x'
+ gain=3
Vsense a n 0
R1 n 0 1k
.param probe(x)='twice(x)+gain*1000*I(Vsense)'
B1 b 0 V='probe (V(a))'
.ends cell
.subckt other a b
.param twice(x)='3*x'
B1 b 0 V='twice(V(a))'
.ends other
"#,
    )?;
    let bundle = compile(&root, InputFormat::HspiceDeck)?;
    let text = std::str::from_utf8(&bundle.primary.bytes)?;
    let Provenance::Native { lines, .. } = &bundle.primary.provenance else {
        anyhow::bail!("missing native source map")
    };
    assert_eq!(lines.len(), text.lines().count());
    assert!(
        text.lines()
            .zip(lines)
            .any(|(line, origin)| line.starts_with(".func ")
                && origin.source.ends_with("functions.lib")
                && (origin.line, origin.last_line) == (2, 3))
    );
    measure_native(
        dir.path(),
        text,
        "op\nwrdata op.dat v(b) v(c)\nac lin 2 1000 2000\nwrdata ac.dat v(b) v(c)",
    )?;
    let op = values(dir.path(), "op.dat")?;
    ensure!(
        (op[0][1] - 5.).abs() < 1e-10 && (op[0][2] - 3.).abs() < 1e-10,
        "{op:?}\n{text}"
    );
    for row in values(dir.path(), "ac.dat")? {
        ensure!(
            (row[1] - 5.).abs() < 1e-10
                && row[2].abs() < 1e-10
                && (row[3] - 3.).abs() < 1e-10
                && row[4].abs() < 1e-10,
            "{row:?}\n{text}"
        );
    }
    fs::write(
        &root,
        r#"Invisible function
.option parhier=local
.inc 'functions.lib'
.param outside(x)='twice(x)'
V1 a 0 'outside(2)'
.end
"#,
    )?;
    let error = compile(&root, InputFormat::HspiceDeck).expect_err("local function escapes scope");
    assert!(
        format!("{error:#}").contains("not visible in this occurrence"),
        "{error:#}"
    );
    Ok(())
}

#[test]
fn hspice_timing_options_are_bound_as_distinct_source_policies() -> Result<()> {
    use model_normalize_ngspice::model_card::parse_spice_constant;
    use ngspice_rs_preprocess::analysis_plan::{
        Directive, DirectiveOrigin,
        timing::{IntegrationCeiling, SavedOutput, StepControl},
    };
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("timing.sp");
    for (options, expected_output, legacy) in [
        ("", SavedOutput::AcceptedSteps, false),
        (
            ".option INTERP RMIN=1e-3 RMAX=2",
            SavedOutput::LinearInterpolation,
            false,
        ),
        (
            ".option INTERP=0 RUNLVL=0 RMIN=1e-3 RMAX=2",
            SavedOutput::AcceptedSteps,
            true,
        ),
        (
            ".option INTERP=1 RUNLVL=0",
            SavedOutput::LinearInterpolation,
            true,
        ),
    ] {
        fs::write(
            &root,
            format!(
                "Timing policies\nV1 a 0 1\nR1 a 0 1k\n.tran 1n 10n 2n 20n START=2n\n{options}\n.end\n"
            ),
        )?;
        let bundle = compile(&root, InputFormat::HspiceDeck)?;
        let native = std::str::from_utf8(&bundle.primary.bytes)?;
        for line in native
            .lines()
            .filter(|line| line.to_ascii_lowercase().starts_with(".options"))
        {
            for option in ["DELMAX", "INTERP", "RUNLVL", "RMIN", "RMAX"] {
                assert!(!line.to_ascii_uppercase().contains(option), "{native}");
            }
        }
        let plan = bundle.analysis_plan.context("missing timing plan")?;
        let Directive::ScheduledTransient { intent } = &plan.entries[0].directive else {
            anyhow::bail!("missing schedule");
        };
        let timing = intent.bind(parse_spice_constant)?;
        assert_eq!(timing.saved_output, expected_output);
        assert_eq!(timing.ceiling, IntegrationCeiling::Automatic);
        if options.contains("INTERP") {
            let option = intent.options.interp.as_ref().context("lost INTERP")?;
            assert!(
                matches!(&option.origin, DirectiveOrigin::Source {path, first_line: 5, last_line: 5} if path == &root)
            );
        }
        match timing.step_control {
            StepControl::RunLevel {
                level,
                soft_maximum,
            } => {
                assert!(!legacy);
                assert_eq!(level, 3);
                assert!((soft_maximum - 0.9e-9).abs() < 1e-22);
            }
            StepControl::Legacy(bounds) => {
                assert!(legacy);
                assert_eq!(bounds.len(), 2);
                for (segment, step) in bounds.iter().zip([1e-9, 2e-9]) {
                    let rmin = if options.contains("RMIN") { 1e-3 } else { 1e-9 };
                    let rmax = if options.contains("RMAX") { 2. } else { 5. };
                    assert!((segment.minimum / (step * rmin) - 1.).abs() < 1e-12);
                    assert!((segment.maximum / (step * rmax) - 1.).abs() < 1e-12);
                }
            }
        }
    }
    for (tran, options) in [
        (".tran 1n 10n", ".option INTERP=2"),
        (".tran 1n 10n", ".option RUNLVL=7"),
        (".tran 1n 10n", ".option RUNLVL=0 RMIN=2 RMAX=1"),
        (".tran 1n 10n 2n .5n", ".option DELMAX=.25n"),
    ] {
        fs::write(
            &root,
            format!("Invalid timing\nV1 a 0 1\nR1 a 0 1k\n{tran}\n{options}\n.end\n"),
        )?;
        assert!(
            compile(&root, InputFormat::HspiceDeck).is_err(),
            "{tran} {options}"
        );
    }
    Ok(())
}
