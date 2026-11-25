//! The bandgap, inverse thermal-voltage and intrinsic-carrier precomputation
//! from ngspice 47 `hsmhvtemp_eval.h` and `hsmhv2temp_eval.h`.
//!
//! These expressions are the common leading temperature dependency, **not**
//! the complete mobility/velocity/noise/thermal HiSIM-HV device evaluation.

use std::sync::OnceLock;

use super::{
    PreparedHiSimHvModel, compiled_hv_by_family, instance::PreparedHiSimHvInstance, lookup_slot,
    source_real_at,
};
use crate::{ModelError, ModelVariant, ParameterScope, ParameterValue};

// The published HiSIM-HV model's constants differ slightly from modern CODATA.
// Preserve the actual upstream values for numerical reference fidelity.
const ELECTRON_CHARGE_C: f64 = 1.602_191_8e-19;
const BOLTZMANN_J_PER_K: f64 = 1.380_622_6e-23;
const INTRINSIC_CARRIER_DENSITY_M3: f64 = 1.04e16;
const CELSIUS_TO_KELVIN: f64 = 273.15;

/// Temperature-dependent values shared by the two HiSIM-HV implementations.
/// Nonlinear currents, terminal charges, full electrothermal feedback and noise
/// are not evaluated by this preparation step.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreparedHiSimHvTemperature {
    /// Model TNOM card value converted from degrees Celsius to kelvin.
    pub nominal_temperature_kelvin: f64,
    /// `CKTtemp` + instance `DTEMP`, excluding local self-heating.
    pub ambient_device_temperature_kelvin: f64,
    /// Device temperature including the supplied thermal rise, if any.
    pub device_temperature_kelvin: f64,
    /// Device temperature divided by nominal temperature.
    pub temperature_ratio: f64,
    /// Bandgap at model nominal temperature in eV.
    pub nominal_bandgap_ev: f64,
    /// Bandgap at device temperature in eV.
    pub bandgap_ev: f64,
    /// Partial temperature derivative of the bandgap in eV/K.
    pub bandgap_derivative_ev_per_kelvin: f64,
    /// sqrt of the bandgap; the C-model's `sqrt_eg` field.
    pub sqrt_bandgap: f64,
    /// C-model `egp12` bandgap-related square root.
    pub ig_bandgap_sqrt: f64,
    /// C-model `egp32` bandgap-related 3/2 power.
    pub ig_bandgap_three_halves: f64,
    /// Inverse thermal voltage, `QE / (KB * T)`, in 1/V.
    pub inverse_thermal_voltage: f64,
    /// Inverse thermal voltage at TNOM, in 1/V.
    pub nominal_inverse_thermal_voltage: f64,
    /// Derivative of inverse thermal voltage with respect to device temperature.
    pub inverse_thermal_voltage_derivative: f64,
    /// Intrinsic carrier concentration (`nin`) in 1/m³, using upstream constants.
    pub intrinsic_carrier_concentration_m3: f64,
    /// Derivative of carrier concentration with respect to device temperature.
    pub intrinsic_carrier_derivative_m3_per_kelvin: f64,
}

/// A once-resolved table of model and instance IDs needed by the repeated
/// temperature evaluation. No parameter-name resolution in the hot path.
#[derive(Clone, Copy)]
struct TemperatureSlots {
    tnom: u16,
    dtemp: u16,
    eg0: u16,
    bgtmp1: u16,
    bgtmp2: u16,
    egig: u16,
    igtemp2: u16,
    igtemp3: u16,
}

static HV1_TEMPERATURE_SLOTS: OnceLock<Result<TemperatureSlots, ModelError>> = OnceLock::new();
static HV2_TEMPERATURE_SLOTS: OnceLock<Result<TemperatureSlots, ModelError>> = OnceLock::new();

