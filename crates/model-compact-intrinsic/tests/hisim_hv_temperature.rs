//! Temperature-dependent *leading* HiSIM-HV setup equations, distinct from
//! the subsequent nonlinear current, mobility and electrothermal kernels.

use model_compact_intrinsic::{
    BoundParameters, ModelError, ModelVariant, ParameterScope, ParameterValue,
    PreparedHiSimHvInstance, PreparedHiSimHvModel, prepare_hisim_hv_instance,
    prepare_hisim_hv_model, prepare_hisim_hv_temperature,
};

fn make_model(
    variant: ModelVariant,
    parameters: &[(&str, ParameterValue)],
) -> Result<PreparedHiSimHvModel, ModelError> {
    prepare_hisim_hv_model(&BoundParameters::bind(
        variant,
        ParameterScope::Model,
        parameters
            .iter()
            .map(|(name, value)| (*name, value.clone())),
    )?)
}

fn make_instance(
    model: &PreparedHiSimHvModel,
    parameters: &[(&str, ParameterValue)],
) -> Result<PreparedHiSimHvInstance, ModelError> {
    prepare_hisim_hv_instance(
        model,
        &BoundParameters::bind(
            model.variant(),
            ParameterScope::Instance,
            parameters
                .iter()
                .map(|(name, value)| (*name, value.clone())),
        )?,
    )
}

fn close(actual: f64, expected: f64, relative: f64) {
    assert!(
        (actual - expected).abs() <= relative * expected.abs().max(1e-30),
        "actual={actual:.15e}, source formula={expected:.15e}"
    );
}

#[test]
fn both_families_preserve_nominal_bandgap_and_carrier_baseline() -> Result<(), ModelError> {
    for variant in [ModelVariant::HiSimHv1, ModelVariant::HiSimHv2] {
        let model = make_model(variant, &[])?;
        let instance = make_instance(&model, &[])?;
        let t = prepare_hisim_hv_temperature(&model, &instance, 300.15, 0.0)?;
        assert_eq!(t.nominal_temperature_kelvin, 300.15);
        assert_eq!(t.device_temperature_kelvin, 300.15);
        assert_eq!(t.temperature_ratio, 1.0);
        assert_eq!(t.bandgap_ev, t.nominal_bandgap_ev);
        close(t.intrinsic_carrier_concentration_m3, 1.04e16, 1e-14);
        close(
            t.inverse_thermal_voltage,
            1.602_191_8e-19 / (1.380_622_6e-23 * 300.15),
            1e-14,
        );
        close(
            t.nominal_inverse_thermal_voltage,
            t.inverse_thermal_voltage,
            1e-14,
        );
        close(t.sqrt_bandgap, t.bandgap_ev.sqrt(), 1e-14);
        close(t.ig_bandgap_three_halves, t.ig_bandgap_sqrt.powi(3), 1e-14);
        assert!(t.intrinsic_carrier_derivative_m3_per_kelvin.is_finite());
    }
    Ok(())
}

#[test]
fn temperature_celsius_model_binning_and_instance_offsets_apply_independently()
-> Result<(), ModelError> {
    for variant in [ModelVariant::HiSimHv1, ModelVariant::HiSimHv2] {
        let model = make_model(
            variant,
            &[
                ("tnom", ParameterValue::Real(36.85)),
                ("eg0", ParameterValue::Real(1.2)),
                ("leg0", ParameterValue::Real(0.1)),
                ("bgtmp1", ParameterValue::Real(1.0e-4)),
                ("bgtmp2", ParameterValue::Real(2.0e-7)),
                ("igtemp2", ParameterValue::Real(3.0)),
                ("igtemp3", ParameterValue::Real(4.0)),
            ],
        )?;
        let instance = make_instance(
            &model,
            &[
                ("l", ParameterValue::Real(4e-6)),
                ("w", ParameterValue::Real(50e-6)),
                ("dtemp", ParameterValue::Real(5.0)),
            ],
        )?;
        let result = prepare_hisim_hv_temperature(&model, &instance, 330.0, 7.0)?;
        let nominal: f64 = 310.0;
        let absolute: f64 = 342.0;
        let eg0 = 1.2 + 0.1 / 4.0;
        let egtnom = nominal.mul_add(-nominal.mul_add(1e-7, 90.25e-6), eg0);
        let eg = 2.0e-7f64.mul_add(
            -nominal.mul_add(-nominal, absolute * absolute),
            1.0e-4f64.mul_add(-(absolute - nominal), egtnom),
        );
        close(result.nominal_temperature_kelvin, nominal, 1e-14);
        close(result.ambient_device_temperature_kelvin, 335.0, 1e-14);
        close(result.device_temperature_kelvin, absolute, 1e-14);
        close(result.temperature_ratio, absolute / nominal, 1e-14);
        close(result.nominal_bandgap_ev, egtnom, 1e-14);
        close(result.bandgap_ev, eg, 1e-14);
        close(
            result.bandgap_derivative_ev_per_kelvin,
            -(2.0 * absolute).mul_add(2.0e-7, 1.0e-4),
            1e-14,
        );
        let beta = 1.602_191_8e-19 / (1.380_622_6e-23 * absolute);
        let beta_nom = 1.602_191_8e-19 / (1.380_622_6e-23 * nominal);
        close(result.inverse_thermal_voltage, beta, 1e-14);
        close(result.nominal_inverse_thermal_voltage, beta_nom, 1e-14);
        let expected_carrier = 1.04e16
            * (absolute / nominal).powf(1.5)
            * (egtnom * 0.5).mul_add(beta_nom, -eg * 0.5 * beta).exp();
        close(
            result.intrinsic_carrier_concentration_m3,
            expected_carrier,
            1e-14,
        );
        let exp_ig = 4.0f64.mul_add(
            1.0 / (absolute * absolute) - 1.0 / (nominal * nominal),
            3.0f64.mul_add(1.0 / absolute - 1.0 / nominal, egtnom),
        );
        close(result.ig_bandgap_sqrt, exp_ig.sqrt(), 1e-14);
    }
    Ok(())
}

