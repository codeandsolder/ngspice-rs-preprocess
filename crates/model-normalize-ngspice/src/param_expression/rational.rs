//! Structural rational algebra over the Laplace variable. Coefficients are
//! ordinary lexical numparam expressions; neither parameters nor device
//! voltages are sampled during polynomial extraction.
use super::{Expr, SpiceExpression, parse_spice_constant};
use anyhow::{Result, bail, ensure};

// Bound expansion of hostile input, independently of the simulator's state
// dimension. This is an expression-expansion resource limit, not a fit order.
const MAX_DEGREE: usize = 256;
const MAX_COEFFICIENT_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Scalar(String);

impl Scalar {
    fn number(value: f64) -> Self {
        Self(format!("{value:e}"))
    }

    fn from_expr(expr: &Expr) -> Result<Self> {
        Ok(match expr.known_value()? {
            Some(value) => Self::number(value),
            None => Self(scalar_text(expr)),
        })
    }

    fn zero() -> Self {
        Self::number(0.)
    }

    fn one() -> Self {
        Self::number(1.)
    }

    fn value(&self) -> Option<f64> {
        parse_spice_constant(&self.0).ok()
    }

    fn is_zero(&self) -> bool {
        self.value().is_some_and(|value| value == 0.)
    }

    fn binary(&self, op: &str, rhs: &Self) -> Result<Self> {
        if let (Some(left), Some(right)) = (self.value(), rhs.value()) {
            let value = match op {
                "+" => left + right,
                "-" => left - right,
                "*" => left * right,
                _ => bail!("invalid polynomial operation"),
            };
            if value.is_finite() {
                return Ok(Self::number(value));
            }
        }
        match op {
            "+" if self.is_zero() => return Ok(rhs.clone()),
            "+" | "-" if rhs.is_zero() => return Ok(self.clone()),
            "-" if self == rhs => return Ok(Self::zero()),
            "*" if self.is_zero() || rhs.is_zero() => return Ok(Self::zero()),
            "*" if self
                .value()
                .is_some_and(|value| value.to_bits() == 1_f64.to_bits()) =>
            {
                return Ok(rhs.clone());
            }
            "*" if rhs
                .value()
                .is_some_and(|value| value.to_bits() == 1_f64.to_bits()) =>
            {
                return Ok(self.clone());
            }
            _ => {}
        }
        ensure!(
            self.0.len() + rhs.0.len() + 5 <= MAX_COEFFICIENT_BYTES,
            "Laplace coefficient expansion exceeds resource limit"
        );
        Ok(Self(format!("({}{op}{})", self.0, rhs.0)))
    }
}

fn contains_s(expr: &Expr) -> bool {
    match expr {
        Expr::Number(_) => false,
        Expr::Symbol(name) => name == "s",
        Expr::Unary(_, value) => contains_s(value),
        Expr::Binary(_, left, right) => contains_s(left) || contains_s(right),
        Expr::Conditional(condition, yes, no) => {
            contains_s(condition) || contains_s(yes) || contains_s(no)
        }
        Expr::Call(_, args) => args.iter().any(contains_s),
    }
}

