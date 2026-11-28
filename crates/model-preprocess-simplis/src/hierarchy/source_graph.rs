//! Admitted catalog provenance. Graph topology participates in content
//! identity, but relocation and physical line numbers do not.
use crate::source_dialect::ScopedStatement;
use anyhow::{Context, Result, ensure};
use model_normalize_ngspice::linked_source::{LinkedLocation, SourceGraph};
use model_preprocess_command::{Resource, WorkBudget};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

/// Physical graph and one location for every retained logical source row.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LibrarySourceGraph {
    /// Independently admitted resource graph.
    pub graph: SourceGraph,
    /// Locations in the exact order of the library's scoped statements.
    pub locations: Vec<LinkedLocation>,
}

fn index(hasher: &mut Sha256, index: usize) {
    hasher.update(index.to_string().as_bytes());
    hasher.update([0]);
}

fn parent(hasher: &mut Sha256, parent: Option<usize>) {
    if let Some(parent) = parent {
        hasher.update([1]);
        index(hasher, parent);
    } else {
        hasher.update([0]);
    }
}

impl LibrarySourceGraph {
    pub(super) fn validate(
        &self,
        statements: &[ScopedStatement],
        sources: &BTreeSet<String>,
    ) -> Result<()> {
        self.graph.validate()?;
        ensure!(
            self.locations.len() == statements.len(),
            "catalog statement graph length mismatch"
        );
        ensure!(
            self.graph
                .visits
                .iter()
                .filter(|visit| visit.parent.is_none())
                .count()
                == 1,
            "catalog source graph must have one resource root"
        );
        for visit in &self.graph.visits {
            ensure!(
                sources.contains(visit.path.to_string_lossy().as_ref()),
                "catalog graph visit has no admitted source"
            );
        }
        for (statement, location) in statements.iter().zip(&self.locations) {
            let visit = self
                .graph
                .visits
                .get(location.visit)
                .context("missing catalog source visit")?;
            ensure!(
                location.scope < self.graph.scopes.len()
                    && statement.source == visit.path.to_string_lossy()
                    && statement.context.language == visit.dialect
                    && statement.context.separator == visit.separator,
                "catalog statement does not match its source graph"
            );
        }
        Ok(())
    }

    pub(super) fn hash(&self, hasher: &mut Sha256) {
        index(hasher, self.graph.visits.len());
        for visit in &self.graph.visits {
            parent(hasher, visit.parent);
            index(hasher, visit.scope);
            hasher.update([visit.dialect.stable_id()]);
            hasher.update(visit.separator.map_or(0, u32::from).to_be_bytes());
            if let Some(section) = &visit.section {
                hasher.update([1]);
                index(hasher, section.len());
                hasher.update(section.as_bytes());
            } else {
                hasher.update([0]);
            }
        }
        index(hasher, self.graph.scopes.len());
        for scope in &self.graph.scopes {
            parent(hasher, scope.parent);
            if let Some(name) = &scope.name {
                index(hasher, name.len());
                hasher.update(name.as_bytes());
            } else {
                index(hasher, 0);
            }
            if let Some(declaration) = &scope.declaration {
                parent(hasher, Some(declaration.visit));
            } else {
                parent(hasher, None);
            }
        }
        index(hasher, self.locations.len());
        for location in &self.locations {
            index(hasher, location.visit);
            index(hasher, location.scope);
        }
    }

    pub(super) fn charge_scan(&self, budget: &WorkBudget) -> Result<()> {
        budget.charge(
            Resource::EvaluationSteps,
            self.graph
                .visits
                .len()
                .saturating_add(self.graph.scopes.len())
                .saturating_add(self.locations.len()),
        )?;
        for visit in &self.graph.visits {
            budget.charge(
                Resource::ScannedBytes,
                visit
                    .path
                    .as_os_str()
                    .len()
                    .saturating_add(visit.section.as_ref().map_or(0, String::len)),
            )?;
        }
        for scope in &self.graph.scopes {
            budget.charge(
                Resource::ScannedBytes,
                scope.name.as_ref().map_or(0, String::len),
            )?;
            if let Some(declaration) = &scope.declaration {
                budget.charge(Resource::ScannedBytes, declaration.path.as_os_str().len())?;
            }
        }
        Ok(())
    }
}
