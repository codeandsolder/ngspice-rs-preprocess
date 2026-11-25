//! Source waveform normalization and mandatory occurrence-bound forcing.

use crate::{
    fresh_identifier, lexical,
    model_card::parse_spice_constant,
    source_identifiers,
    waveform::{
        LtspicePoint, LtspicePulse, LtspicePwl, LtspiceSine, LtspiceWaveform, PreparedWaveform,
        WaveformSpec,
    },
};
use anyhow::{Context, Result, ensure};
use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Range,
};

type Span = Range<usize>;

/// Separate waveform fields at whitespace or commas outside expression groups.
/// Return the closing parenthesis separately so additional fields can be
/// emitted without disturbing the original physical continuation lines.
fn arguments(text: &[u8], name_end: usize) -> Result<(Vec<Range<usize>>, usize)> {
    let tail = text[name_end..].trim_ascii_start();
    ensure!(
        tail.first() == Some(&b'('),
        "expected waveform argument list"
    );
    let start = text.len() - tail.len() + 1;
    let mut balanced = lexical::Balanced::default();
    let mut fields = Vec::new();
    let mut field_start = None;
    for (offset, &byte) in text[start..].iter().enumerate() {
        let index = start + offset;
        if balanced.top_level() && (byte.is_ascii_whitespace() || matches!(byte, b',' | b')')) {
            if let Some(begin) = field_start.take() {
                fields.push(begin..index);
            }
            if byte == b')' {
                return Ok((fields, index));
            }
        } else {
            field_start.get_or_insert(index);
            balanced.advance(byte)?;
        }
    }
    anyhow::bail!("unterminated waveform arguments")
}

fn literal(text: &[u8]) -> Result<f64> {
    let text = std::str::from_utf8(text)?.trim();
    let grouped = text
        .strip_prefix('{')
        .and_then(|text| text.strip_suffix('}'));
    let expression = grouped.unwrap_or(text);
    parse_spice_constant(expression)
        .or_else(|_| super::constants::evaluate(expression, grouped.is_none()))
        .context("waveform timing requires occurrence-specific parameter binding")
}

fn rewrite_tran(statement: &lexical::Statement, edits: &mut Vec<lexical::Edit>) -> Result<()> {
    let text = &statement.text;
    let fields = lexical::tokens(text)?;
    let mut timing = &fields[1..];
    if timing
        .last()
        .is_some_and(|span| text[span.clone()].eq_ignore_ascii_case(b"uic"))
    {
        timing = &timing[..timing.len() - 1];
    }
    ensure!(
        (1..=4).contains(&timing.len()),
        "unsupported LTspice transient analysis options"
    );
    let values = timing.iter().map(|span| literal(&text[span.clone()])).collect::<Result<Vec<_>>>()
        .context("LTspice transient timing requires bound scalar fields; startup/steady modes require typed analysis")?;
    let (interval, final_time, start, max_step) = if values.len() == 1 {
        (0., values[0], 0., 0.)
    } else {
        (
            values[0],
            values[1],
            values.get(2).copied().unwrap_or(0.).max(0.),
            values.get(3).copied().unwrap_or(0.),
        )
    };
    ensure!(
        final_time > start && max_step >= 0.,
        "LTspice transient needs positive duration and nonnegative maximum step"
    );
    // Vendor adaptive default: exactly stop/1024, regardless of plotting step.
    let max_step = if max_step > 0. {
        max_step
    } else {
        final_time / 1024.
    };
    let interval = if interval > 0. { interval } else { max_step };
    ensure!(
        interval > 0. && max_step > 0.,
        "unrepresentable LTspice transient timestep"
    );
    if timing.len() == 1 {
        let replacement = format!("{interval} {final_time} {start} {max_step}");
        lexical::replace_span(statement, timing[0].clone(), replacement.as_bytes(), edits);
    } else {
        let resolved = [interval, final_time, start, max_step];
        for (index, span) in timing.iter().enumerate() {
            if index + 1 == timing.len() && timing.len() < resolved.len() {
                let replacement = resolved[index..]
                    .iter()
                    .map(f64::to_string)
                    .collect::<Vec<_>>()
                    .join(" ");
                lexical::replace_span(statement, span.clone(), replacement.as_bytes(), edits);
            } else if values[index].to_bits() != resolved[index].to_bits()
                || !native_literal(&text[span.clone()])
            {
                lexical::replace_span(
                    statement,
                    span.clone(),
                    resolved[index].to_string().as_bytes(),
                    edits,
                );
            }
        }
    }
    Ok(())
}

fn native_literal(text: &[u8]) -> bool {
    std::str::from_utf8(text)
        .ok()
        .and_then(|value| parse_spice_constant(value).ok())
        .is_some()
}

