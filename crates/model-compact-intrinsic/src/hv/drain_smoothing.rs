//! Native HiSIM-HV effective drain voltage and saturation smoothing.
use super::HiSimHvIntrinsicScalar as Jet;
use crate::{ModelError, ModelVariant};

/// Inputs to the normal-mode `Vdseff` section. Derivatives use intrinsic
/// `Vgs`, `Vds`, `Vbs`, kelvin order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvDrainSmoothingInputs {
    pub drain: Jet,
    pub effective_gate: Jet,
    pub modified_body: Jet,
    pub thermal_voltage: Jet,
    pub oxide_capacitance: Jet,
    /// `qnsub_esi`, prepared from the model's actual channel doping.
    pub charge_density_permittivity: f64,
    /// Geometry- and family-dependent `ddlt` exponent.
    pub exponent: f64,
}
/// Native effective drain voltage and the intermediate saturation limit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvDrainSmoothingPoint {
    pub effective_drain: Jet,
    pub saturation_limit: Jet,
}
fn smooth_positive(value: Jet, delta: f64) -> Jet {
    let root = value.value.hypot(2. * delta);
    let positive = if value.value >= 0. {
        f64::midpoint(value.value, root)
    } else {
        2. * delta * delta / (root - value.value)
    };
    value.map(positive, positive / root)
}
/// Evaluate the original effective channel drain bias, including the small
/// HV2-only offset before the depletion square root.
///
/// The saturation function
/// uses a log-domain equivalent for large drain-to-saturation ratios.
/// # Errors
/// Invalid family, nonpositive physical inputs, or nonfinite derivatives.
pub fn evaluate_hisim_hv_drain_smoothing(
    variant: ModelVariant,
    inputs: &HiSimHvDrainSmoothingInputs,
) -> Result<HiSimHvDrainSmoothingPoint, ModelError> {
    let HiSimHvDrainSmoothingInputs {
        drain,
        effective_gate,
        modified_body,
        thermal_voltage,
        oxide_capacitance,
        charge_density_permittivity,
        exponent,
    } = *inputs;
    for input in [
        drain,
        effective_gate,
        modified_body,
        thermal_voltage,
        oxide_capacitance,
    ] {
        input.checked("HiSIM-HV drain smoothing input")?;
    }
    if drain.value < 0.
        || oxide_capacitance.value <= 0.
        || thermal_voltage.value <= 0.
        || !charge_density_permittivity.is_finite()
        || charge_density_permittivity <= 0.
        || !exponent.is_finite()
        || exponent <= 0.
    {
        return Err(ModelError::InvalidPhysicalParameter(
            "HiSIM-HV drain smoothing input",
        ));
    }
    let epsilon = match variant {
        ModelVariant::HiSimHv1 => 0.,
        ModelVariant::HiSimHv2 => 1e-25,
        _ => return Err(ModelError::WrongModelScope),
    };
    let depletion = charge_density_permittivity / (oxide_capacitance * oxide_capacitance);
    let overdrive = effective_gate - thermal_voltage - modified_body;
    let radicand = smooth_positive(1. + 2. * overdrive / depletion, 1e-3) + epsilon;
    let saturation = smooth_positive(effective_gate + depletion * (1. - radicand.sqrt()), 0.01)
        + 10. * f64::EPSILON;
    // Vds / (1 + (Vds/T10)^ddlt)^(1/ddlt).
    // At zero the right-hand derivative with respect to Vds is exactly one.
    let effective = if drain.value == 0. {
        drain
    } else {
        let log_ratio = exponent * (drain.ln() - saturation.ln());
        if log_ratio.value > 0. {
            saturation * (-(-log_ratio).exp().ln_1p() / exponent).exp()
        } else {
            drain * (-log_ratio.exp().ln_1p() / exponent).exp()
        }
    };
    Ok(HiSimHvDrainSmoothingPoint {
        effective_drain: effective.checked("HiSIM-HV effective drain")?,
        saturation_limit: saturation.checked("HiSIM-HV drain saturation limit")?,
    })
}
