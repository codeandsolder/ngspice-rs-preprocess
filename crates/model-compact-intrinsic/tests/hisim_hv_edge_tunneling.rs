//! Physical HiSIM-HV1/HV2 direct gate-to-S/D tunneling and analytic dI/dV.

use model_compact_intrinsic::{
    BoundParameters, ModelError, ModelVariant, ParameterScope, ParameterValue,
    PreparedHiSimHvEdgeTunneling, prepare_hisim_hv_edge_tunneling, prepare_hisim_hv_instance,
    prepare_hisim_hv_model,
};

fn prepared(
    family: ModelVariant,
    enabled: i32,
    multiplier: f64,
) -> Result<PreparedHiSimHvEdgeTunneling, ModelError> {
    let model = prepare_hisim_hv_model(&BoundParameters::bind(
        family,
        ParameterScope::Model,
        [
            ("tox", ParameterValue::Real(3.2e-9)),
            ("coiigs", ParameterValue::Integer(enabled)),
            ("glksd1", ParameterValue::Real(2.5e-15)),
            ("glksd2", ParameterValue::Real(1.3e6)),
            ("glksd3", ParameterValue::Real(-2.4e6)),
        ],
    )?)?;
    let instance = prepare_hisim_hv_instance(
        &model,
        &BoundParameters::bind(
            family,
            ParameterScope::Instance,
            [
                ("l", ParameterValue::Real(4e-6)),
                ("w", ParameterValue::Real(23e-6)),
                ("nf", ParameterValue::Real(3.0)),
                ("m", ParameterValue::Real(multiplier)),
            ],
        )?,
    )?;
    prepare_hisim_hv_edge_tunneling(&model, &instance)
}

fn close(actual: f64, expected: f64, tolerance: f64) {
    assert!(
        (actual - expected).abs() <= tolerance * expected.abs().max(1e-18),
        "actual {actual:.17e} vs expected {expected:.17e} (tolerance {tolerance})"
    );
}

#[test]
fn direct_edge_tunneling_bias_derivatives_match_independent_finite_differences()
-> Result<(), ModelError> {
    for family in [ModelVariant::HiSimHv1, ModelVariant::HiSimHv2] {
        let prepared = prepared(family, 1, 2.5)?;
        let pristine = prepared;
        for vgs in [-1.5, -0.04, 0.0, 0.2, 1.6] {
            for vds in [-1.3, -0.05, 0.0, 0.3, 2.3] {
                let evaluated = prepared.evaluate(vgs, vds)?;
                let h = 1e-5;
                let gs_up = prepared.evaluate(vgs + h, vds)?;
                let gs_down = prepared.evaluate(vgs - h, vds)?;
                let ds_up = prepared.evaluate(vgs, vds + h)?;
                let ds_down = prepared.evaluate(vgs, vds - h)?;
                if vgs.abs() > 20.0 * h {
                    close(
                        evaluated.gate_source.gate_voltage_derivative_siemens,
                        (gs_up.gate_source.current_a - gs_down.gate_source.current_a) / (2.0 * h),
                        3e-8,
                    );
                } else {
                    assert_eq!(evaluated.gate_source.gate_voltage_derivative_siemens, 0.0);
                }
                if (vgs - vds).abs() > 20.0 * h {
                    close(
                        evaluated.gate_drain.gate_voltage_derivative_siemens,
                        (gs_up.gate_drain.current_a - gs_down.gate_drain.current_a) / (2.0 * h),
                        3e-8,
                    );
                    close(
                        evaluated.gate_drain.drain_voltage_derivative_siemens,
                        (ds_up.gate_drain.current_a - ds_down.gate_drain.current_a) / (2.0 * h),
                        3e-8,
                    );
                } else {
                    assert_eq!(evaluated.gate_drain.gate_voltage_derivative_siemens, 0.0);
                    assert_eq!(evaluated.gate_drain.drain_voltage_derivative_siemens, 0.0);
                }
                close(
                    evaluated.gate_drain.drain_voltage_derivative_siemens,
                    -evaluated.gate_drain.gate_voltage_derivative_siemens,
                    1e-15,
                );
                assert_eq!(evaluated.gate_source.drain_voltage_derivative_siemens, 0.0);
                assert_eq!(ds_up.gate_source.current_a, ds_down.gate_source.current_a);
            }
        }
        assert_eq!(prepared, pristine);
    }
    Ok(())
}

#[test]
fn original_coiigs_selector_and_multiplier_are_respected() -> Result<(), ModelError> {
    for family in [ModelVariant::HiSimHv1, ModelVariant::HiSimHv2] {
        let off = prepared(family, 0, 9.0)?;
        let disabled = off.evaluate(-0.4, 0.8)?;
        for current in [disabled.gate_source, disabled.gate_drain] {
            assert_eq!(current.current_a, 0.0);
            assert_eq!(current.gate_voltage_derivative_siemens, 0.0);
            assert_eq!(current.drain_voltage_derivative_siemens, 0.0);
        }
        let single = prepared(family, 1, 1.0)?.evaluate(-0.4, 0.8)?;
        let triple = prepared(family, 1, 3.0)?.evaluate(-0.4, 0.8)?;
        close(
            triple.gate_source.current_a,
            single.gate_source.current_a * 3.0,
            1e-15,
        );
        close(
            triple.gate_drain.current_a,
            single.gate_drain.current_a * 3.0,
            1e-15,
        );
        close(
            triple.gate_source.gate_voltage_derivative_siemens,
            single.gate_source.gate_voltage_derivative_siemens * 3.0,
            1e-15,
        );
        close(
            triple.gate_drain.drain_voltage_derivative_siemens,
            single.gate_drain.drain_voltage_derivative_siemens * 3.0,
            1e-15,
        );
        assert!(single.gate_source.current_a > 0.0);
        assert!(single.gate_drain.current_a > 0.0);
        assert!(
            prepared(family, 1, 1.0)?
                .evaluate(0.3, -0.5)?
                .gate_source
                .current_a
                < 0.0
        );
    }
    Ok(())
}

#[test]
fn nonfinite_edge_tunneling_trial_voltages_fail_fast() -> Result<(), ModelError> {
    let state = prepared(ModelVariant::HiSimHv1, 1, 1.0)?;
    for (vgs, vds) in [
        (f64::NAN, 0.0),
        (0.0, f64::INFINITY),
        (f64::NEG_INFINITY, 1.0),
        (f64::MAX, -f64::MAX),
    ] {
        assert!(state.evaluate(vgs, vds).is_err());
    }
    Ok(())
}
