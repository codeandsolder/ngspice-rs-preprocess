use std::collections::BTreeSet;

use model_compact_intrinsic::{
    BoundParameters, KernelStatus, ModelError, ModelVariant, ParameterScope, ParameterType,
    ParameterValue, VARIANTS,
};

#[test]
fn model_levels_and_hisim_version_dispatch_match_upstream() {
    use ModelVariant::{
        Bsim3SoiDd, Bsim3SoiFd, Bsim3SoiPd, BsimSoi, HiSim2, HiSimHv1, HiSimHv2, Soi3,
    };
    for (level, expected) in [
        (68, HiSim2),
        (73, HiSimHv2),
        (10, BsimSoi),
        (58, BsimSoi),
        (55, Bsim3SoiFd),
        (56, Bsim3SoiDd),
        (57, Bsim3SoiPd),
        (60, Soi3),
    ] {
        assert_eq!(ModelVariant::from_level_version(level, None), Ok(expected));
    }
    for version in ["1.1", "1.2", "1.2.4"] {
        assert_eq!(
            ModelVariant::from_level_version(73, Some(version)),
            Ok(HiSimHv1)
        );
    }
    for version in ["2.0", "2.1.0", "2.2", "2.20"] {
        assert_eq!(
            ModelVariant::from_level_version(73, Some(version)),
            Ok(HiSimHv2)
        );
    }
    assert_eq!(
        ModelVariant::from_level_version(73, Some("2.3")),
        Err(ModelError::UnknownHiSimHvVersion("2.3".to_owned()))
    );
    assert_eq!(
        ModelVariant::from_level_version(62, None),
        Err(ModelError::UnknownLevel(62))
    );
}

#[test]
fn source_catalogs_are_sorted_unique_and_have_dense_canonical_slots() {
    let expected_counts = [
        (546, 32),
        (610, 38),
        (730, 38),
        (915, 38),
        (393, 16),
        (393, 16),
        (470, 25),
        (87, 25),
    ];
    for (variant, (model_count, instance_count)) in VARIANTS.into_iter().zip(expected_counts) {
        for (scope, count) in [
            (ParameterScope::Model, model_count),
            (ParameterScope::Instance, instance_count),
        ] {
            let catalog = variant.parameters(scope);
            assert_eq!(catalog.len(), count, "{variant:?} {scope:?}");
            assert!(catalog.windows(2).all(|pair| pair[0].name < pair[1].name));
            let slots: BTreeSet<_> = catalog.iter().map(|spec| spec.slot).collect();
            let max_slot = slots.last().copied().map_or(0, usize::from);
            assert_eq!(slots.len(), max_slot + 1);
            for spec in catalog {
                assert_eq!(variant.parameter(scope, spec.name), Some(spec));
                assert_eq!(
                    variant.parameter(scope, &spec.name.to_uppercase()),
                    Some(spec)
                );
            }
        }
        assert_eq!(ModelVariant::KERNEL_STATUS, KernelStatus::NotImplemented);
    }
}

#[test]
fn upstream_real_decks_have_only_recognized_model_parameter_names() {
    const HV2_ONLY: [&str; 17] = [
        "coerrrep", "gdl", "gdld", "gdlp", "ibpc1l", "ibpc1lp", "pt2", "pt4", "pt4p", "ptl",
        "ptlp", "ptp", "subld1l", "subld1lp", "xpdv", "xpvdth", "xpvdthg",
    ];
    let fixtures = [
        (
            ModelVariant::BsimSoi,
            include_str!("fixtures/bsim_soi_n.names"),
        ),
        (
            ModelVariant::BsimSoi,
            include_str!("fixtures/bsim_soi_p.names"),
        ),
        (
            ModelVariant::Bsim3SoiFd,
            include_str!("fixtures/bsim3_soi_fd.names"),
        ),
        (
            ModelVariant::Bsim3SoiDd,
            include_str!("fixtures/bsim3_soi_dd.names"),
        ),
        (
            ModelVariant::Bsim3SoiPd,
            include_str!("fixtures/bsim3_soi_pd.names"),
        ),
        (ModelVariant::HiSim2, include_str!("fixtures/hisim2.names")),
        (
            ModelVariant::HiSimHv1,
            include_str!("fixtures/hisim_hv1.names"),
        ),
        (
            ModelVariant::HiSimHv2,
            include_str!("fixtures/hisim_hv2.names"),
        ),
    ];
    // The upstream HV1 QA input includes 17 parameters only registered by HV2.
    // Keep the mismatch visible instead of silently accepting unknown extensions.
    for (variant, contents) in fixtures {
        let mut count = 0;
        let mut missing = Vec::new();
        for name in contents
            .lines()
            .filter(|line| !line.starts_with('#') && *line != "level")
        {
            count += 1;
            if variant.parameter(ParameterScope::Model, name).is_none() {
                missing.push(name);
            }
        }
        let expected_missing: &[&str] = if variant == ModelVariant::HiSimHv1 {
            &HV2_ONLY
        } else {
            &[]
        };
        assert_eq!(missing, expected_missing, "{variant:?} source mismatch");
        assert!(count >= 100, "fixture for {variant:?} unexpectedly small");
    }
}

