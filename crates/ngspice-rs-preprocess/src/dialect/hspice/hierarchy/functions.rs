//! Bind function macros to the visible occurrence without expanding bodies.
use super::{Names, Precedence, Rows, inner};
use anyhow::{Context, Result, ensure};
use model_normalize_ngspice::model_card::parse_function_signature;
use model_preprocess_command::{Resource, WorkBudget};
use model_preprocess_simplis::{
    BoundExpression, FunctionAlias, rewrite_bound_symbols, rewrite_parameter_symbols,
};
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

pub(super) fn collect(
    text: &str,
    line: usize,
    functions: &mut Functions,
    budget: &WorkBudget,
) -> Result<()> {
    budget.charge(Resource::ScannedBytes, text.len())?;
    budget.charge(Resource::EmittedBytes, text.len())?;
    budget.charge(Resource::GeneratedNodes, 1)?;
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
    budget.charge(Resource::GeneratedNodes, signature.formals.len())?;
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

pub(super) fn charge_declarations(functions: &Functions, budget: &WorkBudget) -> Result<()> {
    budget.charge(Resource::GeneratedNodes, functions.len())?;
    for name in functions.keys() {
        budget.charge(Resource::EmittedBytes, name.len())?;
    }
    Ok(())
}

#[derive(Default)]
pub(super) struct Scope {
    budget: WorkBudget,
    pub(super) names: Names,
    declarations: Functions,
    aliases: BTreeMap<String, FunctionAlias>,
    known: Arc<BTreeSet<String>>,
    inaccessible: Names,
}

pub(super) struct BindContext<'a> {
    pub(super) precedence: Precedence,
    pub(super) known: Arc<BTreeSet<String>>,
    pub(super) limit: usize,
    pub(super) budget: &'a WorkBudget,
}

type Dependencies = BTreeMap<String, BTreeSet<String>>;
type FunctionRows = BTreeMap<String, (String, usize)>;

