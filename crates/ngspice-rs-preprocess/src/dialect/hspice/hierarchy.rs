//! Explicit HSPICE occurrence bindings. Symbolic aliases avoid evaluating
//! parameters at preparation time, so native analysis still owns expressions.
//! Electrical instance names and terminal identities are never flattened.
use super::{LoweredExpansion, SelectedModels, analysis_plan, map_values, models, mos};
use crate::OriginLine;
use anyhow::{Context, Result, ensure};
use model_normalize_ngspice::{
    linked_source::RootKind,
    model_card::{parse_assignments, parse_model_card, positional_prefix},
    parameter_scope::{ModelLibrary, SubcircuitIndex},
    statement_token_spans,
};
use model_preprocess_command::{Resource, WorkBudget};
use model_preprocess_simplis::{
    rewrite_parameter_symbols,
    source_dialect::{DialectContext, SourceDialect, model_mapping::Device},
};
use std::fmt::Write as _;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

mod functions;
use functions::{BindContext, Functions, Scope};

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum Precedence {
    #[default]
    Global,
    Local,
}

type Names = BTreeMap<String, String>;
type Rows = Vec<(String, usize)>;

#[derive(Clone)]
struct Parameter {
    value: String,
    line: usize,
    actual: bool,
}

type Parameters = BTreeMap<String, Parameter>;

#[derive(Default)]
struct Definition {
    body: Vec<usize>,
    parameters: Parameters,
    functions: Functions,
}

fn collect_parameters(
    fields: &str,
    line: usize,
    actual: bool,
    parameters: &mut Parameters,
    budget: &WorkBudget,
) -> Result<()> {
    budget.charge(Resource::ScannedBytes, fields.len())?;
    for field in parse_assignments(fields)? {
        budget.charge(Resource::GeneratedNodes, 1)?;
        let name = field.name.to_ascii_uppercase();
        ensure!(
            name.as_bytes().first().is_some_and(u8::is_ascii_alphabetic),
            "invalid HSPICE parameter name {name}"
        );
        parameters.insert(
            name,
            Parameter {
                value: field.expression.context("missing parameter value")?,
                line,
                actual,
            },
        );
    }
    Ok(())
}

fn collect_definitions(
    source: &ModelLibrary,
    lines: &[&str],
    budget: &WorkBudget,
) -> Result<Vec<Definition>> {
    budget.charge(Resource::GeneratedNodes, source.scopes.len())?;
    let mut definitions = (0..source.scopes.len())
        .map(|_| Definition::default())
        .collect::<Vec<_>>();
    let starts = source
        .scopes
        .iter()
        .enumerate()
        .skip(1)
        .map(|(id, scope)| (scope.source_line - 1, id))
        .collect::<BTreeMap<_, _>>();
    let mut owner = 0;
    for (line, text) in lines.iter().enumerate().skip(1) {
        budget.charge(Resource::Statements, 1)?;
        budget.charge(Resource::ScannedBytes, text.len())?;
        if let Some(&child) = starts.get(&line) {
            ensure!(
                source.scopes[child].parent == Some(owner),
                "inconsistent lexical scope index"
            );
            owner = child;
            let (_, fields) = positional_prefix(text);
            collect_parameters(
                fields,
                line,
                false,
                &mut definitions[owner].parameters,
                budget,
            )?;
        } else if owner != 0 && source.scopes[owner].end_line == line + 1 {
            owner = source.scopes[owner]
                .parent
                .context("missing enclosing definition")?;
        } else {
            definitions[owner].body.push(line);
            if analysis_plan::command(text) == ".PARAM" {
                let tail = text
                    .split_once(char::is_whitespace)
                    .context("empty parameter declaration")?
                    .1;
                collect_parameters(
                    tail,
                    line,
                    false,
                    &mut definitions[owner].parameters,
                    budget,
                )?;
            } else if analysis_plan::command(text) == ".FUNC" {
                functions::collect(text, line, &mut definitions[owner].functions, budget)?;
            }
        }
    }
    ensure!(owner == 0, "unclosed HSPICE hierarchy");
    Ok(definitions)
}

fn inner(expression: &str) -> &str {
    expression
        .trim()
        .strip_prefix('{')
        .and_then(|text| text.strip_suffix('}'))
        .unwrap_or_else(|| expression.trim())
}

