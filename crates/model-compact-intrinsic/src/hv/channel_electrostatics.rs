//! Coupled normal-channel electrostatics, using the source-defined prepared
//! material, bias, quantum oxide, threshold, poly and Poisson constitutive laws.
use super::{
    HiSimHvAccumulationInputs, HiSimHvBiasConditioningInputs, HiSimHvBiasConditioningPoint,
    HiSimHvChannelChargeInputs, HiSimHvChannelChargePoint, HiSimHvChannelModulationInputs,
    HiSimHvChannelModulationPoint, HiSimHvChannelRegion, HiSimHvChannelTerminalCharges,
    HiSimHvDrainSmoothingInputs, HiSimHvElectrostaticMaterialPoint, HiSimHvIntrinsicBias,
    HiSimHvIntrinsicScalar as Jet, HiSimHvPolyDepletionPoint, HiSimHvQuantumOxidePoint,
    HiSimHvThresholdPoint, PreparedHiSimHvChannelChargeArea, PreparedHiSimHvChannelModulation,
    PreparedHiSimHvElectrostaticMaterial, PreparedHiSimHvInstance, PreparedHiSimHvModel,
    PreparedHiSimHvPolyDepletion, PreparedHiSimHvQuantumOxide, PreparedHiSimHvTemperature,
    PreparedHiSimHvThreshold, evaluate_hisim_hv_accumulation, evaluate_hisim_hv_bias_conditioning,
    evaluate_hisim_hv_channel_charge, evaluate_hisim_hv_drain_smoothing, instance::model_real,
    prepare_hisim_hv_channel_modulation, prepare_hisim_hv_electrostatic_material,
    prepare_hisim_hv_poly_depletion, prepare_hisim_hv_quantum_oxide, prepare_hisim_hv_threshold,
};
use crate::{ModelError, ModelVariant, MosPolarity};

/// Size-specific preparation for the enhancement-mode channel. HV2 depletion
/// devices have a distinct Poisson and transport law and are not routed here.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreparedHiSimHvChannelElectrostatics {
    variant: ModelVariant,
    material: PreparedHiSimHvElectrostaticMaterial,
    oxide: PreparedHiSimHvQuantumOxide,
    threshold: PreparedHiSimHvThreshold,
    poly: PreparedHiSimHvPolyDepletion,
    modulation: PreparedHiSimHvChannelModulation,
    charge_area: PreparedHiSimHvChannelChargeArea,
    symmetry_voltage: f64,
    minimum_gate_voltage: f64,
}

/// Coupled electrostatic state consumed by the intrinsic transport and leakage
/// kernels. The charge point is per unit area and not terminal-stamped.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvChannelElectrostaticPoint {
    pub material: HiSimHvElectrostaticMaterialPoint,
    pub conditioned: HiSimHvBiasConditioningPoint,
    pub oxide: HiSimHvQuantumOxidePoint,
    pub threshold: HiSimHvThresholdPoint,
    pub poly: HiSimHvPolyDepletionPoint,
    pub effective_drain: Jet,
    pub charge: HiSimHvChannelChargePoint,
    pub modulation: HiSimHvChannelModulationPoint,
    pub terminal_charges: HiSimHvChannelTerminalCharges,
}

/// Prepare the size-specific normal-mode intrinsic electrostatic evaluator.
/// # Errors
/// Invalid prepared context, nonphysical coefficients, or a depletion device.
pub fn prepare_hisim_hv_channel_electrostatics(
    model: &PreparedHiSimHvModel,
    instance: &PreparedHiSimHvInstance,
) -> Result<PreparedHiSimHvChannelElectrostatics, ModelError> {
    let material = prepare_hisim_hv_electrostatic_material(model, instance)?;
    if material.depletion_mode {
        return Err(ModelError::UnsupportedInstanceParameter(
            "HiSIM-HV2 depletion channel electrostatics",
        ));
    }
    let minimum_gate_voltage = if model.variant() == ModelVariant::HiSimHv2 {
        let sign = if model.polarity() == MosPolarity::Nmos {
            1.
        } else {
            -1.
        };
        sign * model_real(model, "vgsmin")?
    } else {
        0.
    };
    let symmetry_voltage = model_real(model, "vzadd0")?;
    let (modulation, charge_area) = prepare_hisim_hv_channel_modulation(model, instance)?;
    Ok(PreparedHiSimHvChannelElectrostatics {
        variant: model.variant(),
        modulation,
        charge_area,
        material,
        oxide: prepare_hisim_hv_quantum_oxide(model, instance)?,
        threshold: prepare_hisim_hv_threshold(model, instance)?,
        poly: prepare_hisim_hv_poly_depletion(model, instance)?,
        symmetry_voltage,
        minimum_gate_voltage,
    })
}

