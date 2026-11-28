use anyhow::{Context, Result, ensure};
use ngspice_rs_preprocess::{
    Cancellation, InputFormat, Options, PreparedBundle, Request, SourceInput,
    analysis_plan::{
        AnalysisDialect, AnalysisKind, Directive, DirectiveOrigin, Disposition, PlanKind,
    },
    prepare,
};
use std::{fmt::Write as _, fs, path::Path, process::Command};

fn prepare_path(path: &Path, format: InputFormat) -> Result<PreparedBundle> {
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

#[test]
fn simetrix_title_named_section_and_continuation_origins() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("root.net");
    let included = dir.path().join("settings.lib");
    fs::write(&root, b".include this-is-the-title-not-a-path\n*#SIMETRIX\n.lib settings.lib selected\nV1 a 0 dc 1 ac 1\nR1 a b 1k\nC1 b 0 1u\n.tran 1u\n+ 4m 0 10u UIC\n.print tran V(b) I(V1)\n.end\n")?;
    fs::write(&included, b".lib unused\n.options missing_option=1\n.endl unused\n.lib selected\n.options RELTOL = 1e-5\n+ ABSTOL=1p TNOM=30\n.temp 30\n.ac lin 3 100 300\n.endl selected\n")?;
    let prepared = prepare_path(&root, InputFormat::SimetrixDeck)?;
    let plan = prepared.analysis_plan.as_ref().context("missing plan")?;
    assert_eq!(plan.kind, PlanKind::Deck);
    assert_eq!(plan.dialect, AnalysisDialect::Simetrix);
    assert_eq!(
        plan.title.as_deref(),
        Some(".include this-is-the-title-not-a-path")
    );
    plan.validate_supported()?;
    assert!(plan.requires_binding());
    let requests = plan.native_requests().collect::<Vec<_>>();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0].directive,
        Directive::Analysis {
            analysis: AnalysisKind::Ac,
            arguments: vec!["lin".into(), "3".into(), "100".into(), "300".into()],
            uic: false
        }
    );
    assert_eq!(
        requests[0].origin,
        DirectiveOrigin::Source {
            path: included.canonicalize()?,
            first_line: 8,
            last_line: 8
        }
    );
    assert_eq!(
        requests[1].directive,
        Directive::Analysis {
            analysis: AnalysisKind::Transient,
            arguments: vec!["1u".into(), "4m".into(), "0".into(), "10u".into()],
            uic: true
        }
    );
    assert_eq!(
        requests[1].origin,
        DirectiveOrigin::Source {
            path: root.canonicalize()?,
            first_line: 7,
            last_line: 8
        }
    );
    let settings = plan
        .entries
        .iter()
        .find(|entry| entry.command == ".OPTIONS")
        .context("missing options")?;
    assert_eq!(
        settings.origin,
        DirectiveOrigin::Source {
            path: included.canonicalize()?,
            first_line: 5,
            last_line: 6
        }
    );
    let Directive::Options { settings } = &settings.directive else {
        anyhow::bail!("missing typed options");
    };
    assert_eq!(settings.len(), 3);
    assert_eq!(settings[0].name, "RELTOL");
    assert_eq!(settings[0].value.as_deref(), Some("1e-5"));
    assert_eq!(settings[0].disposition, Disposition::Circuit);
    let deck = std::str::from_utf8(&prepared.primary.bytes)?;
    assert!(deck.contains("* .include this-is-the-title-not-a-path"));
    assert!(deck.contains(".options RELTOL=1e-5 ABSTOL=1p TNOM=30"));
    assert!(deck.contains(".temp 30"));
    assert!(!deck.contains("missing_option"));
    assert!(!deck.lines().any(|line| {
        [".tran", ".ac", ".print"]
            .iter()
            .any(|command| line.to_ascii_lowercase().starts_with(command))
    }));
    assert!(!deck.contains("ngspice-rs-preprocess-plan:"));
    Ok(())
}

