//! Single source QJMOD collector punch-through law for both normal
//! physical evaluation and exact first-order Tdev differentiation.
//!
//! The published source Cj value is not identical to dQj/dV around
//! punch-through, so preserve each as an independent native result.
//! The static scalar is f64 for electrical trials and a one-lane dual
//! for electrothermal derivatives. No finite differences or dynamic
//! dispatch occur inside the physical kernel.

use super::super::differentiation::{Dual, Scalar};
use super::{CoefficientSlopes, Collector, Junction};
use crate::EvaluationError;

const SOURCE_EXP_LIMIT: f64 = 80.0;
const SOURCE_COLLECTOR_CAP_RATIO: f64 = 2.4;

#[derive(Clone, Copy)]
struct Coefficients<S> {
    capacitance: S,
    potential: S,
    grading: f64,
    punch_through: S,
}

#[derive(Clone, Copy)]
struct ResultValue<S> {
    charge: S,
    capacitance: S,
    source_capacitance: S,
    d_source_capacitance: S,
}

fn ungraded_integral<S: Scalar>(log: S, amplitude: S, grading: f64) -> S {
    amplitude * (-(log * (1.0 - grading)).exp_m1()) / (1.0 - grading)
}

fn depletion_integral<S: Scalar>(p: Coefficients<S>, first_log: S, second_log: S, reach: S) -> S {
    let reverse_grade = p.grading * 0.25;
    p.potential
        * (ungraded_integral(second_log, p.capacitance, p.grading)
            + ungraded_integral(first_log, reach, reverse_grade)
            - ungraded_integral(second_log, reach, reverse_grade))
}

/// The only implementation of the source QJMOD punch-through branch.
fn physical<S: Scalar>(
    p: Coefficients<S>,
    voltage: S,
    vt: S,
) -> Result<ResultValue<S>, EvaluationError> {
    let one = S::constant(1.0);
    let max_cap = p.capacitance * SOURCE_COLLECTOR_CAP_RATIO;
    let reverse_grading = p.grading * 0.25;
    let separation = p.punch_through - p.potential;
    let smooth_width = separation.mul_add(S::constant(0.1), vt * 4.0);
    if smooth_width.value() <= 0.0 {
        return Err(EvaluationError::InvalidPhysicalDomain);
    }
    let threshold =
        p.potential * (one - S::constant((-SOURCE_COLLECTOR_CAP_RATIO.ln() / p.grading).exp()));
    let first = (threshold - voltage) / vt;
    let (forward_weight, first_voltage) = if first.value() < SOURCE_EXP_LIMIT {
        let root = first.exp();
        (
            root / (one + root),
            vt.mul_add(-first.exp().ln_1p(), threshold),
        )
    } else {
        (one, voltage)
    };
    let second = (separation + first_voltage) / smooth_width;
    let (reverse_weight, reverse_voltage) = if second.value() < SOURCE_EXP_LIMIT {
        let exponent = second.exp();
        (
            exponent / (one + exponent),
            smooth_width.mul_add(
                exponent.ln_1p() - (-(separation + threshold) / smooth_width).exp(),
                -separation,
            ),
        )
    } else {
        (one, first_voltage)
    };
    let log_first = (-first_voltage / p.potential).ln_1p();
    let log_second = (-reverse_voltage / p.potential).ln_1p();
    let reach = p.capacitance
        * ((p.punch_through / p.potential).ln() * (reverse_grading - p.grading)).exp();
    let graded_second = p.capacitance * (log_second * (-p.grading)).exp();
    let intrinsic = graded_second * forward_weight * reverse_weight;
    let reachthrough = reach * (log_first * (-reverse_grading)).exp();
    let source_capacitance = max_cap.mul_add(
        one - forward_weight,
        reachthrough.mul_add(one - reverse_weight, intrinsic),
    );
    // Exact derivative of the source-defined C result, not d²Q/dV².
    let d_forward_weight = if first.value() < SOURCE_EXP_LIMIT {
        -(forward_weight * (one - forward_weight)) / vt
    } else {
        S::constant(0.0)
    };
    let d_reverse_weight = if second.value() < SOURCE_EXP_LIMIT {
        reverse_weight * (one - reverse_weight) * forward_weight / smooth_width
    } else {
        S::constant(0.0)
    };
    let graded_second_slope =
        forward_weight * forward_weight * reverse_weight * reverse_weight * p.grading
            / (p.potential - reverse_voltage);
    let intrinsic_cap_gradient = graded_second
        * forward_weight.mul_add(
            d_reverse_weight,
            d_forward_weight.mul_add(reverse_weight, graded_second_slope),
        );
    let reachthrough_gradient = reachthrough
        * (forward_weight * (one - reverse_weight) * reverse_grading
            / (p.potential - first_voltage)
            - d_reverse_weight);
    let d_source_capacitance = (-max_cap).mul_add(
        d_forward_weight,
        intrinsic_cap_gradient + reachthrough_gradient,
    );
    // Native QJMOD return C differs from the published source Cj term
    // in the reach-through branch. Preserve both independent results.
    let reverse_second = reach * (log_second * (-reverse_grading)).exp();
    let capacitance = max_cap.mul_add(
        one - forward_weight,
        reverse_second
            .mul_add(-reverse_weight, reachthrough)
            .mul_add(forward_weight, intrinsic),
    );
    let integral = depletion_integral(p, log_first, log_second, reach);
    let charge = max_cap.mul_add(voltage - first_voltage, integral);
    if ![
        charge,
        capacitance,
        source_capacitance,
        d_source_capacitance,
    ]
    .iter()
    .all(|x| x.is_finite())
    {
        return Err(EvaluationError::NonFiniteResult);
    }
    Ok(ResultValue {
        charge,
        capacitance,
        source_capacitance,
        d_source_capacitance,
    })
}

/// Source f64 core for the reach-through branch.
pub(super) fn evaluate(p: Collector, voltage: f64, vt: f64) -> Result<Junction, EvaluationError> {
    let result = physical(
        Coefficients {
            capacitance: p.zero_bias_capacitance,
            potential: p.potential,
            grading: p.grading,
            punch_through: p.punch_through,
        },
        voltage,
        vt,
    )?;
    Ok(Junction {
        coulombs: result.charge,
        farads: result.capacitance,
        source_capacitance: result.source_capacitance,
        d_source_capacitance: result.d_source_capacitance,
    })
}

/// Complete source dQj/dTdev and the two distinct capacitance slopes
/// at constant junction voltage for the reach-through QJMOD branch.
pub(super) fn thermal(
    p: Collector,
    slopes: CoefficientSlopes,
    voltage: f64,
    vt: f64,
    kelvin: f64,
) -> Result<super::CollectorSensitivity, EvaluationError> {
    let result = physical(
        Coefficients {
            capacitance: Dual::new(p.zero_bias_capacitance, slopes.zero_bias_capacitance),
            potential: Dual::new(p.potential, slopes.potential),
            grading: p.grading,
            punch_through: Dual::new(p.punch_through, slopes.maximum_or_punchthrough),
        },
        Dual::constant(voltage),
        Dual::new(vt, vt / kelvin),
    )?;
    Ok(super::CollectorSensitivity {
        charge: result.charge.slope,
        capacitance: result.capacitance.slope,
        source_capacitance: result.source_capacitance.slope,
    })
}
