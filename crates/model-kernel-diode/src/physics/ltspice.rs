//! Independently measured `LTspice` semiconductor diode preparation.
//!
//! The intrinsic exponential, generation, avalanche and depletion equations
//! are the same shared primitives used by the SIMetrix producer. The source
//! dialect determines its constants, geometry, conduction ceiling, noise,
//! and temperature-dependent capacitance.

use super::{
    Evaluation, JunctionEnvironment, JunctionLaw, Slope, adjusted_potential, bandgap,
    saturation_exponent, temperature_factor,
};
use crate::{Depletion, Junction, Model, ModelError, finite, positive};

mod ceiling;
use ceiling::Tail;

/// Historical thermal constant used by this source family, J/K.
pub const BOLTZMANN: f64 = 1.380_650_3e-23;
/// Historical electron charge used by this source family, C.
pub const ELEMENTARY_CHARGE: f64 = 1.602_176_462e-19;
/// Thermal voltage per kelvin.
pub const K_OVER_Q: f64 = BOLTZMANN / ELEMENTARY_CHARGE;
const REFERENCE_TEMPERATURE: f64 = 300.15;
const REFERENCE_BANDGAP: f64 = 1.115_087_7;
const MAX_CONDUCTANCE: f64 = 1e12;

/// Sidewall model parameters. The optional potential and continuation
/// coefficient inherit the main junction settings.
#[derive(Clone, Copy, Debug)]
pub struct Sidewall {
    /// Default perimeter.
    pub perimeter: f64,
    /// Sidewall saturation current and emission coefficient. A nonpositive
    /// emission coefficient inherits the original (unclamped) bulk N.
    pub transport: Junction,
    /// Sidewall series resistance.
    pub resistance: f64,
    /// Sidewall zero-bias capacitance.
    pub capacitance: f64,
    /// Explicit sidewall junction potential.
    pub potential: Option<f64>,
    /// Sidewall grading coefficient.
    pub grading: f64,
    /// Explicit sidewall forward continuation coefficient.
    pub forward_coefficient: Option<f64>,
}

impl Default for Sidewall {
    fn default() -> Self {
        Self {
            perimeter: 0.,
            transport: Junction {
                saturation_current: 0.,
                emission: 0.,
            },
            resistance: 0.,
            capacitance: 0.,
            potential: None,
            grading: 0.33,
            forward_coefficient: None,
        }
    }
}

/// Resolved source-specific semiconductor model.
#[derive(Clone, Copy, Debug, Default)]
pub struct SourceModel {
    /// Common bulk diode coefficients.
    pub bulk: Model,
    /// Independent sidewall coefficients.
    pub sidewall: Sidewall,
    /// Soft-recovery coefficient VP.
    pub recovery: f64,
}

/// Occurrence inputs. Model temperature is a measurement temperature only.
#[derive(Clone, Copy, Debug)]
pub struct Instance {
    /// Effective analysis temperature, K.
    pub temperature: f64,
    /// Junction area, nonnegative.
    pub area: f64,
    /// Optional perimeter override; zero retains the model default.
    pub perimeter: Option<f64>,
    /// Parallel count; nonpositive disables the instance.
    pub multiplier: f64,
    /// Nonzero signed series count.
    pub series: f64,
    /// Circuit conductance floor.
    pub gmin: f64,
}

impl Default for Instance {
    fn default() -> Self {
        Self {
            temperature: 300.15,
            area: 1.,
            perimeter: None,
            multiplier: 1.,
            series: 1.,
            gmin: 1e-12,
        }
    }
}

/// One intrinsic branch and its external ohmic path.
#[derive(Clone, Copy, Debug)]
pub struct PreparedBranch {
    law: JunctionLaw,
    forward_tail: Option<Tail>,
    reverse_tail: Option<Tail>,
    factor: f64,
    series_count: f64,
    floor: f64,
    resistance: f64,
    transit_time: f64,
    temperature: f64,
    shot_offset: f64,
}