#[test]
fn simplis_periodic_and_snapshot_semantics_are_not_native_spice_requests() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("simplis.net");
    fs::write(&root, b"V1 a 0 1\nR1 a 0 1k\n.TRAN 10u 2u\n.OPTIONS PSP_NPT=101 PSP_START=3u POP_USE_TRAN_SNAPSHOT\n.POP TRIG_GATE=G1 TRIG_COND=0_TO_1 MAX_PERIOD=1u\n.AC DEC 20 1k 1Meg\n.PRINT ALL\n.end\n")?;
    let prepared = prepare_path(&root, InputFormat::SimplisDeck)?;
    let plan = prepared.analysis_plan.as_ref().context("missing plan")?;
    assert_eq!(plan.dialect, AnalysisDialect::Simplis);
    assert_eq!(plan.title, None);
    assert_eq!(plan.native_requests().count(), 0);
    assert!(plan.requires_binding());
    assert_eq!(
        plan.entries[0].directive,
        Directive::Analysis {
            analysis: AnalysisKind::Transient,
            arguments: vec!["10u".into(), "2u".into()],
            uic: false
        }
    );
    assert_eq!(plan.entries[0].disposition, Disposition::RequiresBinding);
    assert_eq!(plan.entries[2].disposition, Disposition::Unsupported);
    assert_eq!(plan.entries[3].disposition, Disposition::Unsupported);
    let error = plan
        .validate_supported()
        .err()
        .context("unsupported POP accepted")?;
    assert!(error.to_string().contains(".POP"));
    assert!(error.to_string().contains("first_line: 5"));
    let deck = std::str::from_utf8(&prepared.primary.bytes)?;
    assert!(deck.contains("V1 a 0 1"));
    assert!(!deck.contains(".POP"));
    assert!(!deck.contains(".AC"));
    Ok(())
}

#[test]
fn conditional_analysis_tags_follow_actual_preprocessing_and_controls_stay_opaque() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("deck.net");
    fs::write(&root, b"V1 a 0 1\nR1 a 0 1k\n.VAR flag=1\n.IF flag==1\n.TRAN 10u 0\n.ELSE\n.POP MAX_PERIOD=1u\n.ENDIF\n.control\n.include does-not-exist\nlet x=1\n.endc\n.end\n")?;
    let prepared = prepare_path(&root, InputFormat::SimplisDeck)?;
    let plan = prepared.analysis_plan.as_ref().context("missing plan")?;
    assert_eq!(plan.entries.len(), 2);
    assert_eq!(plan.entries[0].command, ".TRAN");
    assert_eq!(plan.entries[1].command, ".CONTROL");
    assert_eq!(
        plan.entries[1].origin,
        DirectiveOrigin::Source {
            path: root.canonicalize()?,
            first_line: 9,
            last_line: 12
        }
    );
    assert!(
        plan.entries[1]
            .statement
            .contains(".include does-not-exist")
    );
    assert_eq!(plan.entries[1].disposition, Disposition::Unsupported);
    assert!(!std::str::from_utf8(&prepared.primary.bytes)?.contains("does-not-exist"));
    Ok(())
}

#[test]
fn unknown_physical_options_are_retained_and_not_applied() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("deck.net");
    fs::write(
        &root,
        b"ordinary title\nV1 a 0 1\nR1 a 0 1k\n.options UNQUALIFIEDOPTION RELTOL=1e-5\n.op\n.end\n",
    )?;
    let prepared = prepare_path(&root, InputFormat::SimetrixDeck)?;
    let plan = prepared.analysis_plan.as_ref().context("missing plan")?;
    let error = plan
        .validate_supported()
        .err()
        .context("unsupported setting accepted")?;
    assert!(error.to_string().contains("UNQUALIFIEDOPTION"));
    assert!(error.to_string().contains("first_line: 4"));
    let deck = std::str::from_utf8(&prepared.primary.bytes)?;
    assert!(deck.contains(".options RELTOL=1e-5"));
    assert!(!deck.contains("UNQUALIFIEDOPTION"));
    Ok(())
}

