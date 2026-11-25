//! Lexically scoped ngspice parameter declarations and model preparation.
//!
//! Source processing is intentionally distinct from the hot-path device kernel.
//! Instantiation supplies an evaluated scalar override for each nested
//! subcircuit invocation; the solver/linker owns X-line expansion and nodes.

use crate::{
    model_card::{ModelCard, logical_statements, parse_assignments, parse_model_cards},
    param_expression::{SpiceExpression, SpiceExpressionContext},
    vdmos_card::{VdmosLiterals, vdmos_resolved},
};
use anyhow::{Context, Result, bail, ensure};
use std::collections::{BTreeMap, BTreeSet};

/// An immutable lexical scope belonging to a source `.subckt` definition.
#[derive(Clone, Debug)]
pub struct ParameterScope {
    /// Subcircuit identifier; the root scope is `None`.
    pub name: Option<String>,
    /// Parent scope index (None only for the root).
    pub parent: Option<usize>,
    /// Physical source line of the opening declaration.
    pub source_line: usize,
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

fn assignments(source: &str, line: usize) -> Result<BTreeMap<String, SpiceExpression>> {
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
                .is_some_and(u8::is_ascii_alphabetic),
            "line {line}: SPICE parameter {name} must begin with a letter"
        );
        definitions.insert(
            name,
            SpiceExpression::parse(value)
                .with_context(|| format!("line {line}: parameter {}", field.name))?,
        );
    }
    Ok(definitions)
}

fn subcircuit_header(
    input: &str,
    line: usize,
) -> Result<(String, BTreeMap<String, SpiceExpression>)> {
    let name_end = input.find(char::is_whitespace).unwrap_or(input.len());
    let name = input[..name_end].trim();
    ensure!(!name.is_empty(), "line {line}: missing .subckt identifier");
    let rest = input[name_end..].trim_start();
    let marker = rest.to_ascii_lowercase().find("params:");
    let params = marker.map_or_else(
        || {
            rest.find('=').map_or("", |eq| {
                let prefix = rest[..eq].trim_end();
                let start = prefix.rfind(char::is_whitespace).map_or(0, |pos| pos + 1);
                &rest[start..]
            })
        },
        |pos| rest[pos + "params:".len()..].trim_start(),
    );
    Ok((name.to_ascii_lowercase(), assignments(params, line)?))
}

