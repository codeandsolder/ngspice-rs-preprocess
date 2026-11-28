//! HiSIM-HV CORSRD=2/3 nonlinear channel access-resistance correction.
//!
//! The actual constitutive law depends on external, not intrinsic, gate,
//! drain and bulk biases. Preserve those independent physical coordinates.
use super::{
    HiSimHvDrainCurrent, HiSimHvElectricalGradient, HiSimHvIntrinsicBias, HiSimHvIntrinsicScalar,
    PreparedHiSimHvInstance, PreparedHiSimHvModel,
    instance::model_real,
    instance_real_at, lookup_slot,
    smoothing::{lower, positive, upper},
};
use crate::{ModelError, ParameterScope, ParameterValue, differential::Differential};
type Jet = Differential<7>;
const SMALL: f64 = 1e-12;
const TRANSITION: f64 = 20.;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvExternalBias {
    pub vgs: f64,
    pub vds: f64,
    pub vbs: f64,
}

/// Local input to the native access correction, before terminal orientation.
/// Current inputs include multiplicity; external partials are independent.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvAccessInputs {
    pub intrinsic: HiSimHvIntrinsicBias,
    pub external: HiSimHvExternalBias,
    pub preaccess_current_a: HiSimHvIntrinsicScalar,
    pub postaccess_current_a: HiSimHvIntrinsicScalar,
    /// False for accumulation or the native no-inversion cutoff.
    pub conducting_channel: bool,
}

/// Effective unscaled access resistance and corrected normalized current.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvAccessCorrectionPoint {
    pub resistance_ohm: f64,
    pub resistance_derivative_ohm_per_v: HiSimHvElectricalGradient,
    pub current: HiSimHvDrainCurrent,
}

/// Bound source-defined channel access coefficients for one instance.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreparedHiSimHvAccessCorrection {
    enabled: bool,
    base_resistance_width: f64,
    gate_coefficient: f64,
    gate_reference: f64,
    gate_modulation: f64,
    drain_exponent: f64,
    body_coefficient: f64,
    body_exponent: f64,
    effective_width_m: f64,
    multiplicity: f64,
}

const fn promote(value: HiSimHvIntrinsicScalar) -> Jet {
    let [gate, drain, body, temperature] = value.gradient;
    Jet {
        value: value.value,
        gradient: [gate, drain, body, temperature, 0., 0., 0.],
    }
}
const fn electrical(value: Jet) -> HiSimHvElectricalGradient {
    let [
        gate,
        drain,
        body,
        _,
        gate_external,
        drain_external,
        body_external,
    ] = value.gradient;
    HiSimHvElectricalGradient {
        inner_vgs: gate,
        inner_vds: drain,
        inner_vbs: body,
        outer_vgs: gate_external,
        outer_vds: drain_external,
        outer_vbs: body_external,
    }
}

/// Prepare the native nonlinear access law, independent of the explicit
/// series-node and drift branches.
/// # Errors
/// Invalid model binding, width, multiplicity or coefficients.
pub fn prepare_hisim_hv_access_correction(
    model: &PreparedHiSimHvModel,
    instance: &PreparedHiSimHvInstance,
) -> Result<PreparedHiSimHvAccessCorrection, ModelError> {
    model.validate_instance(instance)?;
    let binned = |name| {
        let slot = lookup_slot(model.variant(), name)?;
        instance
            .binned_model_slot(slot)
            .ok_or(ModelError::MissingDefaultDependency(slot))
    };
    let slot = lookup_slot(model.variant(), "corsrd")?;
    let selector = match model.get_slot(slot) {
        Some(ParameterValue::Integer(value)) => *value,
        _ => return Err(ModelError::MissingDefaultDependency(slot)),
    };
    // Native temperature-prepared Rd/Rs are zero for nonpositive binned
    // values, otherwise smoothed above a strictly positive lower limit.
    let enabled = matches!(selector, 2 | 3) && (binned("rd")? > 0. || binned("rs")? > 0.);
    let geometry = instance.geometry();
    let lg = geometry.gate_length_m * 1e6;
    let wg = geometry.gate_width_per_finger_m * 1e6;
    let rd23 = binned("rd23")? / 100.;
    let base = if rd23 == 0. {
        0.
    } else {
        let area_factor =
            1. + model_real(model, "rd23s")? / (lg * wg).powf(model_real(model, "rd23sp")?);
        let length_factor = (-model_real(model, "rd23l")? * lg.powf(model_real(model, "rd23lp")?))
            .min(80.)
            .exp();
        positive(Jet::constant(rd23 * area_factor * length_factor), 1e-8).value
    };
    let m_slot = model
        .variant()
        .parameter(ParameterScope::Instance, "m")
        .ok_or_else(|| ModelError::UnknownParameter("m".to_owned()))?
        .slot;
    let result = PreparedHiSimHvAccessCorrection {
        enabled,
        base_resistance_width: base,
        gate_coefficient: binned("rd24")? / 100.,
        gate_reference: model_real(model, "rd25")?,
        gate_modulation: model_real(model, "rd20")?,
        drain_exponent: model_real(model, "rd21")?,
        body_coefficient: binned("rd22")? / 100.,
        body_exponent: model_real(model, "rd22d")?,
        effective_width_m: instance.effective_geometry().channel_width_total_m,
        multiplicity: instance_real_at(instance, m_slot)?,
    };
    if ![
        base,
        result.gate_coefficient,
        result.gate_reference,
        result.gate_modulation,
        result.drain_exponent,
        result.body_coefficient,
        result.body_exponent,
        result.effective_width_m,
        result.multiplicity,
    ]
    .into_iter()
    .all(f64::is_finite)
        || result.effective_width_m <= 0.
        || result.multiplicity <= 0.
    {
        return Err(ModelError::InvalidPhysicalParameter(
            "HiSIM-HV access coefficients",
        ));
    }
    Ok(result)
}

