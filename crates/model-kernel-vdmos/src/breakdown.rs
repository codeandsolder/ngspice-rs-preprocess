//! ngspice 47 body-diode avalanche transition and its temperature sensitivity.
//! The matching equation is from `vdmostemp.c`, with a convergent root solve
//! rather than its fixed 25-iteration approximation.

use super::{Breakdown, Jet, ModelError};

/// Computes XBV at the current temperature, including dXBV/dT for an exact
/// thermal Jacobian. The limiter/bypass policy remains the solver's decision.
pub fn effective_voltage<S: model_kernel_api::taylor::arithmetic::Scalar>(
    parameters: Breakdown,
    saturation_current: Jet<S>,
    thermal_voltage: Jet<S>,
) -> Result<Jet<S>, ModelError> {
    let bv = parameters.bv.abs();
    let cbv = parameters.ibv;
    // This is ngspice 47's no-shift case when the requested breakdown
    // current is below the reverse-region current at BV.
    if (saturation_current.v.scale(bv) / thermal_voltage.v).compare(cbv)
        != Some(std::cmp::Ordering::Less)
    {
        return Ok(Jet::constant(bv));
    }
    // y = log(B + NBV*y), with B = 1 + IBV/Is - BV/Vt.
    let b = 1.0 + cbv / saturation_current - bv / thermal_voltage;
    if !b.v.is_finite() || b.v.value() < 1.0 {
        return Err(ModelError::Invalid("avalanche matching domain"));
    }
    let root = solve_log_root(b.v.value(), parameters.nbv)?;
    let mut y = S::constant(root);
    let mut matched = 1;
    // Formal Newton on the implicit law doubles the matched coefficient
    // order. Keep the independently converged scalar root exactly fixed.
    while matched <= b.v.degree() {
        let argument = b.v + y.scale(parameters.nbv);
        let residual = (y - argument.ln()).with_value(0.);
        let derivative = S::constant(1.) - S::constant(parameters.nbv) / argument;
        y = (y - residual / derivative).with_value(root);
        matched *= 2;
    }
    let denominator = S::constant(parameters.nbv).mul_add(y.offset(-1.), b.v);
    if !denominator.is_finite() || denominator.value() <= 0.0 {
        return Err(ModelError::Invalid("avalanche temperature derivative"));
    }
    let dydt = b.dt / denominator;
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

fn solve_log_root(b: f64, nbv: f64) -> Result<f64, ModelError> {
    let f = |y: f64| y - b.mul_add(1.0, nbv * y).ln();
    let mut y = b.ln().max(1e-10);
    for _ in 0..24 {
        let next = b.mul_add(1.0, nbv * y).ln();
        if (next - y).abs() <= 1e-13 * (1.0 + next.abs()) {
            return Ok(next);
        }
        y = next;
    }
    // Slow fixed-point convergence near the tangent bifurcation: bracket the
    // positive root and bisect a monotone right-hand branch instead.
    let mut low = 0.0;
    let mut high = (b.ln() + nbv.ln_1p() + 1.0).max(1.0);
    for _ in 0..64 {
        if f(high) >= 0.0 {
            break;
        }
        high *= 2.0;
        if !high.is_finite() {
            return Err(ModelError::Invalid("avalanche root bracket"));
        }
    }
    if f(high) < 0.0 {
        return Err(ModelError::Invalid("avalanche root bracket"));
    }
    for _ in 0..64 {
        let mid = f64::midpoint(low, high);
        if f(mid) >= 0.0 {
            high = mid;
        } else {
            low = mid;
        }
    }
    Ok(f64::midpoint(low, high))
}
