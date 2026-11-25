//! Source-derived HiSIM-HV mobility and velocity preparation regression tests.
use model_compact_intrinsic::{
    BoundParameters, ModelError, ModelVariant, ParameterScope, ParameterValue,
    PreparedHiSimHvInstance, PreparedHiSimHvModel, prepare_hisim_hv_instance,
    prepare_hisim_hv_model, prepare_hisim_hv_transport,
};

fn model(
    v: ModelVariant,
    fields: &[(&str, ParameterValue)],
) -> Result<PreparedHiSimHvModel, ModelError> {
    prepare_hisim_hv_model(&BoundParameters::bind(
        v,
        ParameterScope::Model,
        fields.iter().map(|(n, x)| (*n, x.clone())),
    )?)
}
fn device(
    m: &PreparedHiSimHvModel,
    fields: &[(&str, ParameterValue)],
) -> Result<PreparedHiSimHvInstance, ModelError> {
    prepare_hisim_hv_instance(
        m,
        &BoundParameters::bind(
            m.variant(),
            ParameterScope::Instance,
            fields.iter().map(|(n, x)| (*n, x.clone())),
        )?,
    )
}
fn close(x: f64, y: f64, tol: f64) {
    assert!(
        (x - y).abs() <= tol * y.abs().max(1e-30),
        "got {x:.16e}, expected {y:.16e}"
    );
}
#[test]
fn default_transport_is_finite() -> Result<(), ModelError> {
    for v in [ModelVariant::HiSimHv1, ModelVariant::HiSimHv2] {
        let m = model(v, &[])?;
        let i = device(&m, &[])?;
        assert_eq!(i.transport_geometry().half_layout_length_m, 0.0);
        assert!(i.transport_geometry().phonon_scattering_coefficient > 0.0);
        let t = prepare_hisim_hv_transport(&m, &i, 300.15, 0.0)?;
        assert!(t.phonon_coefficient > 0.0);
        assert!(t.saturation_velocity_m_per_s > 0.0);
        assert!(t.thermal_resistance_kelvin_per_watt.is_finite());
        assert!((0.0..=1.0).contains(&t.power_coupling_ratio));
        assert_eq!(t.depletion, None);
    }
    Ok(())
}

#[test]
fn dfm_layout_mobility_uses_original_given_flags_and_correct_units() -> Result<(), ModelError> {
    for v in [ModelVariant::HiSimHv1, ModelVariant::HiSimHv2] {
        let m = model(
            v,
            &[
                ("codfm", ParameterValue::Integer(1)),
                ("mphdfm", ParameterValue::Real(0.25)),
                ("mueph1", ParameterValue::Real(12_000.0)),
                ("nsubc", ParameterValue::Real(1e17)),
                ("muesti1", ParameterValue::Real(2e-3)),
                ("muesti2", ParameterValue::Real(0.2)),
                ("muesti3", ParameterValue::Real(2.0)),
                ("saref", ParameterValue::Real(1.1e-6)),
                ("sbref", ParameterValue::Real(1.3e-6)),
            ],
        )?;
        let fields = [
            ("l", ParameterValue::Real(10e-6)),
            ("w", ParameterValue::Real(90e-6)),
            ("nf", ParameterValue::Real(3.0)),
            ("sa", ParameterValue::Real(0.8e-6)),
            ("sb", ParameterValue::Real(1.2e-6)),
            ("sd", ParameterValue::Real(1.5e-6)),
        ];
        let initial = device(&m, &fields)?;
        let mut assigned = fields.to_vec();
        assigned.push(("nsubcdfm", ParameterValue::Real(2e17)));
        let dfm = device(&m, &assigned)?;
        let g = dfm.transport_geometry();
        let reference = 2.0 / (1.0 / (1.1e-6 + 5e-6) + 1.0 / (1.3e-6 + 5e-6));
        let reciprocal_sum = (0..3)
            .map(|j| {
                let d = f64::from(j) * 11.5e-6;
                1.0 / (5.8e-6 + d) + 1.0 / (6.2e-6 + d)
            })
            .sum::<f64>();
        let lod = 6.0 / reciprocal_sum;
        close(g.reference_half_layout_length_m, reference, 1e-13);
        close(g.half_layout_length_m, lod, 1e-13);
        let base_phonon = initial.transport_geometry().phonon_scattering_coefficient;
        close(
            g.phonon_scattering_coefficient / base_phonon,
            0.25f64.mul_add(2f64.ln(), 1.0),
            2e-13,
        );
        let factor = (1.0 + (2e-5 / lod).powi(2) / 1.2) / (1.0 + (2e-5 / reference).powi(2) / 1.2);
        close(base_phonon, 12_000.0 * factor, 2e-13);
        let no_layout = device(&m, &fields[..3])?;
        assert_eq!(no_layout.transport_geometry().half_layout_length_m, 0.0);
    }
    Ok(())
}

