//! Source-traceable native bipolar model laws, independent from the solver.
//!
//! These are **branch subkernels**, not complete transistor evaluators. A
//! circuit solver must not bind them as complete VBIC/HICUM2 devices until the
//! remaining transport, charge, electrothermal and noise laws are implemented.

pub mod hicum2;
pub mod vbic;

/// Fundamental physical constants, exact under the 2019 SI definition.
const BOLTZMANN: f64 = 1.380_649e-23;
const ELEMENTARY_CHARGE: f64 = 1.602_176_634e-19;

/// Fixed-temperature physical context. Temperature-sensitive model parameters
/// must already have been updated for this temperature by family preparation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Temperature {
    kelvin: f64,
    thermal_voltage: f64,
}

impl Temperature {
    /// Prepare the thermal voltage once. Self-heating changes require a new
    /// temperature-dependent preparation, never reuse stale coefficients.
    /// # Errors
    /// Returns an invalid-temperature error for nonpositive or nonfinite Kelvin values.
    pub fn new(kelvin: f64) -> Result<Self, ModelError> {
        if !kelvin.is_finite() || kelvin <= 0.0 {
            return Err(ModelError::InvalidTemperature);
        }
        let thermal_voltage = BOLTZMANN / ELEMENTARY_CHARGE * kelvin;
        if !thermal_voltage.is_finite() {
            return Err(ModelError::InvalidTemperature);
        }
        Ok(Self {
            kelvin,
            thermal_voltage,
        })
    }

    #[must_use = "the returned absolute temperature is an input to physical model laws"]
    pub const fn kelvin(self) -> f64 {
        self.kelvin
    }

    #[must_use = "the returned voltage scales the temperature-dependent exponential laws"]
    pub const fn thermal_voltage(self) -> f64 {
        self.thermal_voltage
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelError {
    InvalidTemperature,
    InvalidParameter(&'static str),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EvaluationError {
    NonFiniteBias,
    NonFiniteResult,
    InvalidPhysicalDomain,
    NonConverged,
    LengthMismatch,
    MissingCoupling(&'static str),
}

/// Current from the first terminal to the second; derivative with respect to
/// the voltage across the same branch. Solver stamping is external.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Branch {
    pub amperes: f64,
    pub siemens: f64,
}

impl Branch {
    const fn validated(amperes: f64, siemens: f64) -> Result<Self, EvaluationError> {
        if amperes.is_finite() && siemens.is_finite() {
            Ok(Self { amperes, siemens })
        } else {
            Err(EvaluationError::NonFiniteResult)
        }
    }
}

fn validate_nonnegative(name: &'static str, x: f64) -> Result<(), ModelError> {
    if x.is_finite() && x >= 0.0 {
        Ok(())
    } else {
        Err(ModelError::InvalidParameter(name))
    }
}

fn validate_positive(name: &'static str, x: f64) -> Result<(), ModelError> {
    if x.is_finite() && x > 0.0 {
        Ok(())
    } else {
        Err(ModelError::InvalidParameter(name))
    }
}

/// No unbounded buffers, heap allocation, or name lookup on evaluation.
/// The caller provides the destination for a homogeneous family batch.
const fn validate_batch_lengths(inputs: usize, outputs: usize) -> Result<(), EvaluationError> {
    if inputs == outputs {
        Ok(())
    } else {
        Err(EvaluationError::LengthMismatch)
    }
}

impl std::fmt::Display for ModelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidTemperature => f.write_str("temperature must be finite and positive"),
            Self::InvalidParameter(name) => write!(f, "invalid bipolar model parameter: {name}"),
        }
    }
}

impl std::error::Error for ModelError {}

impl std::fmt::Display for EvaluationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NonFiniteBias => f.write_str("non-finite bipolar model branch voltage"),
            Self::NonFiniteResult => f.write_str("bipolar branch current or derivative overflow"),
            Self::InvalidPhysicalDomain => {
                f.write_str("bipolar transport base-charge expression outside physical domain")
            }
            Self::NonConverged => {
                f.write_str("bipolar implicit physical charge solve did not converge")
            }
            Self::LengthMismatch => f.write_str("bipolar batch input/output length mismatch"),
            Self::MissingCoupling(name) => {
                write!(f, "bipolar source Jacobian requires {name} coupling")
            }
        }
    }
}

impl std::error::Error for EvaluationError {}
