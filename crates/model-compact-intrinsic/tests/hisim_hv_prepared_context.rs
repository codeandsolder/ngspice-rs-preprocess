//! End-to-end provenance of model-derived binning, instance geometry, and
//! temperature-dependent physics state. Same-family model mixing is invalid.

use model_compact_intrinsic::{
    BoundParameters, ModelError, ModelVariant, ParameterScope, ParameterValue,
    PreparedHiSimHvInstance, PreparedHiSimHvModel, PreparedHiSimHvTransport,
    prepare_hisim_hv_fringing, prepare_hisim_hv_instance, prepare_hisim_hv_junction_charges,
    prepare_hisim_hv_junction_diodes, prepare_hisim_hv_lateral_field_charge,
    prepare_hisim_hv_model, prepare_hisim_hv_temperature, prepare_hisim_hv_transport,
};

fn model(variant: ModelVariant, xqy1: f64) -> Result<PreparedHiSimHvModel, ModelError> {
    prepare_hisim_hv_model(&BoundParameters::bind(
        variant,
        ParameterScope::Model,
        [("xqy1", ParameterValue::Real(xqy1))],
    )?)
}

fn instance(
    model: &PreparedHiSimHvModel,
    width: f64,
) -> Result<PreparedHiSimHvInstance, ModelError> {
    prepare_hisim_hv_instance(
        model,
        &BoundParameters::bind(
            model.variant(),
            ParameterScope::Instance,
            [
                ("w", ParameterValue::Real(width)),
                ("l", ParameterValue::Real(3e-6)),
                ("as", ParameterValue::Real(2e-11)),
                ("ad", ParameterValue::Real(3e-11)),
                ("ps", ParameterValue::Real(4e-6)),
                ("pd", ParameterValue::Real(7e-6)),
            ],
        )?,
    )
}

fn thermal(
    model: &PreparedHiSimHvModel,
    instance: &PreparedHiSimHvInstance,
) -> Result<PreparedHiSimHvTransport, ModelError> {
    prepare_hisim_hv_transport(model, instance, 321.0, 0.0)
}

#[test]
fn same_family_different_model_card_cannot_reuse_instance_derived_parameters()
-> Result<(), ModelError> {
    for variant in [ModelVariant::HiSimHv1, ModelVariant::HiSimHv2] {
        let model_a = model(variant, 1e-12)?;
        let model_b = model(variant, 2e-12)?;
        let instance_a = instance(&model_a, 12e-6)?;
        let transport_a = thermal(&model_a, &instance_a)?;
        assert_eq!(
            prepare_hisim_hv_temperature(&model_b, &instance_a, 321.0, 0.0),
            Err(ModelError::MismatchedPreparedContext)
        );
        assert_eq!(
            prepare_hisim_hv_transport(&model_b, &instance_a, 321.0, 0.0),
            Err(ModelError::MismatchedPreparedContext)
        );
        assert_eq!(
            prepare_hisim_hv_fringing(&model_b, &instance_a),
            Err(ModelError::MismatchedPreparedContext)
        );
        assert_eq!(
            prepare_hisim_hv_lateral_field_charge(&model_b, &instance_a),
            Err(ModelError::MismatchedPreparedContext)
        );
        assert_eq!(
            prepare_hisim_hv_junction_charges(&model_b, &instance_a, &transport_a),
            Err(ModelError::MismatchedPreparedContext)
        );
        assert_eq!(
            prepare_hisim_hv_junction_diodes(&model_b, &instance_a, &transport_a),
            Err(ModelError::MismatchedPreparedContext)
        );
    }
    Ok(())
}

#[test]
fn prepared_transport_belongs_to_one_specific_instance_even_with_same_model()
-> Result<(), ModelError> {
    for variant in [ModelVariant::HiSimHv1, ModelVariant::HiSimHv2] {
        let mdl = model(variant, 1e-12)?;
        let a = instance(&mdl, 11e-6)?;
        let b = instance(&mdl, 19e-6)?;
        let ta = thermal(&mdl, &a)?;
        let tb = thermal(&mdl, &b)?;
        for (device, state) in [(&a, &tb), (&b, &ta)] {
            assert_eq!(
                prepare_hisim_hv_junction_charges(&mdl, device, state),
                Err(ModelError::MismatchedPreparedContext)
            );
            assert_eq!(
                prepare_hisim_hv_junction_diodes(&mdl, device, state),
                Err(ModelError::MismatchedPreparedContext)
            );
        }
    }
    Ok(())
}

#[test]
fn clones_preserve_exact_model_and_instance_provenance() -> Result<(), ModelError> {
    for variant in [ModelVariant::HiSimHv1, ModelVariant::HiSimHv2] {
        let mdl = model(variant, 1e-12)?;
        let inst = instance(&mdl, 14e-6)?;
        let temp = thermal(&mdl, &inst)?;
        let model_clone = mdl.clone();
        let instance_clone = inst.clone();
        let transport_clone = temp.clone();
        assert_eq!(mdl, model_clone);
        assert_eq!(inst, instance_clone);
        assert_eq!(temp, transport_clone);
        assert!(prepare_hisim_hv_fringing(&model_clone, &instance_clone).is_ok());
        assert!(prepare_hisim_hv_lateral_field_charge(&model_clone, &instance_clone).is_ok());
        assert!(
            prepare_hisim_hv_junction_charges(&model_clone, &instance_clone, &transport_clone,)
                .is_ok()
        );
        assert!(
            prepare_hisim_hv_junction_diodes(&model_clone, &instance_clone, &transport_clone,)
                .is_ok()
        );
    }
    Ok(())
}

#[test]
fn equal_valued_but_independently_prepared_models_are_not_cross_wirable() -> Result<(), ModelError>
{
    for variant in [ModelVariant::HiSimHv1, ModelVariant::HiSimHv2] {
        let a = model(variant, 0.0)?;
        let b = model(variant, 0.0)?;
        assert_eq!(
            a, b,
            "value equality should remain independent of provenance"
        );
        let inst = instance(&a, 18e-6)?;
        assert_eq!(
            prepare_hisim_hv_temperature(&b, &inst, 320.0, 0.0),
            Err(ModelError::MismatchedPreparedContext)
        );
    }
    Ok(())
}
