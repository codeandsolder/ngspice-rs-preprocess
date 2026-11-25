//! HiSIM-HV generated electrical power and native thermal-Jacobian stamping.
//! The tests supply *independent drain-current kernel outputs*: no MOS current
//! is inferred, and no simplified transistor physics is substituted.

use model_compact_intrinsic::{
    BoundParameters, HiSimHvDrainBias, HiSimHvDrainCurrent, HiSimHvElectricalGradient,
    HiSimHvGeneratedHeat, ModelError, ModelVariant, ParameterScope, ParameterValue,
    PreparedHiSimHvTransport, evaluate_hisim_hv_generated_heat, prepare_hisim_hv_instance,
    prepare_hisim_hv_model, prepare_hisim_hv_transport,
};

fn prepared(
    variant: ModelVariant,
    model_fields: &[(&str, ParameterValue)],
    self_heating: bool,
    local_rise: f64,
) -> Result<PreparedHiSimHvTransport, ModelError> {
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
            [(
                "coselfheat",
                ParameterValue::Integer(i32::from(self_heating)),
            )],
        )?,
    )?;
    prepare_hisim_hv_transport(&model, &instance, 311.15, local_rise)
}

fn close(label: &str, actual: f64, expected: f64, relative: f64) {
    let scale = expected.abs().max(1e-14);
    assert!(
        (actual - expected).abs() <= relative * scale,
        "{label}: got {actual:.16e}, expected {expected:.16e}"
    );
}

const fn physical_current() -> HiSimHvDrainCurrent {
    HiSimHvDrainCurrent {
        current_a: 0.34,
        electrical_derivative_a_per_v: HiSimHvElectricalGradient {
            inner_vds: 0.14,
            inner_vgs: 0.52,
            inner_vbs: -0.09,
            outer_vds: 0.041,
            outer_vgs: -0.022,
            outer_vbs: 0.017,
        },
        temperature_derivative_a_per_kelvin: 0.0035,
    }
}

fn power(
    state: &PreparedHiSimHvTransport,
    bias: HiSimHvDrainBias,
    drain: HiSimHvDrainCurrent,
) -> Result<HiSimHvGeneratedHeat, ModelError> {
    evaluate_hisim_hv_generated_heat(state, bias, drain)?.ok_or(
        ModelError::InvalidPhysicalParameter("missing enabled thermal power branch"),
    )
}

#[test]
fn native_power_voltage_partition_and_exact_powrat_unity() -> Result<(), ModelError> {
    for variant in [ModelVariant::HiSimHv1, ModelVariant::HiSimHv2] {
        let fields = [
            ("powrat", ParameterValue::Real(0.3)),
            ("prattemp1", ParameterValue::Real(0.001)),
            ("shemax", ParameterValue::Real(550.0)),
        ];
        let state = prepared(variant, &fields, true, 8.0)?;
        let drain = physical_current();
        let forward = power(
            &state,
            HiSimHvDrainBias {
                inner_vds_v: 2.0,
                outer_vds_v: 3.0,
            },
            drain,
        )?;
        close(
            "interpolated drain voltage",
            forward.effective_drain_voltage_v,
            state.power_coupling_ratio.mul_add(1.0, 2.0),
            1e-14,
        );
        close(
            "raw power",
            forward.raw_generated_power_w,
            drain.current_a * forward.effective_drain_voltage_v,
            1e-14,
        );

        let reverse = power(
            &state,
            HiSimHvDrainBias {
                inner_vds_v: 2.0,
                outer_vds_v: 1.0,
            },
            drain,
        )?;
        assert_eq!(reverse.effective_drain_voltage_v, 2.0);
        let tie = power(
            &state,
            HiSimHvDrainBias {
                inner_vds_v: 0.0,
                outer_vds_v: 1.0,
            },
            drain,
        )?;
        close(
            "sign-product zero uses interpolation",
            tie.effective_drain_voltage_v,
            state.power_coupling_ratio,
            1e-14,
        );

        let unity = prepared(
            variant,
            &[
                ("powrat", ParameterValue::Real(1.0)),
                ("prattemp1", ParameterValue::Real(-0.002)),
            ],
            true,
            8.0,
        )?;
        assert!(unity.power_coupling_ratio < 1.0);
        let exact = power(
            &unity,
            HiSimHvDrainBias {
                inner_vds_v: 2.0,
                outer_vds_v: 3.0,
            },
            drain,
        )?;
        assert_eq!(exact.effective_drain_voltage_v, 3.0);
        let reverse_exact = power(
            &unity,
            HiSimHvDrainBias {
                inner_vds_v: 2.0,
                outer_vds_v: 1.0,
            },
            drain,
        )?;
        assert_eq!(reverse_exact.effective_drain_voltage_v, 2.0);
    }
    Ok(())
}

