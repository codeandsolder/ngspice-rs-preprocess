//! Lower QSPICE independent-source syntax only where the native time law
//! agrees. Complex event schedules belong to the shared waveform binder.

use super::{Feature, literal, reject};
use crate::{
    lexical::{Edit, Statement, replace_span, replace_spans, waveform_tokens},
    linked_source::SourceOrigin,
    param_expression::SpiceExpression,
    waveform::{QspicePoint, QspiceWaveform},
};
use anyhow::{Context, Result};
use model_preprocess_command::{Resource, WorkBudget};
use std::ops::Range;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    Pwl,
    Pulse,
}

impl Shape {
    fn identify(text: &[u8]) -> Option<(Self, usize)> {
        for (keyword, shape) in [(b"pwl".as_slice(), Self::Pwl), (b"pulse", Self::Pulse)] {
            if text
                .get(..keyword.len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case(keyword))
                && text.get(keyword.len()).is_none_or(|byte| *byte == b'(')
            {
                return Some((shape, keyword.len()));
            }
        }
        None
    }
}

struct Arguments {
    fields: Vec<Range<usize>>,
    keyword: Range<usize>,
    bare: bool,
    tail: usize,
}

fn arguments(
    text: &[u8],
    fields: &[Range<usize>],
    index: usize,
    name_len: usize,
) -> Result<Arguments> {
    let keyword = fields[index].start..fields[index].start + name_len;
    let after = &text[keyword.end..];
    let tail = after.trim_ascii_start();
    if tail.starts_with(b"(") {
        // The common lexer already checked balanced groups; the opening
        // parenthesis belongs to either the keyword token or the next token.
        let group = if fields[index].end > keyword.end {
            fields[index].clone()
        } else {
            fields
                .get(index + 1)
                .context("missing waveform arguments")?
                .clone()
        };
        let begin = text.len() - tail.len() + 1;
        let end = group.end.checked_sub(1).context("missing waveform close")?;
        anyhow::ensure!(
            text.get(end) == Some(&b')') && begin <= end,
            "malformed waveform argument group"
        );
        let args = waveform_tokens(&text[begin..end])?
            .into_iter()
            .map(|span| begin + span.start..begin + span.end)
            .collect();
        Ok(Arguments {
            fields: args,
            keyword,
            bare: false,
            tail: group.end,
        })
    } else {
        // A bare waveform terminates at an explicit DC or AC declaration.
        // All other trailing syntax is considered part of the source law.
        let end = fields[index + 1..]
            .iter()
            .find(|span| {
                [b"dc".as_slice(), b"ac"]
                    .iter()
                    .any(|name| text[(*span).clone()].eq_ignore_ascii_case(name))
            })
            .map_or(text.len(), |span| span.start);
        let args = waveform_tokens(&text[keyword.end..end])?
            .into_iter()
            .map(|span| keyword.end + span.start..keyword.end + span.end)
            .collect();
        Ok(Arguments {
            fields: args,
            keyword,
            bare: true,
            tail: end,
        })
    }
}

fn source_modifier(text: &[u8]) -> bool {
    [
        b"trigger".as_slice(),
        b"timectrl",
        b"td",
        b"r",
        b"rpar",
        b"rser",
        b"cpar",
        b"file",
    ]
    .iter()
    .any(|keyword| text.eq_ignore_ascii_case(keyword))
}

fn has_modifier_assignment(text: &[u8], fields: &[Range<usize>]) -> bool {
    fields
        .windows(2)
        .any(|pair| source_modifier(&text[pair[0].clone()]) && text[pair[1].clone()] == *b"=")
}

fn validate_modifiers(
    statement: &Statement,
    origin: &SourceOrigin,
    args: &Arguments,
    fields: &[Range<usize>],
) -> Result<()> {
    let text = &statement.text;
    let command = args.fields.iter().any(|span| {
        [
            b"repeat".as_slice(),
            b"endrepeat",
            b"file",
            b"xtrap",
            b"log",
        ]
        .iter()
        .any(|keyword| text[span.clone()].eq_ignore_ascii_case(keyword))
    });
    let tail_flag = fields
        .iter()
        .filter(|span| span.start >= args.tail)
        .take_while(|span| {
            ![b"dc".as_slice(), b"ac"]
                .iter()
                .any(|keyword| text[(*span).clone()].eq_ignore_ascii_case(keyword))
        })
        .any(|span| {
            [
                b"repeat".as_slice(),
                b"endrepeat",
                b"file",
                b"xtrap",
                b"log",
            ]
            .iter()
            .any(|keyword| text[span.clone()].eq_ignore_ascii_case(keyword))
        });
    if command
        || tail_flag
        || has_modifier_assignment(text, &args.fields)
        || has_modifier_assignment(text, &fields[3..])
    {
        return Err(reject(
            origin,
            Feature::Waveform,
            "repetition, waveform resources, triggering, delay and source parasitics require typed source binding",
        ));
    }
    Ok(())
}

