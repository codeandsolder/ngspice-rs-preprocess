use model_compact_intrinsic::{
    BoundParameters, ModelError, ModelVariant, ParameterScope, ParameterValue,
    PreparedHiSimHvInstance, PreparedHiSimHvModel, prepare_hisim_hv_instance,
    prepare_hisim_hv_model,
};

fn model(
    variant: ModelVariant,
    fields: &[(&str, ParameterValue)],
) -> Result<PreparedHiSimHvModel, ModelError> {
    let source = BoundParameters::bind(
        variant,
        ParameterScope::Model,
        fields.iter().map(|(name, value)| (*name, value.clone())),
    )?;
    prepare_hisim_hv_model(&source)
}

fn instance(
    model: &PreparedHiSimHvModel,
    fields: &[(&str, ParameterValue)],
) -> Result<PreparedHiSimHvInstance, ModelError> {
    let source = BoundParameters::bind(
        model.variant(),
        ParameterScope::Instance,
        fields.iter().map(|(name, value)| (*name, value.clone())),
    )?;
    prepare_hisim_hv_instance(model, &source)
}

fn instance_value<'a>(
    prepared: &'a PreparedHiSimHvInstance,
    name: &str,
) -> Result<&'a ParameterValue, ModelError> {
    let slot = prepared
        .variant()
        .parameter(ParameterScope::Instance, name)
        .ok_or_else(|| ModelError::UnknownParameter(name.to_owned()))?
        .slot;
    prepared
        .parameters()
        .get_slot(slot)
        .ok_or(ModelError::MissingDefaultDependency(slot))
}

fn binned_value(prepared: &PreparedHiSimHvInstance, name: &str) -> Result<f64, ModelError> {
    let slot = prepared
        .variant()
        .parameter(ParameterScope::Model, name)
        .ok_or_else(|| ModelError::UnknownParameter(name.to_owned()))?
        .slot;
    prepared
        .binned_model_slot(slot)
        .ok_or(ModelError::MissingDefaultDependency(slot))
}

#[test]
fn default_instances_inherit_models_and_compute_binned_parameters() -> Result<(), ModelError> {
    for variant in [ModelVariant::HiSimHv1, ModelVariant::HiSimHv2] {
        let prepared_model = model(variant, &[])?;
        let prepared = instance(&prepared_model, &[])?;
        let geometry = prepared.geometry();
        assert_eq!(geometry.gate_length_m, 2e-6);
        assert_eq!(geometry.gate_width_per_finger_m, 5e-6);
        assert_eq!(geometry.length_bin, 2.0);
        assert_eq!(geometry.width_bin, 5.0);
        assert_eq!(prepared.initial_conditions(), [0.0; 3]);
        assert_eq!(instance_value(&prepared, "nf")?, &ParameterValue::Real(1.0));
        assert_eq!(instance_value(&prepared, "m")?, &ParameterValue::Real(1.0));
        assert_eq!(
            instance_value(&prepared, "nsubcdfm")?,
            &ParameterValue::Real(0.0)
        );
        assert_eq!(
            instance_value(&prepared, "off")?,
            &ParameterValue::Flag(false)
        );
        assert_eq!(instance_value(&prepared, "l")?, &ParameterValue::Real(2e-6));
        assert_eq!(instance_value(&prepared, "w")?, &ParameterValue::Real(5e-6));
        assert_eq!(
            instance_value(&prepared, "rbpd")?,
            prepared_model
                .get_slot(
                    variant
                        .parameter(ParameterScope::Model, "rbpd")
                        .ok_or_else(|| ModelError::UnknownParameter("rbpd".to_owned()))?
                        .slot
                )
                .ok_or_else(|| ModelError::UnknownParameter("rbpd".to_owned()))?
        );
        assert_eq!(binned_value(&prepared, "vmax")?, 1e7);
        let count = variant
            .parameters(ParameterScope::Model)
            .iter()
            .filter(|spec| prepared.binned_model_slot(spec.slot).is_some())
            .count();
        assert_eq!(
            count,
            if variant == ModelVariant::HiSimHv1 {
                92
            } else {
                95
            }
        );
        assert!(
            prepared
                .binned_model_slot(
                    variant
                        .parameter(ParameterScope::Model, "tox")
                        .ok_or_else(|| ModelError::UnknownParameter("tox".to_owned()))?
                        .slot
                )
                .is_none()
        );
    }
    Ok(())
}