#[test]
fn shemax_ceiling_preserves_native_smooth_limit_and_all_derivatives() -> Result<(), ModelError> {
    for variant in [ModelVariant::HiSimHv1, ModelVariant::HiSimHv2] {
        let state = prepared(
            variant,
            &[("shemax", ParameterValue::Real(350.0))],
            true,
            5.0,
        )?;
        let thermal = state
            .thermal_branch
            .ok_or(ModelError::InvalidPhysicalParameter(
                "missing passive thermal conductance",
            ))?;
        let current = HiSimHvDrainCurrent {
            current_a: 80.0,
            ..physical_current()
        };
        let result = power(
            &state,
            HiSimHvDrainBias {
                inner_vds_v: 3.0,
                outer_vds_v: 5.0,
            },
            current,
        )?;
        assert_eq!(result.effective_drain_voltage_v, 5.0);
        let ceiling = 350.0 * thermal.conductance_w_per_kelvin;
        let delta = 0.1 * thermal.conductance_w_per_kelvin;
        let arg = ceiling - result.raw_generated_power_w - delta;
        let radical = arg.mul_add(arg, (4.0 * ceiling * delta).abs()).sqrt();
        let source_power = (-0.5f64).mul_add(arg + radical, ceiling);
        let source_slope = f64::midpoint(1.0, arg / radical);
        close(
            "SHEMAX power",
            result.generated_power_w,
            source_power,
            1e-13,
        );
        close(
            "SHEMAX derivative",
            result.limit_derivative,
            source_slope,
            1e-13,
        );
        assert!(result.generated_power_w < result.raw_generated_power_w);
        assert!(result.generated_power_w <= ceiling);
        close(
            "limited gradient",
            result
                .generated_power_electrical_derivative_w_per_v
                .outer_vds,
            source_slope
                * (current
                    .electrical_derivative_a_per_v
                    .outer_vds
                    .mul_add(5.0, current.current_a)),
            1e-13,
        );
        close(
            "thermal residual",
            result.thermal_dc_residual_w,
            thermal.conductive_heat_w - result.generated_power_w,
            1e-13,
        );
        close(
            "thermal diagonal",
            result.thermal_dc_jacobian.temperature_w_per_kelvin,
            thermal.conductance_w_per_kelvin
                - result.generated_power_temperature_derivative_w_per_kelvin,
            1e-13,
        );
    }
    Ok(())
}

