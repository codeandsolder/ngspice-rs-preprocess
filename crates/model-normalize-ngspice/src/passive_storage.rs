//! Immutable polynomial charge and flux laws for source-selected passives.
//!
//! `PSpice` VC1/VC2 and IL1/IL2 describe differential capacitance and
//! inductance respectively. The solver integrates the stored quantity,
//! not the instantaneous coefficient multiplied by the coordinate.

use anyhow::{Result, ensure};

/// Explicit prepared-deck model family for nonlinear `PSpice` capacitance.
pub const PSPICE_CAPACITOR: &str = "pspice_cap";
/// Explicit prepared-deck model family for nonlinear `PSpice` inductance.
pub const PSPICE_INDUCTOR: &str = "pspice_ind";

/// The same scalar constitutive polynomial represents Q(V) or Phi(I).
///
/// Its base is already adjusted for the occurrence's multiplier, geometry,
/// and operating/measurement temperature, once at candidate preparation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PolynomialStorage {
    base: f64,
    linear: f64,
    quadratic: f64,
}

/// Signed storage and its exact incremental value at the same coordinate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StoragePoint {
    /// Charge in coulombs or flux linkage in volt-seconds.
    pub stored: f64,
    /// Differential capacitance in farads or inductance in henries.
    pub differential: f64,
}

impl PolynomialStorage {
    /// Bind an effective base value and two differential coefficients.
    ///
    /// Signed or zero effective values are retained: negative storage
    /// elements can be intentional, and a valid polynomial may cross zero.
    /// A raw zero-valued `PSpice` instance is invalid at source binding, not
    /// here (a multiplier or temperature factor can make the base zero).
    ///
    /// # Errors
    /// Any input is nonfinite.
    pub fn new(base: f64, linear: f64, quadratic: f64) -> Result<Self> {
        ensure!(
            [base, linear, quadratic].into_iter().all(f64::is_finite),
            "nonfinite polynomial storage parameter"
        );
        Ok(Self {
            base,
            linear,
            quadratic,
        })
    }

    /// Evaluate the primitive and tangent without allocation.
    ///
    /// Storage is the integral from zero of the differential polynomial.
    /// Use this same function for initial voltage/current, ordinary trial
    /// states and accepted-state integration. The solver owns history.
    ///
    /// # Errors
    /// Nonfinite coordinate or nonrepresentable result.
    pub fn evaluate(self, coordinate: f64) -> Result<StoragePoint> {
        ensure!(coordinate.is_finite(), "nonfinite storage coordinate");
        if self.base == 0. || coordinate == 0. {
            return Ok(StoragePoint {
                stored: 0.,
                differential: self.base,
            });
        }
        let differential = self
            .quadratic
            .mul_add(coordinate, self.linear)
            .mul_add(coordinate, 1.)
            * self.base;
        let stored = (self.quadratic / 3.)
            .mul_add(coordinate, self.linear * 0.5)
            .mul_add(coordinate, 1.)
            * self.base
            * coordinate;
        ensure!(
            stored.is_finite() && differential.is_finite(),
            "nonfinite polynomial storage result"
        );
        Ok(StoragePoint {
            stored,
            differential,
        })
    }

    /// Stored energy relative to the zero coordinate, in joules.
    ///
    /// This is the integral of x times the differential coefficient.
    /// It is not x times storage divided by two for a nonlinear device.
    ///
    /// # Errors
    /// Nonfinite coordinate or nonrepresentable energy.
    pub fn energy(self, coordinate: f64) -> Result<f64> {
        ensure!(coordinate.is_finite(), "nonfinite storage coordinate");
        if self.base == 0. || coordinate == 0. {
            return Ok(0.);
        }
        let energy = (self.quadratic * 0.25)
            .mul_add(coordinate, self.linear / 3.)
            .mul_add(coordinate, 0.5)
            * self.base
            * coordinate
            * coordinate;
        ensure!(energy.is_finite(), "nonfinite polynomial storage energy");
        Ok(energy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() <= 2e-12 * expected.abs().max(1e-12),
            "{actual} != {expected}"
        );
    }

    #[test]
    fn signed_storage_tangent_and_energy_are_independent_polynomials() -> Result<()> {
        for base in [-4e-3, 1e-9, 2e-3, 0.] {
            let law = PolynomialStorage::new(base, -0.3, 0.07)?;
            for index in -40..=40 {
                let x = f64::from(index) / 10.;
                let point = law.evaluate(x)?;
                let reference =
                    base * ((0.07_f64 / 3.).mul_add(x.powi(3), 0.15_f64.mul_add(-x.powi(2), x)));
                let slope = base * (0.07_f64.mul_add(x.powi(2), 0.3_f64.mul_add(-x, 1.)));
                let energy = base
                    * (0.0175_f64.mul_add(x.powi(4), 0.1_f64.mul_add(-x.powi(3), 0.5 * x.powi(2))));
                close(point.stored, reference);
                close(point.differential, slope);
                close(law.energy(x)?, energy);
            }
        }
        Ok(())
    }

    #[test]
    fn finite_differences_and_independent_integrals() -> Result<()> {
        let law = PolynomialStorage::new(2e-3, 0.2, -0.03)?;
        // Integrate differential C/L and x*C/L from a nonzero initial
        // condition. Simpson quadrature is exact for these polynomials.
        let (start, end) = (-3., 2.);
        let h = (end - start) / 200.;
        let mut storage_delta = 0.;
        let mut energy_delta = 0.;
        for i in 0..=200 {
            let x = f64::from(i).mul_add(h, start);
            let weight: f64 = if i == 0 || i == 200 {
                1.
            } else if i % 2 == 0 {
                2.
            } else {
                4.
            };
            let slope = 2e-3 * ((0.03 * x).mul_add(-x, 0.2_f64.mul_add(x, 1.)));
            storage_delta = weight.mul_add(slope, storage_delta);
            energy_delta = (weight * x).mul_add(slope, energy_delta);
            let step = 1e-5;
            let finite_difference =
                (law.evaluate(x + step)?.stored - law.evaluate(x - step)?.stored) / (2. * step);
            assert!((finite_difference - law.evaluate(x)?.differential).abs() < 1e-12);
        }
        close(
            law.evaluate(end)?.stored - law.evaluate(start)?.stored,
            storage_delta * h / 3.,
        );
        close(law.energy(end)? - law.energy(start)?, energy_delta * h / 3.);
        Ok(())
    }

    #[test]
    fn linear_limit_and_zero_crossing_are_not_clamped() -> Result<()> {
        let linear = PolynomialStorage::new(2e-6, 0., 0.)?;
        assert_eq!(
            linear.evaluate(-2.)?.stored.to_bits(),
            (-4e-6_f64).to_bits()
        );
        let crossing = PolynomialStorage::new(2., -1., 0.)?;
        close(crossing.evaluate(1.)?.differential, 0.);
        close(crossing.evaluate(2.)?.differential, -2.);
        assert!(PolynomialStorage::new(f64::NAN, 0., 0.).is_err());
        assert!(PolynomialStorage::new(1., f64::INFINITY, 0.).is_err());
        assert!(crossing.evaluate(f64::INFINITY).is_err());
        assert!(crossing.evaluate(f64::MAX).is_err());
        assert!(crossing.energy(f64::MAX).is_err());
        Ok(())
    }
}
