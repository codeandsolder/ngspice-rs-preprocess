//! Prepared Berkeley BSIM3 junction conduction and depletion storage.
//!
//! The 3.2.4 and 3.3.0 source revisions use the exponential junction
//! and forward continuation at IJTH, but differ in their knee geometry. The
//! solver supplies limiting, regularizing conductance and polarity/multiplicity;
//! they are not physical parameters.

use super::{
    Bsim3JunctionTemperature, MosDepletionComponent, MosDepletionTemperatureCoefficients,
    MosDiffusionRule, MosJunctionDepletion, MosJunctionOccurrence, MosJunctionPoint,
    MosJunctionSaturation, MosSaturationFallback, MosSeriesResistanceRule, MosSidewallRule, finite,
    nonnegative, positive, prepare_mos_diffusion,
};
use crate::ModelError;

#[derive(Clone, Copy, Debug, PartialEq)]
struct ForwardKnee {
    voltage_v: f64,
    current_a: f64,
    conductance_s: f64,
}

/// Berkeley BSIM3 conduction with an optional forward-current knee.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bsim3BerkeleyJunction {
    saturation_current_a: f64,
    emission_thermal_voltage_v: f64,
    knee: Option<ForwardKnee>,
}

impl Bsim3BerkeleyJunction {
    /// Prepare a single-device current law. Zero IJTH disables the knee.
    /// A distinct reference saturation current reproduces the native 3.3
    /// explicit-geometry knee, including its non-tangent discontinuity.
    ///
    /// # Errors
    /// Invalid physical parameters or nonrepresentable knee coefficients.
    pub fn new(
        saturation_current_a: f64,
        emission_thermal_voltage_v: f64,
        limiting_current_a: f64,
        reference_saturation_a: f64,
    ) -> Result<Self, ModelError> {
        let saturation = nonnegative(saturation_current_a, "BSIM3 saturation current")?;
        let thermal = positive(emission_thermal_voltage_v, "BSIM3 emission thermal voltage")?;
        let limit = nonnegative(limiting_current_a, "BSIM3 forward limiting current")?;
        let knee = if saturation > 0. && limit > 0. {
            let reference = positive(
                reference_saturation_a,
                "BSIM3 knee reference saturation current",
            )?;
            let ratio = limit / reference;
            let log = if ratio.is_finite() {
                ratio.ln_1p()
            } else {
                limit.ln() - reference.ln()
            };
            Some(ForwardKnee {
                voltage_v: nonnegative(thermal * log, "BSIM3 forward knee voltage")?,
                current_a: finite(
                    limit + (reference - saturation),
                    "BSIM3 forward knee current",
                )?,
                conductance_s: nonnegative(
                    (limit + reference) / thermal,
                    "BSIM3 forward knee conductance",
                )?,
            })
        } else {
            None
        };
        Ok(Self {
            saturation_current_a: saturation,
            emission_thermal_voltage_v: thermal,
            knee,
        })
    }

    fn conduction(self, voltage_v: f64) -> (f64, f64) {
        let saturation = self.saturation_current_a;
        if saturation == 0. {
            return (0., 0.);
        }
        if let Some(knee) = self.knee
            && voltage_v >= knee.voltage_v
        {
            return (
                knee.conductance_s
                    .mul_add(voltage_v - knee.voltage_v, knee.current_a),
                knee.conductance_s,
            );
        }
        let exponent = voltage_v / self.emission_thermal_voltage_v;
        let exponential = exponent.exp();
        // Avoid both forward overflow for tiny Is and cancellation of reverse
        // conductance in the limit I -> -Is.
        let weighted = if exponential.is_finite() && exponential > 0. {
            saturation * exponential
        } else {
            (saturation.ln() + exponent).exp()
        };
        let current = if exponent.abs() < 0.5 {
            saturation * exponent.exp_m1()
        } else {
            weighted - saturation
        };
        (current, weighted / self.emission_thermal_voltage_v)
    }