fn bind_values(text: &str, scope: &Scope, limit: usize, budget: &WorkBudget) -> Result<String> {
    map_values(text, limit, budget, |value, remaining| {
        // Explicit braces make standalone positional parameters native values;
        // the common expression scanner handles expressions without renaming
        // node/current probes, function names or assignment destinations.
        budget.charge(Resource::EvaluationSteps, value.len())?;
        let value = scope.calls(value)?;
        budget.charge(Resource::EmittedBytes, value.len())?;
        let tokens = statement_token_spans(&value)?;
        let names = &scope.names;
        let mut output = String::new();
        let mut previous = 0;
        for (index, range) in tokens.iter().enumerate() {
            output.push_str(&value[previous..range.start]);
            let word = &value[range.clone()];
            let assignment = tokens
                .get(index + 1)
                .is_some_and(|next| &value[next.clone()] == "=");
            if assignment {
                output.push_str(word);
            } else if let Some(alias) = names.get(&word.to_ascii_uppercase()) {
                budget.charge(Resource::EmittedBytes, alias.len().saturating_add(2))?;
                write!(output, "{{{alias}}}")?;
            } else {
                output.push_str(&rewrite_parameter_symbols(word, names, true, budget)?);
            }
            previous = range.end;
        }
        output.push_str(&value[previous..]);
        ensure!(
            output.len() <= remaining,
            "HSPICE expression binding byte budget exceeded"
        );
        Ok(output)
    })
}

fn multiplier_card(text: &str, factor: Option<&str>) -> Result<String> {
    let Some(factor) = factor else {
        return Ok(text.to_owned());
    };
    let command = analysis_plan::command(text);
    if command.is_empty() || command.starts_with(['.', '*']) {
        return Ok(text.to_owned());
    }
    let tokens = statement_token_spans(text)?;
    let device = command.as_bytes()[0];
    // Native parallel multiplicity is available for these devices. Voltage
    // sources (including dependent voltage sources) have no parallel factor.
    let current_behavioral = device == b'B'
        && tokens
            .get(3)
            .is_some_and(|range| text[range.clone()].eq_ignore_ascii_case("I"));
    if matches!(device, b'V' | b'E' | b'H') || device == b'B' && !current_behavioral {
        return Ok(text.to_owned());
    }
    ensure!(
        matches!(
            device,
            b'R' | b'C' | b'L' | b'I' | b'G' | b'F' | b'D' | b'M'
        ) || current_behavioral,
        "unqualified multiplier on HSPICE device {command}"
    );
    let mut explicit = None;
    for (index, range) in tokens.iter().enumerate().skip(3) {
        if text[range.clone()].eq_ignore_ascii_case("M")
            && tokens
                .get(index + 1)
                .is_some_and(|eq| &text[eq.clone()] == "=")
        {
            let value = tokens
                .get(index + 2)
                .context("missing element multiplier")?;
            ensure!(
                explicit.replace(value.clone()).is_none(),
                "duplicate element multiplier"
            );
        }
    }
    let mut out = text.to_owned();
    if let Some(range) = explicit {
        let value = inner(&text[range.clone()]);
        out.replace_range(range, &format!("{{({value})*({factor})}}"));
    } else {
        write!(out, " m={{{factor}}}")?;
    }
    Ok(out)
}

#[derive(Default)]
struct ModelFrame<'a> {
    exact: BTreeMap<String, mos::ModelAlias<'a>>,
    bins: BTreeMap<String, Vec<mos::ModelAlias<'a>>>,
}

impl<'a> ModelFrame<'a> {
    fn insert(&mut self, name: String, alias: mos::ModelAlias<'a>) -> Result<()> {
        ensure!(
            !self.exact.contains_key(&name),
            "duplicate HSPICE model {name}"
        );
        if alias.selection.mapping.device == Device::Bsim4
            && let Some((base, extension)) = name.split_once('.')
        {
            ensure!(
                !base.is_empty() && !extension.is_empty(),
                "invalid HSPICE bin name {name}"
            );
            self.bins
                .entry(base.to_owned())
                .or_default()
                .push(alias.clone());
        }
        self.exact.insert(name, alias);
        Ok(())
    }
}

