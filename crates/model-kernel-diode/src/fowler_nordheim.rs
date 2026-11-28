//! Independently measured `LTspice` `LEVEL=2` Fowler–Nordheim tunnelling law.
//!
//! The kernel operates on terminal voltage, not per-junction voltage.
//! It owns no Newton history, integration, or circuit stamping.

use crate::{ModelError, finite};

/// Historical coefficient measured in the `LTspice` oxide capacitance law.
pub const OXIDE_CAPACITANCE_COEFFICIENT: f64 = 0.345_313_3;
/// Historical electron charge used by this model, coulombs.
pub const SHOT_ELEMENTARY_CHARGE: f64 = 1.602_176_462e-19;
const CURRENT_COEFFICIENT: f64 = 1e10;

/// Resolved source model. Geometry is in the source model's own units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Model {
    /// Oxide thickness `Tox`; negative nonzero values are signed vendor inputs.
    pub oxide_thickness: f64,
    /// Default length L.
    pub length: f64,
    /// Default width W.
    pub width: f64,
}

impl Default for Model {
    fn default() -> Self {
        Self {
            oxide_thickness: 100.,
            length: 0.,
            width: 0.,
        }
    }
}

/// Occurrence geometry and parallel/series multiplicities.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Instance {
    /// Override model length, including an explicit zero.
    pub length: Option<f64>,
    /// Override model width, including an explicit zero.
    pub width: Option<f64>,
    /// Signed parallel multiplicity M.
    pub multiplier: f64,
    /// Signed nonzero series multiplicity N.
    pub series: f64,
}

impl Default for Instance {
    fn default() -> Self {
        Self {
            length: None,
            width: None,
            multiplier: 1.,
            series: 1.,
        }
    }
}

/// Terminal current/charge and their exact first voltage derivatives.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Evaluation {
    /// Signed conduction current, A.
    pub current: f64,
    /// Differential terminal conductance, S.
    pub conductance: f64,
    /// Signed terminal charge, C, relative to zero voltage.
    pub charge: f64,
    /// Differential terminal capacitance, F.
    pub capacitance: f64,
}

/// Immutable occurrence-bound physical law.
#[derive(Clone, Copy, Debug)]
pub struct PreparedInstance {
    parallel: [f64; 3],
    thickness: f64,
    series: f64,
    capacitance: f64,
}

impl Model {
    pub(super) fn validate(self) -> Result<(), ModelError> {
        finite(
            "F-N diode model",
            &[self.oxide_thickness, self.length, self.width],
        )?;
        if self.oxide_thickness == 0. {
            return Err(ModelError::Invalid("F-N diode zero Tox"));
        }
        Ok(())
    }

    /// Resolve model and instance geometry before physical evaluation.
    ///
    /// Both resolved dimensions must be nonzero to specify an area; otherwise
    /// the area defaults to one. An instance override is independent per field.
    ///
    /// # Errors
    /// Nonfinite inputs, zero thickness/series count or nonfinite terminal
    /// capacitance are invalid.
    pub fn bind(self, instance: Instance) -> Result<PreparedInstance, ModelError> {
        self.validate()?;
        let length = instance.length.unwrap_or(self.length);
        let width = instance.width.unwrap_or(self.width);
        finite(
            "F-N diode instance",
            &[length, width, instance.multiplier, instance.series],
        )?;
        if instance.series == 0. {
            return Err(ModelError::Invalid("F-N diode zero series count"));
        }
        let dimensions = if length == 0. || width == 0. {
            [1., 1.]
        } else {
            [length, width]
        };
        let parallel = [instance.multiplier, dimensions[0], dimensions[1]];
        let capacitance = scaled_product(
            &[&parallel, &[OXIDE_CAPACITANCE_COEFFICIENT]],
            &[self.oxide_thickness, instance.series],
            0.,
        );
        finite("F-N diode capacitance", &[capacitance])?;
        Ok(PreparedInstance {
            parallel,
            thickness: self.oxide_thickness,
            series: instance.series,
            capacitance,
        })
    }
}

/// Multiply signed factors, divide by nonzero signed factors, and apply an
/// exponential. The common path keeps direct binary64 operations; if any
/// intermediate is outside the normal range, recover the whole product in
/// log magnitude rather than discard a representable scaled result.
fn scaled_product(groups: &[&[f64]], divisors: &[f64], exponent: f64) -> f64 {
    let factors = || groups.iter().flat_map(|group| group.iter().copied());
    if factors().any(|factor| factor == 0.) {
        return 0.;
    }
    let mut value = 1.;
    let mut normal = true;
    for factor in factors() {
        value *= factor;
        normal &= value.is_normal();
    }
    for divisor in divisors {
        value /= divisor;
        normal &= value.is_normal();
    }
    let exponential = exponent.exp();
    value *= exponential;
    if normal && exponential.is_normal() && value.is_normal() {
        return value;
    }
    let negative = factors()
        .chain(divisors.iter().copied())
        .fold(false, |negative, factor| {
            negative ^ factor.is_sign_negative()
        });
    let log = factors().map(|factor| factor.abs().ln()).sum::<f64>()
        - divisors.iter().map(|value| value.abs().ln()).sum::<f64>();
    signed_exp(log + exponent, negative)
}

