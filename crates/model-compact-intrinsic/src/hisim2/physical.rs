//! Geometry and parasitic conductance preparation from the 2.80 temperature routine.
//! Copyright (C) 2014 Hiroshima University & STARC; see package LICENSE.

use std::sync::Arc;

use super::{BodyNetwork, PreparedInstance, PreparedModel, integer, positive, real, slot};
use crate::{BoundParameters, ModelError, ParameterScope};

/// Effective dimensions after metallurgical and short-channel corrections.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EffectiveGeometry {
    /// Effective channel length in meters.
    pub length_m: f64,
    /// Effective width of one finger in meters.
    pub width_per_finger_m: f64,
    /// Effective width of all fingers, excluding multiplicity.
    pub total_width_m: f64,
    /// Short-channel gate length in meters.
    pub short_gate_length_m: f64,
    /// Width-dependent threshold shift in volts.
    pub threshold_shift_v: f64,
    /// STI width coefficient.
    pub sti_width: f64,
    /// Channel length modulation geometry factor.
    pub channel_length_modulation: f64,
}

/// Series resistance and corresponding external and internal conductances.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SeriesResistance {
    /// Resistance before multiplicity scaling, in ohms.
    pub resistance_ohm: f64,
    /// Conductance for an external series branch in siemens.
    pub external_s: f64,
    /// Internal series thermal-noise conductance in siemens.
    pub internal_noise_s: f64,
}

/// Conductances between nodes of the distributed body network, in siemens.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BodyConductance {
    /// External bulk to drain-body node.
    pub drain_bulk: f64,
    /// External bulk to source-body node.
    pub source_bulk: f64,
    /// External bulk to bulk-prime node.
    pub prime_bulk: f64,
    /// Bulk-prime to drain-body node.
    pub drain_prime: f64,
    /// Bulk-prime to source-body node.
    pub source_prime: f64,
}

/// Physical quantities independent of terminal voltages.
#[derive(Clone, Debug, PartialEq)]
pub struct PreparedPhysical {
    /// Effective geometry.
    pub geometry: EffectiveGeometry,
    /// Drain series branch.
    pub drain: SeriesResistance,
    /// Source series branch.
    pub source: SeriesResistance,
    /// Gate conductance when gate resistance is enabled.
    pub gate_s: Option<f64>,
    /// Body conductances when the distributed network is enabled.
    pub body: Option<BodyConductance>,
    kernel_binned: Box<[Option<f64>]>,
}

impl PreparedPhysical {
    /// Binned coefficient in the units used by the original evaluator.
    #[must_use = "kernel coefficients are required for physical evaluation"]
    pub fn kernel_binned_slot(&self, slot: u16) -> Option<f64> {
        self.kernel_binned.get(usize::from(slot)).copied().flatten()
    }
}

const fn finite(v: f64, name: &'static str) -> Result<f64, ModelError> {
    if v.is_finite() {
        Ok(v)
    } else {
        Err(ModelError::InvalidPhysicalParameter(name))
    }
}

fn binned(instance: &PreparedInstance, name: &str) -> Result<f64, ModelError> {
    let id = slot(ParameterScope::Model, name)?;
    instance
        .binned_model_slot(id)
        .ok_or(ModelError::MissingDefaultDependency(id))
}

fn effective_geometry(
    model: &BoundParameters,
    instance: &PreparedInstance,
) -> Result<EffectiveGeometry, ModelError> {
    let gate = instance.geometry();
    let l = gate.length_m;
    let w = gate.width_per_finger_m;
    let lg = l * 1e6;
    let wg = w * 1e6;
    let wl = lg * wg;
    let lln = real(model, "lln")?;
    let wln = real(model, "wln")?;
    let dl = real(model, "xld")?
        + (real(model, "ll")? / 100_f64.powf(lln)) / (l + real(model, "lld")?).powf(lln);
    let dw = real(model, "xwd")?
        + (real(model, "wl")? / 100_f64.powf(wln)) / (w + real(model, "wld")?).powf(wln);
    let length_m = finite(l - (dl + dl), "leff")?;
    if length_m <= 1e-9 {
        return Err(ModelError::InvalidPhysicalParameter("leff"));
    }
    let width_per_finger_m = positive(w - (dw + dw), "weff")?;
    let total_width_m = positive(
        width_per_finger_m * real(instance.parameters(), "nf")?,
        "weff_nf",
    )?;
    let short_gate_length_m = positive(
        l + real(model, "wl1")? / wl.powf(real(model, "wl1p")?),
        "lgatesm",
    )?;
    let threshold_shift_v = finite(
        binned(instance, "wl2")? / wl.powf(real(model, "wl2p")?),
        "dVthsm",
    )?;
    let sti_width = finite(
        binned(instance, "wsti")?
            * (1.0
                + real(model, "wstil")? / (short_gate_length_m * 1e6).powf(real(model, "wstilp")?))
            * (1.0 + real(model, "wstiw")? / wg.powf(real(model, "wstiwp")?)),
        "wsti",
    )?;
    let channel_length_modulation = finite(
        lg.powf(real(model, "clm5")?)
            .mul_add(real(model, "clm6")?, 1.0),
        "clmmod",
    )?;
    Ok(EffectiveGeometry {
        length_m,
        width_per_finger_m,
        total_width_m,
        short_gate_length_m,
        threshold_shift_v,
        sti_width,
        channel_length_modulation,
    })
}

