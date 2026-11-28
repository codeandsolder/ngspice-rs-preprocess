//! Intrinsic physical current, charge and derivatives at bound temperature.
pub mod ltspice;
mod trial;
use super::{
    BOLTZMANN, ELEMENTARY_CHARGE, Instance, Junction, K_OVER_Q, Model, ModelError, Noise, finite,
    nonnegative, positive,
};

#[derive(Clone, Copy, Debug, Default)]
struct Slope {
    value: f64,
    derivative: f64,
}

#[derive(Clone, Copy, Debug)]
struct ExponentialJunction {
    saturation: f64,
    log_saturation: f64,
    inverse_vt: f64,
}

impl ExponentialJunction {
    fn new(
        junction: Junction,
        factor: f64,
        vt: f64,
        temperature_exponent: f64,
    ) -> Result<Self, ModelError> {
        let log_saturation = junction.saturation_current.ln()
            + factor.ln()
            + temperature_exponent / junction.emission;
        let saturation = log_saturation.exp();
        let inverse_vt = (vt * junction.emission).recip();
        finite("derived junction", &[saturation, inverse_vt])?;
        Ok(Self {
            saturation,
            log_saturation,
            inverse_vt,
        })
    }

    // expm1 preserves small forward/reverse bias. For large arguments avoid
    // overflowing exp(x) when the scaled result is representable.
    fn evaluate(self, voltage: f64, knee: Option<f64>) -> Slope {
        if self.log_saturation == f64::NEG_INFINITY {
            return Slope::default();
        }
        let x = voltage * self.inverse_vt;
        let exponential = x.exp();
        let raw = self.saturation * x.exp_m1();
        if x <= 0.0 || (self.saturation > 0.0 && raw.is_finite() && exponential.is_finite()) {
            let scaled_exponential = self.saturation * exponential;
            if let Some(knee) = knee
                && raw > 0.0
            {
                let ratio = raw / knee;
                if ratio.is_finite() {
                    let denominator = 1.0 + ratio;
                    let root = denominator.sqrt();
                    return Slope {
                        value: raw / root,
                        derivative: scaled_exponential / root
                            * self.inverse_vt
                            * 0.5_f64.mul_add(-(ratio / denominator), 1.0),
                    };
                }
            } else {
                return Slope {
                    value: raw,
                    derivative: scaled_exponential * self.inverse_vt,
                };
            }
        }
        // This branch is also valid if I_s underflowed during preparation
        // but a large forward bias makes the physical current representable.
        if x <= 0.0 {
            return Slope {
                value: raw,
                derivative: self.saturation * exponential * self.inverse_vt,
            };
        }
        let one_minus_exp = -(-x).exp_m1();
        let log_raw = self.log_saturation + x + one_minus_exp.ln();
        let (log_current, slope_factor) = knee.map_or((log_raw, 1.0), |knee| {
            let log_ratio = log_raw - knee.ln();
            // log(1+exp(r)) and its sigmoid without overflow.
            let tail = (-log_ratio.abs()).exp();
            let softplus = log_ratio.max(0.0) + tail.ln_1p();
            let fraction = if log_ratio >= 0.0 {
                (1.0 + tail).recip()
            } else {
                tail / (1.0 + tail)
            };
            (
                0.5_f64.mul_add(-softplus, log_raw),
                0.5_f64.mul_add(-fraction, 1.0),
            )
        });
        let value = log_current.exp();
        let derivative = (log_current + self.inverse_vt.abs().ln() + slope_factor.ln()
            - one_minus_exp.ln())
        .exp()
        .copysign(self.inverse_vt);
        Slope { value, derivative }
    }
}

#[derive(Clone, Copy, Debug)]
struct Avalanche {
    voltage: f64,
    log_current: f64,
    inverse_vt: f64,
}

