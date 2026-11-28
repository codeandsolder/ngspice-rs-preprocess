//! Structural rational algebra over the Laplace variable. Coefficients are
//! ordinary lexical numparam expressions; neither parameters nor device
//! voltages are sampled during polynomial extraction.
use super::{Expr, SpiceExpression, parse_spice_constant};
use crate::native_scalar::{BinaryOp, UnaryOp};
use anyhow::{Result, bail, ensure};
use model_preprocess_command::{Resource, WorkBudget};

// Bound expansion of hostile input, independently of the simulator's state
// dimension. This is an expression-expansion resource limit, not a fit order.
pub const MAX_DEGREE: usize = 256;
const MAX_COEFFICIENT_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Scalar(String);

impl Scalar {
    fn number(value: f64, budget: &WorkBudget) -> Result<Self> {
        budget.charge(Resource::EmittedBytes, 32)?;
        Ok(Self(format!("{value:e}")))
    }

    fn from_expr(expr: &Expr, budget: &WorkBudget) -> Result<Self> {
        if let Some(value) = expr.known_value(budget)? {
            return Self::number(value, budget);
        }
        let mut text = String::new();
        scalar_text(expr, &mut text, budget)?;
        Ok(Self(text))
    }

    fn zero(budget: &WorkBudget) -> Result<Self> {
        Self::number(0., budget)
    }

    fn one(budget: &WorkBudget) -> Result<Self> {
        Self::number(1., budget)
    }

    fn copy(&self, budget: &WorkBudget) -> Result<Self> {
        budget.charge(Resource::EmittedBytes, self.0.len())?;
        Ok(self.clone())
    }

    fn value(&self) -> Option<f64> {
        parse_spice_constant(&self.0).ok()
    }

    fn is_zero(&self) -> bool {
        self.value().is_some_and(|value| value == 0.)
    }

    fn binary(&self, op: &str, rhs: &Self, budget: &WorkBudget) -> Result<Self> {
        budget.charge(Resource::EvaluationSteps, 1)?;
        budget.charge(
            Resource::ScannedBytes,
            self.0.len().saturating_add(rhs.0.len()),
        )?;
        if let (Some(left), Some(right)) = (self.value(), rhs.value()) {
            let value = match op {
                "+" => left + right,
                "-" => left - right,
                "*" => left * right,
                "/" => {
                    ensure!(right != 0., "zero Laplace scalar divisor");
                    left / right
                }
                _ => bail!("invalid polynomial operation"),
            };
            ensure!(
                value.is_finite(),
                "non-finite Laplace polynomial coefficient"
            );
            ensure!(
                !matches!(op, "*" | "/") || left == 0. || right == 0. || value != 0.,
                "Laplace polynomial coefficient underflow"
            );
            return Self::number(value, budget);
        }
        match op {
            "+" if self.is_zero() => return rhs.copy(budget),
            "+" | "-" if rhs.is_zero() => return self.copy(budget),
            "-" if self == rhs => return Self::zero(budget),
            "*" if self.is_zero() || rhs.is_zero() => return Self::zero(budget),
            "*" if self
                .value()
                .is_some_and(|value| value.to_bits() == 1_f64.to_bits()) =>
            {
                return rhs.copy(budget);
            }
            "*" if rhs
                .value()
                .is_some_and(|value| value.to_bits() == 1_f64.to_bits()) =>
            {
                return self.copy(budget);
            }
            _ => {}
        }
        ensure!(
            self.0.len() + rhs.0.len() + 5 <= MAX_COEFFICIENT_BYTES,
            "Laplace coefficient expansion exceeds resource limit"
        );
        budget.charge(
            Resource::EmittedBytes,
            self.0
                .len()
                .saturating_add(rhs.0.len())
                .saturating_add(op.len())
                .saturating_add(2),
        )?;
        Ok(Self(format!("({}{op}{})", self.0, rhs.0)))
    }
}

fn contains_s(expr: &Expr, budget: &WorkBudget) -> Result<bool> {
    budget.charge(Resource::EvaluationSteps, 1)?;
    Ok(match expr {
        Expr::Number(_) | Expr::SourceAtom(_) => false,
        Expr::Symbol(name) => name == "s",
        Expr::Unary(_, value) | Expr::ParameterGroup(value) => contains_s(value, budget)?,
        Expr::Binary(_, left, right) | Expr::SourceBinary(_, left, right) => {
            contains_s(left, budget)? || contains_s(right, budget)?
        }
        Expr::Conditional(condition, yes, no) => {
            contains_s(condition, budget)? || contains_s(yes, budget)? || contains_s(no, budget)?
        }
        Expr::Call(_, args) => {
            for arg in args {
                if contains_s(arg, budget)? {
                    return Ok(true);
                }
            }
            false
        }
    })
}

fn append_scalar(text: &mut String, value: &str, budget: &WorkBudget) -> Result<()> {
    ensure!(
        text.len().saturating_add(value.len()) <= MAX_COEFFICIENT_BYTES,
        "Laplace coefficient expansion exceeds resource limit"
    );
    budget.charge(Resource::EmittedBytes, value.len())?;
    text.push_str(value);
    Ok(())
}