impl PreparedHiSimHvAccessCorrection {
    fn resistance(&self, external: HiSimHvExternalBias) -> Jet {
        let gate = Jet::coordinate(external.vgs, 4, 1.);
        let drain = if external.vds >= 0. {
            Jet::coordinate(external.vds, 5, 1.)
        } else {
            Jet::constant(0.)
        };
        let body = Jet::coordinate(external.vbs, 6, 1.);
        let base = Jet::constant(self.base_resistance_width);
        let gate_width = if self.gate_modulation > 0. {
            let limited = lower(
                self.gate_coefficient * (gate - self.gate_reference),
                base,
                1e-4,
            );
            upper(limited, (1. + self.gate_modulation) * base, 1e-4)
        } else {
            base
        };
        let exponent = self.drain_exponent;
        let drain_power = if drain.value < TRANSITION * SMALL {
            let common = (TRANSITION + 1.).powf(exponent - 1.);
            let alpha = common
                * (0.5 * exponent).mul_add(-TRANSITION, TRANSITION + 1.)
                * SMALL.powf(exponent);
            let beta = 0.5 * exponent * common / TRANSITION * SMALL.powf(exponent - 2.);
            alpha + beta * drain * drain
        } else {
            (drain + SMALL).positive_pow(exponent)
        };
        let body_power = (drain + SMALL).positive_pow(self.body_exponent);
        (gate_width * drain_power + body * self.body_coefficient * body_power)
            / self.effective_width_m
    }

    /// Apply the nonlinear access correction before adding the independent
    /// postaccess current terms. All seven Jacobian coordinates are physical.
    /// # Errors
    /// Nonfinite local state or singular current correction.
    pub fn evaluate(
        &self,
        inputs: &HiSimHvAccessInputs,
    ) -> Result<HiSimHvAccessCorrectionPoint, ModelError> {
        if inputs.intrinsic.vds < 0.
            || ![
                inputs.intrinsic.vds,
                inputs.intrinsic.vgs,
                inputs.intrinsic.vbs,
                inputs.external.vgs,
                inputs.external.vds,
                inputs.external.vbs,
            ]
            .into_iter()
            .all(f64::is_finite)
        {
            return Err(ModelError::InvalidPhysicalParameter("HiSIM-HV access bias"));
        }
        let current = promote(inputs.preaccess_current_a).checked("HiSIM-HV preaccess current")?;
        let postaccess =
            promote(inputs.postaccess_current_a).checked("HiSIM-HV postaccess current")?;
        let resistance = if self.enabled && inputs.conducting_channel {
            self.resistance(inputs.external)
        } else {
            Jet::constant(0.)
        };
        let drain = Jet::coordinate(inputs.intrinsic.vds, 1, 1.);
        // The native denominator uses per-unit current, whereas the returned
        // physical current includes M. Do not count M in the voltage drop.
        let corrected = (current
            / (1. + resistance * current / (self.multiplicity * (drain + SMALL)))
            + postaccess)
            .checked("HiSIM-HV access current")?;
        let resistance = resistance.checked("HiSIM-HV access resistance")?;
        Ok(HiSimHvAccessCorrectionPoint {
            resistance_ohm: resistance.value,
            resistance_derivative_ohm_per_v: electrical(resistance),
            current: HiSimHvDrainCurrent {
                current_a: corrected.value,
                electrical_derivative_a_per_v: electrical(corrected),
                temperature_derivative_a_per_kelvin: corrected.gradient[3],
            },
        })
    }
}