fn series(
    model: &BoundParameters,
    instance: &BoundParameters,
    geometry: EffectiveGeometry,
    square_name: &str,
    resistance_name: &str,
) -> Result<SeriesResistance, ModelError> {
    let sheet = real(model, "rsh")?;
    let specific = real(model, resistance_name)?;
    let mut r = 0.0;
    if sheet > 0.0 {
        r = sheet * real(instance, square_name)?;
    }
    if specific > 0.0 {
        r += specific / geometry.total_width_m;
    }
    let resistance_ohm = finite(r, "series resistance")?;
    let m = real(instance, "m")?;
    let mode = integer(model, "corsrd")?;
    let external_s = if mode < 0 && r > 0.0 {
        finite(m / r, "external series conductance")?
    } else {
        0.0
    };
    let internal_noise_s = if mode > 0 && integer(model, "cothrml")? != 0 && r > 0.0 {
        finite(m / r, "internal series conductance")?
    } else {
        0.0
    };
    Ok(SeriesResistance {
        resistance_ohm,
        external_s,
        internal_noise_s,
    })
}

fn gate(
    model: &BoundParameters,
    instance: &PreparedInstance,
    geometry: EffectiveGeometry,
) -> Result<Option<f64>, ModelError> {
    let p = instance.parameters();
    if integer(p, "corg")? != 1 {
        return Ok(None);
    }
    let ngcon = positive(real(p, "ngcon")?, "ngcon")?;
    let l = instance.geometry().length_m;
    let r = finite(
        real(model, "rshg")? * (real(p, "xgw")? + geometry.width_per_finger_m / (3.0 * ngcon))
            / (ngcon * (l - real(p, "xgl")?) * real(p, "nf")?),
        "gate resistance",
    )?;
    let m = real(p, "m")?;
    finite(if r > 1e-3 { m / r } else { m * 1e3 }, "gate conductance").map(Some)
}

fn body(
    model: &BoundParameters,
    instance: &PreparedInstance,
) -> Result<Option<BodyConductance>, ModelError> {
    if instance.topology().body_network == BodyNetwork::Collapsed {
        return Ok(None);
    }
    let p = instance.parameters();
    let m = real(p, "m")?;
    let gbmin = real(model, "gbmin")?;
    let g = |name| -> Result<f64, ModelError> {
        let r = real(p, name)?;
        finite(
            if r < 1e-3 {
                m * 1e3
            } else {
                m * (gbmin + 1.0 / r)
            },
            "body conductance",
        )
    };
    Ok(Some(BodyConductance {
        drain_bulk: g("rbdb")?,
        source_bulk: g("rbsb")?,
        prime_bulk: g("rbpb")?,
        drain_prime: g("rbpd")?,
        source_prime: g("rbps")?,
    }))
}

fn kernel_units(instance: &PreparedInstance) -> Result<Box<[Option<f64>]>, ModelError> {
    let mut values = instance.binned.clone();
    // The source converts only these fields after binning, not their card values.
    for (names, factor) in [
        (&["nsubc", "nsubp", "nsti", "nover"][..], 1e6),
        (
            &[
                "nsubpsti1",
                "nsubcsti1",
                "muesti1",
                "vmax",
                "glksd1",
                "nfalp",
            ][..],
            0.01,
        ),
        (&["wfc", "nftrp"][..], 1e4),
        (&["glksd2", "gleak2", "glkb2", "gidl2"][..], 100.0),
        (&["gidl1"][..], 0.1),
    ] {
        for &name in names {
            let id = slot(ParameterScope::Model, name)?;
            let v = binned(instance, name)? * factor;
            if !v.is_finite() {
                return Err(ModelError::InvalidBinningParameter(id));
            }
            values[usize::from(id)] = Some(v);
        }
    }
    Ok(values)
}

/// Prepare effective geometry, parasitics and evaluator-unit binning.
///
/// # Errors
/// Model/instance context mismatch, invalid geometry or non-finite results.
pub fn prepare_physical(
    model: &PreparedModel,
    instance: &PreparedInstance,
) -> Result<PreparedPhysical, ModelError> {
    if !Arc::ptr_eq(&model.identity, &instance.model_identity) {
        return Err(ModelError::MismatchedPreparedContext);
    }
    let mp = model.parameters();
    let ip = instance.parameters();
    let geometry = effective_geometry(mp, instance)?;
    Ok(PreparedPhysical {
        geometry,
        drain: series(mp, ip, geometry, "nrd", "rd")?,
        source: series(mp, ip, geometry, "nrs", "rs")?,
        gate_s: gate(mp, instance, geometry)?,
        body: body(mp, instance)?,
        kernel_binned: kernel_units(instance)?,
    })
}
