//! Independent measured charge and power witnesses replayed through native
//! ngspice, not through a second implementation of the translator's rules.
use anyhow::{Context, Result, ensure};
use ngspice_rs_preprocess::{Cancellation, InputFormat, Options, Request, SourceInput, prepare};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/runtime-expressions")
}

fn prepare_bundle(
    name: &str,
    power_target: ngspice_rs_preprocess::PowerTarget,
) -> Result<ngspice_rs_preprocess::PreparedBundle> {
    prepare(
        &Request {
            format: InputFormat::SimetrixDeck,
            input: SourceInput::File(fixtures().join(format!("{name}.net"))),
            libraries: Vec::new(),
            search_paths: Vec::new(),
        },
        &Options {
            power_target,
            ..Options::default()
        },
        &Cancellation::default(),
    )
}

fn prepare_deck(name: &str, dir: &Path, analysis: &str) -> Result<()> {
    let bundle = prepare_bundle(name, ngspice_rs_preprocess::PowerTarget::NativeNgspice)?;
    for companion in &bundle.companions {
        fs::write(dir.join(&companion.filename), &companion.bytes)?;
    }
    let mut deck = String::new();
    for line in std::str::from_utf8(&bundle.primary.bytes)?.lines() {
        let directive = line.split_whitespace().next().unwrap_or_default();
        if [".end", ".op", ".ac", ".tran", ".print"]
            .iter()
            .any(|d| directive.eq_ignore_ascii_case(d))
        {
            continue;
        }
        deck.push_str(line);
        deck.push('\n');
    }
    deck.push_str(".control\nset numdgt=17\nset wr_singlescale\n");
    deck.push_str(analysis);
    deck.push_str("\nquit\n.endc\n.end\n");
    fs::write(dir.join("test.cir"), deck)?;
    Ok(())
}

fn native(dir: &Path) -> Result<(bool, String)> {
    let execution = Command::new("ngspice")
        .current_dir(dir)
        .args(["-b", "-n", "test.cir"])
        .output()?;
    Ok((
        execution.status.success(),
        format!(
            "{}\n{}",
            String::from_utf8_lossy(&execution.stdout),
            String::from_utf8_lossy(&execution.stderr)
        ),
    ))
}

fn require_success(dir: &Path) -> Result<()> {
    let (success, log) = native(dir)?;
    let lower = log.to_ascii_lowercase();
    ensure!(
        success
            && !["error:", "fatal error", "aborted", "singular matrix"]
                .iter()
                .any(|s| lower.contains(s)),
        "{log}"
    );
    Ok(())
}

