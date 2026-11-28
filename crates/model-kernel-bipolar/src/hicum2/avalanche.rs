//! HICUM/L2 2.4.0 source HICAVL impact-ionization current.
//!
//! Uses the explicitly published HICJQ collector capacitance Cjci rather
//! than dQjci/dV where they differ. Source branch law and strong-avalanche
//! smoothing are fully coupled to Itf, with analytic intrinsic Jacobians.
//! The limiting QAVL=0 continuation is defined by Cjci0/Cjci.

use super::{
    differentiation::{Dual, Scalar},
    junction, transport,
};
use crate::{EvaluationError, ModelError, validate_nonnegative, validate_positive};

#[derive(Clone, Copy, Debug)]
pub struct Parameters {
    pub favl: f64,
    pub qavl: f64,
    pub kavl: f64,
    /// Effective-temperature internal B-C zero-bias capacitance (F).
    pub collector_zero_capacitance: f64,
    /// Effective-temperature internal B-C built-in potential (V).
    pub collector_potential: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct Bias {
    pub vbici: f64,
    /// HICCR uncorrected forward transport Itf, not net Itf-Itr.
    pub forward: transport::Value2,
    /// HICJQ B-C depletion charge including the *source* capacitance Cjci
    /// and its derivative, as emitted by the HICUM junction kernel.
    pub intrinsic_collector: junction::Junction,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Output {
    /// Generated Iavl (A) and analytic dI/dVbiei, dI/dVbici (S).
    pub generation: transport::Value2,
    /// The source dimensionless avalanche gain including strong correction.
    pub gain: f64,
    /// Partial derivative dGain/dVbici at fixed forward transfer current.
    pub d_gain_vbici: f64,
}

/// Effective source FAVL/QAVL/KAVL logarithmic temperature slopes.
#[derive(Clone, Copy, Debug, Default)]
pub struct TemperatureSlopes {
    pub favl: f64,
    pub qavl: f64,
    pub kavl: f64,
}

/// HICAVL gain derivative at fixed forward transfer current; full
/// generated-current slope also needs caller-supplied dItf/dTdev.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TemperatureSensitivity {
    pub gain: f64,
    pub generation: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct Prepared {
    parameters: Parameters,
    temperature_slopes: TemperatureSlopes,
}

/// Static scalar specialization of the actual source gain equations.
#[derive(Clone, Copy)]
struct GainCoefficients<S> {
    favl: S,
    qavl: S,
    kavl: S,
    collector_zero_capacitance: S,
    collector_potential: S,
}

fn gain<S: Scalar>(
    p: GainCoefficients<S>,
    voltage: S,
    capacitance: S,
    d_cap: S,
) -> Result<(S, S), EvaluationError> {
    let one = S::constant(1.0);
    let border = p.collector_potential - voltage;
    if p.favl.value() == 0.0 || p.collector_zero_capacitance.value() == 0.0 || border.value() <= 0.0
    {
        return Ok((S::constant(0.0), S::constant(0.0)));
    }
    if capacitance.value() <= 0.0 {
        return Err(EvaluationError::InvalidPhysicalDomain);
    }
    let base_width = p.qavl / p.collector_zero_capacitance;
    let (raw, gradient) = if border.value() > base_width.value() {
        let normalized_cap = p.collector_zero_capacitance / capacitance;
        let derivative = -normalized_cap * d_cap / capacitance;
        let amplitude = p.favl * (-normalized_cap).exp();
        let span = border - base_width;
        let integral = (one + normalized_cap).mul_add(span, base_width);
        let d_integral = derivative.mul_add(span, -(one + normalized_cap));
        let avl = amplitude * integral;
        let d_avl = amplitude * derivative.mul_add(-integral, d_integral);
        (avl, d_avl)
    } else {
        let v_q = p.qavl / capacitance;
        let normalized = v_q / border;
        let weight = p.favl * (-normalized).exp();
        let d_vq = -v_q * d_cap / capacitance;
        let d_normalized = d_vq / border + v_q / (border * border);
        (
            weight * border,
            weight * border.mul_add(-d_normalized, -one),
        )
    };
    let (gain, d_gain) = if p.kavl.value() > 0.0 {
        let denominator = p.kavl.mul_add(-raw, one);
        let root = denominator.hypot(0.1);
        let half = if denominator.value() < 0.0 {
            S::constant(0.005) / (root - denominator)
        } else {
            denominator.midpoint(root)
        };
        let d_half = p.kavl * (-0.5) * (one + denominator / root);
        let corrected_gain = raw / half;
        let squared_half = half * half;
        let corrected_gradient = (one / half - raw * d_half / squared_half) * gradient;
        (corrected_gain, corrected_gradient)
    } else {
        (raw, gradient)
    };
    if ![gain, d_gain].iter().all(|x| x.is_finite()) {
        return Err(EvaluationError::NonFiniteResult);
    }
    Ok((gain, d_gain))
}

impl Prepared {
    /// # Errors
    /// Reports the offending unphysical effective HICUM model parameter.
    pub fn new(parameters: Parameters) -> Result<Self, ModelError> {
        Self::with_temperature_slopes(parameters, TemperatureSlopes::default())
    }

    /// Prepared source avalanche gain temperature transform.
    ///
    /// # Errors
    /// Rejects unphysical source coefficient ranges and nonfinite slopes.
    pub fn with_temperature_slopes(
        parameters: Parameters,
        temperature_slopes: TemperatureSlopes,
    ) -> Result<Self, ModelError> {
        if ![
            temperature_slopes.favl,
            temperature_slopes.qavl,
            temperature_slopes.kavl,
        ]
        .iter()
        .all(|s| s.is_finite())
        {
            return Err(ModelError::InvalidParameter("avalanche_temperature_slopes"));
        }
        for (name, value) in [
            ("favl", parameters.favl),
            ("qavl", parameters.qavl),
            ("kavl", parameters.kavl),
            ("cjci0", parameters.collector_zero_capacitance),
        ] {
            validate_nonnegative(name, value)?;
        }
        validate_positive("vdci", parameters.collector_potential)?;
        Ok(Self {
            parameters,
            temperature_slopes,
        })
    }

    /// Evaluate source HICAVL for one intrinsic B-C junction and forward
    /// HICCR current. This output is a generated B-C avalanche current,
    /// subtracted from the diode IBCI at stamping.
    ///
    /// # Errors
    /// Rejects non-finite/invalid source inputs, or numerical overflow.
    pub fn evaluate(&self, bias: Bias) -> Result<Output, EvaluationError> {
        let p = self.parameters;
        if ![
            bias.vbici,
            bias.forward.value,
            bias.forward.d_vbiei,
            bias.forward.d_vbici,
            bias.intrinsic_collector.source_capacitance,
            bias.intrinsic_collector.d_source_capacitance,
        ]
        .iter()
        .all(|x| x.is_finite())
        {
            return Err(EvaluationError::NonFiniteBias);
        }
        if bias.forward.value < 0.0 {
            return Err(EvaluationError::InvalidPhysicalDomain);
        }
        if p.favl == 0.0 || p.collector_zero_capacitance == 0.0 {
            return Ok(Output::default());
        }
        let border = p.collector_potential - bias.vbici;
        if border <= 0.0 {
            return Ok(Output::default());
        }
        let capacitance = bias.intrinsic_collector.source_capacitance;
        if capacitance <= 0.0 {
            return Err(EvaluationError::InvalidPhysicalDomain);
        }
        let (gain, d_gain) = gain(
            GainCoefficients {
                favl: p.favl,
                qavl: p.qavl,
                kavl: p.kavl,
                collector_zero_capacitance: p.collector_zero_capacitance,
                collector_potential: p.collector_potential,
            },
            bias.vbici,
            capacitance,
            bias.intrinsic_collector.d_source_capacitance,
        )?;
        let forward = bias.forward;
        let generated = transport::Value2 {
            value: forward.value * gain,
            d_vbiei: forward.d_vbiei * gain,
            d_vbici: forward.d_vbici.mul_add(gain, forward.value * d_gain),
        };
        if ![
            gain,
            d_gain,
            generated.value,
            generated.d_vbiei,
            generated.d_vbici,
        ]
        .iter()
        .all(|x| x.is_finite())
        {
            return Err(EvaluationError::NonFiniteResult);
        }
        Ok(Output {
            generation: generated,
            gain,
            d_gain_vbici: d_gain,
        })
    }

    /// Exact fixed-forward-current source HICAVL gain temperature
    /// derivative. The forward Itf temperature derivative is supplied
    /// separately; use the full method to complete the current.
    ///
    /// # Errors
    /// Rejects invalid collector source capacitance or overflow.
    pub fn gain_temperature_sensitivity(
        &self,
        bias: Bias,
        collector_sensitivity: junction::CollectorSensitivity,
        coefficient_slopes: junction::CoefficientSlopes,
    ) -> Result<f64, EvaluationError> {
        let p = self.parameters;
        if p.favl == 0.0
            || p.collector_zero_capacitance == 0.0
            || p.collector_potential - bias.vbici <= 0.0
        {
            return Ok(0.0);
        }
        let slopes = self.temperature_slopes;
        let (value, _) = gain(
            GainCoefficients {
                favl: Dual::new(p.favl, p.favl * slopes.favl),
                qavl: Dual::new(p.qavl, p.qavl * slopes.qavl),
                kavl: Dual::new(p.kavl, p.kavl * slopes.kavl),
                collector_zero_capacitance: Dual::new(
                    p.collector_zero_capacitance,
                    coefficient_slopes.zero_bias_capacitance,
                ),
                collector_potential: Dual::new(p.collector_potential, coefficient_slopes.potential),
            },
            Dual::new(bias.vbici, 0.0),
            Dual::new(
                bias.intrinsic_collector.source_capacitance,
                collector_sensitivity.source_capacitance,
            ),
            Dual::new(bias.intrinsic_collector.d_source_capacitance, 0.0),
        )?;
        Ok(value.slope)
    }

    /// Full source avalanche gain and generated-current electrothermal
    /// partials. Requires the exact forward-current dItf/dTdev from
    /// HICCR; never assumes the missing transport derivative is zero.
    ///
    /// # Errors
    /// Nonfinite caller derivatives, collector data or overflow.
    pub fn temperature_sensitivity(
        &self,
        bias: Bias,
        operating: Output,
        collector_sensitivity: junction::CollectorSensitivity,
        coefficient_slopes: junction::CoefficientSlopes,
        d_forward_current_d_temperature: f64,
    ) -> Result<TemperatureSensitivity, EvaluationError> {
        if !d_forward_current_d_temperature.is_finite() {
            return Err(EvaluationError::NonFiniteBias);
        }
        let gain_derivative =
            self.gain_temperature_sensitivity(bias, collector_sensitivity, coefficient_slopes)?;
        let current_derivative = d_forward_current_d_temperature
            .mul_add(operating.gain, bias.forward.value * gain_derivative);
        if !current_derivative.is_finite() {
            return Err(EvaluationError::NonFiniteResult);
        }
        Ok(TemperatureSensitivity {
            gain: gain_derivative,
            generation: current_derivative,
        })
    }

    /// All slice lengths are checked before any writes. Lanes run in order;
    /// the first error leaves that lane and the remaining outputs unchanged.
    ///
    /// # Errors
    /// Reports length mismatch or invalid physical evaluation.
    pub fn evaluate_batch(
        &self,
        inputs: &[Bias],
        outputs: &mut [Output],
    ) -> Result<(), EvaluationError> {
        crate::evaluate_lanes(inputs.iter().copied(), outputs, |input| {
            self.evaluate(input)
        })
    }
}
