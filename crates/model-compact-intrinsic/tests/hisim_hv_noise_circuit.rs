//! Source-resolved native noise references.
use model_compact_intrinsic::{
    BoundParameters, HiSimHvExternalBias, HiSimHvIntrinsicBias, ModelError, ModelVariant,
    ParameterScope, ParameterValue, PreparedHiSimHvInstance, PreparedHiSimHvModel,
    prepare_hisim_hv_instance, prepare_hisim_hv_model, prepare_hisim_hv_normal_channel,
    prepare_hisim_hv_transport,
};
type TestResult = Result<(), Box<dyn std::error::Error>>;
fn prepare(row: &[f64]) -> Result<(PreparedHiSimHvModel, PreparedHiSimHvInstance), ModelError> {
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
        ("coflick", ParameterValue::Integer(i32::from(row[7] != 2.))),
        ("cothrml", ParameterValue::Integer(i32::from(row[7] != 3.))),
        ("coign", ParameterValue::Integer(i32::from(row[7] != 2.))),
    ];
    for name in [
        "corsrd", "coisti", "coisub", "coadov", "coovlp", "coovlps", "coiigs", "cogidl", "conqs",
    ] {
        fields.push((name, ParameterValue::Integer(0)));
    }
    if variant == ModelVariant::HiSimHv2 {
        fields.push(("cordrift", ParameterValue::Integer(0)));
    }
    fields.extend([("xqy", 0.), ("xqy1", 0.)].map(|(n, v)| (n, ParameterValue::Real(v))));
    if row[7] == 1. {
        fields.extend(
            [
                ("nfalp", 2e-6),
                ("nftrp", 2e10),
                ("falph", 0.85),
                ("cit", 1e-8),
                ("bb", 1.5),
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
    Ok((model, instance))
}
#[test]
fn native_source_noise() -> TestResult {
    let mut count = 0;
    let mut failures = [0; 3];
    let mut worst = [0_f64; 3];
    for (index, line) in include_str!("fixtures/hisim-hv-noise-ngspice47.txt")
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
            .map_err(|_| "native noise fixture width")?;
        let (model, instance) = prepare(&row)?;
        let channel = prepare_hisim_hv_normal_channel(&model, &instance)?;
        let temp = prepare_hisim_hv_transport(&model, &instance, row[2] + 273.15, 0.)?;
        let bias = HiSimHvIntrinsicBias {
            vgs: row[9],
            vds: row[10],
            vbs: row[11],
        };
        let outer = HiSimHvExternalBias {
            vgs: bias.vgs,
            vds: bias.vds,
            vbs: bias.vbs,
        };
        // The native C transport subtracts two independently rounded surface
        // potentials before evaluating Idd. In almost depleted channels the
        // subtraction has very poor relative conditioning (Pds ~ 1e-13 V).
        // The Rust implicit difference solve avoids this loss. Bound the
        // rounding of the native endpoints; the noise current is squared in
        // the flicker law. Do not penalize the stable physical result.
        let op = channel.evaluate(bias, &temp)?;
        let q = op.electrostatics.charge;
        let endpoint_roundoff = if q.surface_difference.value > 0. {
            2. * f64::EPSILON * (q.source_surface.value.abs() + q.drain_surface.value.abs())
                / q.surface_difference.value
        } else {
            0.
        };
        let result = channel.evaluate_noise(bias, outer, 0., row[8] == 1., &temp, row[12])?;
        for (i, actual) in [result.thermal, result.flicker, result.induced_gate]
            .into_iter()
            .enumerate()
        {
            let expected = row[13 + i];
            let error = (actual - expected).abs();
            let relative = error / actual.abs().max(expected.abs()).max(1e-45);
            let roundoff = if i < 2 {
                2. * endpoint_roundoff + endpoint_roundoff * endpoint_roundoff
            } else {
                0.
            };
            if error > (5e-6 + roundoff) * actual.abs().max(expected.abs()) + 1e-45 {
                failures[i] += 1;
                if failures[i] <= 8 || relative > worst[i] {
                    eprintln!(
                        "{index} {:?} field={i} actual={actual:.16e} native={expected:.16e} rel={relative:.3e}",
                        &row[..13]
                    );
                }
                worst[i] = worst[i].max(relative);
            }
        }
        count += 1;
    }
    assert_eq!(count, 6144);
    assert_eq!(failures, [0; 3], "worst relative {worst:?}");
    Ok(())
}
