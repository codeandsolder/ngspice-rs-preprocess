//! BSIM1 intrinsic channel current and conservative channel charge.
//!
//! Based on ngspice 47 `b1eval.c`, Copyright 1990 Regents of the University
//! of California, Hong J. Park and Thomas L. Quarles. See package LICENSE.
//! The derivative is that of the actual constitutive function, not a separate
//! Newton-stamp approximation. Junctions and overlaps are separate branches.

use super::differential::Jet;
use super::{Bsim1Coefficients, LegacyBsimChannel, PreparedLegacyBsimInstance};
use crate::ModelError;

/// Fixed reference thermal voltage used by the original BSIM1 equations,
/// independent of the circuit or model-card temperature.
pub const BSIM1_REFERENCE_THERMAL_VOLTAGE: f64 = 1.380_648_52e-23 * 300.15 / 1.602_176_620_8e-19;

/// Polarity-normalized bias with drain >= source. The caller chooses the
/// intrinsic orientation before evaluating and maps the terminal rows back.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LegacyBsimBias {
    /// Drain minus source voltage.
    pub vds: f64,
    /// Gate minus source voltage.
    pub vgs: f64,
    /// Bulk minus source voltage.
    pub vbs: f64,
}

/// Per-device intrinsic current/charge at a normalized bias. The derivative
/// columns are Vds, Vgs, Vbs; the charge rows are drain, gate, source, bulk.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LegacyBsimIntrinsicPoint {
    /// Channel drain current in amperes, before the parallel multiplier.
    pub drain_current: f64,
    /// Exact partial derivatives of channel current, in siemens.
    pub conductance: [f64; 3],
    /// Intrinsic terminal charges, in coulombs, before overlap and junctions.
    pub charge: [f64; 4],
    /// Exact partial derivatives of the charge rows, in farads.
    pub capacitance: [[f64; 3]; 4],
    /// Bias-dependent normalized threshold.
    pub threshold: f64,
    /// Reported saturation voltage (BSIM2 may recompute it for charge).
    pub saturation_voltage: f64,
}

#[derive(Clone, Copy)]
struct Coordinates {
    drain: Jet,
    gate: Jet,
    bulk: Jet,
}

impl Coordinates {
    const fn new(bias: LegacyBsimBias) -> Self {
        Self {
            drain: Jet::coordinate(bias.vds, 0),
            gate: Jet::coordinate(bias.vgs, 1),
            bulk: Jet::coordinate(bias.vbs, 2),
        }
    }
}

#[derive(Clone, Copy)]
struct Surface {
    root: Jet,
    factor: Jet,
    threshold: Jet,
    overdrive: Jet,
}

fn surface(c: &Bsim1Coefficients, x: Coordinates) -> Surface {
    let potential = Jet::constant(c.phi) + (-x.bulk).max(0.);
    let root = potential.sqrt();
    let eta = (x.bulk * c.eta[1] + (x.drain - c.vdd) * c.eta[2] + c.eta[0])
        .max(0.)
        .min(1.);
    let threshold = root * c.k1 - potential * c.k2 - eta * x.drain + c.vfb + c.phi;
    let correction = Jet::constant(1.) - Jet::constant(1.) / (potential * 0.8364 + 1.744);
    let factor = (correction * (0.5 * c.k1) / root + 1.).max(1.);
    Surface {
        root,
        factor,
        threshold,
        overdrive: x.gate - threshold,
    }
}

fn beta(c: &Bsim1Coefficients, x: Coordinates) -> Jet {
    let at_zero = x.bulk * c.beta_zero[1] + c.beta_zero[0];
    let at_vdd = x.bulk * c.beta_vdd[1] + c.beta_vdd[0];
    if x.drain.value > c.vdd {
        at_vdd + (x.drain - c.vdd) * c.beta_vdd[2]
    } else {
        // Algebraically the reference quadratic, in a dimensionless
        // coordinate to avoid squaring a possibly small interpolation anchor.
        let fraction = x.drain / c.vdd;
        at_zero
            + (at_vdd - at_zero) * fraction * (Jet::constant(2.) - fraction)
            + fraction * (fraction - 1.) * (c.beta_vdd[2] * c.vdd)
    }
}

fn strong_current(c: &Bsim1Coefficients, x: Coordinates, s: Surface, length_um: f64) -> (Jet, Jet) {
    if s.overdrive.value < 0. {
        return (Jet::constant(0.), Jet::constant(0.));
    }
    let ugs = (x.bulk * c.ugs[1] + c.ugs[0]).max(0.);
    let uds = (x.bulk * c.uds[1] + (x.drain - c.vdd) * c.uds[2] + c.uds[0]).max(0.) / length_um;
    let beta = beta(c, x) / (ugs * s.overdrive + 1.).max(1.);
    let velocity = (uds * s.overdrive / s.factor).max(0.);
    let k = (velocity + (velocity * 2. + 1.).sqrt() + 1.) * 0.5;
    let saturation = (s.overdrive / (s.factor * k.sqrt())).max(0.);
    let current = if x.drain.value < saturation.value {
        beta * (s.overdrive - s.factor * x.drain * 0.5) * x.drain / (uds * x.drain + 1.).max(1.)
    } else {
        beta * s.overdrive * s.overdrive / (s.factor * k * 2.)
    };
    (current, saturation)
}

