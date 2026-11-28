use std::sync::Arc;

use model_compact_intrinsic::{
    BoundParameters, ModelVariant, ParameterScope, ParameterValue, PreparedLegacyBsimModel,
    prepare_legacy_bsim_model,
};
use model_kernel_api::{
    Artifact, Binding, Capability, Error, PointBuffer, Prepared,
    noise::{Sink, Source},
};
use model_kernel_bsim::{Instance, LegacyBsimKernel, SeriesNodes, legacy_bsim_artifact};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn family_model(
    variant: ModelVariant,
    pmos: bool,
    sheet: f64,
) -> TestResult<Arc<PreparedLegacyBsimModel>> {
    let params = [
        ("tox", ParameterValue::Real(0.024)),
        ("vdd", ParameterValue::Real(5.)),
        ("vfb", ParameterValue::Real(-0.9)),
        ("phi", ParameterValue::Real(0.75)),
        ("k1", ParameterValue::Real(0.75)),
        ("k2", ParameterValue::Real(0.02)),
        ("n0", ParameterValue::Real(1.2)),
        ("dl", ParameterValue::Real(0.17)),
        ("dw", ParameterValue::Real(0.28)),
        ("rsh", ParameterValue::Real(sheet)),
        ("js", ParameterValue::Real(2e-5)),
        ("cj", ParameterValue::Real(1e-3)),
        ("cjsw", ParameterValue::Real(2e-10)),
        ("pb", ParameterValue::Real(0.74)),
        ("pbsw", ParameterValue::Real(0.83)),
        ("mj", ParameterValue::Real(1.)),
        ("mjsw", ParameterValue::Real(0.33)),
        ("cgdo", ParameterValue::Real(1.5e-10)),
        ("cgso", ParameterValue::Real(2e-10)),
        ("cgbo", ParameterValue::Real(4e-11)),
        ("kf", ParameterValue::Real(2e-25)),
        ("af", ParameterValue::Real(1.2)),
        ("pmos", ParameterValue::Flag(pmos)),
    ];
    let specific: &[(&str, f64)] = match variant {
        ModelVariant::Bsim1 => &[
            ("mus", 510.),
            ("muz", 400.),
            ("eta", 0.02),
            ("u0", 0.1),
            ("u1", 0.06),
        ],
        ModelVariant::Bsim2 => &[
            ("mus0", 510.),
            ("mu0", 400.),
            ("eta0", 0.02),
            ("ua0", 0.1),
            ("u10", 0.06),
            ("vbb", -3.),
            ("ai0", 0.05),
            ("bi0", 0.5),
        ],
        _ => return Err("unexpected model family".into()),
    };
    Ok(Arc::new(prepare_legacy_bsim_model(
        &BoundParameters::bind(
            variant,
            ParameterScope::Model,
            params.into_iter().chain(
                specific
                    .iter()
                    .map(|&(name, value)| (name, ParameterValue::Real(value))),
            ),
        )?,
    )?))
}

fn model(pmos: bool, sheet: f64) -> TestResult<Arc<PreparedLegacyBsimModel>> {
    family_model(ModelVariant::Bsim1, pmos, sheet)
}

const fn reference() -> Instance {
    Instance {
        length: 8.3e-6,
        width: 17.6e-6,
        multiplier: 2.,
        drain_area: 3e-11,
        source_area: 4e-11,
        drain_perimeter: 13e-6,
        source_perimeter: 17e-6,
        drain_squares: 2.3,
        source_squares: 3.1,
        gmin: 1e-12,
    }
}

fn artifact_for(
    model: Arc<PreparedLegacyBsimModel>,
    reference: Instance,
    fingerprint: [u8; 16],
) -> TestResult<Arc<Artifact<LegacyBsimKernel>>> {
    let shape = SeriesNodes::resolve(&model, [reference.drain_squares, reference.source_squares])?;
    Ok(legacy_bsim_artifact(model, shape, fingerprint)?)
}

fn close(label: &str, actual: f64, expected: f64, rel: f64, absolute: f64) {
    assert!(
        (actual - expected).abs() <= rel.mul_add(expected.abs().max(actual.abs()), absolute),
        "{label}: {actual:.17e} != {expected:.17e}"
    );
}

struct Sample {
    current: Vec<f64>,
    charge: Vec<f64>,
    conductance: Vec<f64>,
    capacitance: Vec<f64>,
}

