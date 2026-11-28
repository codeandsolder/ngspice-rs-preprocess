//! Native paired overlap-charge operating points and independent derivatives.
use model_compact_intrinsic::{
    BoundParameters, HiSimHvExternalBias, HiSimHvIntrinsicBias, HiSimHvOverlapBias, ModelError,
    ModelVariant, ParameterScope, ParameterValue, PreparedHiSimHvInstance, PreparedHiSimHvModel,
    prepare_hisim_hv_instance, prepare_hisim_hv_model, prepare_hisim_hv_normal_channel,
    prepare_hisim_hv_overlap, prepare_hisim_hv_transport,
};
type TestResult = Result<(), Box<dyn std::error::Error>>;
fn prepare(row: &[f64]) -> Result<(PreparedHiSimHvModel, PreparedHiSimHvInstance), ModelError> {
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
        ("coadov", ParameterValue::Integer(1)),
    ];
    for name in ["corsrd", "coisti", "coisub", "conqs", "coiigs", "cogidl"] {
        fields.push((name, ParameterValue::Integer(0)));
    }
    if variant == ModelVariant::HiSimHv2 {
        fields.push(("cordrift", ParameterValue::Integer(0)));
    }
    let (drain, source, mode, nover, novers, external) = match row[7] {
        0. => (1, 0, 1, 0., 5e17, 0.),
        1. => (0, 1, 1, 1e17, 0., 0.),
        2. => (1, 1, 0, 1e17, 5e17, 0.),
        3. => (1, 1, 1, 1e17, 5e17, 0.),
        4. => (1, 1, 2, 1e17, 5e17, 0.),
        5. => (1, 1, 1, 2e17, 3e17, 0.3),
        _ => return Err(ModelError::InvalidPhysicalParameter("overlap fixture card")),
    };
    fields.extend(
        [("coovlp", drain), ("coovlps", source), ("coqovsm", mode)]
            .map(|(n, v)| (n, ParameterValue::Integer(v))),
    );
    fields.extend(
        [
            ("xqy", 0.),
            ("xqy1", 0.),
            ("cgdo", 0.),
            ("cgso", 0.),
            ("cgbo", 0.),
            ("tpoly", 0.),
            ("loverld", 5e-7),
            ("lovers", 3e-7),
            ("nover", nover),
            ("novers", novers),
            ("cvdsover", external),
        ]
        .map(|(n, v)| (n, ParameterValue::Real(v))),
    );
    if variant == ModelVariant::HiSimHv2 && row[7] == 5. {
        fields.push(("qovadd", ParameterValue::Real(0.001)));
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
            [("l", row[3]), ("w", row[4]), ("m", row[5]), ("nf", row[6])]
                .map(|(n, v)| (n, ParameterValue::Real(v))),
        )?,
    )?;
    Ok((model, instance))
}
fn close(a: f64, b: f64, relative: f64, absolute: f64, ctx: impl std::fmt::Debug) {
    assert!(
        (a - b).abs() <= relative.mul_add(a.abs().max(b.abs()), absolute),
        "{ctx:?}: {a:e} vs {b:e}"
    );
}

