//! HiSIM-HV2 depletion-mode surface, residual-layer and back-layer transport.
use super::{
    HiSimHvDepletionEndpointPoint, HiSimHvIntrinsicScalar as Jet, depletion_surface::cap_distance,
};
use crate::ModelError;

const ELECTRON_CHARGE: f64 = 1.602_191_8e-19;
const EPS: f64 = 10. * f64::EPSILON;
const SMALL: f64 = 1e-25;
const ERR: ModelError =
    ModelError::InvalidPhysicalParameter("HiSIM-HV2 depletion channel transport");

/// The two Coulomb-scattering parameters of a carrier population, in native units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvDepletionCarrierMobility {
    pub coulomb_mobility: f64,
    pub coulomb_density_factor: f64,
}

/// Geometry and native mobility coefficients shared across bias evaluations.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvDepletionChannelCoefficients {
    pub surface: HiSimHvDepletionCarrierMobility,
    pub residual: HiSimHvDepletionCarrierMobility,
    pub back: HiSimHvDepletionCarrierMobility,
    pub inversion_field_factor: f64,
    pub surface_phonon_exponent: f64,
    pub neutral_phonon_exponent: f64,
    pub roughness_exponent: f64,
    pub roughness_mobility: f64,
    pub surface_velocity_exponent: f64,
    pub neutral_velocity_exponent: f64,
    pub length_m: f64,
    pub width_m: f64,
    pub multiplicity: f64,
}

/// Temperature-sensitive transport inputs and the neutral-layer effective voltage.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvDepletionChannelInputs {
    pub inverse_thermal_voltage: Jet,
    pub density_gradient: Jet,
    pub surface_phonon_coefficient: Jet,
    pub neutral_phonon_coefficient: Jet,
    pub surface_saturation_velocity: Jet,
    pub neutral_saturation_velocity: Jet,
    pub effective_neutral_drain: Jet,
}

/// Current and charge densities of the depletion channel, before access correction.
///
/// Current components include multiplicity. The signed charge densities use the
/// native Qbu/Qiu convention, before applying terminal area and partition.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvDepletionChannelPoint {
    pub surface_difference: Jet,
    pub drift_diffusion_integral: Jet,
    pub surface_universal_mobility: Jet,
    pub surface_mobility: Jet,
    pub residual_mobility: Jet,
    pub back_mobility: Jet,
    pub surface_current_a: Jet,
    pub residual_current_a: Jet,
    pub back_current_a: Jet,
    pub drain_current_a: Jet,
    pub bulk_density: Jet,
    pub channel_density: Jet,
    pub noise_inversion_density: Jet,
}

fn surface_integral(
    source: &HiSimHvDepletionEndpointPoint,
    drain: &HiSimHvDepletionEndpointPoint,
    beta: Jet,
) -> (Jet, Jet, Jet) {
    let difference = drain.surface.surface - source.surface.surface;
    let pds = difference + EPS;
    if pds.value < 0. {
        return (Jet::constant(0.), Jet::constant(0.), Jet::constant(0.));
    }
    let density = -(source.surface.surface_charge_density + drain.surface.surface_charge_density);
    let delta = source.smoothing_charge;
    let drift_density = if density.value >= delta.value {
        density
    } else {
        delta * cap_distance((delta - density) / delta, 4.)
    };
    let integral =
        beta * drift_density * pds * 0.5 + drain.transport_carrier - source.transport_carrier;
    (pds, difference, integral)
}

fn nonnegative_pow(field: Jet, exponent: f64) -> Jet {
    if exponent == 0. {
        Jet::constant(1.)
    } else if field.value == 0. && exponent > 0. && field.gradient.into_iter().all(|v| v == 0.) {
        Jet::constant(0.)
    } else {
        field.positive_pow(exponent)
    }
}

impl HiSimHvDepletionCarrierMobility {
    fn coulomb(self, density: Jet) -> Jet {
        self.coulomb_mobility + self.coulomb_density_factor * density / (ELECTRON_CHARGE * 1e15)
    }
    fn neutral(
        self,
        density: Jet,
        density_factor: Jet,
        input: &HiSimHvDepletionChannelInputs,
        coefficients: &HiSimHvDepletionChannelCoefficients,
        field: Jet,
    ) -> Result<Jet, ModelError> {
        let transverse = density / density_factor;
        let inverse = 1. / self.coulomb(density)
            + input.neutral_phonon_coefficient
                * nonnegative_pow(transverse, coefficients.neutral_phonon_exponent);
        if !inverse.value.is_finite() || inverse.value <= 0. {
            return Err(ERR);
        }
        let universal = 1. / (1e4 * inverse);
        let ratio = universal * field / input.neutral_saturation_velocity + SMALL;
        Ok(universal
            * super::channel_mobility::saturation_factor(
                ratio,
                coefficients.neutral_velocity_exponent,
            ))
    }
}