fn scalar_text(expr: &Expr, text: &mut String, budget: &WorkBudget) -> Result<()> {
    budget.charge(Resource::EvaluationSteps, 1)?;
    match expr {
        Expr::Number(value) => {
            let scalar = Scalar::number(*value, budget)?;
            append_scalar(text, &scalar.0, budget)?;
        }
        Expr::Symbol(name) => append_scalar(text, name, budget)?,
        Expr::SourceAtom(_) | Expr::ParameterGroup(_) | Expr::SourceBinary(_, _, _) => {
            bail!("source-only expression cannot be a native Laplace coefficient")
        }
        Expr::Unary(op, value) => {
            append_scalar(text, "(", budget)?;
            append_scalar(text, op.symbol(), budget)?;
            scalar_text(value, text, budget)?;
            append_scalar(text, ")", budget)?;
        }
        Expr::Binary(op, left, right) => {
            append_scalar(text, "(", budget)?;
            scalar_text(left, text, budget)?;
            append_scalar(text, op.symbol(), budget)?;
            scalar_text(right, text, budget)?;
            append_scalar(text, ")", budget)?;
        }
        Expr::Conditional(condition, yes, no) => {
            append_scalar(text, "(", budget)?;
            scalar_text(condition, text, budget)?;
            append_scalar(text, "?", budget)?;
            scalar_text(yes, text, budget)?;
            append_scalar(text, ":", budget)?;
            scalar_text(no, text, budget)?;
            append_scalar(text, ")", budget)?;
        }
        Expr::Call(name, args) => {
            append_scalar(text, name, budget)?;
            append_scalar(text, "(", budget)?;
            for (index, arg) in args.iter().enumerate() {
                if index != 0 {
                    append_scalar(text, ",", budget)?;
                }
                scalar_text(arg, text, budget)?;
            }
            append_scalar(text, ")", budget)?;
        }
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Polynomial(Vec<Scalar>); // ascending powers internally

impl Polynomial {
    fn constant(value: Scalar, budget: &WorkBudget) -> Result<Self> {
        budget.charge(Resource::GeneratedNodes, 1)?;
        Ok(Self(vec![value]))
    }

    fn one(budget: &WorkBudget) -> Result<Self> {
        Self::constant(Scalar::one(budget)?, budget)
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

    fn add(&self, op: &str, rhs: &Self, budget: &WorkBudget) -> Result<Self> {
        let zero = Scalar::zero(budget)?;
        budget.charge(Resource::GeneratedNodes, self.0.len().max(rhs.0.len()))?;
        let coefficients = (0..self.0.len().max(rhs.0.len()))
            .map(|i| {
                self.0
                    .get(i)
                    .unwrap_or(&zero)
                    .binary(op, rhs.0.get(i).unwrap_or(&zero), budget)
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self::trim(coefficients))
    }

    fn multiply(&self, rhs: &Self, budget: &WorkBudget) -> Result<Self> {
        if self.is_zero() || rhs.is_zero() {
            return Self::constant(Scalar::zero(budget)?, budget);
        }
        let count = self.0.len() + rhs.0.len() - 1;
        ensure!(
            count <= MAX_DEGREE + 1,
            "Laplace polynomial degree exceeds resource limit"
        );
        budget.charge(Resource::GeneratedNodes, count)?;
        let mut coefficients = (0..count)
            .map(|_| Scalar::zero(budget))
            .collect::<Result<Vec<_>>>()?;
        for (i, left) in self.0.iter().enumerate() {
            for (j, right) in rhs.0.iter().enumerate() {
                coefficients[i + j] =
                    coefficients[i + j].binary("+", &left.binary("*", right, budget)?, budget)?;
            }
        }
        Ok(Self::trim(coefficients))
    }

    fn copy(&self, budget: &WorkBudget) -> Result<Self> {
        budget.charge(Resource::GeneratedNodes, self.0.len())?;
        Ok(Self(
            self.0
                .iter()
                .map(|value| value.copy(budget))
                .collect::<Result<Vec<_>>>()?,
        ))
    }

    fn equal(&self, rhs: &Self, budget: &WorkBudget) -> Result<bool> {
        if self.0.len() != rhs.0.len() {
            return Ok(false);
        }
        for (left, right) in self.0.iter().zip(&rhs.0) {
            budget.charge(Resource::EvaluationSteps, 1)?;
            budget.charge(
                Resource::ScannedBytes,
                left.0.len().saturating_add(right.0.len()),
            )?;
            if left != right {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn descending(self, budget: &WorkBudget) -> Result<Vec<String>> {
        budget.charge(Resource::GeneratedNodes, self.0.len())?;
        Ok(self.0.into_iter().rev().map(|value| value.0).collect())
    }
}

struct Rational {
    numerator: Polynomial,
    denominator: Polynomial,
}

impl Rational {
    fn constant(value: Scalar, budget: &WorkBudget) -> Result<Self> {
        Ok(Self {
            numerator: Polynomial::constant(value, budget)?,
            denominator: Polynomial::one(budget)?,
        })
    }

    fn multiply(&self, rhs: &Self, budget: &WorkBudget) -> Result<Self> {
        Ok(Self {
            numerator: self.numerator.multiply(&rhs.numerator, budget)?,
            denominator: self.denominator.multiply(&rhs.denominator, budget)?,
        })
    }

    fn reciprocal(self) -> Result<Self> {
        ensure!(!self.numerator.is_zero(), "zero Laplace denominator");
        Ok(Self {
            numerator: self.denominator,
            denominator: self.numerator,
        })
    }

    fn add(&self, op: &str, rhs: &Self, budget: &WorkBudget) -> Result<Self> {
        if self.denominator.equal(&rhs.denominator, budget)? {
            return Ok(Self {
                numerator: self.numerator.add(op, &rhs.numerator, budget)?,
                denominator: self.denominator.copy(budget)?,
            });
        }
        Ok(Self {
            numerator: self.numerator.multiply(&rhs.denominator, budget)?.add(
                op,
                &rhs.numerator.multiply(&self.denominator, budget)?,
                budget,
            )?,
            denominator: self.denominator.multiply(&rhs.denominator, budget)?,
        })
    }

    fn power(mut self, exponent: &Expr, budget: &WorkBudget) -> Result<Self> {
        let value = exponent.evaluate(
            &mut |name: &str| {
                bail!("Laplace power requires a constant integer exponent, not {name}")
            },
            budget,
        )?;
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
            reason = "integer range checked above"
        )]
        #[expect(clippy::cast_sign_loss, reason = "absolute value is nonnegative")]
        let mut exponent = value.abs() as usize;
        let mut result = Self::constant(Scalar::one(budget)?, budget)?;
        while exponent != 0 {
            if exponent & 1 != 0 {
                result = result.multiply(&self, budget)?;
            }
            exponent >>= 1;
            if exponent != 0 {
                self = self.multiply(&self, budget)?;
            }
        }
        Ok(result)
    }

    fn compile(expr: &Expr, budget: &WorkBudget) -> Result<Self> {
        budget.charge(Resource::EvaluationSteps, 1)?;
        if !contains_s(expr, budget)? {
            return Self::constant(Scalar::from_expr(expr, budget)?, budget);
        }
        match expr {
            Expr::Symbol(_) => {
                budget.charge(Resource::GeneratedNodes, 2)?;
                Ok(Self {
                    numerator: Polynomial(vec![Scalar::zero(budget)?, Scalar::one(budget)?]),
                    denominator: Polynomial::one(budget)?,
                })
            }
            Expr::Unary(UnaryOp::Plus, value) => Self::compile(value, budget),
            Expr::Unary(UnaryOp::Minus, value) => Self::constant(Scalar::zero(budget)?, budget)?
                .add("-", &Self::compile(value, budget)?, budget),
            Expr::Binary(op @ (BinaryOp::Add | BinaryOp::Sub), left, right) => Self::compile(
                left, budget,
            )?
            .add(op.symbol(), &Self::compile(right, budget)?, budget),
            Expr::Binary(BinaryOp::Mul, left, right) => {
                Self::compile(left, budget)?.multiply(&Self::compile(right, budget)?, budget)
            }
            Expr::Binary(BinaryOp::Div, left, right) if !contains_s(right, budget)? => {
                // Keep scalar division in the coefficient domain. Clearing a
                // parameter-valued scalar divisor could otherwise make an
                // undefined instance (e.g. p=0 in s/p) appear valid.
                let divisor = Scalar::from_expr(right, budget)?;
                let reciprocal = Scalar::one(budget)?.binary("/", &divisor, budget)?;
                Self::compile(left, budget)?.multiply(&Self::constant(reciprocal, budget)?, budget)
            }
            Expr::Binary(BinaryOp::Div, left, right) => Self::compile(left, budget)?
                .multiply(&Self::compile(right, budget)?.reciprocal()?, budget),
            Expr::Binary(BinaryOp::Pow, base, exponent) => {
                Self::compile(base, budget)?.power(exponent, budget)
            }
            _ => bail!("nonrational Laplace expression (unsupported operation on s)"),
        }
    }
}

/// Extract descending numerator and denominator coefficient expressions from
/// a proper rational transfer. The scalar subexpressions retain lexical
/// parameter names and are bound by the native model at occurrence preparation.
pub fn coefficients(raw: &str, budget: &WorkBudget) -> Result<(Vec<String>, Vec<String>)> {
    let expression = SpiceExpression::parse_with_budget(raw, budget)?;
    let result = Rational::compile(&expression.0, budget)?;
    ensure!(!result.denominator.is_zero(), "zero Laplace denominator");
    ensure!(
        result.numerator.0.len() <= result.denominator.0.len(),
        "improper Laplace transfer requires explicit derivative operators"
    );
    Ok((
        result.numerator.descending(budget)?,
        result.denominator.descending(budget)?,
    ))
}