/// Cumulative time increments are QSPICE syntax; a positive sign on the
/// value field remains an ordinary arithmetic sign. A relative time whose
/// predecessor is symbolic cannot be flattened without occurrence binding.
fn pwl(
    statement: &Statement,
    origin: &SourceOrigin,
    args: &Arguments,
    edits: &mut Vec<Edit>,
) -> Result<()> {
    let text = &statement.text;
    if args.fields.len() < 4 || !args.fields.len().is_multiple_of(2) {
        return Err(reject(
            origin,
            Feature::Waveform,
            "PWL requires at least two time/value pairs",
        ));
    }
    let mut previous = Some(0.);
    for (index, pair) in args.fields.as_chunks::<2>().0.iter().enumerate() {
        let time = &pair[0];
        let source = &text[time.clone()];
        let relative = source.starts_with(b"+");
        let parsed = literal(source);
        let current = if relative {
            let (Some(base), Some(increment)) = (previous, parsed) else {
                return Err(reject(
                    origin,
                    Feature::Waveform,
                    "relative symbolic PWL requires occurrence-bound waveform timing",
                ));
            };
            let resolved = base + increment;
            if increment < 0. || !resolved.is_finite() || (index > 0 && resolved <= base) {
                return Err(reject(
                    origin,
                    Feature::Waveform,
                    "relative PWL increment must advance a representable time",
                ));
            }
            replace_span(
                statement,
                time.clone(),
                resolved.to_string().as_bytes(),
                edits,
            );
            Some(resolved)
        } else {
            parsed
        };
        if let Some(value) = current
            && (value < 0. || (index > 0 && previous.is_some_and(|last| value <= last)))
        {
            return Err(reject(
                origin,
                Feature::Waveform,
                "nonincreasing or negative PWL coordinates need a qualified sided waveform law",
            ));
        }
        previous = current;
    }
    Ok(())
}

fn pulse(statement: &Statement, origin: &SourceOrigin, args: &Arguments) -> Result<()> {
    let text = &statement.text;
    let [_, _, delay, rise, fall, width, period] = args.fields.as_slice() else {
        return Err(reject(
            origin,
            Feature::Waveform,
            "implicit PULSE timing or finite repetition requires source-specific waveform binding",
        ));
    };
    let timing = [delay, rise, fall, width, period].map(|field| literal(&text[field.clone()]));
    let [
        Some(delay),
        Some(rise),
        Some(fall),
        Some(width),
        Some(period),
    ] = timing
    else {
        return Err(reject(
            origin,
            Feature::Waveform,
            "symbolic PULSE timing requires occurrence-bound waveform validation",
        ));
    };
    if delay < 0. || rise <= 0. || fall <= 0. || width <= 0. || period < rise + fall + width {
        return Err(reject(
            origin,
            Feature::Waveform,
            "signed phase, default edges or extended pulse periods require typed waveform timing",
        ));
    }
    Ok(())
}

fn wrap_bare(statement: &Statement, args: &Arguments, edits: &mut Vec<Edit>) -> Result<()> {
    let text = &statement.text;
    let mut keyword = text[args.keyword.clone()].to_vec();
    keyword.push(b'(');
    replace_span(statement, args.keyword.clone(), &keyword, edits);
    let last = args.fields.last().context("missing waveform final value")?;
    let mut final_value = text[last.clone()].to_vec();
    final_value.push(b')');
    replace_span(statement, last.clone(), &final_value, edits);
    Ok(())
}

pub(super) struct Binding {
    pub waveform: QspiceWaveform,
    pub dc_override: Option<String>,
}

