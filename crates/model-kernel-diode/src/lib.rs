//! Independently characterized `SIMetrix` level-1 diode physical kernel.
//!
//! Model preparation and temperature/geometry binding precede Newton
//! evaluation. The caller owns the optional series-resistor node, voltage
//! limiting, MNA stamping, accepted/trial history and charge integration.
//! The intrinsic kernel returns signed current and stored charge with their
//! exact voltage derivatives. Units are SI, with temperature in kelvin.

pub mod artifact;
mod parameters;
mod physics;

pub use parameters::{ParameterError, SOURCE_PARAMETERS, compile_simetrix};

/// Explicit model family used by prepared source and the artifact descriptor.
pub const MODEL_FAMILY: &str = "simetrix_diode_level1";
pub use physics::{Evaluation, NoiseDensity, PreparedInstance, evaluate_batch};

/// Historical constants measured in `SIMetrix` 9.40d.
pub const BOLTZMANN: f64 = 1.380_622_6e-23;
/// Historical electron charge, coulombs.
pub const ELEMENTARY_CHARGE: f64 = 1.602_191_8e-19;
/// Thermal voltage per kelvin.
pub const K_OVER_Q: f64 = BOLTZMANN / ELEMENTARY_CHARGE;

/// Transport or recombination junction parameters.
#[derive(Clone, Copy, Debug)]
pub struct Junction {
    /// Saturation current at the measurement temperature, A.
    pub saturation_current: f64,
    /// Emission coefficient.
    pub emission: f64,
}

/// Positive transport high-injection correction.
#[derive(Clone, Copy, Debug)]
pub struct HighInjection {
    /// Knee current at the measurement temperature, A.
    pub current: f64,
    /// Relative linear temperature coefficient, 1/K.
    pub temperature_coefficient: f64,
}

/// One exponential breakdown component.
#[derive(Clone, Copy, Debug)]
pub struct BreakdownTerm {
    /// Current at the breakdown voltage, A.
    pub current: f64,
    /// Breakdown emission coefficient.
    pub emission: f64,
}

/// Reverse avalanche voltage and additive exponential components.
#[derive(Clone, Copy, Debug)]
pub struct Breakdown {
    /// Positive breakdown voltage at the measurement temperature, V.
    pub voltage: f64,
    /// Primary avalanche component.
    pub primary: BreakdownTerm,
    /// Optional low-current avalanche component.
    pub low_current: BreakdownTerm,
    /// Linear relative temperature coefficient of breakdown voltage, 1/K.
    pub linear_temperature: f64,
    /// Quadratic relative temperature coefficient, 1/K².
    pub quadratic_temperature: f64,
}

/// Depletion-layer charge and recombination grading parameters.
#[derive(Clone, Copy, Debug)]
pub struct Depletion {
    /// Zero-bias capacitance per unit area, F.
    pub capacitance: f64,
    /// Junction potential at the measurement temperature, V.
    pub potential: f64,
    /// Grading exponent.
    pub grading: f64,
    /// Forward continuation threshold as a fraction of potential.
    pub forward_coefficient: f64,
}

/// Ohmic series path before the intrinsic diode junction.
#[derive(Clone, Copy, Debug)]
pub struct Series {
    /// Series resistance per unit area at measurement temperature, ohm.
    pub resistance: f64,
    /// Linear relative temperature coefficient, 1/K.
    pub linear_temperature: f64,
    /// Quadratic relative temperature coefficient, 1/K².
    pub quadratic_temperature: f64,
}

/// Source-dialect model temperature parameters.
#[derive(Clone, Copy, Debug)]
pub struct Temperature {
    /// Measurement temperature, K.
    pub nominal: f64,
    /// Absolute model temperature override, K.
    pub absolute: Option<f64>,
    /// Offset from global analysis temperature, K.
    pub relative: f64,
    /// Saturation-current activation energy, eV.
    pub energy_gap: f64,
    /// Saturation-current temperature exponent.
    pub exponent: f64,
}

/// Source flicker parameters, retained even for non-noise analyses.
#[derive(Clone, Copy, Debug)]
pub struct Noise {
    /// Flicker coefficient KF.
    pub coefficient: f64,
    /// Flicker exponent AF.
    pub exponent: f64,
}

/// Typed level-1 diode model before temperature/instance binding.
#[derive(Clone, Copy, Debug)]
pub struct Model {
    /// Main transport junction.
    pub transport: Junction,
    /// Recombination current junction.
    pub recombination: Junction,
    /// Optional high-injection correction.
    pub high_injection: Option<HighInjection>,
    /// Depletion charge law.
    pub depletion: Depletion,
    /// Optional avalanche junction.
    pub breakdown: Option<Breakdown>,
    /// External ohmic series resistance.
    pub series: Series,
    /// Source model temperature policy.
    pub temperature: Temperature,
    /// Diffusion transit time, s.
    pub transit_time: f64,
    /// Flicker model parameters.
    pub noise: Noise,
}

