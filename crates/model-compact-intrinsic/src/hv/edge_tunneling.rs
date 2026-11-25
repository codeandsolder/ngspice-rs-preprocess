//! HiSIM-HV1/HV2 physical gate-to-source and gate-to-drain edge tunneling
//! (`Igs`/`Igd`) from the exact `hsmhv[2]eval.c` gate-leakage branch.
//!
//! These are independent direct gate-to-S/D currents, not the additional
//! charge-inversion tunneling `Igate`, gate-to-body `Igb`, or FN terms.

use std::sync::OnceLock;

use super::{
    PreparedHiSimHvInstance, PreparedHiSimHvModel, compiled_hv_by_family, instance_real_at,
    lookup_slot, source_real_at,
};
use crate::{ModelError, ModelVariant, ParameterScope, ParameterValue};

const METER_TO_CENTIMETER: f64 = 100.0;
const SOURCE_EDGE_WIDTH_SCALE: f64 = 1e6;

/// One native direct edge-tunneling current, multiplied by instance `M`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvEdgeTunnelingCurrent {
    pub current_a: f64,
    /// Native dI/dVgs, i.e. derivative in the *internal* Vgs coordinate.
    pub gate_voltage_derivative_siemens: f64,
    /// Native dI/dVds; zero for the source side, negative of gate derivative
    /// for the drain side. Native dI/dVbs and local dI/dT are both exactly 0.
    pub drain_voltage_derivative_siemens: f64,
}

/// Complete independent `Igs`/`Igd` direct edge-tunneling contributions.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvEdgeTunnelingCurrents {
    pub gate_source: HiSimHvEdgeTunnelingCurrent,
    pub gate_drain: HiSimHvEdgeTunnelingCurrent,
}

/// Physical edge-tunneling coefficients, prepared for a fixed model/geometry.
///
/// The coefficients preserve the **original C** centimeter-converted
/// binned `GLKSD1/GLKSD2`, model-MKS `GLKSD3`, oxide thickness and effective
/// total width. Nothing is re-binned or resolved in the trial-voltage path.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreparedHiSimHvEdgeTunneling {
    amplitude_a_per_v2: f64,
    linear_exponent_per_v: f64,
    constant_exponent: f64,
    enabled: bool,
}

impl PreparedHiSimHvEdgeTunneling {
    fn side_current(
        self,
        voltage: f64,
        drain: bool,
    ) -> Result<HiSimHvEdgeTunnelingCurrent, ModelError> {
        if !voltage.is_finite() {
            return Err(ModelError::InvalidPhysicalParameter(
                "HiSIM-HV edge-tunneling bias",
            ));
        }
        if !self.enabled {
            return Ok(HiSimHvEdgeTunnelingCurrent {
                current_a: 0.0,
                gate_voltage_derivative_siemens: 0.0,
                drain_voltage_derivative_siemens: 0.0,
            });
        }
        let exponential = self
            .linear_exponent_per_v
            .mul_add(voltage, self.constant_exponent)
            .exp();
        let current = self.amplitude_a_per_v2 * exponential * voltage * voltage;
        let derivative = self.amplitude_a_per_v2
            * exponential
            * voltage
            * self.linear_exponent_per_v.mul_add(voltage, 2.0);
        // Native C applies a sign reversal at zero and on the positive side;
        // `Igs/Igd` are *not* the total partitioned gate currents.
        let sign = if voltage >= 0.0 { -1.0 } else { 1.0 };
        let value = sign * current;
        let gate_derivative = sign * derivative;
        let drain_derivative = if drain { -gate_derivative } else { 0.0 };
        if !value.is_finite() || !gate_derivative.is_finite() || !drain_derivative.is_finite() {
            return Err(ModelError::InvalidPhysicalParameter(
                "HiSIM-HV edge-tunneling current",
            ));
        }
        Ok(HiSimHvEdgeTunnelingCurrent {
            current_a: value,
            gate_voltage_derivative_siemens: gate_derivative,
            drain_voltage_derivative_siemens: drain_derivative,
        })
    }

