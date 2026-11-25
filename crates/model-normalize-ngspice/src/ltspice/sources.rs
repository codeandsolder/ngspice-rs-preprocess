//! Source waveform timing normalization without global simulator modes.

use crate::{lexical, model_card::parse_spice_constant};
use anyhow::{Context, Result, ensure};
use std::ops::Range;

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
    let text = text
        .strip_prefix('{')
        .and_then(|text| text.strip_suffix('}'))
        .unwrap_or(text);
    parse_spice_constant(text)
        .context("waveform timing requires occurrence-specific parameter binding")
}

/// Resolve source-dialect timing from values already bound in the source
/// occurrence. Omitted period is a one-shot; an explicitly undersized period
/// is enlarged before zero-edge defaults are calculated.
fn pulse_timing(values: &[f64]) -> Result<[f64; 6]> {
    let [delay, rise, fall, rest @ ..] = values else {
        anyhow::bail!("LTspice PULSE requires at least five total arguments");
    };
    ensure!(rest.len() <= 3, "too many LTspice PULSE arguments");
    let delay = delay.max(0.);
    let rise = rise.max(0.);
    let fall = fall.max(0.);
    let width = rest.first().copied().unwrap_or(0.).max(0.);
    let supplied_period = rest.get(1).copied().map(|value| value.max(0.));
    let sum = rise + fall + width;
    let period = supplied_period.map_or(0., |period| {
        if period <= sum {
            sum * (1. + 2e-15)
        } else {
            period
        }
    });
    let default_basis = if period > 0. {
        let margin = period - sum;
        if width > 0. {
            width.min(margin)
        } else {
            margin
        }
    } else if width > 0. {
        width
    } else if sum > 0. {
        sum
    } else {
        1e-7
    };
    let default_edge = 0.1 * default_basis;
    let rise = if rise > 0. { rise } else { default_edge };
    let fall = if fall > 0. { fall } else { default_edge };
    let active = rise + fall + width;
    let one_shot = supplied_period.is_none() || period == 0.;
    let period = if one_shot {
        active * (1. + 2e-15)
    } else {
        period
    };
    let count = if one_shot {
        1.
    } else {
        rest.get(2).copied().unwrap_or(0.)
    };
    let timing = [delay, rise, fall, width, period, count];
    ensure!(
        timing.iter().all(|value| value.is_finite())
            && rise > 0.
            && fall > 0.
            && period > 0.
            && active <= period,
        "LTspice PULSE timing cannot be represented as a native pulse"
    );
    Ok(timing)
}

fn rewrite_pulse(
    statement: &lexical::Statement,
    name_end: usize,
    edits: &mut Vec<lexical::Edit>,
) -> Result<()> {
    let text = &statement.text;
    let (fields, close) = arguments(text, name_end)?;
    ensure!(
        (5..=8).contains(&fields.len()),
        "line {}: invalid LTspice PULSE arity",
        statement.line
    );
    let values = fields[2..]
        .iter()
        .map(|span| literal(&text[span.clone()]))
        .collect::<Result<Vec<_>>>()?;
    let timing = pulse_timing(&values)?;
    for (span, (before, after)) in fields[2..].iter().zip(values.iter().zip(timing)) {
        if before.to_bits() != after.to_bits() {
            lexical::replace_span(statement, span.clone(), after.to_string().as_bytes(), edits);
        }
    }
    // The native engine must not supply its analysis-dependent defaults.
    if values.len() < 5 || (values.len() == 5 && timing[5] > 0.) {
        let mut suffix = String::new();
        for value in &timing[values.len()..] {
            use std::fmt::Write as _;
            write!(&mut suffix, " {value}")?;
        }
        suffix.push(')');
        lexical::replace_span(statement, close..close + 1, suffix.as_bytes(), edits);
    }
    Ok(())
}

fn pwl_time(text: &[u8], previous: f64) -> Result<f64> {
    let time = literal(text)?;
    let time = if text.first() == Some(&b'+') {
        previous + time
    } else {
        time
    };
    ensure!(
        time.is_finite() && time >= previous,
        "LTspice PWL times must be chronological"
    );
    Ok(time)
}

