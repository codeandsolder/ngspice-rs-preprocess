//! Native AC response and independent generalized equation Jacobians.
use model_compact_intrinsic::{
    BoundParameters, HiSimHvExternalBias, HiSimHvIntrinsicBias, HiSimHvNqsEquilibrium,
    HiSimHvNqsPoint, HiSimHvNqsState, ModelError, ModelVariant, ParameterScope, ParameterValue,
    PreparedHiSimHvInstance, PreparedHiSimHvModel, prepare_hisim_hv_instance,
    prepare_hisim_hv_model, prepare_hisim_hv_normal_channel, prepare_hisim_hv_transport,
};
type TestResult = Result<(), Box<dyn std::error::Error>>;

fn prepare(
    row: &[f64],
    enabled: bool,
) -> Result<(PreparedHiSimHvModel, PreparedHiSimHvInstance), ModelError> {
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
        ("conqs", ParameterValue::Integer(i32::from(enabled))),
    ];
    for name in [
        "corsrd", "coisti", "coisub", "coadov", "coovlp", "coovlps", "coiigs", "cogidl",
    ] {
        fields.push((name, ParameterValue::Integer(0)));
    }
    if variant == ModelVariant::HiSimHv2 {
        fields.push(("cordrift", ParameterValue::Integer(0)));
    }
    let [dly1, dly2, dly3] = if row[7] == 0. {
        [100e-12, 0.7, 0.8e-6]
    } else {
        [300e-12, 0.2, 2e-6]
    };
    fields.extend(
        [
            ("dly1", dly1),
            ("dly2", dly2),
            ("dly3", dly3),
            ("xqy", 0.),
            ("xqy1", 0.),
        ]
        .map(|(n, v)| (n, ParameterValue::Real(v))),
    );
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
fn equilibrium(point: HiSimHvNqsEquilibrium) -> HiSimHvNqsState {
    HiSimHvNqsState {
        inversion_charge_c: point.inversion_charge_c.value,
        bulk_charge_c: point.bulk_charge_c.value,
    }
}
fn axis(gradient: &[f64], excitation: f64, reverse: bool) -> f64 {
    match excitation {
        0. => gradient[0],
        1. if reverse => -gradient[0] - gradient[1] - gradient[2],
        1. => gradient[1],
        _ => gradient[2],
    }
}
#[test]
fn paired_native_nqs_ac_response() -> TestResult {
    let mut count = 0;
    let mut failures = 0;
    let mut worst = 0_f64;
    for (index, line) in include_str!("fixtures/hisim-hv-nqs-ngspice47.txt")
        .lines()
        .enumerate()
    {
        if line.starts_with('#') {
            continue;
        }
        let row: [f64; 22] = line
            .split_whitespace()
            .map(str::parse)
            .collect::<Result<Vec<_>, _>>()?
            .try_into()
            .map_err(|_| "native NQS fixture width")?;
        let (model, instance) = prepare(&row, true)?;
        let channel = prepare_hisim_hv_normal_channel(&model, &instance)?;
        let temp = prepare_hisim_hv_transport(&model, &instance, row[2] + 273.15, 0.)?;
        let bias = HiSimHvIntrinsicBias {
            vgs: row[10],
            vds: row[11],
            vbs: row[12],
        };
        let outer = HiSimHvExternalBias {
            vgs: bias.vgs,
            vds: bias.vds,
            vbs: bias.vbs,
        };
        let reverse = row[8] == 1.;
        let state = channel.evaluate_with_parasitics(bias, outer, 0., reverse, &temp)?;
        let eq = state.nqs.ok_or("NQS target missing")?;
        let p = eq.evaluate(equilibrium(eq))?;
        let omega = 2. * std::f64::consts::PI * row[13];
        let response = |r: model_compact_intrinsic::HiSimHvNqsScalar, coordinate: usize| {
            let dc = -axis(&r.gradient, row[9], reverse) / r.gradient[coordinate];
            let x = omega / r.gradient[coordinate];
            let re = dc / (1. + x * x);
            (re, -x * re)
        };
        let (ir, ii) = response(p.inversion_residual_a, 4);
        let (br, bi) = response(p.bulk_residual_a, 5);
        let q = state.channel.intrinsic.electrostatics.terminal_charges;
        let mut rows = [
            (p.drain_charge_c, q.drain),
            (p.source_charge_c, q.source),
            (p.body_charge_c, q.bulk),
            (p.gate_charge_c, q.gate),
        ];
        if reverse {
            rows.swap(0, 1);
        }
        for (i, (dynamic, qs)) in rows.into_iter().enumerate() {
            let real = axis(&dynamic.gradient, row[9], reverse)
                + dynamic.gradient[4] * ir
                + dynamic.gradient[5] * br
                - axis(&qs.gradient, row[9], reverse);
            let imag = dynamic.gradient[4] * ii + dynamic.gradient[5] * bi;
            for (part, actual) in [omega * imag, -omega * real].into_iter().enumerate() {
                let expected = row[14 + 2 * i + part];
                let error = (actual - expected).abs();
                let relative = error / actual.abs().max(expected.abs()).max(1e-12);
                if error > 5e-6 * actual.abs().max(expected.abs()) + 1e-12 {
                    failures += 1;
                    if failures <= 20 || relative > worst {
                        eprintln!(
                            "{index} {:?} field={i},{part} actual={actual:.16e} native={expected:.16e} rel={relative:.3e}",
                            &row[..14]
                        );
                    }
                    worst = worst.max(relative);
                }
            }
        }
        count += 1;
    }
    assert_eq!(count, 11520);
    assert_eq!(failures, 0, "worst relative {worst}");
    Ok(())
}

