//! Source parameter compilation, separate from the physical evaluation.
use super::{Breakdown, BreakdownTerm, HighInjection, Model, ModelError, PreparedModel};

/// A source parameter that could not be compiled.
#[derive(Debug, PartialEq, Eq)]
pub enum ParameterError {
    /// Unrecognized field.
    Unknown(String),
    /// Recognized field outside level-1 support.
    Unsupported(String),
    /// Invalid typed model.
    Invalid(ModelError),
}

impl std::fmt::Display for ParameterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown(name) => write!(f, "unknown SIMetrix diode parameter: {name}"),
            Self::Unsupported(name) => write!(f, "unsupported SIMetrix diode parameter: {name}"),
            Self::Invalid(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for ParameterError {}

impl From<ModelError> for ParameterError {
    fn from(value: ModelError) -> Self {
        Self::Invalid(value)
    }
}

struct Grouped {
    ikf: Option<f64>,
    tikf: f64,
    bv: Option<f64>,
    primary: BreakdownTerm,
    low_current: BreakdownTerm,
    tbv: [f64; 2],
}

impl Default for Grouped {
    fn default() -> Self {
        Self {
            ikf: None,
            tikf: 0.0,
            bv: None,
            primary: BreakdownTerm {
                current: 1e-10,
                emission: 1.0,
            },
            low_current: BreakdownTerm {
                current: 0.0,
                emission: 1.0,
            },
            tbv: [0.0; 2],
        }
    }
}

impl Grouped {
    fn set(&mut self, name: &str, value: f64) -> bool {
        let field = match name {
            "ikf" => {
                self.ikf = Some(value);
                return true;
            }
            "bv" => {
                self.bv = Some(value);
                return true;
            }
            "tikf" => &mut self.tikf,
            "ibv" => &mut self.primary.current,
            "nbv" => &mut self.primary.emission,
            "ibvl" => &mut self.low_current.current,
            "nbvl" => &mut self.low_current.emission,
            "tbv1" => &mut self.tbv[0],
            "tbv2" => &mut self.tbv[1],
            _ => return false,
        };
        *field = value;
        true
    }

    fn finish(self, model: &mut Model) {
        model.high_injection =
            self.ikf
                .filter(|current| *current != 0.0)
                .map(|current| HighInjection {
                    current,
                    temperature_coefficient: self.tikf,
                });
        model.breakdown = self
            .bv
            .filter(|voltage| *voltage != 0.0)
            .map(|voltage| Breakdown {
                voltage,
                primary: self.primary,
                low_current: self.low_current,
                linear_temperature: self.tbv[0],
                quadratic_temperature: self.tbv[1],
            });
    }
}

fn set_junction(model: &mut Model, name: &str, value: f64) -> bool {
    let field = match name {
        "is" => &mut model.transport.saturation_current,
        "n" => &mut model.transport.emission,
        "isr" => &mut model.recombination.saturation_current,
        "nr" => &mut model.recombination.emission,
        "cjo" | "cj0" => &mut model.depletion.capacitance,
        "vj" => &mut model.depletion.potential,
        "m" => &mut model.depletion.grading,
        "fc" => &mut model.depletion.forward_coefficient,
        "tt" => &mut model.transit_time,
        _ => return false,
    };
    *field = value;
    true
}

fn set_temperature_and_series(model: &mut Model, name: &str, value: f64) -> bool {
    let field = match name {
        "tnom" | "t_measured" => {
            model.temperature.nominal = value + 273.15;
            return true;
        }
        "t_abs" => {
            model.temperature.absolute = Some(value + 273.15);
            return true;
        }
        "t_rel_global" => &mut model.temperature.relative,
        "eg" => &mut model.temperature.energy_gap,
        "xti" => &mut model.temperature.exponent,
        "rs" => &mut model.series.resistance,
        "trs1" => &mut model.series.linear_temperature,
        "trs2" => &mut model.series.quadratic_temperature,
        "kf" => &mut model.noise.coefficient,
        "af" => &mut model.noise.exponent,
        _ => return false,
    };
    *field = value;
    true
}

/// Compile already-resolved `SIMetrix` level-1 numeric model parameters.
///
/// Names are case-insensitive; temperatures in source cards are Celsius.
/// The caller resolves source expressions before compilation. Unknown
/// and other-level fields are not silently ignored.
///
/// # Errors
/// Rejects unsupported level, unknown parameters and invalid domains.
pub fn compile_simetrix<'a>(
    parameters: impl IntoIterator<Item = (&'a str, f64)>,
) -> Result<PreparedModel, ParameterError> {
    let mut model = Model::default();
    let mut grouped = Grouped::default();
    for (original, value) in parameters {
        if !value.is_finite() {
            return Err(ModelError::Invalid("source parameter").into());
        }
        let name = original.to_ascii_lowercase();
        if name == "level" {
            if value.to_bits() != 1.0_f64.to_bits() {
                return Err(ParameterError::Unsupported(format!("LEVEL={value}")));
            }
        } else if !(grouped.set(&name, value)
            || set_junction(&mut model, &name, value)
            || set_temperature_and_series(&mut model, &name, value))
        {
            return Err(ParameterError::Unknown(original.to_owned()));
        }
    }
    grouped.finish(&mut model);
    Ok(model.prepare()?)
}
