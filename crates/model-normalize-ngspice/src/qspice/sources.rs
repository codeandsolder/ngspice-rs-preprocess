//! Lower QSPICE independent-source syntax only where the native time law
//! agrees. Complex event schedules belong to the shared waveform binder.

use super::{Feature, literal, reject};
use crate::{
    lexical::{Edit, Statement, replace_span, waveform_tokens},
    linked_source::SourceOrigin,
};
use anyhow::{Context, Result};
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
    let command = args.fields.first().is_some_and(|span| {
        [b"repeat".as_slice(), b"file"]
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
            if increment <= 0. || !resolved.is_finite() || resolved <= base {
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

pub(super) fn rewrite(
    statement: &Statement,
    origin: &SourceOrigin,
    fields: &[Range<usize>],
    edits: &mut Vec<Edit>,
) -> Result<()> {
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
        validate_modifiers(statement, origin, &args, fields)?;
        match shape {
            Shape::Pwl => pwl(statement, origin, &args, edits)?,
            Shape::Pulse => pulse(statement, origin, &args)?,
        }
        if args.bare {
            wrap_bare(statement, &args, edits)?;
        }
        break;
    }
    Ok(())
}
