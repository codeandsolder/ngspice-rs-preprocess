//! HV2 depletion-mode effective channel and residual/back-layer drain biases.
use super::{HiSimHvIntrinsicScalar as Jet, depletion_surface::cap_distance};
use crate::ModelError;
const Q: f64 = 1.602_191_8e-19;
const ESI: f64 = 1.034_943e-10;
const EPS: f64 = 10. * f64::EPSILON;
const ERR: ModelError = ModelError::InvalidPhysicalParameter("HiSIM-HV2 depletion effective drain");

/// Conditioned, polarity-normalized voltage inputs before depletion
/// drain saturation. The gate does not include the native tiny offset.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvDepletionDrainInputs {
    pub gate: Jet,
    pub symmetric_body: Jet,
    pub drain: Jet,
    pub thermal_voltage: Jet,
    pub oxide_capacitance: Jet,
    pub depletion_doping: f64,
    pub smoothing_exponent: f64,
    pub drift_gate_offset: f64,
    pub drift_scale: f64,
    pub drift_floor: f64,
}
/// Channel endpoint quasi-Fermi potential and separate neutral-layer
/// transport voltage, with the original intrinsic G/D/B/T derivatives.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvDepletionDrainPoint {
    pub channel: Jet,
    pub drift: Jet,
}
fn upper(x: Jet, limit: Jet, delta: Jet, power: f64) -> Jet {
    let start = limit - delta;
    if x.value <= start.value {
        x
    } else {
        limit - delta * cap_distance((x - start) / delta, power)
    }
}
fn lower(x: Jet, limit: f64, delta: f64, power: f64) -> Jet {
    -upper(-x, Jet::constant(-limit), Jet::constant(delta), power)
}
fn saturation(drain: Jet, limit: Jet, exponent: f64) -> Jet {
    let log_ratio = exponent * (drain.ln() - limit.ln());
    let softplus = if log_ratio.value > 0. {
        log_ratio + (-log_ratio).exp().ln_1p()
    } else {
        log_ratio.exp().ln_1p()
    };
    drain * (-softplus / exponent).exp()
}
fn saturation_voltage(gate: Jet, body: Jet, thermal_voltage: Jet, factor: Jet) -> Jet {
    let argument = lower(
        1. + 2. * (gate - thermal_voltage - body) / factor,
        0.,
        2.,
        4.,
    ) + 1e-25;
    let root = argument.sqrt();
    gate + factor * (1. - argument) / (1. + root)
}
/// Evaluate the distinct depletion surface and neutral-layer voltage limits.
/// # Errors
/// Invalid physical inputs or nonfinite local voltage limits.
pub fn evaluate_hisim_hv_depletion_drain(
    i: HiSimHvDepletionDrainInputs,
) -> Result<HiSimHvDepletionDrainPoint, ModelError> {
    if [
        i.depletion_doping,
        i.smoothing_exponent,
        i.oxide_capacitance.value,
        i.thermal_voltage.value,
    ]
    .into_iter()
    .any(|v| !v.is_finite() || v <= 0.)
        || i.drain.value < 0.
    {
        return Err(ERR);
    }
    if i.drain.value <= 1e-3 {
        let drain = i.drain.checked("HiSIM-HV2 depletion drain")?;
        return Ok(HiSimHvDepletionDrainPoint {
            channel: drain,
            drift: drain,
        });
    }
    let gate = i.gate + 1e7 * EPS;
    let factor = Q * ESI * i.depletion_doping / (i.oxide_capacitance * i.oxide_capacitance);
    let channel_limit = lower(
        saturation_voltage(gate + 2., i.symmetric_body, i.thermal_voltage, factor),
        0.3,
        0.2,
        8.,
    ) + EPS;
    let channel = saturation(i.drain, channel_limit, i.smoothing_exponent);
    let gate_limit = lower(gate, 0., 0.5, 4.);
    let channel = upper(channel, gate_limit, 0.8 * gate_limit, 4.);
    let drift_limit = lower(
        i.drift_scale
            * saturation_voltage(
                gate + i.drift_gate_offset,
                i.symmetric_body,
                i.thermal_voltage,
                factor,
            ),
        i.drift_floor,
        4.,
        8.,
    ) + EPS;
    if drift_limit.value <= 0. {
        return Err(ERR);
    }
    let drift = saturation(i.drain, drift_limit, i.smoothing_exponent);
    Ok(HiSimHvDepletionDrainPoint {
        channel: channel.checked("HiSIM-HV2 depletion channel voltage")?,
        drift: drift.checked("HiSIM-HV2 depletion drift voltage")?,
    })
}
