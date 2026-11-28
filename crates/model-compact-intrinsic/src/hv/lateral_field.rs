//! Native HiSIM-HV lateral-field gate-drain charge, including the additive body term.

use std::sync::OnceLock;

use super::{
    AdditiveChargeSlots, HiSimHvChannelElectrostaticPoint, HiSimHvChannelRegion,
    HiSimHvIntrinsicBias, HiSimHvIntrinsicScalar as Jet, PreparedHiSimHvInstance,
    PreparedHiSimHvModel, PreparedHiSimHvTemperature, compile_additive_charge_slots,
    compiled_hv_by_family, instance::model_real, instance_real_at, lookup_slot, source_real_at,
};
use crate::{ModelError, ModelVariant, ParameterValue};

const METER_TO_MICROMETER: f64 = 1e6;

/// Original static geometry-dependent `here->cqyb0` coefficient, including
/// native `COADOV` output gating and instance `M` on the final charge rows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreparedHiSimHvLateralFieldCharge {
    /// Source `cqyb0` before `COADOV` gating and instance `M`.
    pub raw_body_capacitance_f: f64,
    /// Signed gate/body contribution, `M*cqyb0` when `COADOV==1`.
    pub effective_body_capacitance_f: f64,
    pub output_enabled: bool,
    field_coefficient: f64,
}

/// Complete terminal contribution of the additive `XQY1/XQY2` term.
/// Gate and drain charges cancel, and the source and bulk rows are unchanged.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvLateralFieldCharge {
    pub gate_charge_c: f64,
    pub drain_charge_c: f64,
    /// `dQg/dVbs`, also the negative of `dQd/dVbs`.
    pub gate_body_derivative_f: f64,
}

impl PreparedHiSimHvLateralFieldCharge {
    /// Total native Qy on the gate, with equal and opposite charge on the drain.
    /// # Errors
    /// Invalid local electrostatic state or nonfinite charge.
    pub fn evaluate_channel_charge(
        &self,
        bias: HiSimHvIntrinsicBias,
        channel: &HiSimHvChannelElectrostaticPoint,
        temperature: &PreparedHiSimHvTemperature,
    ) -> Result<Jet, ModelError> {
        let body = Jet::coordinate(bias.vbs, 2, 1.);
        let additive = self.effective_body_capacitance_f * body;
        if self.field_coefficient == 0. {
            return additive.checked("HiSIM-HV lateral body charge");
        }
        let charge = &channel.charge;
        let drop = if matches!(
            charge.region,
            HiSimHvChannelRegion::Transition | HiSimHvChannelRegion::StrongInversion
        ) && charge.drift_diffusion_integral.value >= 1e-15
        {
            let beta = Jet::coordinate(
                temperature.inverse_thermal_voltage,
                3,
                temperature.inverse_thermal_voltage_derivative,
            );
            charge.drift_diffusion_integral / (beta * charge.source_inversion_density)
        } else {
            Jet::constant(0.)
        };
        (additive
            - self.field_coefficient
                * channel.material.bulk_depletion_width_factor
                * (Jet::coordinate(bias.vds, 1, 1.) - drop))
            .checked("HiSIM-HV lateral field charge")
    }

    /// Evaluate the original additive `Qy += cqyb0*Vbs` term.
    ///
    /// `Vbs` is the **internal** source-referenced body bias from the original
    /// channel evaluator, not the external substrate junction voltage.
    ///
    /// # Errors
    /// Rejects nonfinite trial bias and resulting charge.
    pub fn evaluate(
        &self,
        internal_bulk_source_v: f64,
    ) -> Result<HiSimHvLateralFieldCharge, ModelError> {
        if !internal_bulk_source_v.is_finite() {
            return Err(ModelError::InvalidPhysicalParameter(
                "HiSIM-HV lateral-field body bias",
            ));
        }
        let charge = self.effective_body_capacitance_f * internal_bulk_source_v;
        if !charge.is_finite() {
            return Err(ModelError::InvalidPhysicalParameter(
                "HiSIM-HV lateral-field charge",
            ));
        }
        Ok(HiSimHvLateralFieldCharge {
            gate_charge_c: charge,
            drain_charge_c: -charge,
            gate_body_derivative_f: self.effective_body_capacitance_f,
        })
    }
}

