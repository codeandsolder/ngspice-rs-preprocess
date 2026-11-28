//! Prepared HV2 depletion-mode intrinsic channel.
use super::{
    HiSimHvAccessCorrectionPoint, HiSimHvAccessInputs, HiSimHvChannelCorrectionPoint,
    HiSimHvChannelTerminalCharges, HiSimHvDepletionCarrierMobility,
    HiSimHvDepletionChannelCoefficients, HiSimHvDepletionChannelInputs,
    HiSimHvDepletionChannelPoint, HiSimHvDepletionDrainInputs, HiSimHvDepletionDrainPoint,
    HiSimHvDepletionEndpointPoint, HiSimHvDepletionSurfaceInputs,
    HiSimHvElectrostaticFrontEndPoint, HiSimHvExternalBias, HiSimHvIntrinsicBias,
    HiSimHvIntrinsicScalar as Jet, PreparedHiSimHvAccessCorrection,
    PreparedHiSimHvChannelChargeArea, PreparedHiSimHvInstance, PreparedHiSimHvModel,
    PreparedHiSimHvTemperature, PreparedHiSimHvTransport,
    channel_correction::{ChannelCorrection, CorrectionInputs},
    channel_electrostatics::ElectrostaticFrontEnd,
    evaluate_hisim_hv_depletion_drain, evaluate_hisim_hv_depletion_endpoint,
    instance::model_real,
    instance_real_at, lookup_slot, prepare_hisim_hv_access_correction,
    prepare_hisim_hv_channel_modulation, prepare_hisim_hv_electrostatic_material,
};
use crate::{ModelError, ParameterScope};
use std::sync::Arc;
const SILICON_PERMITTIVITY: f64 = 1.034_943e-10;

/// Size-dependent preparation for the distinct HV2 CODEP=1 intrinsic law.
#[derive(Clone, Debug)]
pub struct PreparedHiSimHvDepletionChannel {
    frontend: ElectrostaticFrontEnd,
    mobility: HiSimHvDepletionChannelCoefficients,
    area: PreparedHiSimHvChannelChargeArea,
    correction: ChannelCorrection,
    access: PreparedHiSimHvAccessCorrection,
    layer_thickness_m: f64,
    drift_gate_offset: f64,
    drift_scale: f64,
    drift_floor: f64,
    source_symmetry_voltage: f64,
    identity: Arc<()>,
}

/// Coupled depletion channel state and normalized, conservative terminal charges.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvDepletionChannelEvaluation {
    pub frontend: HiSimHvElectrostaticFrontEndPoint,
    pub effective_drain: HiSimHvDepletionDrainPoint,
    pub source: HiSimHvDepletionEndpointPoint,
    pub drain: HiSimHvDepletionEndpointPoint,
    pub channel: HiSimHvDepletionChannelPoint,
    pub terminal_charges: HiSimHvChannelTerminalCharges,
    pub correction: HiSimHvChannelCorrectionPoint,
    pub modified_source_surface: Jet,
    pub classical_oxide_capacitance: f64,
    pub drain_current_a: Jet,
}

/// Intrinsic state together with the independent-coordinate access correction.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvDepletionChannelWithAccessPoint {
    pub intrinsic: HiSimHvDepletionChannelEvaluation,
    pub access: HiSimHvAccessCorrectionPoint,
}

fn prepare_mobility(
    model: &PreparedHiSimHvModel,
    instance: &PreparedHiSimHvInstance,
    multiplicity: f64,
) -> Result<HiSimHvDepletionChannelCoefficients, ModelError> {
    let binned = |name| {
        let slot = lookup_slot(model.variant(), name)?;
        instance
            .binned_model_slot(slot)
            .ok_or(ModelError::MissingDefaultDependency(slot))
    };
    let scalar = |name| model_real(model, name);
    let geometry = instance.effective_geometry();
    Ok(HiSimHvDepletionChannelCoefficients {
        surface: HiSimHvDepletionCarrierMobility {
            coulomb_mobility: binned("muecb0")?,
            coulomb_density_factor: binned("muecb1")?,
        },
        residual: HiSimHvDepletionCarrierMobility {
            coulomb_mobility: scalar("depmue0")?,
            coulomb_density_factor: scalar("depmue1")?,
        },
        back: HiSimHvDepletionCarrierMobility {
            coulomb_mobility: scalar("depmueback0")?,
            coulomb_density_factor: scalar("depmueback1")?,
        },
        inversion_field_factor: binned("ninv")? / (100. * SILICON_PERMITTIVITY),
        surface_phonon_exponent: scalar("mueph0")?,
        neutral_phonon_exponent: scalar("depmueph0")?,
        roughness_exponent: instance.transport_geometry().surface_roughness_coefficient,
        roughness_mobility: binned("muesr1")?,
        surface_velocity_exponent: scalar("bb")?,
        neutral_velocity_exponent: scalar("depbb")?,
        length_m: geometry.channel_length_m,
        width_m: geometry.channel_width_total_m,
        multiplicity,
    })
}