#[test]
fn original_deck_analysis_round_trip_matches_rc_oracle() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("deck.net");
    fs::write(&root, b"\nV1 a 0 dc 1 ac 1\nR1 a b 1k\nC1 b 0 1u\n.options RELTOL=1e-7 ABSTOL=1e-12\n.ac lin 3 100 300\n.tran 1u 2m 0 1u UIC\n.end\n")?;
    let prepared = prepare_path(&root, InputFormat::SimetrixDeck)?;
    let plan = prepared.analysis_plan.as_ref().context("missing plan")?;
    plan.validate_supported()?;
    assert_eq!(plan.title.as_deref(), Some(""));
    assert!(!plan.requires_binding());
    let mut circuit = std::str::from_utf8(&prepared.primary.bytes)?
        .strip_suffix(".end\n")
        .context("missing end")?
        .to_owned();
    circuit.push_str(".control\nset numdgt=17\nset wr_singlescale\n");
    for entry in plan.native_requests() {
        let Directive::Analysis { arguments, uic, .. } = &entry.directive else {
            anyhow::bail!("wrong request type");
        };
        writeln!(
            circuit,
            "{} {}{}",
            entry.command.trim_start_matches('.').to_ascii_lowercase(),
            arguments.join(" "),
            if *uic { " uic" } else { "" }
        )?;
        match entry.command.as_str() {
            ".AC" => circuit.push_str("let amplitude=mag(v(b))\nwrdata ac.dat amplitude\n"),
            ".TRAN" => circuit.push_str("wrdata tran.dat v(b)\n"),
            _ => anyhow::bail!("unexpected analysis"),
        }
    }
    circuit.push_str("quit\n.endc\n.end\n");
    fs::write(dir.path().join("prepared.cir"), circuit)?;
    let execution = Command::new("ngspice")
        .current_dir(dir.path())
        .args(["-b", "-n", "prepared.cir"])
        .output()?;
    let log = format!(
        "{}\n{}",
        String::from_utf8_lossy(&execution.stdout),
        String::from_utf8_lossy(&execution.stderr)
    );
    ensure!(
        execution.status.success() && !log.contains("Error:"),
        "{log}"
    );
    for line in fs::read_to_string(dir.path().join("ac.dat"))?.lines() {
        let numbers = line
            .split_whitespace()
            .map(str::parse::<f64>)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let [frequency, amplitude] = numbers.as_slice() else {
            anyhow::bail!("invalid AC data");
        };
        let expected = 1. / 1.0f64.hypot(std::f64::consts::TAU * frequency * 1e-3);
        ensure!(
            (amplitude - expected).abs() < 1e-10,
            "AC {frequency}: {amplitude} vs {expected}"
        );
    }
    let trace = fs::read_to_string(dir.path().join("tran.dat"))?;
    for line in trace.lines() {
        let numbers = line
            .split_whitespace()
            .map(str::parse::<f64>)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let [time, value] = numbers.as_slice() else {
            anyhow::bail!("invalid transient data");
        };
        let expected = -(-time / 1e-3).exp_m1();
        ensure!(
            (value - expected).abs() < 2e-6,
            "tran {time}: {value} vs {expected}"
        );
    }
    Ok(())
}

#[test]
fn expanded_library_has_no_independent_executable_analysis() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("cell.lib");
    fs::write(
        &path,
        b".subckt CELL a b\nR1 a b 1k\n.KEEP *I\n.ends CELL\n.op\n.end\n",
    )?;
    let prepared = prepare_path(&path, InputFormat::ExpandedLibrary)?;
    let plan = prepared.analysis_plan.as_ref().context("missing plan")?;
    assert_eq!(plan.kind, PlanKind::Library);
    assert_eq!(plan.native_requests().count(), 0);
    assert_eq!(plan.entries[0].scope, ["CELL"]);
    assert_eq!(plan.entries[0].disposition, Disposition::RequiresBinding);
    assert!(plan.validate_supported().is_err());
    let text = std::str::from_utf8(&prepared.primary.bytes)?;
    assert!(text.contains(".subckt CELL a b"));
    assert!(!text.contains(".op"));
    assert!(!text.ends_with(".end\n"));
    Ok(())
}

#[test]
fn simetrix_transient_defaults_are_explicit_and_symbolic_requests_require_binding() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("deck.net");
    fs::write(
        &root,
        b"title\nV1 a 0 1\nR1 a 0 1k\n.tran 50u\n.tran 0 100u 50u 0\n.tran 1u {stop}\n.end\n",
    )?;
    let prepared = prepare_path(&root, InputFormat::SimetrixDeck)?;
    let plan = prepared.analysis_plan.as_ref().context("missing plan")?;
    let requests = plan.native_requests().collect::<Vec<_>>();
    assert_eq!(requests.len(), 2);
    for request in requests {
        let Directive::Analysis { arguments, .. } = &request.directive else {
            anyhow::bail!("expected analysis");
        };
        assert_eq!(arguments.len(), 4);
        for index in [0, 3] {
            assert!((arguments[index].parse::<f64>()? - 1e-6).abs() < 1e-18);
        }
    }
    assert_eq!(plan.entries[2].disposition, Disposition::RequiresBinding);
    assert!(plan.validate_unbound_execution().is_err());
    Ok(())
}

