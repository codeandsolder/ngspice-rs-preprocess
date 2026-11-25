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

// One name table supplies both the source grammar and the typed compiler.
macro_rules! source_parameters {
    ($($variant:ident => $($name:literal)|+),+ $(,)?) => {
        #[derive(Clone, Copy)]
        enum SourceParameter { $($variant),+ }

        /// Accepted case-insensitive SIMetrix model fields, including aliases.
        pub const SOURCE_PARAMETERS: &[&str] = &[$($($name),+),+];

        impl SourceParameter {
            fn parse(name: &str) -> Option<Self> {
                match name.to_ascii_lowercase().as_str() {
                    $($($name)|+ => Some(Self::$variant),)+
                    _ => None,
                }
            }
        }
    };
}

source_parameters! {
    Level => "level",
    Is => "is", N => "n", Isr => "isr", Nr => "nr",
    Cjo => "cjo" | "cj0", Vj => "vj", M => "m", Fc => "fc", Tt => "tt",
    Tnom => "tnom" | "t_measured", Tabs => "t_abs", Trel => "t_rel_global",
    Eg => "eg", Xti => "xti", Rs => "rs", Trs1 => "trs1", Trs2 => "trs2",
    Kf => "kf", Af => "af", Ikf => "ikf", Tikf => "tikf",
    Bv => "bv", Ibv => "ibv", Nbv => "nbv", Ibvl => "ibvl", Nbvl => "nbvl",
    Tbv1 => "tbv1", Tbv2 => "tbv2",
}

impl SourceParameter {
    fn apply(
        self,
        model: &mut Model,
        grouped: &mut Grouped,
        value: f64,
    ) -> Result<(), ParameterError> {
        use SourceParameter::*;
        let field = match self {
            Level => {
                if value.to_bits() != 1.0_f64.to_bits() {
                    return Err(ParameterError::Unsupported(format!("LEVEL={value}")));
                }
                return Ok(());
            }
            Is => &mut model.transport.saturation_current,
            N => &mut model.transport.emission,
            Isr => &mut model.recombination.saturation_current,
            Nr => &mut model.recombination.emission,
            Cjo => &mut model.depletion.capacitance,
            Vj => &mut model.depletion.potential,
            M => &mut model.depletion.grading,
            Fc => &mut model.depletion.forward_coefficient,
            Tt => &mut model.transit_time,
            Tnom => {
                model.temperature.nominal = value + 273.15;
                return Ok(());
            }
            Tabs => {
                model.temperature.absolute = Some(value + 273.15);
                return Ok(());
            }
            Trel => &mut model.temperature.relative,
            Eg => &mut model.temperature.energy_gap,
            Xti => &mut model.temperature.exponent,
            Rs => &mut model.series.resistance,
            Trs1 => &mut model.series.linear_temperature,
            Trs2 => &mut model.series.quadratic_temperature,
            Kf => &mut model.noise.coefficient,
            Af => &mut model.noise.exponent,
            Ikf => {
                grouped.ikf = Some(value);
                return Ok(());
            }
            Tikf => &mut grouped.tikf,
            Bv => {
                grouped.bv = Some(value);
                return Ok(());
            }
            Ibv => &mut grouped.primary.current,
            Nbv => &mut grouped.primary.emission,
            Ibvl => &mut grouped.low_current.current,
            Nbvl => &mut grouped.low_current.emission,
            Tbv1 => &mut grouped.tbv[0],
            Tbv2 => &mut grouped.tbv[1],
        };
        *field = value;
        Ok(())
    }
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
        SourceParameter::parse(original)
            .ok_or_else(|| ParameterError::Unknown(original.to_owned()))?
            .apply(&mut model, &mut grouped, value)?;
    }
    grouped.finish(&mut model);
    Ok(model.prepare()?)
}
