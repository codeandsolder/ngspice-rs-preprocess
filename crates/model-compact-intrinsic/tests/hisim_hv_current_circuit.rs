//! Converged native channel-current corrections and independent derivatives.
use model_compact_intrinsic::{
    BoundParameters, HiSimHvAccessInputs, HiSimHvExternalBias, HiSimHvIntrinsicBias,
    HiSimHvIntrinsicScalar as Jet, ModelError, ModelVariant, ParameterScope, ParameterValue,
    PreparedHiSimHvInstance, PreparedHiSimHvModel, prepare_hisim_hv_access_correction,
    prepare_hisim_hv_instance, prepare_hisim_hv_model, prepare_hisim_hv_normal_channel,
    prepare_hisim_hv_transport,
};
type TestResult = Result<(), Box<dyn std::error::Error>>;

#[derive(Clone, Copy)]
enum Mechanism {
    Access,
    Sti,
}

fn card_parameters(
    mechanism: Mechanism,
    config: f64,
) -> Result<&'static [(&'static str, f64)], ModelError> {
    match mechanism {
        Mechanism::Sti => match config {
            0. => Ok(&[("wsti", 0.1e-6), ("nsti", 1e17), ("vthsti", 0.1)]),
            1. => Ok(&[
                ("wsti", 0.35e-6),
                ("nsti", 5e17),
                ("vthsti", -0.2),
                ("vdsti", 0.02),
                ("scsti1", 0.1),
                ("scsti2", 0.2),
                ("wstil", 0.5),
                ("wstiw", 0.3),
            ]),
            2. => Ok(&[
                ("wsti", 0.05e-6),
                ("nsti", 1e18),
                ("vthsti", 0.2),
                ("vdsti", -0.01),
                ("scsti1", 0.3),
                ("scsti2", 0.1),
            ]),
            _ => Err(ModelError::InvalidPhysicalParameter("native STI card")),
        },
        Mechanism::Access => access_card_parameters(config),
    }
}

fn access_card_parameters(config: f64) -> Result<&'static [(&'static str, f64)], ModelError> {
    Ok(match config {
        0. => &[("rd", 0.03), ("rs", 0.03), ("rd23", 0.1), ("rd21", 0.6)],
        1. => &[
            ("rd", 0.04),
            ("rs", 0.),
            ("rd23", 0.04),
            ("rd21", 0.75),
            ("rd20", 2.),
            ("rd24", 0.03),
            ("rd25", 1.),
            ("rd22", 0.003),
            ("rd22d", 1.2),
            ("rd23l", 0.03),
            ("rd23lp", 0.7),
            ("rd23s", 0.8),
            ("rd23sp", 0.6),
        ],
        2. => &[("rd", 0.), ("rs", 0.), ("rd23", 0.1), ("rd21", 0.6)],
        _ => return Err(ModelError::InvalidPhysicalParameter("native access card")),
    })
}

fn prepare(
    row: &[f64; 15],
    mechanism: Mechanism,
) -> Result<(PreparedHiSimHvModel, PreparedHiSimHvInstance), ModelError> {
    let variant = match row[0] {
        1. => ModelVariant::HiSimHv1,
        2. => ModelVariant::HiSimHv2,
        _ => return Err(ModelError::InvalidPhysicalParameter("native model family")),
    };
    let mut fields = vec![
        (
            if row[1] < 0. { "pmos" } else { "nmos" },
            ParameterValue::Flag(true),
        ),
        (
            "corsrd",
            ParameterValue::Integer(if matches!(mechanism, Mechanism::Access) {
                2
            } else {
                0
            }),
        ),
        (
            "coisti",
            ParameterValue::Integer(i32::from(matches!(mechanism, Mechanism::Sti))),
        ),
    ];
    for name in ["coisub", "coadov", "coovlp", "conqs"] {
        fields.push((name, ParameterValue::Integer(0)));
    }
    let parameters = card_parameters(mechanism, row[7])?;
    fields.extend(
        parameters
            .iter()
            .map(|(name, value)| (*name, ParameterValue::Real(*value))),
    );
    if variant == ModelVariant::HiSimHv2 {
        fields.push(("cordrift", ParameterValue::Integer(0)));
        if matches!(mechanism, Mechanism::Access) {
            fields.extend(
                [
                    ("ptl", 0.02),
                    ("ptlp", 0.7),
                    ("ptp", 3.7),
                    ("pt2", 0.12),
                    ("pt4", 0.4),
                    ("pt4p", 0.8),
                    ("gdl", 0.001),
                    ("gdld", 0.2e-6),
                    ("gdlp", 0.4),
                    ("gdsleak", 1e-7),
                ]
                .map(|(n, v)| (n, ParameterValue::Real(v))),
            );
        }
    }
    let model = prepare_hisim_hv_model(&BoundParameters::bind(
        variant,
        ParameterScope::Model,
        fields,
    )?)?;
    let instance = prepare_hisim_hv_instance(
        &model,
        &BoundParameters::bind(
            variant,
            ParameterScope::Instance,
            [
                ("l", ParameterValue::Real(row[3])),
                ("w", ParameterValue::Real(row[4])),
                ("m", ParameterValue::Real(row[5])),
                ("nf", ParameterValue::Real(row[6])),
            ],
        )?,
    )?;
    Ok((model, instance))
}

