//! Native composed gate and junction leakage, plus independent local derivatives.
use model_compact_intrinsic::{
    BoundParameters, HiSimHvExternalBias, HiSimHvIntrinsicBias, HiSimHvLeakagePoint, ModelError,
    ModelVariant, ParameterScope, ParameterValue, PreparedHiSimHvInstance, PreparedHiSimHvModel,
    prepare_hisim_hv_instance, prepare_hisim_hv_model, prepare_hisim_hv_normal_channel,
    prepare_hisim_hv_transport,
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
        (
            "coiigs",
            ParameterValue::Integer(i32::from(matches!(row[7], 0. | 2.))),
        ),
        (
            "cogidl",
            ParameterValue::Integer(i32::from(matches!(row[7], 1. | 2.))),
        ),
    ];
    for name in ["corsrd", "coisti", "coisub", "coadov", "coovlp", "conqs"] {
        fields.push((name, ParameterValue::Integer(0)));
    }
    if variant == ModelVariant::HiSimHv2 {
        fields.push(("cordrift", ParameterValue::Integer(0)));
    }
    fields.extend(
        [
            ("tox", 8e-9),
            ("vfbc", -0.8),
            ("eg0", 1.2),
            ("egig", 0.06),
            ("igtemp2", 110.),
            ("igtemp3", 1300.),
            ("gleak1", 50.),
            ("gleak2", 1e7),
            ("gleak3", 0.06),
            ("gleak4", 4.),
            ("gleak5", 7500.),
            ("gleak6", 0.25),
            ("gleak7", 1e-6),
            ("glkb1", 5e-16),
            ("glkb2", 1.),
            ("glkb3", 0.1),
            ("fn1", 50.),
            ("fn2", 170e-6),
            ("fn3", 0.),
            ("fvbs", 0.012),
            ("glksd1", 2.5e-15),
            ("glksd2", 1e3),
            ("glksd3", -1e3),
            ("glpart1", 0.35),
            ("gidl1", 2.),
            ("gidl2", 3e7),
            ("gidl3", 0.9),
            ("gidl4", 0.15),
            ("gidl5", 0.2),
            ("lgidl1", 0.8),
            ("wgidl1", 1.2),
            ("pgidl1", 0.3),
            ("lgidl2", 2e6),
            ("wgidl2", 3e6),
            ("pgidl2", 1e6),
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

fn currents(p: HiSimHvLeakagePoint) -> [model_compact_intrinsic::HiSimHvIntrinsicScalar; 5] {
    [
        p.gate_to_drain,
        p.gate_to_source,
        p.gate_to_body,
        p.drain_to_body,
        p.source_to_body,
    ]
}

fn close(value: f64, reference: f64, relative: f64, absolute: f64, context: impl std::fmt::Debug) {
    assert!(
        (value - reference).abs() <= relative.mul_add(value.abs().max(reference.abs()), absolute),
        "{context:?}: {value:e} vs {reference:e}"
    );
}

#[test]
fn native_terminal_leakage() -> TestResult {
    for (index, line) in include_str!("fixtures/hisim-hv-leakage-ngspice47.txt")
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
            .map_err(|_| "native leakage fixture width")?;
        let (model, instance) = prepare(&row)?;
        let channel = prepare_hisim_hv_normal_channel(&model, &instance)?;
        let temp = prepare_hisim_hv_transport(&model, &instance, row[2] + 273.15, 0.)?;
        let bias = HiSimHvIntrinsicBias {
            vgs: row[8],
            vds: row[9],
            vbs: row[10],
        };
        let result = channel.evaluate_with_parasitics(
            bias,
            HiSimHvExternalBias {
                vgs: bias.vgs,
                vds: bias.vds,
                vbs: bias.vbs,
            },
            0.,
            &temp,
        )?;
        for (i, p) in currents(result.leakage).iter().enumerate() {
            close(p.value, row[11 + i], 5e-7, 1e-19, (index, i, &row[..11]));
        }
    }
    Ok(())
}

#[test]
fn local_terminal_leakage_derivatives() -> TestResult {
    for family in [1., 2.] {
        for polarity in [-1., 1.] {
            for config in [0., 1., 2.] {
                let (model, instance) =
                    prepare(&[family, polarity, 27., 4e-6, 30e-6, 3., 3., config])?;
                let channel = prepare_hisim_hv_normal_channel(&model, &instance)?;
                let evaluate = |x: [f64; 4]| -> Result<_, ModelError> {
                    let temp = prepare_hisim_hv_transport(&model, &instance, x[3], 0.)?;
                    let bias = HiSimHvIntrinsicBias {
                        vgs: x[0],
                        vds: x[1],
                        vbs: x[2],
                    };
                    Ok(currents(
                        channel
                            .evaluate_with_parasitics(
                                bias,
                                HiSimHvExternalBias {
                                    vgs: x[0],
                                    vds: x[1],
                                    vbs: x[2],
                                },
                                0.,
                                &temp,
                            )?
                            .leakage,
                    ))
                };
                for g in [-2., 2., 5., 8.] {
                    for d in [0.1, 1., 5., 15.] {
                        let x = [g, d, -0.5, 340.];
                        let p = evaluate(x)?;
                        for axis in 0..4 {
                            let h = if axis == 3 { 0.01 } else { 1e-5 };
                            let mut a = x;
                            let mut b = x;
                            a[axis] += h;
                            b[axis] -= h;
                            let plus = evaluate(a)?;
                            let minus = evaluate(b)?;
                            for i in 0..5 {
                                close(
                                    p[i].gradient[axis],
                                    (plus[i].value - minus[i].value) / (2. * h),
                                    3e-5,
                                    1e-17,
                                    (family, polarity, config, x, axis, i),
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
