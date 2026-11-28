//! Lexically scoped ngspice parameter declarations and model preparation.
//!
//! Source processing is intentionally distinct from the hot-path device kernel.
//! Actual call-site bindings and terminal identities are derived by the
//! source-side topology pass; the numerical solver owns equation binding.

use crate::{
    linked_source::{LinkedSource, RootKind, SourceOrigin},
    model_card::{
        ModelCard, identifier_start, logical_statements, parse_assignments,
        parse_function_signature, parse_one, positional_prefix,
    },
    param_expression::{SpiceExpression, SpiceExpressionContext},
    vdmos_card::{VdmosLiterals, vdmos_resolved},
};
use anyhow::{Context, Result, bail, ensure};
use model_preprocess_command::{Resource, WorkBudget};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) mod declarations;
pub mod topology;

#[derive(Clone, Debug)]
struct SourceCard {
    line: usize,
    text: String,
}

/// An immutable lexical scope belonging to a source `.subckt` definition.
#[derive(Clone, Debug)]
pub struct ParameterScope {
    /// Subcircuit identifier; the root scope is `None`.
    pub name: Option<String>,
    /// Parent scope index (None only for the root).
    pub parent: Option<usize>,
    /// Physical source line of the opening declaration.
    pub source_line: usize,
    /// Physical source line of the closing declaration (zero for root).
    pub end_line: usize,
    /// Ordered formal terminals.
    pub ports: Vec<String>,
    cards: Vec<SourceCard>,
    defaults: BTreeMap<String, SpiceExpression>,
    local: BTreeMap<String, SpiceExpression>,
    functions: BTreeMap<String, UserFunction>,
}

#[derive(Clone, Debug)]
struct UserFunction {
    formals: Vec<String>,
    body: SpiceExpression,
}

/// Model declaration with its source-scope identity.
#[derive(Clone, Debug)]
pub struct ScopedModel {
    /// Fully parsed `.model` card.
    pub card: ModelCard,
    /// Lexical scope index in `ModelLibrary::scopes`.
    pub scope: usize,
}

/// Source library with lexical model and parameter declarations.
#[derive(Clone, Debug)]
pub struct ModelLibrary {
    /// The root scope is index 0; other scopes are in source declaration order.
    pub scopes: Vec<ParameterScope>,
    /// Model declarations including those nested in `.subckt` definitions.
    pub models: Vec<ScopedModel>,
    globals: BTreeSet<String>,
    origins: Vec<SourceOrigin>,
    budget: WorkBudget,
}

/// Shared declaration index for lexical subcircuit lookup, independently of
/// a simulator's dynamic parameter inheritance rules.
#[derive(Debug)]
pub struct SubcircuitIndex {
    declarations: Vec<BTreeMap<String, usize>>,
    duplicates: Vec<usize>,
}

impl SubcircuitIndex {
    /// Index definitions in declaration order. The first declaration remains
    /// visible; consumers that reject duplicates can inspect them separately.
    ///
    /// # Errors
    /// Invalid declaration parent or missing name.
    pub fn new(source: &ModelLibrary) -> Result<Self> {
        source
            .budget
            .charge(Resource::GeneratedNodes, source.scopes.len())?;
        let mut result = Self {
            declarations: vec![BTreeMap::new(); source.scopes.len()],
            duplicates: Vec::new(),
        };
        for (id, scope) in source.scopes.iter().enumerate().skip(1) {
            let parent = scope.parent.context("non-root scope without parent")?;
            let name = scope.name.as_deref().context("unnamed subcircuit")?;
            source.budget.charge(Resource::GeneratedNodes, 1)?;
            source.budget.charge(Resource::EmittedBytes, name.len())?;
            let table = result
                .declarations
                .get_mut(parent)
                .context("invalid lexical parent")?;
            match table.entry(name.to_owned()) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(id);
                }
                std::collections::btree_map::Entry::Occupied(_) => result.duplicates.push(id),
            }
        }
        Ok(result)
    }

    /// Later declarations that conflict with an earlier lexical declaration.
    #[must_use = "the returned declarations identify duplicate lexical names"]
    pub fn duplicates(&self) -> &[usize] {
        &self.duplicates
    }

    /// Find the visible subcircuit from a lexical calling scope.
    #[must_use = "the returned scope identifies the visible subcircuit"]
    pub fn resolve(&self, source: &ModelLibrary, scope: usize, name: &str) -> Option<usize> {
        lexical_lookup(source, scope, name, &self.declarations)
    }
}

