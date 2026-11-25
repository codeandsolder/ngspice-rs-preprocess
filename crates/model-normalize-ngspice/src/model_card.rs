//! Source-only `.model` statement recognition for plaintext SPICE libraries.
//!
//! This is the lexical front-end, not a solver expression interpreter. Raw
//! expressions stay intact for a later parameter resolver. Keeping that
//! distinction avoids silently evaluating vendor symbols with wrong defaults.

use anyhow::{Context, Result, bail, ensure};

/// A model parameter in original source order; duplicates are deliberately
/// retained for source-dialect resolution rather than guessed here.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelParameter {
    /// Case-preserved identifier.
    pub name: String,
    /// Original expression, or `None` for a bare model flag.
    pub expression: Option<String>,
}

/// One lexed `.model` statement. This is deliberately model-family agnostic.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelCard {
    /// Declared model identifier.
    pub name: String,
    /// Declared model type (e.g. `vdmos`, `vdmosp`, `nmos`).
    pub family: String,
    /// Named parameter tokens, preserving expression text and ordering.
    pub parameters: Vec<ModelParameter>,
    /// 1-based physical source line where the `.model` begins.
    pub source_line: usize,
}

/// Lex `.model` declarations in a plaintext ngspice library, including `+`
/// continuations, nesting, quotes, commas and unparenthesized parameter sets.
///
/// This does not perform `.param` or behavioral-expression evaluation, which
/// must occur in the source compiler with its explicit symbol scopes. Model
/// cards embedded in `.subckt` definitions retain their original source line;
/// a later scope/linking pass is responsible for visibility and precedence.
///
/// # Errors
///
/// Malformed model statements, mismatched parentheses/quotes, or non-UTF-8
/// model statements fail with context rather than producing partial models.
pub fn parse_model_cards(input: &[u8]) -> Result<Vec<ModelCard>> {
    logical_statements(input)?
        .into_iter()
        .filter_map(|(line, statement)| {
            is_model_start(&statement).then(|| {
                parse_one(
                    std::str::from_utf8(&statement).context("non-UTF-8 .model statement")?,
                    line,
                )
            })
        })
        .collect()
}

/// Combine `+` continuation lines and preserve physical source locations.
/// Never interpret opaque non-model source statements while lexing.
pub(crate) fn logical_statements(input: &[u8]) -> Result<Vec<(usize, Vec<u8>)>> {
    let normalized = crate::lexical::normalize_nbsp(input);
    let bytes = normalized.as_deref().unwrap_or(input);
    Ok(crate::lexical::statements(bytes)?
        .into_iter()
        .map(|statement| (statement.line, statement.text))
        .collect())
}

/// Decode named source parameters shared by `.param` and `.subckt` syntax.
/// Values may contain nested expressions/quoted strings, and names require `=`.
pub(crate) fn parse_assignments(input: &str) -> Result<Vec<ModelParameter>> {
    let mut source = Source {
        text: input,
        offset: 0,
    };
    let mut parsed = Vec::new();
    loop {
        source.skip_separators();
        if source.offset == input.len() {
            break;
        }
        let name = source.name();
        ensure!(!name.is_empty(), "invalid parameter identifier in {input}");
        source.skip_space();
        ensure!(
            input.as_bytes().get(source.offset) == Some(&b'='),
            "parameter {name} requires name=value"
        );
        source.offset += 1;
        source.skip_space();
        let expression = source
            .value()
            .with_context(|| format!("parameter {name}"))?;
        parsed.push(ModelParameter {
            name: name.to_owned(),
            expression: Some(expression.to_owned()),
        });
    }
    Ok(parsed)
}

/// Read positional tokens up to the first named parameter or PARAMS: marker.
/// Parenthesized terminal lists and commas use the same token boundaries as
/// native ngspice. Return the unconsumed assignment text.
pub(crate) fn positional_prefix(input: &str) -> (Vec<(&str, usize)>, &str) {
    let mut source = Source {
        text: input,
        offset: 0,
    };
    let mut tokens = Vec::new();
    loop {
        while input
            .as_bytes()
            .get(source.offset)
            .is_some_and(|b| b.is_ascii_whitespace() || matches!(*b, b',' | b'(' | b')'))
        {
            source.offset += 1;
        }
        let start = source.offset;
        let token = source.name();
        if token.is_empty() {
            return (tokens, &input[start..]);
        }
        if token
            .get(..7)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("params:"))
        {
            return (tokens, &input[start + 7..]);
        }
        source.skip_space();
        if input.as_bytes().get(source.offset) == Some(&b'=') {
            return (tokens, &input[start..]);
        }
        tokens.push((token, start + token.len()));
    }
}

