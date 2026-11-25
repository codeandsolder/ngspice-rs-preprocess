//! Lexical model and function visibility before physical model lowering.

use crate::{
    lexical,
    model_card::{ModelCard, parse_model_card},
};
use anyhow::{Context, Result, ensure};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default)]
struct Scope {
    parent: Option<usize>,
    models: BTreeMap<String, Vec<usize>>,
    functions: BTreeSet<String>,
    instances: BTreeSet<String>,
}

struct Declaration {
    scope: usize,
    card: ModelCard,
    base: Option<String>,
}

pub(super) struct Scopes {
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
        let base = &token[4..];
        ensure!(!base.is_empty(), "missing PSpice AKO base model");
        let direct = format!("{}{}", &text[..family.start], &text[family.end..]);
        return Ok((
            parse_model_card(&direct, line, &[])?,
            Some(base.to_ascii_lowercase()),
        ));
    }
    Ok((parse_model_card(text, line, &[])?, None))
}

impl Scopes {
    pub(super) fn collect(statements: &[lexical::Statement]) -> Result<Self> {
        let mut result = Self {
            entries: vec![Scope::default()],
            lines: BTreeMap::new(),
            models: BTreeMap::new(),
            global_ground: false,
        };
        let mut scope = 0usize;
        let mut control = false;
        for statement in statements {
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
                result.entries.push(Scope {
                    parent: Some(parent),
                    ..Scope::default()
                });
            }
            result.lines.insert(statement.line, scope);
            if first == ".ends" {
                scope = result.entries[scope]
                    .parent
                    .context("unmatched PSpice .ENDS")?;
            } else if first == ".model" {
                let (card, base) = parse_model(text, statement.line)?;
                result.entries[scope]
                    .models
                    .entry(card.name.to_ascii_lowercase())
                    .or_default()
                    .push(statement.line);
                result
                    .models
                    .insert(statement.line, Declaration { scope, card, base });
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

    pub(super) fn scope(&self, line: usize) -> Result<usize> {
        self.lines
            .get(&line)
            .copied()
            .context("missing PSpice source scope")
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

    pub(super) fn shadows(&self, mut scope: usize, name: &str) -> bool {
        loop {
            if self.entries[scope].functions.contains(name) {
                return true;
            }
            let Some(parent) = self.entries[scope].parent else {
                return false;
            };
            scope = parent;
        }
    }

    pub(super) fn find_model(&self, mut scope: usize, name: &str) -> Result<Option<usize>> {
        loop {
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

    pub(super) fn model(&self, line: usize) -> Result<ModelCard> {
        // Resolve iteratively, avoiding recursive stack use on long AKO chains.
        let mut stack = Vec::new();
        let mut active = BTreeSet::new();
        let mut current = line;
        loop {
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
        }
        let base = stack.pop().context("empty PSpice model chain")?;
        let mut model = base.card.clone();
        for derived in stack.into_iter().rev() {
            ensure!(
                derived.card.family.eq_ignore_ascii_case(&model.family),
                "PSpice AKO model {} changes base family {} to {}",
                derived.card.name,
                model.family,
                derived.card.family
            );
            model.name.clone_from(&derived.card.name);
            model.source_line = derived.card.source_line;
            model
                .parameters
                .extend(derived.card.parameters.iter().cloned());
        }
        Ok(model)
    }
}