#[test]
fn named_analysis_coordinates_are_not_mistaken_for_native_devices() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("deck.net");
    fs::write(&root, b"title\n*#SIMETRIX SEP=: \nV:drive a 0 1\nR1 a 0 1k\n.dc drive 0 2 1\n.noise V(a) drive dec 10 1 100\n.tf V(a) drive\n.op\n.end\n")?;
    let prepared = prepare_path(&root, InputFormat::SimetrixDeck)?;
    let plan = prepared.analysis_plan.as_ref().context("missing plan")?;
    assert_eq!(plan.entries.len(), 4);
    assert!(
        plan.entries[..3]
            .iter()
            .all(|entry| entry.disposition == Disposition::RequiresBinding)
    );
    assert_eq!(plan.native_requests().count(), 1);
    assert!(plan.validate_unbound_execution().is_err());
    assert!(
        prepared
            .device_identities
            .iter()
            .any(|identity| identity.displayed == "drive" && identity.linked != "drive")
    );
    Ok(())
}

#[test]
fn linked_ltspice_analysis_is_extracted_with_occurrence_and_bound_axes() -> Result<()> {
    use ngspice_rs_preprocess::{
        WorkBudget,
        analysis_plan::{
            measurement::MeasurementOperation,
            sweep::{SweepExecution, SweepTarget},
        },
    };
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("root.cir");
    let settings = dir.path().join("settings.inc");
    fs::write(
        &root,
        b"Native root\n.include settings.inc\nV1 in 0 1\nR1 in out {r}\nC1 out 0 1u\n.op\n.end\n",
    )?;
    fs::write(&settings, b"*#LTSPICE\n.param r=1k\n.step param r list 2k\n+ 1k 3k\n.step temp list 40 20\n.tran 0 {r*1u} 0 0 startup\n.meas tran vmax MAX V(out)\n.meas tran scaled PARAM vmax*2\n.dc V1 1 0 -.5\n")?;
    let prepared = prepare_path(&root, InputFormat::NativeDeck)?;
    let plan = prepared
        .analysis_plan
        .as_ref()
        .context("missing mixed analysis plan")?;
    assert_eq!(plan.dialect, AnalysisDialect::Native);
    assert_eq!(
        plan.entries
            .iter()
            .map(|entry| entry.command.as_str())
            .collect::<Vec<_>>(),
        [".STEP", ".STEP", ".TRAN", ".MEAS", ".MEAS", ".DC", ".OP"]
    );
    assert_eq!(
        plan.entries[0].origin,
        DirectiveOrigin::SourceOccurrence {
            path: settings.canonicalize()?,
            first_line: 3,
            last_line: 4,
            visit: 1,
            scope: 0,
        }
    );
    assert_eq!(plan.entries[6].dialect, AnalysisDialect::Native);
    assert!(
        matches!(&plan.entries[0].directive, Directive::Sweep { axis } if axis.target == SweepTarget::Parameter { name: "r".into() })
    );
    let Directive::DcSweep { axes } = &plan.entries[5].directive else {
        anyhow::bail!("missing continuation sweep");
    };
    assert_eq!(axes[0].execution, SweepExecution::Continuation);
    let Directive::Measurement { intent } = &plan.entries[4].directive else {
        anyhow::bail!("missing source measurement");
    };
    assert_eq!(intent.operation, MeasurementOperation::Parameter);
    assert_eq!(intent.expression, "vmax*2");
    let budget = WorkBudget::default();
    let sweeps = plan.bind_independent_sweeps(
        2,
        |_, _, raw| model_normalize_ngspice::model_card::parse_spice_constant(raw),
        &budget,
    )?;
    assert_eq!(sweeps.count(), 6);
    let coordinates = |ordinal| -> Result<Vec<u64>> {
        sweeps
            .candidate(ordinal)?
            .map(|result| result.map(|coordinate| coordinate.value.to_bits()))
            .collect()
    };
    assert_eq!(coordinates(0)?, [1000_f64, 40.].map(f64::to_bits));
    assert_eq!(coordinates(3)?, [1000_f64, 20.].map(f64::to_bits));
    assert!(plan.validate_unbound_execution().is_err());
    let electrical = std::str::from_utf8(&prepared.primary.bytes)?;
    assert!(electrical.contains("R1 in out {r}"));
    assert!(!electrical.lines().any(|line| matches!(
        line.split_whitespace().next(),
        Some(".step" | ".tran" | ".dc" | ".meas" | ".op")
    )));
    Ok(())
}