/// Immutable bulk/sidewall law. When the sidewall has no separate
/// resistance it shares the bulk intrinsic node.
#[derive(Clone, Copy, Debug)]
pub struct PreparedInstance {
    /// Bulk branch; absent for disabled occurrences.
    pub bulk: Option<PreparedBranch>,
    /// Sidewall branch if a positive perimeter and electrical law are present.
    pub sidewall: Option<PreparedBranch>,
    /// Whether sidewall uses its own resistance rather than the bulk path.
    pub independent_sidewall: bool,
    /// Prepared soft-recovery coefficient.
    pub recovery: f64,
    multiplier: f64,
    series: f64,
    noise: crate::Noise,
}

/// Contributions before optional diffusion recovery.
#[derive(Clone, Copy, Debug, Default)]
pub struct BranchEvaluation {
    /// Conduction current and terminal-voltage derivative, plus full charge.
    pub total: Evaluation,
    /// Depletion charge only, in coulombs.
    pub depletion_charge: f64,
    /// Depletion capacitance only, in farads.
    pub depletion_capacitance: f64,
    /// Equilibrium diffusion charge, in coulombs.
    pub diffusion_charge: f64,
    /// Equilibrium diffusion capacitance, in farads.
    pub diffusion_capacitance: f64,
}

fn capacitance_correction(
    depletion: Depletion,
    nominal: f64,
    temperature: f64,
    potential: f64,
    thermal_grading: f64,
) -> f64 {
    // The historical Berkeley reference bandgap is rounded independently
    // of the temperature-dependent bandgap formula.
    let nominal_ratio = nominal / REFERENCE_TEMPERATURE;
    let offset = (3. * K_OVER_Q * nominal).mul_add(
        -nominal_ratio.ln(),
        bandgap(nominal) - nominal_ratio * REFERENCE_BANDGAP,
    );
    let pbo = (depletion.potential - offset) / nominal_ratio;
    let adjustment = |t: f64, v: f64, grading: f64| {
        grading.mul_add(4e-4 * (t - REFERENCE_TEMPERATURE) - (v - pbo) / pbo, 1.)
    };
    adjustment(temperature, potential, thermal_grading)
        / adjustment(nominal, depletion.potential, depletion.grading)
}

fn prepare_law(
    model: &Model,
    instance: Instance,
    factor: f64,
    thermal_grading: f64,
) -> Result<JunctionLaw, ModelError> {
    let vt = K_OVER_Q * instance.temperature;
    let nominal = model.temperature.nominal;
    let potential =
        adjusted_potential(model.depletion.potential, nominal, instance.temperature, vt);
    let capacitance_factor = if model.depletion.capacitance == 0. {
        1.
    } else {
        capacitance_correction(
            model.depletion,
            nominal,
            instance.temperature,
            potential,
            thermal_grading,
        )
    };
    JunctionLaw::new(
        model,
        JunctionEnvironment {
            factor,
            thermal_voltage: vt,
            saturation_exponent: saturation_exponent(model, instance.temperature, K_OVER_Q),
            temperature_delta: instance.temperature - nominal,
            potential,
            capacitance_factor,
            signed_breakdown: true,
        },
    )
}

impl PreparedBranch {
    fn bulk(model: &Model, instance: Instance, factor: f64) -> Result<Self, ModelError> {
        // The vendor caps the unit-area bulk current law before area/M scale.
        positive("LTspice bulk geometry", factor)?;
        let law = prepare_law(model, instance, 1., model.depletion.grading)?;
        let forward_tail = if model.transport.saturation_current > 0.
            || (model.recombination.saturation_current > 0. && model.recombination.emission > 0.)
        {
            Some(Tail::find(|v| law.forward(v), 1.)?)
        } else {
            None
        };
        let reverse_tail = law
            .avalanche
            .filter(|terms| {
                terms
                    .iter()
                    .any(|term| term.log_current.is_finite() && term.inverse_vt > 0.)
            })
            .map(|_| Tail::find(|v| law.reverse(v), -1.))
            .transpose()?;
        let resistance = model.series.resistance
            * temperature_factor(
                model.series.linear_temperature,
                model.series.quadratic_temperature,
                instance.temperature - model.temperature.nominal,
            )
            * instance.series
            / factor;
        let shot_offset = 2. * law.transport.saturation * factor / model.transport.emission;
        finite(
            "LTspice bulk resistance and shot offset",
            &[resistance, shot_offset],
        )?;
        Ok(Self {
            law,
            forward_tail,
            reverse_tail,
            factor,
            series_count: instance.series,
            floor: instance.gmin,
            resistance,
            transit_time: model.transit_time,
            temperature: instance.temperature,
            shot_offset,
        })
    }

