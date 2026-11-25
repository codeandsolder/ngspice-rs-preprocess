//! Shared first-order forward automatic differentiation for HICUM
//! electrothermal compact-model equations.
//!
//! A single statically specialized scalar lane; no dynamic dispatch,
//! extra allocation, numerical perturbation or hidden callbacks.

use std::ops::{Add, Div, Mul, Neg, Sub};

pub(super) trait Scalar:
    Copy
    + Add<Output = Self>
    + Sub<Output = Self>
    + Mul<Output = Self>
    + Div<Output = Self>
    + Neg<Output = Self>
    + Mul<f64, Output = Self>
    + Div<f64, Output = Self>
{
    fn constant(value: f64) -> Self;
    fn value(self) -> f64;
    fn exp(self) -> Self;
    fn exp_m1(self) -> Self;
    fn ln(self) -> Self;
    fn ln_1p(self) -> Self;
    fn midpoint(self, other: Self) -> Self;
    fn hypot(self, other: f64) -> Self;
    fn mul_add(self, multiplier: Self, addend: Self) -> Self;
    fn is_finite(self) -> bool;
}

impl Scalar for f64 {
    fn constant(value: f64) -> Self {
        value
    }
    fn value(self) -> f64 {
        self
    }
    fn exp(self) -> Self {
        self.exp()
    }
    fn exp_m1(self) -> Self {
        self.exp_m1()
    }
    fn ln(self) -> Self {
        self.ln()
    }
    fn ln_1p(self) -> Self {
        self.ln_1p()
    }
    fn midpoint(self, other: Self) -> Self {
        self.midpoint(other)
    }
    fn hypot(self, other: f64) -> Self {
        self.hypot(other)
    }
    fn mul_add(self, multiplier: Self, addend: Self) -> Self {
        self.mul_add(multiplier, addend)
    }
    fn is_finite(self) -> bool {
        self.is_finite()
    }
}

/// Value and exact d/dTdev. No array or dynamic derivative metadata.
#[derive(Clone, Copy)]
pub(super) struct Dual {
    pub(super) value: f64,
    pub(super) slope: f64,
}

impl Dual {
    pub(super) const fn new(value: f64, slope: f64) -> Self {
        Self { value, slope }
    }
    fn unary(self, value: f64, derivative: f64) -> Self {
        Self::new(value, derivative * self.slope)
    }
}

impl Scalar for Dual {
    fn constant(value: f64) -> Self {
        Self::new(value, 0.0)
    }
    fn value(self) -> f64 {
        self.value
    }
    fn exp(self) -> Self {
        let value = self.value.exp();
        self.unary(value, value)
    }
    fn exp_m1(self) -> Self {
        self.unary(self.value.exp_m1(), self.value.exp())
    }
    fn ln(self) -> Self {
        self.unary(self.value.ln(), 1.0 / self.value)
    }
    fn ln_1p(self) -> Self {
        self.unary(self.value.ln_1p(), 1.0 / (1.0 + self.value))
    }
    fn midpoint(self, other: Self) -> Self {
        Self::new(
            self.value.midpoint(other.value),
            self.slope.midpoint(other.slope),
        )
    }
    fn hypot(self, other: f64) -> Self {
        let value = self.value.hypot(other);
        self.unary(value, self.value / value)
    }
    fn mul_add(self, multiplier: Self, addend: Self) -> Self {
        Self::new(
            self.value.mul_add(multiplier.value, addend.value),
            self.slope.mul_add(
                multiplier.value,
                self.value.mul_add(multiplier.slope, addend.slope),
            ),
        )
    }
    fn is_finite(self) -> bool {
        self.value.is_finite() && self.slope.is_finite()
    }
}

impl Add for Dual {
    type Output = Self;
    fn add(self, other: Self) -> Self {
        Self::new(self.value + other.value, self.slope + other.slope)
    }
}
impl Sub for Dual {
    type Output = Self;
    fn sub(self, other: Self) -> Self {
        Self::new(self.value - other.value, self.slope - other.slope)
    }
}
impl Neg for Dual {
    type Output = Self;
    fn neg(self) -> Self {
        Self::new(-self.value, -self.slope)
    }
}
impl Mul for Dual {
    type Output = Self;
    fn mul(self, other: Self) -> Self {
        Self::new(
            self.value * other.value,
            self.slope.mul_add(other.value, self.value * other.slope),
        )
    }
}
impl Div for Dual {
    type Output = Self;
    fn div(self, other: Self) -> Self {
        let inverse = 1.0 / other.value;
        let value = self.value * inverse;
        Self::new(value, value.mul_add(-other.slope, self.slope) * inverse)
    }
}
impl Mul<f64> for Dual {
    type Output = Self;
    fn mul(self, factor: f64) -> Self {
        Self::new(self.value * factor, self.slope * factor)
    }
}
impl Div<f64> for Dual {
    type Output = Self;
    fn div(self, factor: f64) -> Self {
        self * (1.0 / factor)
    }
}

impl Add<f64> for Dual {
    type Output = Self;
    fn add(self, value: f64) -> Self {
        Self::new(self.value + value, self.slope)
    }
}
impl Sub<f64> for Dual {
    type Output = Self;
    fn sub(self, value: f64) -> Self {
        Self::new(self.value - value, self.slope)
    }
}