fn lexical_lookup(
    source: &ModelLibrary,
    mut scope: usize,
    name: &str,
    table: &[BTreeMap<String, usize>],
) -> Option<usize> {
    let name = name.to_ascii_lowercase();
    loop {
        if let Some(&id) = table.get(scope)?.get(&name) {
            return Some(id);
        }
        scope = source.scopes.get(scope)?.parent?;
    }
}

fn directive<'a>(source: &'a str, name: &str) -> Option<&'a str> {
    let prefix = source.get(..name.len())?;
    if !prefix.eq_ignore_ascii_case(name) {
        return None;
    }
    let tail = &source[name.len()..];
    (tail.is_empty() || tail.as_bytes().first().is_some_and(u8::is_ascii_whitespace))
        .then_some(tail.trim_start())
}

/// ngspice front-end parameter names cannot shadow its operators, built-ins
/// or dynamic behavioral variables. The C front end may silently substitute
/// incorrect values for some of these (notably `pow`). Reject at source load.
fn reserved_parameter(name: &str) -> bool {
    matches!(
        name,
        "not"
            | "and"
            | "or"
            | "div"
            | "mod"
            | "defined"
            | "sqr"
            | "sqrt"
            | "sin"
            | "cos"
            | "tan"
            | "sinh"
            | "cosh"
            | "tanh"
            | "asin"
            | "acos"
            | "atan"
            | "asinh"
            | "acosh"
            | "atanh"
            | "arctan"
            | "exp"
            | "ln"
            | "log"
            | "log10"
            | "abs"
            | "nint"
            | "int"
            | "floor"
            | "ceil"
            | "pow"
            | "pwr"
            | "min"
            | "max"
            | "sgn"
            | "ternary_fcn"
            | "gauss"
            | "agauss"
            | "unif"
            | "aunif"
            | "limit"
            | "time"
            | "temper"
            | "hertz"
    )
}

fn assignments(
    source: &str,
    line: usize,
    budget: &WorkBudget,
) -> Result<BTreeMap<String, SpiceExpression>> {
    let mut definitions = BTreeMap::new();
    for field in parse_assignments(source).with_context(|| format!("line {line}"))? {
        let value = field
            .expression
            .as_deref()
            .context("missing parameter assignment expression")?;
        let name = field.name.to_ascii_lowercase();
        ensure!(
            !reserved_parameter(&name),
            "line {line}: reserved ngspice parameter identifier {name}"
        );
        ensure!(
            field
                .name
                .as_bytes()
                .first()
                .copied()
                .is_some_and(identifier_start),
            "line {line}: SPICE parameter {name} must begin with a letter or underscore"
        );
        definitions.insert(
            name,
            SpiceExpression::parse_with_budget(value, budget)
                .with_context(|| format!("line {line}: parameter {}", field.name))?,
        );
    }
    Ok(definitions)
}

fn subcircuit_header(
    input: &str,
    line: usize,
    budget: &WorkBudget,
) -> Result<(String, Vec<String>, BTreeMap<String, SpiceExpression>)> {
    let (tokens, params) = positional_prefix(input);
    let (name, ports) = tokens
        .split_first()
        .with_context(|| format!("line {line}: missing .subckt identifier"))?;
    let ports = ports
        .iter()
        .map(|(port, _)| port.to_ascii_lowercase())
        .collect();
    Ok((
        name.0.to_ascii_lowercase(),
        ports,
        assignments(params, line, budget)?,
    ))
}