#[test]
fn cotemp_velocity_and_density_respect_heated_branch() -> Result<(), ModelError> {
    for v in [ModelVariant::HiSimHv1, ModelVariant::HiSimHv2] {
        for cotemp in [0, 1, 2, 3] {
            let m = model(
                v,
                &[
                    ("cotemp", ParameterValue::Integer(cotemp)),
                    ("vmax", ParameterValue::Real(1.5e7)),
                    ("vtmp", ParameterValue::Real(0.08)),
                    ("vmaxt1", ParameterValue::Real(0.002)),
                    ("vmaxt2", ParameterValue::Real(2e-6)),
                    ("muetmp", ParameterValue::Real(1.6)),
                    ("ninvdt1", ParameterValue::Real(0.01)),
                    ("ninvdt2", ParameterValue::Real(1e-4)),
                    ("powrat", ParameterValue::Real(0.6)),
                    ("prattemp1", ParameterValue::Real(0.0001)),
                ],
            )?;
            let i = device(
                &m,
                &[
                    ("dtemp", ParameterValue::Real(4.0)),
                    ("coselfheat", ParameterValue::Integer(1)),
                ],
            )?;
            let at = prepare_hisim_hv_transport(&m, &i, 320.0, 11.0)?;
            let nominal = at.temperature.nominal_temperature_kelvin;
            let numerator_t: f64 = if cotemp == 2 { 335.0 } else { 324.0 };
            let diff = numerator_t - nominal;
            let diff2 = nominal.mul_add(-nominal, numerator_t * numerator_t);
            let ratio: f64 = 335.0 / nominal;
            let denominator = 0.08f64.mul_add(
                -(1.0 - ratio),
                0.1f64.mul_add(ratio * ratio, 0.4f64.mul_add(ratio, 1.8)),
            );
            let velocity = i.transport_geometry().saturation_velocity_geometry_factor
                * (1.5e7 / 100.0)
                * (2e-6f64.mul_add(diff2, 0.002f64.mul_add(diff, 1.0)))
                / denominator;
            close(at.saturation_velocity_m_per_s, velocity, 1e-12);
            close(
                at.density_gradient_coefficient,
                i.transport_geometry().density_gradient_base
                    * (1e-4f64.mul_add(diff2, 0.01f64.mul_add(diff, 1.0))),
                1e-12,
            );
            close(
                at.phonon_coefficient,
                ratio.powf(1.6) / i.transport_geometry().phonon_scattering_coefficient,
                1e-12,
            );
            let h = 0.01;
            let plus = prepare_hisim_hv_transport(&m, &i, 320.0, 11.0 + h)?;
            let minus = prepare_hisim_hv_transport(&m, &i, 320.0, 11.0 - h)?;
            close(
                at.saturation_velocity_derivative_m_per_s_kelvin,
                (plus.saturation_velocity_m_per_s - minus.saturation_velocity_m_per_s) / (2.0 * h),
                4e-8,
            );
            close(
                at.phonon_coefficient_derivative_per_kelvin,
                (plus.phonon_coefficient - minus.phonon_coefficient) / (2.0 * h),
                4e-8,
            );
            close(
                at.density_gradient_derivative_per_kelvin,
                (plus.density_gradient_coefficient - minus.density_gradient_coefficient)
                    / (2.0 * h),
                4e-8,
            );
            assert_eq!(
                at.thermal_resistance_kelvin_per_watt,
                plus.thermal_resistance_kelvin_per_watt
            );
            assert_eq!(at.power_coupling_ratio, plus.power_coupling_ratio);
            if cotemp != 2 {
                assert_eq!(at.density_gradient_derivative_per_kelvin, 0.0);
            }
        }
    }
    Ok(())
}

