//! Bind function macros to the visible occurrence without expanding bodies.
use super::{Names, Precedence, Rows, inner};
use anyhow::{Context, Result, ensure};
use model_normalize_ngspice::model_card::parse_function_signature;
use model_preprocess_simplis::{BoundExpression, FunctionAlias, rewrite_bound_symbols};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

#[derive(Clone)]
pub(super) struct Function {
    formals: Vec<String>,
    body: String,
    line: usize,
}

pub(super) type Functions = BTreeMap<String, Arc<Function>>;

pub(super) fn collect(text: &str, line: usize, functions: &mut Functions) -> Result<()> {
    let tail = text
        .split_once(char::is_whitespace)
        .context("empty HSPICE function declaration")?
        .1;
    let (signature, body) = parse_function_signature(tail)?;
    let name = signature.name.to_ascii_uppercase();
    ensure!(
        !matches!(name.as_str(), "V" | "I"),
        "function {name} conflicts with an electrical probe"
    );
    functions.insert(
        name,
        Arc::new(Function {
            formals: signature.formals,
            body: inner(body).to_owned(),
            line,
        }),
    );
    Ok(())
}

#[derive(Default)]
pub(super) struct Scope {
    pub(super) names: Names,
    declarations: Functions,
    aliases: BTreeMap<String, FunctionAlias>,
    known: Arc<BTreeSet<String>>,
    inaccessible: Names,
}

impl Scope {
    pub(super) fn bind(
        names: Names,
        parent: Option<&Self>,
        local: Functions,
        precedence: Precedence,
        known: Arc<BTreeSet<String>>,
        mut fresh: impl FnMut(char) -> Result<String>,
        limit: usize,
    ) -> Result<(Self, Rows)> {
        let mut declarations =
            parent.map_or_else(BTreeMap::new, |scope| scope.declarations.clone());
        for (name, definition) in local {
            if precedence == Precedence::Local || !declarations.contains_key(&name) {
                declarations.insert(name, definition);
            }
        }
        let aliases = declarations
            .iter()
            .map(|(name, definition)| {
                Ok((
                    name.clone(),
                    FunctionAlias {
                        name: fresh('f')?,
                        arity: definition.formals.len(),
                    },
                ))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        let mut scope = Self {
            names,
            declarations,
            aliases,
            known,
            inaccessible: Names::new(),
        };
        let rows = scope.render(&mut fresh, limit)?;
        Ok((scope, rows))
    }

    fn render(
        &mut self,
        fresh: &mut impl FnMut(char) -> Result<String>,
        limit: usize,
    ) -> Result<Rows> {
        let mut rows = BTreeMap::new();
        let mut dependencies = BTreeMap::new();
        let mut remaining = limit;
        for (name, function) in &self.declarations {
            let mut parameters = self.names.clone();
            let mut formals = Vec::new();
            for formal in &function.formals {
                let alias = fresh('a')?;
                parameters.insert(formal.to_ascii_uppercase(), alias.clone());
                formals.push(alias);
            }
            let bound = rewrite_bound_symbols(&function.body, &parameters, &self.aliases, false)?;
            if let Some(hidden) = bound.unbound_calls.intersection(&self.known).next() {
                self.inaccessible.insert(name.clone(), hidden.clone());
            }
            let alias = &self.aliases[name].name;
            let row = format!(".func {alias}({}) {{{}}}", formals.join(","), bound.text);
            remaining = remaining
                .checked_sub(row.len().saturating_add(1))
                .context("HSPICE function binding byte budget exceeded")?;
            rows.insert(name.clone(), (row, function.line));
            dependencies.insert(name.clone(), bound.calls);
        }
        // Native macros may use forward definitions. Emit in dependency order
        // and reject cyclic call graphs before passing them to the simulator.
        let mut result = Vec::new();
        let mut ready = dependencies
            .iter()
            .filter(|(_, calls)| calls.is_empty())
            .map(|(name, _)| name.clone())
            .collect::<BTreeSet<_>>();
        let mut users = BTreeMap::<String, Vec<String>>::new();
        for (name, calls) in &dependencies {
            for call in calls {
                users.entry(call.clone()).or_default().push(name.clone());
            }
        }
        while let Some(name) = ready.pop_first() {
            result.push(rows.remove(&name).context("missing function row")?);
            for user in users.remove(&name).unwrap_or_default() {
                if let Some(hidden) = self.inaccessible.get(&name).cloned() {
                    self.inaccessible.entry(user.clone()).or_insert(hidden);
                }
                let calls = dependencies
                    .get_mut(&user)
                    .context("missing function dependency")?;
                calls.remove(&name);
                if calls.is_empty() {
                    ready.insert(user);
                }
            }
        }
        ensure!(
            rows.is_empty(),
            "recursive HSPICE function definitions: {}",
            rows.keys().cloned().collect::<Vec<_>>().join(", ")
        );
        Ok(result)
    }

    fn validate(&self, bound: BoundExpression) -> Result<String> {
        if let Some(hidden) = bound.unbound_calls.intersection(&self.known).next() {
            anyhow::bail!("HSPICE function {hidden} is not visible in this occurrence");
        }
        for name in &bound.calls {
            if let Some(hidden) = self.inaccessible.get(name) {
                anyhow::bail!(
                    "HSPICE function {name} calls {hidden}, which is not visible in this occurrence"
                );
            }
        }
        Ok(bound.text)
    }

    pub(super) fn expression(&self, value: &str) -> Result<String> {
        self.validate(rewrite_bound_symbols(
            value,
            &self.names,
            &self.aliases,
            false,
        )?)
    }

    pub(super) fn calls(&self, value: &str) -> Result<String> {
        if self.known.is_empty() {
            return Ok(value.to_owned());
        }
        self.validate(rewrite_bound_symbols(
            value,
            &Names::new(),
            &self.aliases,
            true,
        )?)
    }
}
