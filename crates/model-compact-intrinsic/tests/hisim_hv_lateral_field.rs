//! Original HV1/HV2 body-induced lateral channel-charge increment.

use model_compact_intrinsic::{
    BoundParameters, ModelError, ModelVariant, ParameterScope, ParameterValue,
    PreparedHiSimHvLateralFieldCharge, prepare_hisim_hv_instance,
    prepare_hisim_hv_lateral_field_charge, prepare_hisim_hv_model,
};

fn prepared(
    variant: ModelVariant,
    xqy1: f64,
    xqy2: f64,
    coadov: i32,
    multiplier: f64,
) -> Result<PreparedHiSimHvLateralFieldCharge, ModelError> {
    let model = prepare_hisim_hv_model(&BoundParameters::bind(
        variant,
        ParameterScope::Model,
        [
            ("xqy1", ParameterValue::Real(xqy1)),
            ("xqy2", ParameterValue::Real(xqy2)),
            ("coadov", ParameterValue::Integer(coadov)),
        ],
    )?)?;
    let instance = prepare_hisim_hv_instance(
        &model,
        &BoundParameters::bind(
            variant,
            ParameterScope::Instance,
            [
                ("l", ParameterValue::Real(2.5e-6)),
                ("w", ParameterValue::Real(15e-6)),
                ("nf", ParameterValue::Real(3.0)),
                ("m", ParameterValue::Real(multiplier)),
            ],
        )?,
    )?;
    prepare_hisim_hv_lateral_field_charge(&model, &instance)
}

fn close(actual: f64, expected: f64, tol: f64) {
    assert!(
        (actual - expected).abs() <= tol * expected.abs().max(1e-24),
        "actual {actual:.16e}, expected {expected:.16e}, relative tolerance {tol:.3e}"
    );
}

#[test]
fn original_body_charge_is_conservative_and_analytically_differentiable() -> Result<(), ModelError>
{
    for variant in [ModelVariant::HiSimHv1, ModelVariant::HiSimHv2] {
        for xqy1 in [1e-12, -2e-12] {
            let prepared = prepared(variant, xqy1, 0.75, 1, 2.5)?;
            assert!(prepared.output_enabled);
            close(
                prepared.effective_body_capacitance_f,
                2.5 * prepared.raw_body_capacitance_f,
                1e-14,
            );
            let original = prepared;
            for bias in [-1.0, -0.5, 0.0, 0.1, 1.0, 3.0] {
                let charge = prepared.evaluate(bias)?;
                assert_eq!(charge.gate_charge_c, -charge.drain_charge_c);
                close(
                    charge.gate_charge_c,
                    prepared.effective_body_capacitance_f * bias,
                    1e-15,
                );
                let h = 1e-4;
                let upper = prepared.evaluate(bias + h)?;
                let lower = prepared.evaluate(bias - h)?;
                close(
                    (upper.gate_charge_c - lower.gate_charge_c) / (2.0 * h),
                    charge.gate_body_derivative_f,
                    2e-10,
                );
            }
            assert_eq!(prepared, original);
        }
    }
    Ok(())
}

#[test]
fn xqy1_zero_and_coadov_gate_suppress_only_stamped_term() -> Result<(), ModelError> {
    for variant in [ModelVariant::HiSimHv1, ModelVariant::HiSimHv2] {
        let no_term = prepared(variant, 0.0, 1.0, 1, 3.0)?;
        assert_eq!(no_term.raw_body_capacitance_f, 0.0);
        assert_eq!(no_term.effective_body_capacitance_f, 0.0);
        let output_off = prepared(variant, 1e-12, 1.0, 0, 3.0)?;
        assert!(!output_off.output_enabled);
        assert_ne!(output_off.raw_body_capacitance_f, 0.0);
        assert_eq!(output_off.effective_body_capacitance_f, 0.0);
        let charge = output_off.evaluate(3.0)?;
        assert_eq!(charge.gate_charge_c, 0.0);
        assert_eq!(charge.drain_charge_c, 0.0);
        assert_eq!(charge.gate_body_derivative_f, 0.0);
    }
    Ok(())
}

#[test]
fn invalid_internal_body_bias_is_rejected() -> Result<(), ModelError> {
    let prepared = prepared(ModelVariant::HiSimHv1, 1e-12, 1.0, 1, 1.0)?;
    for vbs in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(prepared.evaluate(vbs).is_err());
    }
    Ok(())
}
