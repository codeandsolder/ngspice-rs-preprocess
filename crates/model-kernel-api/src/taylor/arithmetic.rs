//! Fixed-capacity normalized Taylor arithmetic and the scalar constitutive
//! abstraction.
//!
//! Constant operands broadcast without allocating or carrying
//! a separate order context. Nonregular operations propagate nonfinite values
//! that the physical output contract rejects.
use super::MAX_ORDER;
use crate::{Error, Result};
use std::{
    cmp::Ordering,
    fmt::Debug,
    ops::{Add, Div, Mul, Neg, Sub},
};

/// Scalar elementary operations shared by point and Taylor constitutive laws.
pub trait Scalar:
    Copy
    + Debug
    + Default
    + Add<Output = Self>
    + Sub<Output = Self>
    + Mul<Output = Self>
    + Div<Output = Self>
    + Neg<Output = Self>
{
    fn constant(value: f64) -> Self;
    fn value(self) -> f64;
    fn compare(self, boundary: f64) -> Option<Ordering>;
    fn is_zero(self) -> bool;
    fn is_finite(self) -> bool;
    #[must_use = "scalar operations return a new value"]
    fn exp(self) -> Self;
    #[must_use = "scalar operations return a new value"]
    fn exp_m1(self) -> Self;
    #[must_use = "scalar operations return a new value"]
    fn ln(self) -> Self;
    #[must_use = "scalar operations return a new value"]
    fn ln_1p(self) -> Self;
    #[must_use = "scalar operations return a new value"]
    fn sqrt(self) -> Self;
    #[must_use = "scalar operations return a new value"]
    fn powf(self, exponent: f64) -> Self;
    #[must_use = "scalar operations return a new value"]
    fn mul_add(self, multiplier: Self, addend: Self) -> Self;
    #[must_use = "scalar operations return a new value"]
    fn abs(self) -> Self;
    #[must_use = "scalar operations return a new value"]
    fn tanh(self) -> Self;
    #[must_use = "scalar operations return a new value"]
    fn atan(self) -> Self;
    /// Lift an exponential whose scaled constant term is already evaluated.
    /// Useful when the unscaled exp(x) would overflow in higher coefficients.
    #[must_use = "scalar operations return a new value"]
    fn exp_with_value(self, value: f64) -> Self;
    /// Regular continuation of expm1(x)/x through zero.
    #[must_use = "scalar operations return a new value"]
    fn exprel(self) -> Self;
    /// Derivative of the same regular exponential quotient.
    #[must_use = "scalar operations return a new value"]
    fn exprel_slope(self) -> Self;
    fn degree(self) -> usize;
    /// Capture the exact normalized germ without re-evaluating the law.
    fn into_series(self) -> Series;
    #[must_use = "scalar operations return a new value"]
    fn with_value(self, value: f64) -> Self;

    fn compare_to(self, rhs: Self) -> Option<Ordering> {
        let ordering = self.value().partial_cmp(&rhs.value())?;
        if ordering == Ordering::Equal {
            (self - rhs).compare(0.)
        } else {
            Some(ordering)
        }
    }

    #[must_use = "scalar operations return a new value"]
    fn scale(self, factor: f64) -> Self {
        self * Self::constant(factor)
    }
    #[must_use = "scalar operations return a new value"]
    fn offset(self, value: f64) -> Self {
        self + Self::constant(value)
    }
    #[must_use = "scalar operations return a new value"]
    fn recip(self) -> Self {
        Self::constant(1.) / self
    }
}

// Integral of exp(value*t)*t^(moment-1) on [0,1] near zero.
// |value| <= 1/8: 12 terms bound the omitted tail below binary64 rounding
// even at the largest requested moment. No subtraction of nearly equal
// exponentials and no division by a vanishing germ are needed.
const MOMENT_LIMIT: f64 = 0.125;
fn small_exp_moment(value: f64, moment: u32) -> f64 {
    let mut term = 1.;
    let mut sum = f64::from(moment).recip();
    for n in 1..=12_u32 {
        term *= value / f64::from(n);
        let addition = term / f64::from(n + moment);
        sum += addition;
        if addition.abs() <= f64::EPSILON * sum.abs() {
            break;
        }
    }
    sum
}