fn parse_function(
    source: &str,
    line: usize,
    budget: &WorkBudget,
) -> Result<(String, UserFunction)> {
    budget.charge(Resource::ScannedBytes, source.len())?;
    let (signature, body) =
        parse_function_signature(source).with_context(|| format!("line {line}"))?;
    let name = signature.name.to_ascii_lowercase();
    ensure!(
        !reserved_parameter(&name),
        "line {line}: builtin/reserved function identifier {name}"
    );
    budget.charge(Resource::GeneratedNodes, signature.formals.len())?;
    let formals = signature
        .formals
        .into_iter()
        .map(|formal| formal.to_ascii_lowercase())
        .collect();
    let body = body.trim().trim_start_matches('=').trim();
    ensure!(
        !body.is_empty(),
        "line {line}: empty user-defined function body"
    );
    let body = SpiceExpression::parse_with_budget(body, budget)
        .with_context(|| format!("line {line}: function {name}"))?;
    Ok((name, UserFunction { formals, body }))
}

impl ModelLibrary {
    /// Parse lexical `.param`, `.subckt`, `.ends` and `.model` declarations
    /// from one plaintext, already-linked source buffer. Unrecognized circuit
    /// cards are left to the topology parser. `.control` scripts are excluded.
    ///
    /// # Errors
    ///
    /// Rejects unbalanced subcircuits, invalid assignments and model syntax;
    /// raw `.include`/`.lib` references must first be resolved by the linker.
    pub fn parse(source: &[u8]) -> Result<Self> {
        Self::parse_source(source, RootKind::Include)
    }

    /// Parse a linked deck or library with its original physical provenance.
    /// A deck's first physical line is always its title, never an instance.
    /// # Errors
    /// Invalid declarations or a mismatched linked-source origin map.
    pub fn parse_linked(source: &LinkedSource, kind: RootKind) -> Result<Self> {
        ensure!(
            source.bytes.split_inclusive(|&b| b == b'\n').count() == source.origins.len(),
            "linked source origin count does not match physical lines"
        );
        let mut library = Self::parse_source(&source.bytes, kind)?;
        library.origins.clone_from(&source.origins);
        Ok(library)
    }

    /// Parse a deck or include buffer with explicit title-line handling.
    /// # Errors
    /// Invalid source declarations, as in [`Self::parse`].
    pub fn parse_source(source: &[u8], kind: RootKind) -> Result<Self> {
        Self::parse_source_with_budget(source, kind, WorkBudget::default())
    }

    /// Parse and bind a model library under shared cumulative counters.
    /// # Errors
    /// Invalid declarations or resource exhaustion.
    pub fn parse_source_with_budget(
        source: &[u8],
        kind: RootKind,
        budget: WorkBudget,
    ) -> Result<Self> {
        budget.charge(Resource::ScannedBytes, source.len())?;
        budget.charge(
            Resource::Statements,
            source.split_inclusive(|byte| *byte == b'\n').count(),
        )?;
        let skip = if kind == RootKind::Deck {
            source
                .split_inclusive(|&b| b == b'\n')
                .next()
                .context("deck has no title")?
                .len()
        } else {
            0
        };
        let mut library = Self::empty(budget);
        let mut stack = vec![0];
        let mut in_control = false;
        for (line, bytes) in logical_statements(&source[skip..])? {
            let line = line + usize::from(kind == RootKind::Deck);
            let text = std::str::from_utf8(&bytes)
                .with_context(|| format!("line {line}: invalid active source encoding"))?;
            if directive(text, ".control").is_some() {
                in_control = true;
                continue;
            }
            if directive(text, ".endc").is_some() {
                in_control = false;
                continue;
            }
            if in_control {
                continue;
            }
            if [".include", ".inc", ".incpslt", ".lib", ".endl"]
                .iter()
                .any(|name| directive(text, name).is_some())
            {
                bail!(
                    "line {line}: unlinked source resource directive {text}; run SourceLinker::link first"
                );
            }
            if let Some(rest) = directive(text, ".subckt") {
                library.budget.check_depth(stack.len())?;
                library.budget.charge(Resource::GeneratedNodes, 1)?;
                let (name, ports, defaults) = subcircuit_header(rest, line, &library.budget)?;
                let parent = *stack.last().context("missing root parameter scope")?;
                let id = library.scopes.len();
                library.scopes.push(ParameterScope {
                    name: Some(name),
                    parent: Some(parent),
                    source_line: line,
                    end_line: 0,
                    ports,
                    cards: Vec::new(),
                    defaults,
                    local: BTreeMap::new(),
                    functions: BTreeMap::new(),
                });
                stack.push(id);
                continue;
            }
            if let Some(rest) = directive(text, ".ends") {
                ensure!(stack.len() > 1, "line {line}: unmatched .ends");
                let closing = stack.pop().context("missing .subckt scope")?;
                let actual = library.scopes[closing]
                    .name
                    .as_deref()
                    .context("unnamed .subckt")?;
                ensure!(
                    rest.is_empty() || rest.eq_ignore_ascii_case(actual),
                    "line {line}: .ends {rest} does not match .subckt {actual}"
                );
                library.scopes[closing].end_line = line;
                continue;
            }
            library.record(text, line, *stack.last().context("missing scope")?)?;
        }
        ensure!(
            stack.len() == 1,
            "unclosed .subckt at end of source library"
        );
        Ok(library)
    }

