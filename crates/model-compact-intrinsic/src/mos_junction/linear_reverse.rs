//! Linear-reverse MOS junction preparation, distinct from Berkeley BSIM3.
//!
//! The temperature laws follow the conventional MOS junction equations in
//! HSPICE MOSFET Models Manual X-2005.09, pp. 98–106. Source selectors are
//! resolved outside this module; every coefficient is in physical SI units.

use super::{
    MosDepletionComponent, MosDepletionShape, MosDepletionTemperatureCoefficients,
    MosDiffusionRule, MosJunctionDepletion, MosJunctionOccurrence, MosJunctionPoint,
    MosJunctionSaturation, MosLinearReverseJunction, MosSeriesResistanceRule, MosSidewallRule,
    finite, mos_resistance_at_temperature, nonnegative, positive, prepare_mos_diffusion,
};
use crate::ModelError;

/// Silicon or explicitly supplied semiconductor energy-gap law.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MosJunctionBandgap {
    pub zero_temperature_ev: f64,
    pub alpha_ev_per_k: f64,
    pub beta_k: f64,
}

impl MosJunctionBandgap {
    pub const SILICON: Self = Self {
        zero_temperature_ev: 1.16,
        alpha_ev_per_k: 7.02e-4,
        beta_k: 1108.,
    };

    fn at(self, temperature_k: f64) -> Result<f64, ModelError> {
        let gap = positive(self.zero_temperature_ev, "MOS zero-temperature bandgap")?;
        let alpha = nonnegative(self.alpha_ev_per_k, "MOS bandgap alpha")?;
        let beta = nonnegative(self.beta_k, "MOS bandgap beta")?;
        positive(
            (-alpha * temperature_k).mul_add(temperature_k / (temperature_k + beta), gap),
            "MOS junction bandgap",
        )
    }

    fn derivative(self, temperature_k: f64) -> f64 {
        let fraction = temperature_k / (temperature_k + self.beta_k);
        -self.alpha_ev_per_k * fraction * (2. - fraction)
    }
}

/// Alternative depletion temperature laws, independently selectable from the
/// semiconductor bandgap and saturation-current temperature exponent.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MosDepletionThermalLaw {
    /// Full semiconductor potential correction and substrate expansion.
    Semiconductor,
    /// Independently specified linear capacitance and potential coefficients.
    Linear(MosDepletionTemperatureCoefficients),
    /// Linear potential correction, with power-law capacitance rescaling.
    PotentialPower { potential_v_per_k: f64 },
    /// First-order semiconductor potential derivative and half-power density.
    SemiconductorTangent,
}

/// Prepared operating-temperature coefficients for the linear-reverse diode.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MosLinearReverseTemperature {
    nominal_k: f64,
    delta_k: f64,
    ratio: f64,
    emission_thermal_voltage_v: f64,
    saturation_log_scale: f64,
    potential_offset_v: f64,
    potential_derivative_offset_v_per_k: f64,
}

impl MosLinearReverseTemperature {
    /// Derive temperature corrections once for a physical junction.
    ///
    /// # Errors
    /// Invalid temperatures, emission, bandgap or nonrepresentable corrections.
    pub fn new(
        nominal_k: f64,
        operating_k: f64,
        emission: f64,
        exponent: f64,
        bandgap: MosJunctionBandgap,
    ) -> Result<Self, ModelError> {
        const KBOQ: f64 = 8.617_087e-5;
        let nominal = positive(nominal_k, "MOS nominal temperature")?;
        let operating = positive(operating_k, "MOS operating temperature")?;
        let emission = positive(emission, "MOS emission coefficient")?;
        let exponent = finite(exponent, "MOS saturation temperature exponent")?;
        let ratio = positive(operating / nominal, "MOS temperature ratio")?;
        let delta_k = operating - nominal;
        let relative_change = delta_k / nominal;
        let log_ratio = if relative_change > -0.5 && relative_change < 0.5 {
            relative_change.ln_1p()
        } else {
            ratio.ln()
        };
        let eg_nominal = bandgap.at(nominal)?;
        bandgap.at(operating)?;
        let vt = KBOQ * operating;
        let vt_nominal = KBOQ * nominal;
        // Exact divided difference of E(T) - (T/Tnom) E(Tnom). This
        // retains the thermal correction even for adjacent f64 temperatures.
        let gap_difference = (-bandgap.zero_temperature_ev).mul_add(
            relative_change,
            -bandgap.alpha_ev_per_k
                * (operating / (operating + bandgap.beta_k))
                * (bandgap.beta_k / (nominal + bandgap.beta_k))
                * delta_k,
        );
        Ok(Self {
            nominal_k: nominal,
            delta_k,
            ratio,
            emission_thermal_voltage_v: positive(emission * vt, "MOS emission thermal voltage")?,
            saturation_log_scale: finite(
                exponent.mul_add(log_ratio, -gap_difference / vt) / emission,
                "MOS saturation temperature scale",
            )?,
            potential_offset_v: finite(
                (-3. * vt).mul_add(log_ratio, gap_difference),
                "MOS junction potential correction",
            )?,
            potential_derivative_offset_v_per_k: finite(
                bandgap.derivative(nominal) - eg_nominal / nominal - 3. * vt_nominal / nominal,
                "MOS junction potential derivative",
            )?,
        })
    }