struct Binder<'a> {
    budget: &'a WorkBudget,
    source: &'a ModelLibrary,
    index: SubcircuitIndex,
    lines: &'a [&'a str],
    contexts: &'a [DialectContext],
    selections: &'a SelectedModels,
    definitions: Vec<Definition>,
    known_functions: Arc<BTreeSet<String>>,
    precedence: Precedence,
    xmult_in_exp: bool,
    prefix: String,
    serial: usize,
    active: Vec<usize>,
    model_frames: Vec<(usize, ModelFrame<'a>)>,
    pending_mos: BTreeMap<String, mos::Pending<'a>>,
    parameters: Rows,
    emitted_definitions: Rows,
    emitted_bytes: usize,
    limit: usize,
}

impl<'a> Binder<'a> {
    fn account(&mut self, text: String, line: usize) -> Result<(String, usize)> {
        self.budget.charge(Resource::GeneratedNodes, 1)?;
        self.budget
            .charge(Resource::EmittedBytes, text.len().saturating_add(1))?;
        self.emitted_bytes = self
            .emitted_bytes
            .saturating_add(text.len().saturating_add(1));
        ensure!(
            self.emitted_bytes <= self.limit,
            "HSPICE occurrence expansion byte budget exceeded"
        );
        Ok((text, line))
    }

    fn fresh(&mut self, kind: char) -> Result<String> {
        self.budget.charge(Resource::GeneratedNodes, 1)?;
        self.serial = self
            .serial
            .checked_add(1)
            .context("HSPICE occurrence identity overflow")?;
        Ok(format!("{}{kind}{}", self.prefix, self.serial))
    }

    fn declare(&mut self, name: &str, value: &str, line: usize) -> Result<()> {
        let text = format!(".param {name}={{{}}}", inner(value));
        let row = self.account(text, line)?;
        self.parameters.push(row);
        Ok(())
    }

    fn definition_language(&self, scope: usize) -> SourceDialect {
        if scope == 0 {
            self.contexts[0].language
        } else {
            self.contexts[self.source.scopes[scope].source_line - 1].language
        }
    }

    fn precedence(&self, scope: usize) -> Precedence {
        if self.definition_language(scope) == SourceDialect::Hspice {
            self.precedence
        } else {
            Precedence::Local
        }
    }

    fn bind_scope(&mut self, scope: usize, names: Names, parent: Option<&Scope>) -> Result<Scope> {
        let budget = self.budget.clone();
        functions::charge_declarations(&self.definitions[scope].functions, &budget)?;
        let local = self.definitions[scope].functions.clone();
        let precedence = self.precedence(scope);
        let limit = self.limit.saturating_sub(self.emitted_bytes);
        let (scope, rows) = Scope::bind(
            names,
            parent,
            local,
            BindContext {
                precedence,
                known: Arc::clone(&self.known_functions),
                limit,
                budget: &budget,
            },
            |kind| self.fresh(kind),
        )?;
        for (text, line) in rows {
            let row = self.account(text, line)?;
            self.parameters.push(row);
        }
        Ok(scope)
    }

    fn visible(
        &mut self,
        scope: usize,
        caller: &Scope,
        root: &Scope,
        actuals: Parameters,
    ) -> Result<Scope> {
        // PSpice parameters/functions defined in a subcircuit are private to
        // that definition. Only main-circuit declarations are inherited;
        // actual argument expressions still evaluate in the calling instance.
        // PSpice A/D Reference Guide 10.5, .SUBCKT and X, pp. 99–101, 317.
        let parent = if self.definition_language(scope) == SourceDialect::Pspice {
            root
        } else {
            caller
        };
        let parameters = &self.definitions[scope].parameters;
        self.budget.charge(
            Resource::GeneratedNodes,
            parameters.len().saturating_add(parent.names.len()),
        )?;
        for (name, parameter) in parameters {
            self.budget.charge(
                Resource::EmittedBytes,
                name.len().saturating_add(parameter.value.len()),
            )?;
        }
        for (name, alias) in &parent.names {
            self.budget.charge(
                Resource::EmittedBytes,
                name.len().saturating_add(alias.len()),
            )?;
        }
        let mut definitions = parameters.clone();
        definitions.extend(actuals);
        if self.precedence(scope) == Precedence::Global {
            definitions.retain(|name, _| !parent.names.contains_key(name));
        }
        let mut names = parent.names.clone();
        for name in definitions.keys() {
            names.insert(name.clone(), self.fresh('p')?);
        }
        let scope = self.bind_scope(scope, names, Some(parent))?;
        for (name, parameter) in definitions {
            let visible = if parameter.actual { caller } else { &scope };
            let expression = visible.expression(&parameter.value)?;
            self.declare(&scope.names[&name], &expression, parameter.line)?;
        }
        Ok(scope)
    }