/// Named scalar/flag parameters of an already identified MOS instance.
pub(crate) fn instance_fields(input: &str) -> Result<Vec<ModelParameter>> {
    let mut source = Source {
        text: input,
        offset: 0,
    };
    let mut parsed = Vec::new();
    loop {
        source.skip_separators();
        if source.offset == input.len() {
            return Ok(parsed);
        }
        let name = source.name();
        ensure!(!name.is_empty(), "invalid instance parameter in {input}");
        source.skip_space();
        let expression = if input.as_bytes().get(source.offset) == Some(&b'=') {
            source.offset += 1;
            source.skip_space();
            let start = source.offset;
            source.value()?;
            // Native IC supports one or two scalars, parenthesized or
            // comma-separated; keep the entire vector for typed binding.
            if name.eq_ignore_ascii_case("ic") {
                let end = source.offset;
                source.skip_space();
                let first = input.as_bytes().get(source.offset).copied();
                if first == Some(b',') {
                    source.offset += 1;
                    source.skip_space();
                    source.value()?;
                } else if input[start..end].starts_with('(') {
                    source.offset = end;
                } else if first.is_some_and(|b| {
                    b.is_ascii_digit() || matches!(b, b'+' | b'-' | b'.' | b'{' | b'\'' | b'(')
                }) {
                    source.value()?;
                } else {
                    source.offset = end;
                }
            }
            Some(input[start..source.offset].to_owned())
        } else {
            None
        };
        parsed.push(ModelParameter {
            name: name.to_owned(),
            expression,
        });
    }
}

fn is_model_start(line: &[u8]) -> bool {
    let Some(prefix) = line.get(..6) else {
        return false;
    };
    prefix.eq_ignore_ascii_case(b".model") && line.get(6).is_some_and(u8::is_ascii_whitespace)
}

struct Source<'a> {
    text: &'a str,
    offset: usize,
}

impl<'a> Source<'a> {
    fn skip_separators(&mut self) {
        while self
            .text
            .as_bytes()
            .get(self.offset)
            .is_some_and(|byte| byte.is_ascii_whitespace() || *byte == b',')
        {
            self.offset += 1;
        }
    }

    fn skip_space(&mut self) {
        while self
            .text
            .as_bytes()
            .get(self.offset)
            .is_some_and(u8::is_ascii_whitespace)
        {
            self.offset += 1;
        }
    }

    fn name(&mut self) -> &'a str {
        let start = self.offset;
        while self.text.as_bytes().get(self.offset).is_some_and(|byte| {
            !byte.is_ascii_whitespace() && !matches!(*byte, b'=' | b'(' | b')' | b',')
        }) {
            self.offset += 1;
        }
        &self.text[start..self.offset]
    }

    fn value(&mut self) -> Result<&'a str> {
        let start = self.offset;
        let mut quote = None;
        let mut escaped = false;
        let mut round = 0_usize;
        let mut curly = 0_usize;
        let mut square = 0_usize;
        while let Some(&byte) = self.text.as_bytes().get(self.offset) {
            if let Some(delimiter) = quote {
                if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if byte == delimiter {
                    quote = None;
                }
            } else {
                match byte {
                    b'\'' | b'"' => quote = Some(byte),
                    b'(' => round += 1,
                    b')' if round > 0 => round -= 1,
                    b')' if curly == 0 && square == 0 => break,
                    b'{' => curly += 1,
                    b'}' if curly > 0 => curly -= 1,
                    b'[' => square += 1,
                    b']' if square > 0 => square -= 1,
                    b',' if round == 0 && curly == 0 && square == 0 => break,
                    b if b.is_ascii_whitespace() && round == 0 && curly == 0 && square == 0 => {
                        break;
                    }
                    _ => {}
                }
            }
            self.offset += 1;
        }
        ensure!(
            quote.is_none() && round == 0 && curly == 0 && square == 0,
            "unterminated quote or nested model parameter expression"
        );
        ensure!(start != self.offset, "empty .model parameter value");
        Ok(&self.text[start..self.offset])
    }
}

pub(crate) fn parse_one(statement: &str, source_line: usize) -> Result<ModelCard> {
    parse_model_card(statement, source_line, &[])
}

