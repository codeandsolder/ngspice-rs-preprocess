//! `B4SOItemp` effective geometry and raw size-dependent parameters.
//! Copyright 2010 Regents of the University of California. See package LICENSE.

use super::{integer, positive, real};
use crate::{BoundParameters, ModelError};

/// Source-effective electrical and charge dimensions (meters).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BsimSoiGeometry {
    pub length: f64,
    pub width_per_finger: f64,
    pub length_correction: f64,
    pub width_correction: f64,
    pub length_cv_correction: f64,
    pub width_cv_correction: f64,
    pub gate_tunneling_length_correction: f64,
    pub leff: f64,
    pub weff: f64,
    pub leff_cv: f64,
    pub weff_cv: f64,
    pub leff_cv_body: f64,
    pub leff_cv_backgate: f64,
    pub drain_diode_width: f64,
    pub source_diode_width: f64,
    pub drain_diode_width_cv: f64,
    pub source_diode_width_cv: f64,
    pub abulk_cv_factor: f64,
    /// Native `Inv_L`, `Inv_W` and `Inv_LW`.
    pub inverse_bins: [f64; 3],
}

fn correction(
    model: &BoundParameters,
    powers: [f64; 2],
    names: [&str; 3],
) -> Result<f64, ModelError> {
    let [length, width] = powers;
    Ok(real(model, names[0])? / length
        + real(model, names[1])? / width
        + real(model, names[2])? / (length * width))
}

pub(super) fn prepare(
    model: &BoundParameters,
    instance: &BoundParameters,
) -> Result<BsimSoiGeometry, ModelError> {
    let length = real(instance, "l")?;
    let width = positive(real(instance, "w")? / real(instance, "nf")?, "finger width")?;
    let length_powers = [
        positive(length.powf(real(model, "lln")?), "LLN")?,
        positive(width.powf(real(model, "lwn")?), "LWN")?,
    ];
    let width_powers = [
        positive(length.powf(real(model, "wln")?), "WLN")?,
        positive(width.powf(real(model, "wwn")?), "WWN")?,
    ];
    let dl = real(model, "lint")? + correction(model, length_powers, ["ll", "lw", "lwl"])?;
    let lcv = correction(model, length_powers, ["llc", "lwc", "lwlc"])?;
    let dlc = real(model, "dlc")? + lcv;
    let dlcig = real(model, "dlcig")? + lcv;
    let dw = real(model, "wint")? + correction(model, width_powers, ["wl", "ww", "wwl"])?;
    let dwc = real(model, "dwc")? + correction(model, width_powers, ["wlc", "wwc", "wwlc"])?;
    let nbc = real(instance, "nbc")?;
    let nseg = real(instance, "nseg")?;
    let width_offset = nbc.mul_add(-real(model, "dwbc")?, width);
    let leff = positive(2.0_f64.mul_add(-dl, length), "leff")?;
    let weff = positive((2. - nbc).mul_add(-dw, width_offset), "weff")?;
    let leff_cv = positive(2.0_f64.mul_add(-dlc, length), "leffCV")?;
    let weff_cv = positive((2. - nbc).mul_add(-dwc, width_offset), "weffCV")?;
    let leff_cv_body = positive(leff_cv - real(model, "dlcb")?, "leffCVb")?;
    let leff_cv_backgate = positive(
        2.0_f64.mul_add(real(model, "dlbg")?, leff_cv_body),
        "leffCVbg",
    )?;
    let drain_contact_width = real(instance, "pdbcp")?;
    let source_contact_width = real(instance, "psbcp")?;
    let abulk_cv_factor = positive(
        1. + (real(model, "clc")? / leff).powf(real(model, "cle")?),
        "abulkCVfactor",
    )?;
    let inverse_bins = if integer(model, "binunit")? == 1 {
        [1e-6 / leff, 1e-6 / weff, 1e-12 / (leff * weff)]
    } else {
        [1. / leff, 1. / weff, 1. / (leff * weff)]
    };
    for factor in inverse_bins {
        positive(factor, "binning factor")?;
    }
    let geometry = BsimSoiGeometry {
        length,
        width_per_finger: width,
        length_correction: dl,
        width_correction: dw,
        length_cv_correction: dlc,
        width_cv_correction: dwc,
        gate_tunneling_length_correction: dlcig,
        leff,
        weff,
        leff_cv,
        weff_cv,
        leff_cv_body,
        leff_cv_backgate,
        drain_diode_width: weff / nseg + drain_contact_width,
        source_diode_width: weff / nseg + source_contact_width,
        drain_diode_width_cv: weff_cv / nseg + drain_contact_width,
        source_diode_width_cv: weff_cv / nseg + source_contact_width,
        abulk_cv_factor,
        inverse_bins,
    };
    for value in [
        dlcig,
        geometry.drain_diode_width,
        geometry.source_diode_width,
        geometry.drain_diode_width_cv,
        geometry.source_diode_width_cv,
    ] {
        if !value.is_finite() {
            return Err(ModelError::InvalidPhysicalParameter("derived geometry"));
        }
    }
    Ok(geometry)
}