    fn enter_models(&mut self, scope: usize, names: &Scope) -> Result<()> {
        let mut aliases = ModelFrame::default();
        self.budget
            .charge(Resource::GeneratedNodes, self.definitions[scope].body.len())?;
        for line in self.definitions[scope].body.clone() {
            self.budget.charge(Resource::Statements, 1)?;
            self.budget
                .charge(Resource::ScannedBytes, self.lines[line].len())?;
            if analysis_plan::command(self.lines[line]) != ".MODEL" {
                continue;
            }
            let card = parse_model_card(self.lines[line], line + 1, &[])?;
            let selection = self
                .selections
                .get(&line)
                .context("missing typed model selection")?;
            let alias = if scope == 0 && selection.mapping.device != Device::Bsim4 {
                card.name.clone()
            } else {
                self.fresh('d')?
            };
            self.budget.charge(Resource::EmittedBytes, alias.len())?;
            aliases.insert(
                card.name.to_ascii_uppercase(),
                mos::ModelAlias {
                    alias: alias.clone(),
                    selection,
                },
            )?;
            let text = models::render(&card, &alias, self.budget, |value| names.expression(value))?;
            let row = self.account(text, line)?;
            self.parameters.push(row);
        }
        self.model_frames.push((scope, aliases));
        Ok(())
    }

