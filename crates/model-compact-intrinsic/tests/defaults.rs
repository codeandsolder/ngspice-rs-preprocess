use model_compact_intrinsic::{
    BoundParameters, ModelError, ModelVariant, MosPolarity, ParameterScope, ParameterValue,
    PreparedBsimSoiModel, prepare_bsim_soi_model,
};

fn raw_model(inputs: &[(&str, ParameterValue)]) -> Result<BoundParameters, ModelError> {
    BoundParameters::bind(
        ModelVariant::BsimSoi,
        ParameterScope::Model,
        inputs.iter().map(|(name, value)| (*name, value.clone())),
    )
}

fn get<'a>(prepared: &'a PreparedBsimSoiModel, name: &str) -> Option<&'a ParameterValue> {
    ModelVariant::BsimSoi
        .parameter(ParameterScope::Model, name)
        .and_then(|spec| prepared.get_slot(spec.slot))
}

#[test]
fn source_order_defaults_resolve_through_all_real_and_reference_rules() -> Result<(), ModelError> {
    let model = prepare_bsim_soi_model(&raw_model(&[])?, 300.15)?;
    assert_eq!(model.polarity(), MosPolarity::Nmos);
    assert_eq!(get(&model, "mobmod"), Some(&ParameterValue::Integer(1)));
    assert_eq!(get(&model, "capmod"), Some(&ParameterValue::Integer(2)));
    assert_eq!(get(&model, "vgstcvmod"), Some(&ParameterValue::Integer(1)));
    assert_eq!(get(&model, "soimod"), Some(&ParameterValue::Integer(0)));
    assert_eq!(get(&model, "fnoimod"), Some(&ParameterValue::Integer(1)));
    assert_eq!(get(&model, "tnoimod"), Some(&ParameterValue::Integer(0)));
    assert_eq!(get(&model, "tox"), Some(&ParameterValue::Real(100.0e-10)));
    assert_eq!(get(&model, "toxp"), get(&model, "tox"));
    assert_eq!(get(&model, "toxm"), get(&model, "tox"));
    assert_eq!(get(&model, "toxqm"), get(&model, "tox"));
    assert_eq!(get(&model, "vth0"), Some(&ParameterValue::Real(0.7)));
    assert_eq!(get(&model, "u0"), Some(&ParameterValue::Real(0.067)));
    assert_eq!(get(&model, "dsub"), get(&model, "drout"));
    assert_eq!(get(&model, "tnom"), Some(&ParameterValue::Real(300.15)));
    assert_eq!(get(&model, "lpe0"), Some(&ParameterValue::Real(1.74e-7)));
    assert_eq!(get(&model, "egidl"), Some(&ParameterValue::Real(1.2)));
    assert!(matches!(get(&model, "cf"), Some(ParameterValue::Real(value)) if *value > 0.0));
    assert_eq!(get(&model, "nmos"), None);
    Ok(())
}

#[test]
fn polarity_and_mobility_dependent_defaults_follow_upstream() -> Result<(), ModelError> {
    let n = prepare_bsim_soi_model(&raw_model(&[])?, 300.15)?;
    let p = prepare_bsim_soi_model(&raw_model(&[("pmos", ParameterValue::Flag(true))])?, 300.15)?;
    let mode3 = prepare_bsim_soi_model(
        &raw_model(&[("mobmod", ParameterValue::Integer(3))])?,
        300.15,
    )?;
    assert_eq!(p.polarity(), MosPolarity::Pmos);
    assert_eq!(get(&p, "vth0"), Some(&ParameterValue::Real(-0.7)));
    assert_eq!(get(&p, "u0"), Some(&ParameterValue::Real(0.025)));
    assert_eq!(get(&p, "vddeot"), Some(&ParameterValue::Real(-1.5)));
    assert_eq!(get(&n, "vddeot"), Some(&ParameterValue::Real(1.5)));
    assert_eq!(get(&n, "uc"), Some(&ParameterValue::Real(-0.0465e-9)));
    assert_eq!(get(&mode3, "uc"), Some(&ParameterValue::Real(-0.0465)));
    assert_eq!(get(&mode3, "uc1"), Some(&ParameterValue::Real(-0.056)));
    Ok(())
}