    fn record(&mut self, text: &str, line: usize, current: usize) -> Result<()> {
        if let Some(rest) = directive(text, ".func") {
            let (name, function) = parse_function(rest, line, &self.budget)?;
            self.scopes[current].functions.insert(name, function);
            return Ok(());
        }
        if let Some(rest) = directive(text, ".param") {
            let definitions = assignments(rest, line, &self.budget)?;
            self.scopes[current].local.extend(definitions);
            return Ok(());
        }
        if directive(text, ".model").is_some() {
            self.models.push(ScopedModel {
                card: parse_one(text, line)?,
                scope: current,
            });
        } else if let Some(rest) = directive(text, ".global") {
            self.globals
                .extend(rest.split_whitespace().map(str::to_ascii_lowercase));
        } else if text
            .as_bytes()
            .first()
            .is_some_and(|b| matches!(b.to_ascii_lowercase(), b'x' | b'm'))
        {
            self.scopes[current].cards.push(SourceCard {
                line,
                text: text.to_owned(),
            });
        }
        Ok(())
    }

    fn empty(budget: WorkBudget) -> Self {
        Self {
            scopes: vec![ParameterScope {
                name: None,
                parent: None,
                source_line: 0,
                end_line: 0,
                ports: Vec::new(),
                cards: Vec::new(),
                defaults: BTreeMap::new(),
                local: BTreeMap::new(),
                functions: BTreeMap::new(),
            }],
            models: Vec::new(),
            globals: BTreeSet::new(),
            origins: Vec::new(),
            budget,
        }
    }

    /// Evaluate an X-line call-site scalar expression against the **calling**
    /// lexical scope, including any already bound numeric ancestor overrides.
    /// The caller then passes the result to `resolve_vdmos` as a numeric
    /// parameter override for the callee's lexical scope. This preserves
    /// ngspice's per-instance parameter isolation without expanding topology.
    ///
    /// # Errors
    ///
    /// Rejects unknown symbols, invalid scope IDs, cycles and non-finite
    /// arithmetic or source overrides.
    pub fn evaluate_scalar(
        &self,
        caller_scope: usize,
        expression: &str,
        overrides: &[(usize, &str, f64)],
    ) -> Result<f64> {
        ensure!(
            caller_scope < self.scopes.len(),
            "caller scope out of bounds"
        );
        let mut resolver = ScopeResolver::new(self, overrides)?;
        resolver.evaluate(
            caller_scope,
            &SpiceExpression::parse_with_budget(expression, &self.budget)?,
            None,
        )
    }