fn scalar(text: &[u8], field: &Span) -> Result<String> {
    Ok(std::str::from_utf8(&text[field.clone()])?.to_owned())
}

fn positional(text: &[u8], fields: &[Span], index: usize) -> Result<Option<String>> {
    fields
        .get(index)
        .map(|field| scalar(text, field))
        .transpose()
}

#[derive(Clone, Copy)]
enum Shape {
    Pulse,
    Pwl,
    Sine,
}

impl Shape {
    fn identify(field: &[u8]) -> Option<(Self, usize)> {
        for (keyword, shape) in [
            (b"pulse".as_slice(), Self::Pulse),
            (b"pwl", Self::Pwl),
            (b"sine", Self::Sine),
            (b"sin", Self::Sine),
        ] {
            if field
                .get(..keyword.len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case(keyword))
                && field.get(keyword.len()).is_none_or(|byte| *byte == b'(')
            {
                return Some((shape, keyword.len()));
            }
        }
        None
    }

    fn positional(self, text: &[u8], fields: &[Span]) -> Result<LtspiceWaveform> {
        match self {
            Self::Pulse => {
                ensure!(
                    (5..=8).contains(&fields.len()),
                    "invalid LTspice PULSE arity"
                );
                Ok(LtspiceWaveform::Pulse(LtspicePulse {
                    first: scalar(text, &fields[0])?,
                    second: scalar(text, &fields[1])?,
                    delay: scalar(text, &fields[2])?,
                    rise: scalar(text, &fields[3])?,
                    fall: scalar(text, &fields[4])?,
                    width: positional(text, fields, 5)?,
                    period: positional(text, fields, 6)?,
                    count: positional(text, fields, 7)?,
                }))
            }
            Self::Sine => {
                ensure!(
                    (2..=7).contains(&fields.len()),
                    "invalid LTspice SINE arity"
                );
                Ok(LtspiceWaveform::Sine(LtspiceSine {
                    offset: scalar(text, &fields[0])?,
                    amplitude: scalar(text, &fields[1])?,
                    frequency: positional(text, fields, 2)?,
                    delay: positional(text, fields, 3)?,
                    damping: positional(text, fields, 4)?,
                    phase: positional(text, fields, 5)?,
                    count: positional(text, fields, 6)?,
                }))
            }
            Self::Pwl => unreachable!("PWL uses scale and relative-coordinate grammar"),
        }
    }
}

#[derive(Clone, Copy)]
enum ScaleAxis {
    Time,
    Value,
}
impl ScaleAxis {
    const fn parse(field: &[u8]) -> Option<Self> {
        if field.eq_ignore_ascii_case(b"time_scale_factor") {
            Some(Self::Time)
        } else if field.eq_ignore_ascii_case(b"value_scale_factor") {
            Some(Self::Value)
        } else {
            None
        }
    }
}

fn scale_assignment(text: &[u8], fields: &[Span], index: usize) -> Result<Span> {
    let equals = fields
        .get(index + 1)
        .context("missing PWL scale assignment")?;
    let value = fields.get(index + 2).context("missing PWL scale value")?;
    ensure!(
        &text[equals.clone()] == b"=",
        "expected PWL scale assignment"
    );
    Ok(value.clone())
}

struct ParsedWaveform {
    spec: LtspiceWaveform,
    span: Span,
    ignored: Vec<Span>,
}

