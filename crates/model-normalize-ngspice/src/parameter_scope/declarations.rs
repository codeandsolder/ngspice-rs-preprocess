//! Lexical declaration visibility before dialect-specific expression parsing.
//!
//! The index does not evaluate values or infer call-site parameter inheritance.
//! It distinguishes static parameter names and model declarations from opaque
//! positional identifiers, including forward declarations and nested scopes.

use crate::{
    lexical::{Statement, tokens},
    model_card::parse_model_card,
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
    boundaries: Vec<(usize, usize)>,
}

fn assignment_names(text: &[u8]) -> Result<Vec<Vec<u8>>> {
    let fields = tokens(text)?;
    Ok(fields
        .windows(3)
        .filter(|fields| {
            let key = &text[fields[0].clone()];
            text[fields[1].clone()] == *b"="
                && text[fields[2].clone()] != *b"="
                && key
                    .first()
                    .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
                && key
                    .iter()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'$' | b'#'))
        })
        .map(|fields| text[fields[0].clone()].to_ascii_lowercase())
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
            boundaries: vec![(0, 0)],
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
                        let fields = tokens(text)?;
                        let next = result.scopes.len();
                        let identifier = fields.get(1).context("missing subcircuit name")?;
                        result.scopes.push(Scope {
                            parent: Some(scope),
                            name: Some(text[identifier.clone()].to_ascii_lowercase()),
                            ..Scope::default()
                        });
                        scope = next;
                        result.boundaries.push((statement.line, scope));
                        result.record_parameters(
                            scope,
                            &text[identifier.end..],
                            conditional_depth != 0,
                        )?;
                    }
                    b".ends" => {
                        let fields = tokens(text)?;
                        let current = &result.scopes[scope];
                        if let Some(identifier) = fields.get(1) {
                            ensure!(
                                current.name.as_ref().is_some_and(
                                    |name| name.eq_ignore_ascii_case(&text[identifier.clone()])
                                ),
                                "line {}: mismatched subcircuit end",
                                statement.line
                            );
                        }
                        scope = current
                            .parent
                            .with_context(|| format!("line {}: unmatched .ends", statement.line))?;
                        result.boundaries.push((statement.line, scope));
                    }
                    b".if" => conditional_depth += 1,
                    b".endif" => {
                        conditional_depth = conditional_depth
                            .checked_sub(1)
                            .context("unmatched .endif")?;
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

    fn record_parameters(&mut self, scope: usize, text: &[u8], conditional: bool) -> Result<()> {
        let names = assignment_names(text)?;
        if conditional {
            self.scopes[scope].conditional_parameters.extend(names);
        } else {
            self.scopes[scope].parameters.extend(names);
        }
        Ok(())
    }

    fn ancestry(&self, line: usize) -> impl Iterator<Item = &Scope> {
        let boundary = self.boundaries.partition_point(|(start, _)| *start <= line);
        let scope = boundary
            .checked_sub(1)
            .map(|index| self.boundaries[index].1);
        std::iter::successors(scope, |scope| self.scopes[*scope].parent)
            .map(|scope| &self.scopes[scope])
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