    fn candidates(
        &self,
        mut scope: usize,
        model: &str,
        mos: bool,
    ) -> Result<Vec<mos::Candidate<'a>>> {
        let mut candidates = Vec::new();
        loop {
            self.budget
                .charge(Resource::EvaluationSteps, self.model_frames.len())?;
            let (_, aliases) = self
                .model_frames
                .iter()
                .rev()
                .find(|(id, _)| *id == scope)
                .context("inactive lexical HSPICE model scope")?;
            if let Some(alias) = aliases.exact.get(model) {
                self.budget.charge(Resource::GeneratedNodes, 1)?;
                self.budget
                    .charge(Resource::EmittedBytes, alias.alias.len())?;
                candidates.push(mos::Candidate {
                    model: alias.clone(),
                    exact: true,
                });
                break;
            }
            if mos && let Some(bins) = aliases.bins.get(model) {
                for alias in bins {
                    self.budget.charge(Resource::GeneratedNodes, 1)?;
                    self.budget
                        .charge(Resource::EmittedBytes, alias.alias.len())?;
                    candidates.push(mos::Candidate {
                        model: alias.clone(),
                        exact: false,
                    });
                }
            }
            let Some(parent) = self.source.scopes[scope].parent else {
                break;
            };
            scope = parent;
        }
        Ok(candidates)
    }

    fn model_reference(&mut self, scope: usize, text: &str, mos: bool) -> Result<String> {
        let spans = statement_token_spans(text)?;
        let range = spans
            .get(if mos { 5 } else { 3 })
            .context("missing HSPICE device model name")?;
        let model = text[range.clone()].to_ascii_uppercase();
        let candidates = self.candidates(scope, &model, mos)?;
        ensure!(!candidates.is_empty(), "unknown HSPICE model {model}");
        let replacement = if mos {
            let marker = self.fresh('b')?;
            self.pending_mos.insert(
                marker.clone(),
                mos::Pending {
                    source_name: model,
                    candidates,
                },
            );
            marker
        } else {
            let model = &candidates[0].model;
            ensure!(
                model.selection.mapping.letter == 'D',
                "non-diode model referenced by HSPICE diode occurrence"
            );
            model.alias.clone()
        };
        let mut output = text.to_owned();
        output.replace_range(range.clone(), &replacement);
        Ok(output)
    }

    fn call(
        &mut self,
        owner: usize,
        line: usize,
        parent: &Scope,
        root: &Scope,
        inherited_multiplier: Option<&str>,
    ) -> Result<String> {
        let (positional, fields) = positional_prefix(self.lines[line]);
        let (instance, remaining) = positional.split_first().context("empty X occurrence")?;
        let (target, nodes) = remaining
            .split_last()
            .context("X occurrence has no target")?;
        let scope = self
            .index
            .resolve(self.source, owner, target.0)
            .with_context(|| format!("unknown HSPICE subcircuit {}", target.0))?;
        ensure!(
            nodes.len() == self.source.scopes[scope].ports.len(),
            "HSPICE subcircuit {} expects {} terminals, got {}",
            target.0,
            self.source.scopes[scope].ports.len(),
            nodes.len()
        );
        ensure!(
            !self.active.contains(&scope),
            "recursive HSPICE subcircuit {}",
            target.0
        );
        let _depth = self.budget.enter()?;
        self.budget.charge(Resource::EvaluationSteps, 1)?;
        let mut actuals = Parameters::new();
        collect_parameters(fields, line, true, &mut actuals, self.budget)?;
        // HSPICE X syntax gives M a separate physical multiplier meaning.
        // PSpice X PARAMS: values are ordinary actual arguments; do not turn
        // them into a multiplier or apply the HSPICE XMULT_IN_EXP option.
        let local_multiplier = if self.contexts[line].language == SourceDialect::Hspice {
            let multiplier = actuals
                .get("M")
                .map(|parameter| parent.expression(&parameter.value))
                .transpose()?;
            if !self.xmult_in_exp {
                actuals.remove("M");
            }
            multiplier
        } else {
            None
        };
        let multiplier = if let Some(local) = local_multiplier {
            let alias = self.fresh('m')?;
            let expression = inherited_multiplier.map_or_else(
                || inner(&local).to_owned(),
                |parent| format!("({parent})*({})", inner(&local)),
            );
            self.declare(&alias, &expression, line)?;
            Some(alias)
        } else {
            inherited_multiplier.map(str::to_owned)
        };
        let visible = self.visible(scope, parent, root, actuals)?;
        let native_name = self.fresh('s')?;
        self.active.push(scope);
        self.enter_models(scope, &visible)?;
        let mut header = format!(".subckt {native_name}");
        for port in &self.source.scopes[scope].ports {
            write!(header, " {port}")?;
        }
        let opening = self.source.scopes[scope].source_line - 1;
        let closing = self.source.scopes[scope].end_line - 1;
        let mut body = vec![self.account(header, opening)?];
        self.budget
            .charge(Resource::GeneratedNodes, self.definitions[scope].body.len())?;
        for card in self.definitions[scope].body.clone() {
            self.budget.charge(Resource::Statements, 1)?;
            if matches!(
                analysis_plan::command(self.lines[card]).as_str(),
                ".PARAM" | ".MODEL" | ".FUNC"
            ) {
                continue;
            }
            let text = self.card(scope, card, &visible, root, multiplier.as_deref())?;
            body.push(self.account(text, card)?);
        }
        body.push(self.account(format!(".ends {native_name}"), closing)?);
        self.active.pop();
        self.model_frames.pop();
        self.emitted_definitions.extend(body);
        let mut call = instance.0.to_owned();
        for (node, _) in nodes {
            write!(call, " {node}")?;
        }
        write!(call, " {native_name}")?;
        Ok(call)
    }

    fn card(
        &mut self,
        owner: usize,
        line: usize,
        names: &Scope,
        root: &Scope,
        multiplier: Option<&str>,
    ) -> Result<String> {
        let text = self.lines[line];
        self.budget.charge(Resource::ScannedBytes, text.len())?;
        self.budget.charge(Resource::Statements, 1)?;
        let command = analysis_plan::command(text);
        if command.starts_with('X') {
            return self.call(owner, line, names, root, multiplier);
        }
        if command.is_empty() || command.starts_with('*') {
            return Ok(text.to_owned());
        }
        if command == ".PARAM" {
            return names.calls(text);
        }
        let bound = bind_values(text, names, self.limit, self.budget)?;
        let bound = if command.starts_with(['D', 'M']) {
            self.model_reference(owner, &bound, command.starts_with('M'))?
        } else {
            bound
        };
        multiplier_card(&bound, multiplier)
    }

    fn finish(mut self, mos_options: &mos::Options) -> Result<Rows> {
        let title = self.account(self.lines[0].to_owned(), 0)?;
        let names = self.definitions[0]
            .parameters
            .keys()
            .map(|name| (name.clone(), name.clone()))
            .collect();
        let names = self.bind_scope(0, names, None)?;
        self.enter_models(0, &names)?;
        let mut root = Vec::new();
        let mut ends = Vec::new();
        for card in self.definitions[0].body.clone() {
            if matches!(
                analysis_plan::command(self.lines[card]).as_str(),
                ".MODEL" | ".FUNC"
            ) {
                continue;
            }
            let text = self.card(0, card, &names, &names, None)?;
            let row = self.account(text, card)?;
            if analysis_plan::command(self.lines[card]) == ".END" {
                ends.push(row);
            } else {
                root.push(row);
            }
        }
        let mut result = vec![title];
        result.append(&mut self.parameters);
        result.append(&mut root);
        result.append(&mut self.emitted_definitions);
        result.append(&mut ends);
        super::analysis::validate_dc_sources(&result)?;
        let mos_options = mos_options.rewrite(|value| names.expression(value), self.budget)?;
        let library = bound_model_library(&result, self.budget)?;
        mos::finish(
            &mut result,
            &self.pending_mos,
            self.selections,
            &library,
            &mos_options,
            self.budget,
        )?;
        models::resolve_breakdown(&mut result, self.selections, &library, self.budget)?;
        Ok(result)
    }
}

