//! Execute explicitly selected source resources in the current SIMPLIS
//! preprocessing environment, independently of indexed model lookup.

use super::{
    Definition, Environment, Extracted, IncludedSource, PreparedRoot, SubcircuitCatalog,
    directive_name, extract_definitions_scoped, is_directive, is_preprocessor_control,
    is_subcircuit_header, logical_statements, normalized_source_path,
};
use crate::{Substitution, run_preprocess_body_with_includes};
use anyhow::{Context, Result, ensure};
use model_preprocess_command::{Resource, WorkBudget};
use std::collections::BTreeSet;

struct ActiveSource {
    id: String,
    name: String,
    ended: bool,
}

pub(crate) struct RootIncludes<'a> {
    catalog: &'a SubcircuitCatalog,
    active: Vec<ActiveSource>,
    definitions: Vec<Definition>,
    imported: BTreeSet<IncludedSource>,
    origin: Option<String>,
    owner: Option<String>,
}

impl<'a> RootIncludes<'a> {
    pub(super) fn new(catalog: &'a SubcircuitCatalog) -> Self {
        Self {
            catalog,
            active: Vec::new(),
            definitions: Vec::new(),
            imported: BTreeSet::new(),
            origin: None,
            owner: None,
        }
    }

    pub(super) fn for_definition(catalog: &'a SubcircuitCatalog, definition: &Definition) -> Self {
        Self {
            origin: definition.source_name.clone(),
            owner: Some(definition.scope_id.clone()),
            ..Self::new(catalog)
        }
    }

    pub(crate) fn clear_pass(&mut self) {
        self.active.clear();
        self.definitions.clear();
        self.imported.clear();
    }

    pub(crate) fn ended(&self) -> bool {
        self.active.last().is_some_and(|source| source.ended)
    }

    pub(crate) fn finish_source(&mut self, line: &str) -> bool {
        if let Some(source) = self.active.last_mut() {
            if is_directive(line, "END") {
                source.ended = true;
                return true;
            }
            if is_directive(line, "SYSTEMLIB") {
                return true;
            }
        }
        false
    }

    pub(super) fn into_captures(self) -> (Vec<Definition>, BTreeSet<IncludedSource>) {
        (self.definitions, self.imported)
    }

    pub(crate) fn enter(&mut self, target: &str) -> Result<Option<String>> {
        let parent = self
            .active
            .last()
            .map(|source| source.name.as_str())
            .or(self.origin.as_deref());
        let Some((id, name, metadata)) = self.catalog.explicit_include(target, parent)? else {
            return Ok(None);
        };
        ensure!(
            !self
                .active
                .iter()
                .any(|source| normalized_source_path(&source.name) == normalized_source_path(name)),
            "recursive SIMPLIS source include {name:?}"
        );
        let budget = &self.catalog.budget;
        budget.check_depth(self.active.len().saturating_add(1))?;
        budget.charge(Resource::SourceVisits, 1)?;
        budget.charge(Resource::ScannedBytes, metadata.explicit_source.len())?;
        budget.charge(
            Resource::EmittedBytes,
            metadata
                .explicit_source
                .len()
                .saturating_add(id.len())
                .saturating_add(name.len()),
        )?;
        self.active.push(ActiveSource {
            id: id.to_owned(),
            name: name.to_owned(),
            ended: false,
        });
        self.imported.insert(IncludedSource {
            source_id: id.to_owned(),
            source_name: name.to_owned(),
        });
        Ok(Some(metadata.explicit_source.clone()))
    }

    pub(crate) fn leave(&mut self) {
        self.active.pop();
    }

