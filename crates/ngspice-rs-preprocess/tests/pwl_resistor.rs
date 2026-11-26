use anyhow::{Context, Result, ensure};
use ngspice_rs_preprocess::{
    Cancellation, InputFormat, Options, PreparedBundle, Request, SourceInput, prepare,
    pwl_resistor::{Axis, NativeBinding, PreparedPwlResistor, validate_artifact},
};
use std::{fs, path::Path};

const MODEL: &str = ".model MR VPWLR NSEG=4 X0=-1 Y0=-1 X1=0 Y1=2 X2=1 Y2=0 X3=2 Y3=2 X4=3 Y4=3";

fn compile(path: &Path, format: InputFormat, source: &str) -> Result<PreparedBundle> {
    fs::write(path, source)?;
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

// Independent load-line oracle for the published 1 A multiroot witness.
// The producer retains the hint; this is not a test of the native solver.
fn load_line(declaration: &PreparedPwlResistor) -> Result<f64> {
    let knots = &declaration.knots;
    let mut segment = declaration.initial_segment.context("missing segment")? - 1;
    for _ in 0..knots.len() {
        let a = knots[segment];
        let b = knots[segment + 1];
        let voltage =
            a.voltage + (1.0 - a.current) * (b.voltage - a.voltage) / (b.current - a.current);
        let actual = knots[1..knots.len() - 1].partition_point(|knot| knot.voltage <= voltage);
        if actual == segment {
            return Ok(voltage);
        }
        segment = actual;
    }
    anyhow::bail!("load line did not settle")
}

#[test]
fn multiroot_preferences_leave_physical_law_unchanged() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("multiroot.cir");
    let mut reference = None;
    for (hint, expected) in [(1, -1.0 / 3.0), (2, 0.5), (3, 1.5), (4, 1.5)] {
        let bundle = compile(
            &path,
            InputFormat::ExpandedDeck,
            &format!("* multiroot\nI1 0 p 1\n!R1 p 0 MR IC={hint}\n{MODEL}\n.end\n"),
        )?;
        let [resistor] = bundle.pwl_resistors.as_slice() else {
            anyhow::bail!("expected one resistor");
        };
        assert_eq!(resistor.initial_segment, Some(hint));
        assert_eq!(resistor.axis, Axis::Voltage);
        assert_eq!(resistor.device, "!R1");
        assert_eq!(resistor.scope, Vec::<String>::new());
        ensure!((load_line(resistor)? - expected).abs() < 1e-12);
        if let Some(bytes) = &reference {
            assert_eq!(&bundle.primary.bytes, bytes);
        } else {
            reference = Some(bundle.primary.bytes);
        }
    }
    Ok(())
}

#[test]
fn effective_init_override_is_validated() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("override.cir");
    let source = |hint: &str, override_line: &str| {
        format!("* override\n!R1 p n MR IC={hint}\n{MODEL}\n{override_line}\n.end\n")
    };
    let bundle = compile(
        &path,
        InputFormat::ExpandedDeck,
        &source("0", ".INIT !R1=3"),
    )?;
    assert_eq!(bundle.pwl_resistors[0].initial_segment, Some(3));
    for (hint, override_line) in [("0", ""), ("5", ""), ("1.5", ""), ("1", ".INIT !R1=5")] {
        assert!(
            compile(
                &path,
                InputFormat::ExpandedDeck,
                &source(hint, override_line)
            )
            .is_err()
        );
    }
    Ok(())
}