fn native_overlap_charge(fixture: &str, reversed: bool, expected_count: usize) -> TestResult {
    let mut count = 0;
    let mut failures = 0;
    let mut worst = 0_f64;
    for (index, line) in fixture.lines().enumerate() {
        if line.starts_with('#') {
            continue;
        }
        let row: [f64; 14] = line
            .split_whitespace()
            .map(str::parse)
            .collect::<Result<Vec<_>, _>>()?
            .try_into()
            .map_err(|_| "native overlap fixture width")?;
        let (model, instance) = prepare(&row)?;
        let channel = prepare_hisim_hv_normal_channel(&model, &instance)?;
        let overlap = prepare_hisim_hv_overlap(&model, &instance)?;
        let temp = prepare_hisim_hv_transport(&model, &instance, row[2] + 273.15, 0.)?;
        let intrinsic = HiSimHvIntrinsicBias {
            vgs: row[8],
            vds: row[9],
            vbs: row[10],
        };
        let external = HiSimHvExternalBias {
            vgs: row[8],
            vds: row[9],
            vbs: row[10],
        };
        let state = channel.evaluate(intrinsic, &temp)?;
        let q = overlap.evaluate(
            HiSimHvOverlapBias {
                intrinsic,
                external,
                reversed,
            },
            &state.electrostatics,
            &temp.temperature,
        )?;
        for (field, actual) in [
            q.intrinsic.gate.value + q.external.gate.value,
            if reversed {
                q.intrinsic.source.value
            } else {
                q.intrinsic.drain.value
            },
            q.intrinsic.bulk.value + q.external.bulk.value,
        ]
        .into_iter()
        .enumerate()
        {
            let expected = row[11 + field];
            let error = (actual - expected).abs();
            let relative = error / actual.abs().max(expected.abs()).max(1e-27);
            if error > 5e-7 * actual.abs().max(expected.abs()) + 2e-26 {
                failures += 1;
                if failures <= 25 || relative > worst {
                    eprintln!(
                        "{index} {:?} field={field} actual={actual:.16e} native={expected:.16e} rel={relative:.3e}",
                        &row[..11]
                    );
                }
                worst = worst.max(relative);
            }
        }
        for charges in [q.intrinsic, q.external] {
            close(
                charges.gate.value
                    + charges.drain.value
                    + charges.source.value
                    + charges.bulk.value,
                0.,
                0.,
                1e-26,
                (index, "conservation"),
            );
        }
        count += 1;
    }
    assert_eq!(count, expected_count);
    assert_eq!(failures, 0, "worst relative {worst}");
    Ok(())
}

