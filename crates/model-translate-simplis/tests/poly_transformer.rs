//! Circuit-level checks of the generic POLY and transformer lowering.
use anyhow::{Context, Result, ensure};
use model_translate_simplis::translate_simplis_deck;
use std::{fmt::Write as _, fs, process::Command};

fn operating_point(native: &[u8], vectors: &str) -> Result<Vec<f64>> {
    let dir = tempfile::tempdir()?;
    let body = std::str::from_utf8(native)?
        .trim_end()
        .strip_suffix(".end")
        .context("native deck end")?;
    let deck = format!(
        "Generic translation witness\n{body}\n.control\nset wr_singlescale\nset numdgt=16\nop\nwrdata trace.tsv {vectors}\nquit\n.endc\n.end\n"
    );
    fs::write(dir.path().join("case.cir"), deck)?;
    let result = Command::new("ngspice")
        .args(["-n", "-b", "case.cir"])
        .current_dir(dir.path())
        .output()?;
    ensure!(
        result.status.success(),
        "ngspice: {}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let data = fs::read_to_string(dir.path().join("trace.tsv")).with_context(|| {
        format!(
            "missing vectors: {}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        )
    })?;
    data.split_whitespace()
        .skip(1)
        .map(|value| Ok(value.parse::<f64>()?))
        .collect()
}

fn close(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() <= 1e-9 * expected.abs().max(1.),
        "{actual} != {expected}"
    );
}

fn transformer(count: u32) -> Result<(String, String, Vec<f64>)> {
    let mut source = format!("Vdrive p 0 3\n!T1 N_WIND={count} p 0 N1=2");
    let mut loads = String::new();
    let mut probes = String::from("i(Vdrive)");
    let mut expected = vec![0.];
    for winding in 2..=count {
        let node = format!("s{winding}");
        let (dotted, undotted, sign) = if winding % 2 == 0 {
            (node.as_str(), "0", 1.)
        } else {
            ("0", node.as_str(), -1.)
        };
        let turns = f64::from(winding) + 0.5;
        let resistance = f64::from(winding) + 10.;
        let voltage = 3. * turns / 2.;
        write!(source, " {dotted} {undotted} N{winding}={turns}")?;
        writeln!(loads, "R{winding} {node} 0 {resistance}")?;
        write!(probes, " v({node})")?;
        expected[0] -= voltage * voltage / resistance / 3.;
        expected.push(sign * voltage);
    }
    writeln!(source, "\n{loads}.end")?;
    Ok((source, probes, expected))
}

#[test]
fn transformer_winding_boundary_conserves_power_and_polarity() -> Result<()> {
    for count in [2, 33, 255] {
        let (source, probes, expected) = transformer(count)?;
        let translated = translate_simplis_deck(source.as_bytes())?;
        let text = std::str::from_utf8(&translated.bytes)?;
        for prefix in ["V__T1_W", "E__T1_W", "F__T1_W"] {
            assert_eq!(
                text.lines().filter(|line| line.starts_with(prefix)).count(),
                usize::try_from(count - 1)?
            );
        }
        let values = operating_point(&translated.bytes, &probes)?;
        assert_eq!(values.len(), expected.len());
        for (&actual, want) in values.iter().zip(expected) {
            close(actual, want);
        }
    }
    Ok(())
}

#[test]
fn transformer_rejects_invalid_counts_turns_and_ratios() -> Result<()> {
    let (too_many, _, _) = transformer(256)?;
    assert!(translate_simplis_deck(too_many.as_bytes()).is_err());
    for statement in [
        "!T1 N_WIND=0 p 0 N1=2 s 0 N2=3",
        "!T1 N_WIND=1 p 0 N1=2 s 0 N2=3",
        "!T1 N_WIND=2.5 p 0 N1=2 s 0 N2=3",
        "!T1 N_WIND=3 p 0 N1=2 s 0 N2=3",
        "!T1 N_WIND=2 p 0 N1=2 s 0 N3=3",
        "!T1 N_WIND=2 p 0 N1=2 s 0 N2=0",
        "!T1 N_WIND=2 p 0 N1=-2 s 0 N2=3",
        "!T1 N_WIND=2 p 0 N1=1e-300 s 0 N2=1e300",
        "!T1 N_WIND=2 p 0 N1=1e300 s 0 N2=1e-300",
    ] {
        let source = format!("{statement}\n.end\n");
        assert!(
            translate_simplis_deck(source.as_bytes()).is_err(),
            "{statement}"
        );
    }
    Ok(())
}

#[test]
fn polynomial_controls_preserve_order_sign_and_single_coefficient_gain() -> Result<()> {
    let source = b"Va a 0 -2\nVb b 0 3\nVc c 0 0\nIc 0 c -2\nVd d 0 0\nId 0 d 3\nE1 e 0 POLY(2) (a,0) (b,0) 1 2 3 4 5 6 7 8 9 10\nH1 h 0 POLY 2 Vc Vd 1 2 3 4 5 6 7 8 9 10\nG1 0 g POLY(1) a 0 7\nF1 0 f POLY(1) Vc 7\nE2 k 0 POLY(2) a 0 b 0 7\nRg g 0 1\nRf f 0 1\n.end\n";
    let translated = translate_simplis_deck(source)?;
    let values = operating_point(&translated.bytes, "v(e) v(h) v(g) v(f) v(k)")?;
    assert_eq!(values.len(), 5);
    for (&actual, expected) in values.iter().zip([194., 194., -14., -14., 7.]) {
        close(actual, expected);
    }
    Ok(())
}

#[test]
fn sparse_polynomial_above_legacy_limits_executes() -> Result<()> {
    let mut source = String::from("Va a 0 -1\nVb b 0 2\nE1 e 0 POLY(64)");
    for _ in 0..64 {
        source.push_str(" b 0");
    }
    let mut coefficients = vec![0; 1 + 64 + 64 * 65 / 2];
    coefficients[66] = 2;
    let last = coefficients.len() - 1;
    coefficients[last] = 3;
    for coefficient in coefficients {
        write!(source, " {coefficient}")?;
    }
    source.push_str("\nE2 h 0 POLY(1) a 0");
    for _ in 0..513 {
        source.push_str(" 0");
    }
    source.push_str(" 7\n.end\n");
    let translated = translate_simplis_deck(source.as_bytes())?;
    let values = operating_point(&translated.bytes, "v(e) v(h)")?;
    assert_eq!(values.len(), 2);
    close(values[0], 20.);
    close(values[1], -7.);
    Ok(())
}

#[test]
fn malformed_polynomial_dimensions_and_operands_fail() {
    for statement in [
        "E1 out 0 POLY(0) 7",
        "F1 out 0 POLY(0) 7",
        "E1 out 0 POLY(18446744073709551615) a 0 7",
        "F1 out 0 POLY(18446744073709551615) V1 7",
        "E1 out 0 POLY(2) a 0 7",
        "F1 out 0 POLY(2) V1 7",
        "E1 out 0 POLY(1) a 0",
        "H1 out 0 POLY(1) V1",
        "E1 out 0 POLY(1 a 0 7",
        "F1 out 0 POLY(1) V1 (7",
        "E1 out 0 POLY(1) (a,0)) 7",
        "H1 out 0 POLY(1) V1 1e999",
    ] {
        let source = format!("V1 a 0 1\n{statement}\n.end\n");
        assert!(
            translate_simplis_deck(source.as_bytes()).is_err(),
            "{statement}"
        );
    }
}