    /// Resolve a model's numeric SPICE expressions in their lexical context.
    /// For instantiated subcircuits, `overrides` binds evaluated *numeric*
    /// values at the targeted scope ID (one resolver context per instance).
    /// The linker evaluates call-site expressions in the calling scope and
    /// supplies these values here; no X-line expansion occurs in this module.
    ///
    /// # Errors
    ///
    /// Fails on cycles, undefined symbols, invalid scope overrides, unknown
    /// functions or invalid native VDMOS parameters.
    pub fn resolve_vdmos<'a>(
        &'a self,
        model_index: usize,
        overrides: &[(usize, &str, f64)],
    ) -> Result<Option<VdmosLiterals<'a>>> {
        let model = self
            .models
            .get(model_index)
            .context("model index out of bounds")?;
        let mut resolver = ScopeResolver::new(self, overrides)?;
        vdmos_resolved(&model.card, |expr| {
            resolver.evaluate(
                model.scope,
                &SpiceExpression::parse_with_budget(expr, &self.budget)?,
                None,
            )
        })
    }
}

struct ScopeResolver<'a> {
    source: &'a ModelLibrary,
    overrides: BTreeMap<usize, BTreeMap<String, f64>>,
    parameter_parents: BTreeMap<usize, Option<usize>>,
    active: Vec<(usize, String)>,
    cache: BTreeMap<(usize, String), f64>,
    calls: Vec<FunctionFrame>,
}

#[derive(Clone)]
struct BoundExpression {
    expr: SpiceExpression,
    scope: usize,
    frame: Option<usize>,
}
struct FunctionFrame {
    arguments: BTreeMap<String, BoundExpression>,
}
struct ScopedExpressionContext<'r, 'a> {
    resolver: &'r mut ScopeResolver<'a>,
    scope: usize,
    frame: Option<usize>,
}

impl SpiceExpressionContext for ScopedExpressionContext<'_, '_> {
    fn symbol(&mut self, name: &str) -> Result<f64> {
        if let Some(frame) = self.frame {
            let argument = self.resolver.calls[frame].arguments.get(name).cloned();
            if let Some(argument) = argument {
                return self
                    .resolver
                    .evaluate(argument.scope, &argument.expr, argument.frame);
            }
        }
        self.resolver.symbol(self.scope, name)
    }
    fn function(&mut self, name: &str, args: &[SpiceExpression]) -> Result<f64> {
        self.resolver.function(self.scope, self.frame, name, args)
    }
}

impl<'a> ScopeResolver<'a> {
    fn new(source: &'a ModelLibrary, overrides: &[(usize, &str, f64)]) -> Result<Self> {
        let mut values = BTreeMap::<usize, BTreeMap<String, f64>>::new();
        for &(scope, name, value) in overrides {
            ensure!(value.is_finite(), "non-finite override {name}");
            ensure!(
                scope < source.scopes.len(),
                "parameter override scope out of bounds"
            );
            let frame = values.entry(scope).or_default();
            ensure!(!name.is_empty(), "empty parameter override name");
            frame.insert(name.to_ascii_lowercase(), value);
        }
        Ok(Self {
            source,
            overrides: values,
            parameter_parents: BTreeMap::new(),
            active: Vec::new(),
            cache: BTreeMap::new(),
            calls: Vec::new(),
        })
    }

    fn evaluate(
        &mut self,
        scope: usize,
        expr: &SpiceExpression,
        frame: Option<usize>,
    ) -> Result<f64> {
        let budget = self.source.budget.clone();
        let mut context = ScopedExpressionContext {
            resolver: self,
            scope,
            frame,
        };
        expr.evaluate_with_budget(&mut context, &budget)
    }

    fn function(
        &mut self,
        scope: usize,
        caller_frame: Option<usize>,
        name: &str,
        args: &[SpiceExpression],
    ) -> Result<f64> {
        let mut current = Some(scope);
        while let Some(owner) = current {
            self.source.budget.charge(Resource::EvaluationSteps, 1)?;
            if let Some(definition) = self.source.scopes[owner].functions.get(name).cloned() {
                ensure!(
                    definition.formals.len() == args.len(),
                    "function {name} expects {} arguments, got {}",
                    definition.formals.len(),
                    args.len()
                );
                ensure!(
                    self.calls.len() < 64,
                    "function {name}: recursive expression depth exceeded"
                );
                let arguments = definition
                    .formals
                    .into_iter()
                    .zip(args.iter().cloned())
                    .map(|(formal, expr)| {
                        (
                            formal,
                            BoundExpression {
                                expr,
                                scope,
                                frame: caller_frame,
                            },
                        )
                    })
                    .collect();
                let frame = self.calls.len();
                self.calls.push(FunctionFrame { arguments });
                // ngspice functions are source macros, not lexical closures:
                // a globally declared function resolves unbound symbols from
                // the *calling instance's* parameter scope.
                let result = self.evaluate(scope, &definition.body, Some(frame));
                self.calls.pop();
                return result.with_context(|| format!("source function {name}, scope {owner}"));
            }
            current = self.source.scopes[owner].parent;
        }
        bail!("undefined ngspice function {name}")
    }