#[test]
fn temperature_derivatives_match_independent_central_differences() -> Result<(), ModelError> {
    for variant in [ModelVariant::HiSimHv1, ModelVariant::HiSimHv2] {
        let model = make_model(
            variant,
            &[
                ("bgtmp1", ParameterValue::Real(1.1e-4)),
                ("bgtmp2", ParameterValue::Real(1.9e-7)),
                ("tnom", ParameterValue::Real(28.0)),
            ],
        )?;
        let instance = make_instance(&model, &[("dtemp", ParameterValue::Real(3.0))])?;
        let t = 330.0;
        let h = 0.01;
        let center = prepare_hisim_hv_temperature(&model, &instance, t, 0.0)?;
        let plus = prepare_hisim_hv_temperature(&model, &instance, t + h, 0.0)?;
        let minus = prepare_hisim_hv_temperature(&model, &instance, t - h, 0.0)?;
        close(
            center.bandgap_derivative_ev_per_kelvin,
            (plus.bandgap_ev - minus.bandgap_ev) / (2.0 * h),
            1e-8,
        );
        close(
            center.inverse_thermal_voltage_derivative,
            (plus.inverse_thermal_voltage - minus.inverse_thermal_voltage) / (2.0 * h),
            1e-8,
        );
        close(
            center.intrinsic_carrier_derivative_m3_per_kelvin,
            (plus.intrinsic_carrier_concentration_m3 - minus.intrinsic_carrier_concentration_m3)
                / (2.0 * h),
            1e-7,
        );
        assert_eq!(instance.geometry().gate_length_m, 2e-6);
    }
    Ok(())
}

#[test]
fn physical_temperature_and_bandgap_rejections_preserve_variant_isolation() -> Result<(), ModelError>
{
    for variant in [ModelVariant::HiSimHv1, ModelVariant::HiSimHv2] {
        let model = make_model(variant, &[])?;
        let instance = make_instance(&model, &[])?;
        assert_eq!(
            prepare_hisim_hv_temperature(&model, &instance, 0.0, 0.0),
            Err(ModelError::InvalidPhysicalParameter(
                "HiSIM-HV device ambient temperature"
            ))
        );
        assert_eq!(
            prepare_hisim_hv_temperature(&model, &instance, 300.15, -350.0),
            Err(ModelError::InvalidPhysicalParameter(
                "HiSIM-HV heated device temperature"
            ))
        );
        let bad_model = make_model(variant, &[("tnom", ParameterValue::Real(-300.0))])?;
        let bad_instance = make_instance(&bad_model, &[])?;
        assert_eq!(
            prepare_hisim_hv_temperature(&bad_model, &bad_instance, 300.15, 0.0),
            Err(ModelError::InvalidPhysicalParameter(
                "HiSIM-HV nominal temperature"
            ))
        );
        let bad_gap = make_model(variant, &[("eg0", ParameterValue::Real(0.001))])?;
        let gap_instance = make_instance(&bad_gap, &[])?;
        assert_eq!(
            prepare_hisim_hv_temperature(&bad_gap, &gap_instance, 300.15, 0.0),
            Err(ModelError::InvalidPhysicalParameter(
                "HiSIM-HV nominal bandgap"
            ))
        );
        let bad_ig = make_model(variant, &[("igtemp2", ParameterValue::Real(1e8))])?;
        let ig_instance = make_instance(&bad_ig, &[])?;
        assert_eq!(
            prepare_hisim_hv_temperature(&bad_ig, &ig_instance, 350.0, 0.0),
            Err(ModelError::InvalidPhysicalParameter(
                "HiSIM-HV gate-current bandgap"
            ))
        );
    }
    let hv1 = make_model(ModelVariant::HiSimHv1, &[])?;
    let hv2 = make_model(ModelVariant::HiSimHv2, &[])?;
    let second_instance = make_instance(&hv2, &[])?;
    assert_eq!(
        prepare_hisim_hv_temperature(&hv1, &second_instance, 300.15, 0.0),
        Err(ModelError::WrongModelScope)
    );
    Ok(())
}

#[test]
fn repeated_trial_temperatures_do_not_mutate_model_or_instance_state() -> Result<(), ModelError> {
    for variant in [ModelVariant::HiSimHv1, ModelVariant::HiSimHv2] {
        let model = make_model(variant, &[])?;
        let device = make_instance(&model, &[])?;
        let nominal = prepare_hisim_hv_temperature(&model, &device, 300.15, 0.0)?;
        let base_geometry = device.effective_geometry();
        for trial in 0..256 {
            let ambient = f64::from(trial).mul_add(0.1, 295.0);
            let rise = f64::from(trial % 11) * 0.25;
            let evaluated = prepare_hisim_hv_temperature(&model, &device, ambient, rise)?;
            assert_eq!(evaluated.device_temperature_kelvin, ambient + rise);
            assert!(evaluated.bandgap_ev > 0.0);
            assert!(
                evaluated
                    .intrinsic_carrier_derivative_m3_per_kelvin
                    .is_finite()
            );
            assert_eq!(device.effective_geometry(), base_geometry);
        }
        assert_eq!(
            prepare_hisim_hv_temperature(&model, &device, 300.15, 0.0)?,
            nominal
        );
    }
    Ok(())
}
