//! HICUM/L2 2.4.0 HICTUN B-E tunneling branch.
//!
//! A temperature-prepared IBETS/ABET pair multiplies the source QJMODF
//! capacitance-dependent field factor. Source tunode selects the intrinsic
//! or peripheral B-E junction and corresponding topology stamp. The outer
//! gate checks both voltages regardless of selected branch, exactly as the
//! HICUM/L2 2.4.0 Verilog-A source does.

use super::junction;
use crate::{EvaluationError, ModelError, evaluate_lanes, validate_nonnegative, validate_positive};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Location {
    Intrinsic,
    Peripheral,
}

#[derive(Clone, Copy, Debug)]
pub struct Parameters {
    pub saturation: f64,
    pub attenuation: f64,
    pub location: Location,
    pub intrinsic: junction::Forward,
    pub peripheral: junction::Forward,
}

#[derive(Clone, Copy, Debug)]
pub struct Bias {
    pub vbiei: f64,
    pub vbpei: f64,
    pub intrinsic: junction::Junction,
    pub peripheral: junction::Junction,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Output {
    /// Source Ibet; the selected branch contributes negative Ibet to KCL.
    pub amperes: f64,
    pub d_vbiei: f64,
    pub d_vbpei: f64,
}

/// Logarithmic dln IBETS/dTdev and dln ABET/dTdev in 1/K.
#[derive(Clone, Copy, Debug, Default)]
pub struct TemperatureSlopes {
    pub saturation: f64,
    pub attenuation: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct Prepared {
    p: Parameters,
    temperature_slopes: TemperatureSlopes,
}

impl Prepared {
    /// # Errors
    /// Rejects invalid effective HICUM tunneling or junction coefficients.
    pub fn new(p: Parameters) -> Result<Self, ModelError> {
        Self::with_temperature_slopes(p, TemperatureSlopes::default())
    }

    /// Retain exact source IBETS and ABET logarithmic thermal slopes
    /// after nominal-to-effective model preparation.
    ///
    /// # Errors
    /// Rejects unphysical coefficients and nonfinite source slopes.
    pub fn with_temperature_slopes(
        p: Parameters,
        temperature_slopes: TemperatureSlopes,
    ) -> Result<Self, ModelError> {
        if ![
            temperature_slopes.saturation,
            temperature_slopes.attenuation,
        ]
        .iter()
        .all(|value| value.is_finite())
        {
            return Err(ModelError::InvalidParameter("tunneling_temperature_slopes"));
        }
        validate_nonnegative("ibets", p.saturation)?;
        validate_nonnegative("abet", p.attenuation)?;
        for (name, j) in [("intrinsic", p.intrinsic), ("peripheral", p.peripheral)] {
            validate_nonnegative(name, j.zero_bias_capacitance)?;
            validate_positive(name, j.potential)?;
            if !j.grading.is_finite() || j.grading <= 0.0 || j.grading >= 1.0 {
                return Err(ModelError::InvalidParameter(name));
            }
            validate_positive(name, j.max_cap_ratio)?;
        }
        Ok(Self {
            p,
            temperature_slopes,
        })
    }

