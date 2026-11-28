//! HV2 physical drift branch against native measured prime-node voltages.
use model_compact_intrinsic::{
    BoundParameters, HiSimHvDriftBias, HiSimHvExternalBias, HiSimHvIntrinsicBias, ModelError,
    ModelVariant, ParameterScope, ParameterValue, PreparedHiSimHvInstance, PreparedHiSimHvModel,
    prepare_hisim_hv_drift, prepare_hisim_hv_instance, prepare_hisim_hv_model,
    prepare_hisim_hv_normal_channel, prepare_hisim_hv_transport,
};
type TestResult = Result<(), Box<dyn std::error::Error>>;
fn prepare(
    row: &[f64],
    zero_substrate: bool,
) -> Result<(PreparedHiSimHvModel, PreparedHiSimHvInstance), ModelError> {
    let variant = ModelVariant::HiSimHv2;
    let mut fields = vec![
        (
            if row[0] < 0. { "pmos" } else { "nmos" },
            ParameterValue::Flag(true),
        ),
        ("cordrift", ParameterValue::Integer(1)),
        (
            "cosubnode",
            ParameterValue::Integer(i32::from(row[7] == 1.)),
        ),
        ("coovlp", ParameterValue::Integer(i32::from(row[6] != 4.))),
        ("coadov", ParameterValue::Integer(i32::from(row[6] == 3.))),
        ("coqovsm", ParameterValue::Integer(i32::from(row[6] != 3.))),
    ];
    for name in ["corsrd", "coisti", "coisub", "coiigs", "cogidl", "conqs"] {
        fields.push((name, ParameterValue::Integer(0)));
    }
    fields.extend(
        [
            ("nover", 1e17),
            ("loverld", 5e-7),
            ("rsh", 10.),
            ("js0d", 0.),
            ("js0s", 0.),
            ("js0swd", 0.),
            ("js0sws", 0.),
        ]
        .map(|(n, v)| (n, ParameterValue::Real(v))),
    );
    let (cx, qover, bb) = match row[6] {
        0. => (0., 0., 1.),
        1. => (0.35, 1e5, 2.),
        2. => (0.5, 3e4, 1.5),
        3. | 4. => (0.2, 1e5, 1.),
        _ => return Err(ModelError::InvalidPhysicalParameter("drift fixture card")),
    };
    fields.extend(
        [("rdrcx", cx), ("rdrqover", qover), ("rdrbb", bb)]
            .map(|(n, v)| (n, ParameterValue::Real(v))),
    );
    if row[6] == 2. {
        fields.extend(
            [
                ("rdrbbtmp", 0.001),
                ("rdrmuetmp", 1.2),
                ("rdrvtmp", 0.6),
                ("rdrcar", 2e-8),
                ("rdrmuel", 0.3),
                ("rdrvmaxw", 0.4),
                ("rdrvmaxl", 0.2),
                ("rdrdl1", 1e-7),
                ("rdrdl2", 2e-7),
            ]
            .map(|(n, v)| (n, ParameterValue::Real(v))),
        );
    }
    if zero_substrate {
        fields.push(("nsubsub", ParameterValue::Real(0.)));
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
                ("l", row[2]),
                ("w", row[3]),
                ("m", row[4]),
                ("nf", row[5]),
                ("nrd", 2.),
                ("nrs", 1.),
            ]
            .map(|(n, v)| (n, ParameterValue::Real(v))),
        )?,
    )?;
    Ok((model, instance))
}
#[test]
fn native_physical_drift_branch() -> TestResult {
    compare_native(
        include_str!("fixtures/hisim-hv-drift-ngspice47.txt"),
        5120,
        false,
    )
}

#[test]
fn zero_substrate_doping() -> TestResult {
    compare_native(
        include_str!("fixtures/hisim-hv-drift-zero-substrate-ngspice47.txt"),
        16,
        true,
    )
}