fn bound_model_library(rows: &Rows, budget: &WorkBudget) -> Result<ModelLibrary> {
    let mut linked = String::new();
    for (row, _) in rows {
        budget.charge(Resource::ScannedBytes, row.len())?;
        budget.charge(Resource::EmittedBytes, row.len().saturating_add(1))?;
        writeln!(linked, "{row}")?;
    }
    ModelLibrary::parse_source_with_budget(linked.as_bytes(), RootKind::Deck, budget.clone())
}

pub(super) fn bind(
    lowered: &LoweredExpansion,
    limit: usize,
    budget: &WorkBudget,
) -> Result<(Vec<u8>, Vec<OriginLine>)> {
    let LoweredExpansion {
        bytes,
        origins,
        contexts,
        selections,
        state,
    } = lowered;
    budget.charge(Resource::ScannedBytes, bytes.len())?;
    let text = std::str::from_utf8(bytes)?;
    budget.charge(Resource::Statements, text.lines().count())?;
    let lines = text.lines().collect::<Vec<_>>();
    ensure!(
        lines.len() == origins.len() && lines.len() == contexts.len() && !lines.is_empty(),
        "inconsistent HSPICE source span map"
    );
    let source = ModelLibrary::parse_source_with_budget(bytes, RootKind::Deck, budget.clone())?;
    let index = SubcircuitIndex::new(&source)?;
    ensure!(
        index.duplicates().is_empty(),
        "duplicate HSPICE lexical subcircuit declaration"
    );
    let definitions = collect_definitions(&source, &lines, budget)?;
    let lower = text.to_ascii_lowercase();
    let mut prefix = String::from("hspbind_");
    while lower.contains(&prefix) {
        budget.charge(Resource::ScannedBytes, lower.len())?;
        prefix.push('_');
    }
    for definition in &definitions {
        functions::charge_declarations(&definition.functions, budget)?;
    }
    let known_functions = Arc::new(
        definitions
            .iter()
            .flat_map(|definition| definition.functions.keys().cloned())
            .collect(),
    );
    let binder = Binder {
        budget,
        source: &source,
        index,
        lines: &lines,
        contexts,
        selections,
        definitions,
        known_functions,
        precedence: state.parhier.unwrap_or_default(),
        xmult_in_exp: state.xmult_in_exp.unwrap_or_default(),
        prefix,
        serial: 0,
        active: Vec::new(),
        model_frames: Vec::new(),
        pending_mos: BTreeMap::new(),
        parameters: Vec::new(),
        emitted_definitions: Vec::new(),
        emitted_bytes: 0,
        limit,
    };
    let rows = binder.finish(&state.mos)?;
    let mut output = Vec::new();
    let mut mapped = Vec::new();
    for (text, line) in rows {
        ensure!(
            output.len().saturating_add(text.len()).saturating_add(1) <= limit,
            "HSPICE occurrence output byte budget exceeded"
        );
        budget.charge(Resource::EmittedBytes, text.len().saturating_add(1))?;
        budget.charge(Resource::GeneratedNodes, 1)?;
        budget.charge(
            Resource::EmittedBytes,
            origins
                .get(line)
                .context("invalid HSPICE occurrence origin")?
                .source
                .as_os_str()
                .len(),
        )?;
        output.extend_from_slice(text.as_bytes());
        output.push(b'\n');
        mapped.push(
            origins
                .get(line)
                .context("invalid HSPICE occurrence origin")?
                .clone(),
        );
    }
    Ok((output, mapped))
}
