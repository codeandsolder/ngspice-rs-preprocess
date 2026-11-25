use anyhow::{Context, Result, bail, ensure};
use ngspice_rs_preprocess::{
    Cancellation, InputFormat, Options, Request, SourceInput,
    passive_ic::{
        Analysis, BiasConstraint, InitialConditions, Mode, PassiveKind, ResolvedDefaults,
        evaluate_expression,
    },
    prepare,
};
use std::{fs, path::Path};

fn fixtures() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/passive-ic")
}

fn prepare_fixture(name: &str) -> Result<ngspice_rs_preprocess::PreparedBundle> {
    prepare(
        &Request {
            format: InputFormat::SimetrixDeck,
            input: SourceInput::File(fixtures().join(format!("{name}.net"))),
            libraries: Vec::new(),
            search_paths: Vec::new(),
        },
        &Options::default(),
        &Cancellation::default(),
    )
}

fn isolated(expression: &str, analysis: Analysis) -> Result<f64> {
    evaluate_expression(expression, analysis, |name| bail!("unresolved {name}"))
}

fn bias_cap(constraint: BiasConstraint, supply: f64, resistance: f64) -> Result<f64> {
    match constraint {
        BiasConstraint::None => Ok(supply),
        BiasConstraint::CapacitorHard { voltage } => Ok(voltage),
        BiasConstraint::CapacitorSoft {
            voltage,
            resistance: series,
        } => Ok((supply * series + voltage * resistance) / (resistance + series)),
        _ => bail!("not a capacitor bias constraint"),
    }
}

fn bias_inductor(constraint: BiasConstraint, supply: f64, resistance: f64) -> Result<f64> {
    match constraint {
        BiasConstraint::None => Ok(supply / resistance),
        BiasConstraint::InductorHard { current } => Ok(current),
        BiasConstraint::InductorSoft {
            current,
            conductance,
        } => Ok((current + conductance * supply) / (1. + conductance * resistance)),
        _ => bail!("not an inductor bias constraint"),
    }
}

#[test]
fn original_mode_matrix_matches_measured_initial_equilibria() -> Result<()> {
    let analysis = Analysis::Transient { uic: false };
    for name in ["mode-0", "mode-1", "mode-2", "mode-default"] {
        let bundle = prepare_fixture(name)?;
        let policy = bundle
            .initial_conditions
            .context("missing source initialization policy")?;
        let defaults = policy.resolve_defaults(|value| isolated(value, analysis))?;
        let table = fs::read_to_string(fixtures().join(format!("{name}.csv")))?;
        let mut lines = table.lines();
        let headers = lines
            .next()
            .context("missing reference header")?
            .split(',')
            .collect::<Vec<_>>();
        let row = lines
            .next()
            .context("missing reference bias row")?
            .split(',')
            .map(str::parse::<f64>)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let voltage = |name: &str| -> Result<f64> {
            let index = headers
                .iter()
                .position(|header| header.eq_ignore_ascii_case(name))
                .context("missing reference voltage")?;
            Ok(row[index])
        };
        for suffix in ['a', 'b', 'c', 'd', 'e', 'f'] {
            for kind in [PassiveKind::Capacitor, PassiveKind::Inductor] {
                let name = format!(
                    "{}{suffix}",
                    if kind == PassiveKind::Capacitor {
                        "C"
                    } else {
                        "L"
                    }
                );
                let constraint = policy
                    .devices
                    .iter()
                    .find(|device| device.device.eq_ignore_ascii_case(&name))
                    .map(|device| {
                        device
                            .resolve(analysis, defaults, |value| isolated(value, analysis))
                            .map(|resolved| resolved.bias)
                    })
                    .transpose()?
                    .unwrap_or(BiasConstraint::None);
                let (actual, reference) = if kind == PassiveKind::Capacitor {
                    (
                        bias_cap(constraint, 10., 30.)?,
                        voltage(&format!("V(cp{suffix})"))? - voltage(&format!("V(cn{suffix})"))?,
                    )
                } else {
                    (
                        bias_inductor(constraint, 10., 10.)?,
                        (10. - voltage(&format!("V(lp{suffix})"))?) / 10.,
                    )
                };
                ensure!(
                    (actual - reference).abs() < 3e-8,
                    "{name} {actual} != {reference}"
                );
            }
        }
    }
    Ok(())
}

