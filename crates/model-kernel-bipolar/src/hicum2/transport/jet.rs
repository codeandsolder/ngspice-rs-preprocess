//! Small fixed-size forward derivative, with B-E/B-C/total-hole-charge axes.
//!
//! HICCR is implicit in total hole charge: a third differential direction
//! allows solving dQp/dV analytically without per-step numerical differences,
//! dynamic tapes, or a generic differentiation runtime.

use std::ops::{Add, Div, Mul, Neg, Sub};

#[derive(Clone, Copy, Debug, Default)]
pub struct Jet {
    pub value: f64,
    pub be: f64,
    pub bc: f64,
    pub hole: f64,
    /// Fixed-source-voltage and fixed-hole-charge thermal lane.
    pub temperature: f64,
}

impl Jet {
    pub const fn new(value: f64, be: f64, bc: f64, hole: f64) -> Self {
        Self {
            value,
            be,
            bc,
            hole,
            temperature: 0.0,
        }
    }

    pub const fn thermal_constant(value: f64, slope: f64) -> Self {
        Self {
            value,
            be: 0.0,
            bc: 0.0,
            hole: 0.0,
            temperature: slope,
        }
    }

    pub const fn with_temperature(self, slope: f64) -> Self {
        Self {
            temperature: slope,
            ..self
        }
    }

    pub const fn constant(value: f64) -> Self {
        Self::new(value, 0.0, 0.0, 0.0)
    }

    pub const fn emitter(value: f64) -> Self {
        Self::new(value, 1.0, 0.0, 0.0)
    }

    pub const fn collector(value: f64) -> Self {
        Self::new(value, 0.0, 1.0, 0.0)
    }

    pub const fn charge(value: f64) -> Self {
        Self::new(value, 0.0, 0.0, 1.0)
    }

    pub fn exp(self) -> Self {
        let value = self.value.exp();
        self.scaled(value).with_value(value)
    }

    pub fn exp_m1(self) -> Self {
        let value = self.value.exp_m1();
        self.scaled(self.value.exp()).with_value(value)
    }

    pub fn ln(self) -> Self {
        self.scaled(1.0 / self.value).with_value(self.value.ln())
    }

    pub fn ln_1p(self) -> Self {
        self.scaled(1.0 / (1.0 + self.value))
            .with_value(self.value.ln_1p())
    }

    pub fn sqrt(self) -> Self {
        let value = self.value.sqrt();
        self.scaled(0.5 / value).with_value(value)
    }

    pub fn powf(self, exponent: f64) -> Self {
        let value = self.value.powf(exponent);
        self.scaled(exponent * self.value.powf(exponent - 1.0))
            .with_value(value)
    }

    pub const fn with_value(self, value: f64) -> Self {
        Self { value, ..self }
    }

    pub fn scaled(self, scale: f64) -> Self {
        Self {
            value: self.value * scale,
            be: self.be * scale,
            bc: self.bc * scale,
            hole: self.hole * scale,
            temperature: self.temperature * scale,
        }
    }

    pub fn is_finite(self) -> bool {
        [self.value, self.be, self.bc, self.hole, self.temperature]
            .iter()
            .all(|x| x.is_finite())
    }
}

impl Add for Jet {
    type Output = Self;

    fn add(self, rhs: Self) -> Self {
        Self::new(
            self.value + rhs.value,
            self.be + rhs.be,
            self.bc + rhs.bc,
            self.hole + rhs.hole,
        )
        .with_temperature(self.temperature + rhs.temperature)
    }
}

impl Add<f64> for Jet {
    type Output = Self;

    fn add(self, rhs: f64) -> Self {
        self.with_value(self.value + rhs)
    }
}

impl Sub for Jet {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self {
        Self::new(
            self.value - rhs.value,
            self.be - rhs.be,
            self.bc - rhs.bc,
            self.hole - rhs.hole,
        )
        .with_temperature(self.temperature - rhs.temperature)
    }
}

impl Sub<f64> for Jet {
    type Output = Self;

    fn sub(self, rhs: f64) -> Self {
        self.with_value(self.value - rhs)
    }
}

impl Mul for Jet {
    type Output = Self;

    fn mul(self, rhs: Self) -> Self {
        Self::new(
            self.value * rhs.value,
            self.be.mul_add(rhs.value, self.value * rhs.be),
            self.bc.mul_add(rhs.value, self.value * rhs.bc),
            self.hole.mul_add(rhs.value, self.value * rhs.hole),
        )
        .with_temperature(
            self.temperature
                .mul_add(rhs.value, self.value * rhs.temperature),
        )
    }
}

impl Mul<f64> for Jet {
    type Output = Self;

    fn mul(self, rhs: f64) -> Self {
        self.scaled(rhs)
    }
}