/// Parse one already joined model statement.
///
/// Allow explicitly selected
/// parameter names to use the source dialect's positional value syntax.
/// All other bare tokens remain flags; native parsing uses no positional names.
///
/// # Errors
/// Rejects malformed headers, missing selected values and unbalanced expressions.
pub fn parse_model_card(
    statement: &str,
    source_line: usize,
    positional: &[&str],
) -> Result<ModelCard> {
    ensure!(
        is_model_start(statement.as_bytes()),
        "expected .model statement"
    );
    let mut source = Source {
        text: statement,
        offset: 6,
    };
    source.skip_space();
    let name = source.name();
    ensure!(
        !name.is_empty(),
        "line {source_line}: .model requires a name"
    );
    source.skip_space();
    let family = source.name();
    ensure!(
        !family.is_empty(),
        "line {source_line}: .model {name} requires a family"
    );
    source.skip_space();
    let wrapper = statement.as_bytes().get(source.offset) == Some(&b'(');
    if wrapper {
        source.offset += 1;
    }
    let mut parameters = Vec::new();
    loop {
        source.skip_separators();
        let next = statement.as_bytes().get(source.offset).copied();
        if wrapper && next == Some(b')') {
            source.offset += 1;
            source.skip_space();
            ensure!(
                source.offset == statement.len(),
                "line {source_line}: trailing tokens after .model parameters"
            );
            break;
        }
        if next.is_none() {
            ensure!(
                !wrapper,
                "line {source_line}: missing closing .model parenthesis"
            );
            break;
        }
        ensure!(
            next != Some(b')'),
            "line {source_line}: unexpected closing parenthesis"
        );
        let name = source.name();
        ensure!(
            !name.is_empty(),
            "line {source_line}: invalid model parameter identifier"
        );
        source.skip_space();
        let assigned = statement.as_bytes().get(source.offset) == Some(&b'=');
        let expression = if assigned || positional.iter().any(|key| name.eq_ignore_ascii_case(key))
        {
            if assigned {
                source.offset += 1;
                source.skip_space();
            }
            Some(
                source
                    .value()
                    .with_context(|| format!("line {source_line}: {name}"))?
                    .to_owned(),
            )
        } else {
            None
        };
        parameters.push(ModelParameter {
            name: name.to_owned(),
            expression,
        });
    }
    Ok(ModelCard {
        name: name.to_owned(),
        family: family.to_owned(),
        parameters,
        source_line,
    })
}

