//! Source-only `.model` statement recognition for plaintext SPICE libraries.
//!
//! This is the lexical front-end, not a solver expression interpreter. Raw
//! expressions stay intact for a later parameter resolver. Keeping that
//! distinction avoids silently evaluating vendor symbols with wrong defaults.

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::ops::Range;

/// A model parameter in original source order; duplicates are deliberately
/// retained for source-dialect resolution rather than guessed here.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ModelParameter {
    /// Case-preserved identifier.
    pub name: String,
    /// Original expression, or `None` for a bare model flag.
    pub expression: Option<String>,
    /// Offsets relative to the logical source supplied to this parser.
    pub span: FieldSpan,
}

impl ModelParameter {
    fn from_span(statement: &str, span: FieldSpan) -> Self {
        Self {
            name: statement[span.name.clone()].to_owned(),
            expression: span
                .value
                .as_ref()
                .map(|value| statement[value.clone()].to_owned()),
            span,
        }
    }
}

/// Lexical field spans in the original logical statement.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FieldSpan {
    /// Case-preserved field identifier or bare positional token.
    pub name: Range<usize>,
    /// Exact value, including quotes or nested delimiters.
    pub value: Option<Range<usize>>,
    /// Whether the source explicitly separates name and value with an equals sign.
    pub explicit_equals: bool,
}

/// How a model parameter list terminates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModelTermination {
    /// Require the closing model wrapper, if present.
    Complete,
    /// The caller has identified a dialect-specific terminator and supplies only its prefix.
    DialectPrefix,
}

/// One lexed `.model` statement. This is deliberately model-family agnostic.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ModelCard {
    /// Declared model identifier.
    pub name: String,
    /// Declared model type (e.g. `vdmos`, `vdmosp`, `nmos`).
    pub family: String,
    /// Named parameter tokens, preserving expression text and ordering.
    pub parameters: Vec<ModelParameter>,
    /// 1-based physical source line where the `.model` begins, or zero
    /// when only a projected logical statement is available.
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
///
/// # Errors
/// Invalid names, missing assignments or malformed expression values.
pub fn parse_assignments(input: &str) -> Result<Vec<ModelParameter>> {
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
        let field = source.field(&[])?;
        ensure!(
            field.explicit_equals,
            "parameter {} requires name=value",
            &input[field.name]
        );
        parsed.push(ModelParameter::from_span(input, field));
    }
    Ok(parsed)
}

