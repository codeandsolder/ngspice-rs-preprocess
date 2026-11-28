//! Exact HiSIM-HV1/HV2 substrate/source and substrate/drain junction diode laws.

use model_compact_intrinsic::{
    BoundParameters, HiSimHvJunctionBias, HiSimHvJunctionCurrent, HiSimHvJunctionCurrents,
    ModelError, ModelVariant, ParameterScope, ParameterValue, PreparedHiSimHvInstance,
    PreparedHiSimHvModel, PreparedHiSimHvTransport, prepare_hisim_hv_instance,
    prepare_hisim_hv_junction_diodes, prepare_hisim_hv_model, prepare_hisim_hv_transport,
};

fn prepared(
    variant: ModelVariant,
    model_fields: &[(&str, ParameterValue)],
    instance_fields: &[(&str, ParameterValue)],
    local_rise: f64,
) -> Result<
    (
        PreparedHiSimHvModel,
        PreparedHiSimHvInstance,
        PreparedHiSimHvTransport,
    ),
    ModelError,
> {
    let model = prepare_hisim_hv_model(&BoundParameters::bind(
        variant,
        ParameterScope::Model,
        model_fields
            .iter()
            .map(|(name, value)| (*name, value.clone())),
    )?)?;
    let instance = prepare_hisim_hv_instance(
        &model,
        &BoundParameters::bind(
            variant,
            ParameterScope::Instance,
            instance_fields
                .iter()
                .map(|(name, value)| (*name, value.clone())),
        )?,
    )?;
    let transport = prepare_hisim_hv_transport(&model, &instance, 318.15, local_rise)?;
    Ok((model, instance, transport))
}

fn close(label: &str, actual: f64, expected: f64, tolerance: f64) {
    let scale = expected.abs().max(1e-14);
    assert!(
        (actual - expected).abs() <= tolerance * scale,
        "{label}: actual {actual:.16e}, expected {expected:.16e}"
    );
}

fn junction(
    model: &PreparedHiSimHvModel,
    instance: &PreparedHiSimHvInstance,
    transport: &PreparedHiSimHvTransport,
    source: f64,
    drain: f64,
) -> Result<HiSimHvJunctionCurrents, ModelError> {
    prepare_hisim_hv_junction_diodes(model, instance, transport)?.evaluate(HiSimHvJunctionBias {
        bulk_source_v: source,
        bulk_drain_v: drain,
    })
}

const fn source_or_drain(value: HiSimHvJunctionCurrents, source: bool) -> HiSimHvJunctionCurrent {
    if source {
        value.bulk_source
    } else {
        value.bulk_drain
    }
}

#[test]
fn zero_saturation_geometry_has_zero_diode_current() -> Result<(), ModelError> {
    for variant in [ModelVariant::HiSimHv1, ModelVariant::HiSimHv2] {
        let (model, instance, transport) = prepared(variant, &[], &[], 0.0)?;
        let value = junction(&model, &instance, &transport, -0.7, 0.3)?;
        for diode in [value.bulk_source, value.bulk_drain] {
            assert_eq!(diode.current_a, 0.0);
            assert_eq!(diode.conductance_siemens, 0.0);
            assert_eq!(diode.temperature_derivative_a_per_kelvin, 0.0);
        }
    }
    Ok(())
}

