//! Shared physical-line-preserving lowering of series resistance.

use crate::{IdentifierAllocator, NormalizationDialect, lexical};
use anyhow::{Context, Result, ensure};
use model_preprocess_command::{Resource, WorkBudget};
use std::{io::Write, ops::Range};

mod ltspice;

type Assignment = (Range<usize>, Range<usize>);

fn assignments(
    statement: &lexical::Statement,
    fields: &[Range<usize>],
    key: &[u8],
) -> Result<Vec<Assignment>> {
    let text = &statement.text;
    let mut result = Vec::new();
    let start = if matches!(text.first(), Some(b'v' | b'V' | b'i' | b'I')) {
        3
    } else {
        4
    };
    for index in start..fields.len() {
        if !text[fields[index].clone()].eq_ignore_ascii_case(key)
            || text[fields[index - 1].clone()] == *b"="
            || fields
                .get(index + 1)
                .is_none_or(|span| text[span.clone()] != *b"=")
        {
            continue;
        }
        let value = fields
            .get(index + 2)
            .filter(|span| text[(*span).clone()] != *b"=")
            .filter(|_| {
                fields
                    .get(index + 3)
                    .is_none_or(|span| text[span.clone()] != *b"=")
            })
            .with_context(|| {
                format!(
                    "line {}: {} requires a value",
                    statement.line,
                    String::from_utf8_lossy(key)
                )
            })?;
        result.push((fields[index].start..value.end, value.clone()));
    }
    Ok(result)
}

fn assignment(
    statement: &lexical::Statement,
    fields: &[Range<usize>],
    key: &[u8],
) -> Result<Option<Assignment>> {
    let mut found = assignments(statement, fields, key)?;
    ensure!(
        found.len() <= 1,
        "line {}: duplicate {} assignment",
        statement.line,
        String::from_utf8_lossy(key)
    );
    Ok(found.pop())
}

struct Rewriter<'a> {
    input: &'a [u8],
    used: IdentifierAllocator,
    edits: Vec<lexical::Edit>,
    inserted: &'a mut Vec<usize>,
    serial: usize,
}

impl Rewriter<'_> {
    fn remove(&mut self, statement: &lexical::Statement, assignments: &[Range<usize>]) {
        lexical::remove_spans(statement, self.input, assignments, &mut self.edits);
    }

    fn insert_resistor(
        &mut self,
        statement: &lexical::Statement,
        fields: &[Range<usize>],
        resistance: &[u8],
        extra: &[Range<usize>],
    ) -> Result<()> {
        let text = &statement.text;
        let device = &text[fields[0].clone()];
        let node = &text[fields[1].clone()];
        let mut resistor_base = b"R__".to_vec();
        resistor_base.extend_from_slice(device);
        resistor_base.extend_from_slice(b"_RSER");
        let resistor = self.used.fresh(&resistor_base)?;
        let mut internal_base = b"__".to_vec();
        internal_base.extend_from_slice(device);
        write!(&mut internal_base, "_rser_{}", self.serial)?;
        let internal = self.used.fresh(&internal_base)?;
        self.serial += 1;
        let physical = self.input[statement.start..]
            .split_inclusive(|b| *b == b'\n')
            .next()
            .unwrap_or_default();
        let (body, ending) = lexical::split_line_ending(physical);
        let mut line = body[..body.len() - body.trim_ascii_start().len()].to_vec();
        for word in [&resistor[..], node, &internal, resistance] {
            line.extend_from_slice(word);
            line.push(b' ');
        }
        for range in extra {
            line.extend_from_slice(&text[range.clone()]);
            line.push(b' ');
        }
        line.pop();
        line.extend_from_slice(if ending.is_empty() { b"\n" } else { ending });
        self.edits.push((statement.start..statement.start, line));
        self.inserted.push(statement.line);
        lexical::replace_span(statement, fields[1].clone(), &internal, &mut self.edits);
        Ok(())
    }
}

/// Retain physical statements outside control scripts and validate nested
/// subcircuit boundaries. Occurrence-dependent topology is not decided here.
fn device_statements(input: &[u8], budget: &WorkBudget) -> Result<Vec<lexical::Statement>> {
    let mut depth = 1usize;
    let mut control = false;
    let mut output = Vec::new();
    for statement in lexical::statements(input)? {
        budget.charge(Resource::Statements, 1)?;
        let first = statement
            .text
            .split(u8::is_ascii_whitespace)
            .next()
            .unwrap_or_default();
        if first.eq_ignore_ascii_case(b".control") {
            control = true;
        } else if first.eq_ignore_ascii_case(b".endc") {
            control = false;
            continue;
        }
        if control {
            continue;
        }
        if first.eq_ignore_ascii_case(b".subckt") {
            budget.check_depth(depth)?;
            depth += 1;
        } else if first.eq_ignore_ascii_case(b".ends") {
            ensure!(depth > 1, "line {}: unmatched .ends", statement.line);
            depth -= 1;
        } else {
            output.push(statement);
        }
    }
    ensure!(depth == 1, "unterminated passive definition scope");
    Ok(output)
}

pub fn rewrite(
    input: &[u8],
    inserted: &mut Vec<usize>,
    dialect: NormalizationDialect,
    budget: &WorkBudget,
) -> Result<Option<Vec<u8>>> {
    budget.charge(Resource::ScannedBytes, input.len())?;
    if dialect == NormalizationDialect::Native
        && !input
            .windows(4)
            .any(|part| part.eq_ignore_ascii_case(b"rser"))
    {
        return Ok(None);
    }
    if dialect == NormalizationDialect::Ltspice {
        return ltspice::rewrite(input, budget);
    }
    let mut rewriter = Rewriter {
        input,
        inserted,
        used: IdentifierAllocator::new(input, budget)?,
        edits: Vec::new(),
        serial: 0,
    };
    // The narrow native compatibility pass retains its existing scope:
    // only an explicitly written inductor Rser is interpreted.
    let mut control = false;
    for statement in lexical::statements(input)? {
        budget.charge(Resource::Statements, 1)?;
        let text = &statement.text;
        let first = text
            .split(u8::is_ascii_whitespace)
            .next()
            .unwrap_or_default();
        if first.eq_ignore_ascii_case(b".control") {
            control = true;
        } else if first.eq_ignore_ascii_case(b".endc") {
            control = false;
        }
        if control
            || !matches!(first.first(), Some(b'l' | b'L'))
            || !text
                .windows(4)
                .any(|part| part.eq_ignore_ascii_case(b"rser"))
        {
            continue;
        }
        let fields = lexical::tokens(text)?;
        if let Some((span, value)) = assignment(&statement, &fields, b"rser")? {
            let value = &text[value];
            rewriter.insert_resistor(&statement, &fields, value, &[])?;
            rewriter.remove(&statement, &[span]);
        }
    }
    lexical::apply_budgeted_edits(input, rewriter.edits, budget)
}
