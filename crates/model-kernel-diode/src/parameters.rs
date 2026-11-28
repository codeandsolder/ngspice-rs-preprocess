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
            Self::Unknown(name) => write!(f, "unknown diode parameter: {name}"),
            Self::Unsupported(name) => write!(f, "unsupported diode parameter: {name}"),
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

        /// Accepted case-insensitive `SIMetrix` model fields, including aliases.
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
        let field = match self {
            Self::Level => {
                if value.to_bits() != 1.0_f64.to_bits() {
                    return Err(ParameterError::Unsupported(format!("LEVEL={value}")));
                }
                return Ok(());
            }
            Self::Is => &mut model.transport.saturation_current,
            Self::N => &mut model.transport.emission,
            Self::Isr => &mut model.recombination.saturation_current,
            Self::Nr => &mut model.recombination.emission,
            Self::Cjo => &mut model.depletion.capacitance,
            Self::Vj => &mut model.depletion.potential,
            Self::M => &mut model.depletion.grading,
            Self::Fc => &mut model.depletion.forward_coefficient,
            Self::Tt => &mut model.transit_time,
            Self::Tnom => {
                model.temperature.nominal = value + 273.15;
                return Ok(());
            }
            Self::Tabs => {
                model.temperature.absolute = Some(value + 273.15);
                return Ok(());
            }
            Self::Trel => &mut model.temperature.relative,
            Self::Eg => &mut model.temperature.energy_gap,
            Self::Xti => &mut model.temperature.exponent,
            Self::Rs => &mut model.series.resistance,
            Self::Trs1 => &mut model.series.linear_temperature,
            Self::Trs2 => &mut model.series.quadratic_temperature,
            Self::Kf => &mut model.noise.coefficient,
            Self::Af => &mut model.noise.exponent,
            Self::Ikf => {
                grouped.ikf = Some(value);
                return Ok(());
            }
            Self::Tikf => &mut grouped.tikf,
            Self::Bv => {
                grouped.bv = Some(value);
                return Ok(());
            }
            Self::Ibv => &mut grouped.primary.current,
            Self::Nbv => &mut grouped.primary.emission,
            Self::Ibvl => &mut grouped.low_current.current,
            Self::Nbvl => &mut grouped.low_current.emission,
            Self::Tbv1 => &mut grouped.tbv[0],
            Self::Tbv2 => &mut grouped.tbv[1],
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

/// Compile the already-resolved `LTspice` semiconductor model. Ordered
/// duplicate assignments retain last-value precedence.
///
/// The source dispatcher must select the semiconductor family before
/// calling this function. LEVEL and ideal-only smoothing/limiting fields
/// are not electrical parameters of this family.
///
/// # Errors
/// Unknown or unqualified fields, or nonfinite numeric values.
pub fn compile_ltspice<'a>(
    parameters: impl IntoIterator<Item = (&'a str, f64)>,
) -> Result<super::ltspice_semiconductor::SourceModel, ParameterError> {
    let mut model = super::ltspice_semiconductor::SourceModel::default();
    let mut grouped = Grouped::default();
    for (original, value) in parameters {
        if !value.is_finite() {
            return Err(ModelError::Invalid("source parameter").into());
        }
        let name = original.to_ascii_lowercase();
        match name.as_str() {
            "level" | "ilimit" | "revilimit" | "epsilon" | "revepsilon" | "area" | "vpk"
            | "ipk" | "iave" | "irms" | "diss" => {}
            "perim" => model.sidewall.perimeter = value,
            "isw" => model.sidewall.transport.saturation_current = value,
            "ns" => model.sidewall.transport.emission = value,
            "rsw" => model.sidewall.resistance = value,
            "cjsw" => model.sidewall.capacitance = value,
            "vjsw" => model.sidewall.potential = Some(value),
            "mjsw" => model.sidewall.grading = value,
            "fcs" => model.sidewall.forward_coefficient = Some(value),
            "vp" => model.recovery = value,
            "t_measured" | "t_abs" | "t_rel_global" => {
                return Err(ParameterError::Unknown(original.to_owned()));
            }
            _ => SourceParameter::parse(original)
                .ok_or_else(|| ParameterError::Unknown(original.to_owned()))?
                .apply(&mut model.bulk, &mut grouped, value)?,
        }
    }
    grouped.finish(&mut model.bulk);
    Ok(model)
}
