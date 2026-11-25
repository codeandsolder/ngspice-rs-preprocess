//! Run the independent native-ngspice logic regression matrix with an explicit runtime path.
use anyhow::{Context, Result, ensure};
use model_translate_simplis::translate_simplis_deck;
use std::{collections::BTreeMap, fmt::Write as _, path::Path, process::Command};

fn run_native(runtime: &Path, source: &str, control: &str) -> Result<String> {
    let translated = translate_simplis_deck(source.as_bytes())?;
    let text = std::str::from_utf8(&translated.bytes)?;
    let mut deck = text
        .strip_suffix(".end\n")
        .context("missing deck terminator")?
        .to_owned();
    writeln!(deck, ".control\n{control}quit\n.endc\n.end")?;
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("logic.cir");
    std::fs::write(&path, deck)?;
    std::fs::write(
        temp.path().join(".spiceinit"),
        format!("codemodel {}\n", runtime.display()),
    )?;
    let output = Command::new("ngspice")
        .current_dir(temp.path())
        .arg("-b")
        .arg(path)
        .output()?;
    let stdout = String::from_utf8(output.stdout)?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    ensure!(
        output.status.success()
            && stdout.contains("ngspice-47 done")
            && !stderr.contains("Error:")
            && !stderr.contains("timestep too small"),
        "{stdout}\n{stderr}"
    );
    Ok(stdout)
}