#[test]
fn binned_geometry_and_all_four_coefficient_terms_follow_c_macro() -> Result<(), ModelError> {
    for variant in [ModelVariant::HiSimHv1, ModelVariant::HiSimHv2] {
        let model = model(
            variant,
            &[
                ("vmax", ParameterValue::Real(8e6)),
                ("lvmax", ParameterValue::Real(100.0)),
                ("wvmax", ParameterValue::Real(-250.0)),
                ("pvmax", ParameterValue::Real(50.0)),
                ("xl", ParameterValue::Real(1e-6)),
                ("xw", ParameterValue::Real(2e-6)),
                ("lbinn", ParameterValue::Real(2.0)),
                ("wbinn", ParameterValue::Real(0.5)),
            ],
        )?;
        let result = instance(
            &model,
            &[
                ("l", ParameterValue::Real(10e-6)),
                ("w", ParameterValue::Real(60e-6)),
                ("nf", ParameterValue::Real(3.0)),
            ],
        )?;
        let geometry = result.geometry();
        assert!((geometry.gate_length_m - 11e-6).abs() < 1e-19);
        assert!((geometry.gate_width_per_finger_m - 22e-6).abs() < 1e-19);
        assert!((geometry.length_bin - 121.0).abs() < 1e-11);
        assert!((geometry.width_bin - 22f64.sqrt()).abs() < 1e-13);
        let expected = 8e6 + 100.0 / 121.0 - 250.0 / 22f64.sqrt() + 50.0 / (121.0 * 22f64.sqrt());
        assert!((binned_value(&result, "vmax")? - expected).abs() < 1e-8);
        assert_eq!(binned_value(&result, "bgtmp1")?, 90.25e-6);
    }
    Ok(())
}

#[test]
fn symmetry_hv1_and_hv2_respect_independent_given_flags() -> Result<(), ModelError> {
    for variant in [ModelVariant::HiSimHv1, ModelVariant::HiSimHv2] {
        let hv_model = model(variant, &[("cosym", ParameterValue::Integer(1))])?;
        let source = instance(
            &hv_model,
            &[
                ("lover", ParameterValue::Real(20e-9)),
                ("ldrift1", ParameterValue::Real(3e-6)),
                ("ldrift2", ParameterValue::Real(4e-6)),
            ],
        )?;
        assert_eq!(
            instance_value(&source, "ldrift1s")?,
            &ParameterValue::Real(3e-6)
        );
        assert_eq!(
            instance_value(&source, "ldrift2s")?,
            &ParameterValue::Real(4e-6)
        );
        // HV1 source unconditionally copies LOVERS to LOVER under COSYM and
        // may replace LOVERS with LOVERLD; HV2 keeps the provided LOVER.
        let expected_lovers = if variant == ModelVariant::HiSimHv1 {
            1e-6
        } else {
            20e-9
        };
        assert_eq!(
            instance_value(&source, "lovers")?,
            &ParameterValue::Real(expected_lovers)
        );
        assert_eq!(
            instance_value(&source, "lover")?,
            &ParameterValue::Real(expected_lovers)
        );
        let explicit_model = model(
            variant,
            &[
                ("cosym", ParameterValue::Integer(1)),
                ("ldrift1s", ParameterValue::Real(7e-6)),
            ],
        )?;
        let isolated = instance(&explicit_model, &[("ldrift1", ParameterValue::Real(3e-6))])?;
        assert_eq!(
            instance_value(&isolated, "ldrift1s")?,
            &ParameterValue::Real(7e-6)
        );
    }
    Ok(())
}