fn parse_pwl(text: &[u8], start: usize, name_end: usize) -> Result<ParsedWaveform> {
    let tokens = lexical::tokens(&text[name_end..])?
        .into_iter()
        .map(|span| name_end + span.start..name_end + span.end)
        .collect::<Vec<_>>();
    let mut time_scale = "1".to_owned();
    let mut value_scale = "1".to_owned();
    let mut index = 0;
    loop {
        let field = tokens
            .get(index)
            .context("missing LTspice PWL coordinates")?;
        if text[field.clone()].starts_with(b"(") {
            break;
        }
        let axis = ScaleAxis::parse(&text[field.clone()])
            .context("LTspice structured PWL requires typed repeat/trigger/file lowering")?;
        let value = scalar(text, &scale_assignment(text, &tokens, index)?)?;
        match axis {
            ScaleAxis::Time => time_scale = value,
            ScaleAxis::Value => value_scale = value,
        }
        index += 3;
    }
    let list = &tokens[index];
    let (fields, close) = arguments(text, list.start)?;
    ensure!(
        fields.len() >= 2 && fields.len() % 2 == 0,
        "invalid PWL coordinate pairs"
    );
    let points = fields
        .as_chunks::<2>()
        .0
        .iter()
        .map(|[time, value]| {
            let raw = scalar(text, time)?;
            let relative = raw.starts_with('+');
            Ok(LtspicePoint {
                time: if relative { raw[1..].to_owned() } else { raw },
                relative,
                value: scalar(text, value)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    index += 1;
    let mut ignored = Vec::new();
    // The vendor consumes trailing scale assignments without applying them.
    while let Some(field) = tokens.get(index) {
        let word = &text[field.clone()];
        if ScaleAxis::parse(word).is_some() {
            let value = scale_assignment(text, &tokens, index)?;
            ignored.push(field.start..value.end);
            index += 3;
            continue;
        }
        ensure!(
            ![
                b"repeat".as_slice(),
                b"endrepeat",
                b"trigger",
                b"file",
                b"r",
                b"td"
            ]
            .iter()
            .any(|name| word.eq_ignore_ascii_case(name)),
            "LTspice structured PWL requires typed waveform lowering"
        );
        index += 1;
    }
    Ok(ParsedWaveform {
        spec: LtspiceWaveform::Pwl(LtspicePwl {
            points,
            time_scale,
            value_scale,
        }),
        span: start..close + 1,
        ignored,
    })
}

fn parse_waveform(text: &[u8], fields: &[Span]) -> Result<Option<ParsedWaveform>> {
    for field in fields.iter().skip(3) {
        let Some((shape, length)) = Shape::identify(&text[field.clone()]) else {
            continue;
        };
        let start = field.start;
        let name_end = start + length;
        return if matches!(shape, Shape::Pwl) {
            parse_pwl(text, start, name_end).map(Some)
        } else {
            let (arguments, close) = arguments(text, name_end)?;
            Ok(Some(ParsedWaveform {
                spec: shape.positional(text, &arguments)?,
                span: start..close + 1,
                ignored: Vec::new(),
            }))
        };
    }
    Ok(None)
}

/// Preserve explicit leading or named DC independent of transient forcing.
/// The native source parser gives the leading scalar precedence over DC=.
fn dc_field(text: &[u8], fields: &[Span], wave: &ParsedWaveform) -> Result<Option<Span>> {
    if let Some(field) = fields.get(3)
        && field.end <= wave.span.start
        && ![b"dc".as_slice(), b"ac", b"distof1", b"distof2"]
            .iter()
            .any(|name| text[field.clone()].eq_ignore_ascii_case(name))
        && fields.get(4).is_none_or(|next| &text[next.clone()] != b"=")
    {
        return Ok(Some(field.clone()));
    }
    let mut dc = None;
    for (index, field) in fields.iter().enumerate().skip(3) {
        if wave.span.contains(&field.start)
            || wave.ignored.iter().any(|span| span.contains(&field.start))
        {
            continue;
        }
        if text[field.clone()].eq_ignore_ascii_case(b"dc") {
            let mut value = index + 1;
            if fields
                .get(value)
                .is_some_and(|next| &text[next.clone()] == b"=")
            {
                value += 1;
            }
            dc = Some(
                fields
                    .get(value)
                    .context("missing explicit source DC value")?
                    .clone(),
            );
        }
    }
    Ok(dc)
}

struct Bind<'a> {
    used: &'a mut BTreeSet<Vec<u8>>,
    aliases: BTreeMap<String, String>,
    declarations: Vec<u8>,
}

impl Bind<'_> {
    fn scalar(&mut self, raw: &str) -> Result<String> {
        if let Ok(value) = parse_spice_constant(raw) {
            return Ok(value.to_string());
        }
        if let Some(value) = super::constants::try_evaluate(raw, true)? {
            return Ok(value.to_string());
        }
        // Put the expression into its actual declaration scope. All later
        // dialect-specific operator/macro lowering applies to this .param,
        // and the mandatory metadata refers only to the native bound name.
        let expression = crate::param_expression::lower_ltspice(raw, true)?
            .replace('{', "(")
            .replace('}', ")");
        if let Some(alias) = self.aliases.get(&expression) {
            return Ok(alias.clone());
        }
        let name = fresh_identifier(self.used, b"ltprep_waveform")
            .context("LTspice waveform parameter identifier overflow")?;
        let name = String::from_utf8(name)?;
        let line = format!(".param {name}={{{expression}}}\n");
        self.declarations.extend_from_slice(line.as_bytes());
        self.aliases.insert(expression, name.clone());
        Ok(name)
    }
}

struct SourcePass<'a> {
    body: &'a [u8],
    artifact: &'a str,
    used: BTreeSet<Vec<u8>>,
    edits: Vec<lexical::Edit>,
    inserted: BTreeMap<usize, usize>,
    waveforms: Vec<PreparedWaveform>,
    scope: Vec<String>,
}

impl SourcePass<'_> {
    fn source(&mut self, statement: &lexical::Statement) -> Result<()> {
        let text = &statement.text;
        let fields = lexical::tokens(text)?;
        let Some(mut parsed) = parse_waveform(text, &fields)? else {
            return Ok(());
        };
        let dc = dc_field(text, &fields, &parsed)?;
        let mut bind = Bind {
            used: &mut self.used,
            aliases: BTreeMap::new(),
            declarations: Vec::new(),
        };
        parsed.spec.map_scalars(|raw| bind.scalar(raw))?;
        let reference = if bind.aliases.is_empty() {
            parsed.spec.native_reference(parse_spice_constant)?
        } else {
            None
        };
        let mut replacements = parsed
            .ignored
            .iter()
            .map(|span| {
                let start = text[..span.start].trim_ascii_end().len();
                (start..span.end, Vec::new())
            })
            .collect::<Vec<_>>();
        if let Some(reference) = reference {
            replacements.push((parsed.span, reference.into_bytes()));
        } else {
            let dc_override = dc
                .as_ref()
                .map(|field| bind.scalar(std::str::from_utf8(&text[field.clone()])?))
                .transpose()?;
            if let Some((field, value)) = dc.zip(dc_override.as_ref()) {
                let replacement = if parse_spice_constant(value).is_ok() {
                    value.clone()
                } else {
                    format!("{{{value}}}")
                };
                replacements.push((field, replacement.into_bytes()));
            }
            let name = scalar(text, &fields[0])?;
            let declaration = PreparedWaveform {
                artifact: self.artifact.to_owned(),
                scope: self.scope.clone(),
                source: name.clone(),
                spec: WaveformSpec::Ltspice(parsed.spec),
                idle_in_pop: "0".to_owned(),
                dc_override,
            };
            let initial = if bind.aliases.is_empty() {
                declaration.resolve(parse_spice_constant)?.dc
            } else {
                0.
            };
            let replacement = if declaration.dc_override.is_some() {
                Vec::new()
            } else {
                format!("DC {initial}").into_bytes()
            };
            replacements.push((parsed.span, replacement));
            bind.declarations.extend_from_slice(
                format!("* mandatory prepared waveform binding for {name}\n").as_bytes(),
            );
            let count = bind
                .declarations
                .split_inclusive(|byte| *byte == b'\n')
                .count();
            self.inserted.insert(statement.line, count);
            self.edits
                .push((statement.start..statement.start, bind.declarations));
            self.waveforms.push(declaration);
        }
        lexical::replace_spans(statement, self.body, &replacements, &mut self.edits);
        Ok(())
    }
}