fn sample(
    artifact: &Artifact<LegacyBsimKernel>,
    instance: &Prepared<LegacyBsimKernel>,
    coordinates: &[f64],
) -> TestResult<Sample> {
    let mut buffer = PointBuffer::new(coordinates.len())?;
    let mut out = buffer.point();
    artifact.evaluate(instance, coordinates, &mut out)?;
    Ok(Sample {
        current: out.current.to_vec(),
        charge: out.charge.to_vec(),
        conductance: out.conductance.to_vec(),
        capacitance: out.capacitance.to_vec(),
    })
}

#[test]
fn structural_candidates_and_polarity_are_transactional() -> TestResult {
    let model = model(false, 52.)?;
    let instance = reference();
    let artifact = artifact_for(Arc::clone(&model), instance, [1; 16])?;
    let desc = artifact.descriptor();
    assert_eq!(desc.charge, Capability::Computed);
    assert_eq!(desc.noise, Capability::Computed);
    assert_eq!(
        desc.equations
            .iter()
            .map(|row| row.name.as_str())
            .collect::<Vec<_>>(),
        [
            "drain",
            "gate",
            "source",
            "bulk",
            "drain_prime",
            "source_prime"
        ]
    );
    assert_eq!(desc.equations[4].binding, Binding::Hidden);
    assert_eq!(desc.equations[5].binding, Binding::Hidden);
    let base = artifact.prepare(&instance.slots(), 300.15)?;
    let mut updated = instance;
    updated.drain_squares = 3.;
    let candidate = artifact.prepare(&updated.slots(), 350.)?;
    let coordinates = [1.2, 1.7, 0., -0.3, 1.1, 0.1];
    let first = sample(&artifact, &base, &coordinates)?;
    let second = sample(&artifact, &candidate, &coordinates)?;
    assert_ne!(first.current, second.current);
    updated.drain_squares = 0.;
    assert!(matches!(
        artifact.prepare(&updated.slots(), 300.15),
        Err(Error::Unsupported(_))
    ));
    let one_hidden = artifact_for(Arc::clone(&model), updated, [2; 16])?;
    assert_eq!(one_hidden.descriptor().equations.len(), 5);
    let again = sample(&artifact, &base, &coordinates)?;
    assert_eq!(first.current, again.current);
    assert_eq!(first.charge, again.charge);
    assert!(sample(&one_hidden, &base, &[1.2, 1.7, 0., -0.3, 0.1]).is_err());
    Ok(())
}

fn check_jacobian(
    artifact: &Artifact<LegacyBsimKernel>,
    instance: &Prepared<LegacyBsimKernel>,
    coordinates: &[f64],
) -> TestResult {
    let base = sample(artifact, instance, coordinates)?;
    let n = coordinates.len();
    let h = 1e-6;
    for column in 0..n {
        let mut plus = coordinates.to_vec();
        let mut minus = coordinates.to_vec();
        plus[column] += h;
        minus[column] -= h;
        let plus = sample(artifact, instance, &plus)?;
        let minus = sample(artifact, instance, &minus)?;
        for row in 0..n {
            close(
                "nodal dI",
                base.conductance[row * n + column],
                (plus.current[row] - minus.current[row]) / (2. * h),
                2e-6,
                5e-12,
            );
            close(
                "nodal dQ",
                base.capacitance[row * n + column],
                (plus.charge[row] - minus.charge[row]) / (2. * h),
                2e-6,
                1e-19,
            );
        }
    }
    let shifted = coordinates.iter().map(|x| x + 4.25).collect::<Vec<_>>();
    let gauge = sample(artifact, instance, &shifted)?;
    for (a, b) in base.current.iter().zip(&gauge.current) {
        close("gauge current", *a, *b, 1e-12, 1e-16);
    }
    for (a, b) in base.charge.iter().zip(&gauge.charge) {
        close("gauge charge", *a, *b, 1e-12, 1e-26);
    }
    Ok(())
}

#[test]
fn complete_nodal_law_has_exact_jacobians_in_both_polarities_and_orientations() -> TestResult {
    for variant in [ModelVariant::Bsim1, ModelVariant::Bsim2] {
        for pmos in [false, true] {
            let polarity = if pmos { -1. } else { 1. };
            for sheet in [0., 52.] {
                let artifact =
                    artifact_for(family_model(variant, pmos, sheet)?, reference(), [3; 16])?;
                let instance = artifact.prepare(&reference().slots(), 310.)?;
                for forward in [false, true] {
                    let (drain, source) = if forward { (1.1, 0.1) } else { (0.1, 1.1) };
                    for bulk in [-0.4, 0.16] {
                        let mut coordinates = vec![drain, 1.7, source, bulk];
                        if sheet > 0. {
                            coordinates.extend([drain - 0.03, source + 0.02]);
                        }
                        let coordinates = coordinates
                            .into_iter()
                            .map(|x| x * polarity)
                            .collect::<Vec<_>>();
                        check_jacobian(&artifact, &instance, &coordinates)?;
                    }
                }
            }
        }
    }
    Ok(())
}

