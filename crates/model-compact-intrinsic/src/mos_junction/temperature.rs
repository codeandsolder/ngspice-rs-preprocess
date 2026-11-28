//! Temperature preparation shared by the Berkeley BSIM3 junction releases.

use super::{
    MosBottomDensity, MosDepletionShape, MosJunctionDepletion, MosJunctionSaturation, finite,
    nonnegative, positive,
};
use crate::ModelError;

/// The thermal state of a Berkeley BSIM3 junction at an operating temperature.
///
/// The historical thermal-voltage constant and silicon energy-gap law match
/// BSIM3 3.2.4 and 3.3.0. Revision-dependent charge-density corrections remain
/// explicit at the caller; in particular 3.3.0 ACM uses nominal gate-edge density.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bsim3JunctionTemperature {
    delta_k: f64,
    emission_thermal_voltage_v: f64,
    saturation_log_scale: f64,
}

/// Linear temperature coefficients for one depletion contribution.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MosDepletionTemperatureCoefficients {
    pub capacitance_per_k: f64,
    pub potential_v_per_k: f64,
}

impl Bsim3JunctionTemperature {
    /// Prepare temperature-dependent junction factors, using kelvin throughout.
    ///
    /// # Errors
    /// Invalid temperatures, emission coefficient or nonrepresentable factors.
    pub fn new(
        nominal_k: f64,
        operating_k: f64,
        emission: f64,
        exponent: f64,
    ) -> Result<Self, ModelError> {
        const KBOQ: f64 = 8.617_087e-5;
        let nominal = positive(nominal_k, "BSIM3 nominal junction temperature")?;
        let operating = positive(operating_k, "BSIM3 operating junction temperature")?;
        let emission = positive(emission, "BSIM3 junction emission coefficient")?;
        finite(exponent, "BSIM3 junction temperature exponent")?;
        let bandgap = |t: f64| (-7.02e-4 * t).mul_add(t / (t + 1108.), 1.16);
        let nominal_vt = positive(KBOQ * nominal, "BSIM3 nominal thermal voltage")?;
        let operating_vt = positive(KBOQ * operating, "BSIM3 operating thermal voltage")?;
        let scale = exponent.mul_add(
            (operating / nominal).ln(),
            bandgap(nominal) / nominal_vt - bandgap(operating) / operating_vt,
        ) / emission;
        Ok(Self {
            delta_k: operating - nominal,
            emission_thermal_voltage_v: positive(
                emission * operating_vt,
                "BSIM3 emission thermal voltage",
            )?,
            saturation_log_scale: finite(scale, "BSIM3 junction temperature scale")?,
        })
    }

    /// Thermal voltage including the junction emission coefficient.
    #[must_use = "the current law requires the prepared emission thermal voltage"]
    pub const fn emission_thermal_voltage(self) -> f64 {
        self.emission_thermal_voltage_v
    }

    fn saturation_density(self, value: f64) -> Result<f64, ModelError> {
        let value = nonnegative(value, "nominal junction saturation density")?;
        if value == 0. || self.saturation_log_scale == 0. {
            return Ok(value);
        }
        let scale = self.saturation_log_scale.exp();
        let result = if scale.is_finite() && scale > 0. {
            value * scale
        } else {
            (value.ln() + self.saturation_log_scale).exp()
        };
        nonnegative(result, "temperature-adjusted junction saturation density")
    }

    /// Adjust the current densities, leaving the explicit zero-density fallback
    /// unchanged (it is chosen after geometric and temperature preparation).
    ///
    /// # Errors
    /// Invalid nominal densities or nonrepresentable adjusted densities.
    pub fn saturation(
        self,
        nominal: MosJunctionSaturation,
    ) -> Result<MosJunctionSaturation, ModelError> {
        let bottom = match nominal.bottom {
            MosBottomDensity::PerSquareMetre(value) => {
                MosBottomDensity::PerSquareMetre(self.saturation_density(value)?)
            }
            MosBottomDensity::PerMetre(value) => {
                MosBottomDensity::PerMetre(self.saturation_density(value)?)
            }
        };
        Ok(MosJunctionSaturation {
            bottom,
            sidewall_a_per_m: self.saturation_density(nominal.sidewall_a_per_m)?,
            ..nominal
        })
    }

