//! BSIM2 intrinsic channel, physically separate from BSIM1.
//!
//! Based on ngspice 47 `b2eval.c`, Copyright 1990 Regents of the University
//! of California, Min-Chie Jeng, Hong J. Park and Thomas L. Quarles.
//! Differentiate the constitutive equations, including voltage-domain bounds,
//! rather than inheriting the original approximate and clipped Jacobians.

use super::differential::Jet;
use super::{
    Bsim2Coefficients, LegacyBsimBias, LegacyBsimChannel, LegacyBsimIntrinsicPoint,
    PreparedLegacyBsimInstance,
};
use crate::ModelError;

#[derive(Clone, Copy)]
struct Bias {
    drain: Jet,
    gate: Jet,
    bulk: Jet,
}

impl Bias {
    const fn new(input: LegacyBsimBias, c: &Bsim2Coefficients) -> Self {
        let [vdd, vgg, vbb] = c.voltage_anchors;
        Self {
            drain: Jet::coordinate(input.vds, 0).min(2. * vdd),
            gate: Jet::coordinate(input.vgs, 1).min(2. * vgg),
            bulk: Jet::coordinate(input.vbs, 2).max(2. * vbb),
        }
    }
}

#[derive(Clone, Copy)]
struct Surface {
    potential: Jet,
    root: Jet,
    factor: Jet,
    threshold: Jet,
    overdrive: Jet,
    ua: Jet,
    ub: Jet,
    u1: Jet,
}

fn body(c: &Bsim2Coefficients, b: Bias) -> Surface {
    let (potential, root) = if b.bulk.value <= 0. {
        let potential = Jet::constant(c.phi) - b.bulk;
        (potential, potential.sqrt())
    } else {
        (
            Jet::constant(c.phi) / (b.bulk / c.phi + 1.),
            Jet::constant(c.sqrt_phi) / (b.bulk / (2. * c.phi) + 1.),
        )
    };
    let eta = b.bulk * c.eta[1] + c.eta[0];
    let threshold = root * c.k1 - potential * c.k2 - eta * b.drain + c.vfb + c.phi;
    let factor = (Jet::constant(1.) - Jet::constant(1.) / (potential * 0.8364 + 1.744)) / root
        * (0.5 * c.k1)
        + 1.;
    Surface {
        potential,
        root,
        factor,
        threshold,
        overdrive: b.gate - threshold,
        ua: b.bulk * c.ua[1] + c.ua[0],
        ub: b.bulk * c.ub[1] + c.ub[0],
        u1: b.bulk * c.u1[1] + c.u1[0],
    }
}

/// Weak-inversion endpoint and slope for the reference Hermite transition.
#[derive(Clone, Copy)]
struct EffectiveGate {
    value: Jet,
    weak_current_factor: Option<Jet>,
}

