use ParameterValue::{Flag, Integer, Real};
use model_compact_intrinsic::{
    BoundParameters, BsimSoiChannelBias, BsimSoiChannelPoint, BsimSoiChannelScalar,
    BsimSoiInstanceInput, ModelVariant, ParameterScope, ParameterValue, PreparedBsimSoiChannel,
    prepare_bsim_soi_model, prepare_bsim_soi_setup,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;
const CARD: &str = "tsi=3e-8 tbox=1e-7 nch=1.7e17 nsub=6e16 ngate=1e20 k1=.53 k2=-.02 vfb=-.8 dvt0=1.8 dvt1=.5 dvt2=-.04 dvt0w=.2 dvt1w=5e6 dvt2w=-.03 dvtp0=2e-8 dvtp1=.4 dvtp2=1e-4 dvtp3=.1 dvtp4=.3 cdsc=2e-4 cdscb=1e-5 cdscd=1e-5 etab=-.02 dvbd0=.2 dvbd1=.4 k1b=1 k2b=.1 dk2b=.6 cdsbs=1e-5 vsce=.03 lpe0=1e-8 lpeb=2e-8 minv=.1 voff=-.07";

fn prepare(
    pmos: bool,
    heat: bool,
    mode: i32,
    fdmod: i32,
    ambient: f64,
) -> Result<PreparedBsimSoiChannel, Box<dyn std::error::Error>> {
    let sign = if pmos { -1. } else { 1. };
    let card: Vec<_> = CARD
        .split_whitespace()
        .map(|part| -> Result<_, Box<dyn std::error::Error>> {
            let (name, value) = part.split_once('=').ok_or("invalid card")?;
            Ok((name, Real(value.parse()?)))
        })
        .collect::<Result<_, _>>()?;
    let model = BoundParameters::bind(
        ModelVariant::BsimSoi,
        ParameterScope::Model,
        card.into_iter().chain([
            ("pmos", Flag(pmos)),
            ("soimod", Integer(mode)),
            ("fdmod", Integer(fdmod)),
            ("shmod", Integer(i32::from(heat))),
            ("rth0", Real(0.005)),
            ("vth0", Real(sign * 0.55)),
        ]),
    )?;
    let model = prepare_bsim_soi_model(&model, 300.15)?;
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
        ],
    )?;
    let setup = prepare_bsim_soi_setup(
        &model,
        &[BsimSoiInstanceInput {
            parameters: &raw,
            terminal_count: if mode == 2 { 7 } else { 6 },
            geometry_scale: 1.,
        }],
    )?;
    Ok(setup.prepare_channel(0, ambient)?)
}

fn scalar_values(point: BsimSoiChannelPoint) -> [BsimSoiChannelScalar; 13] {
    [
        point.drain_voltage,
        point.gate_voltage,
        point.gate_drain_voltage,
        point.body_voltage,
        point.body_mos,
        point.body_limited,
        point.body_effective,
        point.surface_potential,
        point.depletion_width,
        point.threshold,
        point.subthreshold_factor,
        point.effective_overdrive,
        point.noise_carrier_density,
    ]
}

fn perturb(mut bias: BsimSoiChannelBias, index: usize, delta: f64) -> BsimSoiChannelBias {
    match index {
        0 => bias.gate += delta,
        1 => bias.drain += delta,
        2 => bias.body += delta,
        3 => bias.backgate += delta,
        _ => bias.kelvin += delta,
    }
    bias
}