    fn sidewall(model: &SourceModel, instance: Instance, factor: f64) -> Result<Self, ModelError> {
        positive("LTspice sidewall geometry", factor)?;
        let source = model.sidewall;
        let mut side = model.bulk;
        side.transport = source.transport;
        // A zero inherited N has no conducting sidewall at or below nominal
        // saturation scaling. Positive scaling gives a nonfinite vendor law.
        if side.transport.emission == 0. {
            if side.transport.saturation_current > 0.
                && saturation_exponent(&side, instance.temperature, K_OVER_Q) > 0.
            {
                return Err(ModelError::Invalid(
                    "LTspice zero sidewall emission at positive saturation scaling",
                ));
            }
            side.transport.saturation_current = 0.;
            side.transport.emission = 1.;
        }
        side.recombination.saturation_current = 0.;
        side.high_injection = None;
        side.breakdown = None;
        side.depletion = Depletion {
            capacitance: source.capacitance,
            potential: source.potential.unwrap_or(model.bulk.depletion.potential),
            grading: source.grading,
            forward_coefficient: source
                .forward_coefficient
                .unwrap_or(model.bulk.depletion.forward_coefficient),
        };
        // LTspice uses the sidewall emission coefficient NS in the thermal
        // capacitance numerator, but MJSW in the nominal normalization.
        let law = prepare_law(&side, instance, factor, source.transport.emission)?;
        let forward_tail =
            if side.transport.saturation_current > 0. && source.transport.emission > 0. {
                Some(Tail::sidewall(law.transport)?)
            } else {
                None
            };
        let resistance = source.resistance * instance.series / factor;
        finite("LTspice sidewall resistance", &[resistance])?;
        Ok(Self {
            law,
            forward_tail,
            reverse_tail: None,
            // Sidewall geometry is part of the unscaled law and ceiling.
            factor: 1.,
            series_count: instance.series,
            floor: if side.transport.saturation_current > 0. {
                instance.gmin * instance.multiplier
            } else {
                0.
            },
            resistance,
            transit_time: model.bulk.transit_time,
            temperature: instance.temperature,
            shot_offset: 0.,
        })
    }

    /// Terminal-equivalent series resistance of this branch, ohms.
    #[must_use = "the resistance determines the external ohmic path"]
    pub const fn resistance(self) -> f64 {
        self.resistance
    }

    /// Evaluate conduction and equilibrium storage at the intrinsic terminal
    /// voltage, before any external series-resistance drop.
    ///
    /// # Errors
    /// Nonfinite voltage or nonrepresentable physical values.
    pub fn evaluate(self, voltage: f64) -> Result<BranchEvaluation, ModelError> {
        finite("LTspice intrinsic voltage", &[voltage])?;
        let local = voltage / self.series_count;
        let f = self
            .forward_tail
            .and_then(|tail| tail.evaluate(local))
            .unwrap_or_else(|| self.law.forward(local));
        let r = self
            .reverse_tail
            .and_then(|tail| tail.evaluate(local))
            .unwrap_or_else(|| self.law.reverse(local));
        let physical = (f.value + r.value) * self.factor;
        let current = self.floor.mul_add(local, physical);
        let conductance =
            ((f.derivative + r.derivative) * self.factor + self.floor) / self.series_count;
        let depletion = self.law.depletion.evaluate(local);
        let depletion_charge = self.factor * depletion.value;
        let depletion_capacitance = self.factor * depletion.derivative / self.series_count;
        let diffusion_charge = self.transit_time * current;
        let diffusion_capacitance = self.transit_time * conductance;
        let total = Evaluation {
            current,
            conductance,
            physical_current: physical,
            charge: depletion_charge + diffusion_charge,
            capacitance: depletion_capacitance + diffusion_capacitance,
        };
        finite(
            "LTspice semiconductor diode evaluation",
            &[
                total.current,
                total.conductance,
                total.physical_current,
                total.charge,
                total.capacitance,
                depletion_charge,
                depletion_capacitance,
                diffusion_charge,
                diffusion_capacitance,
            ],
        )?;
        Ok(BranchEvaluation {
            total,
            depletion_charge,
            depletion_capacitance,
            diffusion_charge,
            diffusion_capacitance,
        })
    }