    /// Evaluate conduction and the supplied depletion contributions. No diffusion
    /// transit-time charge is part of the Berkeley BSIM3 junction model.
    ///
    /// # Errors
    /// Nonfinite voltage or nonrepresentable outputs.
    pub fn evaluate(
        self,
        voltage_v: f64,
        depletion: &[MosDepletionComponent],
    ) -> Result<MosJunctionPoint, ModelError> {
        finite(voltage_v, "BSIM3 junction voltage")?;
        let (current, conductance) = self.conduction(voltage_v);
        let mut charge = 0.;
        let mut capacitance = 0.;
        for component in depletion {
            let point = component.evaluate(voltage_v)?;
            charge += point.charge_c;
            capacitance += point.capacitance_f;
        }
        Ok(MosJunctionPoint {
            current_a: finite(current, "BSIM3 junction current")?,
            conductance_s: nonnegative(conductance, "BSIM3 junction conductance")?,
            charge_c: finite(charge, "BSIM3 junction charge")?,
            capacitance_f: nonnegative(capacitance, "BSIM3 junction capacitance")?,
        })
    }
}

/// Geometry used to determine the forward-current continuation threshold.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Bsim3KneeReference {
    /// Use the actual saturation current after the selected geometry law.
    Resolved,
    /// Native 3.3 computes its knee from raw explicit areas/perimeters,
    /// without ACM generation/scaling, unlike its load-time current law.
    RawExplicit,
}

/// Independent physical geometry, sidewall and series-resistance rules.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Bsim3JunctionRules {
    pub diffusion: MosDiffusionRule,
    pub sidewall: MosSidewallRule,
    pub resistance: MosSeriesResistanceRule,
    pub knee_reference: Bsim3KneeReference,
}

/// Model-wide nominal coefficients, independent of occurrence geometry.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bsim3JunctionModel {
    pub saturation: MosJunctionSaturation,
    pub depletion: MosJunctionDepletion,
    pub depletion_temperature: [MosDepletionTemperatureCoefficients; 3],
    pub nominal_temperature_k: f64,
    pub emission_coefficient: f64,
    pub saturation_temperature_exponent: f64,
    pub limiting_current_a: f64,
}

/// Immutable per-terminal physical coefficients, before polarity and multiplicity.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreparedBsim3Junction {
    conduction: Bsim3BerkeleyJunction,
    depletion: [MosDepletionComponent; 3],
    resistance_ohm: f64,
}

impl Bsim3JunctionModel {
    /// Bind one physical terminal at its operating temperature. Junction area
    /// and parasitic resistance may use different geometry policies.
    ///
    /// # Errors
    /// Invalid geometry, temperature, density or physical coefficients.
    pub fn prepare(
        self,
        rules: Bsim3JunctionRules,
        occurrence: MosJunctionOccurrence,
    ) -> Result<PreparedBsim3Junction, ModelError> {
        let temperature = Bsim3JunctionTemperature::new(
            self.nominal_temperature_k,
            occurrence.operating_temperature_k,
            self.emission_coefficient,
            self.saturation_temperature_exponent,
        )?;
        let geometry =
            prepare_mos_diffusion(rules.diffusion, occurrence.dimensions, occurrence.terminal)?;
        let densities = temperature.saturation(self.saturation)?;
        let saturation = densities.prepare(geometry)?;
        let reference = if rules.knee_reference == Bsim3KneeReference::RawExplicit
            && self.limiting_current_a > 0.
        {
            let raw = prepare_mos_diffusion(
                MosDiffusionRule::ExplicitPhysical,
                occurrence.dimensions,
                occurrence.terminal,
            )?;
            MosJunctionSaturation {
                fallback: MosSaturationFallback::ZeroGeometry(1e-14),
                ..densities
            }
            .prepare(raw)?
        } else {
            saturation
        };
        let depletion = temperature
            .depletion(self.depletion, self.depletion_temperature)?
            .prepare(geometry, rules.sidewall, occurrence.gate_width_m)?;
        let conduction = Bsim3BerkeleyJunction::new(
            saturation,
            temperature.emission_thermal_voltage(),
            self.limiting_current_a,
            reference,
        )?;
        let resistance_ohm =
            occurrence.prepare_resistance(rules.resistance, self.nominal_temperature_k)?;
        Ok(PreparedBsim3Junction {
            conduction,
            depletion,
            resistance_ohm,
        })
    }
}

impl PreparedBsim3Junction {
    /// Single-device series resistance, to be divided by parallel multiplicity.
    #[must_use = "the solver uses the resistance to select and stamp series topology"]
    pub const fn resistance_ohm(&self) -> f64 {
        self.resistance_ohm
    }

    /// Evaluate the bound junction at forward-positive voltage.
    ///
    /// # Errors
    /// Nonfinite bias or nonrepresentable physical output.
    pub fn evaluate(&self, voltage_v: f64) -> Result<MosJunctionPoint, ModelError> {
        self.conduction.evaluate(voltage_v, &self.depletion)
    }
}
