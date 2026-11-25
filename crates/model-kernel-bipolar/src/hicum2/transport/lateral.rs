//! HICUM 2.4.0 HICQFC lateral minority-charge spreading.
//!
//! Implements both length/width branches and both HICCR charge-weighting
//! forms of HICFCI/HICFCT. The returned factors are dimensionless functions
//! of Itf/Ick, and Jet carries the exact source charge derivatives.

use super::jet::Scalar;
use crate::EvaluationError;

/// Actual and ICCR-weighted collector-spreading factors.
pub(super) fn factors<S: Scalar>(
    itf: S,
    critical: S,
    ahc: f64,
    width_geometry: f64,
    length_geometry: f64,
    legacy_weighting: bool,
) -> Result<(S, S), EvaluationError> {
    let one = S::constant(1.0);
    let a = one - critical / itf;
    let a_ck = one - (a + (a * a + ahc).sqrt()) / (1.0 + (1.0 + ahc).sqrt());
    let (actual, weighted) = if width_geometry > length_geometry {
        let delta = width_geometry - length_geometry;
        let (width, log_factor) = if width_geometry > 0.01 {
            let ratio_log = ((1.0 + width_geometry) / (1.0 + length_geometry)).ln();
            let scaled = ((a_ck - 1.0) * ratio_log).exp();
            let width = (scaled - 1.0) / (S::constant(length_geometry) - scaled * width_geometry);
            let log_factor =
                ((one + width * width_geometry) / (one + width * length_geometry)).ln();
            (width, log_factor)
        } else {
            let width = (one - a_ck) / (a_ck * width_geometry + 1.0);
            (width, width * delta)
        };
        let main = width
            + (width * width) * (width_geometry.midpoint(length_geometry))
            + (width * width * width) * (width_geometry * length_geometry / 3.0);
        let integral = (log_factor * main - hicfci(width_geometry, length_geometry, width)
            + hicfci(length_geometry, width_geometry, width))
            * (2.0 / delta);
        let weighted = if legacy_weighting {
            (hicfct(width_geometry, width) - hicfct(length_geometry, width)) * (2.0 / delta)
        } else {
            integral
        };
        (integral, weighted)
    } else {
        let width = if width_geometry > 0.01 {
            (one - a_ck) / (a_ck * width_geometry + 1.0)
        } else {
            one - a_ck - a_ck * width_geometry
        };
        let z = width * width_geometry;
        let first = width * width * (one + z / 3.0) / (one + z);
        let weighted = if legacy_weighting {
            if z.scalar() > 0.001 {
                ((one + z) * (one + z).ln() - z) * 2.0
                    / (one + z)
                    / (width_geometry * width_geometry)
            } else {
                width * width * (one - z / 3.0) / (one + z)
            }
        } else {
            first
        };
        (first, weighted)
    };
    if !actual.is_finite() || !weighted.is_finite() {
        return Err(EvaluationError::NonFiniteResult);
    }
    Ok((actual, weighted))
}

fn hicfci<S: Scalar>(width_factor: f64, length_factor: f64, w: S) -> S {
    let z = w * width_factor;
    let log = z.ln_1p();
    if z.scalar() > 1e-6 {
        let x = z + 1.0;
        let x_sq = x * x;
        let first = (x_sq * (log * 2.0 - 1.0) + 1.0) * 0.25;
        let second = (x_sq * x * (log * 3.0 - 1.0) + 1.0) / 9.0;
        let mix = length_factor / width_factor;
        (first * (1.0 - mix) + second * mix) / width_factor
    } else {
        let z_sq = z * z;
        let first = (z * z_sq) * 0.1 - z_sq * 0.25 + z + 3.0;
        let second = (z * z_sq) * -0.2 + z_sq * 0.75 + z * 2.0;
        let square_width = w * w;
        (first * width_factor + second * length_factor) * square_width / 6.0
    }
}

fn hicfct<S: Scalar>(factor: f64, w: S) -> S {
    let z = w * factor;
    if z.scalar() > 1e-6 {
        (z - z.ln_1p()) / factor
    } else {
        z * w * 0.5
    }
}