    /// Series resistor thermal noise density, A²/Hz.
    ///
    /// # Errors
    /// A signed negative resistor has no nonnegative thermal PSD.
    pub fn series_thermal(self) -> Result<f64, ModelError> {
        if self.resistance < 0. {
            return Err(ModelError::Unsupported(
                "negative LTspice diode series noise",
            ));
        }
        let density = if self.resistance == 0. {
            0.
        } else {
            4. * BOLTZMANN * self.temperature / self.resistance
        };
        finite("LTspice diode thermal noise", &[density])?;
        Ok(density)
    }
}

impl SourceModel {
    /// Resolve model and instance coefficients into immutable physical laws.
    ///
    /// # Errors
    /// Invalid source geometry, temperature, nonrepresentable parameters or
    /// unqualified physical domains.
    pub fn bind(mut self, instance: Instance) -> Result<PreparedInstance, ModelError> {
        finite(
            "LTspice diode instance",
            &[
                instance.area,
                instance.multiplier,
                instance.series,
                instance.gmin,
            ],
        )?;
        positive("LTspice diode temperature", instance.temperature)?;
        if instance.area < 0. || instance.gmin < 0. {
            return Err(ModelError::Invalid("LTspice diode area or gmin"));
        }
        if instance.series == 0. {
            return Err(ModelError::Invalid("LTspice diode zero series count"));
        }
        let perimeter = instance
            .perimeter
            .filter(|v| *v != 0.)
            .unwrap_or(self.sidewall.perimeter);
        finite("LTspice diode perimeter", &[perimeter])?;
        self.normalize()?;
        if self.recovery > 0. && self.bulk.transit_time > 0. {
            return Err(ModelError::Unsupported("LTspice dynamic VP recovery"));
        }
        let disabled = instance.area == 0. || instance.multiplier <= 0.;
        let bulk = if disabled {
            None
        } else {
            Some(PreparedBranch::bulk(
                &self.bulk,
                instance,
                instance.area * instance.multiplier,
            )?)
        };
        let sidewall = if !disabled
            && perimeter > 0.
            && self.sidewall.resistance >= 0.
            && (self.sidewall.transport.saturation_current > 0. || self.sidewall.capacitance != 0.)
        {
            Some(PreparedBranch::sidewall(
                &self,
                instance,
                perimeter * instance.multiplier,
            )?)
        } else {
            None
        };
        let independent_sidewall = sidewall.is_some_and(|branch| branch.resistance() != 0.);
        Ok(PreparedInstance {
            bulk,
            sidewall,
            independent_sidewall,
            recovery: self.recovery,
            multiplier: instance.multiplier,
            series: instance.series,
            noise: self.bulk.noise,
        })
    }

