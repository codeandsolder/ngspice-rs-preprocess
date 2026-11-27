//! Independent authored source witnesses measured with `SIMetrix` Elements
//! 9.40d and replayed through the prepared native circuit.
use anyhow::{Context, Result, ensure};
use model_preprocess_command::WorkBudget;
use model_translate_simplis::{translate_simplis_deck, translate_simplis_linked_netlist};
use std::{fmt::Write as _, fs, process::Command};

fn run(source: &str, analysis: &str, vectors: &str) -> Result<Vec<Vec<f64>>> {
    let translated = translate_simplis_deck(source.as_bytes())?;
    run_native(&translated.bytes, &[], analysis, vectors)
}

fn run_native(
    native: &[u8],
    companions: &[(&str, &[u8])],
    analysis: &str,
    vectors: &str,
) -> Result<Vec<Vec<f64>>> {
    let dir = tempfile::tempdir()?;
    for &(name, bytes) in companions {
        fs::write(dir.path().join(name), bytes)?;
    }
    let native = std::str::from_utf8(native)?;
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
            (-1e-6..=1.000_001).contains(&fraction),
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

#[test]
fn level17_ac_jacobian_and_terminal_charge_conservation() -> Result<()> {
    let mut deck = String::from("* Level 17 AC witness\n");
    let nonlinear = "CGDMIN=2n CGDMAX=3n XG1CGD=.3 XG2CGD=.8 VTCGD=1";
    let overlap = "CGSO=2n CGBO=3n";
    let gate_resistance = "CGSO=2n CGBO=3n RG=100";
    let cases = [
        ("NMOS", -5., nonlinear, ""),
        ("NMOS", 0., nonlinear, ""),
        ("NMOS", 5., nonlinear, ""),
        ("PMOS", 5., nonlinear, "M=3"),
        ("NMOS", 0., overlap, ""),
        ("NMOS", 0., gate_resistance, ""),
        ("NMOS", 0., gate_resistance, "M=3"),
    ];
    let mut analysis = String::from("ac dec 1 1k 1meg\n");
    let mut vectors = Vec::new();
    for (i, (family, bias, params, instance)) in cases.into_iter().enumerate() {
        writeln!(
            deck,
            "Vg{i} g{i} 0 DC {bias} AC 1\nVd{i} d{i} 0 0\nVs{i} s{i} 0 0\nVb{i} b{i} 0 0\nM{i} d{i} g{i} s{i} b{i} Q{i} {instance}\n.model Q{i} {family} (LEVEL=17 {params})"
        )?;
        for terminal in ['g', 'd', 's', 'b'] {
            for (suffix, function) in [('r', "real"), ('i', "imag")] {
                let vector = format!("{terminal}{i}{suffix}");
                writeln!(analysis, "let {vector}={function}(i(V{terminal}{i}))")?;
                vectors.push(vector);
            }
        }
    }
    deck.push_str(".end\n");
    let actual = run(&deck, &analysis, &vectors.join(" "))?;
    let mut gate = Vec::new();
    for row in &actual {
        ensure!(
            row.len() == 57,
            "unexpected native AC terminal vector width {}",
            row.len()
        );
        let mut gate_row = vec![row[0]];
        for (case, ports) in row[1..].as_chunks::<8>().0.iter().enumerate() {
            gate_row.extend_from_slice(&ports[..2]);
            let scale = ports.iter().fold(0.0f64, |a, b| a.max(b.abs()));
            for component in 0..2 {
                let g = ports[component];
                let d = ports[2 + component];
                let s = ports[4 + component];
                let b = ports[6 + component];
                let tol = 1e-9f64.mul_add(scale, 1e-11);
                ensure!(
                    (g + d + s + b).abs() <= tol,
                    "charge conservation failure case {case} frequency {}",
                    row[0]
                );
                if case < 4 {
                    ensure!(
                        (g + d).abs() <= tol && s.abs() <= tol && b.abs() <= tol,
                        "nonlinear charge terminal incidence"
                    );
                } else {
                    ensure!(
                        0.4f64.mul_add(g, s).abs() <= tol
                            && 0.6f64.mul_add(g, b).abs() <= tol
                            && d.abs() <= tol,
                        "overlap charge terminal incidence"
                    );
                }
            }
        }
        gate.push(gate_row);
    }
    compare(
        &gate,
        &rows(include_str!("fixtures/level17-ac.tsv"))?,
        1e-5,
        1e-10,
    )
}

#[test]
fn level17_nested_scope_shadowing_and_generated_names_are_electrical() -> Result<()> {
    let deck = "* Local and inherited model declarations\nVg g 0 3\nVglobal d0 0 1\nVchild d1 0 1\nVnative d2 0 1\nVlocal d3 0 1\n.model Shared NMOS (LEVEL = 17 VTO=1 KP=.1 RD=.1)\nM1 d0 g 0 0 Shared\nX1 d1 g 0 child\nX2 d2 g 0 native\nX3 d3 g 0 parent\nR__l17_I_M1_RD __l17_I_M1_DINT 0 1\n.subckt child d g s\nMlocal d g s s Shared\n.ends child\n.subckt native d g s\n.model Shared NMOS (LEVEL=1 VTO=2 KP=.1)\nMlocal d g s s Shared L=1 W=1\n.ends native\n.subckt parent d g s\n.model Shared NMOS (LEVEL=17 VTO=0 KP=.1 RD=.1)\n.subckt grandchild d g s\nMlocal d g s s Shared\n.ends grandchild\nXnested d g s grandchild\n.ends parent\n.end\n";
    let translated = translate_simplis_deck(deck.as_bytes())?;
    let text = std::str::from_utf8(&translated.bytes)?;
    ensure!(
        text.contains("R__l17_0_I_M1_RD") && text.contains("R__l17_I_M1_RD"),
        "generated prefix collides with source"
    );
    let rows = run_native(
        &translated.bytes,
        &[],
        "op",
        "i(Vglobal) i(Vchild) i(Vnative) i(Vlocal)",
    )?;
    // The simple native level-1 shadow has Vov=1, Vds=1, so Id=0.05.
    // Global and inherited discrete instances are equal. The nested
    // lexical model has lower threshold and therefore larger current.
    let row = rows.last().context("missing operating point")?;
    ensure!(row.len() == 5, "unexpected OP vector count");
    ensure!(
        (row[1] - row[2]).abs() < 1e-9
            && (row[3] + 0.05).abs() < 1e-8
            && row[4] < row[1]
            && row[1] < row[3],
        "lexical model binding: {row:?}"
    );
    Ok(())
}

#[test]
fn level17_global_models_bind_across_linked_artifacts() -> Result<()> {
    use model_preprocess_simplis::{
        LinkedGeneratedLibrary, LinkedPreprocessedNetlist, NetlistPreprocessStats,
    };
    let linked = LinkedPreprocessedNetlist {
        included_sources: Default::default(),
        root_bytes: b"* Linked global source model\nVg g 0 3\nVroot d 0 1\nVchild d2 0 1\n.model RootMos NMOS (LEVEL=17 VTO=2 KP=.1)\nM1 d g 0 0 LibraryMos\nX1 d2 g 0 cell\n.include \"models.lib\"\n.end\n".to_vec(),
        libraries: vec![LinkedGeneratedLibrary {
            source_id: "synthetic-models".into(), source_name: "models.lb".into(), filename: "models.lib".into(), dependencies: vec![], included_sources: Default::default(),
            bytes: b".model LibraryMos NMOS (LEVEL=17 VTO=1 KP=.1)\n.subckt cell d g s\nM1 d g s s RootMos\n.ends cell\n".to_vec(),
        }],
        stats: NetlistPreprocessStats { root_specializations: 0, generated_subcircuits: 1, local_parameterized_definitions_removed: 0 },
    };
    let (root, libraries) = translate_simplis_linked_netlist(&linked, &WorkBudget::default())?;
    let lib = libraries.first().context("missing translated library")?;
    let actual = run_native(
        &root.bytes,
        &[("models.lib", &lib.bytes)],
        "op",
        "i(Vroot) i(Vchild)",
    )?;
    let row = actual.last().context("missing linked OP")?;
    ensure!(
        row.len() == 3 && (row[1] + 0.15).abs() < 1e-8 && (row[2] + 0.05).abs() < 1e-8,
        "global linked model binding: {row:?}"
    );
    let mut ambiguous = linked;
    ambiguous.libraries[0]
        .bytes
        .extend_from_slice(b".model RootMos NMOS (LEVEL=1 VTO=0 KP=.1)\n");
    ensure!(
        translate_simplis_linked_netlist(&ambiguous, &WorkBudget::default()).is_err(),
        "conflicting global model accepted"
    );
    Ok(())
}
