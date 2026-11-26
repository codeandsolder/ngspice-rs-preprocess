use ParameterValue::{Flag, Integer, Real};
use model_compact_intrinsic::{
    BoundParameters, BsimSoiChannelBias, BsimSoiChannelScalar, BsimSoiInstanceInput,
    BsimSoiIntrinsicCharges, ModelVariant, ParameterScope, ParameterValue,
    PreparedBsimSoiIntrinsicCharges, prepare_bsim_soi_model, prepare_bsim_soi_setup,
};
type TestResult = Result<(), Box<dyn std::error::Error>>;
const CARD: &str = "tsi=3e-8 tbox=1e-7 nch=1.7e17 nsub=6e16 ngate=1e20 k1=.53 k2=-.02 vfb=-.8 tox=3e-9 fbody=.8 kb1=.9 a0=.8 ags=.2 b0=2e-8 b1=1e-8 keta=-.02 ketas=.02 clc=1e-8 cle=.6 minv=.13 minvcv=.2 voff=-.08 voffcv=-.03 noff=1.1 delvt=.025";
#[derive(Clone, Copy, Debug)]
struct Case {
    pmos: bool,
    heat: bool,
    soi: i32,
    cv: i32,
    partition: f64,
    contact: bool,
    material: bool,
}
impl Case {
    const fn sign(self) -> f64 {
        if self.pmos { -1. } else { 1. }
    }
    fn prepare(
        self,
        capmod: i32,
    ) -> Result<PreparedBsimSoiIntrinsicCharges, Box<dyn std::error::Error>> {
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
                    ("capmod", Integer(capmod)),
                    ("vgstcvmod", Integer(self.cv)),
                    ("xpart", Real(self.partition)),
                    ("mtrlmod", Integer(i32::from(self.material))),
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
                ("agbcp", Real(1e-13)),
                ("aebcp", Real(2e-13)),
                ("agbcp2", Real(if self.contact { 2e-13 } else { 0. })),
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
        Ok(setup.prepare_intrinsic_charges(0, 300.15)?)
    }
    fn bias(self, g: f64, d: f64, b: f64, e: f64, rise: f64) -> BsimSoiChannelBias {
        let s = self.sign();
        BsimSoiChannelBias {
            gate: s * g,
            drain: s * d,
            body: s * b,
            backgate: s * e,
            kelvin: 300.15 + rise,
        }
    }
}
fn cases() -> impl Iterator<Item = Case> {
    [false, true].into_iter().flat_map(|pmos| {
        [false, true].into_iter().flat_map(move |heat| {
            (0..=2).flat_map(move |soi| {
                (0..=2).flat_map(move |cv| {
                    [0.4, 0.5, 0.6].into_iter().flat_map(move |partition| {
                        [false, true].into_iter().flat_map(move |contact| {
                            [false, true].into_iter().map(move |material| Case {
                                pmos,
                                heat,
                                soi,
                                cv,
                                partition,
                                contact,
                                material,
                            })
                        })
                    })
                })
            })
        })
    })
}
fn values(point: BsimSoiIntrinsicCharges) -> [BsimSoiChannelScalar; 6] {
    [
        point.gate,
        point.drain,
        point.source,
        point.body,
        point.backgate,
        point.inversion,
    ]
}
fn perturb(mut bias: BsimSoiChannelBias, axis: usize, h: f64) -> BsimSoiChannelBias {
    match axis {
        0 => bias.gate += h,
        1 => bias.drain += h,
        2 => bias.body += h,
        3 => bias.backgate += h,
        _ => bias.kelvin += h,
    }
    bias
}
#[test]
fn capmod2_gradients_and_conservation() -> TestResult {
    check_gradients_and_conservation(2)
}
#[test]
fn capmod3_gradients_and_conservation() -> TestResult {
    check_gradients_and_conservation(3)
}
fn check_gradients_and_conservation(capmod: i32) -> TestResult {
    for case in cases() {
        let law = case.prepare(capmod)?;
        for (g, d, b, e) in [
            (1.2, 0.6, -0.2, 0.1),
            (-1.4, 0.3, -0.1, 0.2),
            (0.7, -0.4, 0.1, -0.2),
            (2.4, 0.02, -0.4, 0.5),
        ] {
            let bias = case.bias(g, d, b, e, 20.);
            let point = values(law.evaluate(bias)?);
            let sum = point[..5]
                .iter()
                .copied()
                .reduce(|a, b| a + b)
                .ok_or("empty charges")?;
            assert!(sum.value.abs() <= 1e-28);
            assert!(sum.gradient.into_iter().all(|x| x.abs() <= 1e-28));
            for axis in 0..5 {
                let h = if axis == 4 { 1e-3 } else { 1e-5 };
                let hi = values(law.evaluate(perturb(bias, axis, h))?);
                let lo = values(law.evaluate(perturb(bias, axis, -h))?);
                for (index, actual) in point.into_iter().enumerate() {
                    let diff = (hi[index].value - lo[index].value) / (2. * h);
                    let expected = actual.gradient[axis];
                    let tol = 3e-5 * expected.abs().max(diff.abs())
                        + 64. * hi[index].value.abs().max(lo[index].value.abs()) * f64::EPSILON / h
                        + 1e-24;
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
fn native_capmod2_charges() -> TestResult {
    check_native_charges(2, |_| true)
}
#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires local ngspice 47 binary"]
fn native_capmod3_charges() -> TestResult {
    check_native_charges(3, |_| true)
}
#[cfg(target_os = "linux")]
fn check_native_charges(capmod: i32, filter: impl Fn(&Case) -> bool) -> TestResult {
    use std::{fs, process::Command};
    for case in cases().filter(filter) {
        let law = case.prepare(capmod)?;
        let sign = case.sign();
        let polarity = if case.pmos { "pmos" } else { "nmos" };
        let Case {
            heat,
            soi,
            cv,
            partition,
            contact,
            material,
            ..
        } = case;
        let shmod = i32::from(heat);
        let mtrlmod = i32::from(material);
        let rise = if heat { 20. } else { 0. };
        let vth0 = sign * 0.55;
        let area = if contact { 2e-13 } else { 0. };
        for (g, d, b, e) in [
            (1.2, 0.6, -0.2, 0.1),
            (-1.4, 0.3, -0.1, 0.2),
            (0.7, -0.4, 0.1, -0.2),
            (2.4, 0.02, -0.4, 0.5),
        ] {
            let bias = case.bias(g, d, b, e, rise);
            let BsimSoiChannelBias {
                gate,
                drain,
                body,
                backgate,
                ..
            } = bias;
            let deck = format!(
                "BSIMSOI intrinsic charge reference\nVd d 0 {drain}\nVg g 0 {gate}\nVb b 0 {body}\nVe e 0 {backgate}\nVp p 0 {body}\nVt t 0 {rise}\nM1 d g 0 e p b t soi tnodeout l=.5u w=4u nf=2 nseg=2 m=2.3 pd=4u ps=4u agbcp=1e-13 aebcp=2e-13 agbcp2={area}\n.model soi {polarity} level=10 soimod={soi} fdmod=1 shmod={shmod} rth0=.005 vth0={vth0} capmod={capmod} vgstcvmod={cv} xpart={partition} mtrlmod={mtrlmod} eot=3e-9 epsrsub=11.7 phig=4.1 easub=4.05\n+ {CARD}\n.temp 27\n.options gmin=1e-18 reltol=1e-10 abstol=1e-18 vntol=1e-12\n.control\nset numdgt=17\nset wr_singlescale\nop\nwrdata out.dat @m1[qb] @m1[qd] @m1[qs]\nquit\n.endc\n.end\n"
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
            assert_eq!(native.len(), 4);
            let point = law.evaluate(bias)?;
            let (qd, qs) = if d < 0. {
                (native[3], native[2])
            } else {
                (native[2], native[3])
            };
            for (index, (actual, reference)) in [
                (point.body.value, sign * 2.3 * native[1]),
                (point.drain.value, sign * 2.3 * qd),
                (point.source.value, sign * 2.3 * qs),
            ]
            .into_iter()
            .enumerate()
            {
                assert!(
                    (actual - reference).abs() <= 3e-6 * reference.abs() + 1e-23,
                    "{case:?} {bias:?} branch={index}: {actual:e} vs {reference:e}"
                );
            }
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires local ngspice 47 binary"]
fn native_capmod3_generalized_regression() -> TestResult {
    check_native_charges(3, |case| {
        case.material && !case.heat && !case.pmos && case.soi == 0 && case.cv == 0
    })
}
