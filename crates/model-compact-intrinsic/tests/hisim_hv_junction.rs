//! Original HiSIM-HV1/HV2 substrate junction depletion Q, dQ/dV and dQ/dT.

use model_compact_intrinsic::{
    BoundParameters, HiSimHvJunctionBias, ModelError, ModelVariant, ParameterScope, ParameterValue,
    PreparedHiSimHvInstance, PreparedHiSimHvModel, PreparedHiSimHvTransport,
    prepare_hisim_hv_instance, prepare_hisim_hv_junction_charges, prepare_hisim_hv_model,
    prepare_hisim_hv_transport,
};

fn prepared(
    family: ModelVariant,
    model_params: &[(&str, ParameterValue)],
    instance_params: &[(&str, ParameterValue)],
    rise: f64,
) -> Result<
    (
        PreparedHiSimHvModel,
        PreparedHiSimHvInstance,
        PreparedHiSimHvTransport,
    ),
    ModelError,
> {
    let model = prepare_hisim_hv_model(&BoundParameters::bind(
        family,
        ParameterScope::Model,
        model_params
            .iter()
            .map(|(name, value)| (*name, value.clone())),
    )?)?;
    let instance = prepare_hisim_hv_instance(
        &model,
        &BoundParameters::bind(
            family,
            ParameterScope::Instance,
            instance_params
                .iter()
                .map(|(name, value)| (*name, value.clone())),
        )?,
    )?;
    let temp = prepare_hisim_hv_transport(&model, &instance, 323.0, rise)?;
    Ok((model, instance, temp))
}

fn close(value: f64, expected: f64, rel: f64) {
    let delta = expected.abs().max(1e-20);
    assert!(
        (value - expected).abs() <= rel * delta,
        "got {value:.17e}, expected {expected:.17e}, rel {rel:.3e}"
    );
}

#[test]
fn zero_area_and_perimeter_have_zero_depletion_charge() -> Result<(), ModelError> {
    for family in [ModelVariant::HiSimHv1, ModelVariant::HiSimHv2] {
        let (model, instance, temp) = prepared(family, &[], &[], 0.0)?;
        let evaluated = prepare_hisim_hv_junction_charges(&model, &instance, &temp)?.evaluate(
            HiSimHvJunctionBias {
                bulk_source_v: -0.9,
                bulk_drain_v: 0.5,
            },
        )?;
        for value in [evaluated.bulk_source, evaluated.bulk_drain] {
            assert_eq!(value.charge_coulomb, 0.0);
            assert_eq!(value.capacitance_farad, 0.0);
            assert_eq!(value.charge_temperature_derivative_coulomb_per_kelvin, 0.0);
        }
    }
    Ok(())
}

#[test]
fn independent_bulk_source_and_drain_junctions_match_physical_derivatives() -> Result<(), ModelError>
{
    for family in [ModelVariant::HiSimHv1, ModelVariant::HiSimHv2] {
        let instance_fields = [
            ("as", ParameterValue::Real(1e-11)),
            ("ad", ParameterValue::Real(5e-12)),
            ("ps", ParameterValue::Real(10e-6)),
            ("pd", ParameterValue::Real(2e-6)),
            ("m", ParameterValue::Real(2.0)),
            ("coselfheat", ParameterValue::Integer(1)),
        ];
        let model_fields = [
            ("rth0", ParameterValue::Real(0.4)),
            ("tcjbs", ParameterValue::Real(0.004)),
            ("tcjbd", ParameterValue::Real(-0.002)),
            ("tcjbsswg", ParameterValue::Real(0.003)),
            ("tcjbdswg", ParameterValue::Real(-0.001)),
            ("tcjbssw", ParameterValue::Real(0.001)),
            ("tcjbdsw", ParameterValue::Real(0.002)),
        ];
        let (model, instance, temp) = prepared(family, &model_fields, &instance_fields, 12.0)?;
        let reference = HiSimHvJunctionBias {
            bulk_source_v: -0.7,
            bulk_drain_v: 0.15,
        };
        let ready = prepare_hisim_hv_junction_charges(&model, &instance, &temp)?;
        let evaluated = ready.evaluate(reference)?;
        let step = 1e-5;
        for (source, value) in [(true, evaluated.bulk_source), (false, evaluated.bulk_drain)] {
            let mut low = reference;
            let mut high = reference;
            if source {
                low.bulk_source_v -= step;
                high.bulk_source_v += step;
            } else {
                low.bulk_drain_v -= step;
                high.bulk_drain_v += step;
            }
            let left = ready.evaluate(low)?;
            let right = ready.evaluate(high)?;
            let diff = if source {
                (right.bulk_source.charge_coulomb - left.bulk_source.charge_coulomb) / (2.0 * step)
            } else {
                (right.bulk_drain.charge_coulomb - left.bulk_drain.charge_coulomb) / (2.0 * step)
            };
            close(value.capacitance_farad, diff, 2e-8);
            let (m1, i1, t1) = prepared(family, &model_fields, &instance_fields, 12.0 - step)?;
            let (m2, i2, t2) = prepared(family, &model_fields, &instance_fields, 12.0 + step)?;
            let low_t = prepare_hisim_hv_junction_charges(&m1, &i1, &t1)?.evaluate(reference)?;
            let high_t = prepare_hisim_hv_junction_charges(&m2, &i2, &t2)?.evaluate(reference)?;
            let dtemp = if source {
                (high_t.bulk_source.charge_coulomb - low_t.bulk_source.charge_coulomb)
                    / (2.0 * step)
            } else {
                (high_t.bulk_drain.charge_coulomb - low_t.bulk_drain.charge_coulomb) / (2.0 * step)
            };
            close(
                value.charge_temperature_derivative_coulomb_per_kelvin,
                dtemp,
                3e-8,
            );
        }
    }
    Ok(())
}

#[test]
fn prepared_junction_charge_state_is_immutable_over_bias_trials() -> Result<(), ModelError> {
    for family in [ModelVariant::HiSimHv1, ModelVariant::HiSimHv2] {
        let (model, instance, temperature) = prepared(
            family,
            &[],
            &[
                ("as", ParameterValue::Real(2e-11)),
                ("ad", ParameterValue::Real(4e-11)),
                ("ps", ParameterValue::Real(6e-6)),
                ("pd", ParameterValue::Real(11e-6)),
            ],
            0.0,
        )?;
        let ready = prepare_hisim_hv_junction_charges(&model, &instance, &temperature)?;
        let original = ready;
        for index in 0u16..256 {
            let voltage = f64::from(index).mul_add(0.004, -0.5);
            let bias = HiSimHvJunctionBias {
                bulk_source_v: voltage,
                bulk_drain_v: -voltage,
            };
            let charge = ready.evaluate(bias)?;
            assert!(charge.bulk_source.charge_coulomb.is_finite());
            assert!(charge.bulk_drain.charge_coulomb.is_finite());
        }
        assert_eq!(ready, original);
        assert!(
            ready
                .evaluate(HiSimHvJunctionBias {
                    bulk_source_v: f64::NAN,
                    bulk_drain_v: 0.0,
                })
                .is_err()
        );
    }
    Ok(())
}