#[test]
fn hv2_depletion_transport_is_mode_specific_and_differentiable() -> Result<(), ModelError> {
    let variant = ModelVariant::HiSimHv2;
    let hv_model = model(
        variant,
        &[
            ("codep", ParameterValue::Integer(1)),
            ("depmuetmp", ParameterValue::Real(1.8)),
            ("depmueph1", ParameterValue::Real(0.6)),
            ("depmueph0", ParameterValue::Real(0.2)),
            ("depvmax", ParameterValue::Real(2.0e7)),
            ("depvtmp", ParameterValue::Real(0.2)),
        ],
    )?;
    let hv_instance = device(&hv_model, &[("coselfheat", ParameterValue::Integer(1))])?;
    let state = prepare_hisim_hv_transport(&hv_model, &hv_instance, 325.0, 7.0)?;
    let Some(dep) = state.depletion else {
        return Err(ModelError::InvalidPhysicalParameter(
            "missing HV2 depletion transport",
        ));
    };
    let ratio: f64 = state.temperature.temperature_ratio;
    let phn = ratio.powf(1.8) / 0.6;
    close(dep.phonon_coefficient, phn, 1e-12);
    close(dep.phonon_weighted_coefficient, phn * 0.2, 1e-12);
    let denominator = 0.2f64.mul_add(
        -(1.0 - ratio),
        0.1f64.mul_add(ratio * ratio, 0.4f64.mul_add(ratio, 1.8)),
    );
    close(
        dep.saturation_velocity_m_per_s,
        (2.0e7 / 100.0) / denominator,
        1e-12,
    );
    let plus = prepare_hisim_hv_transport(&hv_model, &hv_instance, 325.0, 7.01)?;
    let minus = prepare_hisim_hv_transport(&hv_model, &hv_instance, 325.0, 6.99)?;
    let (Some(higher), Some(lower)) = (plus.depletion, minus.depletion) else {
        return Err(ModelError::InvalidPhysicalParameter("lost depletion mode"));
    };
    close(
        dep.phonon_coefficient_derivative_per_kelvin,
        (higher.phonon_coefficient - lower.phonon_coefficient) / 0.02,
        5e-8,
    );
    close(
        dep.saturation_velocity_derivative_m_per_s_kelvin,
        (higher.saturation_velocity_m_per_s - lower.saturation_velocity_m_per_s) / 0.02,
        5e-8,
    );
    let m2 = model(variant, &[])?;
    assert_eq!(
        prepare_hisim_hv_transport(&m2, &device(&m2, &[])?, 325.0, 0.0)?.depletion,
        None
    );
    Ok(())
}

#[test]
fn fractional_nf_iterates_as_native_int_and_invalid_dfm_is_explicit() -> Result<(), ModelError> {
    for v in [ModelVariant::HiSimHv1, ModelVariant::HiSimHv2] {
        let m = model(
            v,
            &[
                ("codfm", ParameterValue::Integer(1)),
                ("muesti1", ParameterValue::Real(1e-3)),
            ],
        )?;
        let f = [
            ("l", ParameterValue::Real(10e-6)),
            ("nf", ParameterValue::Real(2.5)),
            ("sa", ParameterValue::Real(1e-6)),
            ("sb", ParameterValue::Real(2e-6)),
            ("sd", ParameterValue::Real(3e-6)),
        ];
        let i = device(&m, &f)?;
        let sum = (0..3)
            .map(|j| {
                let offset = f64::from(j) * 13e-6;
                1.0 / (6e-6 + offset) + 1.0 / (7e-6 + offset)
            })
            .sum::<f64>();
        close(
            i.transport_geometry().half_layout_length_m,
            5.0 / sum,
            1e-13,
        );
        assert_eq!(
            device(&m, &[("nsubcdfm", ParameterValue::Real(-1.0))]),
            Err(ModelError::InvalidPhysicalParameter(
                "HiSIM-HV DFM NSUBCDFM"
            ))
        );
    }
    Ok(())
}

