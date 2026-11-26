use ParameterValue::{Flag, Integer, Real};
use model_compact_intrinsic::{
    BoundParameters, BsimSoiChannelBias, BsimSoiInstanceInput, BsimSoiNoiseBias, BsimSoiNoisePoint,
    ModelVariant, ParameterScope, ParameterValue, PreparedBsimSoiNoise, prepare_bsim_soi_model,
    prepare_bsim_soi_setup,
};
type TestResult = Result<(), Box<dyn std::error::Error>>;
const CARD: &str = "tsi=3e-8 tbox=1e-7 nch=1.7e17 nsub=6e16 ngate=1e20 k1=.53 k2=-.02 vfb=-.8 tox=3e-9 toxqm=2e-9 toxref=3e-9 aigc=.012 bigc=.002 cigc=.12 aigsd=.013 bigsd=.003 cigsd=.14 nigc=1.2 pigcd=.7 dlcig=1e-8 poxedge=1.1 alphagb1=.35 betagb1=.03 alphagb2=.25 betagb2=.04 rth0=.005 kf=1e-27 af=1.2 ef=.9 bf=1.8 noif=1.4 noia=6e41 noib=3e26 noic=8e9 rnoia=.58 rnoib=.5164 tnoia=1.5 tnoib=3.5 ntnoi=1.1 rdsw=100 isdif=10 iddif=20";
#[derive(Clone, Copy, Debug)]
struct Case {
    pmos: bool,
    heat: bool,
    capmod: i32,
    tnoi: i32,
    fnoi: i32,
    soi: i32,
    reference_width: f64,
    multiplicity: f64,
}
impl Case {
    const fn sign(self) -> f64 {
        if self.pmos { -1. } else { 1. }
    }
    fn law(self) -> Result<PreparedBsimSoiNoise, Box<dyn std::error::Error>> {
        let card = CARD
            .split_whitespace()
            .map(|part| -> Result<_, Box<dyn std::error::Error>> {
                let (name, value) = part.split_once('=').ok_or("invalid card")?;
                Ok((name, Real(value.parse()?)))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let model = prepare_bsim_soi_model(
            &BoundParameters::bind(
                ModelVariant::BsimSoi,
                ParameterScope::Model,
                card.into_iter().chain([
                    ("pmos", Flag(self.pmos)),
                    ("soimod", Integer(self.soi)),
                    ("fdmod", Integer(1)),
                    ("shmod", Integer(i32::from(self.heat))),
                    ("vth0", Real(self.sign() * 0.55)),
                    ("igcmod", Integer(1)),
                    ("igbmod", Integer(1)),
                    ("capmod", Integer(self.capmod)),
                    ("tnoimod", Integer(self.tnoi)),
                    ("fnoimod", Integer(self.fnoi)),
                    ("w0flk", Real(self.reference_width)),
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
                ("m", Real(self.multiplicity)),
                ("pd", Real(4e-6)),
                ("ps", Real(4e-6)),
            ],
        )?;
        let setup = prepare_bsim_soi_setup(
            &model,
            &[BsimSoiInstanceInput {
                parameters: &raw,
                terminal_count: 7,
                geometry_scale: 1.,
            }],
        )?;
        Ok(setup.prepare_noise(0, 300.15)?)
    }
    fn bias(self, g: f64, d: f64, b: f64, e: f64, frequency: f64) -> BsimSoiNoiseBias {
        let s = self.sign();
        BsimSoiNoiseBias {
            channel: BsimSoiChannelBias {
                gate: s * g,
                drain: s * d,
                body: s * b,
                backgate: s * e,
                kelvin: if self.heat { 325.15 } else { 300.15 },
            },
            body_source_junction: s * b,
            body_drain_junction: s * (b - d),
            contact: s * b,
            frequency,
        }
    }
}
fn cases() -> impl Iterator<Item = Case> {
    [false, true].into_iter().flat_map(|pmos| {
        [false, true].into_iter().flat_map(move |heat| {
            (2..=3).flat_map(move |capmod| {
                (0..=2).flat_map(move |tnoi| {
                    (0..=1).flat_map(move |fnoi| {
                        (0..=2).flat_map(move |soi| {
                            [0., 1e-6].into_iter().map(move |reference_width| Case {
                                pmos,
                                heat,
                                capmod,
                                tnoi,
                                fnoi,
                                soi,
                                reference_width,
                                multiplicity: 2.3,
                            })
                        })
                    })
                })
            })
        })
    })
}
fn compare(actual: f64, expected: f64, relative: f64, context: impl std::fmt::Debug) {
    assert!(
        (actual - expected).abs() <= relative * actual.abs().max(expected.abs()) + 1e-37,
        "{context:?}: {actual:e} vs {expected:e}"
    );
}
#[test]
fn noise_scaling_and_frequency() -> TestResult {
    for case in cases() {
        let law = case.law()?;
        let single = Case {
            multiplicity: 1.,
            ..case
        }
        .law()?;
        for (g, d, b, e) in [
            (1.2, 0.6, -0.2, 0.1),
            (1.8, -0.5, -0.7, -0.2),
            (0.3, 0.1, -0.2, 0.1),
        ] {
            let bias = case.bias(g, d, b, e, 1e3);
            let first = law.evaluate(bias)?;
            let second = law.evaluate(BsimSoiNoiseBias {
                frequency: 2e3,
                ..bias
            })?;
            let single = single.evaluate(bias)?;
            for (a, b) in [
                (first.channel_thermal, single.channel_thermal),
                (first.channel_flicker, single.channel_flicker),
                (first.gate_source_shot, single.gate_source_shot),
                (first.gate_drain_shot, single.gate_drain_shot),
                (first.gate_body_shot, single.gate_body_shot),
                (first.source_body_excess, single.source_body_excess),
                (first.drain_body_excess, single.drain_body_excess),
            ] {
                compare(a, case.multiplicity * b, 1e-11, (case, bias));
            }
            compare(
                first.channel_flicker,
                second.channel_flicker * 2_f64.powf(0.9),
                1e-11,
                (case, bias),
            );
            assert_eq!(first.channel_thermal, second.channel_thermal);
            assert!(
                law.evaluate(BsimSoiNoiseBias {
                    frequency: 0.,
                    ..bias
                })
                .is_err()
            );
        }
    }
    Ok(())
}
#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires local ngspice 47 binary"]
fn native_noise_at_clamped_terminals() -> TestResult {
    use std::{fs, process::Command};
    for case in cases() {
        let law = case.law()?;
        let Case {
            pmos,
            heat,
            capmod,
            tnoi,
            fnoi,
            soi,
            reference_width,
            multiplicity,
        } = case;
        let polarity = if pmos { "pmos" } else { "nmos" };
        let shmod = i32::from(heat);
        let threshold = case.sign() * 0.55;
        let rise = if heat { 25. } else { 0. };
        for (g, d, b, e) in [(1.2, 0.6, -0.2, 0.1), (1.8, -0.5, -0.7, -0.2)] {
            let bias = case.bias(g, d, b, e, 1e3);
            let BsimSoiChannelBias {
                gate,
                drain,
                body,
                backgate,
                ..
            } = bias.channel;
            let deck = format!(
                "BSIMSOI source noise reference\nVd d 0 {drain}\nVg g 0 {gate} ac 1\nVb b 0 {body}\nVe e 0 {backgate}\nVp p 0 {body}\nVt t 0 {rise}\nVs s 0 0\nHd od 0 Vd 1\nHs os 0 Vs 1\nHg og 0 Vg 1\nM1 d g s e p b t soi tnodeout l=.5u w=4u nf=2 nseg=2 m={multiplicity} pd=4u ps=4u\n.model soi {polarity} level=10 soimod={soi} fdmod=1 shmod={shmod} vth0={threshold} capmod={capmod} tnoimod={tnoi} fnoimod={fnoi} w0flk={reference_width} igcmod=1 igbmod=1\n+ {CARD}\n.temp 27\n.options gmin=1e-18 reltol=1e-10 abstol=1e-18 vntol=1e-12\n.control\nset numdgt=17\nset sqrnoise\nset wr_singlescale\nnoise v(od) Vg lin 2 1k 2k 1\nsetplot noise1\nwrdata d.dat onoise.m1.id onoise.m1.1overf onoise.m1.fb_ibd onoise.m1.igd\nnoise v(os) Vg lin 2 1k 2k 1\nsetplot noise3\nwrdata s.dat onoise.m1.fb_ibs onoise.m1.igs\nnoise v(og) Vg lin 2 1k 2k 1\nsetplot noise5\nwrdata b.dat onoise.m1.igb\nquit\n.endc\n.end\n"
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
            let read = |name| -> Result<Vec<f64>, Box<dyn std::error::Error>> {
                Ok(fs::read_to_string(dir.path().join(name))?
                    .split_whitespace()
                    .map(str::parse::<f64>)
                    .collect::<Result<_, _>>()?)
            };
            let d = read("d.dat")?;
            let s = read("s.dat")?;
            let b = read("b.dat")?;
            assert_eq!((d.len(), s.len(), b.len()), (10, 6, 4));
            for (index, frequency) in [1e3, 2e3].into_iter().enumerate() {
                let BsimSoiNoisePoint {
                    channel_thermal,
                    channel_flicker,
                    drain_body_excess,
                    gate_drain_shot,
                    source_body_excess,
                    gate_source_shot,
                    gate_body_shot,
                    ..
                } = law.evaluate(BsimSoiNoiseBias { frequency, ..bias })?;
                for (branch, (actual, expected)) in [
                    (channel_thermal, d[5 * index + 1]),
                    (channel_flicker, d[5 * index + 2]),
                    (drain_body_excess, d[5 * index + 3]),
                    (gate_drain_shot, d[5 * index + 4]),
                    (source_body_excess, s[3 * index + 1]),
                    (gate_source_shot, s[3 * index + 2]),
                    (gate_body_shot, b[2 * index + 1]),
                ]
                .into_iter()
                .enumerate()
                {
                    compare(actual, expected, 3e-5, (case, bias, branch, frequency));
                }
            }
        }
    }
    Ok(())
}
