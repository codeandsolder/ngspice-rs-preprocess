//! Lower source absolute-base exponentiation without changing native power
//! semantics. Only power chains are reassembled; other operators and their
//! precedence are left to the native expression parser.

use super::group_end;
use anyhow::{Context, Result, bail, ensure};
use model_preprocess_command::{Resource, WorkBudget};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Operand,
    AbsolutePower,
    SignedPower,
    Other,
}

impl Kind {
    const fn is_power(self) -> bool {
        matches!(self, Self::AbsolutePower | Self::SignedPower)
    }
}

struct Token {
    kind: Kind,
    text: String,
}

struct Lowerer<'a> {
    limit: usize,
    budget: &'a WorkBudget,
}

struct Cursor<'a> {
    input: &'a str,
    offset: usize,
}

impl<'a> Cursor<'a> {
    const fn new(input: &'a str) -> Self {
        Self { input, offset: 0 }
    }

    fn byte(&self) -> Option<u8> {
        self.input.as_bytes().get(self.offset).copied()
    }

    fn skip_space(&mut self) {
        while self.byte().is_some_and(|byte| byte.is_ascii_whitespace()) {
            self.offset += 1;
        }
    }

    fn number(&mut self) {
        let start = self.offset;
        while let Some(byte) = self.byte() {
            if byte.is_ascii_alphanumeric()
                || byte == b'.'
                || (matches!(byte, b'+' | b'-')
                    && self.offset > start
                    && matches!(self.input.as_bytes()[self.offset - 1], b'e' | b'E'))
            {
                self.offset += 1;
            } else {
                break;
            }
        }
    }

    fn identifier(&mut self) {
        while self.byte().is_some_and(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$' | b'.' | b'@' | b'#')
        }) {
            self.offset += 1;
        }
    }

    fn group(&mut self, budget: &WorkBudget) -> Result<(char, &'a str, char)> {
        let open = self.offset;
        let (opener, closer) = match self.byte() {
            Some(b'(') => ('(', ')'),
            Some(b'{') => ('{', '}'),
            Some(b'[') => ('[', ']'),
            _ => bail!("expected source power operand group"),
        };
        let end = group_end(self.input, open, budget)?;
        self.offset = end + 1;
        Ok((opener, &self.input[open + 1..end], closer))
    }

    fn quoted(&mut self, quote: u8) -> Result<&'a str> {
        let start = self.offset + 1;
        let end = self.input[start..]
            .find(char::from(quote))
            .map(|relative| start + relative)
            .context("unterminated source power expression quote")?;
        self.offset = end + 1;
        Ok(&self.input[start..end])
    }
}

