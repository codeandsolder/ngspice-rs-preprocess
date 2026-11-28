//! Temperature-independent geometry and electrostatic setup expressions from
//! the HiSIM-HV1/HV2 `*temp.c` entry points in ngspice 47.
//!
//! Source model lengths are not all in SI: `ll`/`wl` are converted through
//! `modelMKS` using centimeters raised to `LLN`/`WLN` before evaluating them
//! against the corrected gate dimensions expressed in meters.

use super::{
    PreparedHiSimHvModel,
    instance::{HiSimHvBinningGeometry, instance_real, model_real},
    source_binned_at_name,
};
use crate::{ModelError, ParameterValue};

/// Physical channel geometry and the size-dependent, temperature-independent
/// terms calculated at the start of the HiSIM-HV temperature preparation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvEffectiveGeometry {
    /// `Lgate - (dL + dLLD)` in meters; C rejects nonpositive values.
    pub channel_length_m: f64,
    /// `Wgate - 2*dW` in meters per finger; C rejects nonpositive values.
    pub channel_width_per_finger_m: f64,
    /// `Wgate - 2*dWLD` in meters per finger.
    pub lateral_diffusion_width_per_finger_m: f64,
    /// `Wgate - 2*dWCV` in meters per finger.
    pub capacitance_width_per_finger_m: f64,
    /// `channel_width_per_finger_m * NF`, in meters.
    pub channel_width_total_m: f64,
    /// `capacitance_width_per_finger_m * NF`, in meters.
    pub capacitance_width_total_m: f64,
    /// Length after the `WL1` correction, used by the STI model, in meters.
    pub small_device_gate_length_m: f64,
    /// `WL2 / (WG*LG)^WL2P` using the upstream square-micrometre gate-area expression.
    pub small_device_threshold_correction_v: f64,
    /// Size-and-layout-corrected STI width parameter.
    pub width_sti: f64,
    /// C's size-dependent channel-length-modulation factor.
    pub channel_length_modulation_factor: f64,
}

const fn finite(name: &'static str, value: f64) -> Result<f64, ModelError> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(ModelError::InvalidPhysicalParameter(name))
    }
}

/// Evaluate the common HV1/HV2 source equations without borrowing from a
/// generic MOS approximation. `binned` must belong to this exact instance.
///
/// This is *only* the geometry/temperature-independent start of `*temp.c`;
/// it is not a complete temperature, self-heating, or nonlinear state kernel.
pub(super) fn prepare_effective_geometry(
    model: &PreparedHiSimHvModel,
    values: &[Option<ParameterValue>],
    binning: HiSimHvBinningGeometry,
    binned: &[Option<f64>],
) -> Result<HiSimHvEffectiveGeometry, ModelError> {
    let lgate = binning.gate_length_m;
    let wgate = binning.gate_width_per_finger_m;
    let length_um = lgate * 1e6;
    let width_um = wgate * 1e6;
    let area_um2 = length_um * width_um;
    let lln = model_real(model, "lln")?;
    let wln = model_real(model, "wln")?;

    // hsmhv[2]set.c: modelMKS->ll = ll / pow(100, lln), and likewise wl.
    // hsmhv[2]temp.c: geometry corrections use LG and WG in micrometres, WL in square micrometres.
    let length_correction = (model_real(model, "ll")? / 100f64.powf(lln))
        / (lgate + model_real(model, "lld")?).powf(lln);
    let width_correction = (model_real(model, "wl")? / 100f64.powf(wln))
        / (wgate + model_real(model, "wld")?).powf(wln);
    let length_correction = finite(
        "HiSIM-HV metallurgical length correction",
        length_correction,
    )?;
    let width_correction = finite("HiSIM-HV metallurgical width correction", width_correction)?;

    let d_l = model_real(model, "xld")? + length_correction;
    let d_l_ld = model_real(model, "xldld")? + length_correction;
    let d_w = model_real(model, "xwd")? + width_correction;
    let diffusion_width_correction = model_real(model, "xwdld")? + width_correction;
    let capacitance_width_correction = model_real(model, "xwdc")? + width_correction;

    let effective_length = finite("HiSIM-HV effective channel length", lgate - (d_l + d_l_ld))?;
    if effective_length <= 0.0 {
        return Err(ModelError::InvalidPhysicalParameter(
            "HiSIM-HV effective channel length",
        ));
    }
    let effective_width = finite(
        "HiSIM-HV effective channel width",
        2.0f64.mul_add(-d_w, wgate),
    )?;
    if effective_width <= 0.0 {
        return Err(ModelError::InvalidPhysicalParameter(
            "HiSIM-HV effective channel width",
        ));
    }
    let lateral_diffusion_width = finite(
        "HiSIM-HV lateral diffusion channel width",
        2.0f64.mul_add(-diffusion_width_correction, wgate),
    )?;
    let capacitance_width = finite(
        "HiSIM-HV capacitance channel width",
        2.0f64.mul_add(-capacitance_width_correction, wgate),
    )?;

    let lgatesm = finite(
        "HiSIM-HV short-channel gate length",
        lgate + model_real(model, "wl1")? / area_um2.powf(model_real(model, "wl1p")?),
    )?;
    let dvthsm = finite(
        "HiSIM-HV short-channel threshold correction",
        source_binned_at_name(model, binned, "wl2")? / area_um2.powf(model_real(model, "wl2p")?),
    )?;
    let wsti = finite(
        "HiSIM-HV STI width correction",
        source_binned_at_name(model, binned, "wsti")?
            * (1.0
                + model_real(model, "wstil")? / (lgatesm * 1e6).powf(model_real(model, "wstilp")?))
            * (1.0 + model_real(model, "wstiw")? / width_um.powf(model_real(model, "wstiwp")?)),
    )?;
    let clmmod = finite(
        "HiSIM-HV channel-length modulation factor",
        length_um
            .powf(model_real(model, "clm5")?)
            .mul_add(model_real(model, "clm6")?, 1.0),
    )?;
    let nf = instance_real(values, model.variant(), "nf")?;
    Ok(HiSimHvEffectiveGeometry {
        channel_length_m: effective_length,
        channel_width_per_finger_m: effective_width,
        lateral_diffusion_width_per_finger_m: lateral_diffusion_width,
        capacitance_width_per_finger_m: capacitance_width,
        channel_width_total_m: finite("HiSIM-HV total effective width", effective_width * nf)?,
        capacitance_width_total_m: finite(
            "HiSIM-HV total capacitance width",
            capacitance_width * nf,
        )?,
        small_device_gate_length_m: lgatesm,
        small_device_threshold_correction_v: dvthsm,
        width_sti: wsti,
        channel_length_modulation_factor: clmmod,
    })
}