impl Div for Jet {
    type Output = Self;

    fn div(self, rhs: Self) -> Self {
        let reciprocal = 1.0 / rhs.value;
        let value = self.value * reciprocal;
        let differential_scale = value * reciprocal;
        Self::new(
            value,
            differential_scale.mul_add(-rhs.be, reciprocal * self.be),
            differential_scale.mul_add(-rhs.bc, reciprocal * self.bc),
            differential_scale.mul_add(-rhs.hole, reciprocal * self.hole),
        )
        .with_temperature(
            differential_scale.mul_add(-rhs.temperature, reciprocal * self.temperature),
        )
    }
}

impl Div<f64> for Jet {
    type Output = Self;

    fn div(self, rhs: f64) -> Self {
        self.scaled(1.0 / rhs)
    }
}

impl Neg for Jet {
    type Output = Self;

    fn neg(self) -> Self {
        self.scaled(-1.0)
    }
}

/// Scalar operations shared by first- and second-order source charge AD.
/// The physical HICQFF/HICQFC equations are generic over derivative
/// order, so mixed Tf/voltage derivatives cannot drift from the model.
pub(super) trait Scalar:
    Copy
    + Add<Output = Self>
    + Add<f64, Output = Self>
    + Sub<Output = Self>
    + Sub<f64, Output = Self>
    + Mul<Output = Self>
    + Mul<f64, Output = Self>
    + Div<Output = Self>
    + Div<f64, Output = Self>
    + Neg<Output = Self>
{
    fn constant(value: f64) -> Self;
    /// Source-coefficient temperature lane; ignored by the
    /// independent-current second-order voltage-derivative scalar.
    fn temperature_constant(value: f64, slope: f64) -> Self;
    fn scalar(self) -> f64;
    fn exp(self) -> Self;
    fn exp_m1(self) -> Self;
    fn ln(self) -> Self;
    fn ln_1p(self) -> Self;
    fn sqrt(self) -> Self;
    fn powf(self, exponent: f64) -> Self;
    fn is_finite(self) -> bool;
}

impl Scalar for Jet {
    fn constant(value: f64) -> Self {
        Self::constant(value)
    }
    fn temperature_constant(value: f64, slope: f64) -> Self {
        Self::thermal_constant(value, slope)
    }
    fn scalar(self) -> f64 {
        self.value
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
    fn sqrt(self) -> Self {
        self.sqrt()
    }
    fn powf(self, exponent: f64) -> Self {
        self.powf(exponent)
    }
    fn is_finite(self) -> bool {
        self.is_finite()
    }
}

/// Second-order forward-mode AD, with mixed ∂²/∂Itf∂V(BE/BC) lanes.
///
/// The Itf lane is the independent forward-current variable used to
/// define HICUM's transit time Tf=dQf/dItf. The two voltage lanes also
/// include the actual operating-point dependence of Itf, Ick and Tf0.
/// Mixed lanes provide the full dTf/dV without finite differences.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Mixed {
    pub value: f64,
    pub be: f64,
    pub bc: f64,
    pub hole: f64,
    pub d_hole_be: f64,
    pub d_hole_bc: f64,
    pub temperature: f64,
    pub d_hole_temperature: f64,
}

impl Mixed {
    pub const fn new(value: f64, be: f64, bc: f64, hole: f64) -> Self {
        Self {
            value,
            be,
            bc,
            hole,
            d_hole_be: 0.0,
            d_hole_bc: 0.0,
            temperature: 0.0,
            d_hole_temperature: 0.0,
        }
    }

    pub const fn with_temperature(self, slope: f64) -> Self {
        Self {
            temperature: slope,
            ..self
        }
    }

    fn unary(self, value: f64, first: f64, second: f64) -> Self {
        Self {
            value,
            be: first * self.be,
            bc: first * self.bc,
            hole: first * self.hole,
            d_hole_be: first.mul_add(self.d_hole_be, second * self.hole * self.be),
            d_hole_bc: first.mul_add(self.d_hole_bc, second * self.hole * self.bc),
            temperature: first * self.temperature,
            d_hole_temperature: first.mul_add(
                self.d_hole_temperature,
                second * self.hole * self.temperature,
            ),
        }
    }

    pub fn is_finite(self) -> bool {
        [
            self.value,
            self.be,
            self.bc,
            self.hole,
            self.d_hole_be,
            self.d_hole_bc,
            self.temperature,
            self.d_hole_temperature,
        ]
        .iter()
        .all(|x| x.is_finite())
    }
}