#[test]
fn paired_native_overlap_charge() -> TestResult {
    native_overlap_charge(
        include_str!("fixtures/hisim-hv-overlap-circuit-ngspice47.txt"),
        false,
        6720,
    )
}
#[test]
fn paired_native_reversed_overlap_charge() -> TestResult {
    native_overlap_charge(
        include_str!("fixtures/hisim-hv-overlap-reverse-ngspice47.txt"),
        true,
        3840,
    )
}
#[test]
fn independent_overlap_coordinates_and_orientation() -> TestResult {
    for family in [1., 2.] {
        for config in [0., 1., 2., 3., 4., 5.] {
            let (model, instance) = prepare(&[family, 1., 27., 4e-6, 30e-6, 3., 3., config])?;
            let channel = prepare_hisim_hv_normal_channel(&model, &instance)?;
            let overlap = prepare_hisim_hv_overlap(&model, &instance)?;
            for reversed in [false, true] {
                let evaluate = |x: [f64; 7]| {
                    let intrinsic = HiSimHvIntrinsicBias {
                        vgs: x[0],
                        vds: x[1],
                        vbs: x[2],
                    };
                    let external = HiSimHvExternalBias {
                        vgs: x[4],
                        vds: x[5],
                        vbs: x[6],
                    };
                    let t = prepare_hisim_hv_transport(&model, &instance, x[3], 0.)?;
                    let state = channel.evaluate(intrinsic, &t)?;
                    overlap.evaluate(
                        HiSimHvOverlapBias {
                            intrinsic,
                            external,
                            reversed,
                        },
                        &state.electrostatics,
                        &t.temperature,
                    )
                };
                for gate in [-2., 0.2, 1.3, 3., 7.] {
                    for drain in [0.12, 1.6, 8.] {
                        let x = [gate, drain, -0.23, 315., gate + 0.07, drain + 0.25, -0.31];
                        let point = evaluate(x)?;
                        let rows = |q: model_compact_intrinsic::HiSimHvChargeNetwork| {
                            [
                                q.intrinsic.gate,
                                q.intrinsic.drain,
                                q.intrinsic.source,
                                q.intrinsic.bulk,
                                q.external.gate,
                                q.external.drain,
                                q.external.source,
                                q.external.bulk,
                                q.gate_drain_prime,
                                q.gate_source_prime,
                            ]
                        };
                        let values = rows(point);
                        for axis in 0..7 {
                            let h = if axis == 3 { 0.01 } else { 1e-4 };
                            let mut a = x;
                            let mut b = x;
                            a[axis] += h;
                            b[axis] -= h;
                            let high = rows(evaluate(a)?);
                            let low = rows(evaluate(b)?);
                            for (i, p) in values.iter().enumerate() {
                                let derivative = match (i, axis) {
                                    (0..=3, 0..=3) => p.gradient[axis],
                                    (4..=9, 3) => p.gradient[3],
                                    (4..=9, 4..=6) => p.gradient[axis - 4],
                                    _ => 0.,
                                };
                                // The analytical overlap modes subtract total and
                                // depletion charge; their small remainder inherits
                                // rounding from both terms in the same vector.
                                let range = match i {
                                    0..=3 => 0..4,
                                    4..=7 => 4..8,
                                    _ => i..i + 1,
                                };
                                let roundoff = range
                                    .map(|j| high[j].value.abs() + low[j].value.abs())
                                    .sum::<f64>();
                                close(
                                    derivative,
                                    (high[i].value - low[i].value) / (2. * h),
                                    3e-5,
                                    1e-24 + 8. * f64::EPSILON * roundoff / (2. * h),
                                    (family, config, reversed, x, i, axis),
                                );
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

#[test]
fn native_constant_and_fringe_ac_network() -> TestResult {
    let mut count = 0;
    for (index, line) in include_str!("fixtures/hisim-hv-constant-capacitance-ngspice47.txt")
        .lines()
        .enumerate()
    {
        if line.starts_with('#') {
            continue;
        }
        let row: [f64; 16] = line
            .split_whitespace()
            .map(str::parse)
            .collect::<Result<Vec<_>, _>>()?
            .try_into()
            .map_err(|_| "native AC fixture width")?;
        let variant = if row[0] == 1. {
            ModelVariant::HiSimHv1
        } else {
            ModelVariant::HiSimHv2
        };
        let mut fields = vec![
            (
                if row[1] < 0. { "pmos" } else { "nmos" },
                ParameterValue::Flag(true),
            ),
            ("coadov", ParameterValue::Integer(1)),
        ];
        for name in [
            "corsrd", "coisti", "coisub", "coovlp", "coovlps", "conqs", "coiigs", "cogidl",
        ] {
            fields.push((name, ParameterValue::Integer(0)));
        }
        if variant == ModelVariant::HiSimHv2 {
            fields.push(("cordrift", ParameterValue::Integer(0)));
        }
        fields.extend(
            [
                ("xqy", 0.),
                ("xqy1", 0.),
                ("loverld", 5e-7),
                ("lovers", 3e-7),
                (
                    "cgbo",
                    if row[7] == 0. || row[7] == 3. {
                        3e-10
                    } else {
                        0.
                    },
                ),
                ("tpoly", if row[7] >= 2. { 150e-9 } else { 0. }),
            ]
            .map(|(n, v)| (n, ParameterValue::Real(v))),
        );
        if row[7] != 1. {
            fields.extend(
                [
                    ("cgdo", if row[7] == 2. { 0. } else { 4e-10 }),
                    ("cgso", if row[7] == 2. { 0. } else { 2e-10 }),
                ]
                .map(|(n, v)| (n, ParameterValue::Real(v))),
            );
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
                [("l", row[3]), ("w", row[4]), ("m", row[5]), ("nf", row[6])]
                    .map(|(n, v)| (n, ParameterValue::Real(v))),
            )?,
        )?;
        let channel = prepare_hisim_hv_normal_channel(&model, &instance)?;
        let temp = prepare_hisim_hv_transport(&model, &instance, row[2] + 273.15, 0.)?;
        let intrinsic = HiSimHvIntrinsicBias {
            vgs: row[9],
            vds: row[10],
            vbs: row[11],
        };
        let external = HiSimHvExternalBias {
            vgs: row[9],
            vds: row[10],
            vbs: row[11],
        };
        let reverse = row[8] == 1.;
        let state = channel.evaluate_with_parasitics(intrinsic, external, 0., reverse, &temp)?;
        let core = state.channel.intrinsic.electrostatics.terminal_charges;
        let q = state.quasistatic_charge_network()?;
        let drain = q.gate_drain_prime.gradient[0];
        let source = q.gate_source_prime.gradient[0];
        let bulk = core.bulk.gradient[0] - q.intrinsic.bulk.gradient[0];
        let (d, s) = if reverse {
            (source, drain)
        } else {
            (drain, source)
        };
        for (i, cap) in [d, s, bulk, -d - s - bulk].into_iter().enumerate() {
            close(cap, row[12 + i], 5e-7, 1e-25, (index, i, &row[..12]));
        }
        count += 1;
    }
    assert_eq!(count, 1536);
    Ok(())
}
