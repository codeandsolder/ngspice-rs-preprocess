//! Specialize phase-sensitive table macros without duplicating the source
//! scope resolver. Native macro expansion retains declaration scope.

use super::operators;
use crate::{expression_start, fresh_identifier, lexical, source_identifiers};
use anyhow::{Context, Result, ensure};
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

type Names = BTreeMap<Vec<u8>, Vec<u8>>;

/// Function signature identifier (excluding the parameter list).
pub(super) fn name(text: &[u8]) -> Result<Range<usize>> {
    let fields = lexical::tokens(text)?;
    let field = fields
        .get(1)
        .context("missing LTspice function signature")?;
    let end = text[field.clone()]
        .iter()
        .position(|byte| *byte == b'(')
        .map_or(field.end, |index| field.start + index);
    ensure!(field.start < end, "empty LTspice function name");
    Ok(field.start..end)
}

fn first(text: &[u8]) -> &[u8] {
    text.split(u8::is_ascii_whitespace)
        .next()
        .unwrap_or_default()
}

fn expression_calls(text: &[u8], start: usize) -> Vec<Range<usize>> {
    let model_type = first(text).eq_ignore_ascii_case(b".model");
    lexical::function_calls(text, start)
        .into_iter()
        .filter(|span| !model_type || span.start != start)
        .collect()
}

/// Build the reverse dependency graph across declarations, conservatively
/// marking every declaration of a shadowed name as phase-sensitive. Generated
/// names are shared across lexical scopes, so native scope rules remain intact.
fn specialized_names(input: &[u8], body: &[u8]) -> Result<Names> {
    let mut reverse = BTreeMap::<Vec<u8>, BTreeSet<Vec<u8>>>::new();
    for statement in lexical::statements(body)? {
        let text = &statement.text;
        if !first(text).eq_ignore_ascii_case(b".func") {
            continue;
        }
        let identifier = text[name(text)?].to_ascii_uppercase();
        let Some(start) = expression_start(text, first(text))? else {
            continue;
        };
        for call in expression_calls(text, start) {
            reverse
                .entry(text[call].to_ascii_uppercase())
                .or_default()
                .insert(identifier.clone());
        }
    }
    let mut used = source_identifiers(input);
    let mut pending = vec![b"TABLE".to_vec()];
    let mut names = Names::new();
    while let Some(dependency) = pending.pop() {
        let Some(dependents) = reverse.get(&dependency) else {
            continue;
        };
        for dependent in dependents {
            if names.contains_key(dependent) {
                continue;
            }
            let mut base = b"__lt_param_".to_vec();
            base.extend_from_slice(dependent);
            let alias = fresh_identifier(&mut used, &base)
                .context("LTspice parameter macro identifier overflow")?;
            names.insert(dependent.clone(), alias);
            pending.push(dependent.clone());
        }
    }
    Ok(names)
}

fn clone_definition(
    text: &[u8],
    span: Range<usize>,
    alias: &[u8],
    start: usize,
    names: &Names,
) -> Vec<u8> {
    let mut edits = vec![(span, alias.to_vec())];
    for call in expression_calls(text, start) {
        if let Some(target) = names.get(&text[call.clone()].to_ascii_uppercase()) {
            edits.push((call, target.clone()));
        }
    }
    lexical::apply_edits(text, edits).unwrap_or_default()
}

pub(super) struct Specialization {
    pub bytes: Vec<u8>,
    pub origins: Vec<usize>,
    pub static_macros: BTreeSet<Vec<u8>>,
}

/// Clone only macros that directly or transitively invoke a table. Insert the
/// static clone next to the original declaration, and rewrite static call
/// sites. Each clone has a single physical line with its declaration origin.
pub(super) fn specialize(
    input: &[u8],
    body: &[u8],
    origins: &[usize],
) -> Result<Option<Specialization>> {
    let names = specialized_names(input, body)?;
    if names.is_empty() {
        return Ok(None);
    }
    let mut edits = Vec::new();
    let mut inserted = BTreeSet::new();
    let mut control = false;
    for statement in lexical::statements(body)? {
        let text = &statement.text;
        let keyword = first(text);
        if keyword.eq_ignore_ascii_case(b".control") {
            control = true;
        } else if keyword.eq_ignore_ascii_case(b".endc") {
            control = false;
            continue;
        }
        if control {
            continue;
        }
        let Some(start) = expression_start(text, keyword)? else {
            continue;
        };
        if keyword.eq_ignore_ascii_case(b".func") {
            let span = name(text)?;
            if let Some(alias) = names.get(&text[span.clone()].to_ascii_uppercase()) {
                let mut clone = clone_definition(text, span, alias, start, &names);
                clone.push(b'\n');
                edits.push((statement.start..statement.start, clone));
                inserted.insert(statement.line);
            }
            continue;
        }
        for call in expression_calls(text, start) {
            if let Some(alias) = names.get(&text[call.clone()].to_ascii_uppercase())
                && operators::static_table(text, start, call.start)?
            {
                lexical::replace_span(&statement, call, alias, &mut edits);
            }
        }
    }
    let Some(bytes) = lexical::apply_edits(body, edits) else {
        return Ok(None);
    };
    let mut mapped = Vec::with_capacity(origins.len() + inserted.len());
    for (index, origin) in origins.iter().copied().enumerate() {
        if inserted.contains(&(index + 1)) {
            mapped.push(origin);
        }
        mapped.push(origin);
    }
    ensure!(
        mapped.len() == bytes.split_inclusive(|byte| *byte == b'\n').count(),
        "LTspice function specialization source mapping is inconsistent"
    );
    Ok(Some(Specialization {
        bytes,
        origins: mapped,
        static_macros: names
            .values()
            .map(|name| name.to_ascii_uppercase())
            .collect(),
    }))
}
