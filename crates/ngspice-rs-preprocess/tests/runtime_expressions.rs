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

fn prepare_deck(name: &str, dir: &Path, analysis: &str) -> Result<()> {
    let bundle = prepare(
        &Request {
            format: InputFormat::SimetrixDeck,
            input: SourceInput::File(fixtures().join(format!("{name}.net"))),
            libraries: Vec::new(),
            search_paths: Vec::new(),
        },
        &Options::default(),
        &Cancellation::default(),
    )?;
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
