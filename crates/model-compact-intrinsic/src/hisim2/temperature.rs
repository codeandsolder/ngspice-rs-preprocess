//! Band-gap and intrinsic carrier preparation from the 2.80 temperature routine.
//! Copyright (C) 2014 Hiroshima University & STARC; see package LICENSE.

use std::sync::Arc;

use super::{PreparedInstance, PreparedModel, positive, real, slot};
use crate::{ModelError, ParameterScope};

// The pinned HiSIM2 implementation uses these historical constants, not CODATA.
const CHARGE: f64 = 1.602_191_8e-19;
const BOLTZMANN: f64 = 1.380_622_6e-23;
const INTRINSIC_DENSITY: f64 = 1.04e16;

/// Temperature-dependent material quantities independent of terminal bias.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreparedTemperature {
    /// Actual lattice temperature in kelvin.
    pub kelvin: f64,
    /// Nominal model temperature in kelvin.
    pub nominal_kelvin: f64,
    /// Temperature ratio relative to nominal.
    pub ratio: f64,
    /// Band gap at nominal temperature, in electron volts.
    pub nominal_band_gap_ev: f64,
    /// Band gap at device temperature, in electron volts.
    pub band_gap_ev: f64,
    /// Inverse thermal voltage in inverse volts.
    pub beta: f64,
    /// Inverse nominal thermal voltage in inverse volts.
    pub nominal_beta: f64,
    /// Intrinsic carrier density in inverse cubic meters.
    pub intrinsic_density_per_m3: f64,
}

fn binned(instance: &PreparedInstance, name: &str) -> Result<f64, ModelError> {
    let id = slot(ParameterScope::Model, name)?;
    instance
        .binned_model_slot(id)
        .ok_or(ModelError::MissingDefaultDependency(id))
}

/// Evaluate material quantities using the original model and instance temperature.
///
/// # Errors
/// Mismatched preparation context, invalid band gap or non-finite quantities.
pub fn prepare_temperature(
    model: &PreparedModel,
    instance: &PreparedInstance,
) -> Result<PreparedTemperature, ModelError> {
    if !Arc::ptr_eq(&model.identity, &instance.model_identity) {
        return Err(ModelError::MismatchedPreparedContext);
    }
    let kelvin = instance.temperature_kelvin();
    let nominal_kelvin = real(model.parameters(), "tnom")? + 273.15;
    let ratio = kelvin / nominal_kelvin;
    let nominal_band_gap_ev = positive(
        binned(instance, "eg0")? - nominal_kelvin * 1.0e-7f64.mul_add(nominal_kelvin, 90.25e-6),
        "nominal band gap",
    )?;
    let delta = kelvin - nominal_kelvin;
    let delta_squared = kelvin * kelvin - nominal_kelvin * nominal_kelvin;
    let band_gap_ev = positive(
        nominal_band_gap_ev
            - binned(instance, "bgtmp1")? * delta
            - binned(instance, "bgtmp2")? * delta_squared,
        "band gap",
    )?;
    let beta = CHARGE / (BOLTZMANN * kelvin);
    let nominal_beta = CHARGE / (BOLTZMANN * nominal_kelvin);
    let intrinsic_density_per_m3 = positive(
        INTRINSIC_DENSITY
            * ratio.powf(1.5)
            * (-band_gap_ev / 2.0 * beta + nominal_band_gap_ev / 2.0 * nominal_beta).exp(),
        "intrinsic carrier density",
    )?;
    Ok(PreparedTemperature {
        kelvin,
        nominal_kelvin,
        ratio,
        nominal_band_gap_ev,
        band_gap_ev,
        beta,
        nominal_beta,
        intrinsic_density_per_m3,
    })
}