#[test]
fn ambient_thermal_resistance_and_twice_smoothed_power_ratio_match_source() -> Result<(), ModelError>
{
    for variant in [ModelVariant::HiSimHv1, ModelVariant::HiSimHv2] {
        let hv_model = model(
            variant,
            &[
                ("rth0", ParameterValue::Real(0.3)),
                ("rth0nf", ParameterValue::Real(0.5)),
                ("rth0w", ParameterValue::Real(0.3)),
                ("rth0wp", ParameterValue::Real(1.0)),
                ("rthtemp1", ParameterValue::Real(1e-3)),
                ("rthtemp2", ParameterValue::Real(2e-5)),
                ("powrat", ParameterValue::Real(0.6)),
                ("prattemp1", ParameterValue::Real(1e-4)),
                ("prattemp2", ParameterValue::Real(1e-6)),
            ],
        )?;
        let transistor = device(
            &hv_model,
            &[
                ("l", ParameterValue::Real(10e-6)),
                ("w", ParameterValue::Real(60e-6)),
                ("nf", ParameterValue::Real(2.0)),
                ("m", ParameterValue::Real(3.0)),
                ("dtemp", ParameterValue::Real(5.0)),
                ("coselfheat", ParameterValue::Integer(1)),
            ],
        )?;
        let transport = prepare_hisim_hv_transport(&hv_model, &transistor, 320.0, 19.0)?;
        let nominal = transport.temperature.nominal_temperature_kelvin;
        let ambient = 325.0;
        let diff = ambient - nominal;
        let diff2 = nominal.mul_add(-nominal, ambient * ambient);
        let source_resistance = transistor
            .transport_geometry()
            .thermal_resistance_geometry_factor
            * 2e-5f64.mul_add(diff2, 1e-3f64.mul_add(diff, 0.3 / 100.0));
        close(
            transport.thermal_resistance_kelvin_per_watt,
            source_resistance,
            2e-13,
        );
        let raw = 1e-6f64.mul_add(diff2, 1e-4f64.mul_add(diff, 0.6));
        let lower = (raw - 0.05f64).max(0.0);
        let t1 = 0.95 - lower;
        let upper = 1.0 - f64::midpoint(t1, t1.mul_add(t1, 0.2).sqrt());
        close(transport.power_coupling_ratio, upper, 2e-13);
        let hotter = prepare_hisim_hv_transport(&hv_model, &transistor, 320.0, 39.0)?;
        assert_eq!(
            transport.thermal_resistance_kelvin_per_watt,
            hotter.thermal_resistance_kelvin_per_watt
        );
        assert_eq!(transport.power_coupling_ratio, hotter.power_coupling_ratio);
    }
    Ok(())
}

#[test]
fn transport_invalid_inputs_are_explicit_and_source_state_is_immutable() -> Result<(), ModelError> {
    for variant in [ModelVariant::HiSimHv1, ModelVariant::HiSimHv2] {
        let nonphysical = model(variant, &[("muesti2", ParameterValue::Real(-1.0))])?;
        let invalid_layout = device(
            &nonphysical,
            &[
                ("sa", ParameterValue::Real(1e-6)),
                ("sb", ParameterValue::Real(1e-6)),
            ],
        );
        assert_eq!(
            invalid_layout,
            Err(ModelError::InvalidPhysicalParameter(
                "HiSIM-HV LOD phonon coefficient"
            ))
        );
        let velocity_model = model(variant, &[("vtmp", ParameterValue::Real(1e4))])?;
        let velocity_instance = device(&velocity_model, &[])?;
        assert_eq!(
            prepare_hisim_hv_transport(&velocity_model, &velocity_instance, 270.0, 0.0),
            Err(ModelError::InvalidPhysicalParameter(
                "HiSIM-HV velocity temperature denominator"
            ))
        );
        let normal = model(variant, &[])?;
        let device = device(&normal, &[("coselfheat", ParameterValue::Integer(1))])?;
        let baseline = prepare_hisim_hv_transport(&normal, &device, 300.15, 0.0)?;
        let static_geometry = device.transport_geometry();
        for i in 0..128 {
            let ambient = f64::from(i).mul_add(0.2, 285.0);
            let rise = f64::from(i % 7);
            let trial = prepare_hisim_hv_transport(&normal, &device, ambient, rise)?;
            assert!(trial.phonon_coefficient.is_finite());
            assert!(trial.saturation_velocity_m_per_s.is_finite());
            assert_eq!(device.transport_geometry(), static_geometry);
        }
        assert_eq!(
            prepare_hisim_hv_transport(&normal, &device, 300.15, 0.0)?,
            baseline
        );
    }
    let depletion = model(
        ModelVariant::HiSimHv2,
        &[
            ("codep", ParameterValue::Integer(1)),
            ("depmueph1", ParameterValue::Real(0.0)),
        ],
    )?;
    let prepared = device(&depletion, &[])?;
    assert_eq!(
        prepare_hisim_hv_transport(&depletion, &prepared, 300.15, 0.0),
        Err(ModelError::InvalidPhysicalParameter(
            "HiSIM-HV depletion phonon base"
        ))
    );
    Ok(())
}

