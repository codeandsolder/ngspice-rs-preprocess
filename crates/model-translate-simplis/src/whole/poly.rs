//! SPICE2/PSpice graded-lexicographic polynomials. Enumerate only one
//! run-length encoded monomial at a time; zero coefficients never expand.
//! Balanced emission keeps generated expression depth logarithmic without
//! recursion or relying on dialect-specific signed power operators.

use super::{append_budgeted, append_format_budgeted, pwls};
use anyhow::{Context, Result, ensure};
use model_preprocess_command::{Resource, WorkBudget};

/// Read both parenthesized and legacy whitespace dimension spellings.
pub(super) fn header(tokens: &[String], at: usize) -> Result<Option<(usize, usize)>> {
    let Some(token) = tokens.get(at) else {
        return Ok(None);
    };
    let Some(prefix) = token.get(..4) else {
        return Ok(None);
    };
    if !prefix.eq_ignore_ascii_case("poly") {
        return Ok(None);
    }
    let mut end = at + 1;
    let mut dimension = &token[4..];
    if dimension.is_empty() {
        dimension = tokens.get(end).context("POLY has no dimension")?;
        end += 1;
    } else if !dimension.starts_with('(') {
        return Ok(None);
    }
    if let Some(rest) = dimension.strip_prefix('(') {
        dimension = rest;
        if dimension.is_empty() {
            dimension = tokens.get(end).context("POLY has no dimension")?;
            end += 1;
        }
        if let Some(value) = dimension.strip_suffix(')') {
            dimension = value;
        } else {
            ensure!(
                tokens.get(end).is_some_and(|s| s == ")"),
                "unclosed POLY dimension"
            );
            end += 1;
        }
    }
    let dimension = dimension
        .parse::<usize>()
        .context("POLY needs a positive integer dimension")?;
    ensure!(dimension > 0, "POLY needs a positive integer dimension");
    Ok(Some((dimension, end)))
}

/// A statement token already has balanced braces and quotes, but may mix
/// group punctuation and braced coefficient expressions. Do not split the
/// latter at arithmetic whitespace, commas or parentheses.
#[derive(Clone, Default)]
struct FieldContext {
    braces: usize,
    quoted: bool,
}

impl FieldContext {
    const fn expression(&mut self, c: char) -> bool {
        match c {
            '\'' => {
                self.quoted = !self.quoted;
                true
            }
            '{' if !self.quoted => {
                // Bounded by the number of characters in the input token.
                self.braces += 1;
                true
            }
            '}' if !self.quoted => {
                self.braces = self.braces.saturating_sub(1);
                true
            }
            _ => self.quoted || self.braces != 0,
        }
    }
}

/// Remaining POLY operands admit whitespace, commas and grouping parentheses.
pub(super) fn fields(tokens: &[String]) -> impl Iterator<Item = &str> + Clone {
    tokens.iter().flat_map(|token| {
        let mut context = FieldContext::default();
        token
            .split(move |c: char| {
                !context.expression(c) && (c.is_ascii_whitespace() || matches!(c, ',' | '(' | ')'))
            })
            .filter(|field| !field.is_empty())
    })
}

pub(super) fn operands<'a>(
    tokens: &'a [String],
    control_count: usize,
    budget: &WorkBudget,
) -> Result<impl Iterator<Item = &'a str> + Clone> {
    let mut depth = 0_usize;
    for token in tokens {
        budget.charge(Resource::ScannedBytes, token.len())?;
        let mut context = FieldContext::default();
        for c in token.chars() {
            if context.expression(c) {
                continue;
            }
            match c {
                '(' => {
                    depth = depth
                        .checked_add(1)
                        .context("POLY operand nesting overflow")?;
                }
                ')' => {
                    depth = depth
                        .checked_sub(1)
                        .context("unmatched POLY operand parenthesis")?;
                }
                _ => {}
            }
        }
    }
    ensure!(depth == 0, "unclosed POLY operand parenthesis");
    let operands = fields(tokens);
    let count = operands.clone().count();
    budget.charge(Resource::EvaluationSteps, count)?;
    ensure!(
        count > control_count,
        "POLY source has too few controls or coefficients"
    );
    Ok(operands)
}

