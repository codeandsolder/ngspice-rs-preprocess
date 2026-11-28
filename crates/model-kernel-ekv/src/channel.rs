use std::cmp::Ordering;

use model_kernel_api::taylor::arithmetic::Scalar;

use crate::{Intrinsic, Prepared};

fn square<S: Scalar>(x: S) -> S {
    x * x
}

fn positive<S: Scalar>(x: S, smoothing_squared: f64) -> S {
    let root = square(x).offset(smoothing_squared).sqrt();
    if x.compare(0.) == Some(Ordering::Less) {
        (root - x).recip().scale(0.5 * smoothing_squared)
    } else {
        (x + root).scale(0.5)
    }
}

fn normalized_current<S: Scalar>(x: S) -> S {
    let y = if x.compare(-0.35) == Some(Ordering::Greater) {
        let z = (x.offset(1.3) - x.offset(1.6).ln()).recip().scale(2.);
        interpolate(x, z)
    } else if x.compare(-15.) == Some(Ordering::Greater) {
        interpolate(x, (-x).exp().offset(1.55))
    } else if x.compare(-23.) == Some(Ordering::Greater) {
        (-x).exp().offset(2.).recip()
    } else {
        x.exp().offset(1e-64)
    };
    y * y.offset(1.)
}

fn interpolate<S: Scalar>(x: S, z: S) -> S {
    let next = z.offset(2.) / (x.offset(1.) + z.ln());
    (x.offset(1.) + next.ln()) / next.offset(2.)
}

struct Electrostatics<S> {
    gate_star: S,
    gate_prime: S,
    pinch: S,
    gamma: S,
}

fn pinch<S: Scalar>(gate: S, gamma: S, phi: f64) -> S {
    gate.offset(-phi) - gamma * ((gate + square(gamma).scale(0.25)).sqrt() - gamma.scale(0.5))
}

fn electrostatics<S: Scalar>(p: &Prepared, vd: S, vg: S, vs: S) -> Electrostatics<S> {
    let gate_star = vg.offset(
        p.gamma
            .mul_add(p.phi.sqrt(), -p.threshold - p.flatband_shift + p.phi),
    );
    let gate_prime = positive(gate_star, 32. * p.vt * p.vt);
    let depletion = |v: S| positive(v.offset(p.phi), 16. * p.vt * p.vt).sqrt();
    let long_pinch = pinch(gate_prime, S::constant(p.gamma), p.phi);
    let gamma_star = (depletion(vs) + depletion(vd)).scale(-p.short)
        + long_pinch
            .offset(0.1f64.mul_add(p.vt, p.phi))
            .sqrt()
            .scale(p.narrow)
        + S::constant(p.gamma);
    let gamma = positive(gamma_star, 0.1 * p.vt);
    Electrostatics {
        gate_star,
        gate_prime,
        pinch: pinch(gate_prime, gamma, p.phi),
        gamma,
    }
}

struct Transport<S> {
    forward: S,
    reverse: S,
    reverse_saturated: S,
    saturation: S,
    equivalent_length: S,
}

fn transport<S: Scalar>(p: &Prepared, vp: S, vd: S, vs: S) -> Transport<S> {
    let forward = normalized_current((vp - vs).scale(p.vt.recip()));
    let root = forward.sqrt();
    let vc = p.critical_field * p.length;
    let saturation = root
        .scale(p.vt / vc)
        .offset(0.25)
        .sqrt()
        .offset(-0.5)
        .scale(vc);
    let half_vds = (vd - vs).scale(0.5);
    let smooth = (root - saturation.scale(p.vt.recip()))
        .scale(p.lambda)
        .offset(0.015_625)
        .scale(16. * p.vt * p.vt);
    let effective =
        (square(saturation) + smooth).sqrt() - (square(half_vds - saturation) + smooth).sqrt();
    let reverse_saturation = (root - forward.ln().scale(0.75))
        .scale(p.vt / vc)
        .offset(0.25)
        .sqrt()
        .offset(-0.5)
        .scale(vc)
        .offset(p.vt * ((0.5 * vc / p.vt).ln() - 0.6));
    let reverse_saturated = normalized_current(
        (vp - half_vds - vs - (square(reverse_saturation) + smooth).sqrt()
            + (square(half_vds - reverse_saturation) + smooth).sqrt())
        .scale(p.vt.recip()),
    );
    let shortening = (half_vds - effective)
        .scale((p.characteristic_length * p.critical_field).recip())
        .ln_1p()
        .scale(p.characteristic_length * p.lambda);
    let channel_length =
        (half_vds + effective).scale(p.critical_field.recip()) - shortening + S::constant(p.length);
    Transport {
        forward,
        reverse: normalized_current((vp - vd).scale(p.vt.recip())),
        reverse_saturated,
        saturation,
        equivalent_length: positive(channel_length, (0.1 * p.length).powi(2)),
    }
}

