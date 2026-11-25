//! HICUM/L2 2.4.0 base/collector/substrate diode laws.
//!
//! Follows the HICDIO Verilog-A macro, *including* its asymmetric negative
//! cutoff and large-forward-bias continuation. No Gummel-Poon substitution.
//! Parameters are already updated for instance temperature by the caller.

pub mod artifact;
pub mod avalanche;
pub mod binding;
pub mod device;
pub(super) mod differentiation;
pub mod external;
pub mod instance;
pub mod internal_base;
pub mod junction;
pub mod nodal;
pub mod noise;
pub mod nqs;
pub mod parameters;
pub mod source_defaults;
pub mod stamps;
pub mod substrate;
pub mod temperature;
pub mod thermal;
pub mod topology;
pub mod transport;
pub mod tunneling;
use crate::{
    Branch, EvaluationError, ModelError, Temperature, validate_batch_lengths, validate_nonnegative,
    validate_positive,
};

/// Source TYPE: terminal voltage orientation and output current sign.
/// The physical kernels always consume polarity-normalized B-E/B-C
/// biases; the circuit binder applies NPN or PNP signs and node mapping.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Polarity {
    #[default]
    Npn,
    Pnp,
}

const FORWARD_LIMIT: f64 = 80.0;
const REVERSE_LIMIT: f64 = -14.0;

#[derive(Clone, Copy, Debug)]
pub struct Parameters {
    pub ibeis: f64,
    pub mbei: f64,
    pub ireis: f64,
    pub mrei: f64,
    pub ibeps: f64,
    pub mbep: f64,
    pub ireps: f64,
    pub mrep: f64,
    pub ibcis: f64,
    pub mbci: f64,
    pub ibcxs: f64,
    pub mbcx: f64,
    pub iscs: f64,
    pub msc: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct Bias {
    pub vbiei: f64,
    pub vbpei: f64,
    pub vbici: f64,
    pub vbpci: f64,
    pub vsici: f64,
}

/// Complete source HICDIO diode current temperature derivatives, A/K.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CurrentSensitivity {
    pub ibei: f64,
    pub irei: f64,
    pub ibep: f64,
    pub irep: f64,
    pub ibci: f64,
    pub ijbcx: f64,
    pub ijsc: f64,
}

impl CurrentSensitivity {
    /// Exact HICUM FLSH=2 diode power slope (W/K) at fixed local voltages.
    /// The published source power includes IBEI, IBCI, IBEP, IJBCX,
    /// and IJSC, but *not* the IREI/IREP recombination currents.
    /// Other source heat contributors remain independent.
    #[must_use]
    pub fn flsh2_diode_power_slope(self, bias: Bias) -> f64 {
        self.ibei.mul_add(
            bias.vbiei,
            self.ibci.mul_add(
                bias.vbici,
                self.ibep.mul_add(
                    bias.vbpei,
                    self.ijbcx.mul_add(bias.vbpci, self.ijsc * bias.vsici),
                ),
            ),
        )
    }
}

/// Logarithmic source saturation-current slopes dln(Is)/dTdev (1/K).
#[derive(Clone, Copy, Debug, Default)]
pub struct SaturationSlopes {
    pub ibei: f64,
    pub irei: f64,
    pub ibep: f64,
    pub irep: f64,
    pub ibci: f64,
    pub ijbcx: f64,
    pub ijsc: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Currents {
    pub ibei: Branch,
    pub irei: Branch,
    pub ibep: Branch,
    pub irep: Branch,
    pub ibci: Branch,
    pub ijbcx: Branch,
    pub ijsc: Branch,
}

#[derive(Clone, Copy, Debug)]
pub struct Prepared {
    parameters: Parameters,
    vt: f64,
    temperature_kelvin: f64,
    saturation_slopes: SaturationSlopes,
}

impl Prepared {
    /// # Errors
    /// Returns the offending model parameter on nonfinite or invalid values.
    pub fn new(parameters: Parameters, temperature: Temperature) -> Result<Self, ModelError> {
        Self::with_temperature_slopes(parameters, temperature, SaturationSlopes::default())
    }

    /// Prepare HICDIO source saturation-current thermal sensitivity.
    ///
    /// # Errors
    /// Rejects nonfinite slopes and invalid prepared diode parameters.
    pub fn with_temperature_slopes(
        parameters: Parameters,
        temperature: Temperature,
        saturation_slopes: SaturationSlopes,
    ) -> Result<Self, ModelError> {
        for (name, value) in [
            ("ibei_temperature_slope", saturation_slopes.ibei),
            ("irei_temperature_slope", saturation_slopes.irei),
            ("ibep_temperature_slope", saturation_slopes.ibep),
            ("irep_temperature_slope", saturation_slopes.irep),
            ("ibci_temperature_slope", saturation_slopes.ibci),
            ("ijbcx_temperature_slope", saturation_slopes.ijbcx),
            ("ijsc_temperature_slope", saturation_slopes.ijsc),
        ] {
            if !value.is_finite() {
                return Err(ModelError::InvalidParameter(name));
            }
        }
        for (name, value) in [
            ("ibeis", parameters.ibeis),
            ("ireis", parameters.ireis),
            ("ibeps", parameters.ibeps),
            ("ireps", parameters.ireps),
            ("ibcis", parameters.ibcis),
            ("ibcxs", parameters.ibcxs),
            ("iscs", parameters.iscs),
        ] {
            validate_nonnegative(name, value)?;
        }
        for (name, value) in [
            ("mbei", parameters.mbei),
            ("mrei", parameters.mrei),
            ("mbep", parameters.mbep),
            ("mrep", parameters.mrep),
            ("mbci", parameters.mbci),
            ("mbcx", parameters.mbcx),
            ("msc", parameters.msc),
        ] {
            validate_positive(name, value)?;
        }
        Ok(Self {
            parameters,
            vt: (1.380_650_3e-23 / 1.602_176_462e-19) * temperature.kelvin(),
            temperature_kelvin: temperature.kelvin(),
            saturation_slopes,
        })
    }