static HV1_LATERAL_SLOTS: OnceLock<Result<AdditiveChargeSlots, ModelError>> = OnceLock::new();
static HV2_LATERAL_SLOTS: OnceLock<Result<AdditiveChargeSlots, ModelError>> = OnceLock::new();

/// Prepare the complete native lateral-field and additive body Q terms.
///
/// Original C geometry is in **micrometers**: `LG=(L+XL)*1e6`, and
/// `cqyb0=1e6*Weff*NF*XQY1/pow(LG,XQY2)`. It is added to original `Qy`
/// only if `XQY1 != 0`. `COADOV==1` adds `M*Qy` to the gate and `-M*Qy`
/// to the drain. Trial evaluation has no parameter lookup or power function.
///
/// # Errors
/// Invalid physical geometry, inconsistent model family, or nonfinite result.
pub fn prepare_hisim_hv_lateral_field_charge(
    model: &PreparedHiSimHvModel,
    instance: &PreparedHiSimHvInstance,
) -> Result<PreparedHiSimHvLateralFieldCharge, ModelError> {
    model.validate_instance(instance)?;
    let slots = compiled_hv_by_family(
        model.variant(),
        &HV1_LATERAL_SLOTS,
        &HV2_LATERAL_SLOTS,
        |variant| compile_additive_charge_slots(variant, "xqy1", "xqy2"),
    )?;
    let length_um = instance.geometry().gate_length_m * METER_TO_MICROMETER;
    if !length_um.is_finite() || length_um <= 0.0 {
        return Err(ModelError::InvalidPhysicalParameter(
            "HiSIM-HV lateral-field gate length",
        ));
    }
    let xqy1 = source_real_at(model, slots.model_first)?;
    let xqy2 = source_real_at(model, slots.model_second)?;
    let raw = METER_TO_MICROMETER * instance.effective_geometry().channel_width_total_m * xqy1
        / length_um.powf(xqy2);
    let output_enabled = matches!(
        model.get_slot(slots.coadov),
        Some(ParameterValue::Integer(1))
    );
    let multiplier = instance_real_at(instance, slots.multiplier)?;
    let effective = if output_enabled && xqy1 != 0.0 {
        raw * multiplier
    } else {
        0.0
    };
    if !raw.is_finite() || !effective.is_finite() {
        return Err(ModelError::InvalidPhysicalParameter(
            "HiSIM-HV lateral-field capacitance",
        ));
    }
    let xqy = model_real(model, "xqy")?;
    let xqy = if model.variant() == ModelVariant::HiSimHv1 && xqy > 0. {
        xqy.max(1e-9)
    } else {
        xqy
    };
    let clm_slot = lookup_slot(model.variant(), "clm1")?;
    let clm1 = instance
        .binned_model_slot(clm_slot)
        .ok_or(ModelError::MissingDefaultDependency(clm_slot))?;
    let field_coefficient = if output_enabled && xqy != 0. {
        1.3 * 1.034_943e-10
            * instance.effective_geometry().capacitance_width_total_m
            * multiplier
            * (1. - clm1)
            / xqy
    } else {
        0.
    };
    if !field_coefficient.is_finite() {
        return Err(ModelError::InvalidPhysicalParameter(
            "HiSIM-HV lateral field coefficient",
        ));
    }
    Ok(PreparedHiSimHvLateralFieldCharge {
        raw_body_capacitance_f: raw,
        effective_body_capacitance_f: effective,
        output_enabled,
        field_coefficient,
    })
}