#[test]
fn initial_conditions_and_invalid_geometry_fail_explicitly() -> Result<(), ModelError> {
    for variant in [ModelVariant::HiSimHv1, ModelVariant::HiSimHv2] {
        let prepared_model = model(variant, &[])?;
        for values in [vec![0.0], vec![0.0, 1.0], vec![0.0, 1.0, 2.0]] {
            let length = values.len();
            let result = instance(
                &prepared_model,
                &[("ic", ParameterValue::RealVector(values.clone()))],
            )?;
            let mut expected = [0.0; 3];
            expected[..length].copy_from_slice(&values);
            assert_eq!(result.initial_conditions(), expected);
        }
        for length in [0, 4] {
            assert_eq!(
                instance(
                    &prepared_model,
                    &[("ic", ParameterValue::RealVector(vec![0.0; length]))]
                ),
                Err(ModelError::InvalidInitialConditions(length))
            );
        }
        let impossible_gate = model(variant, &[("xl", ParameterValue::Real(-2e-6))])?;
        assert_eq!(
            instance(&impossible_gate, &[]),
            Err(ModelError::InvalidPhysicalParameter(
                "HiSIM-HV binning geometry"
            ))
        );
        let impossible_binning = model(variant, &[("lbinn", ParameterValue::Real(1e308))])?;
        assert_eq!(
            instance(&impossible_binning, &[]),
            Err(ModelError::InvalidPhysicalParameter(
                "HiSIM-HV binning divisor"
            ))
        );
        let overflowed = model(
            variant,
            &[
                ("vmax", ParameterValue::Real(1.7e308)),
                ("lvmax", ParameterValue::Real(1e308)),
            ],
        )?;
        assert!(matches!(
            instance(&overflowed, &[]),
            Err(ModelError::InvalidBinningParameter(_))
        ));
        for (name, value) in [("l", 0.0), ("w", 0.0), ("nf", 0.0), ("m", -1.0)] {
            assert_eq!(
                instance(&prepared_model, &[(name, ParameterValue::Real(value))]),
                Err(ModelError::InvalidPhysicalParameter("instance L/W/NF/M"))
            );
        }
        for name in ["corbnet", "corg"] {
            assert_eq!(
                instance(&prepared_model, &[(name, ParameterValue::Integer(3))]),
                Err(ModelError::InvalidSelector {
                    name: name.to_owned(),
                    value: 3
                })
            );
        }
        let wrong = BoundParameters::bind(
            ModelVariant::HiSimHv2,
            ParameterScope::Instance,
            [("l", ParameterValue::Real(1e-6))],
        )?;
        if variant == ModelVariant::HiSimHv1 {
            assert_eq!(
                prepare_hisim_hv_instance(&prepared_model, &wrong),
                Err(ModelError::WrongModelScope)
            );
        }
    }
    Ok(())
}

#[test]
fn candidate_and_model_overrides_do_not_leak() -> Result<(), ModelError> {
    let variant = ModelVariant::HiSimHv2;
    let model = model(variant, &[("rd", ParameterValue::Real(0.42))])?;
    let base = instance(&model, &[])?;
    for i in 0..64 {
        let overrides = instance(
            &model,
            &[
                ("nf", ParameterValue::Real(f64::from(i + 1))),
                ("l", ParameterValue::Real(10e-6)),
                ("w", ParameterValue::Real(20e-6)),
                ("dtemp", ParameterValue::Real(f64::from(i) - 20.0)),
            ],
        )?;
        assert_eq!(
            instance_value(&overrides, "nf")?,
            &ParameterValue::Real(f64::from(i + 1))
        );
        assert_eq!(instance_value(&base, "nf")?, &ParameterValue::Real(1.0));
        assert_eq!(binned_value(&overrides, "rd")?, 0.42);
    }
    assert_eq!(instance_value(&base, "dtemp")?, &ParameterValue::Real(0.0));
    assert_eq!(
        instance(&model, &[("subld1", ParameterValue::Real(1e-6))]),
        Err(ModelError::UnsupportedInstanceParameter("subld1/subld2"))
    );
    Ok(())
}