    /// Definitions are declarations, not instructions in the importing
    /// lexical environment. Preserve the body for instance specialization.
    /// An inactive root branch never reaches this method.
    pub(crate) fn capture_definition(
        &mut self,
        lines: &[&str],
        start: usize,
        end: usize,
    ) -> Result<Option<usize>> {
        if !is_subcircuit_header(lines[start]) {
            return Ok(None);
        }
        let budget = &self.catalog.budget;
        let mut depth = 0usize;
        let mut stop = None;
        for (index, line) in lines.iter().enumerate().take(end).skip(start) {
            budget.charge(Resource::Statements, 1)?;
            budget.charge(Resource::ScannedBytes, line.len())?;
            if is_subcircuit_header(line) {
                depth += 1;
                budget.check_depth(depth)?;
            } else if is_directive(line, "ENDS") {
                depth = depth.checked_sub(1).context("unmatched SIMPLIS .ENDS")?;
                if depth == 0 {
                    stop = Some(index + 1);
                    break;
                }
            }
        }
        let stop = stop.context("unterminated SIMPLIS .SUBCKT in active source")?;
        let bytes_len = lines[start..stop]
            .iter()
            .map(|line| line.len().saturating_add(1))
            .fold(0usize, usize::saturating_add);
        budget.charge(Resource::EmittedBytes, bytes_len)?;
        let bytes = lines[start..stop].join("\n");
        let mut extracted = extract_definitions_scoped(&bytes, self.owner.as_deref(), budget)?;
        if let Some(source) = self.active.last() {
            for definition in &mut extracted.definitions {
                budget.charge(
                    Resource::EmittedBytes,
                    source.id.len().saturating_add(source.name.len()),
                )?;
                definition.source_id = Some(source.id.clone());
                definition.source_name = Some(source.name.clone());
            }
        }
        self.definitions.extend(extracted.definitions);
        Ok(Some(stop))
    }
}

pub(super) fn prepare_root(
    text: &str,
    external: &SubcircuitCatalog,
    origin: Option<&str>,
) -> Result<PreparedRoot> {
    let mut includes = RootIncludes::new(external);
    includes.origin = origin.map(str::to_owned);
    let processed = run_preprocess_body_with_includes(
        text.as_bytes(),
        &Environment::with_budget(external.budget.clone()),
        Substitution::Known,
        false,
        &mut BTreeSet::new(),
        Some(&mut includes),
    )
    .context("executing root SIMPLIS source includes")?;
    let root_lines = std::str::from_utf8(&processed.bytes)?
        .lines()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let statements = logical_statements(&root_lines);
    Ok(PreparedRoot {
        extracted: Extracted {
            root_lines,
            definitions: includes.definitions,
        },
        statements,
        globals: processed.environment.globals,
        explicit_sources: includes.imported,
    })
}

/// Index only declarations available without executing the source program.
/// Keep subcircuit bodies intact: their compile-time program belongs to the
/// occurrence, not the library's top-level lexical scope.
pub(super) fn indexed_view(text: &str, budget: &WorkBudget) -> Result<String> {
    let mut output = String::new();
    let mut blocks = Vec::new();
    let mut depth = 0usize;
    let mut previous = false;
    for line in text.lines() {
        budget.charge(Resource::ScannedBytes, line.len())?;
        budget.charge(Resource::Statements, 1)?;
        let directive = directive_name(line).map(str::to_ascii_uppercase);
        let emit = if is_subcircuit_header(line) {
            depth = depth.saturating_add(1);
            budget.check_depth(depth)?;
            blocks.is_empty()
        } else if depth > 0 {
            if is_directive(line, "ENDS") {
                depth -= 1;
            }
            blocks.is_empty()
        } else if line.trim_start().starts_with('+') {
            previous
        } else {
            match directive.as_deref() {
                Some("IF" | "WHILE" | "FOR") => {
                    let end = match directive.as_deref() {
                        Some("IF") => "ENDIF",
                        Some("WHILE") => "ENDWHILE",
                        _ => "ENDFOR",
                    };
                    blocks.push(end);
                    budget.check_depth(blocks.len())?;
                    false
                }
                Some("ENDIF" | "ENDWHILE" | "ENDFOR") => {
                    ensure!(
                        blocks.pop() == directive.as_deref(),
                        "unbalanced library preprocessing block"
                    );
                    false
                }
                Some("ELSE") => {
                    ensure!(blocks.last() == Some(&"ENDIF"), "unmatched library .ELSE");
                    false
                }
                Some("END") if blocks.is_empty() => break,
                _ => {
                    blocks.is_empty()
                        && !is_preprocessor_control(line)
                        && !is_directive(line, "INCLUDE")
                        && !is_directive(line, "INC")
                }
            }
        };
        previous = emit;
        if emit {
            budget.charge(Resource::EmittedBytes, line.len().saturating_add(1))?;
            output.push_str(line);
            output.push('\n');
        }
    }
    ensure!(
        blocks.is_empty() && depth == 0,
        "unterminated library lexical block"
    );
    Ok(output)
}
