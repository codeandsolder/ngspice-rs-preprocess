//! Resolve HSPICE parameter precedence without evaluating scalar values.
//! Each native definition is specialized by its lexical owner and the
//! *names* present in the caller scope, not by parameter values. This
//! preserves dynamic expressions, model visibility, and instance paths.
use super::{braced, command, tail, with_text};
use anyhow::{Context, Result, bail, ensure};
use model_normalize_ngspice::{model_card::parse_parameter_fields, statement_token_spans};
use model_preprocess_simplis::source_dialect::ScopedStatement;
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
};

type Names = BTreeSet<String>;

#[derive(Clone)]
struct Parameter {
    spelling: String,
    value: String,
    origin: usize,
}

type Parameters = BTreeMap<String, Parameter>;

struct Signature {
    prefix: Vec<String>,
    parameters: Parameters,
}

fn parse_parameters(text: &str, origin: usize) -> Result<Parameters> {
    let mut parameters = Parameters::new();
    for field in parse_parameter_fields(text, &[])? {
        let value = field
            .expression
            .with_context(|| format!("parameter {} requires a value", field.name))?;
        // String and vector-valued subcircuit arguments need a distinct
        // typed binding; never turn one into a scalar by accident.
        ensure!(
            !field.name.contains('('),
            "invalid scalar parameter {}",
            field.name
        );
        let name = field.name.to_ascii_uppercase();
        parameters.insert(
            name,
            Parameter {
                spelling: field.name,
                value,
                origin,
            },
        );
    }
    Ok(parameters)
}

fn signature(text: &str, origin: usize) -> Result<Signature> {
    let fields = statement_token_spans(text)?;
    let assignment = fields.iter().position(|span| &text[span.clone()] == "=");
    let marker = fields
        .iter()
        .position(|span| text[span.clone()].eq_ignore_ascii_case("params:"));
    let first = match (assignment, marker) {
        (Some(index), Some(marker)) => {
            ensure!(marker + 2 == index, "invalid PARAMS: field list");
            marker
        }
        (Some(index), None) => index.checked_sub(1).context("missing parameter name")?,
        (None, Some(marker)) => {
            ensure!(marker + 1 == fields.len(), "invalid parameter field list");
            marker
        }
        (None, None) => fields.len(),
    };
    let prefix = fields[..first]
        .iter()
        .map(|span| text[span.clone()].to_owned())
        .collect();
    let start = first + usize::from(marker.is_some());
    let parameters = fields.get(start).map_or_else(
        || Ok(Parameters::new()),
        |span| parse_parameters(&text[span.start..], origin),
    )?;
    Ok(Signature { prefix, parameters })
}

#[derive(Clone, Default)]
struct Definition {
    name: String,
    header: Option<usize>,
    prefix: Vec<String>,
    end: Option<usize>,
    parameters: Parameters,
    body: Vec<usize>,
    children: BTreeMap<String, usize>,
    models: Names,
}

struct SourceGraph<'a> {
    source: &'a [ScopedStatement],
    definitions: Vec<Definition>,
    deck: bool,
}

