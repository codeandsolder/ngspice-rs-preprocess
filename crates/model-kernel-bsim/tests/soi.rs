use ParameterValue::{Flag, Integer, Real};
use model_compact_intrinsic::{
    BoundParameters, ModelVariant, ParameterScope, ParameterValue, PreparedBsimSoiModel,
    prepare_bsim_soi_model,
};
use model_kernel_api::{
    Artifact, EquationKind, PointBuffer, Prepared,
    noise::{Sink, Source},
};
use model_kernel_bsim::soi::{BsimSoiGroup, BsimSoiKernel, Instance};
use std::sync::Arc;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
type NativeCase = (bool, i32, i32, i32, bool, usize, usize);

fn model(
    pmos: bool,
    soi: i32,
    cap: i32,
    heat: bool,
    rg: i32,
    rb: i32,
    rd: i32,
) -> TestResult<Arc<PreparedBsimSoiModel>> {
    let s = if pmos { -1. } else { 1. };
    Ok(Arc::new(prepare_bsim_soi_model(
        &BoundParameters::bind(
            ModelVariant::BsimSoi,
            ParameterScope::Model,
            [
                ("pmos", Flag(pmos)),
                ("soimod", Integer(soi)),
                ("fdmod", Integer(1)),
                ("capmod", Integer(cap)),
                ("shmod", Integer(i32::from(heat))),
                ("rgatemod", Integer(rg)),
                ("rbodymod", Integer(rb)),
                ("rdsmod", Integer(rd)),
                ("tsi", Real(3e-8)),
                ("tbox", Real(1e-7)),
                ("nch", Real(1.7e17)),
                ("nsub", Real(6e16)),
                ("ngate", Real(1e20)),
                ("tox", Real(3e-9)),
                ("k1", Real(0.53)),
                ("k2", Real(-0.02)),
                ("vfb", Real(-0.8)),
                ("vth0", Real(s * 0.55)),
                ("rth0", Real(0.005)),
                ("cth0", Real(1e-6)),
                ("rsh", Real(32.)),
                ("rshg", Real(100.)),
                ("xgw", Real(1e-7)),
                ("xgl", Real(2e-8)),
                ("ngcon", Real(1.)),
                ("xrcrg1", Real(8.)),
                ("xrcrg2", Real(1.2)),
                ("rbody", Real(1e-5)),
                ("rhalo", Real(1000.)),
                ("rbsh", Real(120.)),
                ("rbdb", Real(300.)),
                ("rbsb", Real(500.)),
                ("igcmod", Integer(1)),
                ("igbmod", Integer(1)),
                ("aigc", Real(0.012)),
                ("bigc", Real(0.002)),
                ("cigc", Real(0.12)),
                ("aigsd", Real(0.013)),
                ("bigsd", Real(0.003)),
                ("cigsd", Real(0.14)),
                ("isdif", Real(10.)),
                ("iddif", Real(20.)),
                ("kf", Real(1e-27)),
                ("fnoimod", Integer(0)),
                ("tnoimod", Integer(0)),
            ],
        )?,
        300.15,
    )?))
}

fn instance(count: u8, thermal: bool, length: f64) -> TestResult<Instance> {
    let mut parameters = BoundParameters::bind(
        ModelVariant::BsimSoi,
        ParameterScope::Instance,
        [
            ("l", Real(length)),
            ("w", Real(4e-6)),
            ("nf", Real(2.)),
            ("nseg", Real(2.)),
            ("m", Real(2.3)),
            ("pd", Real(4e-6)),
            ("ps", Real(4e-6)),
            ("nrd", Real(1.2)),
            ("nrs", Real(0.8)),
            ("nrb", Real(1.2)),
        ],
    )?;
    if thermal {
        parameters.assign("tnodeout", Flag(true))?;
    }
    Ok(Instance {
        parameters,
        terminal_count: count,
        geometry_scale: 1.,
    })
}

struct Sample {
    f: Vec<f64>,
    q: Vec<f64>,
    g: Vec<f64>,
    c: Vec<f64>,
}
fn sample(
    a: &Artifact<BsimSoiKernel>,
    p: &Prepared<BsimSoiKernel>,
    x: &[f64],
) -> TestResult<Sample> {
    let mut buf = PointBuffer::new(x.len())?;
    let mut point = buf.point();
    a.evaluate(p, x, &mut point)?;
    Ok(Sample {
        f: point.current.to_vec(),
        q: point.charge.to_vec(),
        g: point.conductance.to_vec(),
        c: point.capacitance.to_vec(),
    })
}
fn close(a: f64, b: f64, rel: f64, abs: f64, context: impl std::fmt::Debug) {
    assert!(
        (a - b).abs() <= rel.mul_add(a.abs().max(b.abs()), abs),
        "{context:?}: {a:e} vs {b:e}"
    );
}
fn coordinates(a: &Artifact<BsimSoiKernel>, pmos: bool, reverse: bool) -> Vec<f64> {
    let s = if pmos { -1. } else { 1. };
    a.descriptor()
        .equations
        .iter()
        .map(|row| {
            let d = if reverse { -0.45 } else { 0.65 };
            let v = match row.name.as_str() {
                "drain" => d + 0.01,
                "drain_prime" => d,
                "source" => -0.005,
                "gate" => 1.25,
                "gate_prime" => 1.2,
                "gate_mid" => 1.23,
                "backgate" => 0.12,
                "contact" => -0.65,
                "body" | "body_internal" => -0.67,
                "drain_body" => -0.66,
                "source_body" => -0.68,
                _ => 0.,
            };
            if row.kind == EquationKind::Thermal {
                325.15
            } else {
                s * v
            }
        })
        .collect()
}

