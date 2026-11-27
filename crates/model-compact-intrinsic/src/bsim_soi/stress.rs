//! Native layout-dependent stress preparation.
//! Copyright 2010 Regents of the University of California. See package LICENSE.

use super::{BsimSoiThermalScalar, PreparedBsimSoiSize, positive, real};
use crate::{BoundParameters, ModelError};
const DELTA: f64 = 1e-9;

/// Temperature-independent layout stress offsets applied after size binning.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BsimSoiStressOffsets {
    pub threshold: f64,
    pub body_coefficient: f64,
    pub dibl: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Stress {
    mobility_scale: f64,
    temperature_coefficient: f64,
    reference: f64,
    actual: f64,
    pub(super) velocity_weight: f64,
    pub(super) offsets: BsimSoiStressOffsets,
}

impl Stress {
    pub(super) fn correction(
        self,
        ratio: BsimSoiThermalScalar,
        weight: f64,
    ) -> Result<BsimSoiThermalScalar, ModelError> {
        let base = self
            .mobility_scale
            .mul_add(self.temperature_coefficient.mul_add(ratio.value, 1.), DELTA);
        let base_slope = self.mobility_scale * self.temperature_coefficient * ratio.d_temperature;
        // (1 + weight*rho)/(1 + weight*rho_ref), algebraically
        // avoiding two divisions by the same temperature-dependent scale.
        let denominator = weight.mul_add(self.reference, base);
        let numerator = weight.mul_add(self.actual, base);
        let value = numerator / denominator;
        BsimSoiThermalScalar::new(value, base_slope * (1. - value) / denominator)
            .checked("layout stress correction")
    }
}

pub(super) fn prepare(
    model: &BoundParameters,
    p: &BoundParameters,
    size: &PreparedBsimSoiSize,
) -> Result<Option<Stress>, ModelError> {
    let sa = real(p, "sa")?;
    let sb = real(p, "sb")?;
    let sd = real(p, "sd")?;
    let nf = positive(real(p, "nf")?, "nf")?;
    if sa <= 0. || sb <= 0. || (nf > 1. && sd <= 0.) || nf < 1. {
        return Ok(None);
    }
    let geometry = size.geometry();
    let length = geometry.length;
    let width = positive(
        geometry.width_per_finger + real(model, "wlod")?.max(0.),
        "stress width",
    )?;
    let shape = |names: [&str; 5]| -> Result<f64, ModelError> {
        let [lp, wp, l, w, c] = names;
        let length = positive(length.powf(real(model, lp)?), "stress length power")?;
        let width = positive(width.powf(real(model, wp)?), "stress width power")?;
        Ok(1.
            + real(model, l)? / length
            + real(model, w)? / width
            + real(model, c)? / (length * width))
    };
    let mobility_scale = shape(["llodku0", "wlodku0", "lku0", "wku0", "pku0"])?;
    let vth_shape = shape(["llodvth", "wlodvth", "lkvth0", "wkvth0", "pkvth0"])?;
    let vth_scale = vth_shape.mul_add(vth_shape, DELTA).sqrt();
    let half = 0.5 * length;
    let reference = 1. / positive(real(model, "saref")? + half, "saref")?
        + 1. / positive(real(model, "sbref")? + half, "sbref")?;
    // Native sums over integral finger indices i < NF. Sum each
    // reciprocal progression with a bounded-cost analytic tail.
    let actual = (reciprocal_progression(sa + half, sd + length, nf)?
        + reciprocal_progression(sb + half, sd + length, nf)?)
        / nf;
    let offset = actual - reference;
    let ku0 = real(model, "ku0")?;
    let result = Stress {
        mobility_scale,
        temperature_coefficient: real(model, "tku0")?,
        reference: ku0 * reference,
        actual: ku0 * actual,
        velocity_weight: real(model, "kvsat")?.clamp(-1., 1.),
        offsets: BsimSoiStressOffsets {
            threshold: real(model, "kvth0")? * offset / vth_scale,
            body_coefficient: real(model, "stk2")? * offset / vth_scale.powf(real(model, "lodk2")?),
            dibl: real(model, "steta0")? * offset / vth_scale.powf(real(model, "lodeta0")?),
        },
    };
    if ![
        result.mobility_scale,
        result.reference,
        result.actual,
        result.offsets.threshold,
        result.offsets.body_coefficient,
        result.offsets.dibl,
    ]
    .into_iter()
    .all(f64::is_finite)
    {
        return Err(ModelError::InvalidPhysicalParameter("layout stress"));
    }
    Ok(Some(result))
}

// Direct prefix plus Euler-Maclaurin tail for the reciprocal progression.
// For a positive start and step the dimensionless tail starts at >=64;
// the first omitted Bernoulli term is below double precision.
fn reciprocal_progression(start: f64, step: f64, nf: f64) -> Result<f64, ModelError> {
    positive(start, "stress distance")?;
    let count = nf.ceil();
    if count <= 1. {
        return Ok(start.recip());
    }
    positive(step, "stress finger spacing")?;
    let prefix: f64 = (0..64)
        .map(f64::from)
        .take_while(|&i| i < count)
        .map(|i| i.mul_add(step, start).recip())
        .sum();
    if count <= 64. {
        return Ok(prefix);
    }
    let lower = 64.0_f64.mul_add(step, start);
    let upper = count.mul_add(step, start);
    let low = step / lower;
    let high = step / upper;
    let low2 = low * low;
    let high2 = high * high;
    let correction = |x: f64| {
        x * (-1.0_f64 / 240.)
            .mul_add(x, 1. / 252.)
            .mul_add(x, -1. / 120.)
            .mul_add(x, 1. / 12.)
    };
    // psi(upper/step)-psi(lower/step) =
    // log(upper/lower)+0.5*(low-high)+B2/2*(low²-high²)...
    let tail = 0.5_f64.mul_add(low - high, ((count - 64.) * (step / lower)).ln_1p())
        + correction(low2)
        - correction(high2);
    let result = prefix + tail / step;
    positive(result, "stress reciprocal progression")
}

#[cfg(test)]
mod tests {
    use super::reciprocal_progression;

    #[test]
    fn bounded_reciprocal_sum_agrees_with_direct_native_sum() -> Result<(), crate::ModelError> {
        for start in [1e-9, 1e-6, 1e-3] {
            for step in [1e-9, 1e-6, 1e-3] {
                for count in [1_u32, 63, 64, 65, 128, 1001, 10000] {
                    let expected: f64 = (0..count)
                        .map(|i| f64::from(i).mul_add(step, start).recip())
                        .sum();
                    let actual = reciprocal_progression(start, step, f64::from(count) - 0.25)?;
                    assert!((actual - expected).abs() <= 2e-13 * expected);
                }
            }
        }
        let huge = reciprocal_progression(1., 1., 1e12)?;
        assert!((huge - 28.208_236_780_830_58).abs() < 1e-11);
        Ok(())
    }
}
