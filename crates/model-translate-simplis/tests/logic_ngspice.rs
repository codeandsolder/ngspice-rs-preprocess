//! Native simulator checks of translated electrical logic, not vendor traces.
use anyhow::{Context, Result, ensure};
use model_translate_simplis::translate_simplis_deck;
use std::{collections::BTreeMap, fmt::Write as _, process::Command};

fn verify(source: &str, checks: &[(String, f64, f64)]) -> Result<()> {
    let translated = translate_simplis_deck(source.as_bytes())?;
    let text = std::str::from_utf8(&translated.bytes)?;
    let mut deck = text
        .strip_suffix(".end\n")
        .context("missing deck terminator")?
        .to_owned();
    deck.push_str(".control\ntran 10n 9u\n");
    for (index, (node, time, _)) in checks.iter().enumerate() {
        writeln!(deck, "meas tran check{index} find v({node}) at={time}u")?;
    }
    deck.push_str("quit\n.endc\n.end\n");
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("logic.cir");
    std::fs::write(&path, deck)?;
    let output = Command::new("ngspice")
        .args(["-b", "-n"])
        .arg(path)
        .output()?;
    let stdout = String::from_utf8(output.stdout)?;
    ensure!(
        output.status.success() && stdout.contains("ngspice-47 done"),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let actual = stdout
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once('=')?;
            Some((
                key.trim().strip_prefix("check")?.parse::<usize>().ok()?,
                value.split_whitespace().next()?.parse::<f64>().ok()?,
            ))
        })
        .collect::<BTreeMap<_, _>>();
    for (index, (node, time, expected)) in checks.iter().enumerate() {
        let value = *actual
            .get(&index)
            .with_context(|| format!("missing {node} at {time}: {stdout}"))?;
        ensure!(
            (value - 0.75 - expected).abs() < 5e-5,
            "{node} at {time} us: {value} != 0.75 + {expected}"
        );
    }
    Ok(())
}

fn loaded(high: bool) -> f64 {
    if high {
        4.5 * 10.0 / 11.0
    } else {
        0.5 * 10.0 / 11.0
    }
}

#[test]
#[ignore = "requires installed ngspice 47 C reference"]
fn numbered_logic_all_arities_polarities_and_initial_states() -> Result<()> {
    let mut source = String::from("Vref rtn 0 0.75\n");
    // Initial ambiguous, then all low, all high, mixed, all ambiguous
    // (hold), all low. The second input crosses a different threshold.
    source.push_str(
        "Va a rtn PWL(0 2.5 1u 2.5 1.1u 0 2u 0 2.1u 5 4u 5 4.1u 2.5 6u 2.5 6.1u 0 9u 0)\n",
    );
    source.push_str("Vb b rtn PWL(0 2.5 1u 2.5 1.1u 0 2u 0 2.1u 5 3u 5 3.1u 0 4u 0 4.1u 2.5 6u 2.5 6.1u 0 9u 0)\n");
    let mut checks = Vec::new();
    for arity in 2..=9 {
        let inputs = std::iter::repeat_n("a", arity - 1)
            .chain(["b"])
            .collect::<Vec<_>>()
            .join(" ");
        for (gate, and, inverted) in [
            ("AND", true, false),
            ("NAND", true, true),
            ("OR", false, false),
            ("NOR", false, true),
        ] {
            for (polarity, negative) in [("POS", false), ("NEG", true)] {
                for initial in [false, true] {
                    let id = format!("{gate}{arity}{polarity}{}", u8::from(initial));
                    writeln!(
                        source,
                        ".MODEL m{id} {gate}{arity} RIN=10Meg ROUT=10 VOL=0.5 VOH=4.5 HYSTWD=1 DELAY=0 TH=2.5 LOGIC={polarity}"
                    )?;
                    writeln!(
                        source,
                        "!d{id} q{id} rtn {inputs} m{id} IC={}",
                        u8::from(initial)
                    )?;
                    writeln!(source, "R{id} q{id} rtn 100")?;
                    let gate_out = |a: bool, b: bool| {
                        let (a, b) = (a ^ negative, b ^ negative);
                        let bit = if and { a && b } else { a || b };
                        bit ^ inverted ^ negative
                    };
                    for (time, high) in [
                        (0.0, initial ^ negative),
                        (1.5, gate_out(false, false)),
                        (2.5, gate_out(true, true)),
                        (3.5, gate_out(true, false)),
                        (5.0, gate_out(true, false)),
                        (8.0, gate_out(false, false)),
                    ] {
                        checks.push((format!("q{id}"), time, loaded(high)));
                    }
                }
            }
        }
    }
    source.push_str(".end\n");
    verify(&source, &checks)
}