fn verify(runtime: &Path, source: &str, checks: &[(String, f64, f64)]) -> Result<()> {
    let mut control = String::from("tran 10n 9u\n");
    for (index, (node, time, _)) in checks.iter().enumerate() {
        writeln!(control, "meas tran check{index} find v({node}) at={time}u")?;
    }
    let stdout = run_native(runtime, source, &control)?;
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

fn numbered_logic_all_arities_polarities_and_initial_states(runtime: &Path) -> Result<()> {
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
    verify(runtime, &source, &checks)
}

fn sr_latch_polarities_priorities_and_loaded_complements(runtime: &Path) -> Result<()> {
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
    verify(runtime, &source, &checks)
}

fn advanced_gate_receivers_ignore_output_initial_hint(runtime: &Path) -> Result<()> {
    let mut source = String::from("Vref rtn 0 0.75\nVa a rtn 2.5\nVlo lo rtn 0\nVhi hi rtn 5\n");
    let mut checks = Vec::new();
    for gate in ["D_AND", "D_OR"] {
        for initial in [false, true] {
            for (input, expected) in [("a", false), ("lo", false), ("hi", gate == "D_OR")] {
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
    verify(runtime, &source, &checks)
}

fn advanced_gate_vendor_boundaries_and_trailing_inversions(runtime: &Path) -> Result<()> {
    // SIMPLIS 9.40.2 final-state sweeps: both IC hints produce the same
    // receiver state, and a receiver holds at either threshold equality.
    let mut source = String::from(
        "Vref rtn 0 0.75\nVlo lo rtn 0\nVhi hi rtn 5\nVa a rtn 2.5\nVu u rtn 3\nVabove above rtn 3.01\n",
    );
    source.push_str("Vfall fall rtn PWL(0 5 1u 5 1.1u 2 3u 2 3.1u 1.99 9u 1.99)\n");
    let mut checks = Vec::new();
    for (case, inputs, expected) in [
        ("first", "lo hi hi", [false, false, true, false]),
        ("middle", "hi lo hi", [false, false, true, true]),
        ("last", "hi hi lo", [true, false, true, true]),
        ("ambiguous", "a a a", [false, false, true, true]),
        ("low", "lo lo lo", [false, false, true, true]),
        ("high", "hi hi hi", [false, false, true, true]),
    ] {
        for (index, (kind, inverted)) in [("D_AND", 1), ("D_AND", 2), ("D_OR", 1), ("D_OR", 2)]
            .into_iter()
            .enumerate()
        {
            let id = format!("{case}{index}");
            writeln!(
                source,
                "A{id} %gnd_ref rtn [ q{id} qb{id} ] [ {inputs} ] MODEL=m{id} IC=0"
            )?;
            writeln!(
                source,
                ".MODEL m{id} DIGITAL1 {kind} RIN=10Meg ROUT=10 VOL=0.5 VOH=4.5 HYSTWD=1 TH=2.5 OUT_DELAY=0 NUM_INVERTED_INPUTS={inverted}"
            )?;
            writeln!(source, "Rq{id} q{id} rtn 100\nRqb{id} qb{id} rtn 100")?;
            checks.push((format!("q{id}"), 8.0, loaded(expected[index])));
            checks.push((format!("qb{id}"), 8.0, loaded(!expected[index])));
        }
    }
    for (index, (input, time, expected)) in [
        ("u", 0.0, false),
        ("above", 0.0, true),
        ("fall", 2.0, true),
        ("fall", 8.0, false),
    ]
    .into_iter()
    .enumerate()
    {
        let id = format!("boundary{index}");
        writeln!(
            source,
            "A{id} %gnd_ref rtn [ q{id} qb{id} ] [ hi {input} ] MODEL=mboundary IC=1"
        )?;
        writeln!(source, "Rq{id} q{id} rtn 100\nRqb{id} qb{id} rtn 100")?;
        checks.push((format!("q{id}"), time, loaded(expected)));
        checks.push((format!("qb{id}"), time, loaded(!expected)));
    }
    source.push_str(".MODEL mboundary DIGITAL1 D_AND RIN=10Meg ROUT=10 VOL=0.5 VOH=4.5 HYSTWD=1 TH=2.5 OUT_DELAY=0 NUM_INVERTED_INPUTS=0\n.end\n");
    verify(runtime, &source, &checks)
}

fn advanced_latch_vendor_ambiguous_inputs(runtime: &Path) -> Result<()> {
    let mut source = String::from(
        "Vref rtn 0 0.75\nVa a rtn 2.5\nVlo lo rtn 0\nVhi hi rtn 5\nVu u rtn 3\nVl l rtn 2\n",
    );
    let mut checks = Vec::new();
    // Each row is the six final states observed in SIMPLIS 9.40.2
    // (active-low S/R/NONE followed by active-high S/R/NONE), IC=1.
    for (case, inputs, expected) in [
        ("al", "a lo", [true, false, true, true, true, true]),
        ("ah", "a hi", [true, true, true, false, false, false]),
        ("la", "lo a", [true, false, true, true, true, true]),
        ("ha", "hi a", [false, false, false, true, true, true]),
        ("aa", "a a", [true, false, true, true, true, true]),
        ("ul", "u lo", [true, false, true, true, true, true]),
        ("lh", "l hi", [true, true, true, false, false, false]),
    ] {
        for (index, (level, dom)) in [
            (0, "S"),
            (0, "R"),
            (0, "NONE"),
            (1, "S"),
            (1, "R"),
            (1, "NONE"),
        ]
        .into_iter()
        .enumerate()
        {
            let id = format!("{case}{index}");
            writeln!(
                source,
                "A{id} %gnd_ref rtn [ q{id} qb{id} ] [ {inputs} ] MODEL=m{id} IC=1"
            )?;
            writeln!(
                source,
                ".MODEL m{id} DIGITAL1 D_SRLATCH RIN=10Meg ROUT=10 VOL=0.5 VOH=4.5 HYSTWD=1 TH=2.5 OUT_DELAY=0 SET_RESET_LEVEL={level} DOM={dom}"
            )?;
            writeln!(source, "Rq{id} q{id} rtn 100\nRqb{id} qb{id} rtn 100")?;
            checks.push((format!("q{id}"), 8.0, loaded(expected[index])));
            checks.push((format!("qb{id}"), 8.0, loaded(!expected[index])));
        }
    }
    source.push_str(".end\n");
    verify(runtime, &source, &checks)
}

/// Expected event times and output states from SIMPLIS 9.40.2's
/// independently exported 100-ohm loaded traces and final-state sweeps.
fn vendor_timing(runtime: &Path) -> Result<()> {
    const BASE: &str = "Vref rtn 0 0.75
V2 b rtn 5
V3 c rtn 0
!D1 t rtn a b M1 IC=0
.MODEL M1 AND2 RIN=10Meg ROUT=10 VOL=0.5 VOH=4.5 HYSTWD=1 TH=2.5 DELAY=500n LOGIC=POS
A2 %gnd_ref rtn [ i ib ] [ a b ] MODEL=M2 IC=0
.MODEL M2 DIGITAL1 D_AND RIN=10Meg ROUT=10 VOL=0.5 VOH=4.5 HYSTWD=1 TH=2.5 OUT_DELAY=500n
A3 %gnd_ref rtn [ l lb ] [ a c ] MODEL=M3 IC=0
.MODEL M3 DIGITAL1 D_SRLATCH RIN=10Meg ROUT=10 VOL=0.5 VOH=4.5 HYSTWD=1 TH=2.5 OUT_DELAY=500n SET_RESET_LEVEL=1 DOM=NONE
!D4 s sb rtn a c M4 IC=0
.MODEL M4 SRFF RIN=10Meg ROUT=10 VOL=0.5 VOH=4.5 HYSTWD=1 TH=2.5 DELAY=500n LOGIC=POS
Rt t rtn 100
Ri i rtn 100
Rib ib rtn 100
Rl l rtn 100
Rlb lb rtn 100
Rs s rtn 100
Rsb sb rtn 100
.end
";
    for (fall, after, long) in [("1.19u", "1.2u", false), ("1.99u", "2u", true)] {
        let source = format!("V1 a rtn PWL(0 0 1u 0 1.01u 5 {fall} 5 {after} 0 9u 0)\n{BASE}");
        let mut checks = Vec::new();
        for (time, values) in [
            (1.3, [false, false, false, false]),
            (1.65, [true, long, long, true]),
            (1.85, [long, long, long, true]),
            (2.6, [false, false, long, true]),
        ] {
            for (node, high) in ["t", "i", "l", "s"].into_iter().zip(values) {
                checks.push((node.to_owned(), time, loaded(high)));
                if node != "t" {
                    checks.push((format!("{node}b"), time, loaded(!high)));
                }
            }
        }
        verify(runtime, &source, &checks)?;
        // eprint exposes the event time directly; analog threshold measurement
        // interpolates a discontinuity and is not a reliable timestamp.
        let stdout = run_native(
            runtime,
            &source,
            "tran 100n 2.6u\neprint ___D1_Q_STATE __A2_Q_STATE __A3_Q_STATE __D4_Q_STATE\n",
        )?;
        let mut previous = ["0s"; 4];
        let mut edges = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
        for line in stdout.lines() {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            if fields.len() != 5 {
                continue;
            }
            let Ok(time) = fields[0].parse::<f64>() else {
                continue;
            };
            for (index, value) in fields[1..].iter().enumerate() {
                if *value != previous[index] {
                    edges[index].push((time, *value == "1s"));
                    previous[index] = value;
                }
            }
        }
        let falling = if long { 2.496e-6 } else { 1.696e-6 };
        let transport = [(1.506e-6, true), (falling, false)];
        let set_only = [(1.506e-6, true)];
        let expected: [&[(f64, bool)]; 4] = [
            &transport,
            if long { &transport } else { &[] },
            if long { &set_only } else { &[] },
            &set_only,
        ];
        for (actual, expected) in edges.iter().zip(expected) {
            ensure!(
                actual.len() == expected.len(),
                "wrong event sequence: {edges:?}\n{stdout}"
            );
            for ((time, bit), (reference_time, reference_bit)) in actual.iter().zip(expected) {
                ensure!(
                    (time - reference_time).abs() < 1e-15 && bit == reference_bit,
                    "event {time} {bit} != {reference_time} {reference_bit}; sequence={edges:?}"
                );
            }
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let model = args.next().context("usage: logic_ngspice RUNTIME_CM")?;
    ensure!(args.next().is_none(), "usage: logic_ngspice RUNTIME_CM");
    let runtime = std::fs::canonicalize(model)?;
    ensure!(
        runtime.is_file() && !runtime.to_string_lossy().contains(char::is_whitespace),
        "runtime must be a file with a whitespace-free absolute path"
    );
    numbered_logic_all_arities_polarities_and_initial_states(&runtime)?;
    sr_latch_polarities_priorities_and_loaded_complements(&runtime)?;
    advanced_gate_receivers_ignore_output_initial_hint(&runtime)?;
    advanced_gate_vendor_boundaries_and_trailing_inversions(&runtime)?;
    advanced_latch_vendor_ambiguous_inputs(&runtime)?;
    vendor_timing(&runtime)?;
    println!("All native ngspice logic regressions passed");
    Ok(())
}
