use ParameterValue::{Flag, Integer, Real};
use model_compact_intrinsic::{
    BoundParameters, BsimSoiInstanceInput, BsimSoiSeriesBias, BsimSoiSeriesParameters,
    ModelVariant, ParameterScope, ParameterValue, PreparedBsimSoiSetup, prepare_bsim_soi_model,
    prepare_bsim_soi_setup,
};
type TestResult = Result<(), Box<dyn std::error::Error>>;

const TRANSPORT: &str = "u0=520 ute=-1.35 vsat=95000 at=17000 ua=1e-9 ua1=2e-10 ub=3e-19 ub1=4e-20 uc=-1e-11 uc1=5e-12 rdsw=180 rsw=200 rdw=120 rswmin=30 rdwmin=40 prwg=.4 prwb=.2 prt=30 wr=.8 ngate=1e20 rsh=50";
const STRESS: &str = "ku0=1e-7 kvsat=.4 tku0=.3 kvth0=1e-8 stk2=2e-9 steta0=3e-9 saref=1e-6 sbref=2e-6 wlod=1e-7 llodku0=.2 wlodku0=.3 lku0=.005 wku0=.002 pku0=1e-5 llodvth=.1 wlodvth=.2 lkvth0=.01 wkvth0=.003 pkvth0=.0001 lodk2=.7 lodeta0=.8";

fn card(text: &str) -> Result<Vec<(&str, ParameterValue)>, Box<dyn std::error::Error>> {
    text.split_whitespace()
        .map(|token| {
            let (name, value) = token.split_once('=').ok_or("bad card")?;
            Ok((name, Real(value.parse()?)))
        })
        .collect()
}

fn setup(
    pmos: bool,
    heat: bool,
    external: bool,
    layout: bool,
    nf: f64,
    extra: &[(&str, ParameterValue)],
) -> Result<PreparedBsimSoiSetup, Box<dyn std::error::Error>> {
    let model = BoundParameters::bind(
        ModelVariant::BsimSoi,
        ParameterScope::Model,
        card(TRANSPORT)?
            .into_iter()
            .chain(card(STRESS)?)
            .chain([
                ("pmos", Flag(pmos)),
                ("shmod", Integer(i32::from(heat))),
                ("rdsmod", Integer(i32::from(external))),
                ("rth0", Real(0.005)),
                ("cth0", Real(1e-6)),
            ])
            .chain(extra.iter().cloned()),
    )?;
    let model = prepare_bsim_soi_model(&model, 300.15)?;
    let raw = BoundParameters::bind(
        ModelVariant::BsimSoi,
        ParameterScope::Instance,
        [
            ("tnodeout", Flag(true)),
            ("l", Real(5e-7)),
            ("w", Real(4e-6)),
            ("nf", Real(nf)),
            ("nseg", Real(2.)),
            ("m", Real(2.3)),
            ("nrd", Real(2.)),
            ("nrs", Real(3.)),
            ("sa", Real(if layout { 0.3e-6 } else { 0. })),
            ("sb", Real(0.7e-6)),
            ("sd", Real(0.2e-6)),
        ],
    )?;
    Ok(prepare_bsim_soi_setup(
        &model,
        &[BsimSoiInstanceInput {
            parameters: &raw,
            terminal_count: 6,
            geometry_scale: 1.,
        }],
    )?)
}

fn close(actual: f64, reference: f64, tolerance: f64, absolute: f64) {
    assert!(
        (actual - reference).abs() <= tolerance * actual.abs().max(reference.abs()) + absolute,
        "{actual:e} vs {reference:e}"
    );
}

