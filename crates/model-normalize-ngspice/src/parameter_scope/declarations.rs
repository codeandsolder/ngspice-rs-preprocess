//! Lexical declaration visibility before dialect-specific expression parsing.
//!
//! The index does not evaluate values or infer call-site parameter inheritance.
//! It distinguishes static parameter names and model declarations from opaque
//! positional identifiers, including forward declarations and nested scopes.

use crate::{
    lexical::{Statement, tokens},
    model_card::{parse_model_card, parse_parameter_fields},
};
use anyhow::{Context, Result, ensure};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default)]
struct Scope {
    parent: Option<usize>,
    name: Option<Vec<u8>>,
    parameters: BTreeSet<Vec<u8>>,
    conditional_parameters: BTreeSet<Vec<u8>>,
    models: BTreeMap<Vec<u8>, Vec<Model>>,
}

struct Model {
    family: Vec<u8>,
    conditional: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Visibility<T> {
    Missing,
    Unique(T),
    Ambiguous,
}

pub struct Declarations {
    scopes: Vec<Scope>,
    boundaries: Vec<(usize, usize, bool)>,
}

fn assignment_names(text: &[u8]) -> Result<Vec<Vec<u8>>> {
    Ok(parse_parameter_fields(std::str::from_utf8(text)?, &[])?
        .into_iter()
        .filter(|parameter| parameter.span.explicit_equals)
        .map(|parameter| parameter.name.into_bytes().to_ascii_lowercase())
        .collect())
}

pub struct DeclarationError {
    pub line: usize,
    pub error: anyhow::Error,
}

impl Declarations {
    pub(crate) fn new(statements: &[Statement]) -> std::result::Result<Self, DeclarationError> {
        let mut result = Self {
            scopes: vec![Scope::default()],
            boundaries: vec![(0, 0, false)],
        };
        let mut scope = 0;
        let mut conditional_depth = 0usize;
        let mut control = false;
        for statement in statements {
            let mut record = || -> Result<()> {
                let text = &statement.text;
                let name = text
                    .split(|byte| byte.is_ascii_whitespace() || *byte == b'(')
                    .next()
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                if name == b".control" {
                    control = true;
                } else if name == b".endc" {
                    control = false;
                    return Ok(());
                }
                if control {
                    return Ok(());
                }
                match name.as_slice() {
                    b".subckt" => {
                        scope = result.open_scope(scope, statement, conditional_depth != 0)?;
                    }
                    b".ends" => {
                        scope = result.close_scope(scope, statement, conditional_depth != 0)?;
                    }
                    b".if" => {
                        conditional_depth += 1;
                        result.boundaries.push((statement.line, scope, true));
                    }
                    b".endif" => {
                        conditional_depth = conditional_depth
                            .checked_sub(1)
                            .context("unmatched .endif")?;
                        result
                            .boundaries
                            .push((statement.line, scope, conditional_depth != 0));
                    }
                    b".param" => {
                        result.record_parameters(
                            scope,
                            &text[name.len()..],
                            conditional_depth != 0,
                        )?;
                    }
                    b".model" => {
                        let card =
                            parse_model_card(std::str::from_utf8(text)?, statement.line, &[])?;
                        result.scopes[scope]
                            .models
                            .entry(card.name.into_bytes().to_ascii_lowercase())
                            .or_default()
                            .push(Model {
                                family: card.family.into_bytes().to_ascii_lowercase(),
                                conditional: conditional_depth != 0,
                            });
                    }
                    _ => {}
                }
                Ok(())
            };
            record().map_err(|error| DeclarationError {
                line: statement.line,
                error,
            })?;
        }
        if scope != 0 || conditional_depth != 0 {
            return Err(DeclarationError {
                line: statements.last().map_or(1, |statement| statement.line),
                error: anyhow::anyhow!("unclosed subcircuit or conditional declaration"),
            });
        }
        Ok(result)
    }

    fn open_scope(
        &mut self,
        parent: usize,
        statement: &Statement,
        conditional: bool,
    ) -> Result<usize> {
        let text = &statement.text;
        let fields = tokens(text)?;
        let identifier = fields.get(1).context("missing subcircuit name")?;
        let scope = self.scopes.len();
        self.scopes.push(Scope {
            parent: Some(parent),
            name: Some(text[identifier.clone()].to_ascii_lowercase()),
            ..Scope::default()
        });
        self.boundaries.push((statement.line, scope, conditional));
        self.record_parameters(scope, &text[identifier.end..], conditional)?;
        Ok(scope)
    }

    fn close_scope(
        &mut self,
        scope: usize,
        statement: &Statement,
        conditional: bool,
    ) -> Result<usize> {
        let text = &statement.text;
        let fields = tokens(text)?;
        let current = &self.scopes[scope];
        if let Some(identifier) = fields.get(1) {
            ensure!(
                current
                    .name
                    .as_ref()
                    .is_some_and(|name| name.eq_ignore_ascii_case(&text[identifier.clone()])),
                "line {}: mismatched subcircuit end",
                statement.line
            );
        }
        let parent = current
            .parent
            .with_context(|| format!("line {}: unmatched .ends", statement.line))?;
        self.boundaries.push((statement.line, parent, conditional));
        Ok(parent)
    }

    fn record_parameters(&mut self, scope: usize, text: &[u8], conditional: bool) -> Result<()> {
        let names = assignment_names(text)?;
        if conditional {
            self.scopes[scope].conditional_parameters.extend(names);
        } else {
            self.scopes[scope].parameters.extend(names);
        }
        Ok(())
    }

    pub(crate) fn unconditional_root(&self, line: usize) -> bool {
        let boundary = self
            .boundaries
            .partition_point(|(start, _, _)| *start <= line);
        self.boundaries
            .get(boundary.saturating_sub(1))
            .is_some_and(|(_, scope, conditional)| *scope == 0 && !conditional)
    }

    fn ancestry(&self, line: usize) -> impl Iterator<Item = &Scope> {
        let boundary = self
            .boundaries
            .partition_point(|(start, _, _)| *start <= line);
        let scope = boundary
            .checked_sub(1)
            .map(|index| self.boundaries[index].1);
        std::iter::successors(scope, |scope| self.scopes[*scope].parent)
            .map(|scope| &self.scopes[scope])
    }

    pub(crate) fn scope_names(&self, line: usize) -> Result<Vec<String>> {
        let mut names = self
            .ancestry(line)
            .filter_map(|scope| scope.name.as_deref())
            .map(|name| Ok(std::str::from_utf8(name)?.to_owned()))
            .collect::<Result<Vec<_>>>()?;
        names.reverse();
        Ok(names)
    }

    pub(crate) fn model(&self, line: usize, name: &[u8]) -> Visibility<&[u8]> {
        let name = name.to_ascii_lowercase();
        for scope in self.ancestry(line) {
            if let Some(models) = scope.models.get(&name) {
                return match models.as_slice() {
                    [model] if !model.conditional => Visibility::Unique(&model.family),
                    _ => Visibility::Ambiguous,
                };
            }
        }
        Visibility::Missing
    }

    pub(crate) fn parameter(&self, line: usize, name: &[u8]) -> Visibility<()> {
        let name = name.to_ascii_lowercase();
        for scope in self.ancestry(line) {
            if scope.parameters.contains(&name) {
                return Visibility::Unique(());
            }
            if scope.conditional_parameters.contains(&name) {
                return Visibility::Ambiguous;
            }
        }
        Visibility::Missing
    }
}