fn close(value: f64, reference: f64, relative: f64, absolute: f64, context: impl std::fmt::Debug) {
    assert!(
        (value - reference).abs() <= relative.mul_add(value.abs().max(reference.abs()), absolute),
        "{context:?}: {value:e} vs {reference:e}"
    );
}

fn compare_circuit(fixture: &str, expected_count: usize, mechanism: Mechanism) -> TestResult {
    let mut count = 0;
    for (index, line) in fixture.lines().filter(|l| !l.starts_with('#')).enumerate() {
        let row: [f64; 15] = line
            .split_whitespace()
            .map(str::parse::<f64>)
            .collect::<Result<Vec<_>, _>>()?
            .try_into()
            .map_err(|_| "native access fixture width")?;
        let (model, instance) = prepare(&row, mechanism)?;
        let channel = prepare_hisim_hv_normal_channel(&model, &instance)?;
        let temperature = prepare_hisim_hv_transport(&model, &instance, row[2] + 273.15, 0.)?;
        let intrinsic = HiSimHvIntrinsicBias {
            vgs: row[8],
            vds: row[9],
            vbs: row[10],
        };
        let point = channel
            .evaluate_with_access(
                intrinsic,
                HiSimHvExternalBias {
                    vgs: intrinsic.vgs,
                    vds: intrinsic.vds,
                    vbs: intrinsic.vbs,
                },
                &temperature,
            )?
            .access;
        let slope = point.current.electrical_derivative_a_per_v;
        let actual = [
            point.current.current_a,
            slope.inner_vgs + slope.outer_vgs,
            slope.inner_vds + slope.outer_vds,
            slope.inner_vbs + slope.outer_vbs,
        ];
        for (field, value) in actual.into_iter().enumerate() {
            // The converged native current itself is tightly qualified; its
            // independently differenced slopes carry finite-step error.
            let relative = if field == 0 { 5e-7 } else { 2e-5 };
            let absolute = if field == 0 { 1e-19 } else { 2e-16 };
            close(
                value,
                row[11 + field],
                relative,
                absolute,
                (index, field, &row[..11]),
            );
        }
        count += 1;
    }
    assert_eq!(count, expected_count);
    Ok(())
}

#[test]
fn native_access_corrected_current_and_tied_terminal_derivatives() -> TestResult {
    compare_circuit(
        include_str!("fixtures/hisim-hv-access-circuit-ngspice47.txt"),
        1536,
        Mechanism::Access,
    )
}

#[test]
fn native_sti_current_and_tied_terminal_derivatives() -> TestResult {
    compare_circuit(
        include_str!("fixtures/hisim-hv-sti-circuit-ngspice47.txt"),
        1920,
        Mechanism::Sti,
    )
}