    /// # Errors
    /// Invalid saturation current or unrepresentable temperature correction.
    pub fn saturation(self, nominal_a: f64) -> Result<f64, ModelError> {
        let current = nonnegative(nominal_a, "MOS nominal saturation current")?;
        if current == 0. || self.saturation_log_scale == 0. {
            return Ok(current);
        }
        let scale = self.saturation_log_scale.exp();
        let adjusted = if scale.is_normal() {
            current * scale
        } else {
            (current.ln() + self.saturation_log_scale).exp()
        };
        nonnegative(adjusted, "MOS temperature-adjusted saturation current")
    }

    /// Apply a thermal law to the already geometry-weighted zero-bias capacitance.
    ///
    /// # Errors
    /// Invalid coefficients, nonpositive corrected potential or negative density.
    pub fn depletion(
        self,
        nominal_f: f64,
        shape: MosDepletionShape,
        law: MosDepletionThermalLaw,
    ) -> Result<MosDepletionComponent, ModelError> {
        let nominal = nonnegative(nominal_f, "MOS nominal depletion capacitance")?;
        let potential = positive(shape.potential_v, "MOS nominal junction potential")?;
        let grading = nonnegative(shape.grading, "MOS depletion grading")?;
        let (adjusted, factor) = match law {
            MosDepletionThermalLaw::Semiconductor => {
                let adjusted = potential.mul_add(self.ratio, self.potential_offset_v);
                (
                    adjusted,
                    grading.mul_add((400e-6 * self.delta_k + 1.) - adjusted / potential, 1.),
                )
            }
            MosDepletionThermalLaw::Linear(coeff) => (
                (-finite(
                    coeff.potential_v_per_k,
                    "MOS junction potential coefficient",
                )?)
                .mul_add(self.delta_k, potential),
                finite(
                    coeff.capacitance_per_k,
                    "MOS junction capacitance coefficient",
                )?
                .mul_add(self.delta_k, 1.),
            ),
            MosDepletionThermalLaw::PotentialPower { potential_v_per_k } => {
                let adjusted = (-finite(potential_v_per_k, "MOS junction potential coefficient")?)
                    .mul_add(self.delta_k, potential);
                (
                    adjusted,
                    (potential / positive(adjusted, "MOS adjusted junction potential")?)
                        .powf(grading),
                )
            }
            MosDepletionThermalLaw::SemiconductorTangent => {
                let change = (potential / self.nominal_k
                    + self.potential_derivative_offset_v_per_k)
                    * self.delta_k;
                (potential + change, (-0.5 / potential).mul_add(change, 1.))
            }
        };
        MosDepletionComponent::new(
            nominal * nonnegative(factor, "MOS junction capacitance temperature factor")?,
            adjusted,
            grading,
        )
    }
}

/// Physical geometry law for a linear-reverse MOS junction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MosLinearReverseGeometry {
    Explicit,
    WidthNormalized,
    Isolated,
    Stacked,
}

