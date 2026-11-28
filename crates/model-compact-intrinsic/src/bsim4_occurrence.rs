//! BSIM4 occurrence-local physical coefficient modifiers.
//!
//! Apply these operations after the model bin and its primary coefficients
//! have been resolved, before computing dependent channel coefficients and
//! before installing the series-resistance topology. See HSPICE MOSFET
//! Models Manual X-2005.09, pp. 443–444, Tables 111–112.

use crate::{ModelError, mos_resistance_at_temperature};

/// Resolved primary channel coefficients. Threshold voltage retains its
/// physical sign; mobility is in SI, regardless of the model-card units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bsim4PrimaryCoefficients {
    pub threshold_voltage_v: f64,
    pub mobility_m2_per_vs: f64,
    pub body_effect_sqrt_v: f64,
    pub subthreshold_factor: f64,
}

/// Occurrence-specific changes, independent of the model's size bin.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bsim4ChannelModifiers {
    pub threshold_shift_v: f64,
    pub mobility_multiplier: f64,
    pub body_effect_shift_sqrt_v: f64,
    pub subthreshold_factor_shift: f64,
}

impl Default for Bsim4ChannelModifiers {
    fn default() -> Self {
        Self {
            threshold_shift_v: 0.,
            mobility_multiplier: 1.,
            body_effect_shift_sqrt_v: 0.,
            subthreshold_factor_shift: 0.,
        }
    }
}

/// Documented resets, to be reported by the caller as source diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Bsim4ChannelModifierResets {
    pub mobility_multiplier: bool,
    pub body_effect_shift: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bsim4ModifiedChannel {
    pub coefficients: Bsim4PrimaryCoefficients,
    pub resets: Bsim4ChannelModifierResets,
}

fn finite(value: f64, parameter: &'static str) -> Result<f64, ModelError> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(ModelError::InvalidPhysicalParameter(parameter))
    }
}

impl Bsim4ChannelModifiers {
    /// Resolve the effective channel coefficients without changing the model
    /// bin shared by other occurrences. The documented negative-multiplier
    /// and excessive negative body-effect-shift cases reset to identity.
    /// Zero mobility and an exactly cancelling body-effect shift are not resets.
    ///
    /// # Errors
    /// Nonfinite input or nonrepresentable effective coefficient.
    pub fn apply(self, base: Bsim4PrimaryCoefficients) -> Result<Bsim4ModifiedChannel, ModelError> {
        let threshold = finite(self.threshold_shift_v, "BSIM4 threshold shift")?;
        let mobility = finite(self.mobility_multiplier, "BSIM4 mobility multiplier")?;
        let body = finite(self.body_effect_shift_sqrt_v, "BSIM4 body-effect shift")?;
        let subthreshold = finite(self.subthreshold_factor_shift, "BSIM4 subthreshold shift")?;
        let base_body = finite(base.body_effect_sqrt_v, "BSIM4 binned body effect")?;
        let resets = Bsim4ChannelModifierResets {
            mobility_multiplier: mobility < 0.,
            body_effect_shift: body < -base_body,
        };
        let mobility = if resets.mobility_multiplier {
            1.
        } else {
            mobility
        };
        let body = if resets.body_effect_shift { 0. } else { body };
        let coefficients = Bsim4PrimaryCoefficients {
            threshold_voltage_v: finite(
                base.threshold_voltage_v + threshold,
                "BSIM4 effective threshold",
            )?,
            mobility_m2_per_vs: finite(
                finite(base.mobility_m2_per_vs, "BSIM4 binned mobility")? * mobility,
                "BSIM4 effective mobility",
            )?,
            body_effect_sqrt_v: finite(base_body + body, "BSIM4 effective body effect")?,
            subthreshold_factor: finite(
                base.subthreshold_factor + subthreshold,
                "BSIM4 effective subthreshold factor",
            )?,
        };
        Ok(Bsim4ModifiedChannel {
            coefficients,
            resets,
        })
    }
}

/// Extrinsic resistance modifiers, in drain/source order.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Bsim4SeriesModifiers {
    /// Additional resistance of one contact, before division by finger count.
    pub contact_per_finger_ohm: [f64; 2],
    /// Linear temperature coefficient of the entire terminal resistance.
    pub temperature_per_k: [f64; 2],
}

/// Resolved series resistances of the full finger group, before parallel
/// instance multiplicity. Negative source contact values reset to zero.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bsim4ModifiedSeries {
    pub resistance_ohm: [f64; 2],
    pub reset_contact: [bool; 2],
}

impl Bsim4SeriesModifiers {
    /// Add each per-finger contact contribution to the already calculated
    /// nominal diffusion resistance, then temperature-correct the total.
    /// The intrinsic bias-dependent channel resistance is not included.
    ///
    /// # Errors
    /// Nonfinite or nonphysical finger count, temperatures, diffusion
    /// resistance, coefficients or effective resistance.
    pub fn prepare(
        self,
        nominal_diffusion_ohm: [f64; 2],
        finger_count: f64,
        nominal_temperature_k: f64,
        operating_temperature_k: f64,
    ) -> Result<Bsim4ModifiedSeries, ModelError> {
        if !finger_count.is_finite() || finger_count <= 0. {
            return Err(ModelError::InvalidPhysicalParameter("BSIM4 finger count"));
        }
        let mut resistance_ohm = [0.; 2];
        let mut reset_contact = [false; 2];
        for (index, diffusion) in nominal_diffusion_ohm.into_iter().enumerate() {
            if !diffusion.is_finite() || diffusion < 0. {
                return Err(ModelError::InvalidPhysicalParameter(
                    "BSIM4 nominal diffusion resistance",
                ));
            }
            let contact = finite(
                self.contact_per_finger_ohm[index],
                "BSIM4 contact resistance",
            )?;
            reset_contact[index] = contact < 0.;
            resistance_ohm[index] = mos_resistance_at_temperature(
                diffusion + contact.max(0.) / finger_count,
                self.temperature_per_k[index],
                nominal_temperature_k,
                operating_temperature_k,
            )?;
        }
        Ok(Bsim4ModifiedSeries {
            resistance_ohm,
            reset_contact,
        })
    }
}

/// All occurrence-local coefficient modifiers, bound before physical
/// preparation and independent of source spelling.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Bsim4OccurrenceModifiers {
    pub channel: Bsim4ChannelModifiers,
    pub series: Bsim4SeriesModifiers,
}