impl Lowerer<'_> {
    fn group(&self, cursor: &mut Cursor<'_>) -> Result<String> {
        let (open, body, close) = cursor.group(self.budget)?;
        let body = self.expression(body)?;
        self.budget
            .charge(Resource::EmittedBytes, body.len().saturating_add(2))?;
        Ok(format!("{open}{body}{close}"))
    }

    fn identifier(&self, cursor: &mut Cursor<'_>) -> Result<String> {
        let start = cursor.offset;
        cursor.identifier();
        let name = &cursor.input[start..cursor.offset];
        self.budget.charge(Resource::EmittedBytes, name.len())?;
        let mut output = name.to_owned();
        let mut first = true;
        loop {
            let before_space = cursor.offset;
            cursor.skip_space();
            let Some(b'(' | b'[') = cursor.byte() else {
                cursor.offset = before_space;
                break;
            };
            if first
                && cursor.byte() == Some(b'(')
                && (name.eq_ignore_ascii_case("V") || name.eq_ignore_ascii_case("I"))
            {
                // Probe arguments are electrical names, not arithmetic.
                let close = group_end(cursor.input, cursor.offset, self.budget)?;
                self.budget
                    .charge(Resource::EmittedBytes, close + 1 - before_space)?;
                output.push_str(&cursor.input[before_space..=close]);
                cursor.offset = close + 1;
            } else {
                let whitespace = &cursor.input[before_space..cursor.offset];
                let group = self.group(cursor)?;
                self.budget.charge(
                    Resource::EmittedBytes,
                    whitespace.len().saturating_add(group.len()),
                )?;
                output.push_str(whitespace);
                output.push_str(&group);
            }
            first = false;
        }
        Ok(output)
    }

    fn operand(&self, cursor: &mut Cursor<'_>) -> Result<Option<String>> {
        let Some(byte) = cursor.byte() else {
            return Ok(None);
        };
        let value = match byte {
            b'(' | b'{' | b'[' => self.group(cursor)?,
            b'\'' | b'"' => {
                let body = cursor.quoted(byte)?;
                let quote = char::from(byte);
                // Single quotes delimit electrical expressions; double
                // quoted literal text is not an expression.
                if byte == b'\'' {
                    format!("{quote}{}{quote}", self.expression(body)?)
                } else {
                    format!("{quote}{body}{quote}")
                }
            }
            b'0'..=b'9' | b'.' => {
                let start = cursor.offset;
                cursor.number();
                cursor.input[start..cursor.offset].to_owned()
            }
            byte if byte.is_ascii_alphabetic() || matches!(byte, b'_' | b'$' | b'@') => {
                self.identifier(cursor)?
            }
            _ => return Ok(None),
        };
        Ok(Some(value))
    }

    fn tokens(&self, input: &str) -> Result<Vec<Token>> {
        let mut cursor = Cursor::new(input);
        let mut tokens = Vec::new();
        let mut bytes = 0usize;
        let mut expect_operand = true;
        while cursor.offset < input.len() {
            let start = cursor.offset;
            cursor.skip_space();
            let token_start = cursor.offset;
            if expect_operand {
                while cursor
                    .byte()
                    .is_some_and(|byte| matches!(byte, b'+' | b'-' | b'!' | b'~'))
                {
                    cursor.offset += 1;
                    cursor.skip_space();
                }
            }
            let operand_start = cursor.offset;
            if let Some(operand) = self.operand(&mut cursor)? {
                let prefix = &input[start..operand_start];
                bytes = bytes
                    .saturating_add(prefix.len())
                    .saturating_add(operand.len());
                ensure!(
                    bytes <= self.limit,
                    "source power expansion byte budget exceeded"
                );
                self.budget.charge(Resource::GeneratedNodes, 1)?;
                self.budget.charge(
                    Resource::EmittedBytes,
                    prefix.len().saturating_add(operand.len()),
                )?;
                tokens.push(Token {
                    kind: Kind::Operand,
                    text: format!("{prefix}{operand}"),
                });
                expect_operand = false;
                continue;
            }
            cursor.offset = token_start;
            let tail = &input[token_start..];
            let (kind, width) = if tail.starts_with("**") {
                (Kind::AbsolutePower, 2)
            } else if tail.starts_with('^') {
                (Kind::SignedPower, 1)
            } else {
                (Kind::Other, tail.chars().next().map_or(0, char::len_utf8))
            };
            cursor.offset += width;
            bytes = bytes.saturating_add(cursor.offset - start);
            ensure!(
                bytes <= self.limit,
                "source power expansion byte budget exceeded"
            );
            self.budget.charge(Resource::GeneratedNodes, 1)?;
            self.budget
                .charge(Resource::EmittedBytes, cursor.offset - start)?;
            tokens.push(Token {
                kind,
                text: input[start..cursor.offset].to_owned(),
            });
            expect_operand = true;
        }
        Ok(tokens)
    }

    fn fold(&self, tokens: &[Token]) -> Result<String> {
        let mut output = String::new();
        let mut index = 0;
        while index < tokens.len() {
            let start = index;
            let first = &tokens[index];
            ensure!(!first.kind.is_power(), "missing left source power operand");
            index += 1;
            if first.kind != Kind::Operand {
                append(&mut output, &first.text, self.limit, self.budget)?;
                continue;
            }
            let mut absolute = false;
            while tokens.get(index).is_some_and(|token| token.kind.is_power()) {
                absolute |= tokens[index].kind == Kind::AbsolutePower;
                ensure!(
                    tokens
                        .get(index + 1)
                        .is_some_and(|token| token.kind == Kind::Operand),
                    "missing right source power operand"
                );
                index += 2;
            }
            if !absolute {
                for token in &tokens[start..index] {
                    append(&mut output, &token.text, self.limit, self.budget)?;
                }
                continue;
            }
            // Both infix operators associate left-to-right in the source
            // dialect. Unary signs are part of their operands.
            let leading = &first.text[..first.text.len() - first.text.trim_start().len()];
            let mut lhs = first.text.trim().to_owned();
            for pair in tokens[start + 1..index].as_chunks::<2>().0 {
                self.budget.charge(Resource::EvaluationSteps, lhs.len())?;
                let rhs = pair[1].text.trim();
                lhs = if pair[0].kind == Kind::AbsolutePower {
                    guarded(
                        &lhs,
                        rhs,
                        self.limit.saturating_sub(output.len()),
                        self.budget,
                    )?
                } else {
                    ensure!(
                        lhs.len().saturating_add(rhs.len()).saturating_add(8) <= self.limit,
                        "source power expansion byte budget exceeded"
                    );
                    self.budget.charge(
                        Resource::EmittedBytes,
                        lhs.len().saturating_add(rhs.len()).saturating_add(8),
                    )?;
                    format!("(({lhs})^({rhs}))")
                };
            }
            append(&mut output, leading, self.limit, self.budget)?;
            append(&mut output, &lhs, self.limit, self.budget)?;
        }
        Ok(output)
    }

    fn expression(&self, input: &str) -> Result<String> {
        ensure!(
            input.len() <= self.limit,
            "source power expansion byte budget exceeded"
        );
        self.budget.charge(Resource::EmittedBytes, input.len())?;
        if !input.contains("**") {
            return Ok(input.to_owned());
        }
        let _frame = self.budget.enter()?;
        self.budget.charge(Resource::EvaluationSteps, input.len())?;
        let tokens = self.tokens(input)?;
        self.fold(&tokens)
    }
}

