//! Shared physical-line-preserving lowering of series resistance.

use crate::{
    IdentifierAllocator, SourceDialects, lexical, linked_source::SourceDialect,
    model_card::parse_spice_constant,
};
use anyhow::{Context, Result, ensure};
use model_preprocess_command::{Resource, WorkBudget};
use std::{collections::BTreeSet, io::Write, ops::Range};

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

fn literal_zero(text: &[u8]) -> bool {
    std::str::from_utf8(text)
        .ok()
        .and_then(|value| parse_spice_constant(value).ok())
        .is_some_and(|value| value == 0.)
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

/// Identify lexical definitions rather than globally matching K references.
/// A K card in one subcircuit never disables default loss in another.
fn scoped(input: &[u8], budget: &WorkBudget) -> Result<Vec<(usize, lexical::Statement)>> {
    let mut scopes = vec![0];
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
            budget.check_depth(scopes.len())?;
            scopes.push(statement.line);
        } else if first.eq_ignore_ascii_case(b".ends") {
            ensure!(scopes.len() > 1, "line {}: unmatched .ends", statement.line);
            scopes.pop();
        } else {
            output.push((
                *scopes.last().context("missing passive definition scope")?,
                statement,
            ));
        }
    }
    ensure!(scopes.len() == 1, "unterminated passive definition scope");
    Ok(output)
}

fn coupled_inductors(
    statements: &[(usize, lexical::Statement)],
    budget: &WorkBudget,
) -> Result<BTreeSet<(usize, Vec<u8>)>> {
    let mut coupled = BTreeSet::new();
    for (scope, statement) in statements {
        budget.charge(Resource::Statements, 1)?;
        if !matches!(statement.text.first(), Some(b'k' | b'K')) {
            continue;
        }
        let fields = lexical::tokens(&statement.text)?;
        ensure!(
            fields.len() >= 4,
            "line {}: mutual inductance needs at least two inductors and a coefficient",
            statement.line
        );
        for span in &fields[1..fields.len() - 1] {
            budget.charge(Resource::GeneratedNodes, 1)?;
            budget.charge(Resource::EmittedBytes, span.len())?;
            coupled.insert((*scope, statement.text[span.clone()].to_ascii_uppercase()));
        }
    }
    Ok(coupled)
}

fn series_extras(
    statement: &lexical::Statement,
    fields: &[Range<usize>],
) -> Result<Vec<Range<usize>>> {
    let mut extra = Vec::new();
    for key in [b"m".as_slice(), b"temp"] {
        if let Some((span, value)) = assignment(statement, fields, key)? {
            ensure!(
                key != b"m" || !literal_zero(&statement.text[value]),
                "line {}: zero multiplicity requires typed passive removal",
                statement.line
            );
            extra.push(span);
        }
    }
    Ok(extra)
}

fn unsupported_parasitics(
    statement: &lexical::Statement,
    fields: &[Range<usize>],
) -> Result<Vec<Range<usize>>> {
    let mut removals = Vec::new();
    for key in [b"rpar".as_slice(), b"cpar", b"lser", b"rlshunt"] {
        if let Some((span, value)) = assignment(statement, fields, key)? {
            ensure!(
                literal_zero(&statement.text[value]),
                "line {}: LTspice {} requires a typed passive parasitic network",
                statement.line,
                String::from_utf8_lossy(key)
            );
            removals.push(span);
        }
    }
    Ok(removals)
}

/// Voltage-source series resistance is a real, noisy resistor, but an
/// internal shunt capacitance changes the observable source branch current.
/// A symbolic series resistance may change the device topology at zero.
fn rewrite_source(rewriter: &mut Rewriter<'_>, statement: &lexical::Statement) -> Result<()> {
    let text = &statement.text;
    let fields = lexical::tokens(text)?;
    let voltage = matches!(text.first(), Some(b'v' | b'V'));
    let mut removals = Vec::new();
    for key in [b"rpar".as_slice(), b"lser", b"rlshunt"] {
        ensure!(
            assignments(statement, &fields, key)?.is_empty(),
            "line {}: unsupported LTspice source {} assignment",
            statement.line,
            String::from_utf8_lossy(key)
        );
    }
    let capacitances = assignments(statement, &fields, b"cpar")?;
    if let Some((_, value)) = capacitances.last() {
        ensure!(
            voltage
                && crate::ltspice::closed_scalar(&text[value.clone()], &rewriter.used.budget)?
                    == Some(0.),
            "line {}: LTspice source Cpar requires typed parasitic current observation",
            statement.line
        );
        removals.extend(capacitances.into_iter().map(|(span, _)| span));
    }
    let resistances = assignments(statement, &fields, b"rser")?;
    if let Some((_, value)) = resistances.last() {
        ensure!(
            voltage,
            "line {}: unsupported LTspice current-source Rser",
            statement.line
        );
        let resistance =
            crate::ltspice::closed_scalar(&text[value.clone()], &rewriter.used.budget)?
                .with_context(|| {
                    format!(
                        "line {}: symbolic source Rser requires typed topology binding",
                        statement.line
                    )
                })?;
        ensure!(
            resistance >= 0.,
            "line {}: negative source Rser requires typed thermal noise",
            statement.line
        );
        if resistance > 0. {
            rewriter.insert_resistor(statement, &fields, resistance.to_string().as_bytes(), &[])?;
        }
        removals.extend(resistances.into_iter().map(|(span, _)| span));
    }
    if !removals.is_empty() {
        rewriter.remove(statement, &removals);
    }
    Ok(())
}