#[test]
fn native_deck_without_source_analysis_preserves_native_execution() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("root.cir");
    let source = b"title\nV1 a 0 1\nR1 a 0 1k\n.tran 1u 2m\n.print tran v(a)\n.end\n";
    fs::write(&root, source)?;
    let bundle = prepare_path(&root, InputFormat::NativeDeck)?;
    assert!(bundle.analysis_plan.is_none());
    assert_eq!(bundle.primary.bytes, source);
    Ok(())
}

fn native_rc_candidate(
    directory: &Path,
    name: &str,
    electrical: &[u8],
    resistance: f64,
    temperature: f64,
    timing: &[String],
    uic: bool,
    alter_parameter: bool,
) -> Result<f64> {
    let mut deck = std::str::from_utf8(electrical)?
        .strip_suffix(".end\n")
        .context("missing electrical deck terminator")?
        .to_owned();
    deck.push_str(".control\nset numdgt=17\nset wr_singlescale\n");
    if alter_parameter {
        writeln!(deck, "alterparam rbase={resistance}")?;
        deck.push_str("reset\n");
    }
    writeln!(deck, "option temp={temperature}")?;
    writeln!(
        deck,
        "tran {}{}",
        timing.join(" "),
        if uic { " UIC" } else { "" }
    )?;
    writeln!(deck, "wrdata {name}.dat v(out)\nquit\n.endc\n.end")?;
    let filename = format!("{name}.cir");
    fs::write(directory.join(&filename), deck)?;
    let result = Command::new("ngspice")
        .current_dir(directory)
        .args(["-n", "-b", &filename])
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
    let trace = fs::read_to_string(directory.join(format!("{name}.dat")))?;
    let mut last = None;
    for line in trace.lines() {
        let values = line
            .split_whitespace()
            .map(str::parse::<f64>)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let [time, voltage] = values.as_slice() else {
            anyhow::bail!("invalid RC trace")
        };
        let expected = -(-time / (resistance * 1e-6)).exp_m1();
        ensure!(
            (voltage - expected).abs() < 3e-6,
            "RC at {time}: {voltage} vs {expected}"
        );
        last = Some(*voltage);
    }
    last.context("empty RC trace")
}

#[test]
fn simetrix_nested_sweeps_bind_only_the_selected_analysis_and_match_native_cases() -> Result<()> {
    use ngspice_rs_preprocess::{WorkBudget, analysis_plan::sweep::SweepTarget};
    let directory = tempfile::tempdir()?;
    let root = directory.path().join("sweep.net");
    fs::write(&root, b"nested RC sweeps\n.param rbase=1k\nV1 in 0 1\nR1 in out {rbase}\nC1 out 0 1u\n.options RELTOL=1e-8 ABSTOL=1e-12\n.tran 1u 4m 0 5u UIC SWEEP PARAM rbase LIN 3 1k 3k\n+ SWEEP TEMP LIST 10 40 NUMCORES=2\n.ac lin 2 100 200\n.end\n")?;
    let bundle = prepare_path(&root, InputFormat::SimetrixDeck)?;
    let plan = bundle
        .analysis_plan
        .as_ref()
        .context("missing analysis plan")?;
    plan.validate_supported()?;
    let (transient_index, transient) = plan
        .entries
        .iter()
        .enumerate()
        .find(|(_, entry)| entry.command == ".TRAN")
        .context("missing transient")?;
    let ac_index = plan
        .entries
        .iter()
        .position(|entry| entry.command == ".AC")
        .context("missing AC")?;
    assert_eq!(transient.independent_axes.len(), 2);
    assert_eq!(transient.parallelism_hint, Some(2));
    assert_eq!(transient.disposition, Disposition::RequiresBinding);
    let Directive::Analysis { arguments, uic, .. } = &transient.directive else {
        anyhow::bail!("missing base transient")
    };
    assert!(*uic);
    let budget = WorkBudget::default();
    let resolve =
        |_: &ngspice_rs_preprocess::analysis_plan::PlanEntry,
         _: &ngspice_rs_preprocess::analysis_plan::sweep::SweepAxis,
         raw: &str| model_normalize_ngspice::model_card::parse_spice_constant(raw);
    let candidates = plan.bind_independent_sweeps(transient_index, resolve, &budget)?;
    assert_eq!(candidates.count(), 6);
    assert_eq!(
        plan.bind_independent_sweeps(ac_index, resolve, &budget)?
            .count(),
        1
    );
    for ordinal in 0..candidates.count() {
        let coordinates = candidates.candidate(ordinal)?.collect::<Result<Vec<_>>>()?;
        let [resistance, temperature] = coordinates.as_slice() else {
            anyhow::bail!("wrong axes")
        };
        assert_eq!(
            resistance.axis.target,
            SweepTarget::Parameter {
                name: "rbase".into()
            }
        );
        assert_eq!(temperature.axis.target, SweepTarget::Temperature);
        let r = resistance.value;
        let temp = temperature.value;
        let expected_r = [1000_f64, 2000., 3000.][ordinal % 3];
        let expected_temp = [10_f64, 40.][ordinal / 3];
        assert_eq!(r.to_bits(), expected_r.to_bits());
        assert_eq!(temp.to_bits(), expected_temp.to_bits());
        let actual = native_rc_candidate(
            directory.path(),
            &format!("prepared-{ordinal}"),
            &bundle.primary.bytes,
            r,
            temp,
            arguments,
            *uic,
            true,
        )?;
        let reference = format!(
            "reference\nV1 in 0 1\nR1 in out {r}\nC1 out 0 1u\n.options RELTOL=1e-8 ABSTOL=1e-12\n.end\n"
        );
        let expected = native_rc_candidate(
            directory.path(),
            &format!("reference-{ordinal}"),
            reference.as_bytes(),
            r,
            temp,
            arguments,
            *uic,
            false,
        )?;
        ensure!(
            (actual - expected).abs() < 3e-6,
            "candidate {ordinal}: {actual} vs {expected}"
        );
    }
    Ok(())
}