fn effective_gate(c: &Bsim2Coefficients, b: Bias, s: Surface) -> EffectiveGate {
    if s.overdrive.value >= c.gate_high || c.subthreshold[0] == 0. {
        return EffectiveGate {
            value: s.overdrive,
            weak_current_factor: None,
        };
    }
    let vt = c.thermal_voltage;
    let offset = b.bulk * c.offset[1] + b.drain * c.offset[2] + c.offset[0];
    let slope =
        Jet::constant(c.subthreshold[1]) / s.root + b.drain * c.subthreshold[2] + c.subthreshold[0];
    let inv_slope = Jet::constant(0.5) / (slope * vt);
    let drain_factor = -(-b.drain / vt).max(-30.).exp_m1();
    if s.overdrive.value <= c.gate_low {
        let exponent = (s.overdrive * inv_slope).max(-30.) + offset * 0.5;
        let exponential = exponent.exp();
        let gate = (s.factor * drain_factor.max(1e-18) * 2.).sqrt() * vt * exponential;
        let weak_current_factor = (exponent * 2.).exp() * drain_factor * (vt * vt);
        return EffectiveGate {
            value: gate,
            weak_current_factor: Some(weak_current_factor),
        };
    }
    let exponent = (inv_slope * c.gate_low).max(-30.) + offset * 0.5;
    // The original interpolating polynomial is the cubic Hermite segment
    // between the lower exponential endpoint and the upper Vgst endpoint.
    // Evaluate in a dimensionless coordinate, not a voltage-power determinant.
    let endpoint = if drain_factor.value == 0. {
        // At Vds=0 the interpolation endpoint has a square-root singular
        // slope; the eventual current is zero and the one-sided derivative
        // of the final law is handled by the boundary caller.
        Jet::constant(0.)
    } else {
        (s.factor * drain_factor * 2.).sqrt() * vt * exponent.exp()
    };
    let span = c.gate_high - c.gate_low;
    let t = (s.overdrive - c.gate_low) / span;
    let one_minus = Jet::constant(1.) - t;
    let lower = endpoint * one_minus * one_minus * (t * 2. + 1.);
    let lower_slope = endpoint * inv_slope * span * t * one_minus * one_minus;
    let upper = t * t * (Jet::constant(3.) - t * 2.) * c.gate_high;
    let upper_slope = -t * t * one_minus * span;
    EffectiveGate {
        value: lower + lower_slope + upper + upper_slope,
        weak_current_factor: None,
    }
}

#[derive(Clone, Copy)]
struct Transport {
    vertical: Jet,
    k: Jet,
    saturation: Jet,
}

fn transport(s: Surface, gate: Jet, minimum: f64) -> Transport {
    let vertical = (gate * (s.ua + gate * s.ub) + 1.).max(0.2);
    let velocity = s.u1 * gate / (s.factor * vertical);
    let k = (velocity + (velocity * 2. + 1.).sqrt() + 1.) * 0.5;
    let saturation = (gate / (s.factor * k.sqrt())).max(minimum);
    Transport {
        vertical,
        k,
        saturation,
    }
}

fn beta(c: &Bsim2Coefficients, b: Bias, saturation: Jet) -> Jet {
    let [vdd, _, _] = c.voltage_anchors;
    let at_zero = b.bulk * c.beta_zero[1] + c.beta_zero[0];
    let at_saturation = b.bulk * c.beta_saturation[1] + c.beta_saturation[0];
    let fit = |values: [f64; 3]| b.bulk * values[1] + b.gate * values[2] + values[0];
    let beta2 = fit(c.beta2);
    let beta3 = fit(c.beta3);
    let beta4 = fit(c.beta4);
    let beta1 = at_saturation - at_zero - (beta3 - beta4 * vdd) * vdd;
    let argument = (b.drain * beta2 / saturation).min(30.);
    at_zero + beta1 * argument.tanh() + b.drain * (beta3 - beta4 * b.drain)
}

fn impact(c: &Bsim2Coefficients, b: Bias, saturation: Jet) -> Jet {
    if c.impact_a[0] == 0. || b.drain.value <= saturation.value {
        return Jet::constant(1.);
    }
    let amplitude = b.bulk * c.impact_a[1] + c.impact_a[0];
    let barrier = b.bulk * c.impact_b[1] + c.impact_b[0];
    let exponent = -(barrier / (b.drain - saturation)).min(30.);
    amplitude * exponent.exp() + 1.
}

fn channel(c: &Bsim2Coefficients, b: Bias, s: Surface, gate: EffectiveGate) -> (Jet, Jet) {
    let v = gate.value;
    if v.value <= 0. {
        return (Jet::constant(0.), Jet::constant(0.));
    }
    let transport = transport(s, v, 1e-18);
    let sat = transport.saturation;
    let beta = beta(c, b, sat);
    let current = gate.weak_current_factor.map_or_else(
        || {
            if b.drain.value <= sat.value {
                let fraction = b.drain / sat - 1.;
                let lateral = s.u1 * (Jet::constant(1.) - fraction * fraction * c.u1[2]);
                let mobility = (transport.vertical + lateral * b.drain).max(0.5);
                beta * (v - s.factor * b.drain * 0.5) * b.drain / mobility
            } else {
                beta * v * v / (transport.vertical * s.factor * transport.k * 2.)
                    * impact(c, b, sat)
            }
        },
        |weak| beta * weak * impact(c, b, sat),
    );
    (current, sat)
}