fn scalar_text(expr: &Expr) -> String {
    match expr {
        Expr::Number(value) => format!("{value:e}"),
        Expr::Symbol(name) => name.clone(),
        Expr::Unary(op, value) => format!("({op}{})", scalar_text(value)),
        Expr::Binary(op, left, right) => {
            format!("({}{op}{})", scalar_text(left), scalar_text(right))
        }
        Expr::Conditional(condition, yes, no) => format!(
            "({}?{}:{})",
            scalar_text(condition),
            scalar_text(yes),
            scalar_text(no)
        ),
        Expr::Call(name, args) => {
            let args = args.iter().map(scalar_text).collect::<Vec<_>>().join(",");
            format!("{name}({args})")
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Polynomial(Vec<Scalar>); // ascending powers internally

impl Polynomial {
    fn constant(value: Scalar) -> Self {
        Self(vec![value])
    }

    fn one() -> Self {
        Self::constant(Scalar::one())
    }

    fn is_zero(&self) -> bool {
        self.0.len() == 1 && self.0[0].is_zero()
    }

    fn trim(mut coefficients: Vec<Scalar>) -> Self {
        while coefficients.len() > 1 && coefficients.last().is_some_and(Scalar::is_zero) {
            coefficients.pop();
        }
        Self(coefficients)
    }

    fn add(&self, op: &str, rhs: &Self) -> Result<Self> {
        let zero = Scalar::zero();
        let coefficients = (0..self.0.len().max(rhs.0.len()))
            .map(|i| {
                self.0
                    .get(i)
                    .unwrap_or(&zero)
                    .binary(op, rhs.0.get(i).unwrap_or(&zero))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self::trim(coefficients))
    }

    fn multiply(&self, rhs: &Self) -> Result<Self> {
        if self.is_zero() || rhs.is_zero() {
            return Ok(Self::constant(Scalar::zero()));
        }
        let count = self.0.len() + rhs.0.len() - 1;
        ensure!(
            count <= MAX_DEGREE + 1,
            "Laplace polynomial degree exceeds resource limit"
        );
        let mut coefficients = vec![Scalar::zero(); count];
        for (i, left) in self.0.iter().enumerate() {
            for (j, right) in rhs.0.iter().enumerate() {
                coefficients[i + j] = coefficients[i + j].binary("+", &left.binary("*", right)?)?;
            }
        }
        Ok(Self::trim(coefficients))
    }

    fn descending(self) -> Vec<String> {
        self.0.into_iter().rev().map(|value| value.0).collect()
    }
}

struct Rational {
    numerator: Polynomial,
    denominator: Polynomial,
}

impl Rational {
    fn constant(value: Scalar) -> Self {
        Self {
            numerator: Polynomial::constant(value),
            denominator: Polynomial::one(),
        }
    }

    fn multiply(&self, rhs: &Self) -> Result<Self> {
        Ok(Self {
            numerator: self.numerator.multiply(&rhs.numerator)?,
            denominator: self.denominator.multiply(&rhs.denominator)?,
        })
    }

    fn reciprocal(self) -> Result<Self> {
        ensure!(!self.numerator.is_zero(), "zero Laplace denominator");
        Ok(Self {
            numerator: self.denominator,
            denominator: self.numerator,
        })
    }

    fn add(&self, op: &str, rhs: &Self) -> Result<Self> {
        if self.denominator == rhs.denominator {
            return Ok(Self {
                numerator: self.numerator.add(op, &rhs.numerator)?,
                denominator: self.denominator.clone(),
            });
        }
        Ok(Self {
            numerator: self
                .numerator
                .multiply(&rhs.denominator)?
                .add(op, &rhs.numerator.multiply(&self.denominator)?)?,
            denominator: self.denominator.multiply(&rhs.denominator)?,
        })
    }

    fn power(mut self, exponent: &Expr) -> Result<Self> {
        let value = exponent.evaluate(&mut |name: &str| {
            bail!("Laplace power requires a constant integer exponent, not {name}")
        })?;
        ensure!(
            value.fract() == 0. && value.abs() <= 256.,
            "Laplace power requires an integer exponent within the resource limit"
        );
        if value < 0. {
            self = self.reciprocal()?;
        }
        // The range and integrality are checked above.
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "integer range and sign checked above"
        )]
        let mut exponent = value.abs() as usize;
        let mut result = Self::constant(Scalar::one());
        while exponent != 0 {
            if exponent & 1 != 0 {
                result = result.multiply(&self)?;
            }
            exponent >>= 1;
            if exponent != 0 {
                self = self.multiply(&self)?;
            }
        }
        Ok(result)
    }

    fn compile(expr: &Expr) -> Result<Self> {
        if !contains_s(expr) {
            return Ok(Self::constant(Scalar::from_expr(expr)?));
        }
        match expr {
            Expr::Symbol(_) => Ok(Self {
                numerator: Polynomial(vec![Scalar::zero(), Scalar::one()]),
                denominator: Polynomial::one(),
            }),
            Expr::Unary("+", value) => Self::compile(value),
            Expr::Unary("-", value) => {
                Self::constant(Scalar::zero()).add("-", &Self::compile(value)?)
            }
            Expr::Binary(op @ ("+" | "-"), left, right) => {
                Self::compile(left)?.add(op, &Self::compile(right)?)
            }
            Expr::Binary("*", left, right) => Self::compile(left)?.multiply(&Self::compile(right)?),
            Expr::Binary("/", left, right) => {
                Self::compile(left)?.multiply(&Self::compile(right)?.reciprocal()?)
            }
            Expr::Binary("**", base, exponent) => Self::compile(base)?.power(exponent),
            _ => bail!("nonrational Laplace expression (unsupported operation on s)"),
        }
    }
}

/// Extract descending numerator and denominator coefficient expressions from
/// a proper rational transfer. The scalar subexpressions retain lexical
/// parameter names and are bound by the native model at occurrence preparation.
pub(crate) fn coefficients(raw: &str) -> Result<(Vec<String>, Vec<String>)> {
    let expression = SpiceExpression::parse(raw)?;
    let result = Rational::compile(&expression.0)?;
    ensure!(!result.denominator.is_zero(), "zero Laplace denominator");
    ensure!(
        result.numerator.0.len() <= result.denominator.0.len(),
        "improper Laplace transfer requires explicit derivative operators"
    );
    Ok((
        result.numerator.descending(),
        result.denominator.descending(),
    ))
}
