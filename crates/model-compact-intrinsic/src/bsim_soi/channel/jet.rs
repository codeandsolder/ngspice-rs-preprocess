//! Fixed-size forward derivatives with respect to the physical bias coordinates.
use super::super::BsimSoiThermalScalar;
use std::ops::{Add, Div, Mul, Neg, Sub};

/// Scalar result and analytic gradient in [Vgs, Vds, Vbs, Ves, kelvin] order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BsimSoiChannelScalar {
    pub value: f64,
    pub gradient: [f64; 5],
}

pub(super) type Jet = BsimSoiChannelScalar;

impl Jet {
    pub(super) const fn constant(value: f64) -> Self {
        Self {
            value,
            gradient: [0.; 5],
        }
    }
    pub(super) const fn coordinate(value: f64, index: usize, scale: f64) -> Self {
        let mut gradient = [0.; 5];
        gradient[index] = scale;
        Self { value, gradient }
    }
    pub(super) const fn thermal(value: BsimSoiThermalScalar) -> Self {
        Self::coordinate(value.value, 4, value.d_temperature)
    }
    fn map(self, value: f64, slope: f64) -> Self {
        Self {
            value,
            gradient: self.gradient.map(|g| g * slope),
        }
    }
    pub(super) fn sqrt(self) -> Self {
        let root = self.value.sqrt();
        self.map(root, 0.5 / root)
    }
    pub(super) fn exp(self) -> Self {
        let exp = self.value.exp();
        self.map(exp, exp)
    }
    pub(super) fn ln(self) -> Self {
        self.map(self.value.ln(), self.value.recip())
    }
    pub(super) fn ln_1p(self) -> Self {
        self.map(self.value.ln_1p(), (1. + self.value).recip())
    }
    pub(super) fn tanh(self) -> Self {
        let value = self.value.tanh();
        self.map(value, (-value).mul_add(value, 1.))
    }
    pub(super) fn floor_exp(self) -> Self {
        if self.value <= -100. {
            Self::constant(3.720_075_976e-44)
        } else {
            self.exp()
        }
    }
    pub(super) fn limited_exp(self) -> Self {
        if self.value > 100. {
            (self - 99.) * 2.688_117_142e43
        } else {
            self.floor_exp()
        }
    }
    pub(super) fn half_positive(self, regularization: Self) -> Self {
        let root = (self * self + regularization).sqrt();
        if self.value >= 0. {
            (self + root) * 0.5
        } else {
            regularization / (root - self) * 0.5
        }
    }
    pub(super) fn checked(self, name: &'static str) -> Result<Self, crate::ModelError> {
        if self.value.is_finite() && self.gradient.into_iter().all(f64::is_finite) {
            Ok(self)
        } else {
            Err(crate::ModelError::InvalidPhysicalParameter(name))
        }
    }
}

impl Add for Jet {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        Self {
            value: self.value + rhs.value,
            gradient: std::array::from_fn(|i| self.gradient[i] + rhs.gradient[i]),
        }
    }
}
impl Sub for Jet {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        self + -rhs
    }
}
impl Mul for Jet {
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
impl Div for Jet {
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
impl Neg for Jet {
    type Output = Self;
    fn neg(self) -> Self {
        self.map(-self.value, -1.)
    }
}

macro_rules! scalar_ops {
    ($trait:ident, $method:ident) => {
        impl $trait<f64> for Jet {
            type Output = Self;
            fn $method(self, rhs: f64) -> Self {
                self.$method(Self::constant(rhs))
            }
        }
        impl $trait<Jet> for f64 {
            type Output = Jet;
            fn $method(self, rhs: Jet) -> Jet {
                Jet::constant(self).$method(rhs)
            }
        }
    };
}
scalar_ops!(Add, add);
scalar_ops!(Sub, sub);
scalar_ops!(Mul, mul);
scalar_ops!(Div, div);