impl MosLinearReverseGeometry {
    const fn rules(self) -> (MosDiffusionRule, MosSidewallRule) {
        match self {
            Self::Explicit => (
                MosDiffusionRule::ExplicitLithographic,
                MosSidewallRule::EntirePerimeter,
            ),
            Self::WidthNormalized => (
                MosDiffusionRule::WidthNormalized,
                MosSidewallRule::EntirePerimeter,
            ),
            Self::Isolated => (MosDiffusionRule::Isolated, MosSidewallRule::SplitPerimeter),
            Self::Stacked => (
                MosDiffusionRule::Stacked,
                MosSidewallRule::AdditionalGateEdge,
            ),
        }
    }
}

/// Nominal single-terminal physical parameters, before temperature and geometry.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MosLinearReverseModel {
    pub saturation: MosJunctionSaturation,
    pub depletion: MosJunctionDepletion,
    pub depletion_temperature: [MosDepletionThermalLaw; 3],
    pub nominal_temperature_k: f64,
    pub emission_coefficient: f64,
    pub saturation_temperature_exponent: f64,
    pub bandgap: MosJunctionBandgap,
    pub reverse_transition_v: f64,
    pub reverse_slope_divisor: f64,
    pub transit_time_s: f64,
    pub resistance_temperature_per_k: f64,
}

/// Immutable per-terminal current, charge and resistance preparation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreparedMosLinearReverseJunction {
    conduction: MosLinearReverseJunction,
    depletion: [MosDepletionComponent; 3],
    resistance_ohm: f64,
}

impl MosLinearReverseModel {
    /// Resolve geometry before temperature scaling of the total saturation
    /// current, including any direct IS contribution. Gate-edge capacitance
    /// uses the physical diffusion width, not intrinsic electrical width.
    ///
    /// # Errors
    /// Invalid physical parameters or nonrepresentable preparation.
    pub fn prepare(
        self,
        mode: MosLinearReverseGeometry,
        resistance: MosSeriesResistanceRule,
        occurrence: MosJunctionOccurrence,
    ) -> Result<PreparedMosLinearReverseJunction, ModelError> {
        let (diffusion, sidewall) = mode.rules();
        let geometry =
            prepare_mos_diffusion(diffusion, occurrence.dimensions, occurrence.terminal)?;
        let temperature = MosLinearReverseTemperature::new(
            self.nominal_temperature_k,
            occurrence.operating_temperature_k,
            self.emission_coefficient,
            self.saturation_temperature_exponent,
            self.bandgap,
        )?;
        let saturation = temperature.saturation(self.saturation.prepare(geometry)?)?;
        let conduction = MosLinearReverseJunction::new(
            saturation,
            temperature.emission_thermal_voltage_v,
            self.reverse_transition_v,
            self.reverse_slope_divisor,
            self.transit_time_s,
        )?;
        let gate_width = if matches!(
            mode,
            MosLinearReverseGeometry::Isolated | MosLinearReverseGeometry::Stacked
        ) {
            occurrence.dimensions.diffusion_width()?
        } else {
            0.
        };
        let nominal = self.depletion.prepare(geometry, sidewall, gate_width)?;
        let adjust = |index: usize| {
            temperature.depletion(
                nominal[index].capacitance_f,
                MosDepletionShape {
                    potential_v: nominal[index].potential_v,
                    grading: nominal[index].grading,
                },
                self.depletion_temperature[index],
            )
        };
        let depletion = [adjust(0)?, adjust(1)?, adjust(2)?];
        let resistance_ohm = mos_resistance_at_temperature(
            occurrence
                .series
                .prepare(resistance, occurrence.dimensions)?,
            self.resistance_temperature_per_k,
            self.nominal_temperature_k,
            occurrence.operating_temperature_k,
        )?;
        Ok(PreparedMosLinearReverseJunction {
            conduction,
            depletion,
            resistance_ohm,
        })
    }
}

impl PreparedMosLinearReverseJunction {
    /// Single-device series resistance, before multiplicity scaling.
    #[must_use = "the solver uses the resistance for topology and stamping"]
    pub const fn resistance_ohm(&self) -> f64 {
        self.resistance_ohm
    }

    /// Evaluate current and charge, with analytic voltage derivatives.
    ///
    /// # Errors
    /// Nonfinite voltage or nonrepresentable physical output.
    pub fn evaluate(&self, voltage_v: f64) -> Result<MosJunctionPoint, ModelError> {
        self.conduction.evaluate(voltage_v, &self.depletion)
    }
}
