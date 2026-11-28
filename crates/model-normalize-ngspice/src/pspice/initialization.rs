//! Device IC properties are translated to finite, analysis-scoped bias
//! equations, not ordinary native UIC hints. Preserve physical source lines.

use super::scope::Scopes;
use crate::{
    MappedNormalization, SourceDialects,
    lexical::{self, Edit, Statement},
    linked_source::{LinkedSource, RootKind, SourceDialect},
    passive_ic::{Capture, InitialConditions},
};
use anyhow::{Context, Result, ensure};
use model_preprocess_command::{Resource, WorkBudget};

fn replace_statement(
    statement: &Statement,
    body: &[u8],
    replacement: String,
    edits: &mut Vec<Edit>,
) {
    lexical::replace_spans(
        statement,
        body,
        &[(0..statement.text.len(), replacement.into_bytes())],
        edits,
    );
}

// Capture the circuit-wide flag regardless of its position in the deck.
// Do not interpret an option's value as another option.
fn options(text: &str) -> Result<Option<String>> {
    let spans = lexical::tokens(text.as_bytes())?;
    let fields = spans
        .iter()
        .map(|span| &text[span.clone()])
        .collect::<Vec<_>>();
    let mut retained = Vec::new();
    let mut found = false;
    let mut i = 1;
    while let Some(field) = fields.get(i) {
        if field.eq_ignore_ascii_case("NOICTRANSLATE") {
            ensure!(fields.get(i + 1) != Some(&"="), "NOICTRANSLATE is a flag");
            found = true;
            i += 1;
        } else if fields.get(i + 1) == Some(&"=") {
            let value = fields.get(i + 2).context("missing option value")?;
            retained.push(format!("{field}={value}"));
            i += 3;
        } else {
            retained.push((*field).to_owned());
            i += 1;
        }
    }
    Ok(found.then(|| {
        if retained.is_empty() {
            "* device initialization option recorded in typed policy".to_owned()
        } else {
            format!("{} {}", fields[0], retained.join(" "))
        }
    }))
}

#[derive(Default)]
struct ContextState {
    control: bool,
    conditional_depth: usize,
}

impl ContextState {
    fn electrical(&mut self, command: &str) -> Result<bool> {
        match command {
            ".control" => self.control = true,
            ".endc" => {
                self.control = false;
                return Ok(false);
            }
            _ if self.control => {}
            ".if" => self.conditional_depth += 1,
            ".endif" => {
                self.conditional_depth = self
                    .conditional_depth
                    .checked_sub(1)
                    .context("unmatched PSpice .ENDIF")?;
            }
            _ => {}
        }
        Ok(!self.control)
    }
}

struct Source<'a> {
    statements: &'a [Statement],
    scopes: Scopes,
    body: &'a [u8],
    origins: &'a [usize],
    dialects: SourceDialects<'a>,
}

impl Source<'_> {
    fn is_pspice(&self, statement: &Statement) -> Result<bool> {
        Ok(self.dialects.at_mapped(self.origins, statement.line)? == SourceDialect::Pspice)
    }
}

fn collect_options(source: &Source<'_>, edits: &mut Vec<Edit>) -> Result<bool> {
    let mut translate = true;
    let mut context = ContextState::default();
    for statement in source.statements {
        source.scopes.budget().charge(Resource::Statements, 1)?;
        let text = std::str::from_utf8(&statement.text)?;
        let command = text
            .split_ascii_whitespace()
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if context.electrical(&command)?
            && source.is_pspice(statement)?
            && matches!(command.as_str(), ".opt" | ".option" | ".options")
            && let Some(replacement) = options(text)?
        {
            ensure!(
                context.conditional_depth == 0 && source.scopes.scope(statement.line)? == 0,
                "NOICTRANSLATE requires unconditional deck scope"
            );
            translate = false;
            replace_statement(statement, source.body, replacement, edits);
        }
    }
    Ok(translate)
}

fn capture_devices(
    source: &Source<'_>,
    translate: bool,
    capture: &mut Capture<'_>,
    edits: &mut Vec<Edit>,
) -> Result<()> {
    let mut context = ContextState::default();
    for statement in source.statements {
        source.scopes.budget().charge(Resource::Statements, 1)?;
        let text = std::str::from_utf8(&statement.text)?;
        let command = text
            .split_ascii_whitespace()
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !context.electrical(&command)? || !source.is_pspice(statement)? {
            continue;
        }
        if command.starts_with(['c', 'l']) {
            let scope = source.scopes.path(source.scopes.scope(statement.line)?);
            let rewritten = capture.pspice_device(scope, text, translate)?;
            if rewritten != text {
                replace_statement(statement, source.body, rewritten, edits);
            }
        }
    }
    Ok(())
}

/// Extract `PSpice` device initialization from a normalized, linked artifact.
///
/// Unlike textual replacement with a permanent source, the resulting policy
/// is selected separately for each elaborated device and enclosing analysis.
/// The number and order of physical lines are unchanged.
///
/// # Errors
/// Invalid IC syntax, malformed or ambiguous scopes,
/// invalid source options or exhausted work budget.
pub fn capture_initialization(
    mapped: &mut MappedNormalization,
    linked: &LinkedSource,
    kind: RootKind,
    budget: &WorkBudget,
) -> Result<Option<InitialConditions>> {
    if !linked
        .visits
        .iter()
        .any(|visit| visit.dialect == SourceDialect::Pspice)
    {
        return Ok(None);
    }
    let bytes = &mut mapped.model.bytes;
    budget.charge(Resource::ScannedBytes, bytes.len())?;
    let start = if kind == RootKind::Deck {
        bytes
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(bytes.len(), |i| i + 1)
    } else {
        0
    };
    let body = &bytes[start..];
    let statements = lexical::statements(body)?;
    let origins = &mapped.input_lines[usize::from(kind == RootKind::Deck)..];
    let dialects = SourceDialects::Linked(linked);
    let source = Source {
        statements: &statements,
        scopes: Scopes::collect(&statements, origins, dialects, budget)?,
        body,
        origins,
        dialects,
    };
    let mut edits = Vec::new();
    let translate = collect_options(&source, &mut edits)?;
    let filename = if kind == RootKind::Deck {
        "deck.cir"
    } else {
        "model.lib"
    };
    let mut conditions = InitialConditions::default();
    let mut capture = Capture::new(&mut conditions, filename, budget)?;
    capture_devices(&source, translate, &mut capture, &mut edits)?;
    for (range, _) in &mut edits {
        range.start += start;
        range.end += start;
    }
    if let Some(rewritten) = lexical::apply_budgeted_edits(bytes, edits, budget)? {
        *bytes = rewritten;
    }
    conditions.validate_artifact(filename, &bytes[start..], budget)?;
    Ok((!conditions.devices.is_empty()).then_some(conditions))
}