#[test]
fn dependent_defaults_use_overrides_and_legacy_parameters_only_when_given() -> Result<(), ModelError>
{
    let model = prepare_bsim_soi_model(
        &raw_model(&[
            ("tox", ParameterValue::Real(2.5e-9)),
            ("drout", ParameterValue::Real(0.91)),
            ("nlx", ParameterValue::Real(7.5e-8)),
            ("ngidl", ParameterValue::Real(4.25)),
            ("lnlx", ParameterValue::Real(1.0e-9)),
            ("wlpe0", ParameterValue::Real(2.0e-9)),
        ])?,
        280.0,
    )?;
    assert_eq!(get(&model, "toxp"), Some(&ParameterValue::Real(2.5e-9)));
    assert_eq!(get(&model, "toxm"), Some(&ParameterValue::Real(2.5e-9)));
    assert_eq!(get(&model, "toxqm"), Some(&ParameterValue::Real(2.5e-9)));
    assert_eq!(get(&model, "dsub"), Some(&ParameterValue::Real(0.91)));
    assert_eq!(get(&model, "lpe0"), Some(&ParameterValue::Real(7.5e-8)));
    assert_eq!(get(&model, "llpe0"), Some(&ParameterValue::Real(1.0e-9)));
    assert_eq!(get(&model, "wlpe0"), Some(&ParameterValue::Real(2.0e-9)));
    assert_eq!(get(&model, "egidl"), Some(&ParameterValue::Real(4.25)));
    let explicit = prepare_bsim_soi_model(
        &raw_model(&[
            ("nlx", ParameterValue::Real(7.5e-8)),
            ("lpe0", ParameterValue::Real(2.0e-7)),
            ("ngidl", ParameterValue::Real(4.25)),
            ("egidl", ParameterValue::Real(0.8)),
        ])?,
        300.0,
    )?;
    assert_eq!(get(&explicit, "lpe0"), Some(&ParameterValue::Real(2.0e-7)));
    assert_eq!(get(&explicit, "egidl"), Some(&ParameterValue::Real(0.8)));
    Ok(())
}

#[test]
fn model_tnom_uses_celsius_input_and_kelvin_nominal_default() -> Result<(), ModelError> {
    let untouched = raw_model(&[("tnom", ParameterValue::Real(27.0))])?;
    let a = prepare_bsim_soi_model(&untouched, 295.0)?;
    let b = prepare_bsim_soi_model(&untouched, 320.0)?;
    assert_eq!(get(&a, "tnom"), Some(&ParameterValue::Real(300.15)));
    assert_eq!(get(&b, "tnom"), Some(&ParameterValue::Real(300.15)));
    assert_eq!(
        untouched.get_slot(
            ModelVariant::BsimSoi
                .parameter(ParameterScope::Model, "tnom")
                .ok_or_else(|| ModelError::UnknownParameter("tnom".to_owned()))?
                .slot
        ),
        Some(&ParameterValue::Real(27.0))
    );
    let default = prepare_bsim_soi_model(&raw_model(&[])?, 320.0)?;
    assert_eq!(get(&default, "tnom"), Some(&ParameterValue::Real(320.0)));
    Ok(())
}

#[test]
fn selector_and_physical_constraints_fail_without_c_style_silent_coercion() -> Result<(), ModelError>
{
    for (name, bad, max) in [
        ("soimod", 4, 3),
        ("rgatemod", -1, 3),
        ("fnoimod", 2, 1),
        ("tnoimod", 3, 2),
        ("rdsmod", 2, 1),
        ("rbodymod", -1, 1),
    ] {
        for valid in 0..=max {
            prepare_bsim_soi_model(
                &raw_model(&[(name, ParameterValue::Integer(valid))])?,
                300.0,
            )?;
        }
        assert_eq!(
            prepare_bsim_soi_model(&raw_model(&[(name, ParameterValue::Integer(bad))])?, 300.0),
            Err(ModelError::InvalidSelector {
                name: name.to_owned(),
                value: bad
            })
        );
    }
    assert_eq!(
        prepare_bsim_soi_model(
            &raw_model(&[
                ("nmos", ParameterValue::Flag(true)),
                ("pmos", ParameterValue::Flag(true)),
            ])?,
            300.0
        ),
        Err(ModelError::ConflictingPolarity)
    );
    for temp in [0.0, -1.0, f64::NAN] {
        assert_eq!(
            prepare_bsim_soi_model(&raw_model(&[])?, temp),
            Err(ModelError::InvalidNominalTemperature)
        );
    }
    assert_eq!(
        prepare_bsim_soi_model(&raw_model(&[("tox", ParameterValue::Real(0.0))])?, 300.0),
        Err(ModelError::InvalidPhysicalParameter("tox"))
    );
    assert_eq!(
        prepare_bsim_soi_model(
            &BoundParameters::bind(
                ModelVariant::HiSim2,
                ParameterScope::Model,
                [("tox", ParameterValue::Real(1e-8))]
            )?,
            300.0
        ),
        Err(ModelError::WrongModelScope)
    );
    Ok(())
}

#[test]
fn independent_parameter_batches_do_not_contaminate_shared_defaults() -> Result<(), ModelError> {
    let base = raw_model(&[("soimod", ParameterValue::Integer(3))])?;
    let a = prepare_bsim_soi_model(&base, 300.0)?;
    for i in 0..128 {
        let b = prepare_bsim_soi_model(
            &raw_model(&[
                ("soimod", ParameterValue::Integer(i % 4)),
                (
                    "tox",
                    ParameterValue::Real(f64::from(i).mul_add(1e-11, 1e-9)),
                ),
            ])?,
            300.0,
        )?;
        assert_eq!(get(&b, "soimod"), Some(&ParameterValue::Integer(i % 4)));
        assert_eq!(get(&a, "soimod"), Some(&ParameterValue::Integer(3)));
        assert_eq!(
            base.get_slot(
                ModelVariant::BsimSoi
                    .parameter(ParameterScope::Model, "soimod")
                    .ok_or_else(|| ModelError::UnknownParameter("soimod".to_owned()))?
                    .slot
            ),
            Some(&ParameterValue::Integer(3))
        );
    }
    Ok(())
}
