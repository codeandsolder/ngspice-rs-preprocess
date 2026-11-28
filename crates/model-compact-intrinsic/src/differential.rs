//! Fixed-size forward derivatives with respect to the physical bias coordinates.
use std::ops::{Add, Div, Mul, Neg, Sub};

/// Scalar and analytic gradient in the owning constitutive law’s coordinate order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Differential<const N: usize> {
    pub value: f64,
    pub gradient: [f64; N],
}

impl<const N: usize> Differential<N> {
    pub(crate) const fn constant(value: f64) -> Self {
        Self {
            value,
            gradient: [0.; N],
        }
    }
    pub(crate) const fn coordinate(value: f64, index: usize, scale: f64) -> Self {
        let mut gradient = [0.; N];
        gradient[index] = scale;
        Self { value, gradient }
    }
    pub(crate) fn map(self, value: f64, slope: f64) -> Self {
        Self {
            value,
            gradient: self.gradient.map(|g| g * slope),
        }
    }
    pub(crate) fn sqrt(self) -> Self {
        let root = self.value.sqrt();
        self.map(root, 0.5 / root)
    }
    pub(crate) fn exp(self) -> Self {
        let exp = self.value.exp();
        self.map(exp, exp)
    }
    pub(crate) fn exp_m1(self) -> Self {
        self.map(self.value.exp_m1(), self.value.exp())
    }
    /// Real power of a strictly positive constitutive argument.
    pub(crate) fn positive_pow(self, exponent: f64) -> Self {
        let power = self.value.powf(exponent);
        self.map(power, exponent * power / self.value)
    }
    pub(crate) fn ln(self) -> Self {
        self.map(self.value.ln(), self.value.recip())
    }
    pub(crate) fn ln_1p(self) -> Self {
        self.map(self.value.ln_1p(), (1. + self.value).recip())
    }
    pub(crate) fn tanh(self) -> Self {
        let value = self.value.tanh();
        self.map(value, (-value).mul_add(value, 1.))
    }
    pub(crate) fn half_positive(self, regularization: Self) -> Self {
        let root = (self * self + regularization).sqrt();
        if self.value >= 0. {
            (self + root) * 0.5
        } else {
            regularization / (root - self) * 0.5
        }
    }
    pub(crate) fn checked(self, name: &'static str) -> Result<Self, crate::ModelError> {
        if self.value.is_finite() && self.gradient.into_iter().all(f64::is_finite) {
            Ok(self)
        } else {
            Err(crate::ModelError::InvalidPhysicalParameter(name))
        }
    }
}

impl<const N: usize> Add for Differential<N> {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        Self {
            value: self.value + rhs.value,
            gradient: std::array::from_fn(|i| self.gradient[i] + rhs.gradient[i]),
        }
    }
}
impl<const N: usize> Sub for Differential<N> {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        self + -rhs
    }
}
impl<const N: usize> Mul for Differential<N> {
    type Output = Self;
    fn mul(self, rhs: Self) -> Self {
        Self {
            value: self.value * rhs.value,
            gradient: std::array::from_fn(|i| {
                self.gradient[i].mul_add(rhs.value, self.value * rhs.gradient[i])
            }),
        }
    }
}
impl<const N: usize> Div for Differential<N> {
    type Output = Self;
    fn div(self, rhs: Self) -> Self {
        let value = self.value / rhs.value;
        Self {
            value,
            gradient: std::array::from_fn(|i| {
                (-value).mul_add(rhs.gradient[i], self.gradient[i]) / rhs.value
            }),
        }
    }
}
impl<const N: usize> Neg for Differential<N> {
    type Output = Self;
    fn neg(self) -> Self {
        self.map(-self.value, -1.)
    }
}

macro_rules! scalar_ops {
    ($trait:ident, $method:ident) => {
        impl<const N: usize> $trait<f64> for Differential<N> {
            type Output = Self;
            fn $method(self, rhs: f64) -> Self {
                self.$method(Self::constant(rhs))
            }
        }
        impl<const N: usize> $trait<Differential<N>> for f64 {
            type Output = Differential<N>;
            fn $method(self, rhs: Differential<N>) -> Differential<N> {
                Differential::constant(self).$method(rhs)
            }
        }
    };
}
scalar_ops!(Add, add);
scalar_ops!(Sub, sub);
scalar_ops!(Mul, mul);
scalar_ops!(Div, div);

impl<const N: usize> Default for Differential<N> {
    fn default() -> Self {
        Self::constant(0.)
    }
}