#[test]
fn independent_access_coordinates_and_near_zero_smoothing() -> TestResult {
    let row = [
        2., 1., 27., 3e-6, 10e-6, 3., 2., 1., 0., 0., 0., 0., 0., 0., 0.,
    ];
    let (model, instance) = prepare(&row, Mechanism::Access)?;
    let access = prepare_hisim_hv_access_correction(&model, &instance)?;
    for outer_drain in [-0.1, 1e-12, 1e-11, 4e-11, 0.2, 2.] {
        let coordinates = [3., 0.2, -0.1, 310., 4., outer_drain, -0.3];
        let evaluate = |values: [f64; 7]| {
            let grad: [f64; 4] = [1e-4, 2e-4, 5e-5, -2e-7];
            let leakage_grad: [f64; 4] = [0., 1e-7, 0., 0.];
            let preaccess = grad.iter().enumerate().fold(4e-4, |sum, (i, g)| {
                g.mul_add(values[i] - coordinates[i], sum)
            });
            let leakage = leakage_grad.iter().enumerate().fold(3e-8, |sum, (i, g)| {
                g.mul_add(values[i] - coordinates[i], sum)
            });
            access.evaluate(&HiSimHvAccessInputs {
                intrinsic: HiSimHvIntrinsicBias {
                    vgs: values[0],
                    vds: values[1],
                    vbs: values[2],
                },
                external: HiSimHvExternalBias {
                    vgs: values[4],
                    vds: values[5],
                    vbs: values[6],
                },
                preaccess_current_a: Jet {
                    value: preaccess,
                    gradient: grad,
                },
                postaccess_current_a: Jet {
                    value: leakage,
                    gradient: leakage_grad,
                },
                conducting_channel: true,
            })
        };
        let point = evaluate(coordinates)?;
        let g = point.current.electrical_derivative_a_per_v;
        let gradients = [
            g.inner_vgs,
            g.inner_vds,
            g.inner_vbs,
            point.current.temperature_derivative_a_per_kelvin,
            g.outer_vgs,
            g.outer_vds,
            g.outer_vbs,
        ];
        let r = point.resistance_derivative_ohm_per_v;
        let resistance_gradients = [
            r.inner_vgs,
            r.inner_vds,
            r.inner_vbs,
            0.,
            r.outer_vgs,
            r.outer_vds,
            r.outer_vbs,
        ];
        for axis in 0..7 {
            let step = if axis == 5 && outer_drain.abs() < 1e-10 {
                1e-13
            } else {
                1e-5
            };
            let mut hi = coordinates;
            let mut lo = coordinates;
            hi[axis] += step;
            lo[axis] -= step;
            let high = evaluate(hi)?;
            let low = evaluate(lo)?;
            hi[axis] += step;
            lo[axis] -= step;
            let higher = evaluate(hi)?;
            let lower = evaluate(lo)?;
            close(
                gradients[axis],
                8.0f64.mul_add(
                    high.current.current_a - low.current.current_a,
                    -(higher.current.current_a - lower.current.current_a),
                ) / (12. * step),
                2e-3,
                1e-9,
                (outer_drain, axis, "current"),
            );
            close(
                resistance_gradients[axis],
                8.0f64.mul_add(
                    high.resistance_ohm - low.resistance_ohm,
                    -(higher.resistance_ohm - lower.resistance_ohm),
                ) / (12. * step),
                2e-6,
                1e-8,
                (outer_drain, axis, "resistance"),
            );
        }
    }
    Ok(())
}

#[test]
fn sti_current_has_independent_temperature_and_bias_gradients() -> TestResult {
    for family in [1., 2.] {
        for polarity in [-1., 1.] {
            for config in [0., 1., 2.] {
                let row = [
                    family, polarity, 27., 3e-6, 10e-6, 2., 1., config, 0., 0., 0., 0., 0., 0., 0.,
                ];
                let (model, instance) = prepare(&row, Mechanism::Sti)?;
                let channel = prepare_hisim_hv_normal_channel(&model, &instance)?;
                let evaluate = |coordinates: [f64; 4]| -> Result<Jet, ModelError> {
                    let transport =
                        prepare_hisim_hv_transport(&model, &instance, coordinates[3], 0.)?;
                    Ok(channel
                        .evaluate(
                            HiSimHvIntrinsicBias {
                                vgs: coordinates[0],
                                vds: coordinates[1],
                                vbs: coordinates[2],
                            },
                            &transport,
                        )?
                        .sti_current_a)
                };
                for gate in [3., 5., 8.] {
                    for drain in [0.1, 1., 5.] {
                        let coordinates = [gate, drain, -0.3, 315.];
                        let point = evaluate(coordinates)?;
                        for axis in 0..4 {
                            let step = if axis == 3 { 0.01 } else { 1e-5 };
                            let mut high = coordinates;
                            let mut low = coordinates;
                            high[axis] += step;
                            low[axis] -= step;
                            let numerical =
                                (evaluate(high)?.value - evaluate(low)?.value) / (2. * step);
                            close(
                                point.gradient[axis],
                                numerical,
                                1e-5,
                                1e-16,
                                (family, polarity, config, coordinates, axis),
                            );
                        }
                    }
                }
            }
        }
    }
    Ok(())
}