fn compare_native(fixture: &str, expected_count: usize, zero_substrate: bool) -> TestResult {
    let mut count = 0;
    let mut failures = [0; 3];
    let mut worst = [0_f64; 3];
    for (index, line) in fixture.lines().enumerate() {
        if line.starts_with('#') {
            continue;
        }
        let row: [f64; 18] = line
            .split_whitespace()
            .map(str::parse)
            .collect::<Result<Vec<_>, _>>()?
            .try_into()
            .map_err(|_| "native drift fixture width")?;
        let (model, instance) = prepare(&row, zero_substrate)?;
        let channel = prepare_hisim_hv_normal_channel(&model, &instance)?;
        let drift = prepare_hisim_hv_drift(&model, &instance)?.ok_or("drift law disabled")?;
        let temp = prepare_hisim_hv_transport(&model, &instance, row[1] + 273.15, 0.)?;
        let reversed = row[8] == 1.;
        let (source, drain, gate, bulk, outer_drain) = if reversed {
            (
                row[13],
                row[14],
                row[9] - row[10],
                row[11] - row[10],
                -row[10],
            )
        } else {
            (row[14], row[13], row[9], row[11], row[10])
        };
        let intrinsic = HiSimHvIntrinsicBias {
            vgs: gate - source,
            vds: drain - source,
            vbs: bulk - source,
        };
        let external = HiSimHvExternalBias {
            vgs: row[9],
            vds: row[10],
            vbs: row[11],
        };
        let bias = HiSimHvDriftBias {
            intrinsic,
            external,
            reversed,
            drain_drop_v: outer_drain - row[13],
            source_drop_v: -row[14],
            substrate_voltage_v: if row[7] == 1. { Some(row[12]) } else { None },
        };
        let state = channel.evaluate_with_parasitics(
            intrinsic,
            external,
            bias.drain_drop_v,
            reversed,
            &temp,
        )?;
        let point = drift.evaluate(bias, state.physical_drain_overlap, &temp.temperature)?;
        for (i, (actual, expected)) in [
            (point.drain_current_a.value, row[15]),
            (point.source_current_a.value, row[16]),
            (state.channel.access.current.current_a, row[17]),
        ]
        .into_iter()
        .enumerate()
        {
            let error = (actual - expected).abs();
            let relative = error / actual.abs().max(expected.abs()).max(1e-16);
            if error > 5e-6 * actual.abs().max(expected.abs()) + 1e-15 {
                failures[i] += 1;
                if failures[i] <= 12 || relative > worst[i] {
                    eprintln!(
                        "{index} {:?} field={i} actual={actual:.16e} native={expected:.16e} rel={relative:.3e} rd={:.16e} PsLD={:.16e} QbLD={:.16e}",
                        &row[..15],
                        point.drain_resistance_ohm.value,
                        state.physical_drain_overlap.surface_potential.value,
                        state.physical_drain_overlap.depletion_density.value
                    );
                }
                worst[i] = worst[i].max(relative);
            }
        }
        count += 1;
    }
    assert_eq!(count, expected_count);
    assert_eq!(failures, [0; 3], "worst relative {worst:?}");
    Ok(())
}

#[test]
fn independent_drift_jacobians() -> TestResult {
    for card in 0..5 {
        for reversed in [false, true] {
            for substrate in [false, true] {
                let mut row = [0.; 18];
                row[..8].copy_from_slice(&[
                    1.,
                    27.,
                    4e-6,
                    12e-6,
                    2.,
                    2.,
                    f64::from(card),
                    if substrate { 1. } else { 0. },
                ]);
                let (model, instance) = prepare(&row, false)?;
                let channel = prepare_hisim_hv_normal_channel(&model, &instance)?;
                let drift = prepare_hisim_hv_drift(&model, &instance)?.ok_or("drift disabled")?;
                let eval = |x: [f64; 10]| -> Result<_, ModelError> {
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
                    let temperature = prepare_hisim_hv_transport(&model, &instance, x[3], 0.)?;
                    let state = channel.evaluate_with_parasitics(
                        intrinsic,
                        external,
                        x[7],
                        reversed,
                        &temperature,
                    )?;
                    drift.evaluate(
                        HiSimHvDriftBias {
                            intrinsic,
                            external,
                            reversed,
                            drain_drop_v: x[7],
                            source_drop_v: x[8],
                            substrate_voltage_v: substrate.then_some(x[9]),
                        },
                        state.physical_drain_overlap,
                        &temperature.temperature,
                    )
                };
                for drop in [-0.8, -0.02, 0.02, 0.8] {
                    let x = [4.1, 2.3, -0.4, 346., 4.5, 3.2, -0.5, drop, -0.03, -0.7];
                    let p = eval(x)?;
                    for axis in 0..10 {
                        let h = if axis == 3 { 1e-2 } else { 1e-5 };
                        let perturb = |offset: f64| {
                            let mut y = x;
                            y[axis] += offset * h;
                            eval(y)
                        };
                        let a = perturb(-2.)?;
                        let b = perturb(-1.)?;
                        let c = perturb(1.)?;
                        let d = perturb(2.)?;
                        for (name, value, av, bv, cv, dv) in [
                            (
                                "Rd",
                                p.drain_resistance_ohm,
                                a.drain_resistance_ohm.value,
                                b.drain_resistance_ohm.value,
                                c.drain_resistance_ohm.value,
                                d.drain_resistance_ohm.value,
                            ),
                            (
                                "Id",
                                p.drain_current_a,
                                a.drain_current_a.value,
                                b.drain_current_a.value,
                                c.drain_current_a.value,
                                d.drain_current_a.value,
                            ),
                            (
                                "Is",
                                p.source_current_a,
                                a.source_current_a.value,
                                b.source_current_a.value,
                                c.source_current_a.value,
                                d.source_current_a.value,
                            ),
                        ] {
                            let numerical = ((av - dv) + 8. * (cv - bv)) / (12. * h);
                            let analytical = value.gradient[axis];
                            let roundoff = 32. * f64::EPSILON * value.value.abs() / h;
                            assert!(
                                (numerical - analytical).abs()
                                    <= 2e-5 * numerical.abs().max(analytical.abs())
                                        + roundoff
                                        + 1e-12,
                                "card {card} reversed {reversed} substrate {substrate} drop {drop} {name} axis {axis}: AD={analytical:e} FD={numerical:e}"
                            );
                        }
                    }
                }
            }
        }
    }
    Ok(())
}