    /// # Errors
    /// Returns a numerical error for nonfinite bias or current/derivative.
    pub fn evaluate(&self, bias: Bias) -> Result<Currents, EvaluationError> {
        if ![bias.vbiei, bias.vbpei, bias.vbici, bias.vbpci, bias.vsici]
            .iter()
            .all(|value| value.is_finite())
        {
            return Err(EvaluationError::NonFiniteBias);
        }
        let p = &self.parameters;
        Ok(Currents {
            ibei: hicdio(p.ibeis, p.mbei, self.vt, bias.vbiei)?,
            irei: hicdio(p.ireis, p.mrei, self.vt, bias.vbiei)?,
            ibep: hicdio(p.ibeps, p.mbep, self.vt, bias.vbpei)?,
            irep: hicdio(p.ireps, p.mrep, self.vt, bias.vbpei)?,
            ibci: hicdio(p.ibcis, p.mbci, self.vt, bias.vbici)?,
            ijbcx: hicdio(p.ibcxs, p.mbcx, self.vt, bias.vbpci)?,
            ijsc: hicdio(p.iscs, p.msc, self.vt, bias.vsici)?,
        })
    }

    /// Exact dI/dTdev for all seven HICDIO branches at fixed normalized
    /// voltage. Is contributes I*dln(Is)/dTdev; VT contributes
    /// -(V/Tdev)*dI/dV even in source forward-limited/reverse-cutoff laws.
    ///
    /// # Errors
    /// Rejects finite-precision derivative overflow.
    pub fn temperature_sensitivity(
        &self,
        bias: Bias,
        operating: Currents,
    ) -> Result<CurrentSensitivity, EvaluationError> {
        let slopes = self.saturation_slopes;
        let t = self.temperature_kelvin;
        let diode = |v: f64, i: Branch, s: f64| s.mul_add(i.amperes, -(v / t) * i.siemens);
        let result = CurrentSensitivity {
            ibei: diode(bias.vbiei, operating.ibei, slopes.ibei),
            irei: diode(bias.vbiei, operating.irei, slopes.irei),
            ibep: diode(bias.vbpei, operating.ibep, slopes.ibep),
            irep: diode(bias.vbpei, operating.irep, slopes.irep),
            ibci: diode(bias.vbici, operating.ibci, slopes.ibci),
            ijbcx: diode(bias.vbpci, operating.ijbcx, slopes.ijbcx),
            ijsc: diode(bias.vsici, operating.ijsc, slopes.ijsc),
        };
        if ![
            result.ibei,
            result.irei,
            result.ibep,
            result.irep,
            result.ibci,
            result.ijbcx,
            result.ijsc,
        ]
        .iter()
        .all(|v| v.is_finite())
        {
            return Err(EvaluationError::NonFiniteResult);
        }
        Ok(result)
    }

    /// # Errors
    /// Returns a length error for incompatible buffers or an evaluation error.
    pub fn evaluate_batch(
        &self,
        inputs: &[Bias],
        outputs: &mut [Currents],
    ) -> Result<(), EvaluationError> {
        validate_batch_lengths(inputs.len(), outputs.len())?;
        for (input, output) in inputs.iter().zip(outputs) {
            *output = self.evaluate(*input)?;
        }
        Ok(())
    }
}

fn hicdio(
    saturation: f64,
    ideality: f64,
    vt: f64,
    voltage: f64,
) -> Result<Branch, EvaluationError> {
    if saturation == 0.0 {
        return Ok(Branch::default());
    }
    let exponent = voltage / (ideality * vt);
    if exponent <= REVERSE_LIMIT {
        return Ok(Branch {
            amperes: -saturation,
            siemens: 0.0,
        });
    }
    let (exp_value, exp_derivative) = if exponent > FORWARD_LIMIT {
        let max_exp = FORWARD_LIMIT.exp();
        ((1.0 + exponent - FORWARD_LIMIT) * max_exp, max_exp)
    } else {
        let value = exponent.exp();
        (value, value)
    };
    Branch::validated(
        saturation * (exp_value - 1.0),
        saturation * exp_derivative / (ideality * vt),
    )
}
