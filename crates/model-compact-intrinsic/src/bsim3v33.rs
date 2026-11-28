//! Berkeley BSIM3v3.3.0 model preparation and constitutive equations.
//!
//! The physical stages are extracted from the qualified Rust implementation,
//! retaining its typed binning and independent C reference behavior. This
//! module does not allocate solver nodes, perform Newton limiting or own
//! accepted/trial history. Only the implemented physical modes are admitted.
//!
//! Copyright 2004 Regents of the University of California. BSIM3v3.3.0:
//! Min-Chie Jeng, Mansun Chan, Weidong Liu, Xuemei Xi. See package LICENSE.

mod charges;
mod dc;
mod defaults;
mod defaults_data;
mod noise;
mod setup;
mod size_data;
mod temperature;
mod terminal;
pub use terminal::TerminalPoint;

pub use charges::{ChargeEvaluation, OverlapCharges};
pub use dc::{Bias, CvIntermediates, DcEvaluation};
pub use defaults::{ModelDefaults, ModelWarning};
pub use noise::NoiseDensities;
pub use setup::{InstanceSetup, ModelSetup, size_dependent_names};
pub use temperature::TemperatureModel;

use crate::{BoundParameters, ModelError, ModelVariant, ParameterScope, ParameterValue};
use std::sync::Arc;

/// Physical preparation or constitutive evaluation error, without deck
/// location, matrix rows or simulator policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Input(ModelError),
    Domain(String),
    Checks(Vec<Diagnostic>),
    Unsupported {
        message: String,
        reference: &'static str,
    },
    Numerical {
        context: String,
        message: String,
    },
}

impl Error {
    fn unsupported(message: impl Into<String>, reference: &'static str) -> Self {
        Self::Unsupported {
            message: message.into(),
            reference,
        }
    }
}

impl From<ModelError> for Error {
    fn from(error: ModelError) -> Self {
        Self::Input(error)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Input(error) => error.fmt(formatter),
            Self::Domain(message) => formatter.write_str(message),
            Self::Checks(diagnostics) => {
                formatter.write_str("BSIM3 fatal parameter checks:")?;
                for diagnostic in diagnostics.iter().filter(|d| d.fatal) {
                    write!(formatter, " {}", diagnostic.message)?;
                }
                Ok(())
            }
            Self::Unsupported { message, reference } => {
                write!(formatter, "{message} ({reference})")
            }
            Self::Numerical { context, message } => write!(formatter, "{context}: {message}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// Nonfatal/fatal physical-model parameter diagnostic, in source check order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub fatal: bool,
    pub message: String,
    pub c_reference: &'static str,
}

impl Diagnostic {
    fn warning(message: impl Into<String>, c_reference: &'static str) -> Self {
        Self {
            fatal: false,
            message: message.into(),
            c_reference,
        }
    }
    fn fatal(message: impl Into<String>, c_reference: &'static str) -> Self {
        Self {
            fatal: true,
            message: message.into(),
            c_reference,
        }
    }
}

/// Resolve native instance setters in source order.
///
/// Expand IC vectors in their source
/// order. Scalar components following an IC vector override it, and later
/// vectors replace only the components that they actually specify.
/// Dimensions passed here have already undergone the deck's SCALE transform.
///
/// # Errors
/// Invalid input types, IC vector arity or nonfinite instance inputs.
pub fn bind_instance<'a>(
    inputs: impl IntoIterator<Item = (&'a str, ParameterValue)>,
) -> Result<BoundParameters> {
    let mut expanded = Vec::new();
    for (name, value) in inputs {
        if name.eq_ignore_ascii_case("ic") {
            let ParameterValue::RealVector(values) = value else {
                return Err(ModelError::WrongType {
                    name: name.into(),
                    expected: crate::ParameterType::RealVector,
                }
                .into());
            };
            if !(1..=3).contains(&values.len()) {
                return Err(ModelError::InvalidInitialConditions(values.len()).into());
            }
            expanded.extend(
                ["icvds", "icvgs", "icvbs"]
                    .into_iter()
                    .zip(values)
                    .map(|(key, value)| (key, ParameterValue::Real(value))),
            );
        } else {
            expanded.push((name, value));
        }
    }
    Ok(BoundParameters::bind(
        ModelVariant::Bsim3V33,
        ParameterScope::Instance,
        expanded,
    )?)
}

/// Immutable occurrence at a particular operating temperature. Model and
/// temperature tables are shared; the size-dependent state is occurrence-local.
#[derive(Clone, Debug, PartialEq)]
pub struct PreparedInstance {
    model: Arc<ModelDefaults>,
    temperature: Arc<TemperatureModel>,
    setup: InstanceSetup,
}

/// One consistent DC and charge evaluation of the same intrinsic bias.
#[derive(Clone, Debug, PartialEq)]
pub struct Point {
    pub dc: DcEvaluation,
    pub charge: ChargeEvaluation,
}

impl PreparedInstance {
    #[must_use = "the prepared geometry and diagnostics describe this occurrence"]
    pub const fn setup(&self) -> &InstanceSetup {
        &self.setup
    }

    /// Evaluate the native physical current and charge equations. Voltages
    /// are intrinsic, source-referenced and normalized by model polarity.
    /// Multiplicity and solver junction regularization remain explicit.
    ///
    /// # Errors
    /// Nonfinite input/output, or unsupported charge/NQS mode.
    pub fn evaluate(&self, bias: Bias, gmin: f64) -> Result<Point> {
        if ![bias.vbs, bias.vgs, bias.vds, gmin]
            .into_iter()
            .all(f64::is_finite)
            || gmin < 0.
        {
            return Err(Error::Domain(
                "invalid BSIM3 bias or junction regularization".into(),
            ));
        }
        let dc = dc::DcModel {
            model: &self.model,
            temperature: &self.temperature,
            setup: &self.setup,
        }
        .evaluate(bias, gmin)?;
        let charge = charges::ChargeModel {
            model: &self.model,
            temperature: &self.temperature,
            setup: &self.setup,
        }
        .evaluate(&dc, bias)?;
        Ok(Point { dc, charge })
    }

    /// Evaluate native noise generators consistently with the same bias and
    /// physical temperature used for the current/charge point.
    ///
    /// # Errors
    /// Unsupported noise law or nonfinite physical result.
    pub fn noise(&self, bias: Bias, gmin: f64) -> Result<NoiseDensities> {
        let point = self.evaluate(bias, gmin)?;
        noise::NoiseModel {
            model: &self.model,
            setup: &self.setup,
        }
        .densities(&point.dc, &point.charge, self.temperature.temp)
    }
}