impl Scalar for f64 {
    fn constant(value: f64) -> Self {
        value
    }
    fn value(self) -> f64 {
        self
    }
    fn compare(self, boundary: f64) -> Option<Ordering> {
        self.partial_cmp(&boundary)
    }
    fn is_zero(self) -> bool {
        self == 0.
    }
    fn is_finite(self) -> bool {
        self.is_finite()
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
    fn mul_add(self, multiplier: Self, addend: Self) -> Self {
        self.mul_add(multiplier, addend)
    }
    fn abs(self) -> Self {
        self.abs()
    }
    fn tanh(self) -> Self {
        self.tanh()
    }
    fn atan(self) -> Self {
        self.atan()
    }
    fn exp_with_value(self, value: f64) -> Self {
        value
    }
    fn exprel(self) -> Self {
        if self == 0. { 1. } else { self.exp_m1() / self }
    }
    fn exprel_slope(self) -> Self {
        if self.abs() <= MOMENT_LIMIT {
            small_exp_moment(self, 2)
        } else {
            (self.exp() - self.exprel()) / self
        }
    }
    fn degree(self) -> usize {
        0
    }
    fn into_series(self) -> Series {
        Series::constant_value(self)
    }
    fn with_value(self, value: f64) -> Self {
        value
    }
}

/// Bounded, allocation-free local series. The inactive suffix is always zero.
#[derive(Clone, Copy, Debug)]
pub struct Series {
    coefficients: [f64; MAX_ORDER + 1],
    order: usize,
}

impl Default for Series {
    fn default() -> Self {
        Self::constant_value(0.)
    }
}

impl Series {
    const fn zero(order: usize) -> Self {
        Self {
            coefficients: [0.; MAX_ORDER + 1],
            order,
        }
    }

    /// # Errors
    /// Empty, excessive or nonfinite coefficient sequence.
    pub fn from_coefficients(coefficients: impl IntoIterator<Item = f64>) -> Result<Self> {
        let mut result = Self::zero(0);
        let mut count = 0;
        for coefficient in coefficients {
            if count > MAX_ORDER {
                return Err(Error::Shape("Taylor coefficient order"));
            }
            if !coefficient.is_finite() {
                return Err(Error::Domain("nonfinite Taylor coefficient".into()));
            }
            result.coefficients[count] = coefficient;
            count += 1;
        }
        if count == 0 {
            return Err(Error::Shape("empty Taylor germ"));
        }
        result.order = count - 1;
        Ok(result)
    }

    /// Access a coefficient; constants and lower-degree operands broadcast zero.
    #[must_use = "the coefficient contributes to physical Taylor assembly"]
    pub fn coefficient(&self, degree: usize) -> f64 {
        self.coefficients.get(degree).copied().unwrap_or(0.)
    }

    #[must_use = "the active order bounds coefficient operations"]
    pub const fn order(&self) -> usize {
        self.order
    }

    fn active(&self) -> &[f64] {
        &self.coefficients[..=self.order]
    }

    const fn constant_value(value: f64) -> Self {
        let mut series = Self::zero(0);
        series.coefficients[0] = value;
        series
    }

    const fn invalid(order: usize) -> Self {
        let mut result = Self::zero(order);
        result.coefficients[0] = f64::NAN;
        result
    }

    fn is_constant(&self) -> bool {
        self.active()[1..].iter().all(|v| *v == 0.)
    }

    fn exponential(self, value: f64) -> Self {
        let mut result = Self::zero(self.order);
        result.coefficients[0] = value;
        for n in 1..=self.order {
            let mut k = 0.;
            let sum = self.coefficients[1..=n]
                .iter()
                .zip(result.coefficients[..n].iter().rev())
                .fold(0., |sum, (&x, &y)| {
                    k += 1.;
                    (k * x).mul_add(y, sum)
                });
            result.coefficients[n] = sum / k;
        }
        result
    }