impl Scalar for Mixed {
    fn constant(value: f64) -> Self {
        Self::new(value, 0.0, 0.0, 0.0)
    }
    fn temperature_constant(value: f64, slope: f64) -> Self {
        Self::new(value, 0.0, 0.0, 0.0).with_temperature(slope)
    }
    fn scalar(self) -> f64 {
        self.value
    }
    fn exp(self) -> Self {
        let value = self.value.exp();
        self.unary(value, value, value)
    }
    fn exp_m1(self) -> Self {
        let slope = self.value.exp();
        self.unary(self.value.exp_m1(), slope, slope)
    }
    fn ln(self) -> Self {
        let inv = 1.0 / self.value;
        self.unary(self.value.ln(), inv, -inv * inv)
    }
    fn ln_1p(self) -> Self {
        let inv = 1.0 / (1.0 + self.value);
        self.unary(self.value.ln_1p(), inv, -inv * inv)
    }
    fn sqrt(self) -> Self {
        let value = self.value.sqrt();
        let first = 0.5 / value;
        self.unary(value, first, -first * first / value)
    }
    fn powf(self, exponent: f64) -> Self {
        let value = self.value.powf(exponent);
        let first = exponent * self.value.powf(exponent - 1.0);
        let second = exponent * (exponent - 1.0) * self.value.powf(exponent - 2.0);
        self.unary(value, first, second)
    }
    fn is_finite(self) -> bool {
        self.is_finite()
    }
}

impl Add for Mixed {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        Self {
            value: self.value + rhs.value,
            be: self.be + rhs.be,
            bc: self.bc + rhs.bc,
            hole: self.hole + rhs.hole,
            d_hole_be: self.d_hole_be + rhs.d_hole_be,
            d_hole_bc: self.d_hole_bc + rhs.d_hole_bc,
            temperature: self.temperature + rhs.temperature,
            d_hole_temperature: self.d_hole_temperature + rhs.d_hole_temperature,
        }
    }
}

impl Add<f64> for Mixed {
    type Output = Self;
    fn add(self, rhs: f64) -> Self {
        Self {
            value: self.value + rhs,
            ..self
        }
    }
}

impl Neg for Mixed {
    type Output = Self;
    fn neg(self) -> Self {
        Self {
            value: -self.value,
            be: -self.be,
            bc: -self.bc,
            hole: -self.hole,
            d_hole_be: -self.d_hole_be,
            d_hole_bc: -self.d_hole_bc,
            temperature: -self.temperature,
            d_hole_temperature: -self.d_hole_temperature,
        }
    }
}

impl Sub for Mixed {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        self + (-rhs)
    }
}

impl Sub<f64> for Mixed {
    type Output = Self;
    fn sub(self, rhs: f64) -> Self {
        Self {
            value: self.value - rhs,
            ..self
        }
    }
}

impl Mul for Mixed {
    type Output = Self;
    fn mul(self, rhs: Self) -> Self {
        Self {
            value: self.value * rhs.value,
            be: self.be.mul_add(rhs.value, self.value * rhs.be),
            bc: self.bc.mul_add(rhs.value, self.value * rhs.bc),
            hole: self.hole.mul_add(rhs.value, self.value * rhs.hole),
            d_hole_be: self.d_hole_be.mul_add(
                rhs.value,
                self.value
                    .mul_add(rhs.d_hole_be, self.hole.mul_add(rhs.be, self.be * rhs.hole)),
            ),
            d_hole_bc: self.d_hole_bc.mul_add(
                rhs.value,
                self.value
                    .mul_add(rhs.d_hole_bc, self.hole.mul_add(rhs.bc, self.bc * rhs.hole)),
            ),
            temperature: self
                .temperature
                .mul_add(rhs.value, self.value * rhs.temperature),
            d_hole_temperature: self.d_hole_temperature.mul_add(
                rhs.value,
                self.value.mul_add(
                    rhs.d_hole_temperature,
                    self.hole
                        .mul_add(rhs.temperature, self.temperature * rhs.hole),
                ),
            ),
        }
    }
}

impl Mul<f64> for Mixed {
    type Output = Self;
    fn mul(self, rhs: f64) -> Self {
        Self {
            value: self.value * rhs,
            be: self.be * rhs,
            bc: self.bc * rhs,
            hole: self.hole * rhs,
            d_hole_be: self.d_hole_be * rhs,
            d_hole_bc: self.d_hole_bc * rhs,
            temperature: self.temperature * rhs,
            d_hole_temperature: self.d_hole_temperature * rhs,
        }
    }
}

impl Div for Mixed {
    type Output = Self;
    fn div(self, rhs: Self) -> Self {
        let inv = 1.0 / rhs.value;
        let reciprocal = rhs.unary(inv, -inv * inv, 2.0 * inv * inv * inv);
        self * reciprocal
    }
}

impl Div<f64> for Mixed {
    type Output = Self;
    fn div(self, rhs: f64) -> Self {
        self * (1.0 / rhs)
    }
}
