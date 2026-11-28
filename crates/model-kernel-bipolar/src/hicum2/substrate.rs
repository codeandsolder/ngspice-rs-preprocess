//! HICUM/L2 2.4.0 substrate parasitic transistor `HSI_Tsu` and `Qdsu`.
//!
//! Source `Sub_Transfer` has two independent junction exponentials. Transfer
//! between the peripheral base and internal substrate is not the ordinary
//! collector-substrate HICDIO junction. Diffusion Qdsu lies on the Bp-Ci
//! charge branch; the solver binds the separate current and charge topology.

use crate::{
    EvaluationError, ModelError, Temperature, evaluate_lanes, validate_nonnegative,
    validate_positive,
};

const SOURCE_K: f64 = 1.380_650_3e-23;
const SOURCE_Q: f64 = 1.602_176_462e-19;
const EXP_LIMIT: f64 = 80.0;

#[derive(Clone, Copy, Debug)]
pub struct Parameters {
    /// Temperature-adjusted ITSS (A).
    pub saturation: f64,
    /// Substrate transistor forward ideality factor MSF.
    pub ideality: f64,
    /// Temperature-adjusted forward storage time TSF (s).
    pub forward_storage_time: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct Bias {
    pub vbpci: f64,
    pub vsici: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Value2 {
    pub value: f64,
    /// Partial wrt peripheral base-to-internal-collector voltage.
    pub d_vbpci: f64,
    /// Partial wrt substrate-to-internal-collector voltage.
    pub d_vsici: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Output {
    /// `HSI_Tsu` generated substrate transfer current, amperes.
    pub current: Value2,
    /// Qdsu parasitic forward diffusion charge, coulombs.
    pub diffusion_charge: Value2,
}

/// Source substrate dI/dTdev and dQdsu/dTdev at fixed bias.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TemperatureSensitivity {
    pub current: f64,
    pub diffusion_charge: f64,
}

/// Source logarithmic ITSS and TSF temperature slopes, per K.
#[derive(Clone, Copy, Debug, Default)]
pub struct TemperatureSlopes {
    pub saturation: f64,
    pub storage_time: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct Prepared {
    parameters: Parameters,
    inverse_nvt: f64,
    temperature_kelvin: f64,
    temperature_slopes: TemperatureSlopes,
}

impl Prepared {
    /// # Errors
    /// Rejects nonphysical parameters, including invalid effective storage.
    pub fn new(parameters: Parameters, temperature: Temperature) -> Result<Self, ModelError> {
        Self::with_temperature_slopes(parameters, temperature, TemperatureSlopes::default())
    }

    /// Prepare exact source substrate-current and diffusion temperature slopes.
    ///
    /// # Errors
    /// Rejects nonfinite slopes and invalid prepared substrate model.
    pub fn with_temperature_slopes(
        parameters: Parameters,
        temperature: Temperature,
        temperature_slopes: TemperatureSlopes,
    ) -> Result<Self, ModelError> {
        for (name, value) in [
            ("itss_temperature_slope", temperature_slopes.saturation),
            ("tsf_temperature_slope", temperature_slopes.storage_time),
        ] {
            if !value.is_finite() {
                return Err(ModelError::InvalidParameter(name));
            }
        }
        validate_nonnegative("itss", parameters.saturation)?;
        validate_nonnegative("tsf", parameters.forward_storage_time)?;
        validate_positive("msf", parameters.ideality)?;
        let inverse_nvt = SOURCE_Q / (SOURCE_K * temperature.kelvin() * parameters.ideality);
        if !inverse_nvt.is_finite() {
            return Err(ModelError::InvalidParameter("msf"));
        }
        Ok(Self {
            parameters,
            inverse_nvt,
            temperature_kelvin: temperature.kelvin(),
            temperature_slopes,
        })
    }

    /// # Errors
    /// Reports invalid branch voltage or current/charge numerical overflow.
    pub fn evaluate(&self, bias: Bias) -> Result<Output, EvaluationError> {
        if !bias.vbpci.is_finite() || !bias.vsici.is_finite() {
            return Err(EvaluationError::NonFiniteBias);
        }
        if self.parameters.saturation == 0.0 {
            return Ok(Output::default());
        }
        let (forward, d_forward) = limited_exponential(bias.vbpci * self.inverse_nvt)?;
        let (reverse, d_reverse) = limited_exponential(bias.vsici * self.inverse_nvt)?;
        let saturation = self.parameters.saturation;
        let current = Value2 {
            value: saturation * (forward - reverse),
            d_vbpci: saturation * d_forward * self.inverse_nvt,
            d_vsici: -saturation * d_reverse * self.inverse_nvt,
        };
        let storage = self.parameters.forward_storage_time * saturation;
        let diffusion_charge = Value2 {
            value: storage * forward,
            d_vbpci: storage * d_forward * self.inverse_nvt,
            d_vsici: 0.0,
        };
        if ![
            current.value,
            current.d_vbpci,
            current.d_vsici,
            diffusion_charge.value,
            diffusion_charge.d_vbpci,
        ]
        .iter()
        .all(|value| value.is_finite())
        {
            return Err(EvaluationError::NonFiniteResult);
        }
        Ok(Output {
            current,
            diffusion_charge,
        })
    }

    /// Exact dI/dTdev and dQdsu/dTdev at fixed normalized voltages,
    /// including both source limexp current and charge laws.
    ///
    /// # Errors
    /// Rejects combined source temperature derivative overflow.
    pub fn temperature_sensitivity(
        &self,
        bias: Bias,
        operating: Output,
    ) -> Result<TemperatureSensitivity, EvaluationError> {
        let t = self.temperature_kelvin;
        let slopes = self.temperature_slopes;
        let current = slopes.saturation.mul_add(
            operating.current.value,
            (bias.vsici / t).mul_add(
                -operating.current.d_vsici,
                -(bias.vbpci / t) * operating.current.d_vbpci,
            ),
        );
        let charge = (slopes.saturation + slopes.storage_time).mul_add(
            operating.diffusion_charge.value,
            -(bias.vbpci / t) * operating.diffusion_charge.d_vbpci,
        );
        if !current.is_finite() || !charge.is_finite() {
            return Err(EvaluationError::NonFiniteResult);
        }
        Ok(TemperatureSensitivity {
            current,
            diffusion_charge: charge,
        })
    }

    /// All slice lengths are checked before any writes. Lanes run in order;
    /// the first error leaves that lane and the remaining outputs unchanged.
    ///
    /// # Errors
    /// Reports incompatible buffers or invalid substrate branch evaluation.
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

// Source uses limexp, unlike HICDIO which also has negative-bias cutoff.
// Verilog-A limexp is a smooth exponential limiter with exactly continuous
// value/slope at its forward limit; normal bias is mathematically exp(x).
fn limited_exponential(exponent: f64) -> Result<(f64, f64), EvaluationError> {
    let (value, derivative) = if exponent > EXP_LIMIT {
        let limit = EXP_LIMIT.exp();
        ((exponent - EXP_LIMIT).mul_add(limit, limit), limit)
    } else {
        let value = exponent.exp();
        (value, value)
    };
    if !value.is_finite() || !derivative.is_finite() {
        return Err(EvaluationError::NonFiniteResult);
    }
    Ok((value, derivative))
}