impl PreparedHiSimHvChannelElectrostatics {
    /// Evaluate the coupled normal-mode intrinsic state at normalized bias and
    /// local device temperature, with the original four-axis Jacobian.
    /// # Errors
    /// Invalid physical trial state or failed surface solve.
    pub fn evaluate(
        &self,
        bias: HiSimHvIntrinsicBias,
        temperature: &PreparedHiSimHvTemperature,
    ) -> Result<HiSimHvChannelElectrostaticPoint, ModelError> {
        let material = self.material.evaluate(temperature)?;
        let conditioned = evaluate_hisim_hv_bias_conditioning(&HiSimHvBiasConditioningInputs {
            bias,
            twice_bulk_potential: material.twice_bulk_potential,
            reference_channel_potential: self.material.reference_channel_potential,
            reference_bulk_potential: self.material.reference_bulk_potential,
            symmetry_voltage: self.symmetry_voltage,
        })?;
        let oxide = self.oxide.evaluate(
            bias,
            conditioned.modified_gate,
            material.poisson_coefficient,
        )?;
        let threshold = self.threshold.evaluate(
            &conditioned,
            &oxide,
            &material,
            temperature,
            self.oxide.zero_field_capacitance(),
        )?;
        let poly = self
            .poly
            .evaluate(bias, &conditioned, threshold.total_shift, temperature)?;
        let beta = Jet::coordinate(
            temperature.inverse_thermal_voltage,
            3,
            temperature.inverse_thermal_voltage_derivative,
        );
        let (effective_drain, charge) =
            self.solve(bias, temperature, &material, &conditioned, &oxide, &poly)?;
        let modulation = self.modulation.evaluate(
            &HiSimHvChannelModulationInputs {
                drain_bias: Jet::coordinate(bias.vds, 1, 1.),
                confined_body: conditioned.confined_body,
                inverse_thermal_voltage: beta,
                symmetry_factor: poly.symmetry_factor,
            },
            &charge,
        )?;
        let terminal_charges = self.charge_area.evaluate(&charge)?;
        Ok(HiSimHvChannelElectrostaticPoint {
            material,
            conditioned,
            oxide,
            threshold,
            poly,
            effective_drain,
            charge,
            modulation,
            terminal_charges,
        })
    }
    fn solve(
        &self,
        bias: HiSimHvIntrinsicBias,
        temperature: &PreparedHiSimHvTemperature,
        material: &HiSimHvElectrostaticMaterialPoint,
        conditioned: &HiSimHvBiasConditioningPoint,
        oxide: &HiSimHvQuantumOxidePoint,
        poly: &HiSimHvPolyDepletionPoint,
    ) -> Result<(Jet, HiSimHvChannelChargePoint), ModelError> {
        let beta = Jet::coordinate(
            temperature.inverse_thermal_voltage,
            3,
            temperature.inverse_thermal_voltage_derivative,
        );
        let zero = Jet::constant(0.);
        if poly.effective_gate.value < conditioned.confined_body.value {
            let acc = evaluate_hisim_hv_accumulation(
                self.variant,
                &HiSimHvAccumulationInputs {
                    effective_gate: poly.effective_gate,
                    confined_body: conditioned.confined_body,
                    inverse_thermal_voltage: beta,
                    poisson_coefficient: material.poisson_coefficient,
                    oxide_capacitance: oxide.capacitance,
                    bandgap: Jet::coordinate(
                        temperature.bandgap_ev,
                        3,
                        temperature.bandgap_derivative_ev_per_kelvin,
                    ),
                    twice_bulk_potential: material.twice_bulk_potential,
                    minimum_gate_voltage: self.minimum_gate_voltage,
                },
            )?;
            return Ok((
                zero,
                HiSimHvChannelChargePoint {
                    region: HiSimHvChannelRegion::Accumulation,
                    source_surface: acc.surface_potential,
                    drain_surface: acc.surface_potential,
                    surface_difference: zero,
                    source_bulk_density: acc.bulk_charge_density,
                    source_inversion_density: zero,
                    inversion_voltage: zero,
                    drift_diffusion_integral: zero,
                    bulk_density: acc.bulk_charge_density,
                    inversion_density: zero,
                    drain_fraction: zero,
                    transition_weight: zero,
                },
            ));
        }
        let drain = evaluate_hisim_hv_drain_smoothing(
            self.variant,
            &HiSimHvDrainSmoothingInputs {
                drain: Jet::coordinate(bias.vds, 1, 1.),
                effective_gate: poly.effective_gate,
                modified_body: conditioned.modified_body,
                thermal_voltage: 1. / beta,
                oxide_capacitance: oxide.capacitance,
                charge_density_permittivity: material.charge_density_permittivity,
                exponent: self.material.drain_smoothing_exponent,
            },
        )?
        .effective_drain;
        let charge = evaluate_hisim_hv_channel_charge(&HiSimHvChannelChargeInputs {
            effective_gate: poly.effective_gate,
            confined_body: conditioned.confined_body,
            effective_drain: drain,
            inverse_thermal_voltage: beta,
            oxide_capacitance: oxide.capacitance,
            poisson_coefficient: material.poisson_coefficient,
            minority_coefficient: material.minority_coefficient,
        })?;
        Ok((
            if charge.region == HiSimHvChannelRegion::NoInversion {
                zero
            } else {
                drain
            },
            charge,
        ))
    }
}