fn reference(name: &str) -> Result<(Vec<String>, Vec<Vec<f64>>)> {
    let csv = fs::read_to_string(fixtures().join(format!("{name}.csv")))?;
    let mut lines = csv.lines();
    let columns = lines
        .next()
        .context("missing CSV columns")?
        .split(',')
        .skip(1)
        .map(str::to_owned)
        .collect();
    let data = lines
        .map(|line| {
            line.split(',')
                .map(str::parse::<f64>)
                .collect::<std::result::Result<Vec<_>, _>>()
                .context("invalid CSV value")
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((columns, data))
}

fn trace(path: &Path) -> Result<Vec<Vec<f64>>> {
    fs::read_to_string(path)?
        .lines()
        .map(|line| {
            line.split_ascii_whitespace()
                .map(str::parse::<f64>)
                .collect::<std::result::Result<Vec<_>, _>>()
                .context("invalid native sample")
        })
        .collect()
}

fn compare(
    actual: &[Vec<f64>],
    expected: &[Vec<f64>],
    axis: bool,
    abs: f64,
    rel: f64,
) -> Result<()> {
    ensure!(
        actual.len() == expected.len(),
        "sample count {} != {}",
        actual.len(),
        expected.len()
    );
    for (index, (actual, expected)) in actual.iter().zip(expected).enumerate() {
        ensure!(
            actual.len() == expected.len(),
            "sample {index}: column count mismatch"
        );
        if axis {
            ensure!(
                (actual[0] - expected[0]).abs() <= 1e-9_f64.mul_add(expected[0].abs(), 1e-12),
                "sample {index}: axis {} != {}",
                actual[0],
                expected[0]
            );
        }
        for (column, (&actual, &expected)) in actual.iter().zip(expected).enumerate().skip(1) {
            ensure!(
                actual.is_finite() && (actual - expected).abs() <= rel.mul_add(expected.abs(), abs),
                "sample {index}, column {column}: {actual} != {expected}"
            );
        }
    }
    Ok(())
}

#[test]
fn absolute_power_values_and_complex_jacobians() -> Result<()> {
    for index in 0..4 {
        let name = format!("power-{index}");
        let dir = tempfile::tempdir()?;
        let (columns, op) = reference(&name)?;
        let (ac_columns, ac) = reference(&format!("{name}-ac"))?;
        let expected_ac_columns = columns
            .iter()
            .flat_map(|name| {
                let suffix = &name[1..];
                [format!("vr{suffix}"), format!("vi{suffix}")]
            })
            .collect::<Vec<_>>();
        assert_eq!(ac_columns, expected_ac_columns);
        let vectors = columns.join(" ");
        let complex = columns
            .iter()
            .map(|v| format!("real({v}) imag({v})"))
            .collect::<Vec<_>>()
            .join(" ");
        prepare_deck(
            &name,
            dir.path(),
            &format!(
                "op\nwrdata op.dat {vectors}\nac lin 1 1k 1k\nwrdata ac1.dat {complex}\nac lin 1 2k 2k\nwrdata ac2.dat {complex}"
            ),
        )?;
        require_success(dir.path()).with_context(|| name.clone())?;
        compare(&trace(&dir.path().join("op.dat"))?, &op, false, 1e-9, 1e-9)
            .with_context(|| name.clone())?;
        let mut actual_ac = trace(&dir.path().join("ac1.dat"))?;
        actual_ac.extend(trace(&dir.path().join("ac2.dat"))?);
        compare(&actual_ac, &ac, true, 1e-9, 1e-9).with_context(|| name.clone())?;
    }
    Ok(())
}

#[test]
fn negative_power_at_zero_remains_a_domain_failure() -> Result<()> {
    let dir = tempfile::tempdir()?;
    prepare_deck("zero-negative", dir.path(), "op\nwrdata op.dat v(out)")?;
    let (_, log) = native(dir.path())?;
    let lower = log.to_ascii_lowercase();
    ensure!(
        !dir.path().join("op.dat").exists() && lower.contains("out of range for"),
        "{log}"
    );
    Ok(())
}

#[test]
fn explicit_charge_grouping_preserves_transient_voltage() -> Result<()> {
    let (_, expected) = reference("charge-fine")?;
    for name in ["charge-parenthesis", "charge-braces", "charge-quoted"] {
        let dir = tempfile::tempdir()?;
        prepare_deck(
            name,
            dir.path(),
            "tran 100n 40u 0 0.5n\nlinearize v(out)\nwrdata wave.dat v(out)",
        )?;
        require_success(dir.path()).with_context(|| name.to_owned())?;
        compare(
            &trace(&dir.path().join("wave.dat"))?,
            &expected,
            true,
            1e-3,
            0.,
        )
        .with_context(|| name.to_owned())?;
    }
    Ok(())
}

#[test]
fn capacitor_temperature_coefficients_preserve_ac_response() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let (_, expected) = reference("cap-tc")?;
    prepare_deck(
        "cap-tc",
        dir.path(),
        "ac dec 10 1000 1000000\nwrdata wave.dat mag(v(out))",
    )?;
    require_success(dir.path())?;
    compare(
        &trace(&dir.path().join("wave.dat"))?,
        &expected,
        true,
        1e-9,
        0.,
    )
}

/// The prepared consumer is a separate repository. Run this explicit
/// cross-repository gate with the qualified `spice-rs` executable on PATH.
fn prepared_solver_deck(name: &str, dir: &Path, analysis: &str) -> Result<()> {
    use ngspice_rs_preprocess::PowerTarget;
    let bundle = prepare_bundle(name, PowerTarget::PreparedSolverV1)?;
    ensure!(bundle.power_target == PowerTarget::PreparedSolverV1);
    for companion in &bundle.companions {
        fs::write(dir.join(&companion.filename), &companion.bytes)?;
    }
    let mut deck = String::new();
    for line in std::str::from_utf8(&bundle.primary.bytes)?.lines() {
        let directive = line.split_whitespace().next().unwrap_or_default();
        if [".end", ".op", ".ac", ".tran", ".print"]
            .iter()
            .any(|d| directive.eq_ignore_ascii_case(d))
        {
            continue;
        }
        deck.push_str(line);
        deck.push('\n');
    }
    deck.push_str(analysis);
    deck.push_str("\n.end\n");
    fs::write(dir.join("test.cir"), deck)?;
    Ok(())
}

fn prepared_solver(dir: &Path) -> Result<std::process::Output> {
    Command::new("spice-rs")
        .current_dir(dir)
        .args([
            "simulate", "--format", "ascii", "--output", "test.raw", "test.cir",
        ])
        .output()
        .context("run the qualified prepared solver")
}

/// Each analysis deliberately emits one point. Resolve vector names from
/// the rawfile rather than assuming the consumer's variable ordering.
fn prepared_solver_trace(dir: &Path, columns: &[String]) -> Result<(Vec<Vec<f64>>, Vec<Vec<f64>>)> {
    let raw = fs::read_to_string(dir.join("test.raw"))?;
    let mut op = Vec::new();
    let mut ac = Vec::new();
    for plot in raw.split("Title:").filter(|p| !p.trim().is_empty()) {
        let (header, values) = plot.split_once("Values:\n").context("missing values")?;
        let (_, variables) = header
            .split_once("Variables:\n")
            .context("missing variables")?;
        let names = variables
            .lines()
            .map(|line| {
                line.split_whitespace()
                    .nth(1)
                    .context("missing vector name")
            })
            .collect::<Result<Vec<_>>>()?;
        let samples = values
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| line.split_whitespace().last().context("missing sample"))
            .collect::<Result<Vec<_>>>()?;
        ensure!(
            samples.len() == names.len(),
            "expected one sample per variable"
        );
        let complex = header.contains("Flags: complex");
        let lookup = |name: &str| -> Result<&str> {
            let index = names
                .iter()
                .position(|candidate| candidate.eq_ignore_ascii_case(name))
                .with_context(|| format!("missing {name}"))?;
            Ok(samples[index])
        };
        if complex {
            let (frequency, _) = lookup("frequency")?
                .split_once(',')
                .context("missing frequency parts")?;
            let mut row = vec![frequency.parse()?];
            for name in columns {
                let (real, imaginary) = lookup(name)?
                    .split_once(',')
                    .context("missing complex sample")?;
                row.extend([real.parse::<f64>()?, imaginary.parse::<f64>()?]);
            }
            ac.push(row);
        } else {
            ensure!(
                header.contains("Plotname: Operating Point"),
                "unexpected real plot"
            );
            let mut row = vec![0.];
            for name in columns {
                row.push(lookup(name)?.parse()?);
            }
            op.push(row);
        }
    }
    ac.sort_by(|a, b| a[0].total_cmp(&b[0]));
    Ok((op, ac))
}