impl Avalanche {
    fn evaluate(self, voltage: f64) -> Slope {
        if self.log_current == f64::NEG_INFINITY {
            return Slope::default();
        }
        let value = (voltage + self.voltage)
            .mul_add(-self.inverse_vt, self.log_current)
            .exp();
        Slope {
            value: -value,
            derivative: value * self.inverse_vt,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Depletion {
    capacitance: f64,
    potential: f64,
    grading: f64,
    threshold: f64,
    threshold_charge: f64,
    threshold_capacitance: f64,
    threshold_slope: f64,
}

impl Depletion {
    fn new(model: super::Depletion, potential: f64, factor: f64) -> Result<Self, ModelError> {
        let threshold = model.forward_coefficient * potential;
        let capacitance = model.capacitance * factor;
        let mut prepared = Self {
            capacitance,
            potential,
            grading: model.grading,
            threshold,
            threshold_charge: 0.0,
            threshold_capacitance: 0.0,
            threshold_slope: 0.0,
        };
        let transition = prepared.below_threshold(threshold);
        prepared.threshold_charge = transition.value;
        prepared.threshold_capacitance = transition.derivative;
        prepared.threshold_slope =
            transition.derivative * model.grading / ((1.0 - model.forward_coefficient) * potential);
        finite(
            "derived depletion law",
            &[
                threshold,
                capacitance,
                prepared.threshold_charge,
                prepared.threshold_capacitance,
                prepared.threshold_slope,
            ],
        )?;
        Ok(prepared)
    }

    fn below_threshold(self, voltage: f64) -> Slope {
        if self.capacitance == 0.0 {
            return Slope::default();
        }
        let log = (-voltage / self.potential).ln_1p();
        let exponent = 1.0 - self.grading;
        let scaled_log = exponent * log;
        // Exact logarithmic M=1 limit, including the continuous tiny
        // exponent limit where multiplication may round to zero.
        let integral = if scaled_log == 0.0 {
            log
        } else {
            scaled_log.exp_m1() / exponent
        };
        Slope {
            value: -self.capacitance * self.potential * integral,
            derivative: self.capacitance * (-self.grading * log).exp(),
        }
    }

    fn evaluate(self, voltage: f64) -> Slope {
        if self.capacitance == 0.0 {
            return Slope::default();
        }
        if voltage < self.threshold {
            return self.below_threshold(voltage);
        }
        let delta = voltage - self.threshold;
        Slope {
            value: (0.5 * self.threshold_slope)
                .mul_add(delta, self.threshold_capacitance)
                .mul_add(delta, self.threshold_charge),
            derivative: self
                .threshold_slope
                .mul_add(delta, self.threshold_capacitance),
        }
    }
}

fn bandgap(temperature: f64) -> f64 {
    1.16 - 7.02e-4 * temperature * temperature / (temperature + 1108.0)
}

fn adjusted_potential(potential: f64, nominal: f64, temperature: f64, vt: f64) -> f64 {
    let ratio = temperature / nominal;
    // Algebraically cancel the arbitrary bandgap reference temperature.
    (3.0 * vt).mul_add(
        -ratio.ln(),
        ratio.mul_add(potential - bandgap(nominal), bandgap(temperature)),
    )
}

fn saturation_exponent(model: &Model, temperature: f64, k_over_q: f64) -> f64 {
    let nominal = model.temperature.nominal;
    let delta = temperature - nominal;
    model.temperature.exponent.mul_add(
        (temperature / nominal).ln(),
        model.temperature.energy_gap / k_over_q * (delta / nominal / temperature),
    )
}

const fn temperature_factor(linear: f64, quadratic: f64, delta: f64) -> f64 {
    quadratic.mul_add(delta, linear).mul_add(delta, 1.0)
}

fn effective_temperature(model: &Model, instance: Instance) -> Result<f64, ModelError> {
    positive("analysis temperature", instance.analysis_temperature)?;
    finite("DTEMP", &[instance.relative_temperature])?;
    let base = model
        .temperature
        .absolute
        .unwrap_or(instance.analysis_temperature + model.temperature.relative);
    let temperature = instance
        .absolute_temperature
        .unwrap_or(base + instance.relative_temperature);
    positive("instance temperature", temperature)?;
    Ok(temperature)
}

/// Shared intrinsic current and depletion law. Source-specific binding
/// determines geometry, historical constants and temperature corrections.
#[derive(Clone, Copy, Debug)]
struct JunctionLaw {
    transport: ExponentialJunction,
    recombination: ExponentialJunction,
    high_injection: Option<f64>,
    depletion: Depletion,
    avalanche: Option<[Avalanche; 2]>,
}

#[derive(Clone, Copy)]
struct JunctionEnvironment {
    factor: f64,
    thermal_voltage: f64,
    saturation_exponent: f64,
    temperature_delta: f64,
    potential: f64,
    capacitance_factor: f64,
    signed_breakdown: bool,
}

impl JunctionLaw {
    fn new(model: &Model, env: JunctionEnvironment) -> Result<Self, ModelError> {
        let transport = ExponentialJunction::new(
            model.transport,
            env.factor,
            env.thermal_voltage,
            env.saturation_exponent,
        )?;
        let recombination = ExponentialJunction::new(
            model.recombination,
            env.factor,
            env.thermal_voltage,
            env.saturation_exponent,
        )?;
        let high_injection = model.high_injection.map(|knee| {
            env.factor
                * knee.current
                * knee
                    .temperature_coefficient
                    .mul_add(env.temperature_delta, 1.)
        });
        if let Some(value) = high_injection {
            positive("temperature-adjusted IKF", value)?;
        }
        let avalanche = prepare_avalanche(
            model,
            env.factor,
            env.thermal_voltage,
            env.temperature_delta,
            env.signed_breakdown,
        )?;
        let depletion = Depletion::new(
            model.depletion,
            env.potential,
            env.factor * env.capacitance_factor,
        )?;
        Ok(Self {
            transport,
            recombination,
            high_injection,
            depletion,
            avalanche,
        })
    }

    fn trial_limits(self) -> trial::TrialLimits {
        trial::TrialLimits::new(self.transport, self.recombination, self.avalanche)
    }

    fn evaluate_recombination(&self, voltage: f64) -> Slope {
        if self.recombination.log_saturation == f64::NEG_INFINITY {
            return Slope::default();
        }
        let rec = self.recombination.evaluate(voltage, None);
        let x = 1.0 - voltage / self.depletion.potential;
        let base = x.mul_add(x, 0.005);
        let generation = base.powf(self.depletion.grading * 0.5);
        let generation_derivative =
            -self.depletion.grading * x / self.depletion.potential / base * generation;
        Slope {
            value: rec.value * generation,
            derivative: rec
                .derivative
                .mul_add(generation, rec.value * generation_derivative),
        }
    }

    fn forward(&self, voltage: f64) -> Slope {
        let transport = self.transport.evaluate(voltage, self.high_injection);
        let recombination = self.evaluate_recombination(voltage);
        Slope {
            value: transport.value + recombination.value,
            derivative: transport.derivative + recombination.derivative,
        }
    }

    fn reverse(&self, voltage: f64) -> Slope {
        let mut result = Slope::default();
        if let Some(terms) = self.avalanche {
            for term in terms {
                let term = term.evaluate(voltage);
                result.value += term.value;
                result.derivative += term.derivative;
            }
        }
        result
    }

    fn conduction(&self, voltage: f64) -> Slope {
        let mut physical = self.forward(voltage);
        // Preserve the original separate addition order for SIMetrix.
        if let Some(terms) = self.avalanche {
            for term in terms {
                let term = term.evaluate(voltage);
                physical.value += term.value;
                physical.derivative += term.derivative;
            }
        }
        physical
    }
}

/// An immutable, temperature- and geometry-bound physical instance.
///
/// Intrinsic bias is anode minus cathode. The solver binds an external
/// resistor node only if the prepared series resistance is nonzero.
#[derive(Clone, Copy, Debug)]
pub struct PreparedInstance {
    law: JunctionLaw,
    gmin: f64,
    transit_time: f64,
    series_resistance: f64,
    temperature: f64,
    multiplier: f64,
    noise: Noise,
    trial_limits: trial::TrialLimits,
}

/// Signed junction outputs, excluding the separate ohmic series branch.
#[derive(Clone, Copy, Debug, Default)]
pub struct Evaluation {
    /// Total intrinsic current including gmin, A.
    pub current: f64,
    /// Exact derivative of current with respect to junction voltage, S.
    pub conductance: f64,
    /// Physical current excluding gmin, A.
    pub physical_current: f64,
    /// Stored charge, C. Includes depletion and diffusion charge.
    pub charge: f64,
    /// Exact derivative of charge with respect to voltage, F.
    pub capacitance: f64,
}

/// Independent local current-noise power spectral densities, in A²/Hz.
/// Shot and flicker sources span the intrinsic junction; the series
/// thermal source spans the external series branch.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoiseDensity {
    /// Intrinsic shot noise.
    pub shot: f64,
    /// Intrinsic flicker noise.
    pub flicker: f64,
    /// Series resistance thermal noise.
    pub series_thermal: f64,
}

impl PreparedInstance {
    pub(super) fn new(model: &Model, instance: Instance) -> Result<Self, ModelError> {
        positive("AREA", instance.area)?;
        positive("M", instance.multiplier)?;
        nonnegative("gmin", instance.gmin)?;
        let factor = instance.area * instance.multiplier;
        positive("AREA*M", factor)?;
        let temperature = effective_temperature(model, instance)?;
        let nominal = model.temperature.nominal;
        let delta = temperature - nominal;
        let vt = K_OVER_Q * temperature;
        positive("thermal voltage", vt)?;
        let potential = adjusted_potential(model.depletion.potential, nominal, temperature, vt);
        positive("temperature-adjusted VJ", potential)?;
        let exponent = saturation_exponent(model, temperature, K_OVER_Q);
        let law = JunctionLaw::new(
            model,
            JunctionEnvironment {
                factor,
                thermal_voltage: vt,
                saturation_exponent: exponent,
                temperature_delta: delta,
                potential,
                capacitance_factor: 1.,
                signed_breakdown: false,
            },
        )?;
        let series_resistance = model.series.resistance
            * temperature_factor(
                model.series.linear_temperature,
                model.series.quadratic_temperature,
                delta,
            )
            / factor;
        finite("temperature-adjusted RS", &[series_resistance])?;
        let gmin = instance.multiplier * instance.gmin;
        nonnegative("scaled gmin", gmin)?;
        Ok(Self {
            law,
            gmin,
            transit_time: model.transit_time,
            series_resistance,
            temperature,
            multiplier: instance.multiplier,
            noise: model.noise,
            trial_limits: law.trial_limits(),
        })
    }

    /// Effective local device temperature, K.
    #[must_use = "the effective temperature is returned without modifying device state"]
    pub const fn temperature(&self) -> f64 {
        self.temperature
    }

    /// Temperature- and geometry-adjusted series resistance, ohm.
    #[must_use = "the adjusted series resistance is required for branch stamping"]
    pub const fn series_resistance(&self) -> f64 {
        self.series_resistance
    }

    /// Evaluate independent noise densities at the already evaluated DC
    /// junction current (including gmin), without reevaluating the junction.
    ///
    /// # Errors
    /// Rejects nonfinite current, nonpositive frequency, nonrepresentable
    /// densities and noise of negative series resistance.
    pub fn noise_density(&self, current: f64, frequency: f64) -> Result<NoiseDensity, ModelError> {
        finite("DC junction current", &[current])?;
        positive("noise frequency", frequency)?;
        if self.series_resistance < 0.0 {
            return Err(ModelError::Unsupported("negative RS noise"));
        }
        let shot = (2.0 * ELEMENTARY_CHARGE) * current.abs();
        let flicker = if self.noise.coefficient == 0.0 {
            0.0
        } else {
            let unit_current = (current.abs() / self.multiplier).max(1e-38);
            self.multiplier * self.noise.coefficient * unit_current.powf(self.noise.exponent)
                / frequency
        };
        let series_thermal = if self.series_resistance == 0.0 {
            0.0
        } else {
            4.0 * BOLTZMANN * self.temperature / self.series_resistance
        };
        finite("noise density", &[shot, flicker, series_thermal])?;
        Ok(NoiseDensity {
            shot,
            flicker,
            series_thermal,
        })
    }

    /// Evaluate the physical junction and both exact voltage derivatives.
    ///
    /// No allocations, parameter lookups or accepted-state mutation occur.
    /// The caller integrates the stored charge using its own method.
    ///
    /// # Errors
    /// A nonfinite voltage or nonrepresentable result is rejected.
    pub fn evaluate(&self, voltage: f64) -> Result<Evaluation, ModelError> {
        finite("junction voltage", &[voltage])?;
        let physical = self.law.conduction(voltage);
        let depletion = self.law.depletion.evaluate(voltage);
        let result = Evaluation {
            current: self.gmin.mul_add(voltage, physical.value),
            conductance: physical.derivative + self.gmin,
            physical_current: physical.value,
            charge: self.transit_time.mul_add(physical.value, depletion.value),
            capacitance: self
                .transit_time
                .mul_add(physical.derivative, depletion.derivative),
        };
        finite(
            "junction evaluation",
            &[
                result.current,
                result.conductance,
                result.physical_current,
                result.charge,
                result.capacitance,
            ],
        )?;
        Ok(result)
    }
}

fn prepare_avalanche(
    model: &Model,
    factor: f64,
    vt: f64,
    delta: f64,
    signed: bool,
) -> Result<Option<[Avalanche; 2]>, ModelError> {
    let Some(breakdown) = model.breakdown else {
        return Ok(None);
    };
    let voltage = breakdown.voltage
        * temperature_factor(
            breakdown.linear_temperature,
            breakdown.quadratic_temperature,
            delta,
        );
    if signed {
        finite("temperature-adjusted BV", &[voltage])?;
    } else {
        positive("temperature-adjusted BV", voltage)?;
    }
    let terms = [breakdown.primary, breakdown.low_current].map(|term| Avalanche {
        voltage,
        log_current: term.current.ln() + factor.ln(),
        inverse_vt: (term.emission * vt).recip(),
    });
    for term in terms {
        if signed {
            finite("avalanche inverse thermal voltage", &[term.inverse_vt])?;
            if term.inverse_vt == 0. {
                return Err(ModelError::Invalid(
                    "zero avalanche inverse thermal voltage",
                ));
            }
        } else {
            positive("avalanche inverse thermal voltage", term.inverse_vt)?;
        }
    }
    Ok(Some(terms))
}

/// Evaluate a homogeneous batch with caller-provided storage.
///
/// # Errors
/// Mismatched slice lengths or an invalid sample fail.
pub fn evaluate_batch(
    instance: &PreparedInstance,
    voltages: &[f64],
    output: &mut [Evaluation],
) -> Result<(), ModelError> {
    if voltages.len() != output.len() {
        return Err(ModelError::Invalid("batch length"));
    }
    for (voltage, slot) in voltages.iter().copied().zip(output) {
        *slot = instance.evaluate(voltage)?;
    }
    Ok(())
}