    fn symbol(&mut self, requested_scope: usize, name: &str) -> Result<f64> {
        let name = name.to_ascii_lowercase();
        let mut current = Some(requested_scope);
        while let Some(scope) = current {
            self.source.budget.charge(Resource::EvaluationSteps, 1)?;
            if let Some(value) = self
                .overrides
                .get(&scope)
                .and_then(|values| values.get(&name))
            {
                return Ok(*value);
            }
            let definitions = &self.source.scopes[scope];
            if let Some(expr) = definitions
                .local
                .get(&name)
                .or_else(|| definitions.defaults.get(&name))
            {
                let key = (scope, name.clone());
                if let Some(value) = self.cache.get(&key) {
                    return Ok(*value);
                }
                // A direct self-reference in a local declaration inherits
                // the caller's value (e.g. gain={gain*2}); indirect cycles
                // remain errors. At the root there is no caller to inherit.
                if self.active.last() == Some(&key)
                    && let Some(parent) = self
                        .parameter_parents
                        .get(&scope)
                        .copied()
                        .unwrap_or(definitions.parent)
                {
                    current = Some(parent);
                    continue;
                }
                ensure!(
                    !self.active.contains(&key),
                    "cyclic parameter reference: {name} in scope {scope}"
                );
                ensure!(
                    self.active.len() < 64,
                    "parameter expression depth exceeded"
                );
                self.active.push(key.clone());
                // These expressions are parsed once at source ingestion. An
                // owned AST allows mutable recursive resolution of other
                // symbols without holding a borrow into the source scope.
                let expr = expr.clone();
                let result = self.evaluate(scope, &expr, None);
                self.active.pop();
                let value = result.with_context(|| format!("parameter {name}, scope {scope}"))?;
                self.cache.insert(key, value);
                return Ok(value);
            }
            current = self
                .parameter_parents
                .get(&scope)
                .copied()
                .unwrap_or(definitions.parent);
        }
        bail!("undefined ngspice model parameter {name}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn p(model: &VdmosLiterals<'_>, name: &str) -> Result<f64> {
        model
            .parameters
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| *value)
            .ok_or_else(|| anyhow::anyhow!("missing {name}"))
    }

    #[test]
    fn forward_global_assignments_case_and_last_definition_win() -> Result<()> {
        let source = b"* title\n.param B={A*2}\n.param a=2\n.param a=3\n.model dut VDMOS (VTO=3 KP={b} LAMBDA={1k/1Meg})\n";
        let lib = ModelLibrary::parse(source)?;
        let vdmos = lib.resolve_vdmos(0, &[])?.context("missing VDMOS")?;
        assert_eq!(p(&vdmos, "KP")?, 6.0);
        assert_eq!(p(&vdmos, "LAMBDA")?, 1e-3);
        assert_eq!(lib.scopes.len(), 1);
        Ok(())
    }