impl HiSimHvDepletionChannelCoefficients {
    fn surface_mobilities(
        &self,
        density: Jet,
        density_factor: Jet,
        integral: Jet,
        input: &HiSimHvDepletionChannelInputs,
    ) -> Result<(Jet, Jet), ModelError> {
        let transverse = self.inversion_field_factor * density / density_factor;
        let inverse = 1. / self.surface.coulomb(density)
            + input.surface_phonon_coefficient
                * nonnegative_pow(transverse, self.surface_phonon_exponent)
            + nonnegative_pow(transverse, self.roughness_exponent) / self.roughness_mobility;
        if !inverse.value.is_finite() || inverse.value <= 0. {
            return Err(ERR);
        }
        let universal = 1. / (1e4 * inverse);
        let field = integral / (input.inverse_thermal_voltage * (density + SMALL) * self.length_m);
        let floor = 0.2 * input.surface_saturation_velocity / universal;
        let longitudinal = (field * field + floor * floor).sqrt();
        let mobility = universal
            * super::channel_mobility::saturation_factor(
                universal * longitudinal / input.surface_saturation_velocity,
                self.surface_velocity_exponent,
            );
        Ok((universal, mobility))
    }

    /// Evaluate all three native depletion-channel currents and area densities.
    /// # Errors
    /// Invalid physical coefficients or nonfinite local transport state.
    pub fn evaluate(
        &self,
        input: &HiSimHvDepletionChannelInputs,
        source: &HiSimHvDepletionEndpointPoint,
        drain: &HiSimHvDepletionEndpointPoint,
    ) -> Result<HiSimHvDepletionChannelPoint, ModelError> {
        if [
            self.length_m,
            self.width_m,
            self.multiplicity,
            self.roughness_mobility,
            self.surface_velocity_exponent,
            self.neutral_velocity_exponent,
            input.inverse_thermal_voltage.value,
            input.surface_saturation_velocity.value,
            input.neutral_saturation_velocity.value,
            source.smoothing_charge.value,
        ]
        .into_iter()
        .any(|v| !v.is_finite() || v <= 0.)
            || input.effective_neutral_drain.value < 0.
        {
            return Err(ERR);
        }
        let (pds, difference, integral) =
            surface_integral(source, drain, input.inverse_thermal_voltage);
        let density_factor = 1. + difference * input.density_gradient;
        let surface_density = -source.transport_carrier;
        let residual_density = -source.residual_carrier;
        let back_density = -source.back_carrier;
        if density_factor.value <= 0.
            || [surface_density, residual_density, back_density]
                .into_iter()
                .any(|v| v.value < 0.)
        {
            return Err(ERR);
        }
        let (universal, mobility) =
            self.surface_mobilities(surface_density, density_factor, integral, input)?;
        let neutral_field = input.effective_neutral_drain / self.length_m;
        let residual =
            self.residual
                .neutral(residual_density, density_factor, input, self, neutral_field)?;
        let back = self
            .back
            .neutral(back_density, density_factor, input, self, neutral_field)?;
        let width = self.width_m * self.multiplicity;
        let surface_current =
            width * integral * mobility / (input.inverse_thermal_voltage * self.length_m);
        let residual_current = width * residual_density * residual * neutral_field;
        let back_current = width * back_density * back * neutral_field;
        let bulk_density = -0.5
            * (source.substrate_inversion
                + drain.substrate_inversion
                + source.surface.substrate_charge_density
                + drain.surface.substrate_charge_density);
        let channel_density = -0.5
            * (source.accumulation_charge
                + drain.accumulation_charge
                + source.surface_depletion
                + drain.surface_depletion
                + source.surface.buried_charge_density
                + drain.surface.buried_charge_density);
        Ok(HiSimHvDepletionChannelPoint {
            surface_difference: pds.checked("HiSIM-HV2 depletion Pds")?,
            drift_diffusion_integral: integral.checked("HiSIM-HV2 depletion Idd")?,
            surface_universal_mobility: universal
                .checked("HiSIM-HV2 depletion surface universal mobility")?,
            surface_mobility: mobility.checked("HiSIM-HV2 depletion surface mobility")?,
            residual_mobility: residual.checked("HiSIM-HV2 depletion residual mobility")?,
            back_mobility: back.checked("HiSIM-HV2 depletion back mobility")?,
            surface_current_a: surface_current.checked("HiSIM-HV2 depletion surface current")?,
            residual_current_a: residual_current.checked("HiSIM-HV2 depletion residual current")?,
            back_current_a: back_current.checked("HiSIM-HV2 depletion back current")?,
            drain_current_a: (surface_current + residual_current + back_current)
                .checked("HiSIM-HV2 depletion total current")?,
            bulk_density: bulk_density.checked("HiSIM-HV2 depletion bulk density")?,
            channel_density: channel_density.checked("HiSIM-HV2 depletion channel density")?,
            noise_inversion_density: (-0.5
                * (source.accumulation_charge + drain.accumulation_charge))
                .checked("HiSIM-HV2 depletion noise density")?,
        })
    }
}