#[test]
fn effective_channel_geometry_uses_upstream_mks_length_and_width_corrections()
-> Result<(), ModelError> {
    for variant in [ModelVariant::HiSimHv1, ModelVariant::HiSimHv2] {
        let model = model(
            variant,
            &[
                ("ll", ParameterValue::Real(2e-10)),
                ("lln", ParameterValue::Real(1.0)),
                ("wl", ParameterValue::Real(1e-10)),
                ("wln", ParameterValue::Real(1.0)),
                ("xld", ParameterValue::Real(0.2e-6)),
                ("xldld", ParameterValue::Real(0.4e-6)),
                ("xwd", ParameterValue::Real(0.3e-6)),
                ("xwdld", ParameterValue::Real(0.4e-6)),
                ("xwdc", ParameterValue::Real(0.5e-6)),
                ("wl1", ParameterValue::Real(1e-10)),
                ("wl1p", ParameterValue::Real(1.0)),
                ("wl2", ParameterValue::Real(1e-8)),
                ("wl2p", ParameterValue::Real(1.0)),
                ("wsti", ParameterValue::Real(0.2)),
                ("wstil", ParameterValue::Real(1e-3)),
                ("wstilp", ParameterValue::Real(1.0)),
                ("wstiw", ParameterValue::Real(2e-3)),
                ("wstiwp", ParameterValue::Real(1.0)),
                ("clm5", ParameterValue::Real(1.0)),
                ("clm6", ParameterValue::Real(0.3)),
            ],
        )?;
        let instance = instance(
            &model,
            &[
                ("l", ParameterValue::Real(40e-6)),
                ("w", ParameterValue::Real(120e-6)),
                ("nf", ParameterValue::Real(3.0)),
            ],
        )?;
        let b = instance.geometry();
        let e = instance.effective_geometry();
        let lg = b.gate_length_m;
        let wg = b.gate_width_per_finger_m;
        let lg_um = lg * 1e6;
        let wg_um = wg * 1e6;
        let area_um2 = lg_um * wg_um;
        let length_correction = (2e-10 / 100.0) / lg;
        let width_correction = (1e-10 / 100.0) / wg;
        let expected_length = lg - (0.2e-6 + length_correction + 0.4e-6 + length_correction);
        let expected_width = 2.0f64.mul_add(-(0.3e-6 + width_correction), wg);
        let expected_cv_width = 2.0f64.mul_add(-(0.5e-6 + width_correction), wg);
        let expected_lgatesm = lg + 1e-10 / area_um2;
        let expected_wsti = 0.2 * (1.0 + 1e-3 / (expected_lgatesm * 1e6)) * (1.0 + 2e-3 / wg_um);
        for (actual, expected) in [
            (e.channel_length_m, expected_length),
            (e.channel_width_per_finger_m, expected_width),
            (
                e.lateral_diffusion_width_per_finger_m,
                2.0f64.mul_add(-(0.4e-6 + width_correction), wg),
            ),
            (e.capacitance_width_per_finger_m, expected_cv_width),
            (e.channel_width_total_m, expected_width * 3.0),
            (e.capacitance_width_total_m, expected_cv_width * 3.0),
            (e.small_device_gate_length_m, expected_lgatesm),
            (e.small_device_threshold_correction_v, 1e-8 / area_um2),
            (e.width_sti, expected_wsti),
            (e.channel_length_modulation_factor, lg_um.mul_add(0.3, 1.0)),
        ] {
            assert!(
                (actual - expected).abs() < 1e-12 * expected.abs().max(1e-30),
                "{variant:?}: actual {actual:e}, source equation {expected:e}"
            );
        }
    }
    Ok(())
}

#[test]
fn nonpositive_effective_channel_dimensions_are_rejected() -> Result<(), ModelError> {
    for variant in [ModelVariant::HiSimHv1, ModelVariant::HiSimHv2] {
        for (name, value, message) in [
            ("xldld", 20e-6, "HiSIM-HV effective channel length"),
            ("xwd", 30e-6, "HiSIM-HV effective channel width"),
        ] {
            let model = model(variant, &[(name, ParameterValue::Real(value))])?;
            assert_eq!(
                instance(
                    &model,
                    &[
                        ("l", ParameterValue::Real(10e-6)),
                        ("w", ParameterValue::Real(50e-6)),
                    ]
                ),
                Err(ModelError::InvalidPhysicalParameter(message))
            );
        }
    }
    Ok(())
}