#[derive(Default)]
struct Sources(Vec<(String, f64, Vec<usize>)>);

impl Sink for Sources {
    fn push(&mut self, source: Source<'_>) -> model_kernel_api::Result<()> {
        self.0.push((
            source.name.into(),
            source.density,
            source.injections.iter().map(|x| x.row).collect(),
        ));
        Ok(())
    }
}

#[test]
fn noise_and_parallel_multiplicity_follow_physical_branch_scaling() -> TestResult {
    let artifact = artifact_for(model(false, 52.)?, reference(), [4; 16])?;
    let coordinates = [1.2, 1.7, 0., -0.3, 1.1, 0.1];
    let single = artifact.prepare(&reference().slots(), 300.)?;
    let mut double = reference();
    double.multiplier *= 2.;
    let double = artifact.prepare(&double.slots(), 600.)?;
    let a = sample(&artifact, &single, &coordinates)?;
    let b = sample(&artifact, &double, &coordinates)?;
    for (left, right) in [
        (&a.current, &b.current),
        (&a.charge, &b.charge),
        (&a.conductance, &b.conductance),
        (&a.capacitance, &b.capacitance),
    ] {
        for (x, y) in left.iter().zip(right) {
            close("m scaling", 2. * x, *y, 1e-13, 1e-30);
        }
    }
    let mut a = Sources::default();
    let mut b = Sources::default();
    artifact.evaluate_noise(&single, &coordinates, 100., &mut a)?;
    artifact.evaluate_noise(&double, &coordinates, 200., &mut b)?;
    assert_eq!(a.0.len(), 4);
    for ((name, original, rows), (new_name, changed, new_rows)) in a.0.iter().zip(&b.0) {
        assert_eq!(name, new_name);
        assert_eq!(rows, new_rows);
        assert!(*original > 0.);
        let factor = if name == "1overf" { 1. } else { 4. };
        close(name, *changed, factor * original, 2e-13, 1e-38);
    }
    Ok(())
}

#[test]
fn native_ngspice_operating_points_include_hidden_series_nodes() -> TestResult {
    for (variant, fixture) in [
        (
            ModelVariant::Bsim1,
            include_str!("fixtures/bsim1-dc-ngspice47.txt"),
        ),
        (
            ModelVariant::Bsim2,
            include_str!("fixtures/bsim2-dc-ngspice47.txt"),
        ),
    ] {
        let mut count = 0;
        for line in fixture.lines().filter(|line| !line.starts_with('#')) {
            let data = line
                .split_whitespace()
                .map(str::parse::<f64>)
                .collect::<Result<Vec<_>, _>>()?;
            let [pmos, sheet, drain, gate, source, bulk, id, ig, is, ib]: [f64; 10] = data
                .try_into()
                .map_err(|_| "wrong native OP fixture width")?;
            let reference = Instance {
                gmin: 0.,
                ..reference()
            };
            let artifact = artifact_for(
                family_model(variant, pmos != 0., sheet)?,
                reference,
                [5; 16],
            )?;
            let prepared = artifact.prepare(&reference.slots(), 300.15)?;
            let mut coords = vec![drain, gate, source, bulk];
            if sheet > 0. {
                // The native reference supplies the external series current.
                // Recover each hidden voltage from the independent Ohm's law.
                coords.push(id.mul_add(
                    sheet * reference.drain_squares / reference.multiplier,
                    drain,
                ));
                coords.push(is.mul_add(
                    sheet * reference.source_squares / reference.multiplier,
                    source,
                ));
            }
            let point = sample(&artifact, &prepared, &coords)?;
            for (actual, expected) in point.current[..4].iter().zip([-id, -ig, -is, -ib]) {
                close("external DC residual", *actual, expected, 3e-9, 1e-15);
            }
            for actual in &point.current[4..] {
                close("native solved hidden residual", *actual, 0., 0., 1e-12);
            }
            count += 1;
        }
        assert_eq!(count, 16);
    }
    Ok(())
}

