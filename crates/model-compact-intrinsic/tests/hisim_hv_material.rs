//! Native HiSIM-HV doping and electrostatic material preparation oracles.
use model_compact_intrinsic::{
    BoundParameters, ModelVariant, ParameterScope,
    ParameterValue::{Integer, Real},
    prepare_hisim_hv_electrostatic_material, prepare_hisim_hv_instance, prepare_hisim_hv_model,
    prepare_hisim_hv_temperature,
};
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
fn near(actual: f64, expected: f64, relative: f64, absolute: f64, context: impl std::fmt::Debug) {
    assert!(
        (actual - expected).abs() <= relative.mul_add(actual.abs().max(expected.abs()), absolute),
        "{context:?}: {actual:e} vs {expected:e}"
    );
}
fn prepared_material_context(
    variant: ModelVariant,
    fields: &[f64; 14],
) -> TestResult<(
    model_compact_intrinsic::PreparedHiSimHvModel,
    model_compact_intrinsic::PreparedHiSimHvInstance,
)> {
    let mut card = vec![
        ("codfm", Integer(1)),
        ("mphdfm", Real(0.)),
        ("nsubc", Real(1e16)),
        ("nsubp", Real(5e17)),
        ("npext", Real(3e17)),
        ("nsubpsti1", Real(8e-5)),
        ("nsubpsti2", Real(0.25)),
        ("nsubpsti3", Real(1.2)),
        ("nsubp0", Real(0.2)),
        ("nsubwp", Real(0.7)),
        ("nsubcw", Real(0.3)),
        ("nsubcwp", Real(0.8)),
        ("lp", Real(0.4e-6)),
        ("lpext", Real(0.15e-6)),
        ("saref", Real(0.5e-6)),
        ("sbref", Real(0.4e-6)),
        ("ddltslp", Real(0.7)),
        ("ddltict", Real(0.8)),
        ("ddltmax", Real(5.)),
        ("xl", Real(0.)),
        ("xw", Real(0.)),
        ("xld", Real(0.)),
        ("xldld", Real(0.)),
        ("ll", Real(0.)),
        ("xwd", Real(0.)),
        ("wl", Real(0.)),
    ];
    if variant == ModelVariant::HiSimHv2 {
        card.push(("coddlt", Integer(i32::from(fields[6] != 0.))));
    }
    let model = prepare_hisim_hv_model(&BoundParameters::bind(
        variant,
        ParameterScope::Model,
        card,
    )?)?;
    let mut instance_card = vec![
        ("l", Real(fields[1] * 1e-6)),
        ("w", Real(fields[2] * fields[3] * 1e-6)),
        ("nf", Real(fields[3])),
    ];
    if fields[4] != 0. {
        instance_card.extend([
            ("sa", Real(0.2e-6)),
            ("sb", Real(0.3e-6)),
            ("sd", Real(0.1e-6)),
        ]);
    }
    if fields[5] != 0. {
        instance_card.push(("nsubcdfm", Real(2e16)));
    }
    let instance = prepare_hisim_hv_instance(
        &model,
        &BoundParameters::bind(variant, ParameterScope::Instance, instance_card)?,
    )?;
    Ok((model, instance))
}
#[test]
fn native_doping_profile_and_temperature_derivatives() -> TestResult {
    let mut count = 0;
    for (index, line) in include_str!("fixtures/hisim-hv-material-ngspice47.txt")
        .lines()
        .filter(|line| !line.starts_with('#'))
        .enumerate()
    {
        let fields: [f64; 14] = line
            .split_whitespace()
            .map(str::parse::<f64>)
            .collect::<Result<Vec<_>, _>>()?
            .try_into()
            .map_err(|_| "material fixture record width")?;
        let variant = match fields[0] {
            1. => ModelVariant::HiSimHv1,
            2. => ModelVariant::HiSimHv2,
            _ => return Err("material reference family".into()),
        };
        let (model, instance) = prepared_material_context(variant, &fields)?;
        let material = prepare_hisim_hv_electrostatic_material(&model, &instance)?;
        let actual = [
            material.channel_doping,
            material.bulk_doping,
            material.pocket_doping,
            material.effective_pocket_doping,
            material.pocket_extension_doping,
            material.pocket_overlap_log,
            material.drain_smoothing_exponent,
        ];
        for (field, (value, reference)) in actual
            .into_iter()
            .zip(fields[7..].iter().copied())
            .enumerate()
        {
            near(value, reference, 2e-12, 1e-13, (index, field));
        }
        for kelvin in [230., 300.15, 410.] {
            let temperature = prepare_hisim_hv_temperature(&model, &instance, kelvin, 0.)?;
            let point = material.evaluate(&temperature)?;
            let step = 1e-3;
            let hi = material.evaluate(&prepare_hisim_hv_temperature(
                &model,
                &instance,
                kelvin + step,
                0.,
            )?)?;
            let lo = material.evaluate(&prepare_hisim_hv_temperature(
                &model,
                &instance,
                kelvin - step,
                0.,
            )?)?;
            let scalars = [
                (
                    point.twice_bulk_potential,
                    hi.twice_bulk_potential,
                    lo.twice_bulk_potential,
                ),
                (
                    point.poisson_coefficient,
                    hi.poisson_coefficient,
                    lo.poisson_coefficient,
                ),
                (
                    point.minority_coefficient,
                    hi.minority_coefficient,
                    lo.minority_coefficient,
                ),
                (
                    point.pocket_overlap_potential,
                    hi.pocket_overlap_potential,
                    lo.pocket_overlap_potential,
                ),
            ];
            for (field, (value, upper, lower)) in scalars.into_iter().enumerate() {
                near(
                    value.gradient[3],
                    (upper.value - lower.value) / (2. * step),
                    3e-6,
                    1e-28,
                    (index, field, kelvin),
                );
            }
        }
        count += 1;
    }
    assert_eq!(count, 192);
    Ok(())
}
#[test]
fn depletion_mode_material_uses_depletion_doping() -> TestResult {
    let variant = ModelVariant::HiSimHv2;
    let model = prepare_hisim_hv_model(&BoundParameters::bind(
        variant,
        ParameterScope::Model,
        [("codep", Integer(1)), ("ndepm", Real(8e16))],
    )?)?;
    let instance = prepare_hisim_hv_instance(
        &model,
        &BoundParameters::bind(
            variant,
            ParameterScope::Instance,
            [("l", Real(2e-6)), ("w", Real(10e-6))],
        )?,
    )?;
    let material = prepare_hisim_hv_electrostatic_material(&model, &instance)?;
    assert!(material.depletion_mode);
    near(
        material.depletion_doping,
        8e22,
        1e-14,
        0.,
        "depletion concentration",
    );
    for kelvin in [230., 300.15, 410.] {
        let t = prepare_hisim_hv_temperature(&model, &instance, kelvin, 0.)?;
        let point = material.evaluate(&t)?;
        let dep = point.depletion.ok_or("missing depletion point")?;
        let ni = t.intrinsic_carrier_concentration_m3;
        let beta = t.inverse_thermal_voltage;
        near(
            dep.twice_depletion_potential.value,
            2. / beta * (8e22 / ni).ln(),
            1e-14,
            0.,
            "Pb2n",
        );
        near(
            dep.built_in_potential.value,
            (8e22 * material.channel_doping / ni.powi(2)).ln() / beta,
            1e-14,
            0.,
            "Vbipn",
        );
        near(
            point.minority_coefficient.value,
            (ni / 8e22).powi(2),
            1e-14,
            0.,
            "cnst1",
        );
        near(
            point.poisson_coefficient.value,
            (2. * 1.034_943e-10 * 1.602_191_8e-19 * 8e22 / beta).sqrt(),
            1e-14,
            0.,
            "cnst0",
        );
        let sample = |temp| -> TestResult<model_compact_intrinsic::HiSimHvDepletionMaterialPoint> {
            Ok(material
                .evaluate(&prepare_hisim_hv_temperature(&model, &instance, temp, 0.)?)?
                .depletion
                .ok_or("missing depletion point")?)
        };
        let hi = sample(kelvin + 1e-3)?;
        let lo = sample(kelvin - 1e-3)?;
        for (center, upper, lower) in [
            (
                dep.twice_depletion_potential,
                hi.twice_depletion_potential,
                lo.twice_depletion_potential,
            ),
            (
                dep.built_in_potential,
                hi.built_in_potential,
                lo.built_in_potential,
            ),
        ] {
            near(
                center.gradient[3],
                (upper.value - lower.value) / 2e-3,
                3e-6,
                1e-12,
                "depletion thermal slope",
            );
        }
    }
    Ok(())
}
