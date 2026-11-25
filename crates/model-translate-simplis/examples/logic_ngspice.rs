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

fn measurements(stdout: &str) -> BTreeMap<usize, f64> {
    stdout
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once('=')?;
            Some((
                key.trim().strip_prefix("check")?.parse::<usize>().ok()?,
                value.split_whitespace().next()?.parse::<f64>().ok()?,
            ))
        })
        .collect()
}

fn verify(runtime: &Path, source: &str, checks: &[(String, f64, f64)]) -> Result<()> {
    let mut control = String::from("tran 10n 9u\n");
    for (index, (node, time, _)) in checks.iter().enumerate() {
        writeln!(control, "meas tran check{index} find v({node}) at={time}u")?;
    }
    let stdout = run_native(runtime, source, &control)?;
    let actual = measurements(&stdout);
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

fn event_edges<const N: usize>(stdout: &str, initial: [bool; N]) -> [Vec<(f64, bool)>; N] {
    let mut previous = initial;
    let mut edges = std::array::from_fn(|_| Vec::new());
    for line in stdout.lines() {
        let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
        if fields.len() != N + 1 {
            continue;
        }
        let Ok(time) = fields[0].parse::<f64>() else {
            continue;
        };
        for (index, value) in fields[1..].iter().enumerate() {
            let bit = *value == "1s";
            if bit != previous[index] {
                edges[index].push((time, bit));
                previous[index] = bit;
            }
        }
    }
    edges
}

fn check_edges(actual: &[Vec<(f64, bool)>], expected: &[&[(f64, bool)]]) -> Result<()> {
    ensure!(actual.len() == expected.len(), "incorrect trace width");
    for (trace, expected) in actual.iter().zip(expected) {
        ensure!(
            trace.len() == expected.len(),
            "wrong event sequence: {actual:?}"
        );
        for ((time, bit), (reference_time, reference_bit)) in trace.iter().zip(*expected) {
            ensure!(
                (time - reference_time).abs() < 1e-15 && bit == reference_bit,
                "event {time} {bit} != {reference_time} {reference_bit}; sequence={actual:?}"
            );
        }
    }
    Ok(())
}

fn threshold_plateau_and_latch_cancellation(runtime: &Path) -> Result<()> {
    let source = "Vref rtn 0 0.75
V1 a rtn PWL(0 0 1u 0 1.01u 3 2u 3 2.01u 5 3u 5 3.01u 2 4u 2 4.01u 0 9u 0)
V2 b rtn 5
V3 c rtn PWL(0 0 4.5u 0 4.51u 5 4.6u 5 4.61u 0 6u 0 6.01u 5 7u 5 7.01u 0 9u 0)
!D1 t rtn a b M1 IC=0
.MODEL M1 AND2 RIN=10Meg ROUT=10 VOL=0.5 VOH=4.5 HYSTWD=1 TH=2.5 DELAY=500n LOGIC=POS
A2 %gnd_ref rtn [ i ib ] [ a b ] MODEL=M2 IC=0
.MODEL M2 DIGITAL1 D_AND RIN=10Meg ROUT=10 VOL=0.5 VOH=4.5 HYSTWD=1 TH=2.5 OUT_DELAY=500n
A3 %gnd_ref rtn [ l lb ] [ a c ] MODEL=M3 IC=0
.MODEL M3 DIGITAL1 D_SRLATCH RIN=10Meg ROUT=10 VOL=0.5 VOH=4.5 HYSTWD=1 TH=2.5 OUT_DELAY=500n SET_RESET_LEVEL=1 DOM=NONE
Rt t rtn 100
Ri i rtn 100
Rib ib rtn 100
Rl l rtn 100
Rlb lb rtn 100
.end
";
    let stdout = run_native(
        runtime,
        source,
        "tran 100n 8u\neprint ___D1_Q_STATE __A2_Q_STATE __A3_Q_STATE\n",
    )?;
    // Holding exactly at either threshold does not transition. Departure
    // must be resolved at the solver's breakpoint precision rather than
    // delayed by an entire coarse accepted step. A short reset is cancelled
    // and the latch retains its committed set bit until the sustained reset.
    let gate = [(2.5e-6, true), (4.5e-6, false)];
    let latch = [(2.5e-6, true), (6.506e-6, false)];
    check_edges(&event_edges(&stdout, [false; 3]), &[&gate, &gate, &latch])
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
    // Independently swept pulse widths 190, 499, 500, 501 and 990 ns.
    // The exact-width 500 ns pulse is suppressed by DIGITAL1.
    for (fall, after, long, falling) in [
        ("1.19u", "1.2u", false, 1.696e-6),
        ("1.499u", "1.509u", false, 2.005e-6),
        ("1.5u", "1.51u", false, 2.006e-6),
        ("1.501u", "1.511u", true, 2.007e-6),
        ("1.99u", "2u", true, 2.496e-6),
    ] {
        let source = format!("V1 a rtn PWL(0 0 1u 0 1.01u 5 {fall} 5 {after} 0 9u 0)\n{BASE}");
        let mut checks = Vec::new();
        for time in [1.3, 1.65, 1.85, 2.2, 2.6] {
            let arrived = time > 1.506;
            let gate = arrived && time * 1e-6 < falling;
            let values = [gate, gate && long, arrived && long, arrived];
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
        let edges = event_edges(&stdout, [false; 4]);
        let transport = [(1.506e-6, true), (falling, false)];
        let set_only = [(1.506e-6, true)];
        let expected: [&[(f64, bool)]; 4] = [
            &transport,
            if long { &transport } else { &[] },
            if long { &set_only } else { &[] },
            &set_only,
        ];
        check_edges(&edges, &expected)?;
    }
    Ok(())
}

/// Vendor startup-state sweep at 0, 2, 2.5, 3, and 5 V with
/// TH=2.5, HYSTWD=1. Independent load-line solutions test both transistor
/// model levels, both device polarities, voltage/current control and
/// reverse bias rather than just checking the reported switch state.
fn switch_startup_and_load_lines(runtime: &Path) -> Result<()> {
    let mut source = String::from("Vref rtn 0 0.75\nVpos pos rtn 1\nVneg neg rtn -1\n");
    let mut checks = Vec::new();
    for (index, control) in [0.0_f64, 2.0, 2.5, 3.0, 5.0].into_iter().enumerate() {
        writeln!(source, "Vc{index} c{index} rtn {control}")?;
        // Current sense has 1 ohm resistance, so its branch current is
        // numerically identical to the differential voltage control.
        writeln!(
            source,
            "Vi{index} c{index} ci{index} 0\nRc{index} ci{index} rtn 1"
        )?;
        for (logic, negative) in [("POS", false), ("NEG", true)] {
            for (ic, suggested) in [("OPEN", false), ("CLOSE", true)] {
                let closed = if control > 3.0 {
                    !negative
                } else if control < 2.0 {
                    negative
                } else {
                    suggested
                };
                let id = format!("{index}{logic}{ic}");
                for (bias, rail) in [(1.0_f64, "pos"), (-1.0, "neg")] {
                    let suffix = format!("{id}{rail}");
                    writeln!(
                        source,
                        "S{suffix} s{suffix} rtn c{index} rtn ms{suffix} IC={ic}"
                    )?;
                    writeln!(
                        source,
                        ".model ms{suffix} VCSW RON=10 ROFF=1Meg TH=2.5 HYSTWD=1 LOGIC={logic}"
                    )?;
                    writeln!(source, "Rs{suffix} {rail} s{suffix} 100")?;
                    let resistance = if closed { 10.0 } else { 1e6 };
                    let expected = bias * resistance / (resistance + 100.0);
                    for time in [0.0, 8.0] {
                        checks.push((format!("s{suffix}"), time, expected));
                    }
                    for (kind, sign) in [("POS", 1.0_f64), ("NEG", -1.0)] {
                        for level in [1, 2] {
                            for (control_kind, pins) in [
                                ("VC", format!("c{index} rtn")),
                                ("IC", format!("Vi{index}")),
                            ] {
                                let q = format!("{suffix}{kind}{level}{control_kind}");
                                let vsat = 0.1 * sign;
                                writeln!(source, "Q{q} q{q} rtn {pins} mq{q} IC={ic}")?;
                                writeln!(
                                    source,
                                    ".model mq{q} {control_kind}Q{kind} VSAT={vsat} RSAT=10 ROFF=1Meg GAIN=0.002 TH=2.5 HYSTWD=1 LOGIC={logic} LEVEL={level}"
                                )?;
                                writeln!(source, "Rq{q} {rail} q{q} 100")?;
                                let off = bias / (1.0 + 100.0 / 1e6);
                                let sat = 100.0_f64.mul_add(vsat, bias * 10.0) / 110.0;
                                let overdrive = if negative {
                                    3.0 - control
                                } else {
                                    control - 2.0
                                };
                                let active = (100.0 * sign * 0.002).mul_add(-overdrive, bias)
                                    / (1.0 + 100.0 / 1e6);
                                let expected = if !closed {
                                    off
                                } else if level == 1 {
                                    sat
                                } else if sign.is_sign_positive() {
                                    active.max(sat).min(off)
                                } else {
                                    active.min(sat).max(off)
                                };
                                for time in [0.0, 8.0] {
                                    checks.push((format!("q{q}"), time, expected));
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    source.push_str(".end\n");
    verify(runtime, &source, &checks)
}

// Direct SIMetrix 9.40d printed waveform samples from a 1 V biased
// switch. Reference maxstep 0.25 ns, print interval 1 ns, RELTOL=1e-6.
fn check_timed_switch(
    runtime: &Path,
    drive: &str,
    tolerance: &str,
    samples: &[(f64, f64)],
) -> Result<()> {
    let source = format!(
        "Vref rtn 0 0.75\nVdrive a rtn {drive}\nVbias p rtn 1\nS1 p rtn a rtn m IC=0\n.model m VSWITCH(RON=10 ROFF=1Meg VT=2.5 VH=0.5 TON=100n TOFF=200n TD_ON=500n TD_OFF=300n TIMETOL={tolerance} GIN=0.125)\n.options reltol=1e-6 abstol=1e-13\n.end\n"
    );
    let mut control = String::from("tran 1n 6u 0 0.25n\n");
    for (index, (time, _)) in samples.iter().enumerate() {
        writeln!(control, "meas tran check{index} find i(vbias) at={time}n")?;
    }
    let stdout = run_native(runtime, &source, &control)?;
    let values = measurements(&stdout);
    for (index, (time, expected)) in samples.iter().enumerate() {
        let actual = -*values
            .get(&index)
            .with_context(|| format!("missing switch current at {time} ns: {stdout}"))?;
        ensure!(
            (actual - expected).abs() <= expected.abs() * 0.002 + 1e-12,
            "TIMETOL={tolerance}, {time} ns: {actual} A != vendor {expected} A"
        );
    }
    Ok(())
}

fn timed_switch_vendor_waveforms(runtime: &Path) -> Result<()> {
    let baseline = "PWL(0 0 1u 0 1.01u 5 1.2u 5 1.21u 0 3u 0 3.01u 5 4u 5 4.01u 0 6u 0)";
    let baseline_samples = [
        (0.0, 1e-6),
        (1550.0, 1e-6),
        (3500.0, 1e-6),
        (3510.0, 1.05512e-6),
        (3540.0, 2.19091e-5),
        (3560.0, 0.000_629_491),
        (3580.0, 0.014_503_464),
        (3600.0, 0.088_730_93),
        (3610.0, 0.1),
        (4300.0, 0.1),
        (4310.0, 0.098_649_46),
        (4350.0, 0.024_021_974),
        (4400.0, 0.000_530_782),
        (4450.0, 9.04963e-6),
        (4500.0, 1.03098e-6),
        (4520.0, 1e-6),
    ];
    for tolerance in ["1f", "1p", "10p", "100p", "1n", "10n", "100n"] {
        check_timed_switch(runtime, baseline, tolerance, &baseline_samples)?;
    }

    timed_switch_pulse_boundaries(runtime)
}

fn timed_switch_pulse_boundaries(runtime: &Path) -> Result<()> {
    // Pulse width exactly equal to propagation delay has no excursion.
    // Width equal to delay + slew duration reverses at the endpoint;
    // a longer pulse reaches a stable rail and incurs the turn-off delay.
    for (width, samples) in [
        (
            499,
            [
                (1507.0, 1e-6),
                (1606.0, 1e-6),
                (1700.0, 1e-6),
                (1900.0, 1e-6),
            ],
        ),
        (
            500,
            [
                (1507.0, 1e-6),
                (1606.0, 1e-6),
                (1700.0, 1e-6),
                (1900.0, 1e-6),
            ],
        ),
        (
            501,
            [
                (1507.0, 1.0034e-6),
                (1606.0, 1e-6),
                (1700.0, 1e-6),
                (1900.0, 1e-6),
            ],
        ),
        (
            550,
            [
                (1507.0, 1.0034e-6),
                (1606.0, 6.04492e-6),
                (1700.0, 1e-6),
                (1900.0, 1e-6),
            ],
        ),
        (
            599,
            [
                (1507.0, 1.0034e-6),
                (1606.0, 0.099_236_131),
                (1700.0, 0.000_409_877),
                (1900.0, 1e-6),
            ],
        ),
        (
            600,
            [
                (1507.0, 1.0034e-6),
                (1606.0, 0.099_999_991),
                (1700.0, 0.000_530_782),
                (1900.0, 1e-6),
            ],
        ),
        (
            601,
            [
                (1507.0, 1.0034e-6),
                (1606.0, 0.099_999_991),
                (1700.0, 0.1),
                (1900.0, 0.1),
            ],
        ),
        (
            650,
            [
                (1507.0, 1.0034e-6),
                (1606.0, 0.099_999_991),
                (1700.0, 0.1),
                (1900.0, 0.1),
            ],
        ),
    ] {
        let end = 1000 + width;
        let drive = format!("PWL(0 0 1u 0 1.01u 5 {end}n 5 {}n 0 6u 0)", end + 10);
        check_timed_switch(runtime, &drive, "10p", &samples)?;
    }
    check_timed_switch(
        runtime,
        "PWL(0 5 1u 5 1.01u 0 1.35u 0 1.36u 5 6u 5)",
        "10p",
        &[
            (0.0, 0.1),
            (1300.0, 0.1),
            (1356.0, 0.016_553_53),
            (1380.0, 0.099_654_058),
            (1400.0, 0.1),
        ],
    )
}

fn timed_switch_runtime_parameters(runtime: &Path) -> Result<()> {
    let source = "Vref rtn 0 0.75
Vdrive a rtn 2.5
Vbias p rtn 1
X1 p rtn a rtn T
.subckt T p n cp cn params: R=10 OFF=1Meg TOL=100p INIT=1
S1 p n cp cn M IC={INIT}
.model M VSWITCH(RON={max(R,1)}, ROFF={OFF}, VT=2.5, VH=0.5, TIMETOL={max(TOL,1p)}, GIN=0.125)
.ends T
.end
";
    let stdout = run_native(
        runtime,
        source,
        "tran 10n 1u\nmeas tran check0 find i(vbias) at=0\nmeas tran check1 find i(vdrive) at=0\n",
    )?;
    let actual = measurements(&stdout);
    for (index, expected) in [-0.1, -0.3125].into_iter().enumerate() {
        let value = *actual
            .get(&index)
            .with_context(|| format!("missing runtime measurement: {stdout}"))?;
        ensure!(
            (value - expected).abs() <= 1e-8,
            "runtime measurement {index}: {value} != {expected}"
        );
    }
    Ok(())
}

fn timed_switch_small_signal_and_stiff_load(runtime: &Path) -> Result<()> {
    let source = "Vdrive a 0 DC 5 AC 1
Vbias p 0 DC 1 AC 1
S1 p 0 a 0 M IC=0
.model M VSWITCH(RON=10 ROFF=1Meg VT=2.5 VH=0.5 GIN=0.125)
.end
";
    let stdout = run_native(
        runtime,
        source,
        "ac lin 3 1k 3k\nlet ib=real(i(vbias))\nlet ic=real(i(vdrive))\nmeas ac check0 find ib at=2k\nmeas ac check1 find ic at=2k\n",
    )?;
    let actual = measurements(&stdout);
    for (index, expected) in [-0.1, -0.125].into_iter().enumerate() {
        let value = *actual
            .get(&index)
            .with_context(|| format!("missing AC measurement: {stdout}"))?;
        ensure!(
            (value - expected).abs() <= 1e-8,
            "AC measurement {index}: {value} != {expected}"
        );
    }
    let source = "Vdrive a 0 5
Vbias p 0 1
Rload p b 1
S1 b 0 a 0 M IC=0
.model M VSWITCH(RON=1n ROFF=1T VT=2.5 VH=0.5)
.end
";
    let stdout = run_native(
        runtime,
        source,
        "tran 10n 1u\nmeas tran check0 find v(b) at=0.5u\n",
    )?;
    let value = *measurements(&stdout)
        .get(&0)
        .with_context(|| format!("missing stiff load measurement: {stdout}"))?;
    ensure!(
        (value - 1e-9 / (1.0 + 1e-9)).abs() < 1e-14,
        "stiff load voltage {value}"
    );
    Ok(())
}

fn switch_named_voltage_branch(runtime: &Path) -> Result<()> {
    let source = "Vref rtn 0 0.75
Vctrl c rtn PWL(0 0 1u 0 1.01u 5 4u 5 4.01u 0 9u 0)
Rctrl c rtn 1k
Vbias p rtn 1
S1 a rtn Rctrl MS IC=CLOSE
S2 b rtn (c,rtn) MS IC=OPEN
Q1 q rtn Vctrl MQ IC=CLOSE
Q2 r rtn c rtn MQ IC=OPEN
Ra p a 100
Rb p b 100
Rq p q 100
Rr p r 100
.MODEL MS VCSW RON=10 ROFF=1Meg TH=2.5 HYSTWD=1 LOGIC=POS
.MODEL MQ VCQPOS VSAT=0.1 RSAT=10 ROFF=1Meg GAIN=1 TH=2.5 HYSTWD=1 LOGIC=POS LEVEL=1
.end
";
    let off = 1e6 / (1e6 + 100.0);
    let on = 10.0 / 110.0;
    let q_on = 20.0 / 110.0;
    let checks = ["a", "b", "q", "r"]
        .into_iter()
        .flat_map(|node| {
            [
                (0.0, off),
                (2.0, if matches!(node, "a" | "b") { on } else { q_on }),
                (8.0, off),
            ]
            .map(|(time, voltage)| (node.to_owned(), time, voltage))
        })
        .collect::<Vec<_>>();
    verify(runtime, source, &checks)
}

// Vendor extrema from the 1 ns sampled SIMetrix 9.40d trace of a
// self-controlled RC relaxation oscillator. The switch's branch current
// determines its own future threshold crossings.
fn timed_switch_closed_loop(runtime: &Path) -> Result<()> {
    let source = "Vbias p 0 PWL(0 0 1n 5 10u 5)
Rcharge p c 1k
Cstore c 0 1n
S1 c 0 c 0 swmod IC=0
.model swmod VSWITCH(RON=100 ROFF=1Meg VT=2.5 VH=0.5 TON=100n TOFF=200n TD_ON=50n TD_OFF=30n TIMETOL=10p)
.options reltol=1e-6 abstol=1e-13
.end
";
    let stdout = run_native(
        runtime,
        source,
        "tran 1n 10u 0 1n\nmeas tran check0 max v(c) from=1.8u to=2u\nmeas tran check1 max_at v(c) from=1.8u to=2u\nmeas tran check2 min v(c) from=2u to=2.2u\nmeas tran check3 min_at v(c) from=2u to=2.2u\nmeas tran check4 max v(c) from=9u to=9.2u\nmeas tran check5 max_at v(c) from=9u to=9.2u\nmeas tran check6 min v(c) from=9.2u to=9.4u\nmeas tran check7 min_at v(c) from=9.2u to=9.4u\n",
    )?;
    let values = measurements(&stdout);
    for (index, expected) in [
        3.199_790_93,
        1.929e-6,
        1.285_854_69,
        2.078e-6,
        3.199_789_77,
        9.12e-6,
        1.285_860_32,
        9.269e-6,
    ]
    .into_iter()
    .enumerate()
    {
        let actual = *values
            .get(&index)
            .with_context(|| format!("missing oscillator measurement {index}: {stdout}"))?;
        let tolerance = if index % 2 == 0 { 1e-4 } else { 2e-9 };
        ensure!(
            (actual - expected).abs() <= tolerance,
            "oscillator {index}: {actual} != {expected}"
        );
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
    threshold_plateau_and_latch_cancellation(&runtime)?;
    switch_startup_and_load_lines(&runtime)?;
    switch_named_voltage_branch(&runtime)?;
    timed_switch_vendor_waveforms(&runtime)?;
    timed_switch_runtime_parameters(&runtime)?;
    timed_switch_small_signal_and_stiff_load(&runtime)?;
    timed_switch_closed_loop(&runtime)?;
    println!("All native ngspice logic regressions passed");
    Ok(())
}