/// Read positional tokens up to the first named parameter or PARAMS: marker.
///
/// Parenthesized terminal lists and commas use the same token boundaries as
/// native ngspice. Return the unconsumed assignment text.
#[must_use]
pub fn positional_prefix(input: &str) -> (Vec<(&str, usize)>, &str) {
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
/// The caller provides the maximum arity of an unparenthesized IC vector.
///
/// # Errors
/// Malformed assignments or unbalanced expressions.
pub fn instance_fields(input: &str, ic_arity: usize) -> Result<Vec<ModelParameter>> {
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
        let mut field = source.field(&[])?;
        // Keep a family-sized unparenthesized IC vector; a balanced
        // parenthesized value already contains the whole vector.
        if input[field.name.clone()].eq_ignore_ascii_case("ic")
            && let Some(value) = &mut field.value
            && !input[value.clone()].starts_with('(')
        {
            for _ in 1..ic_arity {
                let end = value.end;
                source.skip_space();
                let first = input.as_bytes().get(source.offset).copied();
                if first == Some(b',') {
                    source.offset += 1;
                    source.skip_space();
                    source.value()?;
                } else if first.is_some_and(|b| {
                    b.is_ascii_digit() || matches!(b, b'+' | b'-' | b'.' | b'{' | b'\'' | b'(')
                }) {
                    source.value()?;
                } else {
                    source.offset = end;
                    break;
                }
                value.end = source.offset;
            }
        }
        parsed.push(ModelParameter::from_span(input, field));
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
        self.token(false)
    }

    fn token(&mut self, stop_at_equals: bool) -> Result<&'a str> {
        let start = self.offset;
        let mut quote = None;
        let mut escaped = false;
        let mut stack = Vec::new();
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
                if stack.is_empty()
                    && (byte.is_ascii_whitespace()
                        || matches!(byte, b',' | b')' | b';')
                        || stop_at_equals && byte == b'=')
                {
                    break;
                }
                match byte {
                    b'\'' | b'"' => quote = Some(byte),
                    b'(' | b'{' | b'[' => stack.push(byte),
                    b')' | b'}' | b']' => {
                        let expected = match byte {
                            b')' => b'(',
                            b'}' => b'{',
                            _ => b'[',
                        };
                        ensure!(
                            stack.pop() == Some(expected),
                            "mismatched parameter delimiters"
                        );
                    }
                    _ => {}
                }
            }
            self.offset += 1;
        }
        ensure!(
            quote.is_none() && stack.is_empty(),
            "unterminated quote or nested parameter expression"
        );
        ensure!(start != self.offset, "empty parameter token");
        Ok(&self.text[start..self.offset])
    }

    fn field(&mut self, positional: &[&str]) -> Result<FieldSpan> {
        let name_start = self.offset;
        let name = self.token(true)?;
        let name_end = self.offset;
        self.skip_space();
        let explicit_equals = self.text.as_bytes().get(self.offset) == Some(&b'=');
        let value =
            if explicit_equals || positional.iter().any(|key| name.eq_ignore_ascii_case(key)) {
                if explicit_equals {
                    self.offset += 1;
                    self.skip_space();
                }
                let start = self.offset;
                self.token(true)
                    .with_context(|| format!("parameter {name}"))?;
                let end = self.offset;
                self.skip_space();
                ensure!(
                    self.text.as_bytes().get(self.offset) != Some(&b'='),
                    "parameter {name} is missing a value before the next assignment"
                );
                Some(start..end)
            } else {
                None
            };
        Ok(FieldSpan {
            name: name_start..name_end,
            value,
            explicit_equals,
        })
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
    let (name, family, mut source) =
        model_header_source(statement).with_context(|| format!("line {source_line}"))?;
    let parameters = field_spans(&mut source, positional, ModelTermination::Complete)
        .with_context(|| format!("line {source_line}: model {name}"))?
        .into_iter()
        .map(|field| ModelParameter::from_span(statement, field))
        .collect();
    Ok(ModelCard {
        name: name.to_owned(),
        family: family.to_owned(),
        parameters,
        source_line,
    })
}

/// Parse a joined SPICE assignment/flag list with the model-card grammar.
///
/// Positional value names are explicit at the call site. Source ranges are
/// relative to the supplied assignment list.
///
/// # Errors
/// Invalid fields, missing values or unbalanced delimiters.
pub fn parse_parameter_fields(input: &str, positional: &[&str]) -> Result<Vec<ModelParameter>> {
    let mut source = Source {
        text: input,
        offset: 0,
    };
    source.skip_space();
    Ok(
        field_spans(&mut source, positional, ModelTermination::Complete)?
            .into_iter()
            .map(|span| ModelParameter::from_span(input, span))
            .collect(),
    )
}

fn model_header_source(statement: &str) -> Result<(&str, &str, Source<'_>)> {
    let mut source = Source {
        text: statement,
        offset: 0,
    };
    source.skip_space();
    ensure!(
        is_model_start(&statement.as_bytes()[source.offset..]),
        "expected .model statement"
    );
    source.offset += 6;
    source.skip_space();
    let name = source.name();
    ensure!(!name.is_empty(), ".model requires a name");
    source.skip_space();
    let family = source.name();
    ensure!(!family.is_empty(), ".model {name} requires a family");
    source.skip_space();
    Ok((name, family, source))
}

fn field_spans(
    source: &mut Source<'_>,
    positional: &[&str],
    termination: ModelTermination,
) -> Result<Vec<FieldSpan>> {
    let wrapper = source.text.as_bytes().get(source.offset) == Some(&b'(');
    if wrapper {
        source.offset += 1;
    }
    let mut fields = Vec::new();
    loop {
        source.skip_separators();
        let next = source.text.as_bytes().get(source.offset).copied();
        if wrapper && next == Some(b')') {
            source.offset += 1;
            source.skip_space();
            ensure!(
                matches!(source.text.as_bytes().get(source.offset), None | Some(b';')),
                "trailing tokens after model parameter list"
            );
            return Ok(fields);
        }
        if matches!(next, None | Some(b';')) {
            ensure!(
                !wrapper || termination == ModelTermination::DialectPrefix,
                "missing closing model parenthesis"
            );
            return Ok(fields);
        }
        ensure!(next != Some(b')'), "unexpected closing parenthesis");
        fields.push(source.field(positional)?);
    }
}

