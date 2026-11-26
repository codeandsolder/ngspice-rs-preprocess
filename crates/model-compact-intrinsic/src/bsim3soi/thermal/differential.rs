//! Fixed forward derivatives for bias-local compact-model laws.
use std::ops::{Add, Div, Mul, Neg, Sub};

/// Axes: normalized VBS, normalized VDS, absolute temperature.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Differential {
    pub value: f64,
    pub gradient: [f64; 3],
}

impl Differential {
    pub const fn constant(value: f64) -> Self {
        Self {
            value,
            gradient: [0.0; 3],
        }
    }

    pub const fn variable(value: f64, axis: usize) -> Self {
        let mut gradient = [0.0; 3];
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

impl Add for Differential {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        Self {
            value: self.value + rhs.value,
            gradient: std::array::from_fn(|i| self.gradient[i] + rhs.gradient[i]),
        }
    }
}

impl Sub for Differential {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        self + (-rhs)
    }
}

impl Neg for Differential {
    type Output = Self;
    fn neg(self) -> Self {
        self.scale(-1.0)
    }
}

impl Mul for Differential {
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

impl Div for Differential {
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