    #[test]
    fn nested_scopes_and_instance_overrides_are_independent() -> Result<()> {
        let source = b"* title\n.param gain=2 kp_base={gain*2}\n.subckt blk d g s params: gain=1\n.param factor={gain*kp_base}\n.model local VDMOS(VTO=3 KP={factor})\n.subckt inner d g s params: factor=2\n.model nested VDMOS(VTO=3 KP={factor*gain})\n.ends inner\n.ends blk\n.model global VDMOS(VTO=3 KP={kp_base})\n";
        let lib = ModelLibrary::parse(source)?;
        assert_eq!(lib.models.len(), 3);
        let local = lib.resolve_vdmos(0, &[])?.context("local VDMOS")?;
        assert_eq!(p(&local, "KP")?, 4.0);
        let override_local = lib
            .resolve_vdmos(0, &[(1, "gain", 4.0)])?
            .context("override VDMOS")?;
        assert_eq!(p(&override_local, "KP")?, 16.0);
        let call_gain = lib.evaluate_scalar(0, "{gain*2}", &[])?;
        assert_eq!(call_gain, 4.0);
        let from_caller = lib
            .resolve_vdmos(0, &[(1, "gain", call_gain)])?
            .context("missing caller-overridden model")?;
        assert_eq!(p(&from_caller, "KP")?, 16.0);
        let nested = lib
            .resolve_vdmos(1, &[(1, "gain", 4.0), (2, "factor", 3.0)])?
            .context("nested VDMOS")?;
        assert_eq!(p(&nested, "KP")?, 12.0);
        let global = lib.resolve_vdmos(2, &[])?.context("global VDMOS")?;
        assert_eq!(p(&global, "KP")?, 4.0);
        Ok(())
    }

    #[test]
    fn leading_underscore_identifiers_share_the_native_scalar_grammar() -> Result<()> {
        // Independent ngspice-47: a 14 V source using this function and
        // formal name parses and evaluates without compatibility settings.
        let source = b"* title\n.param _a=2\n.func __helper(_x) {3*_x+_a}\n";
        let lib = ModelLibrary::parse(source)?;
        assert_eq!(lib.evaluate_scalar(0, "{__helper(4)}", &[])?, 14.);
        Ok(())
    }

    #[test]
    fn source_functions_forward_nested_and_scope_sensitive() -> Result<()> {
        let text = b"* title\n.param gain={weighted(2,1)}\n.func weighted(x,y) {twice(x)+y}\n.func twice(x) {2*x}\n.model global vdmos (kp={gain} vto=3)\n.subckt stage d g s params: gain=1\n.func scaled(z) {twice(z)*gain}\n.model local vdmos (kp={scaled(1)} vto=3)\n.ends stage\n";
        let lib = ModelLibrary::parse(text)?;
        let global = lib.resolve_vdmos(0, &[])?.context("global model")?;
        assert_eq!(p(&global, "kp")?, 5.0);
        let local = lib
            .resolve_vdmos(1, &[(1, "gain", 4.0)])?
            .context("local model")?;
        assert_eq!(p(&local, "kp")?, 8.0);
        let local_default = lib.resolve_vdmos(1, &[])?.context("default local model")?;
        assert_eq!(p(&local_default, "kp")?, 2.0);
        Ok(())
    }

    #[test]
    fn global_source_function_uses_calling_subcircuit_parameters() -> Result<()> {
        let source = b"* title\n.param gain=2\n.func gainfn(x) {gain*x}\n.subckt stage d g s params: gain=1\n.model dut vdmos(vto=3 kp={gainfn(1)})\n.ends stage\n";
        let library = ModelLibrary::parse(source)?;
        let root_fn = library.evaluate_scalar(0, "gainfn(2)", &[])?;
        assert_eq!(root_fn, 4.0);
        let local_default = library.resolve_vdmos(0, &[])?.context("default model")?;
        assert_eq!(p(&local_default, "kp")?, 1.0);
        let local_override = library
            .resolve_vdmos(0, &[(1, "gain", 4.0)])?
            .context("override model")?;
        assert_eq!(p(&local_override, "kp")?, 4.0);
        Ok(())
    }

    #[test]
    fn raw_include_and_library_references_fail_before_model_preparation() -> Result<()> {
        for directive in [
            ".include other.inc",
            ".inc foo",
            ".lib models.lib corner",
            ".incpslt vendor.lib",
            ".lib corner",
            ".endl corner",
        ] {
            let raw = format!("test title\n{directive}\n.model dut vdmos (kp=2)\n");
            let Err(error) = ModelLibrary::parse(raw.as_bytes()) else {
                bail!("unexpected acceptance of unlinked {directive}");
            };
            ensure!(
                format!("{error:#}").contains("unlinked source"),
                "unexpected error for {directive}: {error:#}"
            );
        }
        Ok(())
    }

