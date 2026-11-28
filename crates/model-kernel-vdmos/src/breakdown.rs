//! ngspice 47 body-diode avalanche transition and its temperature sensitivity.
//! The matching equation is from `vdmostemp.c`, with a convergent root solve
//! rather than its fixed 25-iteration approximation.

use super::{AVALANCHE_MATCH, Breakdown, Jet, ModelError};
use model_kernel_api::taylor::{GuardPredicate, GuardRecorder, arithmetic::Scalar};

/// Computes XBV at the current temperature, including dXBV/dT for an exact
/// thermal Jacobian. The limiter/bypass policy remains the solver's decision.
pub fn effective_voltage<S: Scalar>(
    parameters: Breakdown,
    saturation_current: Jet<S>,
    thermal_voltage: Jet<S>,
    guards: &mut GuardRecorder<'_>,
) -> Result<Jet<S>, ModelError> {
    let bv = parameters.bv.abs();
    let cbv = parameters.ibv;
    // This is ngspice 47's no-shift case when the requested breakdown
    // current is below the reverse-region current at BV.
    let reverse = saturation_current * bv / thermal_voltage;
    if !guards.select(
        AVALANCHE_MATCH,
        GuardPredicate::Less,
        reverse.v,
        S::constant(cbv),
        cbv != 0.,
    ) {
        return Ok(Jet::constant(bv));
    }
    // expm1(y) = delta + NBV*y. Keep delta separate from one:
    // near the matching boundary adding one would erase its constant term.
    let delta = (cbv - reverse) / saturation_current;
    if !delta.v.is_finite() || delta.v.value() < 0. {
        return Err(ModelError::Invalid("avalanche matching domain"));
    }
    let root = solve_log_root(delta.v.value(), parameters.nbv)?;
    let mut y = S::constant(root);
    let mut matched = 1;
    // Formal Newton on the implicit law doubles the matched coefficient
    // order. Keep the independently converged scalar root exactly fixed.
    while matched <= delta.v.degree() {
        let argument = delta.v + y.scale(parameters.nbv);
        let residual = (y - argument.ln_1p()).with_value(0.);
        let derivative = argument.offset(1. - parameters.nbv) / argument.offset(1.);
        y = (y - residual / derivative).with_value(root);
        matched *= 2;
    }
    let denominator = S::constant(parameters.nbv)
        .mul_add(y, delta.v)
        .offset(1. - parameters.nbv);
    if !denominator.is_finite() || denominator.value() <= 0.0 {
        return Err(ModelError::Invalid("avalanche temperature derivative"));
    }
    let dydt = delta.dt / denominator;
    let value = thermal_voltage
        .v
        .scale(parameters.nbv)
        .mul_add(-y, S::constant(bv));
    let dt = thermal_voltage
        .dt
        .mul_add(y, thermal_voltage.v * dydt)
        .scale(-parameters.nbv);
    if !value.is_finite() || !dt.is_finite() {
        return Err(ModelError::Invalid("avalanche voltage"));
    }
    Ok(Jet {
        v: value,
        dx: S::constant(0.),
        dy: S::constant(0.),
        dt,
    })
}

// (expm1(y)-y)/y, without cancellation at a small positive root.
fn exponential_remainder(y: f64) -> f64 {
    let mut term = y * 0.5;
    let mut sum = term;
    for n in 3..=16 {
        term *= y / f64::from(n);
        sum += term;
    }
    sum
}

fn solve_log_root(delta: f64, nbv: f64) -> Result<f64, ModelError> {
    if delta == 0. && nbv <= 1. {
        return Ok(0.);
    }
    let argument = |y: f64| nbv.mul_add(y, delta);
    let f = |y: f64| {
        if y <= 0.125 && y > 0. {
            exponential_remainder(y) + (1. - nbv) - delta / y
        } else {
            y - argument(y).ln_1p()
        }
    };
    // Fixed-point iteration is fast away from the tangent; accept its
    // convergence only where the contraction has a useful error bound.
    let mut y = delta.ln_1p().max(nbv.ln().max(0.));
    for _ in 0..24 {
        let next = argument(y).ln_1p();
        if next.is_finite()
            && nbv / (1. + argument(y)) <= 0.5
            && (next - y).abs() <= 2. * f64::EPSILON * next
        {
            return Ok(next);
        }
        y = next;
    }
    // The positive root lies above ln(NBV) for NBV > 1. The quadratic
    // lower bound on expm1 gives a tight upper bound near a tangent,
    // including arbitrarily small positive delta at NBV = 1.
    let mut low = nbv.ln().max(0.);
    let a = nbv - 1.;
    let mut high = if delta <= 0.125 && a.abs() <= 1. {
        let radius = a.hypot(delta.sqrt() * std::f64::consts::SQRT_2);
        if a >= 0. {
            a + radius
        } else {
            delta / (0.5 * (radius - a))
        }
    } else {
        delta.ln_1p() + nbv.ln_1p() + 1.
    };
    high = high.max(low.next_up()).next_up();
    for _ in 0..64 {
        if f(high) >= 0. {
            break;
        }
        high *= 2.;
        if !high.is_finite() {
            return Err(ModelError::Invalid("avalanche root bracket"));
        }
    }
    if !high.is_finite() || f(high) < 0. {
        return Err(ModelError::Invalid("avalanche root bracket"));
    }
    for _ in 0..128 {
        let mid = f64::midpoint(low, high);
        if mid <= low || mid >= high {
            break;
        }
        if f(mid) >= 0. {
            high = mid;
        } else {
            low = mid;
        }
    }
    Ok(f64::midpoint(low, high))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching_root_resolves_tangent_and_small_positive_excess() {
        for (delta, nbv, expected) in [
            (0., 1. + f64::EPSILON, 2. * f64::EPSILON),
            (1e-24, 1., 2e-24_f64.sqrt()),
            (1e-20, 0.5, 2e-20),
            (0., 2., 1.256_431_208_626_169_7),
        ] {
            let root = solve_log_root(delta, nbv).unwrap();
            assert!((root / expected - 1.).abs() < 1e-10, "{delta} {nbv} {root}");
        }
    }
}