impl Default for Model {
    fn default() -> Self {
        Self {
            transport: Junction {
                saturation_current: 1e-14,
                emission: 1.0,
            },
            recombination: Junction {
                saturation_current: 0.0,
                emission: 2.0,
            },
            high_injection: None,
            depletion: Depletion {
                capacitance: 0.0,
                potential: 1.0,
                grading: 0.5,
                forward_coefficient: 0.5,
            },
            breakdown: None,
            series: Series {
                resistance: 0.0,
                linear_temperature: 0.0,
                quadratic_temperature: 0.0,
            },
            temperature: Temperature {
                nominal: 300.15,
                absolute: None,
                relative: 0.0,
                energy_gap: 1.11,
                exponent: 3.0,
            },
            transit_time: 0.0,
            noise: Noise {
                coefficient: 0.0,
                exponent: 1.0,
            },
        }
    }
}

/// Physical geometry and temperature inputs for one instance.
#[derive(Clone, Copy, Debug)]
pub struct Instance {
    /// Global analysis temperature, K.
    pub analysis_temperature: f64,
    /// Explicit instance TEMP, K.
    pub absolute_temperature: Option<f64>,
    /// Instance DTEMP, K.
    pub relative_temperature: f64,
    /// Junction area multiplier, dimensionless.
    pub area: f64,
    /// Number of parallel instances, dimensionless.
    pub multiplier: f64,
    /// Analysis minimum junction conductance, S.
    pub gmin: f64,
}

impl Default for Instance {
    fn default() -> Self {
        Self {
            analysis_temperature: 300.15,
            absolute_temperature: None,
            relative_temperature: 0.0,
            area: 1.0,
            multiplier: 1.0,
            gmin: 1e-12,
        }
    }
}

/// Immutable, validated source model shared by independent instances.
#[derive(Clone, Copy, Debug)]
pub struct PreparedModel {
    model: Model,
}

pub use model_kernel_api::ModelError;

fn positive(name: &'static str, value: f64) -> Result<(), ModelError> {
    if value.is_finite() && value > 0.0 {
        Ok(())
    } else {
        Err(ModelError::Invalid(name))
    }
}

fn nonnegative(name: &'static str, value: f64) -> Result<(), ModelError> {
    if value.is_finite() && value >= 0.0 {
        Ok(())
    } else {
        Err(ModelError::Invalid(name))
    }
}

fn finite(name: &'static str, values: &[f64]) -> Result<(), ModelError> {
    if values.iter().all(|value| value.is_finite()) {
        Ok(())
    } else {
        Err(ModelError::Invalid(name))
    }
}

impl Model {
    /// Validate the source model before any circuit or instance evaluation.
    ///
    /// # Errors
    /// Invalid domains or nonfinite coefficients are rejected.
    pub fn prepare(self) -> Result<PreparedModel, ModelError> {
        nonnegative("IS", self.transport.saturation_current)?;
        positive("N", self.transport.emission)?;
        nonnegative("ISR", self.recombination.saturation_current)?;
        positive("NR", self.recombination.emission)?;
        if let Some(knee) = self.high_injection {
            positive("IKF", knee.current)?;
            finite("TIKF", &[knee.temperature_coefficient])?;
        }
        nonnegative("CJO", self.depletion.capacitance)?;
        positive("VJ", self.depletion.potential)?;
        finite("M", &[self.depletion.grading])?;
        if !(0.0..1.0).contains(&self.depletion.forward_coefficient) {
            return Err(ModelError::Invalid("FC"));
        }
        if let Some(breakdown) = self.breakdown {
            positive("BV", breakdown.voltage)?;
            nonnegative("IBV", breakdown.primary.current)?;
            positive("NBV", breakdown.primary.emission)?;
            nonnegative("IBVL", breakdown.low_current.current)?;
            positive("NBVL", breakdown.low_current.emission)?;
            finite(
                "TBV",
                &[
                    breakdown.linear_temperature,
                    breakdown.quadratic_temperature,
                ],
            )?;
        }
        finite("RS", &[self.series.resistance])?;
        finite(
            "TRS",
            &[
                self.series.linear_temperature,
                self.series.quadratic_temperature,
            ],
        )?;
        positive("TNOM", self.temperature.nominal)?;
        if let Some(absolute) = self.temperature.absolute {
            positive("T_ABS", absolute)?;
        }
        finite(
            "temperature coefficients",
            &[
                self.temperature.relative,
                self.temperature.energy_gap,
                self.temperature.exponent,
            ],
        )?;
        nonnegative("TT", self.transit_time)?;
        nonnegative("KF", self.noise.coefficient)?;
        finite("AF", &[self.noise.exponent])?;
        Ok(PreparedModel { model: self })
    }
}

impl PreparedModel {
    /// Source parameters, available to the setup layer.
    #[must_use = "the returned source parameters are needed for subsequent setup"]
    pub const fn parameters(&self) -> &Model {
        &self.model
    }

    /// Bind geometry and effective temperature, deriving immutable data.
    ///
    /// # Errors
    /// Nonphysical instance inputs or temperature-dependent parameters fail.
    pub fn bind(&self, instance: Instance) -> Result<PreparedInstance, ModelError> {
        PreparedInstance::new(&self.model, instance)
    }
}