impl<'a> SourceGraph<'a> {
    fn parse(source: &'a [ScopedStatement], deck: bool) -> Result<Self> {
        let mut definitions = vec![Definition::default()];
        let mut stack = vec![0];
        for (index, statement) in source.iter().enumerate() {
            let scope = *stack.last().context("missing lexical source scope")?;
            if deck && index == 0 {
                definitions[scope].body.push(index);
                continue;
            }
            match command(&statement.text).as_str() {
                ".SUBCKT" => {
                    ensure!(
                        stack.len() <= 128,
                        "source subcircuit nesting limit exceeded"
                    );
                    let signature = signature(&statement.text, index)?;
                    let name = signature
                        .prefix
                        .get(1)
                        .context("missing subcircuit name")?
                        .clone();
                    let child = definitions.len();
                    ensure!(
                        definitions[scope]
                            .children
                            .insert(name.to_ascii_uppercase(), child)
                            .is_none(),
                        "duplicate subcircuit {name} in one lexical scope"
                    );
                    definitions.push(Definition {
                        name,
                        header: Some(index),
                        prefix: signature.prefix,
                        parameters: signature.parameters,
                        ..Definition::default()
                    });
                    stack.push(child);
                }
                ".ENDS" => {
                    ensure!(scope != 0, "unmatched .ENDS");
                    let name = tail(&statement.text);
                    ensure!(
                        name.is_empty() || name.eq_ignore_ascii_case(&definitions[scope].name),
                        ".ENDS {name} does not match {}",
                        definitions[scope].name
                    );
                    definitions[scope].end = Some(index);
                    stack.pop();
                }
                ".PARAM" => {
                    let parameters = parse_parameters(tail(&statement.text), index)?;
                    ensure!(
                        !parameters.contains_key("M"),
                        "M is reserved for hierarchical multiplicity"
                    );
                    // Later declarations win at the same level; a body
                    // declaration is the default for an instance actual.
                    definitions[scope].parameters.extend(parameters);
                    definitions[scope].body.push(index);
                }
                ".MODEL" => {
                    let name = statement
                        .text
                        .split_whitespace()
                        .nth(1)
                        .context("missing model name")?;
                    definitions[scope].models.insert(name.to_ascii_uppercase());
                    definitions[scope].body.push(index);
                }
                _ => definitions[scope].body.push(index),
            }
        }
        ensure!(stack.len() == 1, "unterminated subcircuit definition");
        Ok(Self {
            source,
            definitions,
            deck,
        })
    }
}

#[derive(Clone, Eq, Ord, PartialEq, PartialOrd)]
struct Key {
    definition: usize,
    owner: usize,
    inherited: Names,
    implicit: Names,
}

struct Variant {
    definition: usize,
    owner: Option<usize>,
    name: String,
    visible: Names,
    models: Names,
    lines: Vec<ScopedStatement>,
    children: Vec<usize>,
}

struct Compiler<'a> {
    graph: SourceGraph<'a>,
    local: bool,
    prefix: String,
    registry: BTreeMap<Key, usize>,
    variants: Vec<Variant>,
    limit: usize,
    used: usize,
}

impl<'a> Compiler<'a> {
    fn new(graph: SourceGraph<'a>, local: bool, limit: usize) -> Self {
        let mut prefix = "hspice_scope_".to_owned();
        while graph
            .source
            .iter()
            .any(|line| line.text.to_ascii_lowercase().contains(&prefix))
        {
            prefix.push('_');
        }
        let root = &graph.definitions[0];
        let visible = root.parameters.keys().cloned().collect();
        let models = root.models.clone();
        Self {
            graph,
            local,
            prefix,
            registry: BTreeMap::new(),
            variants: vec![Variant {
                definition: 0,
                owner: None,
                name: String::new(),
                visible,
                models,
                lines: Vec::new(),
                children: Vec::new(),
            }],
            limit,
            used: 0,
        }
    }

    fn append(&mut self, variant: usize, origin: usize, text: String) -> Result<()> {
        self.used = self
            .used
            .checked_add(text.len().saturating_add(1))
            .context("scope output length overflow")?;
        ensure!(
            self.used <= self.limit,
            "hierarchical source expansion byte budget exceeded"
        );
        self.variants[variant]
            .lines
            .push(with_text(&self.graph.source[origin], text));
        Ok(())
    }

    fn resolve(&self, caller: usize, name: &str) -> Result<(usize, usize)> {
        let name = name.to_ascii_uppercase();
        let mut scope = Some(caller);
        while let Some(variant) = scope {
            let entry = &self.variants[variant];
            if let Some(&definition) = self.graph.definitions[entry.definition].children.get(&name)
            {
                return Ok((definition, variant));
            }
            scope = entry.owner;
        }
        bail!("unresolved HSPICE subcircuit {name}")
    }