/// Bind the native finite-layer depletion channel to a prepared model and instance.
/// # Errors
/// Invalid context or a model that does not select HV2 CODEP=1.
pub fn prepare_hisim_hv_depletion_channel(
    model: &PreparedHiSimHvModel,
    instance: &PreparedHiSimHvInstance,
) -> Result<PreparedHiSimHvDepletionChannel, ModelError> {
    let material = prepare_hisim_hv_electrostatic_material(model, instance)?;
    if !material.depletion_mode {
        return Err(ModelError::UnsupportedInstanceParameter(
            "HiSIM-HV2 depletion channel requires CODEP=1",
        ));
    }
    let (_, area) = prepare_hisim_hv_channel_modulation(model, instance)?;
    let m_slot = model
        .variant()
        .parameter(ParameterScope::Instance, "m")
        .ok_or_else(|| ModelError::UnknownParameter("m".to_owned()))?
        .slot;
    let multiplicity = instance_real_at(instance, m_slot)?;
    Ok(PreparedHiSimHvDepletionChannel {
        frontend: ElectrostaticFrontEnd::prepare(model, instance, material)?,
        mobility: prepare_mobility(model, instance, multiplicity)?,
        area,
        correction: ChannelCorrection::prepare(model, instance, multiplicity)?,
        access: prepare_hisim_hv_access_correction(model, instance)?,
        layer_thickness_m: model_real(model, "tndep")?,
        drift_gate_offset: model_real(model, "depvdsef1")?,
        drift_scale: model_real(model, "depvdsef2")?,
        drift_floor: model_real(model, "depleak")?,
        source_symmetry_voltage: model_real(model, "pzadd0")?,
        identity: Arc::clone(&instance.identity),
    })
}

impl PreparedHiSimHvDepletionChannel {
    fn endpoints(
        &self,
        bias: HiSimHvIntrinsicBias,
        frontend: &HiSimHvElectrostaticFrontEndPoint,
        temperature: &PreparedHiSimHvTemperature,
    ) -> Result<
        (
            HiSimHvDepletionDrainPoint,
            HiSimHvDepletionEndpointPoint,
            HiSimHvDepletionEndpointPoint,
        ),
        ModelError,
    > {
        let beta = Jet::coordinate(
            temperature.inverse_thermal_voltage,
            3,
            temperature.inverse_thermal_voltage_derivative,
        );
        let material = &self.frontend.material;
        let effective = evaluate_hisim_hv_depletion_drain(HiSimHvDepletionDrainInputs {
            gate: frontend.poly.effective_gate,
            symmetric_body: frontend.conditioned.modified_body,
            drain: Jet::coordinate(bias.vds, 1, 1.),
            thermal_voltage: 1. / beta,
            oxide_capacitance: frontend.oxide.capacitance,
            depletion_doping: material.depletion_doping,
            smoothing_exponent: material.drain_smoothing_exponent,
            drift_gate_offset: self.drift_gate_offset,
            drift_scale: self.drift_scale,
            drift_floor: self.drift_floor,
        })?;
        let mut input = HiSimHvDepletionSurfaceInputs {
            effective_gate: frontend.poly.effective_gate,
            confined_body: frontend.conditioned.confined_body,
            quasi_fermi: Jet::constant(0.),
            inverse_thermal_voltage: beta,
            oxide_capacitance: frontend.oxide.capacitance,
            intrinsic_carrier_density: Jet::coordinate(
                temperature.intrinsic_carrier_concentration_m3,
                3,
                temperature.intrinsic_carrier_derivative_m3_per_kelvin,
            ),
            depletion_doping: material.depletion_doping,
            substrate_doping: material.channel_doping,
            layer_thickness_m: self.layer_thickness_m,
        };
        let source = evaluate_hisim_hv_depletion_endpoint(&input)?;
        input.quasi_fermi = effective.channel;
        let drain = evaluate_hisim_hv_depletion_endpoint(&input)?;
        Ok((effective, source, drain))
    }

