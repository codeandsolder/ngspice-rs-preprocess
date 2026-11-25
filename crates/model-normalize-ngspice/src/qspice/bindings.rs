//! Lower only declarations with established source identity, not arbitrary
//! node/model/parameter spelling. Static parameter references retain their
//! occurrence binding rather than being evaluated in the declaration scope.

use super::{Feature, literal, reject};
use crate::{
    lexical::{Edit, Statement, remove_spans, replace_span},
    linked_source::SourceOrigin,
    model_card::{parse_model_card, parse_parameter_fields},
    param_expression::SpiceExpression,
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

#[derive(Clone, Copy)]
enum Position {
    // A passive's positional identifier can also name a model.
    KnownParameter,
    // A formal, actual or linear gain slot denotes a scalar expression.
    Scalar,
    // A model value can also be a source-specific enumeration or annotation.
    ModelValue,
}

fn brace_reference(
    statement: &Statement,
    origin: &SourceOrigin,
    declarations: &Declarations,
    field: &Range<usize>,
    position: Position,
    edits: &mut Vec<Edit>,
) -> Result<()> {
    let raw = &statement.text[field.clone()];
    if literal(raw).is_some()
        || raw.starts_with(b"{")
        || raw.starts_with(b"'")
        || raw.starts_with(b"\"")
    {
        return Ok(());
    }
    let known = match declarations.parameter(statement.line, raw) {
        Visibility::Missing => false,
        Visibility::Ambiguous => {
            return Err(reject(
                origin,
                Feature::Binding,
                "conditionally declared parameter requires topology selection",
            ));
        }
        Visibility::Unique(()) => true,
    };
    let compound = raw.iter().any(|byte| {
        matches!(
            byte,
            b'+' | b'-'
                | b'*'
                | b'/'
                | b'('
                | b'^'
                | b'?'
                | b'<'
                | b'>'
                | b'!'
                | b'|'
                | b'&'
                | b'%'
        )
    });
    let rewrite = known
        || match position {
            Position::KnownParameter => false,
            Position::Scalar => true,
            Position::ModelValue => compound,
        };
    if rewrite {
        let text = std::str::from_utf8(raw)?;
        SpiceExpression::parse(text).map_err(|error| {
            reject(
                origin,
                Feature::Binding,
                &format!("invalid static scalar expression: {error:#}"),
            )
        })?;
        let mut replacement = Vec::with_capacity(raw.len() + 2);
        replacement.push(b'{');
        replacement.extend_from_slice(raw);
        replacement.push(b'}');
        replace_span(statement, field.clone(), &replacement, edits);
    }
    Ok(())
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
        removals.push(field.clone());
    }
    remove_spans(statement, input, &removals, edits);
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
    let start = fields.first().context("missing assignment card")?.end;
    for parameter in parse_parameter_fields(std::str::from_utf8(&text[start..])?, &[])? {
        if let Some(field) = parameter.span.value {
            brace_reference(
                statement,
                origin,
                declarations,
                &(start + field.start..start + field.end),
                Position::Scalar,
                edits,
            )?;
        }
    }
    Ok(())
}

fn model_references(
    statement: &Statement,
    origin: &SourceOrigin,
    declarations: &Declarations,
    edits: &mut Vec<Edit>,
) -> Result<()> {
    let model = parse_model_card(std::str::from_utf8(&statement.text)?, origin.line, &[])?;
    for parameter in model.parameters {
        // Manufacturer annotations are not scalar expressions.
        if !parameter.name.eq_ignore_ascii_case("mfg")
            && let Some(field) = parameter.span.value
        {
            brace_reference(
                statement,
                origin,
                declarations,
                &field,
                Position::ModelValue,
                edits,
            )?;
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
    if name.eq_ignore_ascii_case(b".model") {
        return model_references(statement, origin, declarations, edits);
    }
    if name.eq_ignore_ascii_case(b".subckt") || designator == b'x' {
        return assignment_references(statement, origin, declarations, fields, edits);
    }
    match designator {
        b'r' | b'c' | b'l' => {
            if let Some(field) = fields.get(3)
                && fields.get(4).is_none_or(|next| text[next.clone()] != *b"=")
                && !passive_model(statement, origin, declarations, &text[field.clone()])?
            {
                brace_reference(
                    statement,
                    origin,
                    declarations,
                    field,
                    Position::KnownParameter,
                    edits,
                )?;
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
                                && text[span.clone()]
                                    .get(keyword.len())
                                    .is_none_or(|byte| *byte == b'(')
                        })
                })
            {
                brace_reference(
                    statement,
                    origin,
                    declarations,
                    field,
                    Position::Scalar,
                    edits,
                )?;
            }
        }
        b'f' | b'h' => {
            if let Some(field) = fields.get(4) {
                brace_reference(
                    statement,
                    origin,
                    declarations,
                    field,
                    Position::Scalar,
                    edits,
                )?;
            }
        }
        b'q' => bjt_polarity(statement, origin, declarations, fields, edits, input)?,
        _ => {}
    }
    Ok(())
}