fn compile_temperature_slots(variant: ModelVariant) -> Result<TemperatureSlots, ModelError> {
    let dtemp = variant
        .parameter(ParameterScope::Instance, "dtemp")
        .ok_or_else(|| ModelError::UnknownParameter("dtemp".to_owned()))?
        .slot;
    Ok(TemperatureSlots {
        tnom: lookup_slot(variant, "tnom")?,
        dtemp,
        eg0: lookup_slot(variant, "eg0")?,
        bgtmp1: lookup_slot(variant, "bgtmp1")?,
        bgtmp2: lookup_slot(variant, "bgtmp2")?,
        egig: lookup_slot(variant, "egig")?,
        igtemp2: lookup_slot(variant, "igtemp2")?,
        igtemp3: lookup_slot(variant, "igtemp3")?,
    })
}

fn temperature_slots(variant: ModelVariant) -> Result<TemperatureSlots, ModelError> {
    compiled_hv_by_family(
        variant,
        &HV1_TEMPERATURE_SLOTS,
        &HV2_TEMPERATURE_SLOTS,
        compile_temperature_slots,
    )
    .copied()
}

fn binned_scalar(instance: &PreparedHiSimHvInstance, slot: u16) -> Result<f64, ModelError> {
    instance
        .binned_model_slot(slot)
        .ok_or(ModelError::MissingDefaultDependency(slot))
}

fn checked_positive(name: &'static str, value: f64) -> Result<f64, ModelError> {
    if value.is_finite() && value > 0.0 {
        Ok(value)
    } else {
        Err(ModelError::InvalidPhysicalParameter(name))
    }
}

const fn checked_finite(name: &'static str, value: f64) -> Result<f64, ModelError> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(ModelError::InvalidPhysicalParameter(name))
    }
}

fn intrinsic_carrier_concentration(
    ratio: f64,
    bandgap: f64,
    nominal_bandgap: f64,
    inverse_thermal_voltage: f64,
    nominal_inverse_thermal_voltage: f64,
) -> Result<(f64, f64), ModelError> {
    let exp = (nominal_bandgap / 2.0)
        .mul_add(
            nominal_inverse_thermal_voltage,
            -bandgap / 2.0 * inverse_thermal_voltage,
        )
        .exp();
    let carrier = checked_positive(
        "HiSIM-HV intrinsic carrier concentration",
        INTRINSIC_CARRIER_DENSITY_M3 * ratio.powf(1.5) * exp,
    )?;
    Ok((carrier, exp))
}

