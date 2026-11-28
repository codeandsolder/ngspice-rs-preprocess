//! Shared graph provenance for source streams, catalog members and artifacts.
use super::{SourceScope, SourceVisit};
use anyhow::{Context, Result, ensure};
use model_preprocess_command::{Resource, WorkBudget};
use serde::{Deserialize, Serialize};

/// Stable indices into the admitted source graph.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LinkedLocation {
    /// Resource occurrence index.
    pub visit: usize,
    /// Electrical declaration scope index.
    pub scope: usize,
}

/// Admitted source occurrence forest and electrical lexical scopes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceGraph {
    /// Resource visits in admission order; independent catalog roots have no parent.
    pub visits: Vec<SourceVisit>,
    /// Lexical scopes in declaration order, starting with the shared deck scope.
    pub scopes: Vec<SourceScope>,
}

/// Translation from one admitted graph to its appended position in a deck.
pub struct GraphRemap {
    visit_offset: usize,
    scope_offset: usize,
    visits: usize,
    scopes: usize,
}

impl GraphRemap {
    fn visit(&self, visit: usize) -> Result<usize> {
        ensure!(visit < self.visits, "invalid source visit");
        self.visit_offset
            .checked_add(visit)
            .context("source visit overflow")
    }

    fn scope(&self, scope: usize) -> Result<usize> {
        ensure!(scope < self.scopes, "invalid source scope");
        if scope == 0 {
            Ok(0)
        } else {
            self.scope_offset
                .checked_add(scope)
                .context("source scope overflow")
        }
    }

    /// Translate a declaration location into the combined graph.
    ///
    /// # Errors
    /// Invalid source index or arithmetic overflow.
    pub fn location(&self, location: LinkedLocation) -> Result<LinkedLocation> {
        Ok(LinkedLocation {
            visit: self.visit(location.visit)?,
            scope: self.scope(location.scope)?,
        })
    }
}

impl SourceGraph {
    /// Check the provenance forest and lexical declaration identities.
    ///
    /// # Errors
    /// Missing root, invalid parent or inconsistent declaration location.
    pub fn validate(&self) -> Result<()> {
        let root = self.scopes.first().context("missing source scope root")?;
        ensure!(
            root.parent.is_none() && root.name.is_none() && root.declaration.is_none(),
            "invalid source scope root"
        );
        ensure!(!self.visits.is_empty(), "missing source visit root");
        for (index, visit) in self.visits.iter().enumerate() {
            ensure!(
                visit.parent.is_none_or(|parent| parent < index)
                    && visit.scope < self.scopes.len()
                    && (visit.parent.is_some() || visit.scope == 0),
                "invalid parent or scope of source visit {index}"
            );
        }
        for (index, scope) in self.scopes.iter().enumerate().skip(1) {
            let origin = scope
                .declaration
                .as_ref()
                .context("missing source scope declaration")?;
            let visit = self
                .visits
                .get(origin.visit)
                .context("missing scope declaration visit")?;
            ensure!(
                scope.parent.is_some_and(|parent| parent < index)
                    && scope.name.as_ref().is_some_and(|name| !name.is_empty())
                    && origin.scope == index
                    && origin.line > 0
                    && origin.path == visit.path,
                "invalid source scope declaration {index}"
            );
        }
        Ok(())
    }

    /// Append an independently admitted catalog graph, sharing the receiving
    /// deck's root electrical scope but retaining independent resource roots.
    ///
    /// # Errors
    /// Invalid provenance or exhausted work budget.
    pub fn append_root(&mut self, source: &Self, budget: &WorkBudget) -> Result<GraphRemap> {
        let scope_offset = self
            .scopes
            .len()
            .checked_sub(1)
            .context("missing receiving root scope")?;
        budget.charge(
            Resource::EvaluationSteps,
            source.visits.len().saturating_add(source.scopes.len()),
        )?;
        source.validate()?;
        let remap = GraphRemap {
            visit_offset: self.visits.len(),
            scope_offset,
            visits: source.visits.len(),
            scopes: source.scopes.len(),
        };
        budget.charge(
            Resource::GeneratedNodes,
            source
                .visits
                .len()
                .saturating_add(source.scopes.len().saturating_sub(1)),
        )?;
        for visit in &source.visits {
            budget.charge(
                Resource::EmittedBytes,
                visit
                    .path
                    .as_os_str()
                    .len()
                    .saturating_add(visit.section.as_ref().map_or(0, String::len)),
            )?;
            let mut visit = visit.clone();
            visit.parent = visit.parent.map(|parent| remap.visit(parent)).transpose()?;
            visit.scope = remap.scope(visit.scope)?;
            self.visits.push(visit);
        }
        for scope in source.scopes.iter().skip(1) {
            let declaration = scope
                .declaration
                .as_ref()
                .context("missing source declaration")?;
            budget.charge(
                Resource::EmittedBytes,
                declaration
                    .path
                    .as_os_str()
                    .len()
                    .saturating_add(scope.name.as_ref().map_or(0, String::len)),
            )?;
            let mut scope = scope.clone();
            scope.parent = scope.parent.map(|parent| remap.scope(parent)).transpose()?;
            let declaration = scope
                .declaration
                .as_mut()
                .context("missing cloned source declaration")?;
            declaration.visit = remap.visit(declaration.visit)?;
            declaration.scope = remap.scope(declaration.scope)?;
            self.scopes.push(scope);
        }
        Ok(remap)
    }
}