    fn compose_derivative(self, value: f64, slope: Self) -> Self {
        let mut result = Self::zero(self.order);
        result.coefficients[0] = value;
        for n in 1..=self.order {
            let mut k = 0.;
            let sum = self.coefficients[1..=n]
                .iter()
                .zip(slope.coefficients[..n].iter().rev())
                .fold(0., |sum, (&x, &y)| {
                    k += 1.;
                    (k * x).mul_add(y, sum)
                });
            result.coefficients[n] = sum / k;
        }
        result
    }
    fn small_exp_moment(self, moment: u32) -> Self {
        // Coefficient k of the shifted entire function equals the integral
        // of exp(x0*t)*t^(k+moment-1) / k!. Compose with a zero-constant
        // germ; retaining exactly the requested polynomial order is enough.
        let mut coefficients = [0.; MAX_ORDER + 1];
        let mut reciprocal_factorial = 1.;
        for (k, coefficient) in (0_u32..).zip(&mut coefficients[..=self.order]) {
            if k != 0 {
                reciprocal_factorial /= f64::from(k);
            }
            *coefficient = small_exp_moment(self.value(), moment + k) * reciprocal_factorial;
        }
        let delta = self.with_value(0.);
        coefficients[..=self.order]
            .iter()
            .rev()
            .fold(Self::constant(0.), |sum, &coefficient| {
                sum * delta + Self::constant(coefficient)
            })
    }
}

impl Add for Series {
    type Output = Self;
    fn add(mut self, rhs: Self) -> Self {
        self.order = self.order.max(rhs.order);
        for (a, b) in self.coefficients[..=self.order]
            .iter_mut()
            .zip(rhs.coefficients)
        {
            *a += b;
        }
        self
    }
}
impl Neg for Series {
    type Output = Self;
    fn neg(mut self) -> Self {
        for a in &mut self.coefficients[..=self.order] {
            *a = -*a;
        }
        self
    }
}
impl Sub for Series {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        self + (-rhs)
    }
}
impl Mul for Series {
    type Output = Self;
    fn mul(self, rhs: Self) -> Self {
        if self.order == 0 {
            return rhs.scale(self.value());
        }
        if rhs.order == 0 {
            return self.scale(rhs.value());
        }
        let order = self.order.max(rhs.order);
        let mut result = Self::zero(order);
        result.coefficients[0] = self.value() * rhs.value();
        for n in 1..=order {
            result.coefficients[n] = self.coefficients[..=n]
                .iter()
                .zip(rhs.coefficients[..=n].iter().rev())
                .fold(0., |sum, (&a, &b)| a.mul_add(b, sum));
        }
        result
    }
}
impl Div for Series {
    type Output = Self;
    fn div(self, rhs: Self) -> Self {
        let order = self.order.max(rhs.order);
        if rhs.value() == 0. {
            return Self::invalid(order);
        }
        if rhs.order == 0 {
            let mut result = self;
            for coefficient in &mut result.coefficients[..=result.order] {
                *coefficient /= rhs.value();
            }
            return result;
        }
        let mut result = Self::zero(order);
        for n in 0..=order {
            let remainder = rhs.coefficients[1..=n]
                .iter()
                .zip(result.coefficients[..n].iter().rev())
                .fold(self.coefficients[n], |sum, (&a, &b)| (-a).mul_add(b, sum));
            result.coefficients[n] = remainder / rhs.value();
        }
        result
    }
}