    /// Evaluate original internal `Igs(Vgs)` and `Igd(Vgs-Vds)` and Jacobians.
    ///
    /// # Errors
    /// Rejects nonfinite source voltages or exponential overflow.
    pub fn evaluate(
        &self,
        internal_gate_source_v: f64,
        internal_drain_source_v: f64,
    ) -> Result<HiSimHvEdgeTunnelingCurrents, ModelError> {
        if !internal_gate_source_v.is_finite() || !internal_drain_source_v.is_finite() {
            return Err(ModelError::InvalidPhysicalParameter(
                "HiSIM-HV edge-tunneling terminal voltage",
            ));
        }
        Ok(HiSimHvEdgeTunnelingCurrents {
            gate_source: self.side_current(internal_gate_source_v, false)?,
            gate_drain: self
                .side_current(internal_gate_source_v - internal_drain_source_v, true)?,
        })
    }
}

#[derive(Clone, Copy)]
struct EdgeSlots {
    glksd1: u16,
    glksd2: u16,
    glksd3: u16,
    tox: u16,
    coiigs: u16,
    multiplier: u16,
}
static HV1_EDGE_SLOTS: OnceLock<Result<EdgeSlots, ModelError>> = OnceLock::new();
static HV2_EDGE_SLOTS: OnceLock<Result<EdgeSlots, ModelError>> = OnceLock::new();

fn compile_slots(variant: ModelVariant) -> Result<EdgeSlots, ModelError> {
    let multiplier = variant
        .parameter(ParameterScope::Instance, "m")
        .ok_or_else(|| ModelError::UnknownParameter("m".to_owned()))?
        .slot;
    Ok(EdgeSlots {
        glksd1: lookup_slot(variant, "glksd1")?,
        glksd2: lookup_slot(variant, "glksd2")?,
        glksd3: lookup_slot(variant, "glksd3")?,
        tox: lookup_slot(variant, "tox")?,
        coiigs: lookup_slot(variant, "coiigs")?,
        multiplier,
    })
}

/// Prepare source-defined direct gate/source and gate/drain edge leakage.
///
/// Original C converts `pParam.GLKSD1` by `/100`, `pParam.GLKSD2` and
/// `modelMKS.GLKSD3` by `*100`; the original equations then use
/// `T4=(pParam.GLKSD1/1e6)*Weff*NF`, `exp(TOX*(GLKSD3-GLKSD2*V))`
/// and `V²/TOX²` with a sign reversal for V >= 0. The original
/// `COIIGS` selector gates both contributions. All temperature/body
/// derivatives of these two source-defined terms are exactly zero.
///
/// # Errors
/// Rejects model/instance provenance mismatches or nonphysical expressions.
pub fn prepare_hisim_hv_edge_tunneling(
    model: &PreparedHiSimHvModel,
    instance: &PreparedHiSimHvInstance,
) -> Result<PreparedHiSimHvEdgeTunneling, ModelError> {
    model.validate_instance(instance)?;
    let slots = compiled_hv_by_family(
        model.variant(),
        &HV1_EDGE_SLOTS,
        &HV2_EDGE_SLOTS,
        compile_slots,
    )?;
    let enabled = match model.get_slot(slots.coiigs) {
        Some(ParameterValue::Integer(value)) => *value != 0,
        _ => return Err(ModelError::MissingDefaultDependency(slots.coiigs)),
    };
    let tox = source_real_at(model, slots.tox)?;
    if !tox.is_finite() || tox <= 0.0 {
        return Err(ModelError::InvalidPhysicalParameter(
            "HiSIM-HV edge-tunneling TOX",
        ));
    }
    let binned = |slot| {
        instance
            .binned_model_slot(slot)
            .ok_or(ModelError::MissingDefaultDependency(slot))
    };
    let amplitude = binned(slots.glksd1)? / METER_TO_CENTIMETER / SOURCE_EDGE_WIDTH_SCALE
        * instance.effective_geometry().channel_width_total_m
        * instance_real_at(instance, slots.multiplier)?
        / tox
        / tox;
    let slope = -tox * (binned(slots.glksd2)? * METER_TO_CENTIMETER);
    let intercept = tox * (source_real_at(model, slots.glksd3)? * METER_TO_CENTIMETER);
    if !amplitude.is_finite() || !slope.is_finite() || !intercept.is_finite() {
        return Err(ModelError::InvalidPhysicalParameter(
            "HiSIM-HV edge-tunneling coefficient",
        ));
    }
    Ok(PreparedHiSimHvEdgeTunneling {
        amplitude_a_per_v2: amplitude,
        linear_exponent_per_v: slope,
        constant_exponent: intercept,
        enabled,
    })
}
