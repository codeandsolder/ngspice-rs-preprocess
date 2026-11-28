//! Prepared runtime power projection. Keep the other source operators in
//! their original grammar; fold only power and multiplicative chains, whose
//! precedence and ordering determine the zero-factor convention.
use super::{Cursor, append};
use crate::source_dialect::{group_end, split_arguments};
use anyhow::{Context, Result, bail, ensure};
use model_preprocess_command::{Resource, WorkBudget};

const ABS: &str = "simetrix_abs_pow";
const CARET: &str = "simetrix_caret_pow";
const REAL: &str = "simetrix_pow";
const PRODUCT: &str = "simetrix_abs_pow_product";

/// An absolute power remains structural until its enclosing product is
/// known. Other text is opaque to this pass, including native probe names.
enum Fragment {
    Text(String),
    Absolute { base: String, exponent: String },
}

struct Lowerer<'a> {
    limit: usize,
    budget: &'a WorkBudget,
}

impl Lowerer<'_> {
    fn text(&self, value: &str) -> Result<Fragment> {
        self.budget.charge(Resource::EmittedBytes, value.len())?;
        Ok(Fragment::Text(value.to_owned()))
    }

    fn call(&self, name: &str, args: &[&str]) -> Result<String> {
        let len = args.iter().fold(name.len().saturating_add(2), |len, arg| {
            len.saturating_add(arg.len()).saturating_add(1)
        });
        ensure!(
            len <= self.limit,
            "source power expansion byte budget exceeded"
        );
        self.budget.charge(Resource::GeneratedNodes, 1)?;
        self.budget.charge(Resource::EmittedBytes, len)?;
        Ok(format!("{name}({})", args.join(",")))
    }

    fn render(&self, value: Fragment) -> Result<String> {
        match value {
            Fragment::Text(text) => Ok(text),
            Fragment::Absolute { base, exponent } => self.call(ABS, &[&base, &exponent]),
        }
    }

    fn group(&self, cursor: &mut Cursor<'_>) -> Result<Fragment> {
        let (open, body, close) = cursor.group(self.budget)?;
        let expression = self.expression(body)?;
        // A mathematical group around one absolute power does not change
        // which operation is the right operand of its enclosing product.
        if matches!(open, '(' | '{') && matches!(expression, Fragment::Absolute { .. }) {
            return Ok(expression);
        }
        let body = self.render(expression)?;
        let mut output = String::new();
        append(&mut output, &open.to_string(), self.limit, self.budget)?;
        append(&mut output, &body, self.limit, self.budget)?;
        append(&mut output, &close.to_string(), self.limit, self.budget)?;
        Ok(Fragment::Text(output))
    }

    fn power_call(&self, name: &str, cursor: &mut Cursor<'_>) -> Result<Fragment> {
        let (_, body, _) = cursor.group(self.budget)?;
        let args = split_arguments(body, self.budget)?;
        let [base, exponent] = args.as_slice() else {
            bail!("{name} requires two arguments")
        };
        ensure!(
            !base.is_empty() && !exponent.is_empty(),
            "{name} requires two arguments"
        );
        let base = self.render(self.expression(base)?)?;
        let exponent = self.render(self.expression(exponent)?)?;
        if name.eq_ignore_ascii_case("PWR") {
            Ok(Fragment::Absolute { base, exponent })
        } else {
            Ok(Fragment::Text(self.call(REAL, &[&base, &exponent])?))
        }
    }

    fn identifier(&self, cursor: &mut Cursor<'_>) -> Result<Fragment> {
        let start = cursor.offset;
        cursor.identifier();
        let name = &cursor.input[start..cursor.offset];
        let end = cursor.offset;
        cursor.skip_space();
        if cursor.byte() == Some(b'(') {
            if name.eq_ignore_ascii_case("V") || name.eq_ignore_ascii_case("I") {
                let close = group_end(cursor.input, cursor.offset, self.budget)?;
                cursor.offset = close + 1;
                return self.text(&cursor.input[start..cursor.offset]);
            }
            if name.eq_ignore_ascii_case("PWR") || name.eq_ignore_ascii_case("POW") {
                return self.power_call(name, cursor);
            }
        }
        cursor.offset = end;
        let mut output = String::new();
        append(&mut output, name, self.limit, self.budget)?;
        loop {
            let end = cursor.offset;
            cursor.skip_space();
            if !matches!(cursor.byte(), Some(b'(' | b'[')) {
                cursor.offset = end;
                break;
            }
            // Function argument lists and TABLE[...] use grouping as syntax,
            // not an optional expression wrapper. Retain the delimiters.
            let (open, body, close) = cursor.group(self.budget)?;
            let body = self.render(self.expression(body)?)?;
            append(&mut output, &open.to_string(), self.limit, self.budget)?;
            append(&mut output, &body, self.limit, self.budget)?;
            append(&mut output, &close.to_string(), self.limit, self.budget)?;
        }
        Ok(Fragment::Text(output))
    }

    fn operand(&self, cursor: &mut Cursor<'_>) -> Result<Option<Fragment>> {
        let start = cursor.offset;
        cursor.skip_space();
        let prefix = cursor.offset;
        while matches!(cursor.byte(), Some(b'+' | b'-' | b'!' | b'~')) {
            cursor.offset += 1;
            cursor.skip_space();
        }
        let atom = cursor.offset;
        let value = match cursor.byte() {
            Some(b'(' | b'{' | b'[') => self.group(cursor)?,
            Some(quote @ (b'\'' | b'"')) => {
                let body = cursor.quoted(quote)?;
                if quote == b'\'' {
                    let body = self.render(self.expression(body)?)?;
                    self.text(&format!("({body})"))?
                } else {
                    self.text(&cursor.input[atom..cursor.offset])?
                }
            }
            Some(b'0'..=b'9' | b'.') => {
                cursor.number();
                self.text(&cursor.input[atom..cursor.offset])?
            }
            Some(byte) if byte.is_ascii_alphabetic() || matches!(byte, b'_' | b'$' | b'@') => {
                self.identifier(cursor)?
            }
            _ => {
                cursor.offset = start;
                return Ok(None);
            }
        };
        self.budget.charge(Resource::GeneratedNodes, 1)?;
        if prefix == atom {
            Ok(Some(value))
        } else {
            let value = self.render(value)?;
            let mut output = String::new();
            append(
                &mut output,
                &cursor.input[prefix..atom],
                self.limit,
                self.budget,
            )?;
            append(&mut output, &value, self.limit, self.budget)?;
            Ok(Some(Fragment::Text(output)))
        }
    }

    fn power(&self, cursor: &mut Cursor<'_>) -> Result<Option<Fragment>> {
        let Some(mut lhs) = self.operand(cursor)? else {
            return Ok(None);
        };
        loop {
            let end = cursor.offset;
            cursor.skip_space();
            let absolute = cursor.input[cursor.offset..].starts_with("**");
            if absolute {
                cursor.offset += 2;
            } else if cursor.byte() == Some(b'^') {
                cursor.offset += 1;
            } else {
                cursor.offset = end;
                break;
            }
            let rhs = self
                .operand(cursor)?
                .context("missing right source power operand")?;
            let base = self.render(lhs)?;
            let exponent = self.render(rhs)?;
            // Source unary signs bind to operands. Both powers associate left.
            lhs = if absolute {
                Fragment::Absolute { base, exponent }
            } else {
                Fragment::Text(self.call(CARET, &[&base, &exponent])?)
            };
        }
        Ok(Some(lhs))
    }

    fn product(&self, cursor: &mut Cursor<'_>) -> Result<Option<Fragment>> {
        let Some(mut lhs) = self.power(cursor)? else {
            return Ok(None);
        };
        loop {
            let end = cursor.offset;
            cursor.skip_space();
            let Some(op @ (b'*' | b'/' | b'%' | b'\\')) = cursor.byte() else {
                cursor.offset = end;
                break;
            };
            cursor.offset += 1;
            let rhs = self
                .power(cursor)?
                .context("missing right multiplicative operand")?;
            let left = self.render(lhs)?;
            lhs = if op == b'*' {
                if let Fragment::Absolute { base, exponent } = rhs {
                    Fragment::Text(self.call(PRODUCT, &[&left, &base, &exponent])?)
                } else {
                    self.binary(left, op, rhs)?
                }
            } else {
                self.binary(left, op, rhs)?
            };
        }
        Ok(Some(lhs))
    }

    fn binary(&self, mut lhs: String, op: u8, rhs: Fragment) -> Result<Fragment> {
        let rhs = self.render(rhs)?;
        append(
            &mut lhs,
            &char::from(op).to_string(),
            self.limit,
            self.budget,
        )?;
        append(&mut lhs, &rhs, self.limit, self.budget)?;
        Ok(Fragment::Text(lhs))
    }

    fn expression(&self, input: &str) -> Result<Fragment> {
        let _frame = self.budget.enter()?;
        self.budget.charge(Resource::EvaluationSteps, input.len())?;
        ensure!(
            input.len() <= self.limit,
            "source power expansion byte budget exceeded"
        );
        let mut cursor = Cursor::new(input);
        let mut output = String::new();
        let mut expect_operand = true;
        while cursor.offset < input.len() {
            if expect_operand && let Some(value) = self.product(&mut cursor)? {
                if output.is_empty() && input[cursor.offset..].trim().is_empty() {
                    return Ok(value);
                }
                append(&mut output, &self.render(value)?, self.limit, self.budget)?;
                expect_operand = false;
                continue;
            }
            let start = cursor.offset;
            let ch = input[start..]
                .chars()
                .next()
                .context("source power expression boundary")?;
            ensure!(
                ch != '^' && !input[start..].starts_with("**"),
                "missing left source power operand"
            );
            cursor.offset += ch.len_utf8();
            append(
                &mut output,
                &input[start..cursor.offset],
                self.limit,
                self.budget,
            )?;
            if !ch.is_whitespace() {
                expect_operand = true;
            }
        }
        Ok(Fragment::Text(output))
    }
}

