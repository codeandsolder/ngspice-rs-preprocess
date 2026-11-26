//! Stack-only forward differentiation of a three-coordinate constitutive law.

use std::ops::{Add, Div, Mul, Neg, Sub};

#[derive(Clone, Copy)]
pub(super) struct Jet {
    pub(super) value: f64,
    pub(super) gradient: [f64; 3],
}

impl Jet {
    pub(super) const fn constant(value: f64) -> Self {
        Self {
            value,
            gradient: [0.; 3],
        }
    }

    pub(super) const fn coordinate(value: f64, index: usize) -> Self {
        let mut point = Self::constant(value);
        point.gradient[index] = 1.;
        point
    }

    fn unary(self, value: f64, slope: f64) -> Self {
        Self {
            value,
            gradient: self.gradient.map(|d| d * slope),
        }
    }

    fn binary(self, rhs: Self, value: f64, a: f64, b: f64) -> Self {
        Self {
            value,
            gradient: std::array::from_fn(|i| a.mul_add(self.gradient[i], b * rhs.gradient[i])),
        }
    }

    pub(super) fn exp(self) -> Self {
        let value = self.value.exp();
        self.unary(value, value)
    }

    pub(super) fn exp_m1(self) -> Self {
        self.unary(self.value.exp_m1(), self.value.exp())
    }

    pub(super) fn sqrt(self) -> Self {
        let value = self.value.sqrt();
        self.unary(value, 0.5 / value)
    }

    /// A bound is part of the constitutive law, not a Jacobian-only clamp.
    pub(super) const fn max(self, lower: f64) -> Self {
        if self.value < lower {
            Self::constant(lower)
        } else {
            self
        }
    }

    pub(super) const fn min(self, upper: f64) -> Self {
        if self.value > upper {
            Self::constant(upper)
        } else {
            self
        }
    }

    pub(super) fn finite(self) -> bool {
        self.value.is_finite() && self.gradient.into_iter().all(f64::is_finite)
    }
}

impl From<f64> for Jet {
    fn from(value: f64) -> Self {
        Self::constant(value)
    }
}

impl<T: Into<Self>> Add<T> for Jet {
    type Output = Self;
    fn add(self, rhs: T) -> Self {
        let rhs = rhs.into();
        self.binary(rhs, self.value + rhs.value, 1., 1.)
    }
}

impl<T: Into<Self>> Sub<T> for Jet {
    type Output = Self;
    fn sub(self, rhs: T) -> Self {
        let rhs = rhs.into();
        self.binary(rhs, self.value - rhs.value, 1., -1.)
    }
}

impl<T: Into<Self>> Mul<T> for Jet {
    type Output = Self;
    fn mul(self, rhs: T) -> Self {
        let rhs = rhs.into();
        self.binary(rhs, self.value * rhs.value, rhs.value, self.value)
    }
}

impl Jet {
    fn quotient(self, rhs: Self) -> Self {
        let value = self.value / rhs.value;
        // Divide after the derivative subtraction; do not square the denominator.
        let gradient = std::array::from_fn(|i| {
            (-value).mul_add(rhs.gradient[i], self.gradient[i]) / rhs.value
        });
        Self { value, gradient }
    }
}

impl<T: Into<Self>> Div<T> for Jet {
    type Output = Self;
    fn div(self, rhs: T) -> Self {
        self.quotient(rhs.into())
    }
}

impl Neg for Jet {
    type Output = Self;
    fn neg(self) -> Self {
        self.unary(-self.value, -1.)
    }
}
