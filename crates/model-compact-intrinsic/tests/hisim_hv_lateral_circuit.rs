//! Paired native lateral-field charge and independent constitutive derivatives.
use model_compact_intrinsic::{
    BoundParameters, HiSimHvChannelElectrostaticPoint, HiSimHvChannelRegion, HiSimHvExternalBias,
    HiSimHvIntrinsicBias, HiSimHvIntrinsicScalar, ModelError, ModelVariant, ParameterScope,
    ParameterValue, PreparedHiSimHvInstance, PreparedHiSimHvModel, prepare_hisim_hv_instance,
    prepare_hisim_hv_lateral_field_charge, prepare_hisim_hv_model, prepare_hisim_hv_normal_channel,
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
    let parameters: &[(&str, f64)] = match row[7] {
        0. => &[("xqy", 1e-7), ("xqy1", 0.)],
        1. => &[("xqy", 0.), ("xqy1", 1e-17), ("xqy2", 0.8)],
        2. => &[("xqy", 2e-7), ("xqy1", 2e-17), ("xqy2", 1.2), ("clm1", 0.4)],
        _ => return Err(ModelError::InvalidPhysicalParameter("lateral fixture card")),
    };
    fields.extend(
        parameters
            .iter()
            .map(|(n, v)| (*n, ParameterValue::Real(*v))),
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
// The native circuit oracle forms Pds by subtracting two separately rounded
// surface potentials, then multiplies it by Fdd and divides by beta*Qn0.
// This magnifies endpoint roundoff in nearly depleted devices. Propagate
// a small endpoint ULP budget through that exact sensitivity, rather than
// giving the entire charge sweep a relaxed relative tolerance.
fn native_endpoint_roundoff(
    row: &[f64; 14],
    instance: &PreparedHiSimHvInstance,
    state: &HiSimHvChannelElectrostaticPoint,
    beta: f64,
) -> Result<f64, ModelError> {
    let xqy = match row[7] {
        0. => 1e-7,
        1. => return Ok(0.),
        2. => 2e-7,
        _ => return Err(ModelError::InvalidPhysicalParameter("lateral fixture card")),
    };
    let c = &state.charge;
    if !matches!(
        c.region,
        HiSimHvChannelRegion::Transition | HiSimHvChannelRegion::StrongInversion
    ) || c.drift_diffusion_integral.value < 1e-15
        || c.surface_difference.value == 0.
    {
        return Ok(0.);
    }
    let slot = instance
        .variant()
        .parameter(ParameterScope::Model, "clm1")
        .ok_or_else(|| ModelError::UnknownParameter("clm1".to_owned()))?
        .slot;
    let clm = instance
        .binned_model_slot(slot)
        .ok_or(ModelError::MissingDefaultDependency(slot))?;
    let scale = 1.3
        * 1.034_943e-10
        * instance.effective_geometry().capacitance_width_total_m
        * row[5]
        * (1. - clm)
        / xqy
        * state.material.bulk_depletion_width_factor;
    let sensitivity = c.drift_diffusion_integral.value
        / c.surface_difference.value
        / (beta * c.source_inversion_density.value);
    Ok((scale * sensitivity).abs()
        * 16.
        * f64::EPSILON
        * (c.source_surface.value.abs() + c.drain_surface.value.abs()))
}
#[test]
fn paired_native_lateral_charge() -> TestResult {
    let mut count = 0;

    for (index, line) in include_str!("fixtures/hisim-hv-lateral-circuit-ngspice47.txt")
        .lines()
        .enumerate()
    {
        if line.starts_with('#') {
            continue;
        }
        let row: [f64; 14] = line
            .split_whitespace()
            .map(str::parse)
            .collect::<Result<Vec<_>, _>>()?
            .try_into()
            .map_err(|_| "native lateral fixture width")?;
        let (model, instance) = prepare(&row)?;
        let channel = prepare_hisim_hv_normal_channel(&model, &instance)?;
        let lateral = prepare_hisim_hv_lateral_field_charge(&model, &instance)?;
        let temp = prepare_hisim_hv_transport(&model, &instance, row[2] + 273.15, 0.)?;
        let bias = HiSimHvIntrinsicBias {
            vgs: row[8],
            vds: row[9],
            vbs: row[10],
        };
        let external = HiSimHvExternalBias {
            vgs: bias.vgs,
            vds: bias.vds,
            vbs: bias.vbs,
        };
        let state = channel.evaluate_with_parasitics(bias, external, 0., false, &temp)?;
        let electrostatics = &state.channel.intrinsic.electrostatics;
        let q = lateral.evaluate_channel_charge(bias, electrostatics, &temp.temperature)?;
        assert_eq!(state.lateral_gate_charge_c, q);
        let roundoff = native_endpoint_roundoff(
            &row,
            &instance,
            electrostatics,
            temp.temperature.inverse_thermal_voltage,
        )?;
        close(
            q.value,
            row[11],
            5e-7,
            2e-27 + roundoff,
            (index, &row[..11], roundoff),
        );
        close(
            -q.value,
            row[12],
            5e-7,
            2e-27 + roundoff,
            (index, &row[..11], roundoff),
        );
        close(0., row[13], 0., 2e-27, (index, &row[..11]));
        count += 1;
    }
    assert_eq!(count, 3360);
    Ok(())
}
#[test]
fn local_lateral_charge_derivatives() -> TestResult {
    for family in [1., 2.] {
        for config in [0., 1., 2.] {
            let (model, instance) = prepare(&[family, 1., 27., 4e-6, 30e-6, 3., 3., config])?;
            let channel = prepare_hisim_hv_normal_channel(&model, &instance)?;
            let lateral = prepare_hisim_hv_lateral_field_charge(&model, &instance)?;
            let evaluate = |x: [f64; 4]| -> Result<HiSimHvIntrinsicScalar, ModelError> {
                let temp = prepare_hisim_hv_transport(&model, &instance, x[3], 0.)?;
                let bias = HiSimHvIntrinsicBias {
                    vgs: x[0],
                    vds: x[1],
                    vbs: x[2],
                };
                let state = channel.evaluate(bias, &temp)?;
                lateral.evaluate_channel_charge(bias, &state.electrostatics, &temp.temperature)
            };
            for gate in [-2., 0., 2., 3., 5., 8.] {
                for drain in [0.01, 0.2, 2., 10.] {
                    let x = [gate, drain, -0.3, 315.];
                    let p = evaluate(x)?;
                    for axis in 0..4 {
                        let h = if axis == 3 { 0.01 } else { 1e-5 };
                        let mut a = x;
                        let mut b = x;
                        a[axis] += h;
                        b[axis] -= h;
                        close(
                            p.gradient[axis],
                            (evaluate(a)?.value - evaluate(b)?.value) / (2. * h),
                            3e-5,
                            1e-23,
                            (family, config, x, axis),
                        );
                    }
                }
            }
        }
    }
    Ok(())
}