#[test]
fn simetrix_compatibility_step_and_continuation_are_not_interchangeable() -> Result<()> {
    use ngspice_rs_preprocess::{
        WorkBudget,
        analysis_plan::sweep::{SweepExecution, SweepTarget},
    };
    let directory = tempfile::tempdir()?;
    let root = directory.path().join("sweep.net");
    fs::write(&root, b"sweep continuation\nV1 in 0 1\nR1 in 0 1k\n.dc DEVICE R1 STP 1k 3k 1k\n+ SWEEP TEMP LIST 10 30\n.end\n")?;
    let bundle = prepare_path(&root, InputFormat::SimetrixDeck)?;
    let plan = bundle.analysis_plan.as_ref().context("missing DC plan")?;
    let Directive::DcSweep { axes } = &plan.entries[0].directive else {
        anyhow::bail!("missing continuation")
    };
    assert_eq!(axes[0].target, SweepTarget::Device { name: "R1".into() });
    assert_eq!(axes[0].execution, SweepExecution::Continuation);
    let budget = WorkBudget::default();
    assert_eq!(
        plan.bind_independent_sweeps(
            0,
            |_, _, raw| model_normalize_ngspice::model_card::parse_spice_constant(raw),
            &budget
        )?
        .count(),
        2
    );
    for (analysis, expected) in [
        (".tran 1m", true),
        (".op", false),
        (".tran 1m\n.ac lin 1 1k 1k", false),
        (".tran 1m SWEEP TEMP LIST 20 40", false),
    ] {
        fs::write(
            &root,
            format!(
                "step companion\n.param rbase=1k\nV1 in 0 1\nR1 in 0 {{rbase}}\n.step param rbase 1k 3k 1k\n{analysis}\n.end\n"
            ),
        )?;
        let bundle = prepare_path(&root, InputFormat::SimetrixDeck)?;
        let plan = bundle
            .analysis_plan
            .as_ref()
            .context("missing compatibility plan")?;
        assert_eq!(plan.validate_supported().is_ok(), expected, "{analysis}");
    }
    Ok(())
}

