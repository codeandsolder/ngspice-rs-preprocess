//! Lexical model and function visibility before physical model lowering.

use crate::{
    SourceDialects, lexical,
    linked_source::SourceDialect,
    model_card::{ModelCard, parse_model_card},
};
use anyhow::{Context, Result, ensure};
use model_preprocess_command::{Resource, WorkBudget};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default)]
struct Scope {
    parent: Option<usize>,
    path: Vec<String>,
    models: BTreeMap<String, Vec<usize>>,
    functions: BTreeSet<String>,
    instances: BTreeSet<String>,
}

struct Declaration {
    scope: usize,
    card: ModelCard,
    base: Option<String>,
    dialect: SourceDialect,
}

pub(super) struct Scopes {
    budget: WorkBudget,
    entries: Vec<Scope>,
    lines: BTreeMap<usize, usize>,
    models: BTreeMap<usize, Declaration>,
    global_ground: bool,
}

fn parse_model(text: &str, line: usize) -> Result<(ModelCard, Option<String>)> {
    let spans = lexical::tokens(text.as_bytes())?;
    let family = spans.get(2).context("missing PSpice model family")?;
    let token = &text[family.clone()];
    if token
        .get(..4)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("ako:"))
    {
        let (base, end) = if token.len() == 4 {
            let name = spans.get(3).context("missing PSpice AKO base model")?;
            (&text[name.clone()], name.end)
        } else {
            (&token[4..], family.end)
        };
        ensure!(!base.is_empty(), "missing PSpice AKO base model");
        let direct = format!("{}{}", &text[..family.start], &text[end..]);
        return Ok((
            parse_model_card(&direct, line, &[])?,
            Some(base.to_ascii_lowercase()),
        ));
    }
    Ok((parse_model_card(text, line, &[])?, None))
}

impl Scopes {
    pub(super) fn collect(
        statements: &[lexical::Statement],
        origins: &[usize],
        dialects: SourceDialects<'_>,
        budget: &WorkBudget,
    ) -> Result<Self> {
        let mut result = Self {
            budget: budget.clone(),
            entries: vec![Scope::default()],
            lines: BTreeMap::new(),
            models: BTreeMap::new(),
            global_ground: false,
        };
        let mut scope = 0usize;
        let mut control = false;
        for statement in statements {
            budget.charge(Resource::Statements, 1)?;
            budget.charge(Resource::ScannedBytes, statement.text.len())?;
            budget.charge(Resource::GeneratedNodes, 1)?;
            let text = std::str::from_utf8(&statement.text)?;
            let first = text
                .split_ascii_whitespace()
                .next()
                .unwrap_or_default()
                .to_ascii_lowercase();
            if first == ".control" {
                control = true;
            }
            if first == ".endc" {
                control = false;
                continue;
            }
            if control {
                continue;
            }
            if first == ".subckt" {
                let parent = scope;
                scope = result.entries.len();
                let parent_path = &result.entries[parent].path;
                budget.check_depth(parent_path.len().saturating_add(1))?;
                charge_scope(parent_path, budget)?;
                let mut path = parent_path.clone();
                let name = text
                    .split_ascii_whitespace()
                    .nth(1)
                    .context("missing PSpice subcircuit name")?;
                budget.charge(Resource::EmittedBytes, name.len())?;
                budget.charge(Resource::GeneratedNodes, 1)?;
                path.push(name.to_owned());
                result.entries.push(Scope {
                    parent: Some(parent),
                    path,
                    ..Scope::default()
                });
            }
            result.lines.insert(statement.line, scope);
            if first == ".ends" {
                scope = result.entries[scope]
                    .parent
                    .context("unmatched PSpice .ENDS")?;
            } else if first == ".model" {
                let dialect = dialects.at_mapped(origins, statement.line)?;
                let (card, base) = if dialect.is_pspice() {
                    parse_model(text, statement.line)
                        .with_context(|| dialects.describe_mapped(origins, statement.line))?
                } else {
                    (
                        parse_model_card(text, statement.line, &[])
                            .with_context(|| dialects.describe_mapped(origins, statement.line))?,
                        None,
                    )
                };
                charge_card(&card, budget)?;
                result.entries[scope]
                    .models
                    .entry(card.name.to_ascii_lowercase())
                    .or_default()
                    .push(statement.line);
                result.models.insert(
                    statement.line,
                    Declaration {
                        scope,
                        card,
                        base,
                        dialect,
                    },
                );
            } else if first == ".global" {
                result.global_ground |= text
                    .split_ascii_whitespace()
                    .skip(1)
                    .any(|node| node.eq_ignore_ascii_case("gnd"));
            } else if first.starts_with('x') {
                result.entries[scope].instances.insert(first);
            } else if first == ".func" {
                let name = text[first.len()..]
                    .trim_start()
                    .split(|c: char| c.is_ascii_whitespace() || c == '(')
                    .next()
                    .filter(|name| !name.is_empty())
                    .context("missing PSpice function name")?;
                result.entries[scope]
                    .functions
                    .insert(name.to_ascii_lowercase());
            }
        }
        ensure!(scope == 0, "unterminated PSpice subcircuit");
        Ok(result)
    }

