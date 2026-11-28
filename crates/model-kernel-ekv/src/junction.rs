//! Explicit SPICE-style PN junctions, separate from the EKV channel.
//! The law uses the charge integral of depletion capacitance (including
//! its forward continuation), not capacitance multiplied by voltage.

use model_kernel_api::{
    Error, Result, taylor::arithmetic::Scalar, trial::limiting::ExponentialLimit,
};

use crate::parameters::bandgap;

/// Nominal zero-bias capacitance and potential for one junction component.
#[derive(Clone, Copy, Debug)]
pub struct Depletion {
    /// Capacitance of this entire per-finger component, farads.
    pub capacitance: f64,
    pub potential: f64,
    pub grading: f64,
}

/// Resolved nominal PN junction. Areas, perimeters and model defaults
/// are resolved by the source adapter before constructing this value.
#[derive(Clone, Copy, Debug)]
pub struct Junction {
    pub saturation_current: f64,
    pub emission: f64,
    pub temperature_exponent: f64,
    pub bottom: Depletion,
    pub sidewall: Depletion,
    pub forward_coefficient: f64,
    pub transit_time: f64,
}

impl Default for Junction {
    fn default() -> Self {
        Self {
            saturation_current: 0.,
            emission: 1.,
            temperature_exponent: 0.,
            bottom: Depletion {
                capacitance: 0.,
                potential: 0.8,
                grading: 0.5,
            },
            sidewall: Depletion {
                capacitance: 0.,
                potential: 0.8,
                grading: 0.33,
            },
            forward_coefficient: 0.5,
            transit_time: 0.,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct PreparedDepletion {
    capacitance: f64,
    potential: f64,
    grading: f64,
    forward_voltage: f64,
    forward_charge: f64,
    forward_capacitance: f64,
    forward_slope: f64,
}

/// Bias-independent physical junction, with positive current and charge
/// into the p side. All quantities are for the complete parallel device.
#[derive(Clone, Copy, Debug)]
pub struct PreparedJunction {
    log_saturation: f64,
    inverse_voltage: f64,
    depletion: [PreparedDepletion; 2],
    transit_time: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct Evaluation {
    pub current: f64,
    pub conductance: f64,
    pub charge: f64,
    pub capacitance: f64,
}

fn primitive(v: f64, potential: f64, grading: f64, capacitance: f64) -> f64 {
    let logarithm = (-v / potential).ln_1p();
    -capacitance * potential * logarithm * ((1. - grading) * logarithm).exprel()
}

impl PreparedDepletion {
    fn new(model: Depletion, fc: f64, temperature: Temperature, multiplier: f64) -> Result<Self> {
        if !model.capacitance.is_finite()
            || model.capacitance < 0.
            || !model.potential.is_finite()
            || model.potential <= 0.
            || !(0. ..=1.).contains(&model.grading)
        {
            return Err(Error::Domain("invalid EKV depletion parameter".into()));
        }
        let potential = model
            .potential
            .mul_add(temperature.ratio, temperature.potential_shift);
        let capacitance = model.capacitance
            * multiplier
            * model.grading.mul_add(
                temperature
                    .delta
                    .mul_add(0.0004, 1. - potential / model.potential),
                1.,
            );
        if !potential.is_finite() || potential <= 0. || !capacitance.is_finite() || capacitance < 0.
        {
            return Err(Error::Domain(
                "invalid temperature-adjusted EKV depletion".into(),
            ));
        }
        let forward_voltage = fc * potential;
        let forward_capacitance = capacitance * (1. - fc).powf(-model.grading);
        let forward_charge = primitive(forward_voltage, potential, model.grading, capacitance);
        let forward_slope = forward_capacitance * model.grading / (potential * (1. - fc));
        if [forward_capacitance, forward_charge, forward_slope]
            .iter()
            .any(|x| !x.is_finite())
        {
            return Err(Error::Domain("nonfinite EKV depletion continuation".into()));
        }
        Ok(Self {
            capacitance,
            potential,
            grading: model.grading,
            forward_voltage,
            forward_charge,
            forward_capacitance,
            forward_slope,
        })
    }

    fn evaluate(self, voltage: f64) -> (f64, f64) {
        if self.capacitance == 0. {
            return (0., 0.);
        }
        if voltage <= self.forward_voltage {
            let capacitance =
                self.capacitance * (-self.grading * (-voltage / self.potential).ln_1p()).exp();
            (
                primitive(voltage, self.potential, self.grading, self.capacitance),
                capacitance,
            )
        } else {
            let delta = voltage - self.forward_voltage;
            let charge = delta.mul_add(
                (0.5 * self.forward_slope).mul_add(delta, self.forward_capacitance),
                self.forward_charge,
            );
            (
                charge,
                self.forward_slope.mul_add(delta, self.forward_capacitance),
            )
        }
    }
}

#[derive(Clone, Copy)]
struct Temperature {
    ratio: f64,
    delta: f64,
    potential_shift: f64,
}

impl Junction {
    /// Prepare a conventional SPICE MOS junction from nominal data and a
    /// thermal voltage selected by the enclosing model.
    ///
    /// # Errors
    /// Invalid model or derived temperature quantities.
    pub fn prepare(
        self,
        nominal: f64,
        temperature: f64,
        vt: f64,
        multiplier: f64,
    ) -> Result<PreparedJunction> {
        if [nominal, temperature, vt, multiplier, self.emission]
            .iter()
            .any(|x| !x.is_finite() || *x <= 0.)
            || [self.saturation_current, self.transit_time]
                .iter()
                .any(|x| !x.is_finite() || *x < 0.)
            || !self.temperature_exponent.is_finite()
            || !(0. ..1.).contains(&self.forward_coefficient)
        {
            return Err(Error::Domain("invalid EKV junction parameter".into()));
        }
        let ratio = temperature / nominal;
        let gap_shift = bandgap(nominal).mul_add(ratio, -bandgap(temperature));
        let potential_shift = (-3. * vt).mul_add(ratio.ln(), -gap_shift);
        let temp = Temperature {
            ratio,
            delta: temperature - nominal,
            potential_shift,
        };
        let log_saturation = if self.saturation_current == 0. {
            f64::NEG_INFINITY
        } else {
            self.temperature_exponent.mul_add(
                ratio.ln(),
                self.saturation_current.ln() + multiplier.ln() + gap_shift / vt,
            )
        };
        let inverse_voltage = (self.emission * vt).recip();
        if (self.saturation_current > 0. && !log_saturation.is_finite())
            || !inverse_voltage.is_finite()
            || inverse_voltage <= 0.
        {
            return Err(Error::Domain(
                "nonfinite EKV junction temperature response".into(),
            ));
        }
        Ok(PreparedJunction {
            log_saturation,
            inverse_voltage,
            transit_time: self.transit_time,
            depletion: [
                PreparedDepletion::new(self.bottom, self.forward_coefficient, temp, multiplier)?,
                PreparedDepletion::new(self.sidewall, self.forward_coefficient, temp, multiplier)?,
            ],
        })
    }
}

impl PreparedJunction {
    /// Evaluate the exact residual, stored charge and both voltage derivatives.
    ///
    /// # Errors
    /// Nonfinite voltage or nonrepresentable physical result.
    pub fn evaluate(self, voltage: f64) -> Result<Evaluation> {
        if !voltage.is_finite() {
            return Err(Error::Domain("nonfinite EKV junction bias".into()));
        }
        let (current, conductance) = if self.log_saturation == f64::NEG_INFINITY {
            (0., 0.)
        } else {
            let x = voltage * self.inverse_voltage;
            let saturation = self.log_saturation.exp();
            let exponential = (self.log_saturation + x).exp();
            let current = if x <= 0. || (x < 700. && saturation > 0.) {
                saturation * x.exp_m1()
            } else {
                exponential - saturation
            };
            (current, exponential * self.inverse_voltage)
        };
        let (q0, c0) = self.depletion[0].evaluate(voltage);
        let (q1, c1) = self.depletion[1].evaluate(voltage);
        let charge = self.transit_time.mul_add(current, q0 + q1);
        let capacitance = self.transit_time.mul_add(conductance, c0 + c1);
        if [current, conductance, charge, capacitance]
            .iter()
            .any(|x| !x.is_finite())
        {
            return Err(Error::Domain("nonfinite EKV junction response".into()));
        }
        Ok(Evaluation {
            current,
            conductance,
            charge,
            capacitance,
        })
    }

    /// The shared logarithmic numerical trial policy, independent of F/Q.
    #[must_use]
    pub fn limiter(self) -> Option<ExponentialLimit> {
        ExponentialLimit::new(self.log_saturation, self.inverse_voltage)
    }
}
