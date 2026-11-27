//! Native HiSIM-HV intrinsic body confinement and zero-drain symmetrization.
use super::{HiSimHvIntrinsicBias, HiSimHvIntrinsicScalar as Jet};
use crate::ModelError;

/// Inputs to the original intrinsic bias conditioning common to HV1 and HV2.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvBiasConditioningInputs {
    pub bias: HiSimHvIntrinsicBias,
    /// Local-temperature channel bulk potential (`Pb2`).
    pub twice_bulk_potential: Jet,
    /// Channel bulk potential at the native 300 K reference (`Pb20`).
    pub reference_channel_potential: f64,
    /// Bulk doping potential at the native 300 K reference (`Pb2c`).
    pub reference_bulk_potential: f64,
    /// Model symmetry voltage (`VZADD0`).
    pub symmetry_voltage: f64,
}
/// Confined and symmetrized voltages with intrinsic Jacobians.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvBiasConditioningPoint {
    pub confined_body: Jet,
    /// Derivative of the body clamp with respect to the original body voltage;
    /// its gradient includes the native second and mixed derivatives.
    pub confinement_slope: Jet,
    pub maximum_body: Jet,
    pub symmetry_offset: Jet,
    pub modified_body: Jet,
    pub modified_drain: Jet,
    pub modified_gate: Jet,
}

fn polynomial4(x: Jet) -> (Jet, Jet) {
    if x.value <= 1. {
        let z = 1. + x * (1. + x * (1. + x * (1. + x)));
        let slope = 1. + x * (2. + x * (3. + 4. * x));
        let reciprocal = 1. / z;
        (reciprocal, slope * reciprocal * reciprocal)
    } else {
        let inverse = 1. / x;
        let inverse2 = inverse * inverse;
        let z = 1. + inverse * (1. + inverse * (1. + inverse * (1. + inverse)));
        let reciprocal = inverse2 * inverse2 / z;
        let slope =
            inverse2 * inverse2 * inverse * (4. + inverse * (3. + inverse * (2. + inverse)))
                / (z * z);
        (reciprocal, slope)
    }
}
fn confined_body(body: Jet, maximum: Jet) -> (Jet, Jet) {
    let boundary = if maximum.value < 0.8 {
        maximum * 0.5
    } else {
        Jet::constant(0.4)
    };
    if body.value <= boundary.value {
        return (body, Jet::constant(1.));
    }
    let span = maximum - boundary;
    let ratio = (body - boundary) / span;
    let (reciprocal, slope) = polynomial4(ratio);
    // Avoid cancellation near the onset of clamping.
    let value = if ratio.value < 0.5 {
        boundary + (body - boundary) * (1. + ratio * (1. + ratio * (1. + ratio))) * reciprocal
    } else {
        maximum - span * reciprocal
    };
    (value, slope)
}
fn symmetry_offset(drain: Jet, slope: Jet, voltage: f64) -> Jet {
    const FLOOR: f64 = 1e-12;
    if voltage <= FLOOR {
        return Jet::constant(FLOOR);
    }
    let ratio = slope * drain / voltage;
    // The highest-order term alone bounds the positive polynomial; this
    // shortcut avoids overflow at ratios whose offset is already clamped.
    if ratio.value > 1. && ratio.value.ln() * 6. > (voltage / FLOOR).ln() + 5040_f64.ln() {
        return Jet::constant(FLOOR);
    }
    let denominator = 1.
        + ratio
            * (0.5
                + ratio
                    * (1. / 6.
                        + ratio
                            * (1. / 24.
                                + ratio * (1. / 120. + ratio * (1. / 720. + ratio / 5040.)))));
    let offset = voltage / denominator;
    if offset.value < FLOOR {
        Jet::constant(FLOOR)
    } else {
        offset
    }
}
/// Condition normalized intrinsic voltages using the native body clamp and
/// symmetry polynomial, including the clamp's mixed temperature derivatives.
/// # Errors
/// Invalid bias, nonpositive body-confinement span, or nonfinite derivatives.
pub fn evaluate_hisim_hv_bias_conditioning(
    inputs: &HiSimHvBiasConditioningInputs,
) -> Result<HiSimHvBiasConditioningPoint, ModelError> {
    let HiSimHvBiasConditioningInputs {
        bias,
        twice_bulk_potential,
        reference_channel_potential,
        reference_bulk_potential,
        symmetry_voltage,
    } = *inputs;
    if ![
        bias.vgs,
        bias.vds,
        bias.vbs,
        reference_channel_potential,
        reference_bulk_potential,
        symmetry_voltage,
    ]
    .into_iter()
    .all(f64::is_finite)
        || bias.vds < 0.
        || symmetry_voltage < 0.
    {
        return Err(ModelError::InvalidPhysicalParameter(
            "HiSIM-HV bias conditioning input",
        ));
    }
    twice_bulk_potential.checked("HiSIM-HV bulk potential")?;
    let argument = 0.8 - (twice_bulk_potential - symmetry_voltage) - 0.1;
    let mut maximum = 0.8 - 0.5 * (argument + (argument * argument + 0.32).sqrt());
    for reference in [reference_channel_potential, reference_bulk_potential] {
        let limit = reference - symmetry_voltage;
        if limit < maximum.value {
            maximum = Jet::constant(limit);
        }
    }
    if maximum.value <= 0. {
        return Err(ModelError::InvalidPhysicalParameter(
            "HiSIM-HV body confinement span",
        ));
    }
    let (body, slope) = confined_body(Jet::coordinate(bias.vbs, 2, 1.), maximum);
    let drain = Jet::coordinate(bias.vds, 1, 1.);
    let gate = Jet::coordinate(bias.vgs, 0, 1.);
    let offset = symmetry_offset(drain, slope, symmetry_voltage);
    Ok(HiSimHvBiasConditioningPoint {
        confined_body: body.checked("HiSIM-HV confined body")?,
        confinement_slope: slope.checked("HiSIM-HV confinement slope")?,
        maximum_body: maximum.checked("HiSIM-HV body maximum")?,
        symmetry_offset: offset.checked("HiSIM-HV symmetry offset")?,
        modified_body: (body + offset).checked("HiSIM-HV modified body")?,
        modified_drain: (drain + 2. * offset).checked("HiSIM-HV modified drain")?,
        modified_gate: (gate + offset).checked("HiSIM-HV modified gate")?,
    })
}