#[test]
fn analysis_lifetime_and_explicit_zero_are_distinct() -> Result<()> {
    let policy = prepare_fixture("analysis-tran")?
        .initial_conditions
        .context("missing policy")?;
    let c = policy
        .devices
        .iter()
        .find(|device| device.device.eq_ignore_ascii_case("C1"))
        .context("missing C1")?;
    let l = policy
        .devices
        .iter()
        .find(|device| device.device.eq_ignore_ascii_case("L1"))
        .context("missing L1")?;
    let defaults = policy.resolve_defaults(|value| isolated(value, Analysis::OperatingPoint))?;
    for (analysis, cap, ind) in [
        (
            Analysis::OperatingPoint,
            BiasConstraint::CapacitorSoft {
                voltage: 3.,
                resistance: 2.,
            },
            BiasConstraint::InductorSoft {
                current: 3.,
                conductance: 1e-9,
            },
        ),
        (
            Analysis::Ac,
            BiasConstraint::CapacitorSoft {
                voltage: 3.,
                resistance: 2.,
            },
            BiasConstraint::InductorSoft {
                current: 3.,
                conductance: 1e-9,
            },
        ),
        (
            Analysis::Transient { uic: false },
            BiasConstraint::CapacitorHard { voltage: 3. },
            BiasConstraint::InductorHard { current: 3. },
        ),
        (
            Analysis::DcSweep,
            BiasConstraint::None,
            BiasConstraint::None,
        ),
        (
            Analysis::Transient { uic: true },
            BiasConstraint::None,
            BiasConstraint::None,
        ),
    ] {
        let c = c.resolve(analysis, defaults, |value| isolated(value, analysis))?;
        let l = l.resolve(analysis, defaults, |value| isolated(value, analysis))?;
        assert_eq!(c.bias, cap);
        assert_eq!(l.bias, ind);
        assert_eq!(c.storage_ic, Some(3.));
        assert_eq!(l.storage_ic, Some(3.));
    }
    Ok(())
}

#[test]
fn node_targets_and_duplicate_order_survive_translation() -> Result<()> {
    let bundle = prepare_fixture("node-duplicate")?;
    let policy = bundle.initial_conditions.context("missing policy")?;
    let first = policy
        .nodes
        .iter()
        .find(|node| node.node.eq_ignore_ascii_case("p"))
        .context("missing node p")?;
    let defaults = policy.resolve_defaults(|value| isolated(value, Analysis::OperatingPoint))?;
    assert_eq!(
        first.resolve(Analysis::OperatingPoint, defaults, |value| isolated(
            value,
            Analysis::OperatingPoint
        ))?,
        (
            4.,
            BiasConstraint::NodeSoft {
                voltage: 4.,
                resistance: 2.
            }
        )
    );
    for analysis in [Analysis::DcSweep, Analysis::Transient { uic: true }] {
        assert_eq!(
            first.resolve(analysis, defaults, |value| isolated(value, analysis))?,
            (4., BiasConstraint::None)
        );
    }
    assert_eq!(
        policy
            .nodes
            .iter()
            .filter(|node| node.node.eq_ignore_ascii_case("p"))
            .count(),
        1
    );
    Ok(())
}

#[test]
fn all_oracle_decks_preserve_typed_declarations_and_manifest_roundtrip() -> Result<()> {
    let mut count = 0;
    for entry in fs::read_dir(fixtures())? {
        let path = entry?.path();
        if path.extension().is_none_or(|extension| extension != "net") {
            continue;
        }
        let name = path
            .file_stem()
            .and_then(|name| name.to_str())
            .context("invalid fixture name")?;
        let bundle = prepare_fixture(name).with_context(|| format!("preparing {name}"))?;
        let policy = bundle
            .initial_conditions
            .as_ref()
            .context("missing policy")?;
        let decoded: InitialConditions = serde_json::from_slice(&serde_json::to_vec(policy)?)?;
        assert_eq!(&decoded, policy);
        for device in &policy.devices {
            let artifact = bundle
                .artifacts()
                .find(|artifact| artifact.filename == device.artifact)
                .context("missing device artifact")?;
            let text = std::str::from_utf8(&artifact.bytes)?;
            ensure!(
                text.lines().any(|line| line
                    .split_whitespace()
                    .next()
                    .is_some_and(|name| name.eq_ignore_ascii_case(&device.device))),
                "missing {} in {}",
                device.device,
                device.artifact
            );
            ensure!(device.target.is_some() || device.mode.is_some() || device.branch.is_some());
        }
        let defaults = policy.resolve_defaults(|value| isolated(value, Analysis::OperatingPoint));
        if name == "icres-zero" {
            assert!(defaults.is_err());
        } else {
            defaults?;
        }
        count += 1;
    }
    assert_eq!(count, 38);
    Ok(())
}

#[test]
fn source_policy_preserves_native_path_and_validates_modes() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("source.net");
    let source = b"* test\nC1 a 0 1u IC=2\n.ic V(a)=1\n.end\n";
    fs::write(&path, source)?;
    let bundle = prepare(
        &Request {
            format: InputFormat::NativeDeck,
            input: SourceInput::File(path),
            libraries: Vec::new(),
            search_paths: Vec::new(),
        },
        &Options::default(),
        &Cancellation::default(),
    )?;
    assert!(bundle.initial_conditions.is_none());
    assert_eq!(bundle.primary.bytes, source);
    let defaults = ResolvedDefaults {
        mode: Mode::Ignore,
        resistance: 2.,
    };
    let mut policy = InitialConditions::default();
    let _ = ngspice_rs_preprocess::passive_ic::capture_statement(
        &mut policy,
        "deck.cir",
        &[],
        "C1 a 0 1u IC=2 ICMODE=3",
    )?;
    assert!(
        policy.devices[0]
            .resolve(Analysis::OperatingPoint, defaults, |value| isolated(
                value,
                Analysis::OperatingPoint
            ))
            .is_err()
    );
    Ok(())
}