pub(in crate::source_dialect) fn lower(
    input: &str,
    limit: usize,
    budget: &WorkBudget,
) -> Result<String> {
    let lowerer = Lowerer { limit, budget };
    lowerer.render(lowerer.expression(input)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn powers_and_products_preserve_source_precedence() -> Result<()> {
        for (input, expected) in [
            ("-2**3^2", "simetrix_caret_pow(simetrix_abs_pow(-2,3),2)"),
            ("-(2**3)", "-simetrix_abs_pow(2,3)"),
            ("2^3**-2", "simetrix_abs_pow(simetrix_caret_pow(2,3),-2)"),
            ("f*PWR(x,p)", "simetrix_abs_pow_product(f,x,p)"),
            ("a/b*(x**p)", "simetrix_abs_pow_product(a/b,x,p)"),
            ("a+b*c**p", "a+simetrix_abs_pow_product(b,c,p)"),
            ("PWR(x,p)*f", "simetrix_abs_pow(x,p)*f"),
            ("a/POW(x,p)", "a/simetrix_pow(x,p)"),
            (
                "IF(x<0,PWR(x,p),x^p)",
                "IF(x<0,simetrix_abs_pow(x,p),simetrix_caret_pow(x,p))",
            ),
            (
                "V(a**b,c^d)**I(v^sense)",
                "simetrix_abs_pow(V(a**b,c^d),I(v^sense))",
            ),
        ] {
            assert_eq!(
                lower(input, 4096, &WorkBudget::default())?,
                expected,
                "{input}"
            );
        }
        assert!(lower("x**", 4096, &WorkBudget::default()).is_err());
        assert!(lower("PWR(x)", 4096, &WorkBudget::default()).is_err());
        Ok(())
    }
}