fn scalar_expression(raw: &[u8], origin: &SourceOrigin, budget: &WorkBudget) -> Result<String> {
    budget.charge(Resource::EmittedBytes, raw.len())?;
    let text = std::str::from_utf8(raw)?;
    let text = text
        .strip_prefix('{')
        .and_then(|s| s.strip_suffix('}'))
        .or_else(|| text.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
        .unwrap_or(text);
    if literal(text.as_bytes()).is_none() {
        SpiceExpression::parse_with_budget(text, budget).map_err(|error| {
            reject(
                origin,
                Feature::Waveform,
                &format!("invalid source expression: {error:#}"),
            )
        })?;
    }
    Ok(text.to_owned())
}

fn bound_waveform(
    text: &[u8],
    shape: Shape,
    args: &Arguments,
    origin: &SourceOrigin,
    budget: &WorkBudget,
) -> Result<QspiceWaveform> {
    match shape {
        Shape::Pwl => {
            if args.fields.len() < 4 || !args.fields.len().is_multiple_of(2) {
                return Err(reject(
                    origin,
                    Feature::Waveform,
                    "PWL requires at least two time/value pairs",
                ));
            }
            budget.charge(Resource::GeneratedNodes, args.fields.len() / 2)?;
            let points = args
                .fields
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| {
                    let time = &text[pair[0].clone()];
                    let relative = time.starts_with(b"+");
                    let time = time.strip_prefix(b"+").unwrap_or(time);
                    Ok(QspicePoint {
                        time: scalar_expression(time, origin, budget)?,
                        relative,
                        value: scalar_expression(&text[pair[1].clone()], origin, budget)?,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(QspiceWaveform::Pwl(points))
        }
        Shape::Pulse => {
            let [first, second, delay, rise, fall, width, period] = args.fields.as_slice() else {
                return Err(reject(
                    origin,
                    Feature::Waveform,
                    "implicit or finite-repetition PULSE timing requires a separately qualified law",
                ));
            };
            let scalar =
                |field: &Range<usize>| scalar_expression(&text[field.clone()], origin, budget);
            Ok(QspiceWaveform::Pulse {
                first: scalar(first)?,
                second: scalar(second)?,
                delay: scalar(delay)?,
                rise: scalar(rise)?,
                fall: scalar(fall)?,
                width: scalar(width)?,
                period: scalar(period)?,
            })
        }
    }
}

fn waveform_span(args: &Arguments) -> Result<Range<usize>> {
    let end = if args.bare {
        args.fields
            .last()
            .context("missing waveform final value")?
            .end
    } else {
        args.tail
    };
    Ok(args.keyword.start..end)
}

// Parse bias declarations outside the waveform, never treating a waveform
// ordinate, an AC magnitude or a node name as an explicit DC override.
fn dc_field(
    text: &[u8],
    fields: &[Range<usize>],
    wave: &Range<usize>,
    origin: &SourceOrigin,
) -> Result<Option<Range<usize>>> {
    let leading = fields
        .get(3)
        .filter(|field| {
            field.end <= wave.start
                && ![b"dc".as_slice(), b"ac", b"distof1", b"distof2"]
                    .iter()
                    .any(|name| text[(*field).clone()].eq_ignore_ascii_case(name))
                && fields.get(4).is_none_or(|next| text[next.clone()] != *b"=")
        })
        .cloned();
    let mut dc = leading;
    for (index, field) in fields.iter().enumerate().skip(3) {
        if wave.contains(&field.start) || !text[field.clone()].eq_ignore_ascii_case(b"dc") {
            continue;
        }
        let mut value = index + 1;
        if fields
            .get(value)
            .is_some_and(|next| text[next.clone()] == *b"=")
        {
            value += 1;
        }
        let span = fields
            .get(value)
            .filter(|span| !wave.contains(&span.start))
            .ok_or_else(|| reject(origin, Feature::Waveform, "missing explicit DC value"))?;
        if dc.replace(span.clone()).is_some() {
            return Err(reject(
                origin,
                Feature::Waveform,
                "conflicting explicit DC declarations",
            ));
        }
    }
    Ok(dc)
}

fn prepare_binding(
    statement: &Statement,
    origin: &SourceOrigin,
    fields: &[Range<usize>],
    waveform: (Shape, &Arguments),
    edits: &mut Vec<Edit>,
    input: &[u8],
    budget: &WorkBudget,
) -> Result<Binding> {
    let (shape, args) = waveform;
    let text = &statement.text;
    let waveform = bound_waveform(text, shape, args, origin, budget)?;
    let span = waveform_span(args)?;
    let dc = dc_field(text, fields, &span, origin)?;
    let dc_override = dc
        .as_ref()
        .map(|field| scalar_expression(&text[field.clone()], origin, budget))
        .transpose()?;
    let mut replacements = Vec::new();
    if let Some(dc) = dc {
        // A symbolic bias is also bound from the mandatory declaration. The
        // native placeholder must be executable before occurrence binding.
        if literal(&text[dc.clone()]).is_none() {
            replacements.push((dc, b"0".to_vec()));
        }
        replacements.push((span, Vec::new()));
    } else {
        replacements.push((span, b"DC 0".to_vec()));
    }
    replace_spans(statement, input, &replacements, edits);
    Ok(Binding {
        waveform,
        dc_override,
    })
}

pub(super) fn rewrite(
    statement: &Statement,
    origin: &SourceOrigin,
    fields: &[Range<usize>],
    edits: &mut Vec<Edit>,
    input: &[u8],
    budget: &WorkBudget,
) -> Result<Option<Binding>> {
    let text = &statement.text;
    if fields
        .get(3..)
        .is_some_and(|tail| has_modifier_assignment(text, tail))
    {
        return Err(reject(
            origin,
            Feature::Waveform,
            "source parasitics or timing options require typed source binding",
        ));
    }
    for (index, field) in fields.iter().enumerate().skip(3) {
        let Some((shape, name_len)) = Shape::identify(&text[field.clone()]) else {
            continue;
        };
        let args = arguments(text, fields, index, name_len)
            .map_err(|error| reject(origin, Feature::Waveform, &error.to_string()))?;
        budget.charge(Resource::EvaluationSteps, args.fields.len())?;
        validate_modifiers(statement, origin, &args, fields)?;
        if args
            .fields
            .iter()
            .any(|span| literal(&text[span.clone()]).is_none())
        {
            return prepare_binding(
                statement,
                origin,
                fields,
                (shape, &args),
                edits,
                input,
                budget,
            )
            .map(Some);
        }
        match shape {
            Shape::Pwl => pwl(statement, origin, &args, edits)?,
            Shape::Pulse => pulse(statement, origin, &args)?,
        }
        if args.bare {
            wrap_bare(statement, &args, edits)?;
        }
        break;
    }
    Ok(None)
}