fn mobility<S: Scalar>(p: &Prepared, vp: S, t: &Transport<S>) -> (S, S) {
    let sf = t.forward.offset(0.25).sqrt();
    let sr = t.reverse.offset(0.25).sqrt();
    let body_root = vp.offset(p.phi + 1e-6).sqrt();
    let slope_increment = body_root.recip().scale(0.5 * p.gamma);
    let qi = ((square(sr) + sr * sf + square(sf)) / (sr + sf))
        .scale(0.666_666_66 + 0.666_666_66)
        .offset(-1.)
        * slope_increment.offset(1.).scale(-p.vt);
    let qb = body_root.scale(-p.gamma) - slope_increment / slope_increment.offset(1.) * qi;
    let beta = p.mobility_factor.map_or_else(
        || {
            (t.equivalent_length * positive(vp, 2. * p.vt * p.vt).scale(p.theta).offset(1.))
                .recip()
                .scale(p.kp_width)
        },
        |field| {
            let eta = if p.sign > 0. {
                0.5
            } else {
                0.333_333_333_333_3
            };
            (t.equivalent_length * (qb + qi.scale(eta)).abs().scale(field).offset(1.))
                .recip()
                .scale(p.kp_width * (field * p.gamma).mul_add(p.phi.sqrt(), 1.))
        },
    );
    (beta, beta * qi.abs())
}

fn partition<S: Scalar>(a: S, b: S) -> S {
    (square(a) * a.scale(3.)
        + square(a) * b.scale(6.)
        + a * square(b).scale(4.)
        + square(b) * b.scale(2.))
        / square(a + b)
}

fn charges<S: Scalar>(p: &Prepared, e: &Electrostatics<S>, t: &Transport<S>) -> [S; 4] {
    let sf = t.forward.offset(0.25).sqrt();
    let sr = t.reverse.offset(0.25).sqrt();
    let body = e.pinch.scale(0.5).offset(p.phi).sqrt().scale(2.);
    let scale = (e.gamma / body).offset(1.).scale(-p.vt * p.area_cox);
    let qd = scale * partition(sr, sf).scale(0.266_666_666).offset(-0.5);
    let qs = scale * partition(sf, sr).scale(0.266_666_666).offset(-0.5);
    let qi = qd + qs;
    let qb = (e.gate_prime - e.gate_star - e.gamma * e.pinch.offset(p.phi + 1e-6).sqrt())
        .scale(p.area_cox)
        - qi * e.gamma / (e.gamma + body);
    [qd, -qi - qb, qs, qb]
}

fn impact<S: Scalar>(p: &Prepared, vds: S, saturation: S, id: S) -> S {
    let excess = vds - saturation.scale(2. * p.impact_saturation);
    if p.impact_ratio == 0. || excess.compare(0.) != Some(Ordering::Greater) {
        return S::constant(0.);
    }
    let exponent = excess.recip().scale(-p.impact_field_length);
    let factor = if exponent.compare(-35.) == Some(Ordering::Less) {
        S::constant((-35.0_f64).exp())
    } else {
        exponent.exp()
    };
    excess * factor * id.scale(p.impact_ratio)
}

pub fn evaluate<S: Scalar>(p: &Prepared, v: [S; 4]) -> Intrinsic<S> {
    let [mut vd, vg, mut vs] = [v[0], v[1], v[2]].map(|x| (x - v[3]).scale(p.sign));
    let reversed = vd.compare_to(vs) == Some(Ordering::Less);
    if reversed {
        std::mem::swap(&mut vd, &mut vs);
    }
    let e = electrostatics(p, vd, vg, vs);
    let t = transport(p, e.pinch, vd, vs);
    let (beta, thermal_conductance) = mobility(p, e.pinch, &t);
    let slope = e
        .pinch
        .offset(4.0f64.mul_add(p.vt, p.phi))
        .sqrt()
        .recip()
        .scale(0.5 * p.gamma)
        .offset(1.);
    let id = (t.forward - t.reverse_saturated) * slope * beta.scale(2. * p.vt * p.vt);
    let substrate = impact(p, vd - vs, t.saturation, id);
    let mut current = [id + substrate, S::constant(0.), -id, -substrate];
    let mut charge = charges(p, &e, &t);
    if reversed {
        current.swap(0, 2);
        charge.swap(0, 2);
    }
    Intrinsic {
        current: current.map(|x| x.scale(p.sign)),
        charge: charge.map(|x| x.scale(p.sign)),
        channel_current: id.scale(if reversed { -p.sign } else { p.sign }),
        thermal_conductance,
    }
}