    #[test]
    fn global_derived_parameter_is_lexical_even_when_function_is_dynamic() -> Result<()> {
        let source = b"* title\n.param gain=2 base={gain*2}\n.func gainfn(x) {gain*x}\n.subckt stage d g s params: gain=1\n.model dut vdmos(vto=3 kp={base})\n.ends stage\n";
        let library = ModelLibrary::parse(source)?;
        let global_derived = library
            .resolve_vdmos(0, &[(1, "gain", 4.0)])?
            .context("derived global model")?;
        assert_eq!(p(&global_derived, "kp")?, 4.0);
        assert_eq!(
            library.evaluate_scalar(1, "gainfn(1)", &[(1, "gain", 4.0)])?,
            4.0
        );
        assert_eq!(
            library.evaluate_scalar(1, "base", &[(1, "gain", 4.0)])?,
            4.0
        );
        Ok(())
    }

    #[test]
    fn source_functions_lazy_arguments_and_recursive_failures() -> Result<()> {
        let text = b".func choose(cond,positive,negative) {cond ? positive : negative}\n.func compose(a,b) {choose(1,a,1/0)+b}\n.model okay vdmos (kp={compose(2,3)} vto=3)\n";
        let lib = ModelLibrary::parse(text)?;
        assert_eq!(p(&lib.resolve_vdmos(0, &[])?.context("model")?, "kp")?, 5.0);
        let wrong_arity = ModelLibrary::parse(b".func sum(a,b) {a+b}\n.model x vdmos kp={sum(3)}")?;
        assert!(wrong_arity.resolve_vdmos(0, &[]).is_err());
        let unknown = ModelLibrary::parse(b".model x vdmos kp={missingfn(1,2)}")?;
        assert!(unknown.resolve_vdmos(0, &[]).is_err());
        let circular =
            ModelLibrary::parse(b".func loop(x) {loop(x)}\n.model x vdmos kp={loop(1)}")?;
        assert!(circular.resolve_vdmos(0, &[]).is_err());
        Ok(())
    }

    #[test]
    fn cycles_undefined_invalid_overrides_and_unbalanced_scope_fail() -> Result<()> {
        for (source, reason) in [
            (
                ".param a={b}\n.param b={a}\n.model x vdmos kp={a}",
                "cyclic",
            ),
            (".model x vdmos kp={missing}", "undefined"),
            (".subckt x a b\n.model x vdmos kp=2", "unclosed"),
            (".ends x\n.model x vdmos kp=2", "unmatched"),
            (".subckt a d g s\n.ends b", "does not match"),
            (
                ".param a=2 + 3\n.model x vdmos kp={a}",
                "requires name=value",
            ),
        ] {
            let result = ModelLibrary::parse(source.as_bytes()).and_then(|lib| {
                if lib.models.is_empty() {
                    Ok(())
                } else {
                    lib.resolve_vdmos(0, &[]).map(|_| ())
                }
            });
            let Err(error) = result else {
                bail!("expected failure for {source}")
            };
            ensure!(
                format!("{error:#}").contains(reason),
                "{source} produced {error:#}"
            );
        }
        for reserved in ["pow", "log", "sin", "time", "ternary_fcn", "min", "mod"] {
            let source = format!(".param {reserved}=3\n.model x vdmos kp=2");
            let Err(error) = ModelLibrary::parse(source.as_bytes()) else {
                bail!("reserved identifier {reserved} was incorrectly accepted");
            };
            ensure!(
                format!("{error:#}").contains("reserved"),
                "unexpected diagnostic for {reserved}: {error:#}"
            );
        }
        let lib = ModelLibrary::parse(b".model x vdmos kp=2")?;
        assert!(lib.resolve_vdmos(0, &[(10, "a", 1.0)]).is_err());
        assert!(lib.resolve_vdmos(0, &[(0, "a", f64::NAN)]).is_err());
        Ok(())
    }
}