    fn transport(
        &self,
        source: &HiSimHvDepletionEndpointPoint,
        drain: &HiSimHvDepletionEndpointPoint,
        effective: HiSimHvDepletionDrainPoint,
        transport: &PreparedHiSimHvTransport,
    ) -> Result<HiSimHvDepletionChannelPoint, ModelError> {
        let local = &transport.temperature;
        let depletion = transport
            .depletion
            .as_ref()
            .ok_or(ModelError::MismatchedPreparedContext)?;
        let temperature_axis = |value, derivative| Jet::coordinate(value, 3, derivative);
        self.mobility.evaluate(
            &HiSimHvDepletionChannelInputs {
                inverse_thermal_voltage: temperature_axis(
                    local.inverse_thermal_voltage,
                    local.inverse_thermal_voltage_derivative,
                ),
                density_gradient: temperature_axis(
                    transport.density_gradient_coefficient,
                    transport.density_gradient_derivative_per_kelvin,
                ),
                surface_phonon_coefficient: temperature_axis(
                    transport.phonon_coefficient,
                    transport.phonon_coefficient_derivative_per_kelvin,
                ),
                neutral_phonon_coefficient: temperature_axis(
                    depletion.phonon_coefficient,
                    depletion.phonon_coefficient_derivative_per_kelvin,
                ),
                surface_saturation_velocity: temperature_axis(
                    transport.saturation_velocity_m_per_s,
                    transport.saturation_velocity_derivative_m_per_kelvin,
                ),
                neutral_saturation_velocity: temperature_axis(
                    depletion.saturation_velocity_m_per_s,
                    depletion.saturation_velocity_derivative_m_per_kelvin,
                ),
                effective_neutral_drain: effective.drift,
            },
            source,
            drain,
        )
    }

    /// Evaluate the depletion channel and its conservative quasi-static charges.
    /// # Errors
    /// Invalid physical trial state or mismatched instance provenance.
    pub fn evaluate(
        &self,
        bias: HiSimHvIntrinsicBias,
        transport: &PreparedHiSimHvTransport,
    ) -> Result<HiSimHvDepletionChannelEvaluation, ModelError> {
        if !Arc::ptr_eq(&self.identity, &transport.instance_identity) {
            return Err(ModelError::MismatchedPreparedContext);
        }
        let frontend = self.frontend.evaluate(bias, &transport.temperature)?;
        let (effective_drain, source, drain) =
            self.endpoints(bias, &frontend, &transport.temperature)?;
        let channel = self.transport(&source, &drain, effective_drain, transport)?;
        let terminal_charges = self.area.evaluate_densities(
            channel.bulk_density,
            channel.channel_density,
            Jet::constant(0.5),
        )?;
        let correction = self.correction.evaluate_core(&CorrectionInputs {
            bias,
            conditioned: frontend.conditioned,
            surface: source.surface.surface,
            surface_difference: channel.surface_difference,
            oxide: frontend.oxide.capacitance,
            mobility: channel.surface_mobility,
            length: Jet::constant(self.mobility.length_m),
            active: true,
        })?;
        let drain_current_a = (channel.drain_current_a
            + correction.parasitic_channel_current_a
            + correction.ohmic_leakage_a)
            .checked("HiSIM-HV2 depletion current")?;
        let modified_source_surface = (source.surface.surface
            + super::smoothing::symmetry_add(
                Jet::coordinate(bias.vds, 1, 1.) - channel.surface_difference,
                self.source_symmetry_voltage,
                10. * f64::EPSILON,
            ))
        .checked("HiSIM-HV2 depletion symmetric source surface")?;
        Ok(HiSimHvDepletionChannelEvaluation {
            frontend,
            effective_drain,
            source,
            drain,
            channel,
            terminal_charges,
            correction,
            modified_source_surface,
            classical_oxide_capacitance: self.frontend.oxide.zero_field_capacitance(),
            drain_current_a,
        })
    }

    /// Evaluate current with independent external and intrinsic access coordinates.
    /// # Errors
    /// Invalid trial state or mismatched prepared context.
    pub fn evaluate_with_access(
        &self,
        bias: HiSimHvIntrinsicBias,
        external: HiSimHvExternalBias,
        transport: &PreparedHiSimHvTransport,
    ) -> Result<HiSimHvDepletionChannelWithAccessPoint, ModelError> {
        let intrinsic = self.evaluate(bias, transport)?;
        let access = self.access.evaluate(&HiSimHvAccessInputs {
            intrinsic: bias,
            external,
            preaccess_current_a: intrinsic.channel.drain_current_a
                + intrinsic.correction.parasitic_channel_current_a,
            postaccess_current_a: intrinsic.correction.ohmic_leakage_a,
            conducting_channel: true,
        })?;
        Ok(HiSimHvDepletionChannelWithAccessPoint { intrinsic, access })
    }
}