/// Decode a single scalar SPICE numeric constant and its engineering suffix.
///
/// Trailing unit letters are supported (e.g. `100nF`, `5kOhm`), as is the
/// SPICE-specific `MEG` (mega), `M` (milli), and `MIL` (25.4 micrometres).
/// Expressions and named symbols stay in the source compiler; this function
/// intentionally rejects them rather than guessing a replacement value.
///
/// # Errors
///
/// Reports malformed, symbolic or nonfinite numeric constants.
pub fn parse_spice_constant(raw: &str) -> Result<f64> {
    let raw = raw.trim();
    let bytes = raw.as_bytes();
    let mut offset = usize::from(matches!(bytes.first(), Some(b'+' | b'-')));
    let mut digits = 0;
    while bytes.get(offset).is_some_and(u8::is_ascii_digit) {
        digits += 1;
        offset += 1;
    }
    if bytes.get(offset) == Some(&b'.') {
        offset += 1;
        while bytes.get(offset).is_some_and(u8::is_ascii_digit) {
            digits += 1;
            offset += 1;
        }
    }
    ensure!(digits > 0, "not a scalar SPICE number: {raw}");
    if matches!(bytes.get(offset), Some(b'e' | b'E' | b'd' | b'D'))
        && !["db", "deg"]
            .iter()
            .any(|unit| raw[offset..].eq_ignore_ascii_case(unit))
    {
        offset += 1;
        if matches!(bytes.get(offset), Some(b'+' | b'-')) {
            offset += 1;
        }
        let exponent_start = offset;
        while bytes.get(offset).is_some_and(u8::is_ascii_digit) {
            offset += 1;
        }
        ensure!(offset > exponent_start, "invalid numeric exponent in {raw}");
    }
    let scalar = raw[..offset]
        .replace(['d', 'D'], "e")
        .parse::<f64>()
        .with_context(|| format!("invalid SPICE numeric value {raw}"))?;
    let suffix = raw[offset..].to_ascii_lowercase();
    ensure!(
        suffix.bytes().all(|byte| byte.is_ascii_alphabetic()),
        "invalid SPICE unit suffix in {raw}"
    );
    let scale = if matches!(
        suffix.as_str(),
        "hz" | "v" | "ohm" | "s" | "rad" | "db" | "deg"
    ) {
        1.0
    } else if suffix.starts_with("meg") {
        1e6
    } else if suffix.starts_with("mil") {
        25.4e-6
    } else {
        match suffix.as_bytes().first() {
            None => 1.0,
            Some(b't') => 1e12,
            Some(b'g') => 1e9,
            Some(b'k') => 1e3,
            Some(b'm') => 1e-3,
            Some(b'u') => 1e-6,
            Some(b'n') => 1e-9,
            Some(b'p') => 1e-12,
            Some(b'f') => 1e-15,
            Some(b'a') => 1e-18,
            _ => bail!("unsupported SPICE numeric suffix in {raw}"),
        }
    };
    let converted = scalar * scale;
    ensure!(converted.is_finite(), "nonfinite SPICE numeric value {raw}");
    Ok(converted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiline_cards_preserve_expressions_flags_and_order() -> Result<()> {
        let source = b"* header\r\n.model nf VDMOS (VTO=3, KP=20 RQ=0.2\r\n+ VQ=1.5, CGS={a + max(b, c)}\r\n+ Cjo='C_JUNC*2' FLAG) $ trailing comment\r\n.model pf VDMOSP VTH0=-3 RDS=1MEG BV=70 IBV=1n\r\n";
        let cards = parse_model_cards(source)?;
        assert_eq!(cards.len(), 2);
        assert_eq!(cards[0].name, "nf");
        assert_eq!(cards[0].source_line, 2);
        assert_eq!(cards[0].parameters.len(), 7);
        assert_eq!(
            cards[0].parameters[4].expression.as_deref(),
            Some("{a + max(b, c)}")
        );
        assert_eq!(
            cards[0].parameters[5].expression.as_deref(),
            Some("'C_JUNC*2'")
        );
        assert_eq!(cards[0].parameters[6].expression, None);
        assert_eq!(cards[1].name, "pf");
        assert_eq!(cards[1].parameters[2].expression.as_deref(), Some("70"));
        Ok(())
    }

    #[test]
    fn source_scope_and_comment_lexing_is_non_destructive() -> Result<()> {
        let input = b"* ignored .model bogus vdmos\n.subckt A x y\n.model d1 vdmos (is=2n $ model comment\n+ tt={if(v(a)>0,5n,10n)} cgdmax=3n)\n.ends\n.model d2 vdmos(vto=2.5 cgs=2n)\n";
        let cards = parse_model_cards(input)?;
        assert_eq!(cards.len(), 2);
        assert_eq!(cards[0].source_line, 3);
        assert_eq!(
            cards[0].parameters[1].expression.as_deref(),
            Some("{if(v(a)>0,5n,10n)}")
        );
        assert_eq!(cards[1].parameters.len(), 2);
        Ok(())
    }

    #[test]
    fn malformed_cards_fail_without_partial_results() {
        for broken in [
            ".model x vdmos (vto=3 cgs=2n",
            ".model x vdmos (vto=)",
            ".model x vdmos (cgs={a+b)",
            ".model x vdmos (cgs=\"unterminated)",
            ".model x vdmos (vto=3) stray",
        ] {
            assert!(parse_model_cards(broken.as_bytes()).is_err(), "{broken}");
        }
    }

    #[test]
    fn spice_scales_are_not_si_prefix_guesses() -> Result<()> {
        for (raw, expected) in [
            ("100nF", 1e-7),
            ("1MEGOhm", 1e6),
            ("1m", 1e-3),
            ("2.5MIL", 63.5e-6),
            ("-4.5K", -4500.0),
            ("3.2d-2U", 3.2e-8),
            ("1e3", 1000.0),
            (".5pF", 5e-13),
            ("+6", 6.0),
            ("1a", 1e-18),
            ("1.0e7Hz", 1e7),
            ("100V", 100.0),
            ("45deg", 45.0),
            ("3dB", 3.0),
            ("20rad", 20.0),
            ("10Ohm", 10.0),
        ] {
            let actual = parse_spice_constant(raw)?;
            assert!(
                (actual - expected).abs() < expected.abs().max(1e-18) * 1e-12,
                "{raw}: {actual} != {expected}"
            );
        }
        for malformed in ["", "1e", "{1k*2}", "1x", "NaN", "10$bad", "2.5.5", "1e400"] {
            assert!(parse_spice_constant(malformed).is_err(), "{malformed}");
        }
        Ok(())
    }
}