fn rewrite_pwl(
    statement: &lexical::Statement,
    input: &[u8],
    name_end: usize,
    edits: &mut Vec<lexical::Edit>,
) -> Result<()> {
    let text = &statement.text;
    let (fields, close) = arguments(text, name_end)
        .context("LTspice structured PWL requires typed waveform lowering")?;
    ensure!(
        fields.len() >= 2 && fields.len() % 2 == 0,
        "invalid PWL coordinate pairs"
    );
    let tail = &text[close + 1..];
    ensure!(
        ![
            b"repeat".as_slice(),
            b"endrepeat",
            b"trigger",
            b"file",
            b"time_scale_factor",
            b"value_scale_factor"
        ]
        .iter()
        .any(|keyword| tail
            .windows(keyword.len())
            .any(|part| part.eq_ignore_ascii_case(keyword))),
        "LTspice structured PWL requires typed waveform lowering"
    );
    let points = fields.as_chunks::<2>().0;
    let mut times = Vec::with_capacity(points.len());
    let mut previous = 0.;
    for point in points {
        previous = pwl_time(&text[point[0].clone()], previous)?;
        times.push(previous);
    }
    let mut removals = Vec::new();
    let mut index = 0;
    while index < points.len() {
        let time = times[index];
        let end = times[index..]
            .iter()
            .position(|value| *value > time)
            .map_or(points.len(), |offset| index + offset);
        let first = &points[index];
        if text[first[0].clone()].first() == Some(&b'+') {
            lexical::replace_span(
                statement,
                first[0].clone(),
                time.to_string().as_bytes(),
                edits,
            );
        }
        if end - index > 1 {
            // The vendor collapses repeated coordinates to the arithmetic
            // mean before interpolation; they are not instantaneous jumps.
            let count = f64::from(u32::try_from(end - index)?);
            let mean = points[index..end].iter().try_fold(0., |sum, point| {
                Ok::<_, anyhow::Error>(sum + literal(&text[point[1].clone()])? / count)
            })?;
            ensure!(mean.is_finite(), "nonfinite averaged PWL ordinate");
            lexical::replace_span(
                statement,
                first[1].clone(),
                mean.to_string().as_bytes(),
                edits,
            );
            for point in &points[index + 1..end] {
                let preceding = text[..point[0].start].trim_ascii_end();
                let start = if preceding.last() == Some(&b',') {
                    preceding.len() - 1
                } else {
                    point[0].start
                };
                removals.push(start..point[1].end);
            }
        }
        index = end;
    }
    lexical::remove_spans(statement, input, &removals, edits);
    Ok(())
}

fn waveform_name(text: &[u8], field: &Range<usize>, name: &[u8]) -> bool {
    text[field.clone()]
        .get(..name.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(name))
        && text[field.clone()]
            .get(name.len())
            .is_none_or(|byte| *byte == b'(')
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
    let (step, stop, start, max_step) = if values.len() == 1 {
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
        stop > start && max_step >= 0.,
        "LTspice transient needs positive duration and nonnegative maximum step"
    );
    // Vendor adaptive default: exactly stop/1024, regardless of plotting step.
    let max_step = if max_step > 0. {
        max_step
    } else {
        stop / 1024.
    };
    let step = if step > 0. { step } else { max_step };
    ensure!(
        step > 0. && max_step > 0.,
        "unrepresentable LTspice transient timestep"
    );
    if timing.len() == 1 {
        let replacement = format!("{step} {stop} {start} {max_step}");
        lexical::replace_span(statement, timing[0].clone(), replacement.as_bytes(), edits);
    } else {
        let resolved = [step, stop, start, max_step];
        for (index, span) in timing.iter().enumerate() {
            if index + 1 == timing.len() && timing.len() < resolved.len() {
                let replacement = resolved[index..]
                    .iter()
                    .map(f64::to_string)
                    .collect::<Vec<_>>()
                    .join(" ");
                lexical::replace_span(statement, span.clone(), replacement.as_bytes(), edits);
            } else if values[index].to_bits() != resolved[index].to_bits() {
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

fn rewrite_statement(
    statement: &lexical::Statement,
    input: &[u8],
    edits: &mut Vec<lexical::Edit>,
) -> Result<()> {
    let text = &statement.text;
    let first = text
        .split(u8::is_ascii_whitespace)
        .next()
        .unwrap_or_default();
    if first.eq_ignore_ascii_case(b".tran") {
        return rewrite_tran(statement, edits);
    }
    if !matches!(text.first(), Some(b'V' | b'v' | b'I' | b'i')) {
        return Ok(());
    }
    let fields = lexical::tokens(text)?;
    for field in fields.iter().skip(3) {
        if waveform_name(text, field, b"pulse") {
            return rewrite_pulse(statement, field.start + 5, edits);
        }
        if waveform_name(text, field, b"pwl") {
            return rewrite_pwl(statement, input, field.start + 3, edits);
        }
    }
    Ok(())
}

pub(super) fn rewrite(input: &[u8]) -> Result<Option<Vec<u8>>> {
    let mut control = false;
    let mut edits = Vec::new();
    for statement in lexical::statements(input)? {
        let first = statement
            .text
            .split(u8::is_ascii_whitespace)
            .next()
            .unwrap_or_default();
        if first.eq_ignore_ascii_case(b".control") {
            control = true;
        } else if first.eq_ignore_ascii_case(b".endc") {
            control = false;
        }
        if !control {
            rewrite_statement(&statement, input, &mut edits)
                .with_context(|| format!("line {}: LTspice source waveform", statement.line))?;
        }
    }
    Ok(lexical::apply_edits(input, edits))
}
