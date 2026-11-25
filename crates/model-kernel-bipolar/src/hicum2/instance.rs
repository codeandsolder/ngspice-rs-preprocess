//! HICUM/L2 native SPICE instance binding, outside the solver hot path.
//!
//! The HICUM 2.4.0 Verilog-A source has only source-model coefficients;
//! the ngspice LEVEL=8 Q-device frontend additionally offers AREA, M,
//! TEMP, DT, OFF and IC. The physical coefficient scaling below follows
//! ngspice src/spicelib/devices/hicum2/hicum2setup.c, not a generic BJT
//! scaling approximation. AREA and M are deliberately *not* conflated:
//! the perimeter currents and capacitors scale with M only, whereas
//! intrinsic collector/emitter parameters scale with AREA*M.
//!
//! All scaling occurs on a separate per-instance nominal object, before
//! topology reduction and effective-temperature preparation. The circuit
//! owns trial/accepted states and transient initial-condition use.

use super::{temperature, topology};
use crate::{ModelError, Temperature};

const CELSIUS_ZERO: f64 = 273.15;

/// Instance-only frontend settings. The C/B/E/S/thermal pin resolution
/// belongs to the deck elaborator, not to the physical model parameters.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Options {
    /// ngspice Q AREA multiplier (distinct from M).
    pub area: f64,
    /// ngspice Q parallel-device multiplicity.
    pub multiplicity: f64,
    /// Device ambient override, in Celsius, or global circuit ambient.
    pub temp_celsius: Option<f64>,
    /// Device-specific additive ambient offset in kelvin.
    pub dt_kelvin: f64,
    /// Initial solver condition only. Does not change physical I/Q laws.
    pub off: bool,
    /// Optional VBE/VCE/VCS initial conditions, in this source order.
    pub initial: InitialVoltages,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct InitialVoltages {
    pub vbe: Option<f64>,
    pub vce: Option<f64>,
    pub vcs: Option<f64>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            area: 1.0,
            multiplicity: 1.0,
            temp_celsius: None,
            dt_kelvin: 0.0,
            off: false,
            initial: InitialVoltages::default(),
        }
    }
}

impl Options {
    /// # Errors
    /// Invalid area, multiplier, local temperature offset or IC vector.
    pub fn validate(self) -> Result<(), ModelError> {
        for (name, value) in [("area", self.area), ("m", self.multiplicity)] {
            if !value.is_finite() || value <= 0.0 {
                return Err(ModelError::InvalidParameter(name));
            }
        }
        if self.temp_celsius.is_some_and(|value| !value.is_finite()) {
            return Err(ModelError::InvalidParameter("temp"));
        }
        if !self.dt_kelvin.is_finite() {
            return Err(ModelError::InvalidParameter("dt"));
        }
        for (name, value) in [
            ("ic_vbe", self.initial.vbe),
            ("ic_vce", self.initial.vce),
            ("ic_vcs", self.initial.vcs),
        ] {
            if value.is_some_and(|value| !value.is_finite()) {
                return Err(ModelError::InvalidParameter(name));
            }
        }
        let scale = self.area * self.multiplicity;
        if !scale.is_finite() || scale == 0.0 {
            return Err(ModelError::InvalidParameter("area*m"));
        }
        Ok(())
    }
}

/// Independently scaled device instance, including immutable topology.
///
/// The original nominal model can be reused across instances; the
/// frontend also keeps each instance's initial conditions separately.
#[derive(Clone, Copy, Debug)]
pub struct Bound {
    pub nominal: temperature::Nominal,
    pub topology: topology::Prepared,
    /// Absolute global/device-specified ambient before model-card DT.
    pub ambient: Temperature,
    pub initial: InitialVoltages,
    pub off: bool,
}

impl Bound {
    /// Construct one instance from the model's verified scalar parameters,
    /// global circuit ambient and pre-elaborated instance-only settings.
    ///
    /// # Errors
    /// Rejects invalid multipliers, effective instance temperature,
    /// scaled coefficient overflow, or source topology inconsistencies.
    pub fn new(
        model: &temperature::Nominal,
        global_ambient: Temperature,
        options: Options,
    ) -> Result<Self, ModelError> {
        options.validate()?;
        let base_kelvin = options
            .temp_celsius
            .map_or_else(|| global_ambient.kelvin(), |value| value + CELSIUS_ZERO);
        let ambient = Temperature::new(base_kelvin + options.dt_kelvin)?;
        let nominal = scale(model, options)?;
        let topology = nominal.physical_topology()?;
        Ok(Self {
            nominal,
            topology,
            ambient,
            initial: options.initial,
            off: options.off,
        })
    }

    /// Re-prepare the same nominal per-instance device at a thermal-node
    /// Newton trial, with no mutation of shared model or accepted state.
    ///
    /// # Errors
    /// Returns the named invalid transformed physical parameter.
    pub fn prepare(&self, thermal_rise_kelvin: f64) -> Result<temperature::Prepared, ModelError> {
        self.nominal.prepare(self.ambient, thermal_rise_kelvin)
    }
}

