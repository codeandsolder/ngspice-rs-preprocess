//! ngspice 47 body-diode avalanche transition and its temperature sensitivity.
//! The matching equation is from `vdmostemp.c`, with a convergent root solve
//! rather than its fixed 25-iteration approximation.

use super::{Breakdown, Jet, ModelError};

/// Computes XBV at the current temperature, including dXBV/dT for an exact
/// thermal Jacobian. The limiter/bypass policy remains the solver's decision.
pub fn effective_voltage(
    parameters: Breakdown,
    saturation_current: Jet,
    thermal_voltage: Jet,
) -> Result<Jet, ModelError> {
    let bv = parameters.bv.abs();
    let cbv = parameters.ibv;
    // This is ngspice 47's no-shift case when the requested breakdown
    // current is below the reverse-region current at BV.
    if cbv <= saturation_current.v * bv / thermal_voltage.v {
        return Ok(Jet::constant(bv));
    }

    // Set XBV = BV - NBV*Vt*y. The C matching equation becomes
    // y = log(B + NBV*y), with B = 1 + IBV/Is - BV/Vt.
    let b = 1.0 + cbv / saturation_current - bv / thermal_voltage;
    if !b.v.is_finite() || b.v < 1.0 {
        return Err(ModelError::Invalid("avalanche matching domain"));
    }

    let y = solve_log_root(b.v, parameters.nbv)?;
    // Differentiate the *converged* implicit equation:
    // dy/dT = B'(T) / (B + NBV*y - NBV), independent of iteration count.
    let denominator = parameters.nbv.mul_add(y - 1.0, b.v);
    if !denominator.is_finite() || denominator <= 0.0 {
        return Err(ModelError::Invalid("avalanche temperature derivative"));
    }
    let dydt = b.dt / denominator;
    let value = (parameters.nbv * thermal_voltage.v).mul_add(-y, bv);
    let dt = -parameters.nbv * thermal_voltage.dt.mul_add(y, thermal_voltage.v * dydt);
    if !value.is_finite() || !dt.is_finite() {
        return Err(ModelError::Invalid("avalanche voltage"));
    }
    Ok(Jet {
        v: value,
        dx: 0.0,
        dy: 0.0,
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