#[test]
fn transport_derivatives_and_limits() -> TestResult {
    for pmos in [false, true] {
        for heat in [false, true] {
            for external in [false, true] {
                for layout in [false, true] {
                    for nf in [1., 2., 3.5] {
                        let setup = setup(pmos, heat, external, layout, nf, &[])?;
                        let law = setup.prepare_transport(0, 300.15)?;
                        for t in [240., 300.15, 380.] {
                            let h = 1e-3;
                            let point = law.evaluate(t)?;
                            let plus = law.evaluate(t + h)?;
                            let minus = law.evaluate(t - h)?;
                            for (a, b, c) in [
                                (point.mobility, plus.mobility, minus.mobility),
                                (
                                    point.saturation_velocity,
                                    plus.saturation_velocity,
                                    minus.saturation_velocity,
                                ),
                                (point.ua, plus.ua, minus.ua),
                                (point.ub, plus.ub, minus.ub),
                                (point.uc, plus.uc, minus.uc),
                            ] {
                                close(a.d_temperature, (b.value - c.value) / (2. * h), 2e-6, 1e-28);
                            }
                            if external {
                                for (g, b) in [(-0.4, -0.2), (0.4, 0.3), (1.5, -0.5)] {
                                    let bias = BsimSoiSeriesBias { gate: g, body: b };
                                    let eval = |temp, gate, body| {
                                        law.external_series(
                                            temp,
                                            BsimSoiSeriesBias { gate, body },
                                            BsimSoiSeriesBias { gate, body },
                                        )
                                    };
                                    let p = law.external_series(t, bias, bias)?;
                                    let gp = eval(t, g + 1e-4, b)?;
                                    let gm = eval(t, g - 1e-4, b)?;
                                    let bp = eval(t, g, b + 1e-4)?;
                                    let bm = eval(t, g, b - 1e-4)?;
                                    let tp = eval(t + h, g, b)?;
                                    let tm = eval(t - h, g, b)?;
                                    for (a, c, d, e, f, j, k) in [
                                        (
                                            p.source, gp.source, gm.source, bp.source, bm.source,
                                            tp.source, tm.source,
                                        ),
                                        (
                                            p.drain, gp.drain, gm.drain, bp.drain, bm.drain,
                                            tp.drain, tm.drain,
                                        ),
                                    ] {
                                        close(
                                            a.d_gate,
                                            (c.conductance - d.conductance) / 2e-4,
                                            2e-6,
                                            1e-13,
                                        );
                                        close(
                                            a.d_body,
                                            (e.conductance - f.conductance) / 2e-4,
                                            2e-6,
                                            1e-13,
                                        );
                                        close(
                                            a.d_temperature,
                                            (j.conductance - k.conductance) / (2. * h),
                                            2e-6,
                                            1e-13,
                                        );
                                    }
                                }
                            } else {
                                for (g, b) in [(0.2, -0.2), (-3., -0.5), (1.5, 0.2)] {
                                    let p = law.internal_resistance(t, g, b)?;
                                    close(
                                        p.d_overdrive,
                                        (law.internal_resistance(t, g + 1e-5, b)?.resistance
                                            - law.internal_resistance(t, g - 1e-5, b)?.resistance)
                                            / 2e-5,
                                        2e-6,
                                        1e-9,
                                    );
                                    close(
                                        p.d_root_potential,
                                        (law.internal_resistance(t, g, b + 1e-5)?.resistance
                                            - law.internal_resistance(t, g, b - 1e-5)?.resistance)
                                            / 2e-5,
                                        2e-6,
                                        1e-9,
                                    );
                                    close(
                                        p.d_temperature,
                                        (law.internal_resistance(t + h, g, b)?.resistance
                                            - law.internal_resistance(t - h, g, b)?.resistance)
                                            / (2. * h),
                                        2e-6,
                                        1e-9,
                                    );
                                }
                            }
                        }
                        if !layout {
                            assert_eq!(law.stress_offsets(), Default::default());
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn native(deck: &str) -> Result<Vec<f64>, Box<dyn std::error::Error>> {
    use std::{fs, process::Command};
    let dir = tempfile::tempdir()?;
    fs::write(dir.path().join("deck.cir"), deck)?;
    let output = Command::new("ngspice")
        .current_dir(dir.path())
        .args(["-b", "-n", "deck.cir"])
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(fs::read_to_string(dir.path().join("out.dat"))?
        .split_whitespace()
        .map(str::parse)
        .collect::<Result<_, _>>()?)
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires local ngspice 47 binary"]
fn native_external_access_conductance() -> TestResult {
    for pmos in [false, true] {
        let polarity = if pmos { "pmos" } else { "nmos" };
        let sign = if pmos { -1. } else { 1. };
        for heat in [false, true] {
            for mode in [false, true] {
                let setup = setup(pmos, heat, mode, false, 2., &[])?;
                for temp in [-40., 85.] {
                    let ambient = temp + 273.15;
                    let law = setup.prepare_transport(0, ambient)?;
                    for drain in [-0.6, 0.6] {
                        let vd = sign * drain;
                        let vg = sign * 1.2;
                        let vb = sign * -0.1;
                        let rise = if heat { 10. } else { 0. };
                        let shmod = i32::from(heat);
                        let rdsmod = i32::from(mode);
                        let deck = format!(
                            "BSIMSOI series reference\nVd d 0 {vd}\nVg g 0 {vg}\nVe e 0 0\nVb b 0 {vb}\nVs s 0 0\nVt t 0 {rise}\nM1 d g s e b t soi tnodeout l=.5u w=4u nf=2 nseg=2 m=2.3 nrd=2 nrs=3 pd=4u ps=4u\n.model soi {polarity} level=10 soimod=0 shmod={shmod} rdsmod={rdsmod} rth0=.005 cth0=1e-6\n+ {TRANSPORT}\n.temp {temp}\n.options gmin=1e-18 reltol=1e-10 abstol=1e-18 vntol=1e-12\n.control\nset numdgt=17\nset wr_singlescale\nop\nwrdata out.dat @m1[vgs] @m1[vds] @m1[vbs] i(vd) i(vs)\nquit\n.endc\n.end\n"
                        );
                        let values = native(&deck)?;
                        assert_eq!(values.len(), 6);
                        let source = vg - sign * values[1];
                        let drain = source + sign * values[2];
                        let measured_source = values[5] / source;
                        let measured_drain = -values[4] / (vd - drain);
                        let point = law.external_series(
                            ambient + rise,
                            BsimSoiSeriesBias {
                                gate: vg - source,
                                body: vb - source,
                            },
                            BsimSoiSeriesBias {
                                gate: vg - drain,
                                body: vb - drain,
                            },
                        )?;
                        close(point.source.conductance, measured_source, 2e-5, 1e-10);
                        close(point.drain.conductance, measured_drain, 2e-5, 1e-10);
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
fn native_layout_and_transport_equivalent_card() -> TestResult {
    // Compare original temperature- and layout-dependent native model against
    // a native card with the Rust-resolved effective coefficients explicitly
    // substituted. Both circuits retain identical material temperature laws.
    for pmos in [false, true] {
        let sign = if pmos { -1. } else { 1. };
        let polarity = if pmos { "pmos" } else { "nmos" };
        for heat in [false, true] {
            let shmod = i32::from(heat);
            let rise = if heat { 15. } else { 0. };
            for nf in [1., 3.] {
                let vth = sign * 0.55;
                let setup = setup(
                    pmos,
                    heat,
                    false,
                    true,
                    nf,
                    &[
                        ("vth0", Real(vth)),
                        ("k1", Real(0.5)),
                        ("k2", Real(-0.02)),
                        ("eta0", Real(0.08)),
                        ("vfb", Real(-0.8)),
                    ],
                )?;
                for temp in [-30., 75.] {
                    let ambient = temp + 273.15;
                    let law = setup.prepare_transport(0, ambient)?;
                    let p = law.evaluate(ambient + rise)?;
                    let offsets = law.stress_offsets();
                    let BsimSoiSeriesParameters::Internal { resistance } = p.series else {
                        return Err("wrong series".into());
                    };
                    let width = setup.sizes()[0].geometry().weff;
                    let rdsw = resistance.value * (width * 1e6).powf(0.8);
                    let (u0, vsat, ua, ub, uc) = (
                        p.mobility.value,
                        p.saturation_velocity.value,
                        p.ua.value,
                        p.ub.value,
                        p.uc.value,
                    );
                    let threshold = vth + offsets.threshold;
                    let k2 = -0.02 + offsets.body_coefficient;
                    let eta0 = 0.08 + offsets.dibl;
                    let resolved = format!(
                        "u0={u0:e} ute=0 vsat={vsat:e} at=0 ua={ua:e} ua1=0 ub={ub:e} ub1=0 uc={uc:e} uc1=0 rdsw={rdsw:e} prt=0 vth0={threshold:e} k2={k2:e} eta0={eta0:e}"
                    );
                    for (gate, drain) in [(0.8, 0.05), (1.2, 0.7), (1.5, -0.2)] {
                        let vg = sign * gate;
                        let vd = sign * drain;
                        let vb = sign * -0.15;
                        let mut outputs = Vec::new();
                        for original in [true, false] {
                            let (layout, card) = if original {
                                ("sa=.3u sb=.7u sd=.2u", STRESS.to_owned())
                            } else {
                                ("", resolved.clone())
                            };
                            let deck = format!(
                                "BSIMSOI stress equivalence\nVd d 0 {vd}\nVg g 0 {vg}\nVe e 0 0\nVb b 0 {vb}\nVt t 0 {rise}\nM1 d g 0 e b t soi tnodeout l=.5u w=4u nf={nf} nseg=2 m=2.3 pd=4u ps=4u {layout}\n.model soi {polarity} level=10 soimod=0 shmod={shmod} rth0=.005 cth0=1e-6 vth0={vth:e} k1=.5 k2=-.02 eta0=.08 vfb=-.8\n+ {TRANSPORT}\n+ {card}\n.temp {temp}\n.options gmin=1e-18 reltol=1e-10 abstol=1e-18 vntol=1e-12\n.control\nset numdgt=17\nset wr_singlescale\nop\nwrdata out.dat @m1[ids] @m1[vth] @m1[vdsat] @m1[gm] @m1[gds]\nquit\n.endc\n.end\n"
                            );
                            outputs.push(native(&deck)?);
                        }
                        assert_eq!(outputs[0].len(), 6);
                        for (&a, &b) in outputs[0][1..].iter().zip(&outputs[1][1..]) {
                            close(a, b, 3e-6, 1e-11);
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

#[test]
fn electrostatic_ambient_and_live_factors() -> TestResult {
    for pmos in [false, true] {
        for heat in [false, true] {
            for material in [0, 1] {
                for explicit in [false, true] {
                    let mut extra = vec![
                        ("mtrlmod", Integer(material)),
                        ("nch", Real(1.7e17)),
                        ("nsub", Real(6e16)),
                        ("xt", Real(1.1e-7)),
                        ("vbm", Real(-3.)),
                        ("k1w1", Real(2e-7)),
                        ("k1w2", Real(1e-7)),
                        ("epsrox", Real(4.2)),
                        ("epsrsub", Real(12.5)),
                        ("eot", Real(8e-9)),
                        ("minv", Real(0.12)),
                        ("minvcv", Real(-0.2)),
                        ("dvtp2", Real(1e-4)),
                        ("dvtp3", Real(0.2)),
                        ("dsub", Real(0.4)),
                        ("drout", Real(0.6)),
                        ("pdiblc1", Real(0.3)),
                        ("pdiblc2", Real(0.015)),
                    ];
                    if explicit {
                        extra.extend([
                            ("k1", Real(0.53)),
                            ("k2", Real(-0.025)),
                            ("vfb", Real(-0.8)),
                        ]);
                    }
                    let prepared = setup(pmos, heat, false, true, 2., &extra)?;
                    for ambient in [243.15, 300.15, 358.15] {
                        let law = prepared.prepare_electrostatics(0, ambient)?;
                        let base = law.base();
                        let material = prepared.prepare_temperature(0, ambient)?;
                        let stress = prepared.prepare_transport(0, ambient)?.stress_offsets();
                        let sign = if pmos { -1. } else { 1. };
                        // The native threshold is an ambient-derived coefficient
                        // (including stress), not a live thermal unknown.
                        let phi = if heat {
                            let ambient_setup = setup(pmos, false, false, true, 2., &extra)?;
                            ambient_setup
                                .prepare_temperature(0, ambient)?
                                .evaluate(ambient)?
                                .surface_potential
                                .value
                        } else {
                            material.evaluate(ambient)?.surface_potential.value
                        };
                        close(
                            sign * (base.threshold - stress.threshold),
                            base.flatband + phi + base.k1_effective * phi.sqrt(),
                            1e-12,
                            1e-15,
                        );
                        if explicit {
                            close(base.k1, 0.53, 1e-14, 0.);
                            close(base.k2, -0.025 + stress.body_coefficient, 1e-14, 0.);
                        }
                        for rise in [-10., 0., 40.] {
                            let t = ambient + rise;
                            let h = 1e-3;
                            let a = law.evaluate(t)?;
                            let b = law.evaluate(t + h)?;
                            let c = law.evaluate(t - h)?;
                            for (a, b, c) in [
                                (a.theta_body, b.theta_body, c.theta_body),
                                (a.theta_output, b.theta_output, c.theta_output),
                            ] {
                                close(a.d_temperature, (b.value - c.value) / (2. * h), 3e-6, 1e-16);
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
fn native_electrostatic_default_equivalence() -> TestResult {
    for pmos in [false, true] {
        let sign = if pmos { -1. } else { 1. };
        let polarity = if pmos { "pmos" } else { "nmos" };
        for heat in [false, true] {
            let shmod = i32::from(heat);
            let rise = if heat { 10. } else { 0. };
            for original_card in [
                "nch=1.7e17 nsub=6e16 xt=1.1e-7 vbm=-3 k1w1=2e-7 k1w2=1e-7",
                "nch=1.7e17 nsub=6e16 k1=.45",
                "nch=1.7e17 nsub=-6e16 gamma2=.18 gamma1=.5 vbx=-.7 vbm=-3 vfb=-.8",
                "gamma1=.5 gamma2=.18 vbx=-.7 vbm=-3",
                "nch=1.7e17 nsub=6e16 vfb=-.8 vth0=.55",
            ] {
                let extra = card(original_card)?;
                let setup = setup(pmos, heat, false, false, 2., &extra)?;
                for temp in [-30., 75.] {
                    let ambient = temp + 273.15;
                    let base = setup.prepare_electrostatics(0, ambient)?.base();
                    let (k1, k2, vfb, vth0) = (base.k1, base.k2, base.flatband, base.threshold);
                    let resolved = format!("k1={k1:e} k2={k2:e} vfb={vfb:e} vth0={vth0:e}");
                    for (g, d, b) in [(0.9, 0.2, -0.15), (1.4, 0.7, -0.3)] {
                        let vg = sign * g;
                        let vd = sign * d;
                        let vb = sign * b;
                        let mut outputs = Vec::new();
                        for correction in ["", resolved.as_str()] {
                            let deck = format!(
                                "BSIMSOI derived coefficient reference\nVd d 0 {vd}\nVg g 0 {vg}\nVe e 0 0\nVb b 0 {vb}\nVt t 0 {rise}\nM1 d g 0 e b t soi tnodeout l=.5u w=4u nf=2 nseg=2 m=2.3 pd=4u ps=4u\n.model soi {polarity} level=10 soimod=0 shmod={shmod} rth0=.005 cth0=1e-6\n+ {TRANSPORT}\n+ {original_card}\n+ {correction}\n.temp {temp}\n.options gmin=1e-18 reltol=1e-10 abstol=1e-18 vntol=1e-12\n.control\nset numdgt=17\nset wr_singlescale\nop\nwrdata out.dat @m1[ids] @m1[vth] @m1[gm] @m1[gds] @m1[gmbs]\nquit\n.endc\n.end\n"
                            );
                            outputs.push(native(&deck)?);
                        }
                        assert_eq!(outputs[0].len(), 6);
                        for (&a, &b) in outputs[0][1..].iter().zip(&outputs[1][1..]) {
                            close(a, b, 2e-6, 1e-12);
                        }
                    }
                }
            }
        }
    }
    Ok(())
}