fn append(output: &mut String, text: &str, limit: usize, budget: &WorkBudget) -> Result<()> {
    ensure!(
        text.len() <= limit.saturating_sub(output.len()),
        "source power expansion byte budget exceeded"
    );
    budget.charge(Resource::EmittedBytes, text.len())?;
    output.push_str(text);
    Ok(())
}

/// The source zero-base derivative is zero for nonnegative powers, even
/// where the ordinary native power derivative is singular. Keep a true
/// negative-power singularity visible to the simulator.
pub(super) fn guarded(
    base: &str,
    exponent: &str,
    limit: usize,
    budget: &WorkBudget,
) -> Result<String> {
    let base = base.trim();
    let exponent = exponent.trim();
    let bound = base
        .len()
        .saturating_mul(2)
        .saturating_add(exponent.len().saturating_mul(3))
        .saturating_add(48);
    ensure!(
        bound <= limit,
        "source power expansion byte budget exceeded"
    );
    budget.charge(Resource::EmittedBytes, bound)?;
    Ok(format!(
        "((({base})==0)&&(({exponent})>=0)?(({exponent})==0?1:0):abs({base})^({exponent}))"
    ))
}

#[cfg(test)]
fn lower(input: &str, limit: usize) -> Result<String> {
    lower_with_budget(input, limit, &WorkBudget::default())
}

pub(super) fn lower_with_budget(input: &str, limit: usize, budget: &WorkBudget) -> Result<String> {
    Lowerer { limit, budget }.expression(input)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_power_chains_and_unary_operands() -> Result<()> {
        let limit = 4096;
        let first = guarded("2", "3", limit, &WorkBudget::default())?;
        assert_eq!(
            lower("2**3**2", limit)?,
            guarded(&first, "2", limit, &WorkBudget::default())?
        );
        assert_eq!(
            lower("-2**3", limit)?,
            guarded("-2", "3", limit, &WorkBudget::default())?
        );
        assert_eq!(
            lower("2**-3", limit)?,
            guarded("2", "-3", limit, &WorkBudget::default())?
        );
        assert_eq!(
            lower("2^3**2", limit)?,
            guarded("((2)^(3))", "2", limit, &WorkBudget::default())?
        );
        assert_eq!(lower("2**3^2", limit)?, format!("(({first})^(2))"));
        assert_eq!(
            lower("V(a-b,c)**.5", limit)?,
            guarded("V(a-b,c)", ".5", limit, &WorkBudget::default())?
        );
        for literal in ["V(node**name)", r#""PWR(x,2)""#, "a^b"] {
            // A voltage/current probe names electrical objects, not expressions.
            assert_eq!(lower(literal, limit)?, literal);
        }
        assert!(lower("a**", limit).is_err());
        assert!(lower("**a", limit).is_err());
        assert!(lower("2**3**2", 64).is_err());
        Ok(())
    }
}
