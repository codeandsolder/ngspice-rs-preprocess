use model_compact_intrinsic::{
    BoundParameters, ModelError, ModelVariant, MosPolarity, ParameterScope, ParameterValue,
    PreparedHiSimHvModel, prepare_hisim_hv_model,
};

fn prepare(
    variant: ModelVariant,
    inputs: &[(&str, ParameterValue)],
) -> Result<PreparedHiSimHvModel, ModelError> {
    let raw = BoundParameters::bind(
        variant,
        ParameterScope::Model,
        inputs.iter().map(|(name, value)| (*name, value.clone())),
    )?;
    prepare_hisim_hv_model(&raw)
}

fn param<'a>(
    model: &'a PreparedHiSimHvModel,
    name: &str,
) -> Result<&'a ParameterValue, ModelError> {
    model
        .variant()
        .parameter(ParameterScope::Model, name)
        .and_then(|spec| model.get_slot(spec.slot))
        .ok_or_else(|| ModelError::UnknownParameter(name.to_owned()))
}

#[test]
fn implementations_keep_distinct_source_defaults() -> Result<(), ModelError> {
    let first = prepare(ModelVariant::HiSimHv1, &[])?;
    let second = prepare(ModelVariant::HiSimHv2, &[])?;
    assert_eq!(first.polarity(), MosPolarity::Nmos);
    assert_eq!(second.polarity(), MosPolarity::Nmos);
    assert_eq!(
        param(&first, "version")?,
        &ParameterValue::Text("1.24".to_owned())
    );
    assert_eq!(
        param(&second, "version")?,
        &ParameterValue::Text("2.20".to_owned())
    );
    assert_eq!(param(&first, "corsrd")?, &ParameterValue::Integer(3));
    assert_eq!(param(&second, "corsrd")?, &ParameterValue::Integer(0));
    assert_eq!(param(&first, "tox")?, &ParameterValue::Real(30e-9));
    assert_eq!(param(&second, "tox")?, &ParameterValue::Real(7e-9));
    assert_eq!(param(&first, "mueph1")?, &ParameterValue::Real(25e3));
    assert_eq!(param(&second, "mueph1")?, &ParameterValue::Real(20e3));
    assert_eq!(param(&second, "coldrift")?, &ParameterValue::Integer(1));
    assert_eq!(param(&second, "vgsmin")?, &ParameterValue::Real(-100.0));
    Ok(())
}

#[test]
fn symmetry_uses_given_bits_and_replaces_earlier_defaults() -> Result<(), ModelError> {
    for variant in [ModelVariant::HiSimHv1, ModelVariant::HiSimHv2] {
        let a = prepare(variant, &[("cosym", ParameterValue::Integer(1))])?;
        assert_eq!(param(&a, "rs")?, param(&a, "rd")?);
        assert_eq!(param(&a, "cgso")?, param(&a, "cgdo")?);
        assert_eq!(param(&a, "ldrift1s")?, param(&a, "ldrift1")?);
        assert_eq!(param(&a, "ldrift2s")?, param(&a, "ldrift2")?);
        assert_eq!(param(&a, "novers")?, param(&a, "nover")?);
        let override_rd = prepare(
            variant,
            &[
                ("cosym", ParameterValue::Integer(1)),
                ("rd", ParameterValue::Real(0.12)),
                ("rs", ParameterValue::Real(0.03)),
            ],
        )?;
        assert_eq!(param(&override_rd, "rs")?, &ParameterValue::Real(0.03));
        assert_eq!(param(&override_rd, "rd")?, &ParameterValue::Real(0.12));
        let default = prepare(variant, &[])?;
        assert_ne!(param(&default, "ldrift1s")?, param(&default, "ldrift1")?);
    }
    Ok(())
}

#[test]
fn depletion_and_drift_follow_hv2_model_rules() -> Result<(), ModelError> {
    let normal = prepare(ModelVariant::HiSimHv2, &[])?;
    let depleted = prepare(
        ModelVariant::HiSimHv2,
        &[("codep", ParameterValue::Integer(1))],
    )?;
    assert_eq!(param(&normal, "vfbc")?, &ParameterValue::Real(-1.0));
    assert_eq!(param(&depleted, "vfbc")?, &ParameterValue::Real(-0.2));
    assert_eq!(param(&depleted, "nsubc")?, &ParameterValue::Real(5e16));
    assert_eq!(param(&normal, "nsubc")?, &ParameterValue::Real(3e17));
    assert_eq!(param(&depleted, "copprv")?, &ParameterValue::Integer(0));
    assert_eq!(param(&normal, "copprv")?, &ParameterValue::Integer(1));
    let manual = prepare(
        ModelVariant::HiSimHv2,
        &[
            ("codep", ParameterValue::Integer(1)),
            ("vfbc", ParameterValue::Real(-0.71)),
            ("cordrift", ParameterValue::Integer(0)),
            ("corsrd", ParameterValue::Integer(2)),
        ],
    )?;
    assert_eq!(param(&manual, "vfbc")?, &ParameterValue::Real(-0.71));
    assert_eq!(param(&manual, "corsrd")?, &ParameterValue::Integer(2));
    let forced = prepare(
        ModelVariant::HiSimHv2,
        &[("corsrd", ParameterValue::Integer(2))],
    )?;
    assert_eq!(param(&forced, "corsrd")?, &ParameterValue::Integer(0));
    Ok(())
}