fn rewrite_explicit(
    rewriter: &mut Rewriter<'_>,
    statement: &lexical::Statement,
    scope: usize,
    coupled: &BTreeSet<(usize, Vec<u8>)>,
    include: bool,
) -> Result<()> {
    let text = &statement.text;
    if matches!(text.first(), Some(b'v' | b'V' | b'i' | b'I')) {
        return rewrite_source(rewriter, statement);
    }
    if !matches!(text.first(), Some(b'c' | b'C' | b'l' | b'L')) {
        return Ok(());
    }
    let fields = lexical::tokens(text)?;
    if fields.len() < 4 {
        return Ok(());
    }
    let inductor = matches!(text.first(), Some(b'l' | b'L'));
    ensure!(
        assignment(statement, &fields, b"ic")?.is_none(),
        "line {}: LTspice passive IC requires analysis-specific bias initialization",
        statement.line
    );
    ensure!(
        fields.get(4).is_none_or(|span| text[span.clone()] != *b"="),
        "line {}: nonlinear LTspice passive requires typed lowering",
        statement.line
    );
    let mut removals = unsupported_parasitics(statement, &fields)?;
    let extra = series_extras(statement, &fields)?;
    let explicit = assignment(statement, &fields, b"rser")?;
    let resistance = if let Some((span, value)) = explicit {
        removals.push(span);
        let value = &text[value];
        (!literal_zero(value)).then_some(value)
    } else if inductor && !coupled.contains(&(scope, text[fields[0].clone()].to_ascii_uppercase()))
    {
        ensure!(
            !include || scope != 0,
            "line {}: default inductor resistance needs the complete LTspice deck to resolve mutual coupling; specify Rser explicitly",
            statement.line
        );
        Some(b"1m".as_slice())
    } else {
        None
    };
    if let Some(value) = resistance {
        rewriter.insert_resistor(statement, &fields, value, &extra)?;
    }
    if !removals.is_empty() {
        rewriter.remove(statement, &removals);
    }
    Ok(())
}

pub struct SeriesRewrite {
    pub(super) bytes: Vec<u8>,
    pub(super) ltspice: bool,
    pub(super) native: bool,
}

fn rewrite_native(rewriter: &mut Rewriter<'_>, statement: &lexical::Statement) -> Result<()> {
    let text = &statement.text;
    if !matches!(text.first(), Some(b'l' | b'L'))
        || !text
            .windows(4)
            .any(|part| part.eq_ignore_ascii_case(b"rser"))
    {
        return Ok(());
    }
    let fields = lexical::tokens(text)?;
    if let Some((span, value)) = assignment(statement, &fields, b"rser")? {
        rewriter.insert_resistor(statement, &fields, &text[value], &[])?;
        rewriter.remove(statement, &[span]);
    }
    Ok(())
}

pub fn rewrite(
    input: &[u8],
    inserted: &mut Vec<usize>,
    dialects: SourceDialects<'_>,
    title_lines: usize,
    include: bool,
    origins: Option<&[usize]>,
    budget: &WorkBudget,
) -> Result<Option<SeriesRewrite>> {
    budget.charge(Resource::ScannedBytes, input.len())?;
    let has_ltspice = dialects.includes(SourceDialect::Ltspice);
    if !has_ltspice
        && !input
            .windows(4)
            .any(|part| part.eq_ignore_ascii_case(b"rser"))
    {
        return Ok(None);
    }
    let mut rewriter = Rewriter {
        input,
        inserted,
        used: IdentifierAllocator::new(input, budget)?,
        edits: Vec::new(),
        serial: 0,
    };
    let mut ltspice = false;
    let mut native = false;
    if has_ltspice {
        // Coupling is a physical relation across dialect boundaries. Index
        // every lexical scope, but lower only the source's own passive law.
        let statements = scoped(input, budget)?;
        let coupled = coupled_inductors(&statements, budget)?;
        for (scope, statement) in &statements {
            budget.charge(Resource::Statements, 1)?;
            let previous = rewriter.edits.len();
            let line = statement.line + title_lines;
            let source = if let Some(origins) = origins {
                dialects.at_mapped(origins, line)?
            } else {
                dialects.at(line)?
            };
            if source == SourceDialect::Ltspice {
                rewrite_explicit(&mut rewriter, statement, *scope, &coupled, include)?;
                ltspice |= rewriter.edits.len() != previous;
            } else {
                rewrite_native(&mut rewriter, statement)?;
                native |= rewriter.edits.len() != previous;
            }
        }
    } else {
        // The narrow native compatibility pass does not add default losses.
        let mut control = false;
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
            }
            if !control {
                let previous = rewriter.edits.len();
                rewrite_native(&mut rewriter, &statement)?;
                native |= rewriter.edits.len() != previous;
            }
        }
    }
    Ok(
        lexical::apply_budgeted_edits(input, rewriter.edits, budget)?.map(|bytes| SeriesRewrite {
            bytes,
            ltspice,
            native,
        }),
    )
}
