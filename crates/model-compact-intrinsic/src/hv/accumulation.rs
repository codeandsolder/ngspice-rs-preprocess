//! Normal-mode HiSIM-HV accumulation surface potential from the native
//! three-degree depletion polynomial and the version-specific confinement.
use super::HiSimHvIntrinsicScalar as Jet;
use crate::{ModelError, ModelVariant};

/// Inputs to the normal-mode accumulation branch, where `Vgp < Vbscl`.
/// All voltage and temperature derivatives use the intrinsic four axes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvAccumulationInputs {
    pub effective_gate: Jet,
    pub confined_body: Jet,
    pub inverse_thermal_voltage: Jet,
    pub poisson_coefficient: Jet,
    pub oxide_capacitance: Jet,
    /// Device bandgap, needed by HV1's native surface confinement.
    pub bandgap: Jet,
    /// Twice the bulk potential `Pb2`, needed by HV1.
    pub twice_bulk_potential: Jet,
    /// Normalized model `VGS_MIN`, needed by HV2.
    pub minimum_gate_voltage: f64,
}
/// Native accumulation output. Source and drain potentials coincide,
/// channel inversion charge and channel conduction are both zero.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvAccumulationPoint {
    pub surface_potential: Jet,
    pub bulk_charge_density: Jet,
}

/// Solve the original normal-mode accumulation constitutive law.
///
/// The native
/// closed-form cubic is evaluated in a cancellation-resistant form; HV1
/// and HV2 use their distinct source-defined surface confinement limits.
/// # Errors
/// Rejects invalid variant, bias, capacitance, or nonfinite output.
pub fn evaluate_hisim_hv_accumulation(
    variant: ModelVariant,
    inputs: &HiSimHvAccumulationInputs,
) -> Result<HiSimHvAccumulationPoint, ModelError> {
    let HiSimHvAccumulationInputs {
        effective_gate,
        confined_body,
        inverse_thermal_voltage: beta,
        poisson_coefficient,
        oxide_capacitance,
        bandgap,
        twice_bulk_potential,
        minimum_gate_voltage,
    } = *inputs;
    for input in [
        effective_gate,
        confined_body,
        beta,
        poisson_coefficient,
        oxide_capacitance,
    ] {
        input.checked("HiSIM-HV accumulation input")?;
    }
    if effective_gate.value > confined_body.value
        || beta.value <= 0.
        || poisson_coefficient.value <= 0.
        || oxide_capacitance.value <= 0.
    {
        return Err(ModelError::InvalidPhysicalParameter(
            "HiSIM-HV accumulation input",
        ));
    }
    let fac1 = poisson_coefficient / oxide_capacitance;
    let limit = match variant {
        ModelVariant::HiSimHv1 => bandgap - twice_bulk_potential,
        ModelVariant::HiSimHv2 => 2. / beta * (-minimum_gate_voltage / fac1).ln(),
        _ => return Err(ModelError::WrongModelScope),
    }
    .checked("HiSIM-HV accumulation confinement")?;
    if limit.value == 0. {
        return Err(ModelError::InvalidPhysicalParameter(
            "HiSIM-HV accumulation confinement",
        ));
    }
    let overdrive = effective_gate - confined_body;
    let tx = beta * overdrive;
    let ty = 1. / (beta * fac1);
    // Native Cardano expression, with Ac1 = Ac4/(Ac2+Ac31) to
    // avoid subtracting two almost equal positive numbers.
    let cubic_scale = 2. + 3. * std::f64::consts::SQRT_2 * ty;
    let ac4 = 8. * cubic_scale * cubic_scale * cubic_scale;
    let t5 = 9. * ty * (tx - 2.);
    let ac31 = 7. * std::f64::consts::SQRT_2 - t5;
    let ac2 = (ac4 + ac31 * ac31).sqrt();
    let cardano_radicand = ac4 / (ac2 + ac31);
    let cube = cardano_radicand.value.cbrt();
    let acd = cardano_radicand.map(cube, 1. / (3. * cube * cube));
    let acn =
        -4. * std::f64::consts::SQRT_2 - 12. * ty + 2. * acd + std::f64::consts::SQRT_2 * acd * acd;
    let raw_chi = acn / acd;
    // The Cardano root is equivalently the monotone cubic
    // Chi*(1 + beta*fac1/sqrt(2)*(1-Chi/6+Chi²/36)) = TX.
    // Refine by one exact Newton step to avoid cancellation near flatband.
    let weight = beta * fac1 / std::f64::consts::SQRT_2;
    let poly = 1. - raw_chi / 6. + raw_chi * raw_chi / 36.;
    let slope = 1. + weight * (1. - raw_chi / 3. + raw_chi * raw_chi / 12.);
    let chi = raw_chi - (raw_chi * (1. + weight * poly) - tx) / slope;
    let raw = chi / beta;
    let ratio = raw / limit;
    let surface = confined_body + raw / (1. + ratio * ratio).sqrt();
    let bulk = oxide_capacitance * (effective_gate - surface);
    Ok(HiSimHvAccumulationPoint {
        surface_potential: surface.checked("HiSIM-HV accumulation surface")?,
        bulk_charge_density: bulk.checked("HiSIM-HV accumulation charge")?,
    })
}