#[test]
fn native_temperature_row_matches_exact_node_level_stamping() -> Result<(), ModelError> {
    for variant in [ModelVariant::HiSimHv1, ModelVariant::HiSimHv2] {
        for pmos in [false, true] {
            let flags = if pmos {
                vec![("pmos", ParameterValue::Flag(true))]
            } else {
                Vec::new()
            };
            let state = prepared(variant, &flags, true, 7.0)?;
            let bias = HiSimHvDrainBias {
                inner_vds_v: 2.0,
                outer_vds_v: 3.5,
            };
            let result = power(&state, bias, physical_current())?;
            let stamp = result.thermal_dc_jacobian;
            let gradient = result.generated_power_electrical_derivative_w_per_v;
            let polarity = if pmos { -1.0 } else { 1.0 };
            close(
                "external drain",
                stamp.drain_external_w_per_v,
                -polarity * gradient.outer_vds,
                1e-14,
            );
            close(
                "internal drain",
                stamp.drain_internal_w_per_v,
                -polarity * gradient.inner_vds,
                1e-14,
            );
            close(
                "internal gate",
                stamp.gate_internal_w_per_v,
                -polarity * (gradient.inner_vgs + gradient.outer_vgs),
                1e-14,
            );
            close(
                "external source",
                stamp.source_external_w_per_v,
                polarity * (gradient.outer_vds + gradient.outer_vgs + gradient.outer_vbs),
                1e-14,
            );
            close(
                "internal source",
                stamp.source_internal_w_per_v,
                polarity * (gradient.inner_vds + gradient.inner_vgs + gradient.inner_vbs),
                1e-14,
            );
            close(
                "internal body",
                stamp.body_internal_w_per_v,
                -polarity * (gradient.inner_vbs + gradient.outer_vbs),
                1e-14,
            );
            let conservation = stamp.drain_external_w_per_v
                + stamp.drain_internal_w_per_v
                + stamp.gate_internal_w_per_v
                + stamp.source_external_w_per_v
                + stamp.source_internal_w_per_v
                + stamp.body_internal_w_per_v;
            assert!(
                conservation.abs() < 1e-12,
                "electrical thermal row violates source-node invariance: {conservation}"
            );
            let passive = state
                .thermal_branch
                .ok_or(ModelError::InvalidPhysicalParameter(
                    "missing passive branch",
                ))?;
            close(
                "thermal diagonal no type multiplier",
                stamp.temperature_w_per_kelvin,
                passive.conductance_w_per_kelvin
                    - result.generated_power_temperature_derivative_w_per_kelvin,
                1e-14,
            );
        }
    }
    Ok(())
}

/// Construct an affine drain-current test kernel whose exact gradients are
/// independently known. This validates power-chain-rule math, not MOS physics.
fn affine_sample(
    axis: usize,
    excursion: f64,
    baseline: HiSimHvDrainCurrent,
) -> HiSimHvDrainCurrent {
    let partials = baseline.electrical_derivative_a_per_v;
    let selected = match axis {
        0 => partials.inner_vds,
        1 => partials.inner_vgs,
        2 => partials.inner_vbs,
        3 => partials.outer_vds,
        4 => partials.outer_vgs,
        5 => partials.outer_vbs,
        6 => baseline.temperature_derivative_a_per_kelvin,
        _ => unreachable!("7-dimensional electrical/thermal source sample"),
    };
    HiSimHvDrainCurrent {
        current_a: selected.mul_add(excursion, baseline.current_a),
        ..baseline
    }
}

#[test]
fn full_power_derivatives_match_independent_seven_dimensional_finite_differences()
-> Result<(), ModelError> {
    for variant in [ModelVariant::HiSimHv1, ModelVariant::HiSimHv2] {
        for reverse in [false, true] {
            let state = prepared(
                variant,
                &[
                    ("powrat", ParameterValue::Real(0.42)),
                    ("shemax", ParameterValue::Real(420.0)),
                ],
                true,
                0.0,
            )?;
            let bias = HiSimHvDrainBias {
                inner_vds_v: 1.2,
                outer_vds_v: if reverse { 0.8 } else { 2.1 },
            };
            let baseline = physical_current();
            let source = power(&state, bias, baseline)?;
            let predicted = source.generated_power_electrical_derivative_w_per_v;
            let predictions = [
                predicted.inner_vds,
                predicted.inner_vgs,
                predicted.inner_vbs,
                predicted.outer_vds,
                predicted.outer_vgs,
                predicted.outer_vbs,
                source.generated_power_temperature_derivative_w_per_kelvin,
            ];
            for (axis, expected) in predictions.into_iter().enumerate() {
                let step = 1e-5;
                let mut observed = [0.0; 2];
                for (sample_index, excursion) in [-step, step].into_iter().enumerate() {
                    let mut sample_bias = bias;
                    if axis == 0 {
                        sample_bias.inner_vds_v += excursion;
                    }
                    if axis == 3 {
                        sample_bias.outer_vds_v += excursion;
                    }
                    let shifted_current = affine_sample(axis, excursion, baseline);
                    observed[sample_index] =
                        power(&state, sample_bias, shifted_current)?.generated_power_w;
                }
                let measured = (observed[1] - observed[0]) / (2.0 * step);
                close(
                    "seven-dimensional analytic/finite-difference chain rule",
                    expected,
                    measured,
                    2e-7,
                );
            }
        }
    }
    Ok(())
}

