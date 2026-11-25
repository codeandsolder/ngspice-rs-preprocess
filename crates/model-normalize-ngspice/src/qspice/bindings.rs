//! Lower only declarations with established source identity, not arbitrary
//! node/model/parameter spelling. Static parameter references retain their
//! occurrence binding rather than being evaluated in the declaration scope.

use super::{Feature, literal, reject};
use crate::{
    lexical::{Edit, Statement, replace_span},
    linked_source::SourceOrigin,
    parameter_scope::declarations::{Declarations, Visibility},
};
use anyhow::{Context, Result};
use std::ops::Range;

pub(super) fn passive_model(
    statement: &Statement,
    origin: &SourceOrigin,
    declarations: &Declarations,
    field: &[u8],
) -> Result<bool> {
    match declarations.model(statement.line, field) {
        Visibility::Missing => Ok(false),
        Visibility::Unique(_)
            if declarations.parameter(statement.line, field) == Visibility::Missing
                && literal(field).is_none()
                && !field.first().is_some_and(u8::is_ascii_digit)
                && !(field.starts_with(b".") && field.get(1).is_some_and(u8::is_ascii_digit)) =>
        {
            Ok(true)
        }
        _ => Err(reject(
            origin,
            Feature::Binding,
            "ambiguous passive model/parameter/value identity",
        )),
    }
}

fn brace_reference(
    statement: &Statement,
    origin: &SourceOrigin,
    declarations: &Declarations,
    field: &Range<usize>,
    edits: &mut Vec<Edit>,
) -> Result<()> {
    let raw = &statement.text[field.clone()];
    match declarations.parameter(statement.line, raw) {
        Visibility::Missing => Ok(()),
        Visibility::Ambiguous => Err(reject(
            origin,
            Feature::Binding,
            "conditionally declared parameter requires topology selection",
        )),
        Visibility::Unique(()) => {
            let mut replacement = Vec::with_capacity(raw.len() + 2);
            replacement.push(b'{');
            replacement.extend_from_slice(raw);
            replacement.push(b'}');
            replace_span(statement, field.clone(), &replacement, edits);
            Ok(())
        }
    }
}

const fn is_bjt_family(family: &[u8]) -> bool {
    matches!(family, b"npn" | b"pnp")
}

// The optional fourth terminal precedes the model. Determine the model
// from the visible declaration, not from the appearance of the node name.
fn bjt_polarity(
    statement: &Statement,
    origin: &SourceOrigin,
    declarations: &Declarations,
    fields: &[Range<usize>],
    edits: &mut Vec<Edit>,
    input: &[u8],
) -> Result<()> {
    let text = &statement.text;
    // NPN/PNP can be legal node or model identifiers. Only consider the
    // instance parameter tail after an identified model.
    let assertion = fields.iter().enumerate().skip(5).any(|(index, field)| {
        is_bjt_family(&text[field.clone()].to_ascii_lowercase())
            && text[fields[index - 1].clone()] != *b"="
    });
    if !assertion {
        return Ok(());
    }
    let mut candidates = Vec::new();
    for (index, field) in fields.iter().enumerate().skip(4).take(2) {
        match declarations.model(statement.line, &text[field.clone()]) {
            Visibility::Unique(family) if is_bjt_family(family) => candidates.push((index, family)),
            Visibility::Ambiguous => {
                return Err(reject(
                    origin,
                    Feature::Binding,
                    "ambiguous BJT model declaration",
                ));
            }
            _ => {}
        }
    }
    let [candidate] = candidates.as_slice() else {
        return Err(reject(
            origin,
            Feature::Binding,
            "cannot resolve the BJT model for a polarity assertion",
        ));
    };
    let (model_index, family) = *candidate;
    let mut removals = Vec::new();
    for (index, field) in fields.iter().enumerate().skip(model_index + 1) {
        let flag = text[field.clone()].to_ascii_lowercase();
        if !is_bjt_family(&flag)
            || fields
                .get(index.wrapping_sub(1))
                .is_some_and(|span| text[span.clone()] == *b"=")
        {
            continue;
        }
        if fields
            .get(index + 1)
            .is_some_and(|span| text[span.clone()] == *b"=")
        {
            return Err(reject(
                origin,
                Feature::Binding,
                "assigned BJT polarity flag is not qualified",
            ));
        }
        if flag != family {
            return Err(reject(
                origin,
                Feature::Binding,
                "BJT polarity assertion contradicts the visible model declaration",
            ));
        }
        removals.extend(statement.source_ranges(field.clone()));
        replace_span(statement, field.clone(), b"", edits);
    }
    for (plus, code) in &statement.continuations {
        if input[code.clone()]
            .iter()
            .enumerate()
            .all(|(offset, byte)| {
                byte.is_ascii_whitespace()
                    || removals
                        .iter()
                        .any(|range| range.contains(&(code.start + offset)))
            })
        {
            edits.push((*plus..*plus + 1, vec![b'*']));
        }
    }
    Ok(())
}

fn assignment_references(
    statement: &Statement,
    origin: &SourceOrigin,
    declarations: &Declarations,
    fields: &[Range<usize>],
    edits: &mut Vec<Edit>,
) -> Result<()> {
    let text = &statement.text;
    for pair in fields.windows(2) {
        if text[pair[0].clone()] == *b"=" {
            brace_reference(statement, origin, declarations, &pair[1], edits)?;
        }
    }
    Ok(())
}

pub(super) fn rewrite(
    statement: &Statement,
    origin: &SourceOrigin,
    declarations: &Declarations,
    fields: &[Range<usize>],
    edits: &mut Vec<Edit>,
    input: &[u8],
) -> Result<()> {
    let text = &statement.text;
    let designator = text
        .first()
        .context("empty source statement")?
        .to_ascii_lowercase();
    let name = fields.first().map_or(&[][..], |field| &text[field.clone()]);
    if name.eq_ignore_ascii_case(b".subckt") || designator == b'x' {
        return assignment_references(statement, origin, declarations, fields, edits);
    }
    match designator {
        b'r' | b'c' | b'l' => {
            if let Some(field) = fields.get(3)
                && fields.get(4).is_none_or(|next| text[next.clone()] != *b"=")
                && !passive_model(statement, origin, declarations, &text[field.clone()])?
            {
                brace_reference(statement, origin, declarations, field, edits)?;
            }
        }
        b'e' | b'g' => {
            if let Some(field) = fields.get(5)
                && fields.get(3).is_some_and(|span| {
                    ![b"value".as_slice(), b"table", b"poly"]
                        .iter()
                        .any(|keyword| {
                            text[span.clone()]
                                .get(..keyword.len())
                                .is_some_and(|prefix| prefix.eq_ignore_ascii_case(keyword))
                        })
                })
            {
                brace_reference(statement, origin, declarations, field, edits)?;
            }
        }
        b'f' | b'h' => {
            if let Some(field) = fields.get(4) {
                brace_reference(statement, origin, declarations, field, edits)?;
            }
        }
        b'q' => bjt_polarity(statement, origin, declarations, fields, edits, input)?,
        _ => {}
    }
    Ok(())
}