#[test]
fn physical_channel_bias_derivatives() -> TestResult {
    for pmos in [false, true] {
        let sign = if pmos { -1. } else { 1. };
        for heat in [false, true] {
            for mode in 0..=2 {
                for fdmod in 0..=1 {
                    let law = prepare(pmos, heat, mode, fdmod, 300.15)?;
                    for t in [250., 340.] {
                        for (gate, drain, body, back) in [
                            (-0.5, 0.1, -0.2, -0.4),
                            (0.6, 0.8, -0.1, 0.6),
                            (1.4, -0.3, 0.1, -0.2),
                            (1.5, 0.7, 0.4, 0.8),
                        ] {
                            let bias = BsimSoiChannelBias {
                                gate: sign * gate,
                                drain: sign * drain,
                                body: sign * body,
                                backgate: sign * back,
                                kelvin: t,
                            };
                            let point = scalar_values(law.evaluate(bias)?);
                            for axis in 0..5 {
                                let h = if axis == 4 { 1e-3 } else { 1e-5 };
                                let high = scalar_values(law.evaluate(perturb(bias, axis, h))?);
                                let low = scalar_values(law.evaluate(perturb(bias, axis, -h))?);
                                for (i, ((a, hi), lo)) in
                                    point.iter().zip(high).zip(low).enumerate()
                                {
                                    let diff = (hi.value - lo.value) / (2. * h);
                                    let expected = a.gradient[axis];
                                    let scale = (hi.value.abs().max(lo.value.abs()) * f64::EPSILON
                                        / h)
                                        .max(1e-25);
                                    assert!(
                                        (expected - diff).abs()
                                            <= 2e-5 * expected.abs().max(diff.abs()) + 64. * scale,
                                        "pmos={pmos} heat={heat} mode={mode} fdmod={fdmod} {bias:?} field={i} axis={axis}: {expected:e} vs {diff:e}"
                                    );
                                }
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
fn native_channel_threshold() -> TestResult {
    use std::{fs, process::Command};
    for pmos in [false, true] {
        let sign = if pmos { -1. } else { 1. };
        let polarity = if pmos { "pmos" } else { "nmos" };
        for heat in [false, true] {
            let shmod = i32::from(heat);
            for mode in 0..=2 {
                for fdmod in 0..=1 {
                    let law = prepare(pmos, heat, mode, fdmod, 300.15)?;
                    for (gate, drain, body, back) in [
                        (0.1, 0.1, -0.2, -0.4),
                        (0.6, 0.8, -0.1, 0.6),
                        (1.4, -0.3, 0.1, -0.2),
                        (1.5, 0.7, 0.4, 0.8),
                    ] {
                        let vg = sign * gate;
                        let vd = sign * drain;
                        let vb = sign * body;
                        let ve = sign * back;
                        let rise = if heat { 15. } else { 0. };
                        let vth0 = sign * 0.55;
                        let nodes = if mode == 2 {
                            "d g 0 e b 0 t"
                        } else {
                            "d g 0 e b t"
                        };
                        let deck = format!(
                            "BSIMSOI channel threshold reference\nVd d 0 {vd}\nVg g 0 {vg}\nVb b 0 {vb}\nVe e 0 {ve}\nVt t 0 {rise}\nM1 {nodes} soi tnodeout l=.5u w=4u nf=2 nseg=2 m=2.3 pd=4u ps=4u\n.model soi {polarity} level=10 soimod={mode} fdmod={fdmod} shmod={shmod} rth0=.005 vth0={vth0}\n+ {CARD}\n.temp 27\n.options gmin=1e-18 reltol=1e-10 abstol=1e-18 vntol=1e-12\n.control\nset numdgt=17\nset wr_singlescale\nop\nwrdata out.dat @m1[vth]\nquit\n.endc\n.end\n"
                        );
                        let dir = tempfile::tempdir()?;
                        fs::write(dir.path().join("test.cir"), deck)?;
                        let output = Command::new("ngspice")
                            .args(["-b", "test.cir"])
                            .current_dir(dir.path())
                            .output()?;
                        if !output.status.success() {
                            return Err(format!(
                                "ngspice: {}",
                                String::from_utf8_lossy(&output.stderr)
                            )
                            .into());
                        }
                        let data = fs::read_to_string(dir.path().join("out.dat"))?;
                        let reference: f64 = data
                            .split_whitespace()
                            .nth(1)
                            .ok_or("missing threshold")?
                            .parse::<f64>()?
                            / 2.3;
                        let bias = BsimSoiChannelBias {
                            gate: vg,
                            drain: vd,
                            body: vb,
                            backgate: ve,
                            kelvin: 300.15 + rise,
                        };
                        let actual = law.evaluate(bias)?.threshold.value;
                        assert!(
                            (actual - reference).abs() <= 3e-7 * reference.abs() + 1e-8,
                            "pmos={pmos} heat={heat} mode={mode} fdmod={fdmod} {bias:?}: {actual:e} vs {reference:e}"
                        );
                    }
                }
            }
        }
    }
    Ok(())
}