#[test]
fn current_defined_law_keeps_signed_coordinates_and_branch() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("current.cir");
    let bundle = compile(
        &path,
        InputFormat::ExpandedLibrary,
        "!R2 p n MC IC=2\n.model MC IPWLR NSEG=4 X0=-1 Y0=-1 X1=2 Y1=0 X2=0 Y2=1 X3=2 Y3=2 X4=3 Y4=3\n",
    )?;
    let [resistor] = bundle.pwl_resistors.as_slice() else {
        anyhow::bail!("expected one resistor");
    };
    assert_eq!(resistor.artifact, bundle.primary.filename);
    assert_eq!(resistor.axis, Axis::Current);
    assert_eq!(resistor.positive, "p");
    assert_eq!(resistor.negative, "n");
    assert_eq!(resistor.knots[1].voltage.to_bits(), 2.0f64.to_bits());
    assert_eq!(resistor.knots[1].current.to_bits(), 0.0f64.to_bits());
    let NativeBinding::VoltageLaw {
        sensor,
        source,
        intermediate,
    } = &resistor.binding
    else {
        anyhow::bail!("wrong binding");
    };
    let native = std::str::from_utf8(&bundle.primary.bytes)?;
    assert!(native.contains(&format!("{sensor} p {intermediate} 0")));
    assert!(native.contains(&format!("{source} {intermediate} n V=")));
    let encoded = serde_json::to_vec(&bundle.pwl_resistors)?;
    let decoded: Vec<PreparedPwlResistor> = serde_json::from_slice(&encoded)?;
    assert_eq!(decoded, bundle.pwl_resistors);
    let budget = model_preprocess_command::WorkBudget::default();
    validate_artifact(
        &decoded,
        &bundle.primary.filename,
        &bundle.primary.bytes,
        &budget,
    )?;
    assert!(
        validate_artifact(
            &decoded,
            &bundle.primary.filename,
            native
                .replace(&format!("{sensor} p"), &format!("{sensor} n"))
                .as_bytes(),
            &budget,
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn linked_occurrences_keep_local_initial_segments() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("linked.cir");
    let source = format!(
        "* linked\n.INIT X1.!R1=2\n.INIT X2.!R1=3\nX1 a b CHILD\nX2 b c CHILD\nX3 c d CHILD\nX4 d e CHILD\n.subckt CHILD p n\n!R1 p n MR IC=1\n{MODEL}\n.ends CHILD\n.end\n"
    );
    let bundle = compile(&path, InputFormat::SimplisDeck, &source)?;
    let root = std::str::from_utf8(&bundle.primary.bytes)?;
    for (instance, hint) in [("X1", 2), ("X2", 3), ("X3", 1), ("X4", 1)] {
        let target = root
            .lines()
            .find(|line| {
                line.split_whitespace()
                    .next()
                    .is_some_and(|name| name.eq_ignore_ascii_case(instance))
            })
            .and_then(|line| line.split_whitespace().last())
            .with_context(|| format!("missing {instance}: {root}"))?;
        let declaration = bundle
            .pwl_resistors
            .iter()
            .find(|decl| {
                decl.scope
                    .last()
                    .is_some_and(|name| name.eq_ignore_ascii_case(target))
            })
            .with_context(|| {
                format!(
                    "missing declaration for {target}: {:?}",
                    bundle.pwl_resistors
                )
            })?;
        assert_eq!(declaration.initial_segment, Some(hint));
        assert_eq!(declaration.positive, "p");
        assert_eq!(declaration.negative, "n");
    }
    assert_eq!(bundle.pwl_resistors.len(), 3);
    Ok(())
}

#[test]
fn allocated_branches_do_not_alias_distinct_resistors_or_source_nodes() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("collision.cir");
    let bundle = compile(
        &path,
        InputFormat::ExpandedDeck,
        "* collision\n!R$A p n MC IC=1\n!R_A q n MC IC=2\nB__R_A_IPWLR dummy 0 I=0\nV__R_A_IPWLR_SENSE t 0 0\nRkeep __R_A_IPWLR_MID 0 1k\nF1 out 0 !R$A 1\nF2 other 0 !R_A 1\n.model MC IPWLR NSEG=2 X0=0 Y0=0 X1=1 Y1=2 X2=2 Y2=3\n.end\n",
    )?;
    let [a, b] = bundle.pwl_resistors.as_slice() else {
        anyhow::bail!("expected two resistors");
    };
    assert_eq!(a.device, "!R$A");
    assert_eq!(b.device, "!R_A");
    assert_eq!(a.initial_segment, Some(1));
    assert_eq!(b.initial_segment, Some(2));
    let native = std::str::from_utf8(&bundle.primary.bytes)?;
    let mut names = std::collections::BTreeSet::from([
        "V__R_A_IPWLR_SENSE".to_owned(),
        "B__R_A_IPWLR".to_owned(),
        "__R_A_IPWLR_MID".to_owned(),
    ]);
    for (resistor, control) in [(a, "B__F1"), (b, "B__F2")] {
        let NativeBinding::VoltageLaw {
            sensor,
            source,
            intermediate,
        } = &resistor.binding
        else {
            anyhow::bail!("incorrect current-defined law binding");
        };
        for name in [sensor, source, intermediate] {
            assert!(
                names.insert(name.to_ascii_uppercase()),
                "{name} aliases another identifier"
            );
        }
        let card = native
            .lines()
            .find(|line| line.starts_with(control))
            .with_context(|| format!("missing {control}: {native}"))?;
        assert!(card.contains(&format!("i({sensor})")), "{card}");
    }
    Ok(())
}

#[test]
fn floating_resistor_physics_matches_independent_dc_laws() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("floating.cir");
    let bundle = compile(
        &path,
        InputFormat::ExpandedDeck,
        &format!(
            "* floating branch laws\nVREF n 0 5\nVDRIVE p n 0\nIFORCE n q 0\n!R1 p n MR IC=2\n!R2 q n MC IC=3\n{MODEL}\n.model MC IPWLR NSEG=4 X0=-1 Y0=-1 X1=2 Y1=0 X2=0 Y2=1 X3=2 Y3=2 X4=3 Y4=3\n.end\n"
        ),
    )?;
    let body = bundle
        .primary
        .bytes
        .strip_suffix(b".end\n")
        .context("missing deck terminator")?;
    let mut deck = body.to_vec();
    deck.extend_from_slice(b".control\nset numdgt=17\nset wr_singlescale\ndc VDRIVE -2 4 0.5\nwrdata voltage.dat i(VDRIVE)\ndc IFORCE -2 4 0.5\nwrdata current.dat v(q,n)\nquit\n.endc\n.end\n");
    fs::write(&path, deck)?;
    let run = std::process::Command::new("ngspice")
        .args(["-b", "-n", "floating.cir"])
        .current_dir(dir.path())
        .output()?;
    let log = format!(
        "{}\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    ensure!(
        run.status.success() && !log.to_ascii_lowercase().contains("error:"),
        "{log}"
    );
    for (file, sign) in [("voltage.dat", -1.0), ("current.dat", 1.0)] {
        let data = fs::read_to_string(dir.path().join(file))?;
        assert_eq!(data.lines().count(), 13);
        for line in data.lines() {
            let values = line
                .split_whitespace()
                .map(str::parse::<f64>)
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let [coordinate, result] = values.as_slice() else {
                anyhow::bail!("unexpected DC output {line}");
            };
            let x = *coordinate;
            let expected = if x <= 0.0 {
                3.0 * x + 2.0
            } else if x <= 1.0 {
                2.0 - 2.0 * x
            } else if x <= 2.0 {
                2.0 * (x - 1.0)
            } else {
                x
            };
            ensure!(
                (sign * result - expected).abs() < 1e-8,
                "{file}: {x} -> {result}, expected {expected}"
            );
        }
    }
    Ok(())
}