    /// # Errors
    /// Rejects invalid voltages or capacitances and numerical overflow.
    pub fn evaluate(&self, bias: Bias) -> Result<Output, EvaluationError> {
        if ![
            bias.vbiei,
            bias.vbpei,
            bias.intrinsic.source_capacitance,
            bias.intrinsic.d_source_capacitance,
            bias.peripheral.source_capacitance,
            bias.peripheral.d_source_capacitance,
        ]
        .iter()
        .all(|value| value.is_finite())
        {
            return Err(EvaluationError::NonFiniteBias);
        }
        if self.p.saturation == 0.0 || (bias.vbiei >= 0.0 && bias.vbpei >= 0.0) {
            return Ok(Output::default());
        }
        let (voltage, j, supplied) = match self.p.location {
            Location::Intrinsic => (bias.vbiei, self.p.intrinsic, bias.intrinsic),
            Location::Peripheral => (bias.vbpei, self.p.peripheral, bias.peripheral),
        };
        if j.zero_bias_capacitance == 0.0 {
            return Ok(Output::default());
        }
        if supplied.source_capacitance <= 0.0 {
            return Err(EvaluationError::InvalidPhysicalDomain);
        }
        let capacitance_ratio = supplied.source_capacitance / j.zero_bias_capacitance;
        let field_coefficient = 1.0 - 1.0 / j.grading;
        let field = (field_coefficient * capacitance_ratio.ln()).exp();
        // The source's field factor can underflow for extremely small
        // grading exponents. Its physical current and derivative both
        // vanish in that limit, including when ABET is exactly zero.
        if field == 0.0 {
            return Ok(Output::default());
        }
        let d_field =
            field * field_coefficient * supplied.d_source_capacitance / supplied.source_capacitance;
        let activation = (-self.p.attenuation / field).exp();
        let amplitude = -self.p.saturation / j.potential * activation;
        let current = amplitude * voltage * field;
        let derivative =
            amplitude * (voltage * d_field).mul_add(1.0 + self.p.attenuation / field, field);
        if ![current, derivative].iter().all(|value| value.is_finite()) {
            return Err(EvaluationError::NonFiniteResult);
        }
        let output = match self.p.location {
            Location::Intrinsic => Output {
                amperes: current,
                d_vbiei: derivative,
                d_vbpei: 0.0,
            },
            Location::Peripheral => Output {
                amperes: current,
                d_vbiei: 0.0,
                d_vbpei: derivative,
            },
        };
        Ok(output)
    }

    /// Exact source HICTUN dI/dTdev at constant normalized bias.
    /// Includes IBETS, ABET and both temperature-prepared junction
    /// Cj0/Vd and effective QJMODF source capacitance derivatives.
    ///
    /// # Errors
    /// Rejects nonphysical supplied capacitance and nonfinite results.
    pub fn temperature_sensitivity(
        &self,
        bias: Bias,
        operating: Output,
        junction_coefficient_slopes: junction::ParameterSlopes,
        junction_charge_slopes: junction::EmitterSensitivities,
    ) -> Result<f64, EvaluationError> {
        let p = self.p;
        if p.saturation == 0.0 || (bias.vbiei >= 0.0 && bias.vbpei >= 0.0) {
            return Ok(0.0);
        }
        let (j, supplied, coefficients, thermal) = match p.location {
            Location::Intrinsic => (
                p.intrinsic,
                bias.intrinsic,
                junction_coefficient_slopes.intrinsic_emitter,
                junction_charge_slopes.intrinsic,
            ),
            Location::Peripheral => (
                p.peripheral,
                bias.peripheral,
                junction_coefficient_slopes.peripheral_emitter,
                junction_charge_slopes.peripheral,
            ),
        };
        if j.zero_bias_capacitance == 0.0 || operating.amperes == 0.0 {
            return Ok(0.0);
        }
        if supplied.source_capacitance <= 0.0 {
            return Err(EvaluationError::InvalidPhysicalDomain);
        }
        let ratio = supplied.source_capacitance / j.zero_bias_capacitance;
        let field = ((1.0 - 1.0 / j.grading) * ratio.ln()).exp();
        if field == 0.0 {
            return Ok(0.0);
        }
        let source_capacitance_slope = thermal.capacitance / supplied.source_capacitance;
        let reference_capacitance_slope =
            coefficients.zero_bias_capacitance / j.zero_bias_capacitance;
        let logarithmic_field =
            (1.0 - 1.0 / j.grading) * (source_capacitance_slope - reference_capacitance_slope);
        let activation = p.attenuation / field;
        let result = operating.amperes
            * ((1.0 + activation).mul_add(
                logarithmic_field,
                activation.mul_add(
                    -self.temperature_slopes.attenuation,
                    self.temperature_slopes.saturation - coefficients.potential / j.potential,
                ),
            ));
        if !result.is_finite() {
            return Err(EvaluationError::NonFiniteResult);
        }
        Ok(result)
    }

    /// All slice lengths are checked before any writes. Lanes run in order;
    /// the first error leaves that lane and the remaining outputs unchanged.
    ///
    /// # Errors
    /// Reports incompatible buffers or invalid source tunneling bias.
    pub fn evaluate_batch(
        &self,
        inputs: &[Bias],
        outputs: &mut [Output],
    ) -> Result<(), EvaluationError> {
        evaluate_lanes(inputs.iter().copied(), outputs, |input| {
            self.evaluate(input)
        })
    }
}