pub(super) struct SourceRewrite {
    pub bytes: Vec<u8>,
    pub origins: Vec<usize>,
    pub waveforms: Vec<PreparedWaveform>,
}

pub(super) fn rewrite(
    body: &[u8],
    artifact: &str,
    origins: &[usize],
) -> Result<Option<SourceRewrite>> {
    let mut pass = SourcePass {
        body,
        artifact,
        used: source_identifiers(body),
        edits: Vec::new(),
        inserted: BTreeMap::new(),
        waveforms: Vec::new(),
        scope: Vec::new(),
    };
    let mut control = false;
    for statement in lexical::statements(body)? {
        let first = statement
            .text
            .split(u8::is_ascii_whitespace)
            .next()
            .unwrap_or_default();
        if first.eq_ignore_ascii_case(b".control") {
            control = true;
            continue;
        }
        if first.eq_ignore_ascii_case(b".endc") {
            control = false;
            continue;
        }
        if control {
            continue;
        }
        if first.eq_ignore_ascii_case(b".subckt") {
            let name = statement
                .text
                .split(u8::is_ascii_whitespace)
                .nth(1)
                .context("missing subcircuit name")?;
            pass.scope.push(std::str::from_utf8(name)?.to_owned());
        } else if first.eq_ignore_ascii_case(b".ends") {
            ensure!(pass.scope.pop().is_some(), "unmatched .ends");
        } else if first.eq_ignore_ascii_case(b".tran") {
            rewrite_tran(&statement, &mut pass.edits)?;
        } else if matches!(first.first(), Some(b'V' | b'v' | b'I' | b'i')) {
            pass.source(&statement)
                .with_context(|| format!("line {}: LTspice source waveform", statement.line))?;
        }
    }
    ensure!(
        pass.scope.is_empty(),
        "unterminated LTspice subcircuit scope"
    );
    let Some(bytes) = lexical::apply_edits(body, pass.edits) else {
        return Ok(None);
    };
    let mut mapped = Vec::new();
    for (index, origin) in origins.iter().copied().enumerate() {
        if let Some(count) = pass.inserted.get(&(index + 1)) {
            mapped.extend(std::iter::repeat_n(origin, *count));
        }
        mapped.push(origin);
    }
    ensure!(
        mapped.len() == bytes.split_inclusive(|byte| *byte == b'\n').count(),
        "waveform source mapping is inconsistent"
    );
    Ok(Some(SourceRewrite {
        bytes,
        origins: mapped,
        waveforms: pass.waveforms,
    }))
}
