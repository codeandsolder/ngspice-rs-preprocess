//! Source-level controlled transfer declarations. TABLE is a static voltage
//! law; FREQ is a complex AC response, not a transient waveform.
use crate::{
    lexical,
    param_expression::{SpiceExpression, SpiceExpressionContext},
};
use anyhow::{Context, Result, bail, ensure};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputKind {
    Voltage,
    Current,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Control {
    Expression(String),
    Differential { positive: String, negative: String },
    Node(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrequencyEncoding {
    Cartesian,
    Polar { decibels: bool, radians: bool },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Transfer {
    Table {
        points: Vec<[String; 2]>,
        multiplier: Option<String>,
    },
    Frequency {
        encoding: FrequencyEncoding,
        points: Vec<[String; 3]>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ControlledTransfer {
    pub name: String,
    pub output: OutputKind,
    pub positive: String,
    pub negative: String,
    pub control: Control,
    pub transfer: Transfer,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ResolvedTransfer {
    Table {
        points: Vec<[f64; 2]>,
        multiplier: f64,
        smoothing_fraction: f64,
    },
    Frequency {
        /// Frequency in Hz, real gain, imaginary gain.
        points: Vec<[f64; 3]>,
    },
}

const fn eq(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

fn scalar(text: &str) -> Result<String> {
    SpiceExpression::parse(text).with_context(|| format!("invalid transfer scalar {text}"))?;
    Ok(text.to_owned())
}

/// Scan point lists without splitting quoted/braced parameter expressions.
/// Parentheses and brackets delimit complete rows or groups of complete rows.
fn points<const N: usize>(text: &str) -> Result<Vec<[String; N]>> {
    let mut values = Vec::new();
    let mut groups = Vec::new();
    let mut start = None;
    let mut braces = 0usize;
    let mut quote = None;
    let mut escaped = false;
    for (index, byte) in text.bytes().enumerate() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == q {
                quote = None;
            }
            continue;
        }
        if matches!(byte, b'\'' | b'"') {
            start.get_or_insert(index);
            quote = Some(byte);
            continue;
        }
        if byte == b'{' {
            start.get_or_insert(index);
            braces += 1;
            continue;
        }
        if byte == b'}' {
            braces = braces
                .checked_sub(1)
                .context("unmatched transfer expression brace")?;
            continue;
        }
        if braces != 0 {
            continue;
        }
        if byte.is_ascii_whitespace() || matches!(byte, b',' | b'(' | b')' | b'[' | b']') {
            if let Some(begin) = start.take() {
                values.push(scalar(&text[begin..index])?);
            }
            match byte {
                b'(' | b'[' => {
                    ensure!(
                        values.len().is_multiple_of(N),
                        "transfer group starts inside a row"
                    );
                    groups.push((byte, values.len()));
                }
                b')' | b']' => {
                    let (open, first) =
                        groups.pop().context("unmatched transfer list delimiter")?;
                    ensure!(
                        matches!((open, byte), (b'(', b')') | (b'[', b']')),
                        "mismatched transfer list delimiter"
                    );
                    ensure!(
                        values.len() > first && (values.len() - first).is_multiple_of(N),
                        "transfer point group must contain complete rows of {N} values"
                    );
                }
                _ => {}
            }
        } else {
            start.get_or_insert(index);
        }
    }
    ensure!(
        quote.is_none() && braces == 0 && groups.is_empty(),
        "unterminated transfer list"
    );
    if let Some(begin) = start {
        values.push(scalar(&text[begin..])?);
    }
    ensure!(
        !values.is_empty() && values.len().is_multiple_of(N),
        "expected complete transfer rows of {N} values"
    );
    let mut rows = Vec::with_capacity(values.len() / N);
    let mut values = values.into_iter();
    while values.len() != 0 {
        // The length invariant above guarantees that every row is complete.
        let mut row = std::array::from_fn(|_| String::new());
        for slot in &mut row {
            *slot = values.next().context("incomplete transfer row")?;
        }
        rows.push(row);
    }
    Ok(rows)
}

fn control_expression(token: &str, allow_node: bool) -> Result<Control> {
    let expression = token
        .strip_prefix('{')
        .and_then(|s| s.strip_suffix('}'))
        .context("expected a braced transfer control expression")?
        .trim();
    ensure!(!expression.is_empty(), "empty transfer control expression");
    if allow_node
        && expression
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        && expression
            .as_bytes()
            .first()
            .is_some_and(|b| !b.is_ascii_digit())
    {
        return Ok(Control::Node(expression.to_owned()));
    }
    Ok(Control::Expression(expression.to_owned()))
}

fn table(
    fields: &[&str],
    control_nodes: bool,
    output: OutputKind,
) -> Result<(Control, Transfer, bool)> {
    let (control, rest) = if control_nodes {
        ensure!(
            output == OutputKind::Voltage,
            "four-node G TABLE is not a native form"
        );
        let [positive, negative, _, rest @ ..] = fields else {
            bail!("missing differential TABLE control");
        };
        (
            Control::Differential {
                positive: (*positive).to_owned(),
                negative: (*negative).to_owned(),
            },
            rest,
        )
    } else {
        let fields = fields.strip_prefix(&["="]).unwrap_or(fields);
        let (expression, rest) = fields.split_first().context("missing TABLE control")?;
        (control_expression(expression, false)?, rest)
    };
    let rest = rest.strip_prefix(&["="]).unwrap_or(rest);
    let (rows, multiplier) = if let [rows @ .., key, "=", value] = rest
        && eq(key, "m")
    {
        ensure!(
            output == OutputKind::Current,
            "multiplier is only valid for G TABLE"
        );
        (rows, Some(scalar(value)?))
    } else {
        (rest, None)
    };
    // Some producers wrap the entire point list in braces after '='.
    // Only strip this wrapper when it contains list syntax, not a scalar.
    let list = rows.join(" ");
    let wrapped = list
        .strip_prefix('{')
        .and_then(|s| s.strip_suffix('}'))
        .filter(|s| s.trim_start().starts_with('('));
    Ok((
        control,
        Transfer::Table {
            points: points(wrapped.unwrap_or(&list))?,
            multiplier,
        },
        wrapped.is_some(),
    ))
}

fn frequency(fields: &[&str]) -> Result<(Control, Transfer)> {
    let fields = fields.strip_prefix(&["="]).unwrap_or(fields);
    let (expression, rest) = fields.split_first().context("missing FREQ control")?;
    let control = control_expression(expression, true)?;
    let mut rest = rest.strip_prefix(&["="]).unwrap_or(rest);
    let mut decibels = true;
    let mut radians = false;
    let mut cartesian = false;
    while let Some((keyword, tail)) = rest.split_first() {
        let keyword = keyword
            .strip_prefix('{')
            .and_then(|s| s.strip_suffix('}'))
            .unwrap_or(keyword);
        if eq(keyword, "mag") {
            decibels = false;
        } else if eq(keyword, "db") {
            decibels = true;
        } else if eq(keyword, "rad") {
            radians = true;
        } else if eq(keyword, "deg") {
            radians = false;
        } else if eq(keyword, "r_i") {
            cartesian = true;
        } else {
            break;
        }
        rest = tail;
    }
    let encoding = if cartesian {
        FrequencyEncoding::Cartesian
    } else {
        FrequencyEncoding::Polar { decibels, radians }
    };
    Ok((
        control,
        Transfer::Frequency {
            encoding,
            points: points(&rest.join(" "))?,
        },
    ))
}

/// Recognize an E/G transfer card without interpreting ordinary node names as
/// keywords.
///
/// Runtime controls remain source expressions; point coordinates are
/// numparam expressions compiled independently at occurrence binding.
/// # Errors
/// Invalid transfer grammar, ambiguous hybrid syntax or malformed points.
pub fn parse(statement: &str) -> Result<Option<ControlledTransfer>> {
    Ok(parse_with_layout(statement)?.map(|(transfer, _)| transfer))
}

fn parse_with_layout(statement: &str) -> Result<Option<(ControlledTransfer, bool)>> {
    let first = statement
        .trim_start()
        .as_bytes()
        .first()
        .map(u8::to_ascii_lowercase);
    let output = match first {
        Some(b'e') => OutputKind::Voltage,
        Some(b'g') => OutputKind::Current,
        _ => return Ok(None),
    };
    let spans = lexical::tokens(statement.as_bytes())?;
    let fields = spans
        .into_iter()
        .map(|span| &statement[span])
        .collect::<Vec<_>>();
    let [name, positive, third, rest @ ..] = fields.as_slice() else {
        return Ok(None);
    };
    let (negative, control, transfer, wrapped) =
        if eq(third, "freq") && rest.first().is_some_and(|s| s.starts_with('{')) {
            let (control, transfer) = frequency(rest)?;
            ("0", control, transfer, false)
        } else if let Some((keyword, tail)) = rest.split_first() {
            if eq(keyword, "table") && !ordinary_control_tail(tail) {
                let (control, transfer, wrapped) = table(tail, false, output)?;
                (*third, control, transfer, wrapped)
            } else if eq(keyword, "freq") && !ordinary_control_tail(tail) {
                let (control, transfer) = frequency(tail)?;
                (*third, control, transfer, false)
            } else if rest.len() > 3 && rest.get(2).is_some_and(|s| eq(s, "table")) {
                let (control, transfer, wrapped) = table(rest, true, output)?;
                (*third, control, transfer, wrapped)
            } else {
                return Ok(None);
            }
        } else {
            return Ok(None);
        };
    validate_known(&transfer)?;
    Ok(Some((
        ControlledTransfer {
            name: (*name).to_owned(),
            output,
            positive: (*positive).to_owned(),
            negative: negative.to_owned(),
            control,
            transfer,
        },
        wrapped,
    )))
}

#[derive(Debug)]
struct RequiresBinding;

impl std::fmt::Display for RequiresBinding {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("transfer expression requires occurrence binding")
    }
}

impl std::error::Error for RequiresBinding {}

impl SpiceExpressionContext for RequiresBinding {
    fn symbol(&mut self, _name: &str) -> Result<f64> {
        Err(Self.into())
    }

    fn function(&mut self, _name: &str, _args: &[SpiceExpression]) -> Result<f64> {
        Err(Self.into())
    }
}

fn known_scalar(text: &str) -> Result<Option<f64>> {
    match SpiceExpression::parse(text)?.evaluate_with(&mut RequiresBinding) {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.downcast_ref::<RequiresBinding>().is_some() => Ok(None),
        Err(error) => Err(error).with_context(|| format!("invalid transfer scalar {text}")),
    }
}

fn known_points<const N: usize>(rows: &[[String; N]]) -> Result<Vec<[Option<f64>; N]>> {
    let mut result = Vec::with_capacity(rows.len());
    let mut previous = None;
    for row in rows {
        let mut values = [None; N];
        for (slot, text) in values.iter_mut().zip(row) {
            *slot = known_scalar(text)?;
        }
        if let Some(x) = values[0] {
            ensure!(
                previous.is_none_or(|last| last < x),
                "known transfer abscissas must strictly increase"
            );
            previous = Some(x);
        }
        result.push(values);
    }
    Ok(result)
}

fn validate_known(transfer: &Transfer) -> Result<()> {
    match transfer {
        Transfer::Table { points, multiplier } => {
            known_points(points)?;
            if let Some(value) = multiplier {
                known_scalar(value)?;
            }
        }
        Transfer::Frequency { encoding, points } => {
            for [frequency, a, _] in known_points(points)? {
                ensure!(
                    frequency.is_none_or(|value| value >= 0.),
                    "negative transfer frequency"
                );
                if let FrequencyEncoding::Polar { decibels, .. } = encoding
                    && let Some(value) = a
                {
                    let magnitude = if *decibels {
                        10_f64.powf(value / 20.)
                    } else {
                        value
                    };
                    ensure!(
                        magnitude >= 0. && magnitude.is_finite(),
                        "invalid transfer magnitude"
                    );
                }
            }
        }
    }
    Ok(())
}

fn ordinary_control_tail(tail: &[&str]) -> bool {
    tail.len() >= 2
        && !tail[0].starts_with(['{', '='])
        && !tail.iter().any(|word| word.starts_with(['(', '[']))
}
fn finite(text: &str, resolve: &mut impl FnMut(&str) -> Result<f64>) -> Result<f64> {
    let value = resolve(text).with_context(|| format!("resolving transfer scalar {text}"))?;
    ensure!(value.is_finite(), "non-finite transfer scalar {text}");
    Ok(value)
}

fn resolve_points<const N: usize>(
    points: &[[String; N]],
    resolve: &mut impl FnMut(&str) -> Result<f64>,
) -> Result<Vec<[f64; N]>> {
    ensure!(!points.is_empty(), "empty transfer");
    let mut output: Vec<[f64; N]> = Vec::with_capacity(points.len());
    for point in points {
        let mut row = [0.; N];
        for (slot, text) in row.iter_mut().zip(point) {
            *slot = finite(text, resolve)?;
        }
        ensure!(
            output.last().is_none_or(|previous| previous[0] < row[0]),
            "transfer abscissas must strictly increase"
        );
        output.push(row);
    }
    Ok(output)
}

impl ControlledTransfer {
    /// Bind point expressions in the lexical scope of an elaborated instance.
    /// Complex frequency interpolation operates on Cartesian gains in Hz.
    /// TABLE retains the native endpoint/smoothing policy for solver evaluation.
    /// # Errors
    /// Non-finite or unordered points, unresolved expressions, invalid frequency
    /// or magnitude data, or unrepresentable complex gain.
    pub fn resolve(
        &self,
        mut resolve: impl FnMut(&str) -> Result<f64>,
    ) -> Result<ResolvedTransfer> {
        match &self.transfer {
            Transfer::Table { points, multiplier } => {
                let points = resolve_points(points, &mut resolve)?;
                let multiplier = multiplier
                    .as_deref()
                    .map_or(Ok(1.), |text| finite(text, &mut resolve))?;
                let smoothing_fraction = if matches!(self.control, Control::Differential { .. }) {
                    0.001
                } else {
                    0.1
                };
                Ok(ResolvedTransfer::Table {
                    points,
                    multiplier,
                    smoothing_fraction,
                })
            }
            Transfer::Frequency { points, encoding } => {
                let mut points = resolve_points(points, &mut resolve)?;
                for [frequency, a, b] in &mut points {
                    ensure!(*frequency >= 0., "negative transfer frequency");
                    if let FrequencyEncoding::Polar { decibels, radians } = *encoding {
                        let magnitude = if decibels { 10_f64.powf(*a / 20.) } else { *a };
                        ensure!(
                            magnitude >= 0. && magnitude.is_finite(),
                            "invalid transfer magnitude"
                        );
                        let phase = if radians { *b } else { b.to_radians() };
                        let (sin, cos) = phase.sin_cos();
                        *a = magnitude * cos;
                        *b = magnitude * sin;
                        ensure!(
                            a.is_finite() && b.is_finite(),
                            "non-finite complex transfer gain"
                        );
                    }
                }
                Ok(ResolvedTransfer::Frequency { points })
            }
        }
    }
}

fn native_scalar(value: &str) -> std::borrow::Cow<'_, str> {
    if crate::model_card::parse_spice_constant(value).is_ok() || value.starts_with('{') {
        std::borrow::Cow::Borrowed(value)
    } else {
        let expression = value
            .strip_prefix('\'')
            .and_then(|s| s.strip_suffix('\''))
            .unwrap_or(value);
        std::borrow::Cow::Owned(format!("{{{expression}}}"))
    }
}

fn table_needs_rewrite(points: &[[String; 2]], multiplier: Option<&str>) -> bool {
    points
        .iter()
        .flatten()
        .map(String::as_str)
        .chain(multiplier)
        .any(|value| native_scalar(value) != value)
}

fn emit_table(
    source: &ControlledTransfer,
    points: &[[String; 2]],
    multiplier: Option<&str>,
) -> Result<String> {
    use std::fmt::Write;
    let mut text = format!("{} {} {}", source.name, source.positive, source.negative);
    match &source.control {
        Control::Expression(expression) => {
            write!(text, " TABLE {{{expression}}}=")?;
            for [x, y] in points {
                write!(text, "({},{})", native_scalar(x), native_scalar(y))?;
            }
        }
        Control::Differential { positive, negative } => {
            write!(text, " {positive} {negative} TABLE=(")?;
            for (index, value) in points.iter().flatten().enumerate() {
                if index > 0 {
                    text.push(',');
                }
                text.push_str(&native_scalar(value));
            }
            text.push(')');
        }
        Control::Node(_) => bail!("TABLE requires an expression or differential control"),
    }
    if let Some(value) = multiplier {
        write!(text, " m={}", native_scalar(value))?;
    }
    Ok(text)
}

/// The native xfer model is an AC-only transfer; DC uses its first real ordinate.
fn emit_frequency(
    source: &ControlledTransfer,
    encoding: FrequencyEncoding,
    points: &[[String; 3]],
    used: &mut std::collections::BTreeSet<Vec<u8>>,
    serial: usize,
) -> Result<String> {
    use std::fmt::Write;
    let mut fresh = |base: &[u8]| -> Result<String> {
        let name = super::fresh_identifier(used, base).context("transfer identifier overflow")?;
        Ok(String::from_utf8(name)?)
    };
    let model = fresh(format!("__freq_{serial}_model").as_bytes())?;
    let device = fresh(format!("A__freq_{serial}").as_bytes())?;
    let output = fresh(format!("__freq_{serial}_out").as_bytes())?;
    let mut text = String::new();
    // A voltage-controlled unity source keeps the original electrical branch
    // identity (including I(E...)) and G's output-current polarity.
    writeln!(
        text,
        "{} {} {} {output} 0 1",
        source.name, source.positive, source.negative
    )?;
    let input = match &source.control {
        Control::Node(name) => name.clone(),
        Control::Expression(expression) => {
            let node = fresh(format!("__freq_{serial}_in").as_bytes())?;
            let driver = fresh(format!("B__freq_{serial}").as_bytes())?;
            writeln!(text, "{driver} {node} 0 V={{{expression}}}")?;
            node
        }
        Control::Differential { .. } => bail!("FREQ control must be an expression or node"),
    };
    writeln!(text, "{device} %v({input}) %v({output}) {model}")?;
    write!(text, ".model {model} xfer ")?;
    match encoding {
        FrequencyEncoding::Cartesian => text.push_str("r_i=true"),
        FrequencyEncoding::Polar { decibels, radians } => {
            write!(
                text,
                "db={} rad={}",
                if decibels { "true" } else { "false" },
                if radians { "true" } else { "false" }
            )?;
        }
    }
    text.push_str(" table=[");
    for (i, value) in points.iter().flatten().enumerate() {
        if i != 0 {
            text.push(' ');
        }
        text.push_str(&native_scalar(value));
    }
    text.push(']');
    Ok(text)
}

/// Native source transformation with one physical input-line reference for
/// each additional generated line. Existing physical continuations become
/// comments, so the caller can preserve its source map.
#[derive(Debug)]
pub struct LoweredSources {
    pub bytes: Vec<u8>,
    pub inserted_lines: Vec<usize>,
}

/// Validate native TABLE cards without altering their electrical semantics,
/// and explicitly lower FREQ to native xfer with independent encoding flags.
///
/// Control commands, ordinary E/G cards and comments are not transformed.
/// # Errors
/// Malformed transfer cards, invalid text, or identifier allocation failure.
pub fn lower_sources(input: &[u8]) -> Result<Option<LoweredSources>> {
    if !input.windows(4).any(|w| w.eq_ignore_ascii_case(b"freq"))
        && !input.windows(5).any(|w| w.eq_ignore_ascii_case(b"table"))
    {
        return Ok(None);
    }
    let mut used = None;
    let mut edits = Vec::new();
    let mut inserted_lines = Vec::new();
    let mut in_control = false;
    for statement in lexical::statements(input)? {
        let text = &statement.text;
        let first = text
            .split(u8::is_ascii_whitespace)
            .next()
            .unwrap_or_default();
        if first.eq_ignore_ascii_case(b".control") {
            in_control = true;
        } else if first.eq_ignore_ascii_case(b".endc") {
            in_control = false;
        }
        if in_control || !matches!(first.first(), Some(b'e' | b'E' | b'g' | b'G')) {
            continue;
        }
        if !text.windows(4).any(|w| w.eq_ignore_ascii_case(b"freq"))
            && !text.windows(5).any(|w| w.eq_ignore_ascii_case(b"table"))
        {
            continue;
        }
        let source =
            std::str::from_utf8(text).context("controlled transfer source is not UTF-8")?;
        let Some((transfer, wrapped)) = parse_with_layout(source)
            .with_context(|| format!("line {}: controlled transfer", statement.line))?
        else {
            continue;
        };
        let generated = match &transfer.transfer {
            Transfer::Table { points, multiplier } => {
                if !wrapped && !table_needs_rewrite(points, multiplier.as_deref()) {
                    continue;
                }
                emit_table(&transfer, points, multiplier.as_deref())?
            }
            Transfer::Frequency { encoding, points } => emit_frequency(
                &transfer,
                *encoding,
                points,
                used.get_or_insert_with(|| super::source_identifiers(input)),
                statement.line,
            )?,
        };
        let physical = input[statement.start..]
            .split_inclusive(|b| *b == b'\n')
            .next()
            .unwrap_or_default();
        let (_, ending) = lexical::split_line_ending(physical);
        let newline = if ending == b"\r\n" { "\r\n" } else { "\n" };
        let extra = generated.bytes().filter(|b| *b == b'\n').count();
        inserted_lines.extend(std::iter::repeat_n(statement.line, extra));
        let generated = generated.replace('\n', newline);
        lexical::replace_span(&statement, 0..text.len(), generated.as_bytes(), &mut edits);
        for (plus, _) in &statement.continuations {
            edits.push((*plus..*plus + 1, vec![b'*']));
        }
    }
    Ok(
        lexical::apply_edits(input, edits).map(|bytes| LoweredSources {
            bytes,
            inserted_lines,
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{linked_source::RootKind, normalize_mapped, normalize_ngspice};
    use std::{fs, process::Command};

    fn parse_card(input: &str) -> Result<ControlledTransfer> {
        parse(input)?.context("expected controlled transfer")
    }

    fn literal(text: &str) -> Result<f64> {
        SpiceExpression::parse(text)?.evaluate(|name| bail!("unbound parameter {name}"))
    }

    fn close(actual: f64, expected: f64) {
        assert!((actual - expected).abs() < 1e-8, "{actual} != {expected}");
    }

    #[test]
    fn distinct_table_forms_preserve_parameterized_control_and_policy() -> Result<()> {
        let expression = parse_card(
            "Gtab out ref TABLE={V(a,b)+I(Vprobe)} = (({lo},0) ({hi + 1},{gain*2})) m={m}",
        )?;
        assert_eq!(
            expression.control,
            Control::Expression("V(a,b)+I(Vprobe)".into())
        );
        assert_eq!(expression.output, OutputKind::Current);
        let resolved = expression.resolve(|text| {
            SpiceExpression::parse(text)?.evaluate(|name| {
                Ok(match name {
                    "lo" => -1.,
                    "hi" => 1.,
                    "gain" => 3.,
                    "m" => 2.,
                    _ => bail!("unexpected symbol"),
                })
            })
        })?;
        let ResolvedTransfer::Table {
            points,
            multiplier,
            smoothing_fraction,
        } = resolved
        else {
            bail!("expected table");
        };
        assert_eq!(points, [[-1., 0.], [2., 6.]]);
        close(multiplier, 2.);
        close(smoothing_fraction, 0.1);
        let differential = parse_card("Etab out ref cp cn TABLE=(-1,0,2,6)")?;
        assert_eq!(
            differential.control,
            Control::Differential {
                positive: "cp".into(),
                negative: "cn".into()
            }
        );
        let ResolvedTransfer::Table {
            points,
            smoothing_fraction,
            ..
        } = differential.resolve(literal)?
        else {
            bail!("expected table");
        };
        assert_eq!(points, [[-1., 0.], [2., 6.]]);
        close(smoothing_fraction, 0.001);
        assert!(parse("Etab p n TABLE {V(a)} = (1,3) (1,4)").is_err());
        let one = parse_card("Etab p n TABLE {V(a)} = (1,3)")?;
        assert!(
            matches!(one.resolve(literal)?, ResolvedTransfer::Table { points, .. } if points.len() == 1)
        );
        Ok(())
    }

    #[test]
    fn rejects_hybrids_without_treating_ordinary_nodes_as_keywords() -> Result<()> {
        for card in [
            "E1 p n cp cn TABLE {V(a)}=(0,0)(1,2)",
            "G1 p n cp cn TABLE=(0,0,1,2)",
            "E1 p n TABLE {V(a)}=(0)(0,1,2)",
            "E1 p n TABLE {}=(0,0)(1,1)",
            "E1 p n TABLE {V(a)}=(0,0) m=2",
            "E1 p n FREQ {V(a)} MAG (0,1)",
        ] {
            assert!(parse(card).is_err(), "{card}");
        }
        for card in [
            "E1 p n table cn 2",
            "G1 p n freq cn 2",
            "E1 p n cp cn table",
            "E1 p freq cp cn 1",
            "E1 p n cp cn 1",
        ] {
            assert!(parse(card)?.is_none(), "{card}");
        }
        Ok(())
    }

    #[test]
    fn frequency_formats_resolve_to_cartesian_points() -> Result<()> {
        for card in [
            "E1 out 0 FREQ {in} MAG RAD (10Hz,2,0)(20Hz,2,1.5707963267948966)",
            "E1 out 0 FREQ {in} DB DEG (10,6.020599913279624,0)(20,6.020599913279624,90)",
            "E1 out 0 FREQ {in} R_I (10,2,0)(20,0,2)",
        ] {
            let card = parse_card(card)?;
            assert_eq!(card.control, Control::Node("in".into()));
            let ResolvedTransfer::Frequency { points } = card.resolve(literal)? else {
                bail!("expected frequency response");
            };
            for (actual, expected) in points.iter().flatten().zip([10., 2., 0., 20., 0., 2.]) {
                close(*actual, expected);
            }
        }
        assert!(parse("E1 out 0 FREQ {in} R_I (10,1,0)(9,1,0)").is_err());
        assert!(parse("E1 out 0 FREQ {in} MAG (10,-1,0)").is_err());
        assert!(parse("E1 out 0 FREQ {in} R_I (-1,1,0)").is_err());
        Ok(())
    }

    #[test]
    fn lowering_preserves_comments_source_map_and_branch_names() -> Result<()> {
        let input = b"title\r\nL1 a b 1u Rser=1\r\nE1 out 0 FREQ {v(a,b)*2}=MAG RAD\r\n+ (10,2,0)\r\n+ (20,2,1.57) ; tail\r\nR1 __freq_3_out 0 1k\r\nE2 q 0 TABLE {v(a)}=(0,0)(1,1)\r\n.end\r\n";
        let result = normalize_mapped(input, RootKind::Deck, crate::NormalizationDialect::Native)?;
        let text = std::str::from_utf8(&result.model.bytes)?;
        assert!(text.contains("E1 out 0 __freq_3_out_1 0 1\r\n"));
        assert!(text.contains("db=false rad=true table=[10 2 0 20 2 1.57]"));
        assert!(text.contains("E2 q 0 TABLE {v(a)}=(0,0)(1,1)"));
        assert!(text.contains("; tail"));
        assert_eq!(result.input_lines, [1, 2, 2, 3, 3, 3, 3, 4, 5, 6, 7, 8]);
        let repeated = normalize_mapped(
            &result.model.bytes,
            RootKind::Deck,
            crate::NormalizationDialect::Native,
        )?;
        assert_eq!(repeated.model.bytes, result.model.bytes);
        assert_eq!(
            normalize_ngspice(b"E1 out 0 TABLE {v(a)}=(0,0)(1,1)")?.bytes,
            b"E1 out 0 TABLE {v(a)}=(0,0)(1,1)"
        );
        Ok(())
    }

    #[test]
    fn table_symbolic_coordinates_are_native_numparam_values() -> Result<()> {
        let input = b"Gtab p 0 TABLE {v(in)}=(lo,0)(hi,gain) m=mult";
        let output = normalize_ngspice(input)?;
        assert_eq!(
            output.bytes,
            b"Gtab p 0 TABLE {v(in)}=({lo},0)({hi},{gain}) m={mult}"
        );
        assert!(parse("E1 p 0 TABLE {v(in)}=(0,1)(1,2)(0,3)").is_err());
        assert!(parse("E1 p 0 TABLE {v(in)}=0 (1,2) 3").is_err());
        Ok(())
    }

    #[test]
    fn native_xfer_reference_complex_interpolation_and_current_polarity() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = b"Frequency transfer reference\n.param f0=100 f1=300 gain=2\nV1 in 0 dc 1 ac 1\nV2 ref 0 dc 0.25 ac 0.25\nEout out 0 FREQ {V(in,ref)*2} MAG RAD ({f0},{gain},0) ({f1},{gain},1.5707963267948966)\nRload out 0 1k\nGout gout 0 FREQ {V(in,ref)*2} R_I (f0,gain,0) (f1,0,gain)\nRg gout 0 1\n.control\nset wr_singlescale\nset wr_vecnames\nac lin 5 50 350\nwrdata response.dat v(out) v(gout)\nquit\n.endc\n.end\n";
        let normalized =
            normalize_mapped(source, RootKind::Deck, crate::NormalizationDialect::Native)?;
        fs::write(
            directory.path().join("reference.cir"),
            normalized.model.bytes,
        )?;
        let output = Command::new("ngspice")
            .args(["-b", "reference.cir"])
            .current_dir(directory.path())
            .output();
        let output = match output {
            Ok(output) => output,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        ensure!(
            output.status.success(),
            "ngspice: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let data = fs::read_to_string(directory.path().join("response.dat"))?;
        let mut count = 0;
        for row in data.lines().skip(1) {
            let values = row
                .split_whitespace()
                .map(str::parse::<f64>)
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let [frequency, re, im, gre, gim] = values.as_slice() else {
                bail!("invalid reference row");
            };
            let fraction = ((frequency - 100.) / 200.).clamp(0., 1.);
            close(*re, 3. * (1. - fraction));
            close(*im, 3. * fraction);
            close(*gre, -*re);
            close(*gim, -*im);
            count += 1;
        }
        assert_eq!(count, 5);
        Ok(())
    }
}