    fn normalize(&mut self) -> Result<(), ModelError> {
        let model = &mut self.bulk;
        // These are independently witnessed source clamps, not solver
        // regularization or an attempt to sanitize arbitrary arithmetic.
        finite(
            "LTspice diode model",
            &[
                model.transport.saturation_current,
                model.transport.emission,
                model.recombination.saturation_current,
                model.recombination.emission,
                model.depletion.capacitance,
                model.depletion.potential,
                model.depletion.grading,
                model.depletion.forward_coefficient,
                model.series.resistance,
                model.series.linear_temperature,
                model.series.quadratic_temperature,
                model.temperature.energy_gap,
                model.temperature.exponent,
                model.transit_time,
                model.noise.coefficient,
                model.noise.exponent,
                self.recovery,
            ],
        )?;
        positive("LTspice nominal temperature", model.temperature.nominal)?;
        model.transport.saturation_current = model.transport.saturation_current.max(0.);
        if self.sidewall.transport.emission <= 0. {
            self.sidewall.transport.emission = model.transport.emission;
        }
        model.transport.emission = model.transport.emission.max(0.1);
        model.recombination.saturation_current = model.recombination.saturation_current.max(0.);
        if model.recombination.saturation_current == 0. {
            model.recombination.emission = 1.;
        }
        if model.recombination.emission == 0. {
            return Err(ModelError::Invalid("LTspice zero recombination emission"));
        }
        if let Some(knee) = model.high_injection {
            finite(
                "LTspice high injection",
                &[knee.current, knee.temperature_coefficient],
            )?;
        }
        model.high_injection = model.high_injection.filter(|knee| knee.current > 0.);
        model.transit_time = model.transit_time.max(0.);
        model.breakdown = model.breakdown.filter(|breakdown| breakdown.voltage != 0.);
        if let Some(breakdown) = &mut model.breakdown {
            for term in [&mut breakdown.primary, &mut breakdown.low_current] {
                finite(
                    "LTspice breakdown",
                    &[
                        term.current,
                        term.emission,
                        breakdown.voltage,
                        breakdown.linear_temperature,
                        breakdown.quadratic_temperature,
                    ],
                )?;
                term.current = term.current.max(0.);
                if term.current == 0. {
                    term.emission = 1.;
                }
                if term.emission == 0. {
                    return Err(ModelError::Invalid("LTspice zero breakdown emission"));
                }
            }
        }
        // The vendor caps the depletion transition at 95% of VJ.
        model.depletion.forward_coefficient = model.depletion.forward_coefficient.min(0.95);
        positive("LTspice junction potential", model.depletion.potential)?;
        finite(
            "LTspice sidewall model",
            &[
                self.sidewall.perimeter,
                self.sidewall.transport.saturation_current,
                self.sidewall.transport.emission,
                self.sidewall.resistance,
                self.sidewall.capacitance,
                self.sidewall.grading,
            ],
        )?;
        self.sidewall.transport.saturation_current =
            self.sidewall.transport.saturation_current.max(0.);
        if let Some(v) = self.sidewall.potential {
            finite("LTspice VJSW", &[v])?;
            positive("LTspice VJSW", v)?;
        }
        if let Some(fc) = self.sidewall.forward_coefficient {
            finite("LTspice FCS", &[fc])?;
            self.sidewall.forward_coefficient = Some(fc.min(0.95));
        }
        Ok(())
    }
}

impl PreparedInstance {
    /// Shot and flicker power densities for an individual junction branch.
    /// Sidewall shot noise excludes its conductance-floor current; the bulk
    /// law includes it and the historical saturation offset. Independent
    /// series paths require distinct local noise injection coordinates.
    ///
    /// # Errors
    /// Invalid bias/frequency or nonrepresentable/negative noise.
    pub fn branch_noise(
        self,
        evaluated: BranchEvaluation,
        sidewall: bool,
        frequency: f64,
    ) -> Result<[f64; 2], ModelError> {
        positive("LTspice diode noise frequency", frequency)?;
        let branch = if sidewall { self.sidewall } else { self.bulk };
        let Some(branch) = branch else {
            return Ok([0., 0.]);
        };
        let current = if sidewall {
            evaluated.total.physical_current
        } else {
            evaluated.total.current
        };
        let shot = ((2. * ELEMENTARY_CHARGE) * (current + branch.shot_offset).abs()
            + 4. * BOLTZMANN * branch.temperature * branch.floor)
            / self.series;
        let flicker = if self.noise.coefficient == 0. {
            0.
        } else {
            self.multiplier
                * self.noise.coefficient
                * (current.abs() / self.multiplier).powf(self.noise.exponent)
                / self.series
                / frequency
        };
        finite("LTspice diode noise density", &[shot, flicker])?;
        if shot < 0. || flicker < 0. {
            return Err(ModelError::Invalid("LTspice negative diode noise density"));
        }
        Ok([shot, flicker])
    }
}
