use ParameterValue::{Flag, Integer, Real};
use model_compact_intrinsic::{
    BoundParameters, BsimSoiChannelBias, BsimSoiChannelScalar, BsimSoiGateBias,
    BsimSoiGateCurrents, BsimSoiInstanceInput, ModelVariant, ParameterScope, ParameterValue,
    PreparedBsimSoiGateTunneling, prepare_bsim_soi_model, prepare_bsim_soi_setup,
};
type TestResult = Result<(), Box<dyn std::error::Error>>;
const CARD: &str = "tsi=3e-8 tbox=1e-7 nch=1.7e17 nsub=6e16 ngate=1e20 k1=.53 k2=-.02 vfb=-.8 tox=3e-9 toxqm=2e-9 toxref=3e-9 ntox=1.2 dlcig=1e-8 poxedge=1.1 aigc=.012 bigc=.002 cigc=.12 aigsd=.013 bigsd=.003 cigsd=.14 aigbcp2=.013 bigbcp2=.003 cigbcp2=.14 nigc=1.2 pigcd=.7 alphagb1=.35 betagb1=.03 alphagb2=.25 betagb2=.04 vgb1=5 vgb2=5 vevb=.075 vecb=.026 ebg=1.1 voxh=5 deltavox=.005 dwg=1e-8 dwb=5e-9";
#[derive(Clone, Copy, Debug)]
struct Case {
    pmos: bool,
    heat: bool,
    soi: i32,
    igc: i32,
    igb: i32,
    material: i32,
    contact_area: f64,
}
impl Case {
    const fn sign(self) -> f64 {
        if self.pmos { -1. } else { 1. }
    }
    fn prepare(self) -> Result<PreparedBsimSoiGateTunneling, Box<dyn std::error::Error>> {
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
                    ("rth0", Real(0.005)),
                    ("vth0", Real(self.sign() * 0.55)),
                    ("igcmod", Integer(self.igc)),
                    ("igbmod", Integer(self.igb)),
                    ("mtrlmod", Integer(self.material)),
                    ("eot", Real(3e-9)),
                    ("epsrsub", Real(11.7)),
                    ("phig", Real(4.1)),
                    ("easub", Real(4.05)),
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
                ("psbcp", Real(1e-7)),
                ("pdbcp", Real(2e-7)),
                ("agbcpd", Real(1e-13)),
                ("agbcp2", Real(self.contact_area)),
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
        Ok(setup.prepare_gate_tunneling(0, 300.15)?)
    }
    fn bias(
        self,
        g: f64,
        d: f64,
        b: f64,
        e: f64,
        contact_voltage: f64,
        rise: f64,
    ) -> BsimSoiGateBias {
        let sign = self.sign();
        BsimSoiGateBias {
            channel: BsimSoiChannelBias {
                gate: sign * g,
                drain: sign * d,
                body: sign * b,
                backgate: sign * e,
                kelvin: 300.15 + rise,
            },
            contact: sign * contact_voltage,
        }
    }
}
fn cases() -> impl Iterator<Item = Case> {
    [false, true].into_iter().flat_map(|pmos| {
        [false, true].into_iter().flat_map(move |heat| {
            (0..=2).flat_map(move |soi| {
                [(0, 0), (1, 0), (0, 1), (1, 1)]
                    .into_iter()
                    .flat_map(move |(igc, igb)| {
                        [0, 1].into_iter().flat_map(move |material| {
                            [0., 2e-13].into_iter().map(move |contact_area| Case {
                                pmos,
                                heat,
                                soi,
                                igc,
                                igb,
                                material,
                                contact_area,
                            })
                        })
                    })
            })
        })
    })
}
const fn values(point: &BsimSoiGateCurrents) -> [BsimSoiChannelScalar; 6] {
    [
        point.source_edge,
        point.drain_edge,
        point.source_channel,
        point.drain_channel,
        point.body,
        point.contact,
    ]
}
fn perturb(mut bias: BsimSoiGateBias, axis: usize, h: f64) -> BsimSoiGateBias {
    match axis {
        0 => bias.channel.gate += h,
        1 => bias.channel.drain += h,
        2 => bias.channel.body += h,
        3 => bias.channel.backgate += h,
        4 => bias.channel.kelvin += h,
        _ => bias.contact += h,
    }
    bias
}
#[test]
fn gate_tunneling_gradients() -> TestResult {
    for case in cases() {
        let law = case.prepare()?;
        for (g, d, b, e, p) in [
            (1.2, 0.6, -0.2, 0.1, -0.3),
            (-1.4, 0.3, -0.1, 0.2, -0.4),
            (0.7, -0.4, 0.1, -0.2, 0.3),
            (0.2, 0.7, -0.4, 0.5, 0.9),
        ] {
            let bias = case.bias(g, d, b, e, p, 20.);
            let point = law.evaluate(bias)?;
            for axis in 0..6 {
                let h = if axis == 4 { 1e-3 } else { 1e-5 };
                let hi = values(&law.evaluate(perturb(bias, axis, h))?);
                let lo = values(&law.evaluate(perturb(bias, axis, -h))?);
                for (index, actual) in values(&point).into_iter().enumerate() {
                    let diff = (hi[index].value - lo[index].value) / (2. * h);
                    let expected = if axis < 5 {
                        actual.gradient[axis]
                    } else if index == 5 {
                        point.d_contact
                    } else {
                        0.
                    };
                    let tol = 3e-5_f64.mul_add(
                        expected.abs().max(diff.abs()),
                        64. * hi[index].value.abs().max(lo[index].value.abs()) * f64::EPSILON / h,
                    ) + 1e-22;
                    assert!(
                        (expected - diff).abs() <= tol,
                        "{case:?} {bias:?} axis={axis} branch={index}: {expected:e} vs {diff:e}"
                    );
                }
            }
        }
    }
    Ok(())
}
#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires local ngspice 47 binary"]
fn native_gate_tunneling() -> TestResult {
    use std::{fs, process::Command};
    for case in cases() {
        let law = case.prepare()?;
        let sign = case.sign();
        let polarity = if case.pmos { "pmos" } else { "nmos" };
        let Case {
            heat,
            soi,
            igc,
            igb,
            material,
            contact_area,
            ..
        } = case;
        let shmod = i32::from(heat);
        let channel_selector = igc;
        let body_selector = igb;
        let mtrlmod = material;
        let rise = if heat { 20. } else { 0. };
        let vth0 = sign * 0.55;
        let area = contact_area;
        for (g, d, b, e, p) in [
            (1.2, 0.6, -0.2, 0.1, -0.3),
            (-1.4, 0.3, -0.1, 0.2, -0.4),
            (0.7, -0.4, 0.1, -0.2, 0.3),
            (0.2, 0.7, -0.4, 0.5, 0.9),
        ] {
            let bias = case.bias(g, d, b, e, p, rise);
            let BsimSoiChannelBias {
                gate,
                drain,
                body,
                backgate,
                ..
            } = bias.channel;
            let contact = bias.contact;
            let deck = format!(
                "BSIMSOI gate tunneling reference\nVd d 0 {drain}\nVg g 0 {gate}\nVb b 0 {body}\nVe e 0 {backgate}\nVp p 0 {contact}\nVt t 0 {rise}\nM1 d g 0 e p b t soi tnodeout l=.5u w=4u nf=2 nseg=2 m=2.3 pd=4u ps=4u psbcp=1e-7 pdbcp=2e-7 agbcpd=1e-13 agbcp2={area}\n.model soi {polarity} level=10 soimod={soi} fdmod=1 shmod={shmod} rth0=.005 vth0={vth0} igcmod={channel_selector} igbmod={body_selector} mtrlmod={mtrlmod} eot=3e-9 epsrsub=11.7 phig=4.1 easub=4.05\n+ {CARD}\n.temp 27\n.options gmin=1e-18 reltol=1e-10 abstol=1e-18 vntol=1e-12\n.control\nset numdgt=17\nset wr_singlescale\nop\nwrdata out.dat @m1[igs] @m1[igd] @m1[igcs] @m1[igcd] i(vg)\nquit\n.endc\n.end\n"
            );
            let dir = tempfile::tempdir()?;
            fs::write(dir.path().join("test.cir"), deck)?;
            let output = Command::new("ngspice")
                .args(["-b", "-n", "test.cir"])
                .current_dir(dir.path())
                .output()?;
            if !output.status.success() {
                return Err(format!("ngspice: {}", String::from_utf8_lossy(&output.stderr)).into());
            }
            let native = fs::read_to_string(dir.path().join("out.dat"))?
                .split_whitespace()
                .map(str::parse::<f64>)
                .collect::<Result<Vec<_>, _>>()?;
            assert_eq!(native.len(), 6);
            let point = values(&law.evaluate(bias)?);
            let (cs, cd) = if d < 0. {
                (native[4], native[3])
            } else {
                (native[3], native[4])
            };
            let references = [sign * native[1], sign * native[2], sign * cs, sign * cd];
            for (index, reference) in references.into_iter().enumerate() {
                let actual = point[index].value;
                assert!(
                    (actual - reference).abs() <= 3e-6_f64.mul_add(reference.abs(), 1e-18),
                    "{case:?} {bias:?} branch={index}: {actual:e} vs {reference:e}"
                );
            }
            let actual: f64 = point.into_iter().map(|x| x.value).sum();
            // Native reverse IGC stamping folds the omitted backgate
            // column into the physical drain. Its DC equivalent source also
            // omits the backgate term. Compare the constitutive sum after
            // removing precisely that source-defined stamp defect.
            let correction = if d < 0. {
                -drain * (point[2].gradient[3] + point[3].gradient[3])
            } else {
                0.
            };
            let reference = -native[5] - correction;
            assert!(
                (actual - reference).abs() <= 3e-6_f64.mul_add(reference.abs(), 1e-17),
                "{case:?} {bias:?} total: {actual:e} vs {reference:e}, body={:?} contact={:?}",
                point[4],
                point[5]
            );
        }
    }
    Ok(())
}