impl Scope {
    pub(super) fn bind(
        names: Names,
        parent: Option<&Self>,
        local: Functions,
        context: BindContext<'_>,
        mut fresh: impl FnMut(char) -> Result<String>,
    ) -> Result<(Self, Rows)> {
        let BindContext {
            precedence,
            known,
            limit,
            budget,
        } = context;
        if let Some(scope) = parent {
            charge_declarations(&scope.declarations, budget)?;
        }
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
                budget.charge(Resource::EmittedBytes, name.len())?;
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
            budget: budget.clone(),
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
            self.budget
                .charge(Resource::GeneratedNodes, self.names.len())?;
            for (key, value) in &self.names {
                self.budget.charge(
                    Resource::EmittedBytes,
                    key.len().saturating_add(value.len()),
                )?;
            }
            let mut parameters = self.names.clone();
            let mut formals = Vec::new();
            for formal in &function.formals {
                let alias = fresh('a')?;
                self.budget.charge(
                    Resource::EmittedBytes,
                    formal.len().saturating_add(alias.len().saturating_mul(2)),
                )?;
                parameters.insert(formal.to_ascii_uppercase(), alias.clone());
                formals.push(alias);
            }
            let bound = rewrite_bound_symbols(
                &function.body,
                &parameters,
                &self.aliases,
                false,
                &self.budget,
            )?;
            if let Some(hidden) = bound.unbound_calls.intersection(&self.known).next() {
                self.inaccessible.insert(name.clone(), hidden.clone());
            }
            let alias = &self.aliases[name].name;
            self.budget.charge(
                Resource::EmittedBytes,
                alias
                    .len()
                    .saturating_add(bound.text.len())
                    .saturating_add(
                        formals
                            .iter()
                            .map(|formal| formal.len().saturating_add(1))
                            .sum::<usize>(),
                    )
                    .saturating_add(12),
            )?;
            let row = format!(".func {alias}({}) {{{}}}", formals.join(","), bound.text);
            remaining = remaining
                .checked_sub(row.len().saturating_add(1))
                .context("HSPICE function binding byte budget exceeded")?;
            rows.insert(name.clone(), (row, function.line));
            dependencies.insert(name.clone(), bound.calls);
        }
        self.order(rows, dependencies)
    }

    fn order(&mut self, mut rows: FunctionRows, mut dependencies: Dependencies) -> Result<Rows> {
        // Native macros may use forward definitions. Emit in dependency order
        // and reject cyclic call graphs before passing them to the simulator.
        let mut result = Vec::new();
        self.budget
            .charge(Resource::GeneratedNodes, dependencies.len())?;
        let mut ready = dependencies
            .iter()
            .filter(|(_, calls)| calls.is_empty())
            .map(|(name, _)| {
                self.budget.charge(Resource::EmittedBytes, name.len())?;
                Ok(name.clone())
            })
            .collect::<Result<BTreeSet<_>>>()?;
        let mut users = BTreeMap::<String, Vec<String>>::new();
        let mut depths = BTreeMap::<String, usize>::new();
        for (name, calls) in &dependencies {
            for call in calls {
                self.budget.charge(Resource::GeneratedNodes, 1)?;
                self.budget.charge(
                    Resource::EmittedBytes,
                    call.len().saturating_add(name.len()),
                )?;
                users.entry(call.clone()).or_default().push(name.clone());
            }
        }
        while let Some(name) = ready.pop_first() {
            self.budget.charge(Resource::EvaluationSteps, 1)?;
            let depth = depths.remove(&name).unwrap_or(1);
            self.budget.check_depth(depth)?;
            result.push(rows.remove(&name).context("missing function row")?);
            for user in users.remove(&name).unwrap_or_default() {
                self.budget.charge(Resource::EvaluationSteps, 1)?;
                let child_depth = depths.entry(user.clone()).or_default();
                *child_depth = (*child_depth).max(depth.saturating_add(1));
                if let Some(hidden) = self.inaccessible.get(&name)
                    && !self.inaccessible.contains_key(&user)
                {
                    self.budget.charge(Resource::GeneratedNodes, 1)?;
                    self.budget.charge(
                        Resource::EmittedBytes,
                        user.len().saturating_add(hidden.len()),
                    )?;
                    self.inaccessible.insert(user.clone(), hidden.clone());
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
        if self.known.is_empty() {
            return rewrite_parameter_symbols(value, &self.names, false, &self.budget);
        }
        self.validate(rewrite_bound_symbols(
            value,
            &self.names,
            &self.aliases,
            false,
            &self.budget,
        )?)
    }

    pub(super) fn calls(&self, value: &str) -> Result<String> {
        if self.known.is_empty() {
            self.budget.charge(Resource::EmittedBytes, value.len())?;
            return Ok(value.to_owned());
        }
        self.validate(rewrite_bound_symbols(
            value,
            &Names::new(),
            &self.aliases,
            true,
            &self.budget,
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use model_preprocess_command::{Cancellation, WorkLimits};

    fn bind_chain(length: usize, depth: usize) -> Result<Rows> {
        let budget = WorkBudget::new(
            WorkLimits {
                depth,
                ..WorkLimits::default()
            },
            Cancellation::default(),
        );
        let mut declarations = Functions::new();
        collect(".func f0(x) {x+1}", 0, &mut declarations, &budget)?;
        for index in 1..length {
            collect(
                &format!(".func f{index}(x) {{f{}(x)+1}}", index - 1),
                index,
                &mut declarations,
                &budget,
            )?;
        }
        let known = Arc::new(declarations.keys().cloned().collect());
        let mut serial = 0;
        let (_, rows) = Scope::bind(
            Names::new(),
            None,
            declarations,
            BindContext {
                precedence: Precedence::Global,
                known,
                limit: usize::MAX,
                budget: &budget,
            },
            |kind| {
                serial += 1;
                Ok(format!("test_{kind}{serial}"))
            },
        )?;
        Ok(rows)
    }

    #[test]
    fn function_dependency_depth_is_bounded() -> Result<()> {
        let rows = bind_chain(4, 4)?;
        assert_eq!(
            rows.iter().map(|(_, line)| *line).collect::<Vec<_>>(),
            vec![0, 1, 2, 3]
        );
        let error = bind_chain(5, 4).expect_err("function depth must be bounded");
        assert!(
            format!("{error:#}").contains("recursion depth budget exceeded"),
            "{error:#}"
        );
        Ok(())
    }

    #[test]
    fn cancelled_function_collection_is_rejected() {
        let cancellation = Cancellation::default();
        let budget = WorkBudget::new(WorkLimits::default(), cancellation.clone());
        cancellation.cancel();
        assert!(collect(".func f(x) {x}", 0, &mut Functions::new(), &budget).is_err());
    }
}
