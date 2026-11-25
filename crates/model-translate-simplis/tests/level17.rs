//! Independent authored source witnesses measured with SIMetrix Elements
//! 9.40d and replayed through the prepared native circuit.
use anyhow::{Context, Result, ensure};
use model_translate_simplis::translate_simplis_deck;
use std::{fmt::Write as _, fs, process::Command};

fn run(source: &str, analysis: &str, vectors: &str) -> Result<Vec<Vec<f64>>> {
    let translated = translate_simplis_deck(source.as_bytes())?;
    let dir = tempfile::tempdir()?;
    let native = String::from_utf8(translated.bytes)?;
    let body = native
        .trim_end()
        .strip_suffix(".end")
        .context("native deck end")?;
    let deck = format!(
        "{body}\n.options reltol=1e-7 abstol=1e-13 vntol=1e-8\n.control\nset wr_singlescale\nset numdgt=16\n{analysis}\nwrdata trace.tsv {vectors}\nquit\n.endc\n.end\n"
    );
    fs::write(dir.path().join("case.cir"), &deck)?;
    let result = Command::new("ngspice")
        .args(["-b", "case.cir"])
        .current_dir(dir.path())
        .output()?;
    ensure!(
        result.status.success(),
        "native ngspice failed: {}\n{}\n{deck}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let text = fs::read_to_string(dir.path().join("trace.tsv")).with_context(|| {
        format!(
            "missing native vectors: {}\n{}\n{deck}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        )
    })?;
    rows(&text)
}

fn rows(text: &str) -> Result<Vec<Vec<f64>>> {
    text.lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
        .map(|line| {
            line.split_whitespace()
                .map(|value| {
                    value
                        .parse()
                        .with_context(|| format!("invalid vector {value}"))
                })
                .collect()
        })
        .collect()
}

fn compare(
    actual: &[Vec<f64>],
    expected_rows: &[Vec<f64>],
    relative: f64,
    absolute: f64,
) -> Result<()> {
    let mut failures = Vec::new();
    for expected in expected_rows {
        let x = expected[0];
        let hi = actual.partition_point(|row| row[0] < x);
        let a = &actual[hi.saturating_sub(1).min(actual.len() - 1)];
        let b = &actual[hi.min(actual.len() - 1)];
        let fraction = if b[0] > a[0] {
            (x - a[0]) / (b[0] - a[0])
        } else {
            0.
        };
        ensure!(
            fraction >= -1e-6 && fraction <= 1.000_001,
            "reference outside native vector"
        );
        ensure!(
            a.len() == expected.len() && b.len() == expected.len(),
            "unexpected vector width"
        );
        for i in 1..expected.len() {
            let got = fraction.mul_add(b[i] - a[i], a[i]);
            let want = expected[i];
            let error = (got - want).abs();
            if error > relative.mul_add(want.abs(), absolute) {
                failures.push(format!(
                    "x={x} vector={i}: native={got:.12e} vendor={want:.12e} error={error:.6e}"
                ));
            }
        }
    }
    ensure!(failures.is_empty(), "{}", failures.join("\n"));
    Ok(())
}

#[test]
fn level17_gate_drain_charge_matches_independent_ramp() -> Result<()> {
    let mut deck = String::from("* Level 17 gate charge witness\n");
    let cases = [
        ("NMOS", "CGDMIN=2n CGDMAX=0 XG1CGD=.3 XG2CGD=.8 VTCGD=1", ""),
        ("NMOS", "CGDMIN=0 CGDMAX=3n XG1CGD=.3 XG2CGD=.8 VTCGD=1", ""),
        (
            "NMOS",
            "CGDMIN=2n CGDMAX=3n XG1CGD=.3 XG2CGD=.8 VTCGD=1",
            "",
        ),
        (
            "PMOS",
            "CGDMIN=2n CGDMAX=3n XG1CGD=.3 XG2CGD=.8 VTCGD=1",
            "",
        ),
        (
            "NMOS",
            "CGDMIN=2n CGDMAX=3n XG1CGD=.3 XG2CGD=.8 VTCGD=1",
            "M=3",
        ),
        ("NMOS", "CGDMIN=2n CGDMAX=0 XG1CGD=0 XG2CGD=.8 VTCGD=1", ""),
        ("NMOS", "", ""),
    ];
    for (i, (family, fields, instance)) in cases.into_iter().enumerate() {
        writeln!(
            deck,
            "Vg{i} g{i} 0 PWL(0 -10 20u 10)\nM{i} 0 g{i} 0 0 Q{i} {instance}\n.model Q{i} {family} (LEVEL=17 {fields})"
        )?;
    }
    deck.push_str(".end\n");
    let actual = run(
        &deck,
        "tran 100n 20u 0 5n",
        "i(Vg0) i(Vg1) i(Vg2) i(Vg3) i(Vg4) i(Vg5) i(Vg6)",
    )?;
    compare(
        &actual,
        &rows(include_str!("fixtures/level17-cgd.tsv"))?,
        3e-4,
        1e-9,
    )
}