#[test]
fn source_drain_diode_conductance_and_thermal_derivatives_are_physical() -> Result<(), ModelError> {
    for variant in [ModelVariant::HiSimHv1, ModelVariant::HiSimHv2] {
        let model_fields = [
            ("rth0", ParameterValue::Real(0.4)),
            ("js0", ParameterValue::Real(3e-8)),
            ("js0sw", ParameterValue::Real(4e-9)),
            ("vdiffj", ParameterValue::Real(0.02)),
            ("xti", ParameterValue::Real(1.3)),
            ("xti2", ParameterValue::Real(1.7)),
        ];
        let model_fields_hv2 = [
            ("rth0", ParameterValue::Real(0.4)),
            ("js0d", ParameterValue::Real(3e-8)),
            ("js0swd", ParameterValue::Real(4e-9)),
            ("js0s", ParameterValue::Real(5e-8)),
            ("js0sws", ParameterValue::Real(2e-9)),
            ("vdiffjd", ParameterValue::Real(0.02)),
            ("vdiffjs", ParameterValue::Real(0.03)),
            ("xtid", ParameterValue::Real(1.3)),
            ("xti2d", ParameterValue::Real(1.7)),
            ("xtis", ParameterValue::Real(1.8)),
            ("xti2s", ParameterValue::Real(2.0)),
        ];
        let fields = if variant == ModelVariant::HiSimHv1 {
            model_fields.as_slice()
        } else {
            model_fields_hv2.as_slice()
        };
        let instance_fields = [
            ("ad", ParameterValue::Real(4e-11)),
            ("as", ParameterValue::Real(2e-11)),
            ("pd", ParameterValue::Real(8e-6)),
            ("ps", ParameterValue::Real(3e-6)),
            ("m", ParameterValue::Real(2.5)),
            ("coselfheat", ParameterValue::Integer(1)),
        ];
        let (model, instance, transport) = prepared(variant, fields, &instance_fields, 14.0)?;
        let prepared_diodes = prepare_hisim_hv_junction_diodes(&model, &instance, &transport)?;
        for bias in [-0.5, -0.07, -0.003, 0.0, 0.08, 0.2, 0.6] {
            for source in [false, true] {
                let base = prepared_diodes.evaluate(HiSimHvJunctionBias {
                    bulk_source_v: bias,
                    bulk_drain_v: bias,
                })?;
                let expected = source_or_drain(base, source);
                let h = 1e-5;
                let higher = source_or_drain(
                    prepared_diodes.evaluate(HiSimHvJunctionBias {
                        bulk_source_v: bias + h,
                        bulk_drain_v: bias + h,
                    })?,
                    source,
                );
                let lower = source_or_drain(
                    prepared_diodes.evaluate(HiSimHvJunctionBias {
                        bulk_source_v: bias - h,
                        bulk_drain_v: bias - h,
                    })?,
                    source,
                );
                close(
                    "analytic diode conductance",
                    expected.conductance_siemens,
                    (higher.current_a - lower.current_a) / (2.0 * h),
                    4e-8,
                );
                let hot = prepare_hisim_hv_transport(&model, &instance, 318.15, 14.0 + h)?;
                let cold = prepare_hisim_hv_transport(&model, &instance, 318.15, 14.0 - h)?;
                let hot_current = source_or_drain(
                    prepare_hisim_hv_junction_diodes(&model, &instance, &hot)?.evaluate(
                        HiSimHvJunctionBias {
                            bulk_source_v: bias,
                            bulk_drain_v: bias,
                        },
                    )?,
                    source,
                );
                let cold_current = source_or_drain(
                    prepare_hisim_hv_junction_diodes(&model, &instance, &cold)?.evaluate(
                        HiSimHvJunctionBias {
                            bulk_source_v: bias,
                            bulk_drain_v: bias,
                        },
                    )?,
                    source,
                );
                close(
                    "analytic diode temperature derivative",
                    expected.temperature_derivative_a_per_kelvin,
                    (hot_current.current_a - cold_current.current_a) / (2.0 * h),
                    6e-7,
                );
            }
        }
    }
    Ok(())
}

#[test]
fn diode_thermal_derivative_follows_native_binned_rth_and_topology() -> Result<(), ModelError> {
    for family in [ModelVariant::HiSimHv1, ModelVariant::HiSimHv2] {
        let instance = [
            ("as", ParameterValue::Real(5e-11)),
            ("ad", ParameterValue::Real(2e-11)),
            ("ps", ParameterValue::Real(7e-6)),
            ("pd", ParameterValue::Real(4e-6)),
            ("coselfheat", ParameterValue::Integer(1)),
        ];
        let bias = HiSimHvJunctionBias {
            bulk_source_v: -0.5,
            bulk_drain_v: 0.18,
        };
        let (no_heat_model, no_heat_inst, no_heat) = prepared(
            family,
            &[("rth0", ParameterValue::Real(0.0))],
            &instance,
            20.0,
        )?;
        let no_heat_current =
            prepare_hisim_hv_junction_diodes(&no_heat_model, &no_heat_inst, &no_heat)?
                .evaluate(bias)?;
        let baseline = prepare_hisim_hv_transport(&no_heat_model, &no_heat_inst, 318.15, 0.0)?;
        assert_eq!(
            no_heat_current,
            prepare_hisim_hv_junction_diodes(&no_heat_model, &no_heat_inst, &baseline)?
                .evaluate(bias)?
        );
        assert_eq!(
            no_heat_current
                .bulk_drain
                .temperature_derivative_a_per_kelvin,
            0.0
        );
        assert_eq!(
            no_heat_current
                .bulk_source
                .temperature_derivative_a_per_kelvin,
            0.0
        );

        let disabled = instance
            .iter()
            .map(|(name, value)| {
                if *name == "coselfheat" {
                    (*name, ParameterValue::Integer(0))
                } else {
                    (*name, value.clone())
                }
            })
            .collect::<Vec<_>>();
        let (disabled_model, disabled_inst, disabled_transport) = prepared(
            family,
            &[("rth0", ParameterValue::Real(0.4))],
            &disabled,
            0.0,
        )?;
        let currents =
            prepare_hisim_hv_junction_diodes(&disabled_model, &disabled_inst, &disabled_transport)?
                .evaluate(bias)?;
        assert_eq!(
            currents.bulk_source.temperature_derivative_a_per_kelvin,
            0.0
        );
        assert_eq!(currents.bulk_drain.temperature_derivative_a_per_kelvin, 0.0);
    }
    Ok(())
}

