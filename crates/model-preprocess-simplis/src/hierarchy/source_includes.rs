//! Execute explicitly selected source resources in the current SIMPLIS
//! preprocessing environment, independently of indexed model lookup.

use super::{
    Definition, Environment, Extracted, PreparedRoot, SubcircuitCatalog,
    extract_definitions_scoped, is_directive, is_subcircuit_header, logical_statements,
};
use crate::{Substitution, run_preprocess_body_with_includes};
use anyhow::{Context, Result, ensure};
use model_preprocess_command::Resource;
use std::collections::BTreeSet;

struct ActiveSource {
    id: String,
    name: String,
}

pub(crate) struct RootIncludes<'a> {
    catalog: &'a SubcircuitCatalog,
    active: Vec<ActiveSource>,
    definitions: Vec<Definition>,
    imported: BTreeSet<String>,
}

impl<'a> RootIncludes<'a> {
    fn new(catalog: &'a SubcircuitCatalog) -> Self {
        Self {
            catalog,
            active: Vec::new(),
            definitions: Vec::new(),
            imported: BTreeSet::new(),
        }
    }

    pub(crate) fn included(&self) -> bool {
        !self.active.is_empty()
    }

    pub(crate) fn enter(&mut self, target: &str) -> Result<Option<String>> {
        let parent = self.active.last().map(|source| source.name.as_str());
        let Some((id, name, metadata)) = self.catalog.explicit_include(target, parent)? else {
            return Ok(None);
        };
        ensure!(
            !self.active.iter().any(|source| source.name == name),
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
        });
        self.imported.insert(id.to_owned());
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
        let bytes = lines[start..stop].join("\n");
        budget.charge(Resource::EmittedBytes, bytes.len())?;
        let mut extracted = extract_definitions_scoped(&bytes, None, budget)?;
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

pub(super) fn prepare_root(text: &str, external: &SubcircuitCatalog) -> Result<PreparedRoot> {
    let mut includes = RootIncludes::new(external);
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