    fn capacitance(self, value: f64, coefficient: f64) -> Result<f64, ModelError> {
        let nominal = nonnegative(value, "nominal depletion density")?;
        let factor = finite(
            finite(coefficient, "depletion temperature coefficient")?.mul_add(self.delta_k, 1.),
            "depletion temperature factor",
        )?
        .max(0.);
        nonnegative(nominal * factor, "temperature-adjusted depletion density")
    }

    fn shape(
        self,
        shape: MosDepletionShape,
        coefficient: f64,
    ) -> Result<MosDepletionShape, ModelError> {
        let nominal = finite(shape.potential_v, "nominal junction potential")?.max(0.1);
        let potential = finite(
            (-finite(coefficient, "junction potential temperature coefficient")?)
                .mul_add(self.delta_k, nominal),
            "temperature-adjusted junction potential",
        )?
        .max(0.01);
        Ok(MosDepletionShape {
            potential_v: potential,
            ..shape
        })
    }

    /// Prepare the three depletion components with Berkeley's nonnegative
    /// capacitance and minimum-potential limits.
    ///
    /// Set the gate-edge capacitance temperature coefficient to zero when
    /// the selected native release/mode uses nominal gate-edge density.
    /// The caller supplies coefficients in bottom, sidewall, gate-edge order.
    ///
    /// # Errors
    /// Invalid coefficients or nonrepresentable temperature corrections.
    pub fn depletion(
        self,
        nominal: MosJunctionDepletion,
        coefficients: [MosDepletionTemperatureCoefficients; 3],
    ) -> Result<MosJunctionDepletion, ModelError> {
        let [bottom, side, gate] = coefficients;
        let bottom_density = match nominal.bottom {
            MosBottomDensity::PerSquareMetre(value) => {
                MosBottomDensity::PerSquareMetre(self.capacitance(value, bottom.capacitance_per_k)?)
            }
            MosBottomDensity::PerMetre(value) => {
                MosBottomDensity::PerMetre(self.capacitance(value, bottom.capacitance_per_k)?)
            }
        };
        Ok(MosJunctionDepletion {
            bottom: bottom_density,
            bottom_shape: self.shape(nominal.bottom_shape, bottom.potential_v_per_k)?,
            sidewall_f_per_m: self.capacitance(nominal.sidewall_f_per_m, side.capacitance_per_k)?,
            sidewall_shape: self.shape(nominal.sidewall_shape, side.potential_v_per_k)?,
            gate_edge_f_per_m: self
                .capacitance(nominal.gate_edge_f_per_m, gate.capacitance_per_k)?,
            gate_edge_shape: self.shape(nominal.gate_edge_shape, gate.potential_v_per_k)?,
            fallback_f: nominal.fallback_f,
        })
    }
}

/// Apply a linear temperature coefficient to a prepared extrinsic resistance.
///
/// # Errors
/// Invalid absolute temperatures, resistance or nonphysical result.
pub fn mos_resistance_at_temperature(
    resistance_ohm: f64,
    coefficient_per_k: f64,
    nominal_k: f64,
    operating_k: f64,
) -> Result<f64, ModelError> {
    let resistance = nonnegative(resistance_ohm, "nominal MOS series resistance")?;
    let delta = positive(operating_k, "MOS operating temperature")?
        - positive(nominal_k, "MOS nominal temperature")?;
    let coefficient = finite(coefficient_per_k, "MOS resistance temperature coefficient")?;
    let factor = nonnegative(
        coefficient.mul_add(delta, 1.),
        "MOS resistance temperature factor",
    )?;
    nonnegative(
        resistance * factor,
        "temperature-adjusted MOS series resistance",
    )
}
