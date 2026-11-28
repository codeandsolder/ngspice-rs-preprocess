//! Normal-channel HiSIM-HV universal mobility, velocity saturation and Ids0.
use super::{
    HiSimHvAccessCorrectionPoint, HiSimHvAccessInputs, HiSimHvChannelChargePoint,
    HiSimHvChannelCorrectionPoint, HiSimHvChannelElectrostaticPoint, HiSimHvChannelModulationPoint,
    HiSimHvChannelRegion, HiSimHvExternalBias, HiSimHvIntrinsicBias, HiSimHvIntrinsicScalar as Jet,
    PreparedHiSimHvAccessCorrection, PreparedHiSimHvChannelElectrostatics, PreparedHiSimHvInstance,
    PreparedHiSimHvModel, PreparedHiSimHvTransport, channel_correction::ChannelCorrection,
    instance::model_real, instance_real_at, lookup_slot, prepare_hisim_hv_access_correction,
    prepare_hisim_hv_channel_electrostatics,
};
use crate::{ModelError, ParameterScope};
use std::sync::Arc;

const SILICON_PERMITTIVITY: f64 = 1.034_943e-10;
const ELECTRON_CHARGE: f64 = 1.602_191_8e-19;
const SMALL: f64 = 1e-25;

/// Geometry-dependent coefficients of the native universal mobility law.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvChannelMobilityCoefficients {
    pub depletion_field_factor: f64,
    pub inversion_field_factor: f64,
    pub coulomb_mobility: f64,
    pub coulomb_density_factor: f64,
    pub phonon_exponent: f64,
    pub roughness_exponent: f64,
    pub roughness_mobility: f64,
    pub velocity_exponent: f64,
    pub effective_width_m: f64,
    pub multiplicity: f64,
}

/// Local temperature-dependent mobility inputs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvChannelMobilityInputs {
    pub inverse_thermal_voltage: Jet,
    pub density_gradient: Jet,
    pub phonon_coefficient: Jet,
    pub saturation_velocity_m_per_s: Jet,
}

/// Intrinsic normal-channel transport before access and leakage terms.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvChannelMobilityPoint {
    pub transverse_field: Jet,
    pub universal_mobility_m2_per_vs: Jet,
    pub longitudinal_field: Jet,
    pub mobility_m2_per_vs: Jet,
    /// Normalized Ids0, scaled by the instance multiplicity.
    pub drain_current_a: Jet,
}

fn saturation_factor(ratio: Jet, exponent: f64) -> Jet {
    // Evaluate (1 + ratio^BB)^(-1/BB) without overflowing ratio^BB.
    let log_ratio = ratio.ln();
    if log_ratio.value > 0. {
        (-log_ratio - (-exponent * log_ratio).exp().ln_1p() / exponent).exp()
    } else {
        (-(exponent * log_ratio).exp().ln_1p() / exponent).exp()
    }
}

impl HiSimHvChannelMobilityCoefficients {
    /// Evaluate the native universal mobility, field correction and core current.
    ///
    /// The four derivatives are with respect to normalized intrinsic bias and
    /// local device temperature. Accumulation and the no-inversion region
    /// bypass transport just as in the original evaluator.
    /// # Errors
    /// Nonphysical coefficients or nonfinite transport state.
    pub fn evaluate(
        &self,
        inputs: &HiSimHvChannelMobilityInputs,
        charge: &HiSimHvChannelChargePoint,
        modulation: &HiSimHvChannelModulationPoint,
    ) -> Result<HiSimHvChannelMobilityPoint, ModelError> {
        let zero = Jet::constant(0.);
        if matches!(
            charge.region,
            HiSimHvChannelRegion::Accumulation | HiSimHvChannelRegion::NoInversion
        ) {
            return Ok(HiSimHvChannelMobilityPoint {
                transverse_field: zero,
                universal_mobility_m2_per_vs: zero,
                longitudinal_field: zero,
                mobility_m2_per_vs: zero,
                drain_current_a: zero,
            });
        }
        let field = (self.depletion_field_factor * charge.bulk_density
            + self.inversion_field_factor * charge.inversion_density)
            / (1. + charge.surface_difference * inputs.density_gradient);
        let sheet_density = charge.inversion_density / (ELECTRON_CHARGE * 1e15);
        let coulomb = self.coulomb_mobility + self.coulomb_density_factor * sheet_density;
        if field.value <= 0.
            || coulomb.value <= 0.
            || self.roughness_mobility <= 0.
            || self.velocity_exponent <= 0.
            || inputs.saturation_velocity_m_per_s.value <= 0.
            || inputs.inverse_thermal_voltage.value <= 0.
            || modulation.effective_length_m.value <= 0.
        {
            return Err(ModelError::InvalidPhysicalParameter(
                "HiSIM-HV channel mobility domain",
            ));
        }
        let inverse_mobility = 1. / coulomb
            + inputs.phonon_coefficient * field.positive_pow(self.phonon_exponent)
            + field.positive_pow(self.roughness_exponent) / self.roughness_mobility;
        let universal = 1. / (1e4 * inverse_mobility);
        let length = modulation.effective_length_m;
        let transport_field = charge.drift_diffusion_integral
            / (inputs.inverse_thermal_voltage * (charge.source_inversion_density + SMALL) * length);
        let velocity = inputs.saturation_velocity_m_per_s;
        let floor = 0.2 * velocity / universal;
        let longitudinal = (transport_field * transport_field + floor * floor).sqrt();
        let mobility = universal
            * saturation_factor(universal * longitudinal / velocity, self.velocity_exponent);
        let current =
            self.multiplicity * self.effective_width_m * charge.drift_diffusion_integral * mobility
                / (inputs.inverse_thermal_voltage * length);
        Ok(HiSimHvChannelMobilityPoint {
            transverse_field: field.checked("HiSIM-HV transverse field")?,
            universal_mobility_m2_per_vs: universal.checked("HiSIM-HV universal mobility")?,
            longitudinal_field: longitudinal.checked("HiSIM-HV longitudinal field")?,
            mobility_m2_per_vs: mobility.checked("HiSIM-HV field-limited mobility")?,
            drain_current_a: current.checked("HiSIM-HV core channel current")?,
        })
    }
}