#[test]
fn gmin_is_a_single_consistent_junction_conductance() -> TestResult {
    let reference = Instance {
        gmin: 0.,
        ..reference()
    };
    let artifact = artifact_for(model(false, 0.)?, reference, [6; 16])?;
    let baseline = artifact.prepare(&reference.slots(), 300.15)?;
    let gmin = 1e-5;
    let candidate = artifact.prepare(&Instance { gmin, ..reference }.slots(), 300.15)?;
    let coordinates = [1.2, 1.7, 0.1, -0.3];
    let a = sample(&artifact, &baseline, &coordinates)?;
    let b = sample(&artifact, &candidate, &coordinates)?;
    let g = gmin * reference.multiplier;
    let drain = g * (coordinates[0] - coordinates[3]);
    let source = g * (coordinates[2] - coordinates[3]);
    for ((x, y), expected) in
        a.current
            .iter()
            .zip(&b.current)
            .zip([drain, 0., source, -drain - source])
    {
        close("single gmin", y - x, expected, 1e-12, 1e-18);
    }
    for ((x, y), expected) in a.conductance.iter().zip(&b.conductance).zip([
        g,
        0.,
        0.,
        -g,
        0.,
        0.,
        0.,
        0.,
        0.,
        0.,
        g,
        -g,
        -g,
        0.,
        -g,
        2. * g,
    ]) {
        close("single gmin Jacobian", y - x, expected, 1e-12, 1e-18);
    }
    Ok(())
}

#[test]
fn physical_noise_density_from_native_dc_measurements() -> TestResult {
    let reference = Instance {
        gmin: 0.,
        ..reference()
    };
    let artifact = artifact_for(model(false, 0.)?, reference, [7; 16])?;
    let prepared = artifact.prepare(&reference.slots(), 300.15)?;
    let coordinates = [1.204_300_731_234_587_5, 1.7, 0.1, -0.3];
    // Independent ngspice 47 DC measurements at a fixed drain voltage
    // (noise-dc.cir). The gate sweep measures the total device gm.
    let id = 9.956_992_687_654_125e-5_f64;
    let gm = (9.957_182_602_677_812e-5 - 9.956_802_774_274_54e-5) / 2e-5;
    let thermal = 4. * 1.380_648_52e-23 * 300.15;
    let cox = 3.453e-13 / (0.024e-4);
    let area = (8.3e-6 - 0.17e-6) * (17.6e-6 - 0.28e-6);
    for frequency in [1000., 2000., 3000.] {
        let mut sources = Sources::default();
        artifact.evaluate_noise(&prepared, &coordinates, frequency, &mut sources)?;
        assert_eq!(sources.0.len(), 2);
        for (name, psd, _) in sources.0 {
            let expected = match name.as_str() {
                "id" => thermal * (2. / 3.) * gm,
                "1overf" => {
                    2e-25 * reference.multiplier * (id / reference.multiplier).powf(1.2)
                        / (frequency * area * cox * cox)
                }
                _ => return Err("unexpected source".into()),
            };
            close(&name, psd, expected, 1e-5, 1e-36);
        }
    }
    Ok(())
}

#[test]
fn structural_resolution_is_independent_of_channel_preparation() -> TestResult {
    let model = family_model(ModelVariant::Bsim2, false, 52.)?;
    let shape = SeriesNodes::resolve(&model, [0., 3.])?;
    assert_eq!(
        shape,
        SeriesNodes {
            drain: false,
            source: true
        }
    );
    assert!(SeriesNodes::resolve(&model, [-1., 3.]).is_err());
    let a = legacy_bsim_artifact(Arc::clone(&model), shape, [8; 16])?;
    let b = legacy_bsim_artifact(Arc::clone(&model), shape, [8; 16])?;
    assert_eq!(a.descriptor(), b.descriptor());
    let mut invalid = reference();
    invalid.drain_squares = 0.;
    invalid.length = 0.;
    assert!(a.prepare(&invalid.slots(), 300.15).is_err());
    let valid = Instance {
        length: 10e-6,
        ..invalid
    };
    let prepared = a.prepare(&valid.slots(), 300.15)?;
    assert_eq!(a.descriptor().equations.len(), 5);
    sample(&a, &prepared, &[0.5, 1.5, 0., -0.3, 0.02])?;
    Ok(())
}