/// Calculate the common *leading* temperature laws in both HiSIM-HV sources.
///
/// The caller supplies the circuit temperature **in kelvin** and the device's
/// local self-heating temperature rise **in kelvin**; this rise does not mutate
/// the ambient circuit temperature. Both may vary per candidate/iteration.
///
/// # Errors
/// Returns errors on nonphysical temperatures, mismatched source variants,
/// invalid bandgaps, missing prepared model/instance input or overflowing
/// thermal concentrations. No approximate fallback is performed.
pub fn prepare_hisim_hv_temperature(
    model: &PreparedHiSimHvModel,
    instance: &PreparedHiSimHvInstance,
    circuit_temperature_kelvin: f64,
    local_self_heating_kelvin: f64,
) -> Result<PreparedHiSimHvTemperature, ModelError> {
    model.validate_instance(instance)?;
    let slots = temperature_slots(model.variant())?;
    let nominal = checked_positive(
        "HiSIM-HV nominal temperature",
        source_real_at(model, slots.tnom)? + CELSIUS_TO_KELVIN,
    )?;
    let dtemp = match instance.parameters().get_slot(slots.dtemp) {
        Some(ParameterValue::Real(value)) => *value,
        _ => return Err(ModelError::MissingDefaultDependency(slots.dtemp)),
    };
    // In the C eval, DTEMP is added only when explicitly given, otherwise it
    // was defaulted to zero during C setup; both yield the same temperature.
    let ambient = checked_positive(
        "HiSIM-HV device ambient temperature",
        circuit_temperature_kelvin + dtemp,
    )?;
    let t = checked_positive(
        "HiSIM-HV heated device temperature",
        ambient + local_self_heating_kelvin,
    )?;
    let ratio = checked_positive("HiSIM-HV temperature ratio", t / nominal)?;

    let egtnom = checked_positive(
        "HiSIM-HV nominal bandgap",
        nominal.mul_add(
            -nominal.mul_add(1e-7, 90.25e-6),
            binned_scalar(instance, slots.eg0)?,
        ),
    )?;
    let bgtmp1 = binned_scalar(instance, slots.bgtmp1)?;
    let bgtmp2 = binned_scalar(instance, slots.bgtmp2)?;
    let diff = t - nominal;
    let diff2 = nominal.mul_add(-nominal, t * t);
    let eg = checked_positive(
        "HiSIM-HV temperature bandgap",
        bgtmp2.mul_add(-diff2, bgtmp1.mul_add(-diff, egtnom)),
    )?;
    let eg_derivative = checked_finite(
        "HiSIM-HV bandgap derivative",
        (2.0 * t).mul_add(-bgtmp2, -bgtmp1),
    )?;
    let inverse_t = 1.0 / t;
    let inverse_tnom = 1.0 / nominal;
    let inverse_t_squared_difference = inverse_tnom.mul_add(-inverse_tnom, inverse_t * inverse_t);
    let t3 = checked_positive(
        "HiSIM-HV gate-current bandgap",
        source_real_at(model, slots.igtemp3)?.mul_add(
            inverse_t_squared_difference,
            source_real_at(model, slots.igtemp2)?.mul_add(
                inverse_t - inverse_tnom,
                egtnom + source_real_at(model, slots.egig)?,
            ),
        ),
    )?;
    let igsqrt = t3.sqrt();
    let beta = checked_positive(
        "HiSIM-HV inverse thermal voltage",
        ELECTRON_CHARGE_C / (BOLTZMANN_J_PER_K * t),
    )?;
    let beta_nom = checked_positive(
        "HiSIM-HV nominal inverse thermal voltage",
        ELECTRON_CHARGE_C / (BOLTZMANN_J_PER_K * nominal),
    )?;
    let beta_derivative = checked_finite(
        "HiSIM-HV inverse thermal voltage derivative",
        -ELECTRON_CHARGE_C / (BOLTZMANN_J_PER_K * t * t),
    )?;
    let (carrier, exp) = intrinsic_carrier_concentration(ratio, eg, egtnom, beta, beta_nom)?;
    let carrier_log_derivative = (beta / 2.0).mul_add(-eg_derivative, -eg / 2.0 * beta_derivative);
    let carrier_derivative = checked_finite(
        "HiSIM-HV intrinsic carrier derivative",
        carrier.mul_add(
            carrier_log_derivative,
            INTRINSIC_CARRIER_DENSITY_M3 * exp * 1.5 * ratio.sqrt() / nominal,
        ),
    )?;

    Ok(PreparedHiSimHvTemperature {
        nominal_temperature_kelvin: nominal,
        ambient_device_temperature_kelvin: ambient,
        device_temperature_kelvin: t,
        temperature_ratio: ratio,
        nominal_bandgap_ev: egtnom,
        bandgap_ev: eg,
        bandgap_derivative_ev_per_kelvin: eg_derivative,
        sqrt_bandgap: eg.sqrt(),
        ig_bandgap_sqrt: igsqrt,
        ig_bandgap_three_halves: checked_positive(
            "HiSIM-HV gate-current bandgap power",
            t3 * igsqrt,
        )?,
        inverse_thermal_voltage: beta,
        nominal_inverse_thermal_voltage: beta_nom,
        inverse_thermal_voltage_derivative: beta_derivative,
        intrinsic_carrier_concentration_m3: carrier,
        intrinsic_carrier_derivative_m3_per_kelvin: carrier_derivative,
    })
}