#[test]
fn raw_file_options_share_source_fields_with_analysis_options() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("root.cir");
    fs::write(&root, b"Option ownership\n.param pw=0\nV1 x 0 1\nR1 x 0 1k\n.options MAXSTEP=1u,RELTOL=1e-5\n+ plotwinsize pw\n+ SSTOL=1m,\n+ numdgt={7+8}\n.tran 1m\n.end\n")?;
    let bundle = prepare_path(&root, InputFormat::LtspiceDeck)?;
    let plan = bundle
        .analysis_plan
        .as_ref()
        .context("missing analysis plan")?;
    let Directive::Options { settings } = &plan.entries[0].directive else {
        anyhow::bail!("missing option settings");
    };
    assert_eq!(
        settings
            .iter()
            .map(|setting| (&*setting.name, setting.disposition))
            .collect::<Vec<_>>(),
        [
            ("MAXSTEP", Disposition::RequiresBinding),
            ("RELTOL", Disposition::Circuit),
            ("PLOTWINSIZE", Disposition::Circuit),
            ("SSTOL", Disposition::RequiresBinding),
            ("NUMDGT", Disposition::Circuit),
        ]
    );
    let text = std::str::from_utf8(&bundle.primary.bytes)?;
    let ngspice_rs_preprocess::Provenance::Native { lines, .. } = &bundle.primary.provenance else {
        anyhow::bail!("missing source provenance");
    };
    for (marker, line) in [
        ("ltspice_output_plotwinsize=", 6),
        ("ltspice_output_numdgt=", 8),
    ] {
        let origin = text
            .lines()
            .zip(lines)
            .find_map(|(card, origin)| card.contains(marker).then_some(origin))
            .context("missing output projection")?;
        assert_eq!(origin.line, line);
        assert_eq!(origin.source, root.canonicalize()?);
    }
    assert!(text.contains(".options RELTOL=1e-5"), "{text}");
    assert!(
        !text
            .lines()
            .filter(|line| !line.starts_with('*'))
            .any(|line| line.contains("MAXSTEP") || line.contains("SSTOL")),
        "{text}"
    );
    Ok(())
}

#[test]
fn pspice_capture_binds_independent_and_continuation_axes_separately() -> Result<()> {
    use ngspice_rs_preprocess::{
        WorkBudget,
        analysis_plan::sweep::{SweepExecution, SweepTarget},
    };
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("root.cir");
    fs::write(&root, b"PSpice numerical analysis\n.param rbase=1k other=1\nV1 in 0 1\nR1 in 0 {rbase}\n.temp 65\n.step PARAM rbase 3k,1k,1k\n.dc V1 LIST .1,.2 PARAM other 1,3,1\n.tran/OP 1u 4m 0 {SCHEDULE(0,5u,1m,10u)} SKIPBP\n.probe/CSDF V(in)\n.end\n")?;
    let bundle = prepare_path(&root, InputFormat::PspiceDeck)?;
    let plan = bundle
        .analysis_plan
        .as_ref()
        .context("missing PSpice plan")?;
    plan.validate_supported()?;
    assert!(plan.requires_binding());
    assert_eq!(plan.dialect, AnalysisDialect::Pspice);
    assert_eq!(
        plan.entries
            .iter()
            .map(|entry| entry.command.as_str())
            .collect::<Vec<_>>(),
        [".TEMP", ".STEP", ".DC", ".TRAN/OP", ".PROBE/CSDF"]
    );
    let Directive::DcSweep { axes } = &plan.entries[2].directive else {
        anyhow::bail!("missing DC continuation axes")
    };
    assert_eq!(axes.len(), 2);
    assert!(
        axes.iter()
            .all(|axis| axis.execution == SweepExecution::Continuation)
    );
    assert_eq!(axes[0].target, SweepTarget::Source { name: "V1".into() });
    assert_eq!(
        axes[1].target,
        SweepTarget::Parameter {
            name: "other".into()
        }
    );
    let Directive::Analysis { arguments, uic, .. } = &plan.entries[3].directive else {
        anyhow::bail!("missing transient request")
    };
    assert!(*uic);
    assert_eq!(arguments, &["1u", "4m", "0", "{SCHEDULE(0,5u,1m,10u)}"]);
    assert!(plan.entries[3].statement.ends_with("SKIPBP"));
    let budget = WorkBudget::default();
    for index in [2, 3] {
        let bound = plan.bind_independent_sweeps(
            index,
            |_, _, raw| model_normalize_ngspice::model_card::parse_spice_constant(raw),
            &budget,
        )?;
        assert_eq!(bound.count(), 3);
        for (ordinal, expected) in [3000_f64, 2000., 1000.].into_iter().enumerate() {
            let coordinates = bound.candidate(ordinal)?.collect::<Result<Vec<_>>>()?;
            assert_eq!(coordinates.len(), 1);
            assert_eq!(coordinates[0].value.to_bits(), expected.to_bits());
        }
    }
    let text = std::str::from_utf8(&bundle.primary.bytes)?;
    assert!(text.contains(".temp 65"), "{text}");
    assert!(
        !text.lines().any(|line| [".step", ".dc", ".tran", ".probe"]
            .iter()
            .any(|prefix| line.to_ascii_lowercase().starts_with(prefix))),
        "{text}"
    );
    Ok(())
}