#[test]
fn independent_nqs_trial_rows() -> TestResult {
    for family in [1., 2.] {
        for config in [0., 1.] {
            let (model, instance) = prepare(&[family, 1., 27., 4e-6, 30e-6, 3., 3., config], true)?;
            let channel = prepare_hisim_hv_normal_channel(&model, &instance)?;
            let evaluate = |x: [f64; 6]| -> Result<HiSimHvNqsPoint, Box<dyn std::error::Error>> {
                let b = HiSimHvIntrinsicBias {
                    vgs: x[0],
                    vds: x[1],
                    vbs: x[2],
                };
                let outer = HiSimHvExternalBias {
                    vgs: b.vgs,
                    vds: b.vds,
                    vbs: b.vbs,
                };
                let t = prepare_hisim_hv_transport(&model, &instance, x[3], 0.)?;
                let state = channel.evaluate_with_parasitics(b, outer, 0., false, &t)?;
                Ok(state
                    .nqs
                    .ok_or("NQS target missing")?
                    .evaluate(HiSimHvNqsState {
                        inversion_charge_c: x[4],
                        bulk_charge_c: x[5],
                    })?)
            };
            let rows = |p: HiSimHvNqsPoint| {
                [
                    p.inversion_residual_a,
                    p.bulk_residual_a,
                    p.inversion_state_c,
                    p.bulk_state_c,
                    p.gate_charge_c,
                    p.drain_charge_c,
                    p.source_charge_c,
                    p.body_charge_c,
                ]
            };
            for g in [-2., 0.2, 1.3, 3., 5., 8.] {
                for d in [0.1, 1.6, 8.] {
                    let x = [g, d, -0.23, 315., -2e-13, -1e-13];
                    let values = rows(evaluate(x)?);
                    for i in 0..6 {
                        let h = match i {
                            3 => 0.1,
                            4 | 5 => 1e-17,
                            _ => 1e-3,
                        };
                        let mut a = x;
                        let mut b = x;
                        a[i] += h;
                        b[i] -= h;
                        let high = rows(evaluate(a)?);
                        let low = rows(evaluate(b)?);
                        a[i] = x[i] + 2. * h;
                        b[i] = x[i] - 2. * h;
                        let high2 = rows(evaluate(a)?);
                        let low2 = rows(evaluate(b)?);
                        for (j, p) in values.iter().enumerate() {
                            let floor = if j < 2 { 1e-10 } else { 1e-23 };
                            let rounding = 8.
                                * f64::EPSILON
                                * (8. * (high[j].value.abs() + low[j].value.abs())
                                    + high2[j].value.abs()
                                    + low2[j].value.abs())
                                / (12. * h);
                            close(
                                p.gradient[i],
                                (8. * (high[j].value - low[j].value)
                                    - (high2[j].value - low2[j].value))
                                    / (12. * h),
                                3e-5,
                                floor + rounding,
                                (family, config, x, i, j),
                            );
                        }
                    }
                    close(values[4..].iter().map(|p| p.value).sum(), 0., 0., 1e-27, x);
                    for i in 0..6 {
                        close(
                            values[4..].iter().map(|p| p.gradient[i]).sum(),
                            0.,
                            0.,
                            1e-15,
                            (x, i),
                        );
                    }
                }
            }
            let (model, instance) =
                prepare(&[family, 1., 27., 4e-6, 30e-6, 3., 3., config], false)?;
            let channel = prepare_hisim_hv_normal_channel(&model, &instance)?;
            let b = HiSimHvIntrinsicBias {
                vgs: 3.,
                vds: 2.,
                vbs: 0.,
            };
            let t = prepare_hisim_hv_transport(&model, &instance, 300., 0.)?;
            assert!(
                channel
                    .evaluate_with_parasitics(
                        b,
                        HiSimHvExternalBias {
                            vgs: 3.,
                            vds: 2.,
                            vbs: 0.
                        },
                        0.,
                        false,
                        &t
                    )?
                    .nqs
                    .is_none()
            );
        }
    }
    Ok(())
}