#[test]
fn aliases_share_slots_but_model_and_instance_modes_remain_independent() -> Result<(), ModelError> {
    let variant = ModelVariant::BsimSoi;
    let canonical = variant.parameter(ParameterScope::Instance, "delvto");
    let alias = variant.parameter(ParameterScope::Instance, "delvt0");
    assert_eq!(canonical.map(|spec| spec.slot), alias.map(|spec| spec.slot));
    let canonical_slot = canonical
        .map(|spec| spec.slot)
        .ok_or_else(|| ModelError::UnknownParameter("delvto".to_owned()))?;
    let model = BoundParameters::bind(
        variant,
        ParameterScope::Model,
        [
            ("SOIMOD", ParameterValue::Integer(0)),
            ("CAPMOD", ParameterValue::Integer(2)),
        ],
    )?;
    let instance = BoundParameters::bind(
        variant,
        ParameterScope::Instance,
        [
            ("SOIMOD", ParameterValue::Integer(1)),
            ("L", ParameterValue::Real(0.25e-6)),
            ("delvto", ParameterValue::Real(0.1)),
            ("delvt0", ParameterValue::Real(0.2)),
        ],
    )?;
    assert_eq!(
        instance.get_slot(canonical_slot),
        Some(&ParameterValue::Real(0.2))
    );
    let model_slot = variant
        .parameter(ParameterScope::Model, "soimod")
        .ok_or_else(|| ModelError::UnknownParameter("soimod".to_owned()))?
        .slot;
    let instance_slot = variant
        .parameter(ParameterScope::Instance, "soimod")
        .ok_or_else(|| ModelError::UnknownParameter("soimod".to_owned()))?
        .slot;
    assert_eq!(
        model.get_slot(model_slot),
        Some(&ParameterValue::Integer(0))
    );
    assert_eq!(
        instance.get_slot(instance_slot),
        Some(&ParameterValue::Integer(1))
    );
    assert_eq!(instance.variant(), variant);
    assert_eq!(instance.scope(), ParameterScope::Instance);
    Ok(())
}

#[test]
fn hv_version_selector_types_and_thermal_controls_do_not_collapse_across_versions()
-> Result<(), ModelError> {
    let v1 = ModelVariant::HiSimHv1;
    let v2 = ModelVariant::HiSimHv2;
    for variant in [v1, v2] {
        assert_eq!(
            variant
                .parameter(ParameterScope::Model, "version")
                .map(|spec| spec.kind),
            Some(ParameterType::Text)
        );
        assert_eq!(variant.terminals().len(), 6);
        assert!(
            variant
                .parameter(ParameterScope::Instance, "coselfheat")
                .is_some()
        );
        let bound = BoundParameters::bind(
            variant,
            ParameterScope::Model,
            [(
                "version",
                ParameterValue::Text(if variant == v1 { "1.2.4" } else { "2.2.0" }.to_owned()),
            )],
        )?;
        let slot = variant
            .parameter(ParameterScope::Model, "version")
            .ok_or_else(|| ModelError::UnknownParameter("version".to_owned()))?
            .slot;
        assert!(matches!(
            bound.get_slot(slot),
            Some(ParameterValue::Text(_))
        ));
    }
    assert_eq!(ModelVariant::HiSim2.terminals().len(), 4);
    assert_eq!(ModelVariant::Soi3.terminals().len(), 6);
    assert_eq!(ModelVariant::BsimSoi.terminals()[4], "");
    assert_eq!(ModelVariant::BsimSoi.terminals().len(), 8);
    assert!(v2.parameter(ParameterScope::Model, "rth0").is_some());
    Ok(())
}

#[test]
fn candidate_binding_is_independent_and_bad_inputs_fail_closed() -> Result<(), ModelError> {
    let variant = ModelVariant::BsimSoi;
    let pairs = [("l", 0.2e-6), ("w", 1.0e-6)];
    let a = BoundParameters::bind(
        variant,
        ParameterScope::Instance,
        pairs
            .into_iter()
            .map(|(key, value)| (key, ParameterValue::Real(value))),
    )?;
    let b = BoundParameters::bind(
        variant,
        ParameterScope::Instance,
        [("l", ParameterValue::Real(0.5e-6))],
    )?;
    let l_slot = variant
        .parameter(ParameterScope::Instance, "l")
        .ok_or_else(|| ModelError::UnknownParameter("l".to_owned()))?
        .slot;
    assert_eq!(a.get_slot(l_slot), Some(&ParameterValue::Real(0.2e-6)));
    assert_eq!(b.get_slot(l_slot), Some(&ParameterValue::Real(0.5e-6)));
    assert_eq!(
        BoundParameters::bind(
            variant,
            ParameterScope::Model,
            [("capmod", ParameterValue::Real(2.0))]
        ),
        Err(ModelError::WrongType {
            name: "capmod".to_owned(),
            expected: ParameterType::Integer
        })
    );
    assert_eq!(
        BoundParameters::bind(
            variant,
            ParameterScope::Instance,
            [("l", ParameterValue::Real(f64::NAN))]
        ),
        Err(ModelError::NonFinite("l".to_owned()))
    );
    assert_eq!(
        BoundParameters::bind(
            variant,
            ParameterScope::Model,
            [("no_such_parameter", ParameterValue::Integer(1))]
        ),
        Err(ModelError::UnknownParameter("no_such_parameter".to_owned()))
    );
    let ic = BoundParameters::bind(
        ModelVariant::HiSim2,
        ParameterScope::Instance,
        [("ic", ParameterValue::RealVector(vec![0.0, 1.0, -2.0]))],
    )?;
    assert!(
        ic.get_slot(
            ModelVariant::HiSim2
                .parameter(ParameterScope::Instance, "ic")
                .ok_or_else(|| ModelError::UnknownParameter("ic".to_owned()))?
                .slot
        )
        .is_some()
    );
    assert_eq!(
        BoundParameters::bind(
            ModelVariant::HiSim2,
            ParameterScope::Instance,
            [("ic", ParameterValue::RealVector(vec![1.0, f64::INFINITY]))]
        ),
        Err(ModelError::NonFinite("ic".to_owned()))
    );
    Ok(())
}