pub(super) fn coefficients<'a>(
    fields: impl Iterator<Item = &'a str>,
    budget: &WorkBudget,
) -> Result<Vec<f64>> {
    let mut coefficients = Vec::new();
    for token in fields {
        budget.charge(Resource::GeneratedNodes, 1)?;
        let value = pwls::constant(token, budget)?
            .context("POLY coefficient must be a numeric constant")?;
        ensure!(value.is_finite(), "POLY coefficient must be finite");
        coefficients.try_reserve(1)?;
        coefficients.push(value);
    }
    ensure!(!coefficients.is_empty(), "POLY source has no coefficients");
    Ok(coefficients)
}

#[derive(Default)]
struct Monomial {
    degree: usize,
    runs: Vec<(usize, usize)>,
}

impl Monomial {
    /// Successor of a nondecreasing multiset of factor indices. The last
    /// maximal run is removed, the preceding factor is incremented, and the
    /// removed suffix is filled with that index. All-maximal advances degree.
    fn advance(&mut self, dimension: usize, budget: &WorkBudget) -> Result<()> {
        budget.charge(Resource::EvaluationSteps, 1)?;
        let trailing = if self
            .runs
            .last()
            .is_some_and(|&(index, _)| index == dimension - 1)
        {
            self.runs.pop().map_or(0, |(_, count)| count)
        } else {
            0
        };
        self.runs.try_reserve(1)?;
        if let Some((index, count)) = self.runs.last_mut() {
            let next = *index + 1;
            *count -= 1;
            if *count == 0 {
                self.runs.pop();
            }
            self.runs.push((
                next,
                trailing
                    .checked_add(1)
                    .context("POLY multiplicity overflow")?,
            ));
        } else {
            self.degree = self.degree.checked_add(1).context("POLY degree overflow")?;
            self.runs.push((0, self.degree));
        }
        Ok(())
    }

    fn factors(&self) -> impl Iterator<Item = usize> + '_ {
        self.runs
            .iter()
            .flat_map(|&(index, count)| std::iter::repeat_n(index, count))
    }
}

enum Emit {
    Group { count: usize, parentheses: bool },
    Operator,
    Close,
}

/// Emit a balanced binary tree in source order with bounded explicit stack.
/// Each leaf is supplied lazily; punctuation and factors are charged before
/// growing the intermediate output. No factorials, recursive calls or powers.
fn balanced(
    output: &mut String,
    count: usize,
    operator: &str,
    parentheses: bool,
    budget: &WorkBudget,
    mut leaf: impl FnMut(&mut String) -> Result<()>,
) -> Result<()> {
    let mut work = Vec::new();
    work.try_reserve(1)?;
    work.push(Emit::Group { count, parentheses });
    while let Some(action) = work.pop() {
        budget.charge(Resource::EvaluationSteps, 1)?;
        match action {
            Emit::Group { count: 0, .. } => {}
            Emit::Group { count: 1, .. } => leaf(output)?,
            Emit::Group { count, parentheses } => {
                work.try_reserve(4)?;
                if parentheses {
                    append_budgeted(output, "(", budget)?;
                    work.push(Emit::Close);
                }
                let left = count / 2;
                work.push(Emit::Group {
                    count: count - left,
                    parentheses: true,
                });
                work.push(Emit::Operator);
                work.push(Emit::Group {
                    count: left,
                    parentheses: true,
                });
            }
            Emit::Operator => append_budgeted(output, operator, budget)?,
            Emit::Close => append_budgeted(output, ")", budget)?,
        }
    }
    Ok(())
}

struct Terms<'a> {
    coefficients: std::slice::Iter<'a, f64>,
    monomial: Monomial,
    dimension: usize,
    budget: &'a WorkBudget,
}