#[derive(Default)]
struct Sources(Vec<(String, f64)>);
impl Sink for Sources {
    fn push(&mut self, source: Source<'_>) -> model_kernel_api::Result<()> {
        assert_eq!(source.injections.len(), 2);
        assert_ne!(source.injections[0].row, source.injections[1].row);
        self.0.push((source.name.into(), source.density));
        Ok(())
    }
}

#[test]
fn assembled_jacobians_conserve_across_topologies() -> TestResult {
    for pmos in [false, true] {
        for soi in 0..=2 {
            for cap in 2..=3 {
                for (count, flag) in [
                    (4, false),
                    (5, false),
                    (5, true),
                    (6, false),
                    (6, true),
                    (7, true),
                ] {
                    for (heat, rg, rb, rd) in [
                        (false, 0, 0, 0),
                        (true, 1, 1, 0),
                        (true, 2, 0, 1),
                        (true, 3, 1, 1),
                    ] {
                        let context = (pmos, soi, cap, count, flag, heat, rg, rb, rd);
                        let group = BsimSoiGroup::new(
                            model(pmos, soi, cap, heat, rg, rb, rd)?,
                            vec![instance(count, flag, 5e-7)?],
                        )?;
                        let a = group.artifact(0, 1e-12, [1; 16])?;
                        let p = a.prepare(&group.defaults(1e-12), 300.15)?;
                        for reverse in [false, true] {
                            let x = coordinates(&a, pmos, reverse);
                            let base = sample(&a, &p, &x)
                                .map_err(|e| format!("{context:?} {reverse}: {e}"))?;
                            let n = x.len();
                            let mut shift = x.clone();
                            for (v, row) in shift.iter_mut().zip(&a.descriptor().equations) {
                                if row.kind == EquationKind::Electrical {
                                    *v += 0.75;
                                }
                            }
                            let moved = sample(&a, &p, &shift)?;
                            for i in 0..n {
                                close(
                                    base.f[i],
                                    moved.f[i],
                                    1e-10,
                                    1e-14,
                                    (context, reverse, i, "gauge F"),
                                );
                                close(
                                    base.q[i],
                                    moved.q[i],
                                    1e-10,
                                    1e-24,
                                    (context, reverse, i, "gauge Q"),
                                );
                            }
                            for col in 0..n {
                                let step = if a.descriptor().equations[col].kind
                                    == EquationKind::Thermal
                                {
                                    1e-3
                                } else {
                                    1e-6
                                };
                                let mut plus = x.clone();
                                plus[col] += step;
                                let mut minus = x.clone();
                                minus[col] -= step;
                                let hi = sample(&a, &p, &plus)?;
                                let lo = sample(&a, &p, &minus)?;
                                for row in 0..n {
                                    close(
                                        base.g[row * n + col],
                                        (hi.f[row] - lo.f[row]) / (2. * step),
                                        3e-4,
                                        2e-10,
                                        (context, reverse, row, col, "G"),
                                    );
                                    close(
                                        base.c[row * n + col],
                                        (hi.q[row] - lo.q[row]) / (2. * step),
                                        3e-4,
                                        2e-20,
                                        (context, reverse, row, col, "C"),
                                    );
                                }
                            }
                            let mut sources = Sources::default();
                            a.evaluate_noise(&p, &x, 1e3, &mut sources)?;
                            assert!(
                                sources
                                    .0
                                    .iter()
                                    .any(|(name, density)| name == "id" && *density > 0.)
                            );
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

#[test]
fn group_candidate_replays_and_does_not_mutate_reference() -> TestResult {
    let model = model(false, 0, 3, true, 3, 1, 1)?;
    let group = BsimSoiGroup::new(
        Arc::clone(&model),
        vec![instance(7, true, 5e-7)?, instance(7, true, 8e-7)?],
    )?;
    let a = group.artifact(0, 1e-12, [2; 16])?;
    let defaults = group.defaults(1e-12);
    let original = a.prepare(&defaults, 300.15)?;
    let x = coordinates(&a, false, false);
    let before = sample(&a, &original, &x)?;
    let mut edit = defaults.clone();
    let second_l = a
        .descriptor()
        .parameters
        .iter()
        .position(|s| s.name == "i1_l")
        .ok_or("length")?;
    edit[second_l] = 1.2e-6;
    let candidate = a.prepare(&edit, 300.15)?;
    let updated = sample(&a, &candidate, &x)?;
    let independent = BsimSoiGroup::new(
        model,
        vec![instance(7, true, 5e-7)?, instance(7, true, 1.2e-6)?],
    )?;
    let b = independent.artifact(0, 1e-12, [3; 16])?;
    let rebuilt = b.prepare(&independent.defaults(1e-12), 300.15)?;
    let expected = sample(&b, &rebuilt, &x)?;
    assert_eq!(updated.f, expected.f);
    assert_eq!(updated.q, expected.q);
    assert_eq!(updated.g, expected.g);
    assert_eq!(updated.c, expected.c);
    assert_eq!(before.f, sample(&a, &original, &x)?.f);
    assert_eq!(before.q, sample(&a, &original, &x)?.q);
    let mut structural = defaults;
    let rg = a
        .descriptor()
        .parameters
        .iter()
        .position(|s| s.name == "i0_rgatemod")
        .ok_or("rgatemod")?;
    structural[rg] = 0.;
    assert!(a.prepare(&structural, 300.15).is_err());
    Ok(())
}

#[test]
fn native_clamped_terminal_residual_and_admittance() -> TestResult {
    let fixture = include_str!("fixtures/bsimsoi-clamped-ngspice47.txt");
    let card = fixture
        .lines()
        .find_map(|line| line.strip_prefix("# model-card "))
        .ok_or("native model card")?;
    let raw_card = card
        .split_whitespace()
        .map(|assignment| -> TestResult<(&str, ParameterValue)> {
            let (name, value) = assignment.split_once('=').ok_or("model assignment")?;
            let spec = ModelVariant::BsimSoi
                .parameter(ParameterScope::Model, name)
                .ok_or("model slot")?;
            let value = if matches!(spec.kind, model_compact_intrinsic::ParameterType::Integer) {
                Integer(value.parse()?)
            } else {
                Real(value.parse()?)
            };
            Ok((name, value))
        })
        .collect::<TestResult<Vec<_>>>()?;
    let mut count = 0;
    let mut mismatches = Vec::new();
    let mut compare =
        |actual: f64, expected: f64, relative: f64, absolute: f64, context: (NativeCase, &str)| {
            if !actual.is_finite()
                || !expected.is_finite()
                || (actual - expected).abs()
                    > relative.mul_add(actual.abs().max(expected.abs()), absolute)
            {
                mismatches.push(format!("{context:?}: {actual:e} vs {expected:e}"));
            }
        };
    let mut group = None;
    for line in fixture.lines().filter(|line| !line.starts_with('#')) {
        let values = line.split_whitespace().collect::<Vec<_>>();
        assert_eq!(values.len(), 34);
        let pmos = values[0] == "1";
        let soi = values[1].parse::<i32>()?;
        let cap = values[2].parse::<i32>()?;
        let heat = values[3].parse::<i32>()?;
        let reverse = values[4] == "1";
        let excite = values[5].parse::<usize>()?;
        if excite == 0 {
            let model = Arc::new(prepare_bsim_soi_model(
                &BoundParameters::bind(
                    ModelVariant::BsimSoi,
                    ParameterScope::Model,
                    raw_card.iter().cloned().chain([
                        ("pmos", Flag(pmos)),
                        ("soimod", Integer(soi)),
                        ("fdmod", Integer(1)),
                        ("capmod", Integer(cap)),
                        ("shmod", Integer(heat)),
                        ("vth0", Real(if pmos { -0.55 } else { 0.55 })),
                    ]),
                )?,
                300.15,
            )?);
            let mut raw = instance(7, true, 5e-7)?;
            raw.parameters.assign("nrd", Real(0.))?;
            raw.parameters.assign("nrs", Real(0.))?;
            let model_group = BsimSoiGroup::new(model, vec![raw])?;
            let artifact = model_group.artifact(0, 1e-18, [4; 16])?;
            let prepared = artifact.prepare(&model_group.defaults(1e-18), 300.15)?;
            group = Some((artifact, prepared));
        }
        let (a, p) = group.as_ref().ok_or("native fixture group")?;
        let data = values[6..]
            .iter()
            .map(|value| value.parse::<f64>())
            .collect::<Result<Vec<_>, _>>()?;
        let mut x = data[..7].to_vec();
        x[6] += 300.15;
        let point = sample(a, p, &x)?;
        assert_eq!(point.f.len(), 7);
        for row in 0..7 {
            let context = (pmos, soi, cap, heat, reverse, excite, row);
            compare(point.f[row], -data[7 + row], 3e-5, 2e-13, (context, "DC"));
            // Native current-source signs are opposite to residual signs.
            compare(
                point.g[row * 7 + excite],
                -data[14 + 2 * row],
                3e-4,
                2e-10,
                (context, "AC real"),
            );
            compare(
                point.c[row * 7 + excite],
                -data[15 + 2 * row] / (2. * std::f64::consts::PI * 1e6),
                3e-4,
                2e-20,
                (context, "AC imaginary"),
            );
        }
        count += 1;
    }
    assert_eq!(count, 336);
    assert!(
        mismatches.is_empty(),
        "{} native mismatches:\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
    Ok(())
}