#[test]
fn capped_power_at_threshold_has_source_derivative_in_jacobian() -> Result<(), ModelError> {
    for variant in [ModelVariant::HiSimHv1, ModelVariant::HiSimHv2] {
        let state = prepared(
            variant,
            &[("shemax", ParameterValue::Real(500.0))],
            true,
            0.0,
        )?;
        let thermal = state
            .thermal_branch
            .ok_or(ModelError::InvalidPhysicalParameter(
                "missing thermal conductance",
            ))?;
        let ceiling = 500.0 * thermal.conductance_w_per_kelvin;
        let bias = HiSimHvDrainBias {
            inner_vds_v: 2.0,
            outer_vds_v: 3.0,
        };
        let current = HiSimHvDrainCurrent {
            current_a: ceiling / 3.0,
            ..physical_current()
        };
        let outcome = power(&state, bias, current)?;
        close(
            "near-ceiling raw power",
            outcome.raw_generated_power_w,
            ceiling,
            1e-14,
        );
        assert!((0.0..1.0).contains(&outcome.limit_derivative));
        assert!(outcome.limit_derivative > 0.4 && outcome.limit_derivative < 0.6);
        let h = 1e-6;
        let elevated = HiSimHvDrainCurrent {
            current_a: current.current_a + h,
            ..current
        };
        let reduced = HiSimHvDrainCurrent {
            current_a: current.current_a - h,
            ..current
        };
        let numerical = (power(&state, bias, elevated)?.generated_power_w
            - power(&state, bias, reduced)?.generated_power_w)
            / (2.0 * h);
        close(
            "SHEMAX active slope in current",
            numerical,
            outcome.limit_derivative * outcome.effective_drain_voltage_v,
            5e-8,
        );
    }
    Ok(())
}

#[test]
fn thermal_disabled_and_invalid_current_inputs_are_explicit() -> Result<(), ModelError> {
    let invalid_error = Err(ModelError::InvalidPhysicalParameter(
        "HiSIM-HV electrothermal power must be finite",
    ));
    for variant in [ModelVariant::HiSimHv1, ModelVariant::HiSimHv2] {
        let disabled = prepared(variant, &[], false, 0.0)?;
        let bias = HiSimHvDrainBias {
            inner_vds_v: 1.0,
            outer_vds_v: 2.0,
        };
        let physical = physical_current();
        assert_eq!(
            evaluate_hisim_hv_generated_heat(&disabled, bias, physical)?,
            None
        );
        let enabled = prepared(variant, &[], true, 0.0)?;
        assert_eq!(
            evaluate_hisim_hv_generated_heat(
                &enabled,
                HiSimHvDrainBias {
                    inner_vds_v: f64::NAN,
                    ..bias
                },
                physical
            ),
            invalid_error
        );
        assert_eq!(
            evaluate_hisim_hv_generated_heat(
                &enabled,
                bias,
                HiSimHvDrainCurrent {
                    current_a: f64::INFINITY,
                    ..physical
                }
            ),
            invalid_error
        );
        assert_eq!(
            evaluate_hisim_hv_generated_heat(
                &enabled,
                bias,
                HiSimHvDrainCurrent {
                    temperature_derivative_a_per_kelvin: f64::NEG_INFINITY,
                    ..physical
                }
            ),
            invalid_error
        );
        assert_eq!(
            evaluate_hisim_hv_generated_heat(
                &enabled,
                bias,
                HiSimHvDrainCurrent {
                    electrical_derivative_a_per_v: HiSimHvElectricalGradient {
                        outer_vbs: f64::NAN,
                        ..physical.electrical_derivative_a_per_v
                    },
                    ..physical
                }
            ),
            invalid_error
        );
    }
    Ok(())
}