#[test]
fn polarity_versions_and_unsupported_selectors_are_explicit() -> Result<(), ModelError> {
    for variant in [ModelVariant::HiSimHv1, ModelVariant::HiSimHv2] {
        let pmos = prepare(variant, &[("pmos", ParameterValue::Flag(true))])?;
        assert_eq!(pmos.polarity(), MosPolarity::Pmos);
        assert_eq!(param(&pmos, "mueph1")?, &ParameterValue::Real(9e3));
        if variant == ModelVariant::HiSimHv2 {
            assert_eq!(param(&pmos, "vgsmin")?, &ParameterValue::Real(100.0));
        }
        let err = prepare(
            variant,
            &[
                ("nmos", ParameterValue::Flag(true)),
                ("pmos", ParameterValue::Flag(true)),
            ],
        );
        assert_eq!(err, Err(ModelError::ConflictingPolarity));
        assert_eq!(
            prepare(
                variant,
                &[("version", ParameterValue::Text("3.0".to_owned()))]
            ),
            Err(ModelError::UnknownHiSimHvVersion("3.0".to_owned()))
        );
    }
    for variant in [ModelVariant::HiSimHv1, ModelVariant::HiSimHv2] {
        assert_eq!(
            prepare(variant, &[("tox", ParameterValue::Real(0.0))]),
            Err(ModelError::InvalidPhysicalParameter("tox"))
        );
        assert_eq!(
            prepare(variant, &[("level", ParameterValue::Integer(68))]),
            Err(ModelError::UnknownLevel(68))
        );
    }
    assert_eq!(
        prepare(
            ModelVariant::HiSimHv1,
            &[("xqy", ParameterValue::Real(0.5e-9))]
        ),
        Err(ModelError::InvalidPhysicalParameter("xqy"))
    );
    for name in ["codep", "coddlt", "corbnet"] {
        for val in [-1, 2] {
            assert_eq!(
                prepare(
                    ModelVariant::HiSimHv2,
                    &[(name, ParameterValue::Integer(val))]
                ),
                Err(ModelError::InvalidSelector {
                    name: name.to_owned(),
                    value: val
                })
            );
        }
    }
    let soi = BoundParameters::bind(
        ModelVariant::BsimSoi,
        ParameterScope::Model,
        [("tox", ParameterValue::Real(5e-9))],
    )?;
    assert_eq!(
        prepare_hisim_hv_model(&soi),
        Err(ModelError::WrongModelScope)
    );
    Ok(())
}

#[test]
fn copy_dependencies_and_trials_are_independent() -> Result<(), ModelError> {
    for variant in [ModelVariant::HiSimHv1, ModelVariant::HiSimHv2] {
        let source = BoundParameters::bind(
            variant,
            ParameterScope::Model,
            [("xwd", ParameterValue::Real(1e-10))],
        )?;
        let base = prepare_hisim_hv_model(&source)?;
        assert_eq!(param(&base, "xwdc")?, &ParameterValue::Real(1e-10));
        for n in 0..64 {
            let trial = prepare(
                variant,
                &[
                    (
                        "js0",
                        ParameterValue::Real(f64::from(n).mul_add(1e-11, 1e-10)),
                    ),
                    ("cosym", ParameterValue::Integer(n % 2)),
                ],
            )?;
            assert_eq!(param(&trial, "xwdc")?, param(&trial, "xwd")?);
            assert_eq!(param(&base, "xwdc")?, &ParameterValue::Real(1e-10));
        }
        let xwd_slot = variant
            .parameter(ParameterScope::Model, "xwd")
            .ok_or_else(|| ModelError::UnknownParameter("xwd".to_owned()))?
            .slot;
        assert_eq!(
            source.get_slot(xwd_slot),
            Some(&ParameterValue::Real(1e-10))
        );
    }
    Ok(())
}