fn weak_current(c: &Bsim1Coefficients, x: Coordinates, overdrive: Jet) -> Jet {
    // The original law disables weak inversion when N0 >= 200.
    // The exact zero-mobility limit is zero, not the C 0/0 form.
    if c.subthreshold[0] >= 200. || c.beta_zero[0] == 0. {
        return Jet::constant(0.);
    }
    let vt = BSIM1_REFERENCE_THERMAL_VOLTAGE;
    let slope =
        (x.bulk * c.subthreshold[1] + x.drain * c.subthreshold[2] + c.subthreshold[0]).max(0.5);
    let exponent = overdrive / (slope * vt);
    let drain_factor = -(-x.drain / vt).exp_m1();
    let scaled = drain_factor * (6.04965 / 4.5);
    // Cancel the common beta*Vt² factor in the harmonic limiting expression.
    // Use a reciprocal exponential when positive, avoiding Inf/Inf in strong inversion.
    let fraction = if exponent.value >= 0. {
        scaled / (scaled + (-exponent).exp())
    } else {
        let ratio = scaled * exponent.exp();
        ratio / (ratio + 1.)
    };
    fraction * (4.5 * vt * vt * c.beta_zero[0])
}

fn depletion(c: &Bsim1Coefficients, gate_bulk: Jet) -> Jet {
    if c.k1 == 0. {
        // Continuous K1 -> 0 limit; the unreduced C expression divides by K1².
        Jet::constant(0.)
    } else {
        let root = (gate_bulk * 4. + c.k1 * c.k1).sqrt();
        gate_bulk * (2. * c.k1) / (root + c.k1)
    }
}

/// Strong-inversion charge in normalized units of `CoxWL`. The common charge
/// calculation is independent of the two drain-partition choices.
fn inversion(c: &Bsim1Coefficients, x: Coordinates, s: Surface, threshold: Jet) -> [Jet; 3] {
    let overdrive = x.gate - threshold;
    let pinchoff = (overdrive / s.factor).max(0.);
    if x.drain.value >= pinchoff.value {
        let third = Jet::constant(1.) / (s.factor * 3.);
        let gate = x.gate - c.vfb - c.phi - overdrive * third;
        let bulk = Jet::constant(c.vfb + c.phi) - threshold
            + (Jet::constant(1.) - s.factor) * overdrive * third;
        let drain = if c.partition_zero_drain {
            Jet::constant(0.)
        } else {
            overdrive * (-4. / 15.)
        };
        [drain, gate, bulk]
    } else {
        let avds = s.factor * x.drain;
        let ent = overdrive - avds * 0.5;
        let (ratio, vcom) = if ent.value > 1e-8 {
            let ratio = avds / ent;
            let normalized = overdrive / ent;
            let vcom =
                normalized * normalized / 6. - ratio * normalized * 0.125 + ratio * ratio * 0.025;
            (ratio, vcom)
        } else {
            (Jet::constant(2.), Jet::constant(4. / 15.))
        };
        let delta = x.drain - avds;
        let gate = x.gate - c.vfb - c.phi - x.drain * 0.5 + x.drain * ratio / 12.;
        let bulk = Jet::constant(c.vfb + c.phi) - threshold + delta * 0.5 - delta * ratio / 12.;
        let drain = if c.partition_zero_drain {
            -(overdrive * 0.5 - avds * 0.75 + avds * ratio * 0.125)
        } else {
            -((overdrive - avds) * 0.5 + avds * vcom)
        };
        [drain, gate, bulk]
    }
}

fn charge(c: &Bsim1Coefficients, x: Coordinates, s: Surface, oxide: f64) -> [Jet; 4] {
    // The reference C-V threshold omits the DC DIBL and second body term.
    let threshold = s.root * c.k1 + c.vfb + c.phi;
    let gate_bulk = x.gate - x.bulk - c.vfb;
    let [drain, gate, bulk] = if gate_bulk.value < 0. {
        [Jet::constant(0.), gate_bulk, -gate_bulk]
    } else if x.gate.value < threshold.value {
        let q = depletion(c, gate_bulk);
        [Jet::constant(0.), q, -q]
    } else {
        inversion(c, x, s, threshold)
    }
    .map(|q| q * oxide);
    let source = -(drain + gate + bulk);
    [drain, gate, source, bulk]
}

pub(super) fn threshold(c: &Bsim1Coefficients, bias: LegacyBsimBias) -> f64 {
    surface(c, Coordinates::new(bias)).threshold.value
}

/// Evaluate the intrinsic BSIM1 channel and its exact local Jacobians.
/// No numerical history, matrix rows, allocations or finite differencing.
///
/// # Errors
/// Wrong family, nonfinite/incorrectly oriented bias or invalid physical result.
pub fn evaluate_bsim1_intrinsic(
    instance: &PreparedLegacyBsimInstance,
    bias: LegacyBsimBias,
) -> Result<LegacyBsimIntrinsicPoint, ModelError> {
    let LegacyBsimChannel::Bsim1(c) = &instance.channel else {
        return Err(ModelError::WrongModelScope);
    };
    super::validate_bias(bias)?;
    let x = Coordinates::new(bias);
    let s = surface(c, x);
    let length_um = instance.geometry.effective_length * 1e6;
    let (strong, saturation) = strong_current(c, x, s, length_um);
    let current = (strong + weak_current(c, x, s.overdrive)).max(0.);
    let charge = charge(c, x, s, instance.oxide_capacitance);
    if !current.finite()
        || !charge.into_iter().all(Jet::finite)
        || !s.threshold.finite()
        || !saturation.finite()
    {
        return Err(ModelError::InvalidPhysicalParameter(
            "nonfinite BSIM1 channel evaluation",
        ));
    }
    Ok(LegacyBsimIntrinsicPoint {
        drain_current: current.value,
        conductance: current.gradient,
        charge: charge.map(|q| q.value),
        capacitance: charge.map(|q| q.gradient),
        threshold: s.threshold.value,
        saturation_voltage: saturation.value,
    })
}
