//! Independent body-bias-induced charge contribution to the HiSIM-HV1/HV2
//! lateral-field channel-charge branch, from original `hsmhv[2]temp.c`
//! and the `Qy`/terminal-charge updates in original `hsmhv[2]eval.c`.
//!
//! This is *only* the complete, additive `XQY1/XQY2` charge term. The
//! separate bias-dependent base `Qy` and channel Ids/Q remain distinct.

use std::sync::OnceLock;

use super::{
    AdditiveChargeSlots, PreparedHiSimHvInstance, PreparedHiSimHvModel,
    compile_additive_charge_slots, compiled_hv_by_family, instance_real_at, source_real_at,
};
use crate::{ModelError, ParameterValue};

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

/// Prepare the complete independently additive body/lateral-field Q term.
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
    Ok(PreparedHiSimHvLateralFieldCharge {
        raw_body_capacitance_f: raw,
        effective_body_capacitance_f: effective,
        output_enabled,
    })
}