#[test]
fn level17_dc_temperature_and_multiplicity_match_independent_sweep() -> Result<()> {
    let mut deck = String::from(
        "* Level 17 DC temperature witness\nVsw x 0 0\nBgn gn 0 V=5\nBgp gp 0 V=-5\nBdp dp 0 V=-V(x)\n",
    );
    let fields = "VTO=2 KP=.1 RD=.5 RS=.2 TC1RD=.01 TC2RD=.0001 TNOM=25";
    let pfields = "VTO=-2 KP=.1 RD=.5 RS=.2 TC1RD=.01 TC2RD=.0001 TNOM=25";
    for (i, (family, params, instance)) in [
        ("NMOS", fields, ""),
        ("NMOS", fields, "M=3"),
        ("PMOS", pfields, ""),
        ("NMOS", fields, "TEMP=75"),
        ("NMOS", "", ""),
        ("NMOS", fields, "TEMP=25"),
        ("NMOS", fields, "M=.5 TEMP=75"),
    ]
    .into_iter()
    .enumerate()
    {
        let (drain, gate) = if family == "PMOS" {
            ("dp", "gp")
        } else {
            ("x", "gn")
        };
        writeln!(
            deck,
            "Vs{i} {drain} d{i} 0\nM{i} d{i} {gate} 0 0 Q{i} {instance}\n.model Q{i} {family} (LEVEL=17 {params})"
        )?;
    }
    deck.push_str(".end\n");
    let actual = run(
        &deck,
        "dc Vsw -1 5 .25",
        "i(Vs0) i(Vs1) i(Vs2) i(Vs3) i(Vs4) i(Vs5) i(Vs6)",
    )?;
    let mut reference = rows(include_str!("fixtures/level17-dc.tsv"))?;
    // Q4 has no series resistance, so its forward body-diode voltage
    // is exactly -Vsw. Convert only the independent ideal diode term
    // using the documented physical constants of each simulator.
    let source_vt: f64 = 300.15 * (1.380_622_6e-23 / 1.602_191_8e-19);
    let native_vt: f64 = 300.15 * (1.380_648_52e-23 / 1.602_176_620_8e-19);
    for row in &mut reference {
        let voltage = -row[0];
        let delta = 1e-14 * ((voltage / source_vt).exp_m1() - (voltage / native_vt).exp_m1());
        row[5] += delta;
    }
    compare(&actual, &reference, 1e-3, 1e-9)
}

#[test]
fn level17_absolute_junction_capacitance_matches_independent_ramp() -> Result<()> {
    let mut deck = String::from("* Level 17 reverse junction charge witness\n");
    for (i, params) in [
        "CBD=2n",
        "CJ=2n",
        "CJSW=2n",
        "CBD=3n CJ=2n CJSW=1n",
        "CBS=2n",
        "CJ=2n CJSW=1n",
        "CBD=2n PB=1.5 MJ=.3",
    ]
    .into_iter()
    .enumerate()
    {
        writeln!(
            deck,
            "Vd{i} d{i} 0 PWL(0 0 10u 5)\nVs{i} s{i} 0 PWL(0 0 10u 5)\nM{i} d{i} 0 s{i} 0 Q{i}\n.model Q{i} NMOS (LEVEL=17 VTO=100 IS=1e-30 {params})"
        )?;
    }
    deck.push_str(".end\n");
    let vectors = (0..7)
        .map(|i| format!("i(Vd{i}) i(Vs{i})"))
        .collect::<Vec<_>>()
        .join(" ");
    let actual = run(&deck, "tran 100n 10u 0 5n", &vectors)?;
    compare(
        &actual,
        &rows(include_str!("fixtures/level17-junction.tsv"))?,
        5e-4,
        2e-9,
    )
}