    fn instantiate(&mut self, key: &Key, stack: &mut Vec<usize>) -> Result<usize> {
        ensure!(
            !stack.contains(&key.definition),
            "recursive HSPICE subcircuit {}",
            self.graph.definitions[key.definition].name
        );
        ensure!(stack.len() < 128, "HSPICE call depth exceeded");
        if let Some(&id) = self.registry.get(key) {
            return Ok(id);
        }
        let definition = self.graph.definitions[key.definition].clone();
        let id = self.variants.len();
        // Library entry points retain their declared names. Additional
        // internally specialized definitions are hygienic, including
        // lexically nested definitions.
        let export = !self.graph.deck
            && key.owner == 0
            && key.inherited == self.variants[0].visible
            && key.implicit.is_empty();
        let name = if export {
            definition.name.clone()
        } else {
            format!("{}{id}", self.prefix)
        };
        let visible = key
            .inherited
            .iter()
            .chain(definition.parameters.keys())
            .chain(key.implicit.iter())
            .cloned()
            .collect();
        let mut models = self.variants[key.owner].models.clone();
        models.extend(definition.models.iter().cloned());
        self.variants.push(Variant {
            definition: key.definition,
            owner: Some(key.owner),
            name: name.clone(),
            visible,
            models,
            lines: Vec::new(),
            children: Vec::new(),
        });
        self.variants[key.owner].children.push(id);
        self.registry.insert(key.clone(), id);
        stack.push(key.definition);
        let mut prefix = definition.prefix.clone();
        prefix[1] = name;
        let mut header = prefix.join(" ");
        // M is never overridden by PARHIER; it has its own multiplicative
        // semantics and is always an explicit native formal.
        header.push_str(" params:");
        self.append(
            id,
            definition.header.context("missing subcircuit header")?,
            header,
        )?;
        for (name, parameter) in &definition.parameters {
            if !self.local && name != "M" && key.inherited.contains(name) {
                continue;
            }
            self.append(
                id,
                parameter.origin,
                format!("+ {}={}", parameter.spelling, braced(&parameter.value)),
            )?;
        }
        for name in &key.implicit {
            // Every generated call supplies the required actual. Do not
            // invent a fallback value for an absent argument.
            self.append(
                id,
                definition.header.context("missing header")?,
                format!("+ {name}={{{}required_{id}_{name}}}", self.prefix),
            )?;
        }
        self.compile_body(id, &definition, stack)?;
        stack.pop();
        Ok(id)
    }

    fn compile_call(&mut self, caller: usize, origin: usize, stack: &mut Vec<usize>) -> Result<()> {
        let text = &self.graph.source[origin].text;
        let mut signature = signature(text, origin)?;
        let name = signature
            .prefix
            .last()
            .context("missing subcircuit target")?;
        ensure!(signature.prefix.len() >= 3, "incomplete subcircuit call");
        let (definition, owner) = self.resolve(caller, name)?;
        let inherited = self.variants[caller]
            .visible
            .iter()
            .filter(|name| name.as_str() != "M")
            .cloned()
            .collect::<Names>();
        if !self.local {
            signature
                .parameters
                .retain(|name, _| name == "M" || !inherited.contains(name));
        }
        let implicit = signature
            .parameters
            .keys()
            .filter(|name| {
                !self.graph.definitions[definition]
                    .parameters
                    .contains_key(*name)
            })
            .cloned()
            .collect();
        let key = Key {
            definition,
            owner,
            inherited,
            implicit,
        };
        let target = self.instantiate(&key, stack)?;
        let last = signature
            .prefix
            .last_mut()
            .context("missing subcircuit target")?;
        last.clone_from(&self.variants[target].name);
        let mut text = signature.prefix.join(" ");
        for parameter in signature.parameters.values() {
            write!(text, " {}={}", parameter.spelling, braced(&parameter.value))?;
        }
        self.append(caller, origin, text)
    }