/// Bound normal-channel constitutive evaluator with instance provenance.
#[derive(Clone, Debug)]
pub struct PreparedHiSimHvNormalChannel {
    electrostatics: PreparedHiSimHvChannelElectrostatics,
    mobility: HiSimHvChannelMobilityCoefficients,
    correction: ChannelCorrection,
    access: PreparedHiSimHvAccessCorrection,
    sti: super::sti::StiCorrection,
    impact: super::impact::ImpactIonization,
    leakage: super::leakage::Leakage,
    identity: Arc<()>,
}

/// Coupled intrinsic charge and channel current, excluding access and junction branches.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvNormalChannelPoint {
    pub electrostatics: HiSimHvChannelElectrostaticPoint,
    pub mobility: HiSimHvChannelMobilityPoint,
    pub correction: HiSimHvChannelCorrectionPoint,
    /// Edge-channel current added after nonlinear access resistance.
    pub sti_current_a: Jet,
    /// Total intrinsic channel current before external access correction.
    pub drain_current_a: Jet,
}

/// Normal channel with the physically independent external-bias correction.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvNormalChannelWithAccessPoint {
    pub intrinsic: HiSimHvNormalChannelPoint,
    pub access: HiSimHvAccessCorrectionPoint,
}

/// Coupled normal-channel state with impact and gate/junction leakage currents.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvNormalChannelWithParasiticsPoint {
    pub channel: HiSimHvNormalChannelWithAccessPoint,
    pub impact: super::HiSimHvImpactPoint,
    pub leakage: super::HiSimHvLeakagePoint,
}

/// Prepare the native enhancement-mode channel for repeated local evaluation.
/// # Errors
/// Invalid prepared model, geometry, or unsupported depletion mode.
pub fn prepare_hisim_hv_normal_channel(
    model: &PreparedHiSimHvModel,
    instance: &PreparedHiSimHvInstance,
) -> Result<PreparedHiSimHvNormalChannel, ModelError> {
    let electrostatics = prepare_hisim_hv_channel_electrostatics(model, instance)?;
    let binned = |name| {
        let slot = lookup_slot(model.variant(), name)?;
        instance
            .binned_model_slot(slot)
            .ok_or(ModelError::MissingDefaultDependency(slot))
    };
    let length = (instance.geometry().gate_length_m * 1e6).powf(model_real(model, "ndeplp")?);
    let depletion = binned("ndep")? * length
        / (100. * SILICON_PERMITTIVITY * (length + model_real(model, "ndepl")?));
    let m_slot = model
        .variant()
        .parameter(ParameterScope::Instance, "m")
        .ok_or_else(|| ModelError::UnknownParameter("m".to_owned()))?
        .slot;
    let mobility = HiSimHvChannelMobilityCoefficients {
        depletion_field_factor: depletion,
        inversion_field_factor: binned("ninv")? / (100. * SILICON_PERMITTIVITY),
        coulomb_mobility: binned("muecb0")?,
        coulomb_density_factor: binned("muecb1")?,
        phonon_exponent: model_real(model, "mueph0")?,
        roughness_exponent: instance.transport_geometry().surface_roughness_coefficient,
        roughness_mobility: binned("muesr1")?,
        velocity_exponent: model_real(model, "bb")?,
        effective_width_m: instance.effective_geometry().channel_width_total_m,
        multiplicity: instance_real_at(instance, m_slot)?,
    };
    Ok(PreparedHiSimHvNormalChannel {
        electrostatics,
        mobility,
        correction: ChannelCorrection::prepare(model, instance, mobility.multiplicity)?,
        access: prepare_hisim_hv_access_correction(model, instance)?,
        sti: super::sti::StiCorrection::prepare(model, instance, mobility.multiplicity)?,
        impact: super::impact::ImpactIonization::prepare(model, instance)?,
        leakage: super::leakage::Leakage::prepare(model, instance)?,
        identity: Arc::clone(&instance.identity),
    })
}