/// Parse model fields from a joined logical statement, retaining exact source spans.
///
/// For non-model statements, skip the optional model wrapper and scan top-level
/// fields and positional tokens with the same lexical grammar.
///
/// # Errors
/// Malformed assignments, missing selected positional values, and unbalanced
/// delimiters are errors, rather than indistinguishable from absent parameters.
pub fn statement_field_spans(
    statement: &str,
    positional: &[&str],
    termination: ModelTermination,
) -> Result<Vec<FieldSpan>> {
    let trimmed = statement.trim_start();
    if trimmed.starts_with('*') {
        return Ok(Vec::new());
    }
    let mut source = if is_model_start(trimmed.as_bytes()) {
        model_header_source(statement)?.2
    } else {
        Source {
            text: statement,
            offset: 0,
        }
    };
    source.skip_space();
    field_spans(&mut source, positional, termination)
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
    fn model_fields_keep_exact_spans_order_and_dialect_forms() -> Result<()> {
        let source = r#".model LEVEL NMOS (level=7 LEVEL = 17
            text="RG=99, a b" q={max(a, f(b[2], 'RG=4'))} VLEVEL=3
            LIMIT(VDS) = (*, 400) USER [1, 2] flag)"#;
        let card = parse_model_card(source, 2, &["USER"])?;
        let names = card
            .parameters
            .iter()
            .map(|field| field.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "level",
                "LEVEL",
                "text",
                "q",
                "VLEVEL",
                "LIMIT(VDS)",
                "USER",
                "flag"
            ]
        );
        for field in &card.parameters {
            assert_eq!(&source[field.span.name.clone()], field.name);
            assert_eq!(
                field.span.value.as_ref().map(|span| &source[span.clone()]),
                field.expression.as_deref()
            );
        }
        assert_eq!(card.parameters[1].expression.as_deref(), Some("17"));
        assert_eq!(
            card.parameters[2].expression.as_deref(),
            Some(r#""RG=99, a b""#)
        );
        assert_eq!(card.parameters[5].expression.as_deref(), Some("(*, 400)"));
        assert_eq!(card.parameters[6].expression.as_deref(), Some("[1, 2]"));
        assert!(!card.parameters[6].span.explicit_equals);
        assert!(card.parameters[7].expression.is_none());
        Ok(())
    }

    #[test]
    fn standalone_mapping_fields_use_the_model_grammar() -> Result<()> {
        let source = r#"   (DEVICE="mos 1", LIMIT(VDS, VGS) = (*, 400), FLAG, USER [1, {x + y}])"#;
        let fields = parse_parameter_fields(source, &["USER"])?;
        assert_eq!(fields.len(), 4);
        assert_eq!(fields[1].name, "LIMIT(VDS, VGS)");
        assert_eq!(fields[1].expression.as_deref(), Some("(*, 400)"));
        assert_eq!(fields[2].expression, None);
        assert_eq!(fields[3].expression.as_deref(), Some("[1, {x + y}]"));
        for field in fields {
            assert_eq!(&source[field.span.name], field.name);
            assert_eq!(
                field.span.value.map(|span| &source[span]),
                field.expression.as_deref()
            );
        }
        assert!(parse_parameter_fields("DEVICE= LEVEL=2", &[]).is_err());
        Ok(())
    }

    #[test]
    fn malformed_fields_are_not_missing_fields() {
        for source in [
            ".model m D (IS= RS=2)",
            ".model m D (IS= RS = 2)",
            ".model m D (IS={max(1,2}) )",
            ".model m D (IS=[1,2})",
            ".model m D (IS=)",
            ".model m D (USER)",
        ] {
            assert!(parse_model_card(source, 1, &["USER"]).is_err(), "{source}");
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