impl Terms<'_> {
    fn next_nonzero(&mut self) -> Result<f64> {
        for &coefficient in self.coefficients.by_ref() {
            self.monomial.advance(self.dimension, self.budget)?;
            if coefficient != 0. {
                return Ok(coefficient);
            }
        }
        anyhow::bail!("POLY internal coefficient traversal mismatch")
    }

    fn emit(
        &mut self,
        output: &mut String,
        write_factor: &mut impl FnMut(&mut String, usize) -> Result<()>,
    ) -> Result<()> {
        let coefficient = self.next_nonzero()?;
        let degree = self.monomial.degree;
        self.budget.charge(Resource::GeneratedNodes, degree)?;
        append_format_budgeted(output, self.budget, format_args!("({coefficient:.17e})*"))?;
        let mut factors = self.monomial.factors();
        balanced(output, degree, "*", true, self.budget, |output| {
            write_factor(
                output,
                factors
                    .next()
                    .context("POLY internal factor count mismatch")?,
            )
        })
    }
}

pub(super) fn write_expression(
    output: &mut String,
    dimension: usize,
    coefficients: &[f64],
    budget: &WorkBudget,
    mut write_factor: impl FnMut(&mut String, usize) -> Result<()>,
) -> Result<()> {
    ensure!(dimension > 0, "POLY needs a positive integer dimension");
    let (&first, tail) = coefficients
        .split_first()
        .context("POLY source has no coefficients")?;
    // SPICE2/PSpice/SIMetrix one-dimensional shorthand is gain, not offset.
    let (constant, tail) = if dimension == 1 && tail.is_empty() {
        (0., coefficients)
    } else {
        (first, tail)
    };
    append_format_budgeted(output, budget, format_args!("{constant:.17e}"))?;
    budget.charge(Resource::EvaluationSteps, tail.len())?;
    let count = tail
        .iter()
        .filter(|&&coefficient| coefficient != 0.)
        .count();
    if count == 0 {
        return Ok(());
    }
    let mut terms = Terms {
        coefficients: tail.iter(),
        monomial: Monomial::default(),
        dimension,
        budget,
    };
    append_budgeted(output, "+", budget)?;
    balanced(output, count, "+", false, budget, |output| {
        terms.emit(output, &mut write_factor)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use model_preprocess_command::{Cancellation, WorkLimits};

    fn evaluated(controls: &[f64], coefficients: &[f64]) -> Result<(f64, String)> {
        let budget = WorkBudget::default();
        let mut output = String::new();
        write_expression(
            &mut output,
            controls.len(),
            coefficients,
            &budget,
            |output, index| {
                append_format_budgeted(output, &budget, format_args!("({:.17e})", controls[index]))
            },
        )?;
        Ok((crate::parse_numeric(&output)?, output))
    }

    #[test]
    fn operand_groups_preserve_constant_expression_boundaries() -> Result<()> {
        let tokens = crate::syntax::statement_tokens(
            "POLY ( 2 ) (a, 0) (b,0) (0, {2 * (3 + 1)}, 'min(4, 7)')",
        )?;
        let (dimension, start) = header(&tokens, 0)?.context("POLY header")?;
        assert_eq!(dimension, 2);
        let budget = WorkBudget::default();
        let mut operands = operands(&tokens[start..], 4, &budget)?;
        assert_eq!(
            operands.by_ref().take(4).collect::<Vec<_>>(),
            ["a", "0", "b", "0"]
        );
        let coefficients = coefficients(operands, &budget)?;
        assert_eq!(coefficients, [0., 8., 4.]);
        Ok(())
    }

    #[test]
    fn graded_lexicographic_coefficients_through_cubic() -> Result<()> {
        let controls = [-2., 3., -0.5];
        // Independent nested enumeration of constant, linear, quadratic,
        // and cubic terms, deliberately nonuniform signed coefficients.
        let mut terms = vec![1.];
        for &a in &controls {
            terms.push(a);
        }
        for (i, &a) in controls.iter().enumerate() {
            for &b in &controls[i..] {
                terms.push(a * b);
            }
        }
        for (i, &a) in controls.iter().enumerate() {
            for (j, &b) in controls.iter().enumerate().skip(i) {
                for &c in &controls[j..] {
                    terms.push(a * b * c);
                }
            }
        }
        for selected in 0..terms.len() {
            let mut coefficients = vec![0.; terms.len()];
            coefficients[selected] = 1.;
            let (actual, _) = evaluated(&controls, &coefficients)?;
            assert_eq!(
                actual.to_bits(),
                terms[selected].to_bits(),
                "term {selected}"
            );
        }
        let coefficients = (0..terms.len())
            .map(|index| u32::try_from(index + 1).map(f64::from))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let expected = terms
            .iter()
            .zip(&coefficients)
            .map(|(term, coefficient)| term * coefficient)
            .sum::<f64>();
        let (actual, _) = evaluated(&controls, &coefficients)?;
        assert_eq!(actual.to_bits(), expected.to_bits());
        Ok(())
    }

    #[test]
    fn sparse_high_dimension_and_high_degree_have_no_fixed_caps() -> Result<()> {
        let mut controls = vec![1.; 64];
        controls[0] = -2.;
        controls[63] = 3.;
        let mut coefficients = vec![0.; 1 + 64 + 64 * 65 / 2];
        coefficients[64] = 7.;
        coefficients[66] = 5.; // x0*x1, not x0*x0
        let last = coefficients.len() - 1;
        coefficients[last] = -3.; // x63*x63
        let (actual, expression) = evaluated(&controls, &coefficients)?;
        assert_eq!(actual.to_bits(), (-16_f64).to_bits());
        assert!(
            expression.len() < 300,
            "zero coefficients must not emit factors"
        );

        let mut coefficients = vec![0.; 514];
        coefficients[513] = 2.;
        let (actual, expression) = evaluated(&[-1.], &coefficients)?;
        assert_eq!(actual.to_bits(), (-2_f64).to_bits());
        assert!(expression.len() < 20_000);
        Ok(())
    }

    #[test]
    fn single_coefficient_shorthand_is_dimension_specific() -> Result<()> {
        let (gain, _) = evaluated(&[-2.], &[7.])?;
        let (constant, _) = evaluated(&[-2., 3.], &[7.])?;
        assert_eq!(gain.to_bits(), (-14_f64).to_bits());
        assert_eq!(constant.to_bits(), 7_f64.to_bits());
        Ok(())
    }

    #[test]
    fn budgets_stop_sparse_traversal_and_materialization() -> Result<()> {
        let coefficients = vec![0.; 1_000];
        let budget = WorkBudget::new(
            WorkLimits {
                evaluation_steps: 10,
                ..WorkLimits::default()
            },
            Cancellation::default(),
        );
        let error = write_expression(&mut String::new(), 1, &coefficients, &budget, |_, _| Ok(()))
            .err()
            .context("sparse scan exceeds budget")?;
        assert!(format!("{error:#}").contains("EvaluationSteps"));
        assert!(budget.check().is_err());

        let mut coefficients = vec![0.; 1_000];
        coefficients[999] = 1.;
        let budget = WorkBudget::new(
            WorkLimits {
                emitted_bytes: 100,
                ..WorkLimits::default()
            },
            Cancellation::default(),
        );
        let mut output = String::new();
        let error = write_expression(&mut output, 1, &coefficients, &budget, |output, _| {
            append_budgeted(output, "v(a,0)", &budget)
        })
        .err()
        .context("factor emission exceeds budget")?;
        assert!(format!("{error:#}").contains("EmittedBytes"));
        assert!(output.len() <= 100);
        assert!(budget.check().is_err());
        Ok(())
    }
}