#[test]
#[ignore = "requires installed ngspice 47 C reference"]
fn sr_latch_polarities_priorities_and_loaded_complements() -> Result<()> {
    let mut source = String::from("Vref rtn 0 0.75\n");
    // Inactive, set, hold, simultaneous assertion, hold, reset, hold.
    for (name, negative) in [("pos", false), ("neg", true)] {
        let voltage = |active: bool| if active ^ negative { 5 } else { 0 };
        let off = voltage(false);
        let on = voltage(true);
        writeln!(
            source,
            "Vs{name} s{name} rtn PWL(0 {off} 1u {off} 1.1u {on} 2u {on} 2.1u {off} 3u {off} 3.1u {on} 4u {on} 4.1u {off} 9u {off})"
        )?;
        writeln!(
            source,
            "Vr{name} r{name} rtn PWL(0 {off} 3u {off} 3.1u {on} 4u {on} 4.1u {off} 5u {off} 5.1u {on} 6u {on} 6.1u {off} 9u {off})"
        )?;
    }
    let mut checks = Vec::new();
    for (logic, negative) in [("POS", false), ("NEG", true)] {
        let nodes = logic.to_ascii_lowercase();
        for initial in [false, true] {
            for dom in ["S", "R", "NONE", "SRFF"] {
                let id = format!("{dom}{logic}{}", u8::from(initial));
                if dom == "SRFF" {
                    writeln!(
                        source,
                        "!d{id} q{id} qb{id} rtn s{nodes} r{nodes} m{id} IC={}",
                        u8::from(initial)
                    )?;
                    writeln!(
                        source,
                        ".MODEL m{id} SRFF RIN=10Meg ROUT=10 VOL=0.5 VOH=4.5 HYSTWD=1 TH=2.5 LOGIC={logic}"
                    )?;
                } else {
                    writeln!(
                        source,
                        "A{id} %gnd_ref rtn [ q{id} qb{id} ] [ s{nodes} r{nodes} ] MODEL=m{id} IC={}",
                        u8::from(initial)
                    )?;
                    writeln!(
                        source,
                        ".MODEL m{id} DIGITAL1 D_SRLATCH RIN=10Meg ROUT=10 VOL=0.5 VOH=4.5 HYSTWD=1 TH=2.5 OUT_DELAY=0 SET_RESET_LEVEL={} DOM={dom}",
                        u8::from(!negative)
                    )?;
                }
                writeln!(source, "Rq{id} q{id} rtn 100\nRqb{id} qb{id} rtn 100")?;
                let output_negative = dom == "SRFF" && negative;
                let both = dom != "R";
                for (time, bit) in [
                    (0.0, initial),
                    (1.5, true),
                    (2.5, true),
                    (3.5, both),
                    (4.5, both),
                    (5.5, false),
                    (8.0, false),
                ] {
                    let high = bit ^ output_negative;
                    checks.push((format!("q{id}"), time, loaded(high)));
                    checks.push((format!("qb{id}"), time, loaded(!high)));
                }
            }
        }
    }
    source.push_str(".end\n");
    verify(&source, &checks)
}

#[test]
#[ignore = "requires installed ngspice 47 C reference"]
fn advanced_gate_initial_hint_and_definite_inputs() -> Result<()> {
    let mut source = String::from("Vref rtn 0 0.75\nVa a rtn 2.5\nVlo lo rtn 0\nVhi hi rtn 5\n");
    let mut checks = Vec::new();
    for gate in ["D_AND", "D_OR"] {
        for initial in [false, true] {
            for (input, expected) in [
                ("a", initial),
                ("lo", gate == "D_OR" && initial),
                ("hi", gate == "D_OR" || initial),
            ] {
                let id = format!("{gate}{input}{}", u8::from(initial));
                writeln!(
                    source,
                    "A{id} %gnd_ref rtn [ q{id} qb{id} ] [ a {input} ] MODEL=m{id} IC={}",
                    u8::from(initial)
                )?;
                writeln!(
                    source,
                    ".MODEL m{id} DIGITAL1 {gate} RIN=10Meg ROUT=10 VOL=0.5 VOH=4.5 HYSTWD=1 TH=2.5 OUT_DELAY=0"
                )?;
                writeln!(source, "Rq{id} q{id} rtn 100\nRqb{id} qb{id} rtn 100")?;
                checks.push((format!("q{id}"), 0.0, loaded(expected)));
                checks.push((format!("qb{id}"), 0.0, loaded(!expected)));
            }
        }
    }
    source.push_str(".end\n");
    verify(&source, &checks)
}