impl Scalar for Series {
    fn constant(value: f64) -> Self {
        Self::constant_value(value)
    }
    fn value(self) -> f64 {
        self.coefficients[0]
    }
    fn compare(self, boundary: f64) -> Option<Ordering> {
        let cmp = self.value().partial_cmp(&boundary)?;
        if cmp != Ordering::Equal {
            return Some(cmp);
        }
        self.active()[1..]
            .iter()
            .find(|v| **v != 0.)
            .map_or(Some(Ordering::Equal), |v| v.partial_cmp(&0.))
    }
    fn is_zero(self) -> bool {
        self.active().iter().all(|v| *v == 0.)
    }
    fn is_finite(self) -> bool {
        self.active().iter().all(|v| v.is_finite())
    }
    fn exp(self) -> Self {
        self.exponential(self.value().exp())
    }
    fn exp_m1(self) -> Self {
        let mut result = self.exp();
        result.coefficients[0] = self.value().exp_m1();
        result
    }
    fn ln(self) -> Self {
        if self.value() <= 0. {
            return Self::invalid(self.order);
        }
        self.compose_derivative(self.value().ln(), self.recip())
    }
    fn ln_1p(self) -> Self {
        if self.value() <= -1. {
            return Self::invalid(self.order);
        }
        self.compose_derivative(self.value().ln_1p(), self.offset(1.).recip())
    }
    fn sqrt(self) -> Self {
        if self.value() == 0. && self.is_constant() {
            return self;
        }
        if self.value() <= 0. {
            return Self::invalid(self.order);
        }
        let root = self.value().sqrt();
        let mut result = Self::zero(self.order);
        result.coefficients[0] = root;
        for n in 1..=self.order {
            let remainder = result.coefficients[1..n]
                .iter()
                .zip(result.coefficients[1..n].iter().rev())
                .fold(self.coefficients[n], |sum, (&a, &b)| (-a).mul_add(b, sum));
            result.coefficients[n] = (remainder / root) * 0.5;
        }
        result
    }
    fn powf(self, exponent: f64) -> Self {
        // Integer powers at zero are regular. Other nonpositive-base germs
        // are not generically analytic; reject rather than invent a series.
        if exponent == 0. {
            return Self::constant(1.);
        }
        if self.is_constant() {
            return Self::constant(self.value().powf(exponent));
        }
        if self.value() > 0. {
            let mut exponent_log = self.ln().scale(exponent);
            let value = self.value().powf(exponent);
            exponent_log.coefficients[0] = 0.;
            return exponent_log.exponential(value);
        }
        if exponent.fract() != 0. || !exponent.is_finite() {
            return Self::invalid(self.order);
        }
        if self.value() < 0. {
            let magnitude = (-self).powf(exponent);
            return if exponent.rem_euclid(2.) == 0. {
                magnitude
            } else {
                -magnitude
            };
        }
        // A nonconstant germ at zero has valuation at least one. Integer
        // powers above the retained order vanish; negative powers are poles.
        if exponent < 0. {
            return Self::invalid(self.order);
        }
        let Some(mut remaining) = (1_u32..)
            .take(self.order)
            .find(|&n| f64::from(n).to_bits() == exponent.to_bits())
        else {
            return Self::zero(self.order);
        };
        let mut base = self;
        let mut result = Self::constant(1.);
        while remaining != 0 {
            if remaining & 1 != 0 {
                result = result * base;
            }
            remaining >>= 1;
            if remaining != 0 {
                base = base * base;
            }
        }
        result
    }
    fn mul_add(self, multiplier: Self, addend: Self) -> Self {
        let order = self.order.max(multiplier.order).max(addend.order);
        let mut result = Self::zero(order);
        if self.order == 0 || multiplier.order == 0 {
            let (constant, series) = if self.order == 0 {
                (self.value(), multiplier)
            } else {
                (multiplier.value(), self)
            };
            for (dst, (&value, &offset)) in result.coefficients[..=order]
                .iter_mut()
                .zip(series.coefficients.iter().zip(&addend.coefficients))
            {
                *dst = constant.mul_add(value, offset);
            }
            return result;
        }
        for n in 0..=order {
            result.coefficients[n] = self.coefficients[..=n]
                .iter()
                .zip(multiplier.coefficients[..=n].iter().rev())
                .fold(addend.coefficients[n], |sum, (&a, &b)| a.mul_add(b, sum));
        }
        result
    }
    fn abs(self) -> Self {
        if self.compare(0.) == Some(Ordering::Less) {
            -self
        } else {
            self
        }
    }
    fn scale(mut self, factor: f64) -> Self {
        for coefficient in &mut self.coefficients[..=self.order] {
            *coefficient *= factor;
        }
        self
    }
    fn tanh(self) -> Self {
        let tail = self.abs().scale(-2.).exp();
        let correction = tail.scale(2.) / tail.offset(1.);
        let result = if self.compare(0.) == Some(Ordering::Less) {
            correction.offset(-1.)
        } else {
            Self::constant(1.) - correction
        };
        result.with_value(self.value().tanh())
    }
    fn atan(self) -> Self {
        let slope = if self.value().abs() > 1. {
            let inv = self.recip();
            let square = inv * inv;
            square / square.offset(1.)
        } else {
            (self * self).offset(1.).recip()
        };
        self.compose_derivative(self.value().atan(), slope)
    }
    fn exp_with_value(self, value: f64) -> Self {
        self.exponential(value)
    }
    fn exprel(self) -> Self {
        if self.is_constant() {
            return Self::constant(self.value().exprel());
        }
        if self.value().abs() <= MOMENT_LIMIT {
            self.small_exp_moment(1).with_value(self.value().exprel())
        } else {
            self.exp_m1() / self
        }
    }
    fn exprel_slope(self) -> Self {
        if self.is_constant() {
            return Self::constant(self.value().exprel_slope());
        }
        if self.value().abs() <= MOMENT_LIMIT {
            self.small_exp_moment(2)
        } else {
            (self.exp() - self.exprel()) / self
        }
    }
    fn into_series(self) -> Series {
        self
    }
    fn degree(self) -> usize {
        self.order
    }
    fn with_value(mut self, value: f64) -> Self {
        self.coefficients[0] = value;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_order_exp_and_geometric_series() -> Result<()> {
        let x = Series::from_coefficients(
            [0., 1.]
                .into_iter()
                .chain(std::iter::repeat_n(0., MAX_ORDER - 1)),
        )?;
        let e = x.exp();
        let geometric = (Series::constant(1.) - x).recip();
        let mut expected = 1.;
        let mut degree = 0.;
        for k in 0..=MAX_ORDER {
            if k != 0 {
                degree += 1.;
                expected /= degree;
            }
            assert!((e.coefficient(k) / expected - 1.).abs() < 3e-14);
            assert_eq!(geometric.coefficient(k).to_bits(), 1_f64.to_bits());
        }
        Ok(())
    }

    #[test]
    fn regular_exponential_quotient_retains_all_orders() -> Result<()> {
        let x = Series::from_coefficients(
            [0., 1.]
                .into_iter()
                .chain(std::iter::repeat_n(0., MAX_ORDER - 1)),
        )?;
        let y = x.exprel();
        let slope = x.exprel_slope();
        let mut reciprocal_factorial = 1.;
        for (k, degree) in (0..=MAX_ORDER).zip(0_u32..) {
            if degree != 0 {
                reciprocal_factorial /= f64::from(degree);
            }
            let expected = reciprocal_factorial / f64::from(degree + 1);
            let expected_slope = reciprocal_factorial / f64::from(degree + 2);
            assert!((y.coefficient(k) / expected - 1.).abs() < 3e-14);
            assert!((slope.coefficient(k) / expected_slope - 1.).abs() < 3e-14);
        }
        Ok(())
    }

    #[test]
    fn right_germ_and_regular_zero_integer_power() -> Result<()> {
        let x = Series::from_coefficients([0., 0., -2., 1., 0.])?;
        assert_eq!(x.compare(0.), Some(Ordering::Less));
        assert_eq!(x.abs().coefficient(2).to_bits(), 2_f64.to_bits());
        assert_eq!(x.powf(2.).coefficient(4).to_bits(), 4_f64.to_bits());
        assert!(!x.powf(0.5).is_finite());
        assert!(!x.recip().is_finite());
        Ok(())
    }
}
