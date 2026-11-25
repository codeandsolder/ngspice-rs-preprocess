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