impl PreparedHiSimHvNormalChannel {
    /// Evaluate normal channel, access, impact and leakage with an independent
    /// normalized HV2 drain drift-region voltage drop (unused for HV1).
    /// # Errors
    /// Invalid local constitutive state or mismatched context.
    pub fn evaluate_with_parasitics(
        &self,
        bias: HiSimHvIntrinsicBias,
        external: HiSimHvExternalBias,
        drift_voltage: f64,
        transport: &PreparedHiSimHvTransport,
    ) -> Result<HiSimHvNormalChannelWithParasiticsPoint, ModelError> {
        let channel = self.evaluate_with_access(bias, external, transport)?;
        let impact = self.impact.evaluate(
            bias,
            external,
            drift_voltage,
            &channel,
            &transport.temperature,
        )?;
        let leakage = self
            .leakage
            .evaluate(bias, &channel.intrinsic, &transport.temperature)?;
        Ok(HiSimHvNormalChannelWithParasiticsPoint {
            channel,
            impact,
            leakage,
        })
    }

    /// Evaluate intrinsic charge and access-corrected channel current in their
    /// independent physical coordinates, before explicit series/drift branches.
    /// # Errors
    /// Mismatched context or invalid local constitutive state.
    pub fn evaluate_with_access(
        &self,
        bias: HiSimHvIntrinsicBias,
        external: HiSimHvExternalBias,
        transport: &PreparedHiSimHvTransport,
    ) -> Result<HiSimHvNormalChannelWithAccessPoint, ModelError> {
        let intrinsic = self.evaluate(bias, transport)?;
        let access = self.access.evaluate(&HiSimHvAccessInputs {
            intrinsic: bias,
            external,
            preaccess_current_a: intrinsic.mobility.drain_current_a
                + intrinsic.correction.parasitic_channel_current_a,
            postaccess_current_a: intrinsic.correction.ohmic_leakage_a + intrinsic.sti_current_a,
            conducting_channel: !matches!(
                intrinsic.electrostatics.charge.region,
                HiSimHvChannelRegion::Accumulation | HiSimHvChannelRegion::NoInversion
            ),
        })?;
        Ok(HiSimHvNormalChannelWithAccessPoint { intrinsic, access })
    }

    /// Evaluate core current and terminal charge with one consistent temperature state.
    /// # Errors
    /// Mismatched instance or nonphysical local constitutive state.
    pub fn evaluate(
        &self,
        bias: HiSimHvIntrinsicBias,
        transport: &PreparedHiSimHvTransport,
    ) -> Result<HiSimHvNormalChannelPoint, ModelError> {
        if !Arc::ptr_eq(&self.identity, &transport.instance_identity) {
            return Err(ModelError::MismatchedPreparedContext);
        }
        let t = &transport.temperature;
        let electrostatics = self.electrostatics.evaluate(bias, t)?;
        let inputs = HiSimHvChannelMobilityInputs {
            inverse_thermal_voltage: Jet::coordinate(
                t.inverse_thermal_voltage,
                3,
                t.inverse_thermal_voltage_derivative,
            ),
            density_gradient: Jet::coordinate(
                transport.density_gradient_coefficient,
                3,
                transport.density_gradient_derivative_per_kelvin,
            ),
            phonon_coefficient: Jet::coordinate(
                transport.phonon_coefficient,
                3,
                transport.phonon_coefficient_derivative_per_kelvin,
            ),
            saturation_velocity_m_per_s: Jet::coordinate(
                transport.saturation_velocity_m_per_s,
                3,
                transport.saturation_velocity_derivative_m_per_s_kelvin,
            ),
        };
        let mobility =
            self.mobility
                .evaluate(&inputs, &electrostatics.charge, &electrostatics.modulation)?;
        let correction = self.correction.evaluate(bias, &electrostatics, &mobility)?;
        let sti_current_a = self.sti.evaluate(bias, &electrostatics, &mobility, t)?;
        let drain_current_a = (mobility.drain_current_a
            + sti_current_a
            + correction.parasitic_channel_current_a
            + correction.ohmic_leakage_a)
            .checked("HiSIM-HV intrinsic channel current")?;
        Ok(HiSimHvNormalChannelPoint {
            electrostatics,
            mobility,
            correction,
            sti_current_a,
            drain_current_a,
        })
    }
}