    pub(super) const fn budget(&self) -> &WorkBudget {
        &self.budget
    }

    pub(super) fn scope(&self, line: usize) -> Result<usize> {
        self.lines
            .get(&line)
            .copied()
            .context("missing PSpice source scope")
    }

    pub(super) fn path(&self, scope: usize) -> &[String] {
        &self.entries[scope].path
    }

    pub(super) const fn local_ground(&self, scope: usize) -> bool {
        scope != 0 && !self.global_ground
    }

    pub(super) fn hierarchical_ground(&self, scope: usize, node: &str) -> bool {
        if self.global_ground {
            return false;
        }
        let Some((instance, _)) = node.split_once('.') else {
            return false;
        };
        self.entries[scope]
            .instances
            .contains(&instance.to_ascii_lowercase())
            && node
                .rsplit_once('.')
                .is_some_and(|(_, last)| last.eq_ignore_ascii_case("gnd"))
    }

    pub(super) fn shadows(&self, mut scope: usize, name: &str) -> Result<bool> {
        loop {
            self.budget.charge(Resource::EvaluationSteps, 1)?;
            if self.entries[scope].functions.contains(name) {
                return Ok(true);
            }
            let Some(parent) = self.entries[scope].parent else {
                return Ok(false);
            };
            scope = parent;
        }
    }

    pub(super) fn find_model(&self, mut scope: usize, name: &str) -> Result<Option<usize>> {
        loop {
            self.budget.charge(Resource::EvaluationSteps, 1)?;
            if let Some(lines) = self.entries[scope].models.get(name) {
                ensure!(lines.len() == 1, "ambiguous scoped PSpice model {name}");
                return Ok(lines.first().copied());
            }
            let Some(parent) = self.entries[scope].parent else {
                return Ok(None);
            };
            scope = parent;
        }
    }

    pub(super) fn model_declarations(&self) -> impl Iterator<Item = (usize, &str)> {
        self.models
            .iter()
            .filter(|(_, declaration)| declaration.dialect.is_pspice())
            .map(|(&line, declaration)| (line, declaration.card.family.as_str()))
    }

    pub(super) fn is_pspice_model(&self, line: usize) -> bool {
        self.models
            .get(&line)
            .is_some_and(|model| model.dialect.is_pspice())
    }

    pub(super) fn model(&self, line: usize) -> Result<ModelCard> {
        // Resolve iteratively, avoiding recursive stack use on long AKO chains.
        let mut stack = Vec::new();
        let mut active = BTreeSet::new();
        let mut current = line;
        loop {
            self.budget.charge(Resource::EvaluationSteps, 1)?;
            self.budget.charge(Resource::GeneratedNodes, 1)?;
            ensure!(
                active.insert(current),
                "cyclic PSpice AKO model inheritance"
            );
            let declaration = self
                .models
                .get(&current)
                .context("missing PSpice model declaration")?;
            stack.push(declaration);
            let Some(base) = &declaration.base else {
                break;
            };
            current = self
                .find_model(declaration.scope, base)?
                .with_context(|| format!("missing PSpice AKO base model {base}"))?;
            ensure!(
                self.is_pspice_model(current),
                "PSpice AKO model inheritance across source dialects needs a qualified physical model contract"
            );
        }
        let base = stack.pop().context("empty PSpice model chain")?;
        charge_card(&base.card, &self.budget)?;
        let mut model = base.card.clone();
        super::temperature::validate_base(&model)?;
        for derived in stack.into_iter().rev() {
            ensure!(
                derived.card.family.eq_ignore_ascii_case(&model.family),
                "PSpice AKO model {} changes base family {} to {}",
                derived.card.name,
                model.family,
                derived.card.family
            );
            charge_card(&derived.card, &self.budget)?;
            model.name.clone_from(&derived.card.name);
            model.source_line = derived.card.source_line;
            super::temperature::inherit(&mut model, &derived.card, &self.budget)?;
        }
        Ok(model)
    }
}

pub(super) fn charge_scope(scope: &[String], budget: &WorkBudget) -> Result<()> {
    budget.charge(Resource::GeneratedNodes, scope.len())?;
    for part in scope {
        budget.charge(Resource::EmittedBytes, part.len())?;
    }
    Ok(())
}

fn charge_card(card: &ModelCard, budget: &WorkBudget) -> Result<()> {
    budget.charge(Resource::GeneratedNodes, card.parameters.len())?;
    budget.charge(Resource::EvaluationSteps, card.parameters.len())?;
    budget.charge(
        Resource::EmittedBytes,
        card.name.len().saturating_add(card.family.len()),
    )?;
    for field in &card.parameters {
        budget.charge(
            Resource::EmittedBytes,
            field
                .name
                .len()
                .saturating_add(field.expression.as_ref().map_or(0, String::len)),
        )?;
    }
    Ok(())
}