fn parse_function(source: &str, line: usize) -> Result<(String, UserFunction)> {
    let open = source.find('(').with_context(|| {
        format!("line {line}: user-defined function requires formal argument list")
    })?;
    let close = source[open + 1..]
        .find(')')
        .map(|pos| pos + open + 1)
        .with_context(|| {
            format!("line {line}: user-defined function missing closing parenthesis")
        })?;
    let name = source[..open].trim().to_ascii_lowercase();
    ensure!(
        !name.is_empty() && name.as_bytes().first().is_some_and(u8::is_ascii_alphabetic),
        "line {line}: invalid function identifier {name}"
    );
    ensure!(
        !reserved_parameter(&name),
        "line {line}: builtin/reserved function identifier {name}"
    );
    let raw = source[open + 1..close].trim();
    let mut seen = BTreeSet::new();
    let mut formals = Vec::new();
    if !raw.is_empty() {
        for field in raw.split(',') {
            let argument = field.trim().to_ascii_lowercase();
            ensure!(
                !argument.is_empty()
                    && argument
                        .as_bytes()
                        .first()
                        .is_some_and(u8::is_ascii_alphabetic)
                    && argument
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'),
                "line {line}: invalid formal parameter {argument}"
            );
            ensure!(
                seen.insert(argument.clone()),
                "line {line}: repeated function formal {argument}"
            );
            formals.push(argument);
        }
    }
    let body = source[close + 1..].trim().trim_start_matches('=').trim();
    ensure!(
        !body.is_empty(),
        "line {line}: empty user-defined function body"
    );
    let body =
        SpiceExpression::parse(body).with_context(|| format!("line {line}: function {name}"))?;
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
        let cards = parse_model_cards(source)?;
        let mut by_line = cards
            .into_iter()
            .map(|card| (card.source_line, card))
            .collect::<BTreeMap<_, _>>();
        let mut library = Self {
            scopes: vec![ParameterScope {
                name: None,
                parent: None,
                source_line: 0,
                defaults: BTreeMap::new(),
                local: BTreeMap::new(),
                functions: BTreeMap::new(),
            }],
            models: Vec::new(),
        };
        let mut stack = vec![0];
        let mut in_control = false;
        for (line, bytes) in logical_statements(source)? {
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
                let (name, defaults) = subcircuit_header(rest, line)?;
                let parent = *stack.last().context("missing root parameter scope")?;
                let id = library.scopes.len();
                library.scopes.push(ParameterScope {
                    name: Some(name),
                    parent: Some(parent),
                    source_line: line,
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
                continue;
            }
            if let Some(rest) = directive(text, ".func") {
                let (name, function) = parse_function(rest, line)?;
                let current = *stack.last().context("missing function scope")?;
                library.scopes[current].functions.insert(name, function);
                continue;
            }
            if let Some(rest) = directive(text, ".param") {
                let definitions = assignments(rest, line)?;
                let current = *stack.last().context("missing parameter scope")?;
                library.scopes[current].local.extend(definitions);
                continue;
            }
            if directive(text, ".model").is_some() {
                let card = by_line.remove(&line).ok_or_else(|| {
                    anyhow::anyhow!("line {line}: parsed model has no corresponding source")
                })?;
                library.models.push(ScopedModel {
                    card,
                    scope: *stack.last().context("missing scope")?,
                });
            }
        }
        ensure!(
            stack.len() == 1,
            "unclosed .subckt at end of source library"
        );
        Ok(library)
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
        resolver.evaluate(caller_scope, &SpiceExpression::parse(expression)?, None)
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
            resolver.evaluate(model.scope, &SpiceExpression::parse(expr)?, None)
        })
    }
}

struct ScopeResolver<'a> {
    source: &'a ModelLibrary,
    overrides: Vec<BTreeMap<String, f64>>,
    active: BTreeSet<(usize, String)>,
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
        let mut values = vec![BTreeMap::new(); source.scopes.len()];
        for &(scope, name, value) in overrides {
            ensure!(value.is_finite(), "non-finite override {name}");
            let frame = values
                .get_mut(scope)
                .context("parameter override scope out of bounds")?;
            ensure!(!name.is_empty(), "empty parameter override name");
            frame.insert(name.to_ascii_lowercase(), value);
        }
        Ok(Self {
            source,
            overrides: values,
            active: BTreeSet::new(),
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
        let mut context = ScopedExpressionContext {
            resolver: self,
            scope,
            frame,
        };
        expr.evaluate_with(&mut context)
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
            if let Some(definition) = self.source.scopes[owner].functions.get(name).cloned() {
                ensure!(
                    definition.formals.len() == args.len(),
                    "function {name} expects {} arguments, got {}",
                    definition.formals.len(),
                    args.len()
                );
                ensure!(
                    self.calls.len() < 512,
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
            if let Some(value) = self.overrides[scope].get(&name) {
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
                ensure!(
                    self.active.insert(key.clone()),
                    "cyclic parameter reference: {name} in scope {scope}"
                );
                // These expressions are parsed once at source ingestion. An
                // owned AST allows mutable recursive resolution of other
                // symbols without holding a borrow into the source scope.
                let expr = expr.clone();
                let result = self.evaluate(scope, &expr, None);
                self.active.remove(&key);
                let value = result.with_context(|| format!("parameter {name}, scope {scope}"))?;
                self.cache.insert(key, value);
                return Ok(value);
            }
            current = definitions.parent;
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