/// Scale all nominal HICUM coefficients before temperature preparation.
///
/// Perimeter and substrate parameters are intentionally *not* scaled
/// uniformly. No model-card name lookup or dynamic dispatch is involved.
///
/// # Errors
/// Rejects arithmetic overflow or invalid scaled nominal coefficients.
pub fn scale(
    model: &temperature::Nominal,
    options: Options,
) -> Result<temperature::Nominal, ModelError> {
    options.validate()?;
    let mut nominal = *model;
    let area_m = options.area * options.multiplicity;
    let inverse = 1.0 / area_m;
    let m = options.multiplicity;
    if !inverse.is_finite() {
        return Err(ModelError::InvalidParameter("area*m"));
    }
    scale_intrinsic(&mut nominal, area_m);
    scale_peripheral(&mut nominal, m);
    scale_resistors(&mut nominal, inverse);
    scale_noise(&mut nominal, model, m);
    if ![
        nominal.transport.qp0,
        nominal.transport.c10,
        nominal.transport.icbar,
        nominal.auxiliary.rth,
        nominal.auxiliary.cth,
        nominal
            .transport
            .junction
            .intrinsic_emitter
            .zero_bias_capacitance,
        nominal.diodes.ibeis,
        nominal.diodes.ireis,
        nominal.auxiliary.ibets,
        nominal
            .transport
            .junction
            .peripheral_emitter
            .zero_bias_capacitance,
        nominal.diodes.ibeps,
        nominal.diodes.ireps,
        nominal.auxiliary.cbepar,
        nominal.diodes.ibcis,
        nominal
            .transport
            .junction
            .intrinsic_collector
            .zero_bias_capacitance,
        nominal.auxiliary.qavl,
        nominal
            .transport
            .junction
            .external_collector
            .external_zero_bias_capacitance,
        nominal
            .transport
            .junction
            .external_collector
            .peripheral_zero_bias_capacitance,
        nominal.auxiliary.cbcpar,
        nominal.diodes.ibcxs,
        nominal.auxiliary.re,
        nominal.transport.rci0,
        nominal.auxiliary.rbx,
        nominal.auxiliary.rcx,
        nominal.auxiliary.rbi0,
        nominal.noise.kf,
        nominal.noise.kfre,
    ]
    .iter()
    .all(|value| value.is_finite())
    {
        return Err(ModelError::InvalidParameter("instance_scaled_coefficient"));
    }
    Ok(nominal)
}
fn scale_intrinsic(nominal: &mut temperature::Nominal, area_m: f64) {
    // Intrinsic transport and thermal network.
    nominal.transport.qp0 *= area_m;
    // Applying the scale in two steps avoids overflowing AREA*M squared
    // when the very small physical C10 coefficient still fits in f64.
    nominal.transport.c10 *= area_m;
    nominal.transport.c10 *= area_m;
    nominal.transport.icbar *= area_m;
    nominal.auxiliary.rth /= area_m;
    nominal.auxiliary.cth *= area_m;
    // Intrinsic B-E and B-C, avalanche, tunneling (regardless of tunode).
    nominal
        .transport
        .junction
        .intrinsic_emitter
        .zero_bias_capacitance *= area_m;
    nominal.diodes.ibeis *= area_m;
    nominal.diodes.ireis *= area_m;
    nominal.auxiliary.ibets *= area_m;
    nominal.diodes.ibcis *= area_m;
    nominal
        .transport
        .junction
        .intrinsic_collector
        .zero_bias_capacitance *= area_m;
    nominal.auxiliary.qavl *= area_m;
}

fn scale_peripheral(nominal: &mut temperature::Nominal, m: f64) {
    nominal
        .transport
        .junction
        .peripheral_emitter
        .zero_bias_capacitance *= m;
    nominal.diodes.ibeps *= m;
    nominal.diodes.ireps *= m;
    nominal.auxiliary.cbepar *= m;
    nominal
        .transport
        .junction
        .external_collector
        .external_zero_bias_capacitance *= m;
    nominal
        .transport
        .junction
        .external_collector
        .peripheral_zero_bias_capacitance *= m;
    nominal.auxiliary.cbcpar *= m;
    nominal.diodes.ibcxs *= m;
}

fn scale_resistors(nominal: &mut temperature::Nominal, inverse: f64) {
    // Native HICUM does *not* area-scale RSU, CSU or substrate families.
    nominal.auxiliary.re *= inverse;
    nominal.transport.rci0 *= inverse;
    nominal.auxiliary.rbx *= inverse;
    nominal.auxiliary.rcx *= inverse;
    nominal.auxiliary.rbi0 *= inverse;
}

fn scale_noise(nominal: &mut temperature::Nominal, model: &temperature::Nominal, m: f64) {
    // Do not form 0 * infinity. A disabled flicker contribution stays
    // identically zero even for extreme but otherwise valid M.
    if nominal.noise.kf != 0.0 {
        nominal.noise.kf *= m.powf(1.0 - model.noise.af);
    }
    if nominal.noise.kfre != 0.0 {
        nominal.noise.kfre *= m.powf(1.0 - model.noise.afre);
    }
}