#[test]
fn trial_policy_is_coherent_for_both_polarities_orientations_and_series_shapes() -> TestResult {
    use model_kernel_api::trial::{
        Convergence, ConvergenceInput, TrialInput, TrialOutcome, TrialPhase, TrialSample,
    };
    for variant in [ModelVariant::Bsim1, ModelVariant::Bsim2] {
        for pmos in [false, true] {
            let sign = if pmos { -1. } else { 1. };
            let model = family_model(variant, pmos, 52.)?;
            for squares in [[0., 0.], [1., 0.], [0., 1.], [1., 1.]] {
                let reference = Instance {
                    drain_squares: squares[0],
                    source_squares: squares[1],
                    ..reference()
                };
                let artifact = artifact_for(Arc::clone(&model), reference, [9; 16])?;
                let prepared = artifact.prepare(&reference.slots(), 300.15)?;
                let width = artifact.descriptor().equations.len();
                let drain = if squares[0] > 0. { 4 } else { 0 };
                let source = if squares[1] > 0. { width - 1 } else { 2 };
                let mut proposed = vec![0.; width];
                let mut initialized = vec![0.; width];
                assert_eq!(
                    artifact.linearize_trial(
                        &prepared,
                        TrialInput {
                            proposed: &proposed,
                            phase: TrialPhase::ColdStart
                        },
                        &mut initialized
                    )?,
                    TrialOutcome::Initialized
                );
                close(
                    "cold-start drain",
                    initialized[drain] - initialized[source],
                    0.,
                    0.,
                    0.,
                );
                close(
                    "cold-start bulk",
                    sign * (initialized[3] - initialized[source]),
                    -1.,
                    0.,
                    0.,
                );
                sample(&artifact, &prepared, &initialized)?;
                for reverse in [false, true] {
                    let mut previous = vec![0.; width];
                    previous[drain] = sign * if reverse { -0.5 } else { 0.5 };
                    previous[1] = sign * 1.2;
                    previous[3] = -sign * 0.7;
                    proposed.clone_from(&previous);
                    proposed[drain] = sign * if reverse { 80. } else { -80. };
                    proposed[1] = sign * 200.;
                    proposed[3] = sign * 200.;
                    let old = sample(&artifact, &prepared, &previous)?;
                    let mut limited = vec![0.; width];
                    for phase in [
                        TrialPhase::Newton {
                            previous: &previous,
                        },
                        TrialPhase::Predictor {
                            accepted: &previous,
                        },
                    ] {
                        assert_eq!(
                            artifact.linearize_trial(
                                &prepared,
                                TrialInput {
                                    proposed: &proposed,
                                    phase
                                },
                                &mut limited
                            )?,
                            TrialOutcome::Limited
                        );
                        // A single step fraction controls all intrinsic voltage
                        // differences. Both junctions remain representable.
                        let ratio = (limited[drain] - previous[drain])
                            / (proposed[drain] - previous[drain]);
                        assert!(ratio > 0. && ratio < 1.);
                        for row in [1, 3] {
                            close(
                                "coherent step",
                                (limited[row] - previous[row]) / (proposed[row] - previous[row]),
                                ratio,
                                1e-12,
                                1e-15,
                            );
                        }
                        let point = sample(&artifact, &prepared, &limited)?;
                        let tolerance = vec![1e-12; width];
                        let previous_sample = TrialSample {
                            coordinates: &previous,
                            residual: &old.current,
                            jacobian: &old.conductance,
                        };
                        assert_eq!(
                            artifact.check_trial_convergence(
                                &prepared,
                                ConvergenceInput {
                                    previous: previous_sample,
                                    candidate: TrialSample {
                                        coordinates: &limited,
                                        residual: &point.current,
                                        jacobian: &point.conductance
                                    },
                                    candidate_outcome: TrialOutcome::Limited,
                                    relative_tolerance: 1e-6,
                                    absolute_tolerances: &tolerance,
                                }
                            )?,
                            Convergence::Retry
                        );
                        assert_eq!(
                            artifact.check_trial_convergence(
                                &prepared,
                                ConvergenceInput {
                                    previous: previous_sample,
                                    candidate: previous_sample,
                                    candidate_outcome: TrialOutcome::Exact,
                                    relative_tolerance: 1e-6,
                                    absolute_tolerances: &tolerance,
                                }
                            )?,
                            Convergence::Satisfied
                        );
                    }
                    // Exact loads are independent of trial policy and no
                    // failed/limited trial has changed the immutable instance.
                    assert_eq!(
                        artifact.linearize_trial(
                            &prepared,
                            TrialInput {
                                proposed: &previous,
                                phase: TrialPhase::Exact
                            },
                            &mut limited
                        )?,
                        TrialOutcome::Exact
                    );
                    assert_eq!(limited, previous);
                    assert_eq!(
                        sample(&artifact, &prepared, &previous)?.current,
                        old.current
                    );
                }
            }
        }
    }
    Ok(())
}