#[test]
fn pspice_digital_options_are_bound_not_emitted_as_native_options() -> Result<()> {
    use model_normalize_ngspice::pspice::{
        digital_corners::DelaySelection, digital_io::InterfaceLevel, digital_options::InitialState,
    };
    use ngspice_rs_preprocess::WorkBudget;
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("root.cir");
    fs::write(&root, b"PSpice digital policy\nV1 in 0 1\nR1 in 0 1k\n.options DIGMNTYMX={unused} DIGIOLVL=3\n+ DIGINITSTATE=0 DIGFREQ=1G DIGERRDEFAULT=0 GMIN=1e-10\n.options DIGMNTYMX=4 DIGERRLIMIT=100\n.end\n")?;
    let bundle = prepare_path(&root, InputFormat::PspiceDeck)?;
    let plan = bundle
        .analysis_plan
        .as_ref()
        .context("missing digital option plan")?;
    plan.validate_supported()?;
    assert!(plan.requires_binding());
    let bound = plan.bind_pspice_digital_options(
        |_, raw| model_normalize_ngspice::model_card::parse_spice_constant(raw),
        &WorkBudget::default(),
    )?;
    assert_eq!(bound.delay_selection, DelaySelection::WorstCase);
    assert_eq!(bound.interface_level, InterfaceLevel::Three);
    assert_eq!(bound.initial_state, InitialState::Clear);
    assert_eq!(bound.frequency.to_bits(), 1e9_f64.to_bits());
    assert_eq!(bound.constraint_error_limit, None);
    assert_eq!(
        bound.total_error_limit.map(f64::to_bits),
        Some(100_f64.to_bits())
    );
    let text = std::str::from_utf8(&bundle.primary.bytes)?;
    let active = text
        .lines()
        .filter(|line| !line.trim_start().starts_with('*'))
        .collect::<Vec<_>>()
        .join("\n")
        .to_ascii_uppercase();
    assert!(active.contains("GMIN=1E-10"), "{text}");
    assert!(!active.contains("DIGMNTYMX"), "{text}");
    assert!(!active.contains("DIGIOLVL"), "{text}");
    assert!(!active.contains("DIGINITSTATE"), "{text}");
    assert!(!active.contains("DIGFREQ"), "{text}");
    assert!(!active.contains("DIGERR"), "{text}");
    fs::write(
        &root,
        b"Unknown digital policy\n.options DIGUNQUALIFIED=1\n.end\n",
    )?;
    assert!(prepare_path(&root, InputFormat::PspiceDeck).is_err());
    Ok(())
}

#[test]
fn pspice_temperature_and_statistical_policies_are_not_interchangeable() -> Result<()> {
    use ngspice_rs_preprocess::WorkBudget;
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("root.cir");
    for (directives, supported) in [
        (".temp 65,-40,25\n.op", true),
        (".temp 65,-40\n.step param r 1 2 1\n.op", false),
        (".temp 65\n.step temp 25 50 25\n.op", false),
        (".step param r 1 2 1\n.dc PARAM R 1 2 1", false),
        (".dc V1 1 2 1 v1 2 3 1", false),
    ] {
        fs::write(
            &root,
            format!("PSpice policy\n.param r=1\nV1 in 0 1\nR1 in 0 1k\n{directives}\n.end\n"),
        )?;
        let bundle = prepare_path(&root, InputFormat::PspiceDeck)?;
        let plan = bundle
            .analysis_plan
            .as_ref()
            .context("missing analysis plan")?;
        assert_eq!(plan.validate_supported().is_ok(), supported, "{directives}");
        if supported {
            let budget = WorkBudget::default();
            let bound = plan.bind_independent_sweeps(
                1,
                |_, _, raw| model_normalize_ngspice::model_card::parse_spice_constant(raw),
                &budget,
            )?;
            assert_eq!(bound.count(), 3);
            let coordinates = bound.candidate(1)?.collect::<Result<Vec<_>>>()?;
            assert_eq!(coordinates[0].value.to_bits(), (-40_f64).to_bits());
        }
    }
    for statistical in [
        ".mc 10 dc V(in) YMAX",
        ".WCASE DC V(in) YMAX",
        ".distribution test 1 2",
    ] {
        fs::write(
            &root,
            format!("PSpice statistical\nV1 in 0 1\nR1 in 0 1k\n{statistical}\n.end\n"),
        )?;
        let error = prepare_path(&root, InputFormat::PspiceDeck)
            .expect_err("statistical request must not be reduced to nominal execution");
        assert!(
            format!("{error:#}").contains("realization policy"),
            "{error:#}"
        );
    }
    Ok(())
}
