//! Normal HiSIM-HV channel shortening and physical channel terminal charge.
use super::{
    HiSimHvChannelChargePoint, HiSimHvChannelRegion, HiSimHvIntrinsicScalar as Jet,
    PreparedHiSimHvInstance, PreparedHiSimHvModel, instance_real_at, lookup_slot,
};
use crate::{ModelError, ParameterScope};
const EPS: f64 = 10. * f64::EPSILON;
const SILICON_PERMITTIVITY: f64 = 1.034_943e-10;
const ELECTRON_CHARGE: f64 = 1.602_191_8e-19;

/// Geometry and native model coefficients for channel-length modulation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreparedHiSimHvChannelModulation {
    pub channel_length_m: f64,
    pub potential_partition: f64,
    pub bulk_factor: f64,
    pub inversion_factor: f64,
    pub length_factor: f64,
    pub channel_doping_m3: f64,
}
/// Local intrinsic channel geometry after the native CLM law.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvChannelModulationPoint {
    pub shortening_m: Jet,
    pub effective_length_m: Jet,
    pub drain_surface: Jet,
}
/// Normalized channel-only charge vector (includes device multiplicity).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvChannelTerminalCharges {
    pub gate: Jet,
    pub drain: Jet,
    pub source: Jet,
    pub bulk: Jet,
}
/// Physical channel capacitance area, including device multiplicity.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreparedHiSimHvChannelChargeArea {
    area_m2: f64,
}

/// Bind native CLM and charge-area parameters for one prepared instance.
/// # Errors
/// Invalid model binding or physical geometry.
pub fn prepare_hisim_hv_channel_modulation(
    model: &PreparedHiSimHvModel,
    instance: &PreparedHiSimHvInstance,
) -> Result<
    (
        PreparedHiSimHvChannelModulation,
        PreparedHiSimHvChannelChargeArea,
    ),
    ModelError,
> {
    let material = super::prepare_hisim_hv_electrostatic_material(model, instance)?;
    let binned = |name| {
        let slot = lookup_slot(model.variant(), name)?;
        instance
            .binned_model_slot(slot)
            .ok_or(ModelError::MissingDefaultDependency(slot))
    };
    let geometry = instance.effective_geometry();
    let modulation = PreparedHiSimHvChannelModulation {
        channel_length_m: geometry.channel_length_m,
        potential_partition: binned("clm1")?,
        bulk_factor: binned("clm2")?,
        inversion_factor: binned("clm3")?,
        length_factor: geometry.channel_length_modulation_factor,
        channel_doping_m3: material.channel_doping,
    };
    let m_slot = model
        .variant()
        .parameter(ParameterScope::Instance, "m")
        .ok_or_else(|| ModelError::UnknownParameter("m".to_owned()))?
        .slot;
    let area = geometry.channel_length_m
        * geometry.capacitance_width_total_m
        * instance_real_at(instance, m_slot)?;
    if !area.is_finite() || area <= 0. {
        return Err(ModelError::InvalidPhysicalParameter(
            "HiSIM-HV intrinsic charge area",
        ));
    }
    Ok((
        modulation,
        PreparedHiSimHvChannelChargeArea { area_m2: area },
    ))
}

/// Local native channel-shortening inputs; all gradients use normalized
/// intrinsic Vgs, Vds, Vbs and kelvin coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvChannelModulationInputs {
    pub drain_bias: Jet,
    pub confined_body: Jet,
    pub inverse_thermal_voltage: Jet,
    pub symmetry_factor: Jet,
}

impl PreparedHiSimHvChannelModulation {
    /// Evaluate the source-defined channel length modulation law.
    /// # Errors
    /// Invalid physical length, charge state, or nonfinite result.
    pub fn evaluate(
        &self,
        inputs: &HiSimHvChannelModulationInputs,
        charge: &HiSimHvChannelChargePoint,
    ) -> Result<HiSimHvChannelModulationPoint, ModelError> {
        let zero = Jet::constant(0.);
        let active = matches!(
            charge.region,
            HiSimHvChannelRegion::Transition | HiSimHvChannelRegion::StrongInversion
        );
        let enabled = self.bulk_factor >= EPS || self.inversion_factor >= EPS;
        let mut surface = charge.drain_surface;
        let shortening = if active {
            if enabled {
                surface = self.potential_partition * (inputs.drain_bias + charge.source_surface)
                    + (1. - self.potential_partition) * surface;
            }
            let maximum = charge.source_surface + inputs.drain_bias - EPS;
            if surface.value > maximum.value {
                surface = maximum;
            }
            if enabled {
                self.shortening(inputs, charge, surface)?
            } else {
                zero
            }
        } else {
            zero
        };
        let length = self.channel_length_m - shortening;
        let length = if length.value < 1e-9 {
            Jet::constant(1e-9)
        } else {
            length
        };
        Ok(HiSimHvChannelModulationPoint {
            shortening_m: shortening.checked("HiSIM-HV channel shortening")?,
            effective_length_m: length.checked("HiSIM-HV channel length")?,
            drain_surface: surface.checked("HiSIM-HV modulated drain surface")?,
        })
    }
    fn shortening(
        &self,
        inputs: &HiSimHvChannelModulationInputs,
        charge: &HiSimHvChannelChargePoint,
        surface: Jet,
    ) -> Result<Jet, ModelError> {
        let qnsub = ELECTRON_CHARGE * self.channel_doping_m3;
        let depletion_width = (2. * SILICON_PERMITTIVITY / qnsub).sqrt()
            * (charge.drain_surface - inputs.confined_body).sqrt();
        let coefficient = SILICON_PERMITTIVITY
            / (self.bulk_factor * qnsub
                + self.inversion_factor * charge.source_inversion_density / depletion_width);
        let delta = surface - charge.drain_surface;
        let transport = charge.drift_diffusion_integral
            / (inputs.inverse_thermal_voltage * charge.source_inversion_density);
        let electric = 2. * qnsub / SILICON_PERMITTIVITY * delta + 1e5;
        let linear =
            (2. * transport + electric * coefficient) * coefficient / self.channel_length_m;
        let quadratic = 4. * electric * coefficient * coefficient;
        let root = (linear * linear + quadratic).sqrt();
        let shortening = if linear.value >= 0. {
            0.5 * quadratic / (root + linear)
        } else {
            0.5 * (root - linear)
        };
        (shortening * inputs.symmetry_factor * self.length_factor * charge.transition_weight)
            .checked("HiSIM-HV channel length modulation")
    }
}
impl PreparedHiSimHvChannelChargeArea {
    /// Convert area charge densities into the normalized, conservative terminal
    /// vector. The physical polarity and drain/source orientation belong to
    /// the device-level assembly; no overlap or junction charges are included.
    /// # Errors
    /// Nonfinite terminal charge or Jacobian.
    pub fn evaluate(
        self,
        charge: &HiSimHvChannelChargePoint,
    ) -> Result<HiSimHvChannelTerminalCharges, ModelError> {
        let bulk = -self.area_m2 * charge.bulk_density;
        let inversion = -self.area_m2 * charge.inversion_density;
        let drain = inversion * charge.drain_fraction;
        let source = inversion - drain;
        let gate = -(bulk + inversion);
        Ok(HiSimHvChannelTerminalCharges {
            gate: gate.checked("HiSIM-HV channel gate charge")?,
            drain: drain.checked("HiSIM-HV channel drain charge")?,
            source: source.checked("HiSIM-HV channel source charge")?,
            bulk: bulk.checked("HiSIM-HV channel bulk charge")?,
        })
    }
}