fn signed_exp(log: f64, negative: bool) -> f64 {
    log.exp().copysign(if negative { -1. } else { 1. })
}

impl PreparedInstance {
    /// Terminal capacitance, constant with voltage.
    #[must_use = "the returned capacitance is needed for charge integration"]
    pub const fn capacitance(self) -> f64 {
        self.capacitance
    }

    /// Evaluate the memoryless conduction and linear oxide storage laws.
    ///
    /// At exactly zero voltage, conduction and its derivative are zero even
    /// for negative oxide thickness. Away from zero, signed negative `Tox` is
    /// evaluated without artificial floors or saturation.
    ///
    /// # Errors
    /// Nonfinite voltage, or nonrepresentable physical outputs.
    pub fn evaluate(self, voltage: f64) -> Result<Evaluation, ModelError> {
        finite("F-N diode voltage", &[voltage])?;
        let (current, conductance) = self.conduction(voltage);
        let charge = scaled_product(
            &[&self.parallel, &[OXIDE_CAPACITANCE_COEFFICIENT, voltage]],
            &[self.thickness, self.series],
            0.,
        );
        finite("F-N diode evaluation", &[current, conductance, charge])?;
        Ok(Evaluation {
            current,
            conductance,
            charge,
            capacitance: self.capacitance,
        })
    }

    fn conduction(self, voltage: f64) -> (f64, f64) {
        if voltage == 0. || self.parallel[0] == 0. {
            return (0., 0.);
        }
        let local = (voltage / self.series).abs();
        let thickness = self.thickness.abs();
        let polarity = self.thickness.signum();
        let direct_ratio = local / thickness;
        // A nonrepresentable intermediate local voltage does not imply
        // that the field ratio or the terminal physical law is nonfinite.
        let ratio = if local.is_normal() && direct_ratio.is_normal() {
            direct_ratio
        } else {
            (voltage.abs().ln() - self.series.abs().ln() - thickness.ln()).exp()
        };
        let exponent = -polarity / ratio;
        let sign = voltage.signum() * self.series.signum();
        let derivative_factor = if local.is_normal() {
            let numerator = 2_f64.mul_add(local, self.thickness);
            if numerator.is_finite() {
                numerator / thickness
            } else {
                2_f64.mul_add(ratio, polarity)
            }
        } else {
            2_f64.mul_add(ratio, polarity)
        };
        if ratio.is_normal() && derivative_factor.is_finite() {
            return (
                scaled_product(
                    &[&self.parallel, &[CURRENT_COEFFICIENT, sign, ratio, ratio]],
                    &[],
                    exponent,
                ),
                scaled_product(
                    &[&self.parallel, &[CURRENT_COEFFICIENT, derivative_factor]],
                    &[self.series, thickness],
                    exponent,
                ),
            );
        }
        // For an extreme field, retain its logarithm before rounding the
        // ratio to zero or infinity. At tiny fields the tunnelling exponent
        // dominates all finite binary64 geometry and multiplicity factors.
        let log_ratio = voltage.abs().ln() - self.series.abs().ln() - thickness.ln();
        let negative_scale = self
            .parallel
            .iter()
            .filter(|v| v.is_sign_negative())
            .count()
            % 2
            != 0;
        let scale = self
            .parallel
            .iter()
            .map(|value| value.abs().ln())
            .sum::<f64>()
            + CURRENT_COEFFICIENT.ln();
        let current = signed_exp(
            2_f64.mul_add(log_ratio, scale) + exponent,
            negative_scale ^ sign.is_sign_negative(),
        );
        let conductance = if derivative_factor == 0. {
            0.
        } else {
            let derivative_log = if derivative_factor.is_finite() {
                derivative_factor.abs().ln()
            } else {
                // In this regime the bounded additive term ±1 cannot
                // affect the rounded magnitude of 2*ratio.
                std::f64::consts::LN_2 + log_ratio
            };
            signed_exp(
                scale + derivative_log - self.series.abs().ln() - thickness.ln() + exponent,
                negative_scale
                    ^ self.series.is_sign_negative()
                    ^ derivative_factor.is_sign_negative(),
            )
        };
        (current, conductance)
    }

    /// Shot noise power density in A²/Hz, from the terminal conduction current.
    ///
    /// # Errors
    /// Nonfinite current or a negative series multiplicity at nonzero current.
    /// No thermal or flicker term is part of this source law.
    pub fn shot_noise(self, current: f64) -> Result<f64, ModelError> {
        finite("F-N diode noise current", &[current])?;
        if current == 0. {
            return Ok(0.);
        }
        if self.series < 0. {
            return Err(ModelError::Invalid("F-N diode negative shot noise density"));
        }
        let density = scaled_product(
            &[&[2. * SHOT_ELEMENTARY_CHARGE, current.abs()]],
            &[self.series],
            0.,
        );
        finite("F-N diode shot noise", &[density])?;
        Ok(density)
    }
}