#[test]
#[ignore = "cross-repository gate: requires qualified spice-rs on PATH"]
fn prepared_absolute_power_matches_measured_values_and_jacobians() -> Result<()> {
    for index in 0..4 {
        let name = format!("power-{index}");
        let dir = tempfile::tempdir()?;
        let (columns, op) = reference(&name)?;
        let (_, ac) = reference(&format!("{name}-ac"))?;
        prepared_solver_deck(&name, dir.path(), ".op\n.ac lin 1 1k 1k\n.ac lin 1 2k 2k")?;
        let execution = prepared_solver(dir.path())?;
        ensure!(
            execution.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&execution.stderr)
        );
        let (actual_op, actual_ac) = prepared_solver_trace(dir.path(), &columns)?;
        compare(&actual_op, &op, false, 1e-9, 1e-9).with_context(|| name.clone())?;
        compare(&actual_ac, &ac, true, 1e-9, 1e-9).with_context(|| name.clone())?;
    }
    let dir = tempfile::tempdir()?;
    prepared_solver_deck("zero-negative", dir.path(), ".op")?;
    let execution = prepared_solver(dir.path())?;
    ensure!(
        !execution.status.success() && !dir.path().join("test.raw").exists(),
        "standalone singular power unexpectedly succeeded"
    );
    Ok(())
}

#[test]
#[ignore = "cross-repository gate: requires qualified spice-rs on PATH"]
fn prepared_signed_exponent_and_zero_product_follow_source_semantics() -> Result<()> {
    let dir = tempfile::tempdir()?;
    prepared_solver_deck("prepared-signed", dir.path(), ".op\n.ac lin 1 1k 1k")?;
    let execution = prepared_solver(dir.path())?;
    ensure!(
        execution.status.success(),
        "{}",
        String::from_utf8_lossy(&execution.stderr)
    );
    let columns = [
        "caret",
        "realpart",
        "absolute",
        "masked",
        "caretzero",
        "abszero",
        "macro",
    ]
    .map(|name| format!("v({name})"));
    let (op, ac) = prepared_solver_trace(dir.path(), &columns)?;
    // Measured caret and POW exponent slopes for base -2, exponent 3;
    // absolute-base slope is 2^3 ln(2). The scalar/behavioural zero floor
    // is observable as a nonzero value, not just within an absolute tolerance.
    compare(
        &op,
        &[vec![0., -8., -8., 8., 0., 1e-154, 0., 8.]],
        false,
        1e-12,
        1e-9,
    )?;
    ensure!((op[0][5] / 1e-154 - 1.).abs() < 1e-9);
    compare(
        &ac,
        &[vec![
            1000.,
            1842.0680744,
            0.,
            -5.5451774445,
            0.,
            5.5451774445,
            0.,
            0.,
            0.,
            5e99,
            0.,
            0.,
            0.,
            5.5451774445,
            0.,
        ]],
        true,
        1e-12,
        1e-9,
    )?;
    Ok(())
}
