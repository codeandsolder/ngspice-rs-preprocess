use ParameterValue::{Flag, Integer, Real};
use model_compact_intrinsic::{
    BoundParameters, BsimSoiChannelBias, BsimSoiInstanceInput, ModelVariant, ParameterScope,
    ParameterValue, PreparedBsimSoiSetup, prepare_bsim_soi_model, prepare_bsim_soi_setup,
};
type TestResult = Result<(), Box<dyn std::error::Error>>;
const CARD: &str = "tsi=3e-8 tbox=1e-7 nch=1.7e17 nsub=6e16 ngate=1e20 k1=.53 k2=-.02 vfb=-.8 tox=3e-9 toxqm=2e-9 toxref=3e-9 igcmod=1 aigc=.012 bigc=.002 cigc=.12 aigsd=.013 bigsd=.003 cigsd=.14 dlcig=1e-8 poxedge=1.1 rth0=.005 cth0=1e-6 wth0=2e-7 rshg=100 xgw=1e-7 xgl=2e-8 ngcon=1 xrcrg1=8 xrcrg2=1.2 rbody=1e-5 rhalo=1e3 rbsh=120 rbdb=300 rbsb=500 gbmin=1e-9 isdif=1e4 iddif=2e4";
fn setup(
    pmos: bool,
    heat: bool,
    soi: i32,
    gate_mode: i32,
    body_mode: i32,
    independent_junctions: bool,
) -> Result<PreparedBsimSoiSetup, Box<dyn std::error::Error>> {
    let card = CARD
        .split_whitespace()
        .chain(
            independent_junctions
                .then_some(["isbjt=0", "idbjt=0"])
                .into_iter()
                .flatten(),
        )
        .map(|part| -> Result<_, Box<dyn std::error::Error>> {
            let (name, value) = part.split_once('=').ok_or("invalid card")?;
            Ok((
                name,
                if name == "igcmod" {
                    Integer(value.parse()?)
                } else {
                    Real(value.parse()?)
                },
            ))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let sign = if pmos { -1. } else { 1. };
    let model = prepare_bsim_soi_model(
        &BoundParameters::bind(
            ModelVariant::BsimSoi,
            ParameterScope::Model,
            card.into_iter().chain([
                ("pmos", Flag(pmos)),
                ("soimod", Integer(soi)),
                ("fdmod", Integer(1)),
                ("shmod", Integer(i32::from(heat))),
                ("rgatemod", Integer(gate_mode)),
                ("rbodymod", Integer(body_mode)),
                ("vth0", Real(sign * 0.55)),
            ]),
        )?,
        300.15,
    )?;
    let raw = BoundParameters::bind(
        ModelVariant::BsimSoi,
        ParameterScope::Instance,
        [
            ("tnodeout", Flag(true)),
            ("l", Real(5e-7)),
            ("w", Real(4e-6)),
            ("nf", Real(2.)),
            ("nseg", Real(2.)),
            ("m", Real(2.3)),
            ("nrb", Real(1.2)),
            ("pd", Real(4e-6)),
            ("ps", Real(4e-6)),
        ],
    )?;
    Ok(prepare_bsim_soi_setup(
        &model,
        &[BsimSoiInstanceInput {
            parameters: &raw,
            terminal_count: 7,
            geometry_scale: 1.,
        }],
    )?)
}
fn near(actual: f64, expected: f64, tol: f64, floor: f64) {
    assert!(
        (actual - expected).abs() <= tol * actual.abs().max(expected.abs()) + floor,
        "actual={actual:e}, expected={expected:e}"
    );
}
#[test]
fn parasitic_derivatives() -> TestResult {
    for pmos in [false, true] {
        for heat in [false, true] {
            for soi in 0..=2 {
                for mode in 0..=3 {
                    let setup = setup(pmos, heat, soi, mode, 1, false)?;
                    let law = setup.prepare_parasitics(0, 300.15)?;
                    let sign = if pmos { -1. } else { 1. };
                    for (g, d, b, e) in [
                        (0.3, 0.1, -0.2, 0.1),
                        (1.2, 0.6, -0.2, 0.1),
                        (1.8, -0.5, 0.1, -0.2),
                        (1.2, 0., -0.1, 0.1),
                    ] {
                        let bias = BsimSoiChannelBias {
                            gate: sign * g,
                            drain: sign * d,
                            body: sign * b,
                            backgate: sign * e,
                            kelvin: 325.15,
                        };
                        let point = law.evaluate(bias)?;
                        let values = |p: model_compact_intrinsic::BsimSoiParasiticPoint| {
                            [
                                p.gate_relaxation,
                                p.heat.generated,
                                p.heat.residual,
                                p.heat.energy,
                            ]
                        };
                        let outputs = values(point);
                        let drain = setup.prepare_drain_channel(0, 300.15)?.evaluate(bias)?;
                        near(
                            point.heat.generated.value,
                            if heat {
                                drain.current.value * bias.drain
                            } else {
                                0.
                            },
                            1e-14,
                            1e-24,
                        );
                        if mode <= 1 {
                            assert_eq!(point.gate_relaxation.value, 0.);
                        }
                        if soi == 2 {
                            assert_eq!(point.body_contact, 0.);
                            assert_eq!(point.drain_body, 0.);
                            assert_eq!(point.source_body, 0.);
                        }
                        for axis in 0..5 {
                            if axis == 1 && d == 0. {
                                continue;
                            } // One-sided orientation at zero VDS.
                            let h = if axis == 4 { 1e-3 } else { 1e-5 };
                            let perturb = |offset| {
                                let mut p = bias;
                                match axis {
                                    0 => p.gate += offset,
                                    1 => p.drain += offset,
                                    2 => p.body += offset,
                                    3 => p.backgate += offset,
                                    _ => p.kelvin += offset,
                                }
                                p
                            };
                            let hi = values(law.evaluate(perturb(h))?);
                            let lo = values(law.evaluate(perturb(-h))?);
                            for ((exact, a), b) in outputs.into_iter().zip(hi).zip(lo) {
                                let numerical = (a.value - b.value) / (2. * h);
                                let floor = 64. * a.value.abs().max(b.value.abs()) * f64::EPSILON
                                    / h
                                    + 1e-20;
                                near(exact.gradient[axis], numerical, 2e-5, floor);
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires local ngspice 47 binary"]
fn native_parasitic_heat_and_gate() -> TestResult {
    use std::{fs, process::Command};
    for pmos in [false, true] {
        for soi in 0..=2 {
            for mode in 1..=3 {
                let setup = setup(pmos, true, soi, mode, 0, false)?;
                let law = setup.prepare_parasitics(0, 300.15)?;
                let sign = if pmos { -1. } else { 1. };
                let polarity = if pmos { "pmos" } else { "nmos" };
                let threshold = sign * 0.55;
                for (g, d, b, e) in [(1.2, 0.6, -0.2, 0.1), (1.8, -0.5, 0.1, -0.2)] {
                    let (gate, drain, body, backgate) = (sign * g, sign * d, sign * b, sign * e);
                    let middle = if mode == 3 { "v(m1#midgate)" } else { "v(g)" };
                    let deck = format!(
                        "BSIMSOI parasitic reference\nVd d 0 {drain}\nVg g 0 {gate}\nVb b 0 {body}\nVe e 0 {backgate}\nVp p 0 {body}\nVt t 0 25\nM1 d g 0 e p b t soi tnodeout l=.5u w=4u nf=2 nseg=2 m=2.3 nrb=1.2 pd=4u ps=4u\n.model soi {polarity} level=10 soimod={soi} fdmod=1 shmod=1 rgatemod={mode} rbodymod=0 vth0={threshold}\n+ {CARD}\n.temp 27\n.options gmin=1e-18 reltol=1e-10 abstol=1e-18 vntol=1e-12\n.control\nset numdgt=17\nset wr_singlescale\nsave all\nop\nwrdata out.dat v(m1#gate) {middle} i(vg) i(vt)\nquit\n.endc\n.end\n"
                    );
                    let dir = tempfile::tempdir()?;
                    fs::write(dir.path().join("test.cir"), deck)?;
                    let output = Command::new("ngspice")
                        .args(["-b", "-n", "test.cir"])
                        .current_dir(dir.path())
                        .output()?;
                    assert!(
                        output.status.success(),
                        "{}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                    let native = fs::read_to_string(dir.path().join("out.dat"))?
                        .split_whitespace()
                        .map(str::parse::<f64>)
                        .collect::<Result<Vec<_>, _>>()?;
                    assert_eq!(native.len(), 5);
                    let point = law.evaluate(BsimSoiChannelBias {
                        gate: native[1],
                        drain,
                        body,
                        backgate,
                        kelvin: 325.15,
                    })?;
                    let terminal_current = -native[3];
                    near(point.heat.residual.value, -native[4], 3e-6, 1e-12);
                    if mode == 1 {
                        near(
                            point.gate_electrode * (gate - native[1]),
                            terminal_current,
                            1e-5,
                            1e-12,
                        );
                    } else {
                        near(
                            point.gate_relaxation.value * (native[2] - native[1]),
                            terminal_current,
                            1e-5,
                            1e-12,
                        );
                    }
                    if mode == 3 {
                        near(
                            point.gate_electrode * (gate - native[2]),
                            terminal_current,
                            1e-5,
                            1e-12,
                        );
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires local ngspice 47 binary"]
fn native_body_resistance() -> TestResult {
    use std::{fs, process::Command};
    for pmos in [false, true] {
        for heat in [false, true] {
            for distributed in [0, 1] {
                let prepared = setup(pmos, heat, 0, 0, distributed, true)?;
                let law = prepared.prepare_parasitics(0, 300.15)?;
                let junction = prepared.prepare_junctions(0, 300.15)?;
                let sign = if pmos { -1. } else { 1. };
                let polarity = if pmos { "pmos" } else { "nmos" };
                let threshold = sign * 0.55;
                let thermal = i32::from(heat);
                for (g, d, b, e) in [(1.2, 0.6, 0.05, 0.1), (1.8, -0.5, -0.7, -0.2)] {
                    let (gate, drain, body, backgate) = (sign * g, sign * d, sign * b, sign * e);
                    let contact = body - sign * 0.1;
                    let sides = if distributed == 1 {
                        "v(m1#sbody) v(m1#dbody)"
                    } else {
                        "v(b) v(b)"
                    };
                    let deck = format!(
                        "BSIMSOI body resistance reference\nVd d 0 {drain}\nVg g 0 {gate}\nVb b 0 {body}\nVe e 0 {backgate}\nVp p 0 {contact}\nVt t 0 25\nM1 d g 0 e p b t soi tnodeout l=.5u w=4u nf=2 nseg=2 m=2.3 nrb=1.2 pd=4u ps=4u\n.model soi {polarity} level=10 soimod=0 fdmod=1 shmod={thermal} rgatemod=0 rbodymod={distributed} vth0={threshold}\n+ {CARD} isbjt=0 idbjt=0\n.temp 27\n.options gmin=1e-18 reltol=1e-10 abstol=1e-18 vntol=1e-12\n.control\nset numdgt=17\nset wr_singlescale\nsave all\nop\nwrdata out.dat {sides} i(vp) @m1[ibs] @m1[ibd]\nquit\n.endc\n.end\n"
                    );
                    let dir = tempfile::tempdir()?;
                    fs::write(dir.path().join("test.cir"), deck)?;
                    let output = Command::new("ngspice")
                        .args(["-b", "-n", "test.cir"])
                        .current_dir(dir.path())
                        .output()?;
                    assert!(
                        output.status.success(),
                        "{}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                    let diagnostics = String::from_utf8_lossy(&output.stderr);
                    assert!(
                        !diagnostics.contains("failed") && !diagnostics.contains("singular matrix"),
                        "{diagnostics}"
                    );
                    let native = fs::read_to_string(dir.path().join("out.dat"))?
                        .split_whitespace()
                        .map(str::parse::<f64>)
                        .collect::<Result<Vec<_>, _>>()?;
                    assert_eq!(native.len(), 6);
                    let point = law.evaluate(BsimSoiChannelBias {
                        gate,
                        drain,
                        body,
                        backgate,
                        kelvin: 325.15,
                    })?;
                    near(
                        point.body_contact * (body - contact),
                        native[3],
                        3e-6,
                        1e-14,
                    );
                    if distributed == 1 {
                        let j = junction.evaluate(325.15, native[1], native[2] - drain)?;
                        let source = j
                            .source
                            .independent_current()
                            .plus(j.neutral_body.source_recombination);
                        let drain_current = j
                            .drain
                            .independent_current()
                            .plus(j.neutral_body.drain_recombination);
                        // Native line850 zeros GGjdb/GGjsb in DC, while
                        // Isbsp/Idbdp retain the equivalent source (I-Jx).
                        // The independent diode currents have no cross-junction
                        // terms with ISBJT=IDBJT=0. For the normalized drain
                        // branch, Gjdd=-Gjdb, even with distributed body nodes.
                        near(source.current, sign * native[4], 1e-5, 1e-13);
                        near(drain_current.current, sign * native[5], 1e-5, 1e-13);
                        let source_voltage = if d >= 0. {
                            native[1]
                        } else {
                            native[1] + drain
                        };
                        let drain_voltage = if d >= 0. {
                            native[2] - 2. * drain
                        } else {
                            native[2] - drain
                        };
                        for (actual, current, voltage) in [
                            (
                                point.source_body * (body - native[1]),
                                source,
                                source_voltage,
                            ),
                            (
                                point.drain_body * (body - native[2]),
                                drain_current,
                                drain_voltage,
                            ),
                        ] {
                            let equivalent = current.current
                                - current.conductance * voltage
                                - current.d_temperature * 25.;
                            near(actual, equivalent, 1e-5, 1e-13);
                        }
                    }
                }
            }
        }
    }
    Ok(())
}
