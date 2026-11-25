//! Explicit HSPICE occurrence bindings. Symbolic aliases avoid evaluating
//! parameters at preparation time, so native analysis still owns expressions.
//! Electrical instance names and terminal identities are never flattened.
use super::{analysis_plan, map_values};
use crate::OriginLine;
use anyhow::{Context, Result, bail, ensure};
use model_normalize_ngspice::{
    linked_source::RootKind,
    model_card::{parse_assignments, positional_prefix},
    parameter_scope::{ModelLibrary, SubcircuitIndex},
    statement_token_spans,
};
use model_preprocess_simplis::rewrite_parameter_symbols;
use std::collections::BTreeMap;
use std::fmt::Write as _;

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
}

fn collect_parameters(
    fields: &str,
    line: usize,
    actual: bool,
    parameters: &mut Parameters,
) -> Result<()> {
    for field in parse_assignments(fields)? {
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

fn collect_definitions(source: &ModelLibrary, lines: &[&str]) -> Result<Vec<Definition>> {
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
        if let Some(&child) = starts.get(&line) {
            ensure!(
                source.scopes[child].parent == Some(owner),
                "inconsistent lexical scope index"
            );
            owner = child;
            let (_, fields) = positional_prefix(text);
            collect_parameters(fields, line, false, &mut definitions[owner].parameters)?;
            ensure!(
                !definitions[owner].parameters.contains_key("M"),
                "explicit HSPICE M formal requires multiplier-default qualification"
            );
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
                collect_parameters(tail, line, false, &mut definitions[owner].parameters)?;
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
        .unwrap_or(expression.trim())
}

fn bind_values(text: &str, names: &Names, limit: usize) -> Result<String> {
    map_values(text, limit, |value, remaining| {
        // Explicit braces make standalone positional parameters native values;
        // the common expression scanner handles expressions without renaming
        // node/current probes, function names or assignment destinations.
        let tokens = statement_token_spans(value)?;
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
                write!(output, "{{{alias}}}")?;
            } else {
                output.push_str(&rewrite_parameter_symbols(word, names, true)?);
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
        matches!(device, b'R' | b'C' | b'L' | b'I' | b'G' | b'F') || current_behavioral,
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

struct Binder<'a> {
    source: &'a ModelLibrary,
    index: SubcircuitIndex,
    lines: &'a [&'a str],
    definitions: Vec<Definition>,
    precedence: Precedence,
    prefix: String,
    serial: usize,
    active: Vec<usize>,
    parameters: Rows,
    emitted_definitions: Rows,
    emitted_bytes: usize,
    limit: usize,
}

impl Binder<'_> {
    fn account(&mut self, text: String, line: usize) -> Result<(String, usize)> {
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

    fn visible(&mut self, scope: usize, parent: &Names, actuals: Parameters) -> Result<Names> {
        let mut definitions = self.definitions[scope].parameters.clone();
        definitions.extend(actuals);
        if self.precedence == Precedence::Global {
            definitions.retain(|name, _| !parent.contains_key(name));
        }
        let mut names = parent.clone();
        for name in definitions.keys() {
            names.insert(name.clone(), self.fresh('p')?);
        }
        for (name, parameter) in definitions {
            let visible = if parameter.actual { parent } else { &names };
            let expression = rewrite_parameter_symbols(&parameter.value, visible, false)?;
            self.declare(&names[&name], &expression, parameter.line)?;
        }
        Ok(names)
    }

    fn call(
        &mut self,
        owner: usize,
        line: usize,
        parent: &Names,
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
            self.active.len() < 128 && !self.active.contains(&scope),
            "recursive HSPICE subcircuit {}",
            target.0
        );
        let mut actuals = Parameters::new();
        collect_parameters(fields, line, true, &mut actuals)?;
        let local_multiplier = actuals
            .remove("M")
            .map(|parameter| rewrite_parameter_symbols(&parameter.value, parent, false))
            .transpose()?;
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
        let visible = self.visible(scope, parent, actuals)?;
        let native_name = self.fresh('s')?;
        self.active.push(scope);
        let mut header = format!(".subckt {native_name}");
        for port in &self.source.scopes[scope].ports {
            write!(header, " {port}")?;
        }
        let opening = self.source.scopes[scope].source_line - 1;
        let closing = self.source.scopes[scope].end_line - 1;
        let mut body = vec![self.account(header, opening)?];
        for card in self.definitions[scope].body.clone() {
            if analysis_plan::command(self.lines[card]) == ".PARAM" {
                continue;
            }
            let text = self.card(scope, card, &visible, multiplier.as_deref())?;
            body.push(self.account(text, card)?);
        }
        body.push(self.account(format!(".ends {native_name}"), closing)?);
        self.active.pop();
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
        names: &Names,
        multiplier: Option<&str>,
    ) -> Result<String> {
        let text = self.lines[line];
        let command = analysis_plan::command(text);
        if command.starts_with('X') {
            return self.call(owner, line, names, multiplier);
        }
        if command.is_empty()
            || command.starts_with('*')
            || command == ".PARAM"
            || command == ".FUNC"
        {
            return Ok(text.to_owned());
        }
        let bound = bind_values(text, names, self.limit)?;
        multiplier_card(&bound, multiplier)
    }

    fn finish(mut self) -> Result<Rows> {
        let title = self.account(self.lines[0].to_owned(), 0)?;
        let names = self.definitions[0]
            .parameters
            .keys()
            .map(|name| (name.clone(), name.clone()))
            .collect();
        let mut root = Vec::new();
        let mut ends = Vec::new();
        for card in self.definitions[0].body.clone() {
            let text = self.card(0, card, &names, None)?;
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
        Ok(result)
    }
}

pub fn bind(
    bytes: &[u8],
    origins: &[OriginLine],
    precedence: Precedence,
    limit: usize,
) -> Result<(Vec<u8>, Vec<OriginLine>)> {
    let text = std::str::from_utf8(bytes)?;
    let lines = text.lines().collect::<Vec<_>>();
    ensure!(
        lines.len() == origins.len() && !lines.is_empty(),
        "inconsistent HSPICE source span map"
    );
    let source = ModelLibrary::parse_source(bytes, RootKind::Deck)?;
    let index = SubcircuitIndex::new(&source)?;
    ensure!(
        index.duplicates().is_empty(),
        "duplicate HSPICE lexical subcircuit declaration"
    );
    let definitions = collect_definitions(&source, &lines)?;
    let lower = text.to_ascii_lowercase();
    let mut prefix = String::from("hspbind_");
    while lower.contains(&prefix) {
        prefix.push('_');
    }
    // Source function macros have simulator-specific visibility; don't
    // silently reinterpret them across specialized parameter scopes.
    if definitions
        .iter()
        .skip(1)
        .any(|definition| !definition.parameters.is_empty())
        && lines
            .iter()
            .any(|line| analysis_plan::command(line) == ".FUNC")
    {
        bail!("HSPICE functions with hierarchical parameters require qualified binding");
    }
    let binder = Binder {
        source: &source,
        index,
        lines: &lines,
        definitions,
        precedence,
        prefix,
        serial: 0,
        active: Vec::new(),
        parameters: Vec::new(),
        emitted_definitions: Vec::new(),
        emitted_bytes: 0,
        limit,
    };
    let rows = binder.finish()?;
    let mut output = Vec::new();
    let mut mapped = Vec::new();
    for (text, line) in rows {
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