    fn compile_body(
        &mut self,
        variant: usize,
        definition: &Definition,
        stack: &mut Vec<usize>,
    ) -> Result<()> {
        for &origin in &definition.body {
            let statement = &self.graph.source[origin];
            if self.graph.deck && origin == 0 {
                self.append(variant, origin, statement.text.clone())?;
                continue;
            }
            let name = command(&statement.text);
            if name == ".PARAM" {
                // Subcircuit .PARAM declarations were folded into formals,
                // with their original physical spans on continuation lines.
                if variant == 0 {
                    let parameters = parse_parameters(tail(&statement.text), origin)?;
                    for (key, parameter) in parameters {
                        if definition
                            .parameters
                            .get(&key)
                            .is_some_and(|last| last.origin == origin)
                        {
                            self.append(
                                variant,
                                origin,
                                format!(
                                    ".param {}={}",
                                    parameter.spelling,
                                    braced(&parameter.value)
                                ),
                            )?;
                        }
                    }
                }
            } else if name.starts_with('X') {
                self.compile_call(variant, origin, stack)?;
            } else {
                let entry = &self.variants[variant];
                let text = bind_bare_values(&statement.text, &entry.visible, &entry.models)?;
                self.append(variant, origin, text)?;
            }
        }
        Ok(())
    }

    fn compile(mut self) -> Result<Vec<ScopedStatement>> {
        let root = self.graph.definitions[0].clone();
        self.compile_body(0, &root, &mut Vec::new())?;
        if !self.graph.deck {
            for definition in root.children.values() {
                self.instantiate(
                    &Key {
                        definition: *definition,
                        owner: 0,
                        inherited: self.variants[0].visible.clone(),
                        implicit: Names::new(),
                    },
                    &mut Vec::new(),
                )?;
            }
        }
        let mut output = Vec::new();
        self.render(0, &mut output)?;
        Ok(output)
    }

    fn render(&self, id: usize, output: &mut Vec<ScopedStatement>) -> Result<()> {
        let variant = &self.variants[id];
        let mut rendered_children = false;
        for (index, line) in variant.lines.iter().enumerate() {
            if id == 0 && !(self.graph.deck && index == 0) && command(&line.text) == ".END" {
                ensure!(!rendered_children, "multiple root .END statements");
                for &child in &variant.children {
                    self.render(child, output)?;
                }
                rendered_children = true;
            }
            output.push(line.clone());
        }
        if !rendered_children {
            for &child in &variant.children {
                self.render(child, output)?;
            }
        }
        if id != 0 {
            let origin = self.graph.definitions[variant.definition]
                .end
                .context("missing .ENDS")?;
            output.push(with_text(
                &self.graph.source[origin],
                format!(".ends {}", variant.name),
            ));
        }
        Ok(())
    }
}

/// Native numparam needs braces around unquoted parameter-valued device
/// fields. Use the structural token positions and the bound symbol set.
fn bind_bare_values(text: &str, names: &Names, models: &Names) -> Result<String> {
    let name = command(text);
    if name.is_empty() || name.starts_with(['.', '*', '$']) || names.is_empty() {
        return Ok(text.to_owned());
    }
    let fields = statement_token_spans(text)?;
    let mut edits = Vec::new();
    for (index, span) in fields.iter().enumerate().skip(1) {
        let value = &text[span.clone()];
        let key = value.to_ascii_uppercase();
        if !names.contains(&key) || models.contains(&key) {
            continue;
        }
        let assigned = index >= 2 && &text[fields[index - 1].clone()] == "=";
        let positional = match name.as_bytes()[0] {
            b'R' | b'C' | b'L' | b'K' => index == 3,
            b'V' | b'I' => index >= 3,
            b'E' | b'G' => index == 5,
            b'F' | b'H' | b'D' => index == 4,
            _ => false,
        };
        if assigned || positional {
            edits.push(span.clone());
        }
    }
    let mut text = text.to_owned();
    for span in edits.into_iter().rev() {
        let replacement = braced(&text[span.clone()]);
        text.replace_range(span, &replacement);
    }
    Ok(text)
}

pub(super) fn bind(
    source: &[ScopedStatement],
    deck: bool,
    local: bool,
    limit: usize,
) -> Result<Vec<ScopedStatement>> {
    Compiler::new(SourceGraph::parse(source, deck)?, local, limit).compile()
}