#[test]
fn hv2_source_drain_asymmetry_and_multiplicity_use_original_parameters() -> Result<(), ModelError> {
    let model_fields = [
        ("js0s", ParameterValue::Real(0.12)),
        ("js0d", ParameterValue::Real(0.005)),
        ("js0sws", ParameterValue::Real(0.01)),
        ("js0swd", ParameterValue::Real(0.001)),
        ("vdiffjs", ParameterValue::Real(0.05)),
        ("vdiffjd", ParameterValue::Real(0.01)),
        ("rth0", ParameterValue::Real(0.5)),
    ];
    let parameters = [
        ("as", ParameterValue::Real(4e-11)),
        ("ad", ParameterValue::Real(4e-11)),
        ("ps", ParameterValue::Real(4e-6)),
        ("pd", ParameterValue::Real(4e-6)),
        ("m", ParameterValue::Real(2.0)),
    ];
    let family = ModelVariant::HiSimHv2;
    let (model, inst, transport) = prepared(family, &model_fields, &parameters, 0.0)?;
    let diode = junction(&model, &inst, &transport, 0.28, 0.28)?;
    assert_ne!(diode.bulk_source.current_a, diode.bulk_drain.current_a);
    let mut one = parameters.to_vec();
    one[4].1 = ParameterValue::Real(1.0);
    let (model2, inst2, transport2) = prepared(family, &model_fields, &one, 0.0)?;
    let single = junction(&model2, &inst2, &transport2, 0.28, 0.28)?;
    for (scaled, original) in [
        (diode.bulk_source, single.bulk_source),
        (diode.bulk_drain, single.bulk_drain),
    ] {
        close(
            "native multiplicity Id",
            scaled.current_a,
            original.current_a * 2.0,
            1e-12,
        );
        close(
            "native multiplicity G",
            scaled.conductance_siemens,
            original.conductance_siemens * 2.0,
            1e-12,
        );
    }
    Ok(())
}

#[test]
fn precompiled_diode_state_is_immutable_across_many_bias_trials_and_new_temperatures()
-> Result<(), ModelError> {
    for variant in [ModelVariant::HiSimHv1, ModelVariant::HiSimHv2] {
        let fields = if variant == ModelVariant::HiSimHv1 {
            vec![
                ("js0", ParameterValue::Real(0.004)),
                ("js0sw", ParameterValue::Real(0.0008)),
                ("rth0", ParameterValue::Real(0.5)),
            ]
        } else {
            vec![
                ("js0s", ParameterValue::Real(0.003)),
                ("js0d", ParameterValue::Real(0.005)),
                ("js0sws", ParameterValue::Real(0.0008)),
                ("js0swd", ParameterValue::Real(0.0005)),
                ("rth0", ParameterValue::Real(0.5)),
            ]
        };
        let device = [
            ("as", ParameterValue::Real(1e-11)),
            ("ad", ParameterValue::Real(3e-11)),
            ("ps", ParameterValue::Real(4e-6)),
            ("pd", ParameterValue::Real(6e-6)),
            ("coselfheat", ParameterValue::Integer(1)),
        ];
        let (model, instance, cold_transport) = prepared(variant, &fields, &device, 0.0)?;
        let cold = prepare_hisim_hv_junction_diodes(&model, &instance, &cold_transport)?;
        let pristine = cold;
        for i in 0..256u16 {
            let voltage = f64::from(i).mul_add(0.006, -0.6);
            let result = cold.evaluate(HiSimHvJunctionBias {
                bulk_source_v: voltage,
                bulk_drain_v: -0.5 * voltage,
            })?;
            for value in [result.bulk_source, result.bulk_drain] {
                assert!(value.current_a.is_finite());
                assert!(value.conductance_siemens.is_finite());
                assert!(value.temperature_derivative_a_per_kelvin.is_finite());
            }
            assert_eq!(cold, pristine);
        }
        let hot_transport = prepare_hisim_hv_transport(&model, &instance, 318.15, 20.0)?;
        let hot = prepare_hisim_hv_junction_diodes(&model, &instance, &hot_transport)?;
        let bias = HiSimHvJunctionBias {
            bulk_source_v: 0.22,
            bulk_drain_v: 0.14,
        };
        assert_ne!(cold.evaluate(bias)?, hot.evaluate(bias)?);
        assert_eq!(cold, pristine);
    }
    Ok(())
}