fn charge(
    c: &Bsim2Coefficients,
    b: Bias,
    s: Surface,
    oxide: f64,
    saturation: Jet,
) -> ([Jet; 4], Jet) {
    let effective_bulk = if b.bulk.value < 0. {
        b.bulk
    } else {
        Jet::constant(c.phi) - s.potential
    };
    let gate_bulk = b.gate - effective_bulk - c.vfb;
    let bulk_factor = gate_bulk - s.overdrive;
    let qbulk = bulk_factor / 3.;
    let mut sat = saturation;
    let [drain, gate, bulk] = if gate_bulk.value <= 0. {
        [Jet::constant(0.), gate_bulk, -gate_bulk]
    } else if s.overdrive.value <= 0. {
        let ratio = gate_bulk / bulk_factor;
        let gate = gate_bulk * (Jet::constant(1.) - ratio * (Jet::constant(1.) - ratio / 3.));
        [Jet::constant(0.), gate, -gate]
    } else {
        if s.overdrive.value < c.gate_high {
            sat = transport(s, s.overdrive, 0.).saturation;
        }
        if b.drain.value >= sat.value {
            [
                s.overdrive * (-4. / 15.),
                s.overdrive * (2. / 3.) + qbulk,
                -qbulk,
            ]
        } else {
            // Cancel common overdrive in the linear-region charge ratios.
            let fraction = Jet::constant(1.) - b.drain / sat;
            let drain_overdrive = s.overdrive * fraction;
            let t0 = Jet::constant(1.) / (fraction + 1.);
            let t1 = fraction * t0;
            let t5 = t0 * t1;
            let gate = (s.overdrive + drain_overdrive - drain_overdrive * t0) * (2. / 3.) + qbulk;
            let drain = -(drain_overdrive * 0.2
                + s.overdrive * 0.8
                + drain_overdrive * t1
                + t5 * (drain_overdrive - s.overdrive) * 0.2)
                / 3.;
            [drain, gate, -qbulk]
        }
    }
    .map(|q| q * oxide);
    ([drain, gate, -(drain + gate + bulk), bulk], sat)
}

pub(super) fn threshold(c: &Bsim2Coefficients, bias: LegacyBsimBias) -> f64 {
    body(c, Bias::new(bias, c)).threshold.value
}

/// Evaluate BSIM2's native current and conservative charge (physical xpart
/// modes 0/1) in polarity-normalized forward orientation.
///
/// # Errors
/// Wrong family, nonfinite/reversed bias, or a nonphysical evaluation.
pub fn evaluate_bsim2_intrinsic(
    instance: &PreparedLegacyBsimInstance,
    input: LegacyBsimBias,
) -> Result<LegacyBsimIntrinsicPoint, ModelError> {
    let LegacyBsimChannel::Bsim2(c) = &instance.channel else {
        return Err(ModelError::WrongModelScope);
    };
    super::validate_bias(input)?;
    let b = Bias::new(input, c);
    let s = body(c, b);
    let gate = effective_gate(c, b, s);
    let (current, saturation) = channel(c, b, s, gate);
    let current = current.max(0.);
    let (charge, saturation) = charge(c, b, s, instance.oxide_capacitance, saturation);
    if !current.finite()
        || !charge.into_iter().all(Jet::finite)
        || !s.threshold.finite()
        || !saturation.finite()
    {
        return Err(ModelError::InvalidPhysicalParameter(
            "nonfinite BSIM2 channel evaluation",
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
