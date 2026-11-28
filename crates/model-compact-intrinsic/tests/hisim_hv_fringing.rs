//! Original HV1/HV2 bias-linear external gate/d/s fringing contributions.

use model_compact_intrinsic::{
    BoundParameters, ModelError, ModelVariant, ParameterScope, ParameterValue,
    prepare_hisim_hv_fringing, prepare_hisim_hv_instance, prepare_hisim_hv_model,
};

fn prepared(
    variant: ModelVariant,
    coadov: i32,
    m: f64,
) -> Result<model_compact_intrinsic::PreparedHiSimHvFringing, ModelError> {
    let model = prepare_hisim_hv_model(&BoundParameters::bind(
        variant,
        ParameterScope::Model,
        [
            ("tox", ParameterValue::Real(2.6e-9)),
            ("tpoly", ParameterValue::Real(130e-9)),
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
                ("w", ParameterValue::Real(19e-6)),
                ("nf", ParameterValue::Real(3.0)),
                ("m", ParameterValue::Real(m)),
            ],
        )?,
    )?;
    prepare_hisim_hv_fringing(&model, &instance)
}

fn close(actual: f64, expected: f64, tolerance: f64) {
    assert!(
        (actual - expected).abs() < tolerance * expected.abs().max(1e-25),
        "actual={actual:.17e} expected={expected:.17e} tolerance={tolerance:e}"
    );
}

#[test]
fn source_fringe_charge_and_analytic_derivatives_are_consistent() -> Result<(), ModelError> {
    for variant in [ModelVariant::HiSimHv1, ModelVariant::HiSimHv2] {
        let fringe = prepared(variant, 1, 2.5)?;
        assert!(fringe.output_enabled);
        assert!(fringe.raw_capacitance_f > 0.0);
        close(
            fringe.effective_capacitance_f,
            fringe.raw_capacitance_f * 2.5,
            1e-15,
        );
        let h = 1e-4;
        for (vgs, vds) in [(-2.5, 1.0), (-0.1, -0.5), (0.0, 0.0), (1.6, 4.2)] {
            let charge = fringe.evaluate(vgs, vds)?;
            close(
                charge.gate_drain_charge_c,
                fringe.effective_capacitance_f * (vgs - vds),
                1e-15,
            );
            close(
                charge.gate_source_charge_c,
                fringe.effective_capacitance_f * vgs,
                1e-15,
            );
            let higher = fringe.evaluate(vgs + h, vds)?;
            let lower = fringe.evaluate(vgs - h, vds)?;
            close(
                (higher.gate_drain_charge_c - lower.gate_drain_charge_c) / (2.0 * h),
                fringe.effective_capacitance_f,
                2e-10,
            );
            close(
                (higher.gate_source_charge_c - lower.gate_source_charge_c) / (2.0 * h),
                fringe.effective_capacitance_f,
                2e-10,
            );
            let higher = fringe.evaluate(vgs, vds + h)?;
            let lower = fringe.evaluate(vgs, vds - h)?;
            close(
                (higher.gate_drain_charge_c - lower.gate_drain_charge_c) / (2.0 * h),
                -fringe.effective_capacitance_f,
                2e-10,
            );
            assert_eq!(higher.gate_source_charge_c, lower.gate_source_charge_c);
        }
    }
    Ok(())
}

#[test]
fn disabled_overlap_model_keeps_prepared_coefficient_without_stamping() -> Result<(), ModelError> {
    for variant in [ModelVariant::HiSimHv1, ModelVariant::HiSimHv2] {
        let disabled = prepared(variant, 0, 9.0)?;
        assert!(!disabled.output_enabled);
        assert!(disabled.raw_capacitance_f > 0.0);
        assert_eq!(disabled.effective_capacitance_f, 0.0);
        let charge = disabled.evaluate(4.0, 2.0)?;
        assert_eq!(charge.gate_drain_charge_c, 0.0);
        assert_eq!(charge.gate_source_charge_c, 0.0);
        let enabled = prepared(variant, 1, 1.0)?;
        close(enabled.raw_capacitance_f, disabled.raw_capacitance_f, 1e-15);
    }
    Ok(())
}

#[test]
fn invalid_trial_voltages_rejected() -> Result<(), ModelError> {
    let prepared = prepared(ModelVariant::HiSimHv1, 1, 1.0)?;
    for (gate, drain) in [
        (f64::NAN, 0.0),
        (0.0, f64::INFINITY),
        (f64::NEG_INFINITY, -3.0),
        (f64::MAX, -f64::MAX),
    ] {
        assert!(prepared.evaluate(gate, drain).is_err());
    }
    Ok(())
}