#[test]
fn thermal_branch_matches_native_passive_heat_and_charge_laws() -> Result<(), ModelError> {
    for variant in [ModelVariant::HiSimHv1, ModelVariant::HiSimHv2] {
        let hv_model = model(
            variant,
            &[
                ("cth0", ParameterValue::Real(4e-7)),
                ("rth0", ParameterValue::Real(0.6)),
                ("rth0nf", ParameterValue::Real(0.3)),
            ],
        )?;
        let transistor = device(
            &hv_model,
            &[
                ("coselfheat", ParameterValue::Integer(1)),
                ("l", ParameterValue::Real(10e-6)),
                ("w", ParameterValue::Real(90e-6)),
                ("nf", ParameterValue::Real(3.0)),
                ("m", ParameterValue::Real(2.0)),
            ],
        )?;
        let local_rise = 16.0;
        let state = prepare_hisim_hv_transport(&hv_model, &transistor, 315.0, local_rise)?;
        let Some(thermal) = state.thermal_branch else {
            return Err(ModelError::InvalidPhysicalParameter(
                "thermal branch missing",
            ));
        };
        let expected_capacity =
            4e-7 * 100.0 * 2.0 * transistor.effective_geometry().channel_width_total_m;
        close(thermal.heat_capacity_j_per_kelvin, expected_capacity, 1e-13);
        close(thermal.stored_heat_j, expected_capacity * local_rise, 1e-13);
        close(
            thermal.conductance_w_per_kelvin,
            1.0 / state.thermal_resistance_kelvin_per_watt.max(1e-4),
            1e-13,
        );
        close(
            thermal.conductive_heat_w,
            thermal.conductance_w_per_kelvin * local_rise,
            1e-13,
        );
        let colder = prepare_hisim_hv_transport(&hv_model, &transistor, 315.0, -8.0)?;
        let Some(cold) = colder.thermal_branch else {
            return Err(ModelError::InvalidPhysicalParameter(
                "cold thermal branch missing",
            ));
        };
        close(
            cold.conductance_w_per_kelvin,
            thermal.conductance_w_per_kelvin,
            1e-13,
        );
        close(cold.heat_capacity_j_per_kelvin, expected_capacity, 1e-13);
        close(
            cold.conductive_heat_w,
            -8.0 * cold.conductance_w_per_kelvin,
            1e-13,
        );
        close(
            cold.stored_heat_j,
            -8.0 * cold.heat_capacity_j_per_kelvin,
            1e-13,
        );
    }
    Ok(())
}

#[test]
fn self_heating_gate_and_native_resistance_floor_are_independent() -> Result<(), ModelError> {
    for variant in [ModelVariant::HiSimHv1, ModelVariant::HiSimHv2] {
        let hv_model = model(
            variant,
            &[
                ("cth0", ParameterValue::Real(1e-8)),
                ("rth0", ParameterValue::Real(0.0)),
            ],
        )?;
        let disabled = device(&hv_model, &[])?;
        assert_eq!(
            prepare_hisim_hv_transport(&hv_model, &disabled, 300.15, 0.0)?.thermal_branch,
            None
        );
        assert_eq!(
            prepare_hisim_hv_transport(&hv_model, &disabled, 300.15, 2.0),
            Err(ModelError::InvalidPhysicalParameter(
                "HiSIM-HV local temperature rise requires COSELFHEAT"
            ))
        );
        let enabled = device(&hv_model, &[("coselfheat", ParameterValue::Integer(1))])?;
        let active = prepare_hisim_hv_transport(&hv_model, &enabled, 300.15, 2.0)?;
        let Some(thermal) = active.thermal_branch else {
            return Err(ModelError::InvalidPhysicalParameter(
                "passive thermal branch not active",
            ));
        };
        assert_eq!(active.thermal_resistance_kelvin_per_watt, 0.0);
        close(thermal.conductance_w_per_kelvin, 10_000.0, 1e-13);
        close(thermal.conductive_heat_w, 20_000.0, 1e-13);
        close(
            thermal.heat_capacity_j_per_kelvin,
            1e-8 * 100.0 * enabled.effective_geometry().channel_width_total_m,
            1e-13,
        );
        let explicitly_disabled = device(&hv_model, &[("coselfheat", ParameterValue::Integer(0))])?;
        assert_eq!(
            prepare_hisim_hv_transport(&hv_model, &explicitly_disabled, 300.15, 0.0)?
                .thermal_branch,
            None
        );
    }
    Ok(())
}
