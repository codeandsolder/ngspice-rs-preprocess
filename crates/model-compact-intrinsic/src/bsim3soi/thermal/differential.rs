//! Fixed forward derivatives for bias-local compact-model laws.
use std::ops::{Add, Div, Mul, Neg, Sub};

/// Fixed-size forward gradient with caller-selected coordinate axes.
#[derive(Clone, Copy, Debug)]
pub(super) struct Differential<const N: usize = 3> {
    pub value: f64,
    pub gradient: [f64; N],
}

impl<const N: usize> Differential<N> {
    pub const fn constant(value: f64) -> Self {
        Self {
            value,
            gradient: [0.0; N],
        }
    }

    pub const fn variable(value: f64, axis: usize) -> Self {
        let mut gradient = [0.0; N];
        gradient[axis] = 1.0;
        Self { value, gradient }
    }

    pub fn chain(self, value: f64, slope: f64) -> Self {
        Self {
            value,
            gradient: self.gradient.map(|d| d * slope),
        }
    }

    pub fn scale(self, factor: f64) -> Self {
        self.chain(self.value * factor, factor)
    }

    pub fn exp(self) -> Self {
        let e = self.value.exp();
        self.chain(e, e)
    }

    pub fn exp_m1(self) -> Self {
        self.chain(self.value.exp_m1(), self.value.exp())
    }

    pub fn ln(self) -> Self {
        self.chain(self.value.ln(), self.value.recip())
    }

    pub fn ln_1p(self) -> Self {
        self.chain(self.value.ln_1p(), (1.0 + self.value).recip())
    }

    pub fn sqrt(self) -> Self {
        let root = self.value.sqrt();
        self.chain(root, 0.5 / root)
    }

    pub fn powf(self, power: f64) -> Self {
        self.chain(self.value.powf(power), power * self.value.powf(power - 1.0))
    }

    pub fn reciprocal(self) -> Self {
        let inverse = self.value.recip();
        self.chain(inverse, -inverse * inverse)
    }

    pub fn finite(self) -> bool {
        self.value.is_finite() && self.gradient.into_iter().all(f64::is_finite)
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
        self + (-rhs)
    }
}

impl<const N: usize> Neg for Differential<N> {
    type Output = Self;
    fn neg(self) -> Self {
        self.scale(-1.0)
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

impl<const N: usize> Default for Differential<N> {
    fn default() -> Self {
        Self::constant(0.0)
    }
}
