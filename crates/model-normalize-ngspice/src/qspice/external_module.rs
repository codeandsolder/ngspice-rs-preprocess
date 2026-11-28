//! Source declarations only: no module discovery, loading or execution.
use super::{Feature, reject};
use crate::{
    lexical::{Statement, normalize_nbsp, statements, tokens},
    linked_source::{LinkedSource, RootKind, SourceDialect, SourceOrigin},
    model_card::parse_parameter_fields,
};
use anyhow::{Context, Result, ensure};
use model_preprocess_command::{Resource, WorkBudget};

/// One ordered circuit terminal and its explicitly supplied conversion code.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct ModuleTerminal {
    /// Original circuit node spelling.
    pub node: String,
    /// Exact suffix after the acute accent, absent if omitted in source.
    pub conversion: Option<String>,
}

/// One external-module instance parameter in source order.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct ModuleParameter {
    /// Explicit source data type, if passed to the module rather than the device.
    pub data_type: Option<String>,
    /// Original case-preserved parameter name.
    pub name: String,
    /// Uninterpreted source value or expression, including quotation marks.
    pub value: String,
}

/// A source-level dependency on an external module, not an executable artifact.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct ExternalModuleDeclaration {
    /// Declaration's physical source, visit and electrical scope.
    pub origin: SourceOrigin,
    /// Original instance name, including its QSPICE designator.
    pub instance: String,
    /// Input terminal order.
    pub inputs: Vec<ModuleTerminal>,
    /// Output terminal order.
    pub outputs: Vec<ModuleTerminal>,
    /// Common terminal order.
    pub common: Vec<ModuleTerminal>,
    /// Unresolved module dependency, with original spelling and quoting.
    pub module_reference: String,
    /// Ordered typed and device parameters; repeated names are not collapsed.
    pub parameters: Vec<ModuleParameter>,
}

pub(crate) fn is_declaration(text: &[u8]) -> bool {
    text.first().is_some_and(|byte| !byte.is_ascii())
        && std::str::from_utf8(text).is_ok_and(|text| text.trim_start().starts_with(['Ø', 'ø']))
}

fn terminal(raw: &str) -> Result<ModuleTerminal> {
    ensure!(!raw.contains(['«', '»']), "nested terminal group");
    let (node, conversion) = raw
        .rsplit_once('´')
        .map_or((raw, None), |(node, code)| (node, Some(code)));
    ensure!(!node.is_empty(), "missing terminal node");
    ensure!(
        conversion.is_none_or(|code| !code.is_empty()),
        "empty terminal conversion code"
    );
    Ok(ModuleTerminal {
        node: node.to_owned(),
        conversion: conversion.map(str::to_owned),
    })
}

fn group(tail: &mut &str) -> Result<Vec<ModuleTerminal>> {
    let contents = tail
        .trim_start()
        .strip_prefix('«')
        .context("expected opening terminal group «")?;
    let (nodes, rest) = contents
        .split_once('»')
        .context("missing closing terminal group »")?;
    *tail = rest;
    nodes.split_whitespace().map(terminal).collect()
}

fn parameters(text: &str) -> Result<Vec<ModuleParameter>> {
    let mut result = Vec::new();
    let mut type_start = None;
    let mut type_end = 0;
    for field in parse_parameter_fields(text, &[])? {
        if let Some(value) = field.expression {
            result.push(ModuleParameter {
                data_type: type_start
                    .take()
                    .map(|start| text[start..type_end].to_owned()),
                name: field.name,
                value,
            });
        } else {
            type_start.get_or_insert(field.span.name.start);
            type_end = field.span.name.end;
        }
    }
    ensure!(
        type_start.is_none(),
        "external module parameter requires an assignment"
    );
    Ok(result)
}

fn parse(text: &str, origin: &SourceOrigin) -> Result<ExternalModuleDeclaration> {
    let text = text.trim_start();
    let name_end = text
        .find(|character: char| character.is_whitespace() || character == '«')
        .context("external module declaration requires terminal groups")?;
    let instance = &text[..name_end];
    ensure!(
        instance.len() > 'Ø'.len_utf8() && !instance.contains('»'),
        "missing external module instance name"
    );
    let mut tail = &text[name_end..];
    let inputs = group(&mut tail).context("input terminals")?;
    let outputs = group(&mut tail).context("output terminals")?;
    let common = group(&mut tail).context("common terminals")?;
    let tail = tail.trim_start();
    let fields = tokens(tail.as_bytes())?;
    let module = fields.first().context("missing external module identity")?;
    let module_reference = &tail[module.clone()];
    ensure!(
        !matches!(module_reference, "=" | "\"\"" | "''") && !module_reference.contains(['«', '»']),
        "invalid external module identity"
    );
    Ok(ExternalModuleDeclaration {
        origin: origin.clone(),
        instance: instance.to_owned(),
        inputs,
        outputs,
        common,
        module_reference: module_reference.to_owned(),
        parameters: parameters(&tail[module.end..])?,
    })
}

fn parse_statement(
    statement: &Statement,
    origin: &SourceOrigin,
    budget: &WorkBudget,
) -> Result<ExternalModuleDeclaration> {
    budget.charge(
        Resource::EmittedBytes,
        statement
            .text
            .len()
            .saturating_add(origin.path.as_os_str().len()),
    )?;
    let normalized = normalize_nbsp(&statement.text);
    let declaration = std::str::from_utf8(normalized.as_deref().unwrap_or(&statement.text))
        .context("external module source encoding")
        .and_then(|text| parse(text, origin))
        .map_err(|error| {
            reject(
                origin,
                Feature::ExternalModule,
                &format!("invalid declaration: {error:#}"),
            )
        })?;
    budget.charge(
        Resource::GeneratedNodes,
        1usize
            .saturating_add(declaration.inputs.len())
            .saturating_add(declaration.outputs.len())
            .saturating_add(declaration.common.len())
            .saturating_add(declaration.parameters.len()),
    )?;
    Ok(declaration)
}

/// Inspect explicit external-module dependencies in a selected linked source graph.
/// No binary or module-supplied resource is admitted by this inspection.
///
/// # Errors
/// Malformed source declarations, missing provenance, or exhausted work budget.
pub fn external_modules(
    linked: &LinkedSource,
    kind: RootKind,
    budget: &WorkBudget,
) -> Result<Vec<ExternalModuleDeclaration>> {
    let skip_title = kind == RootKind::Deck;
    let body = if skip_title {
        linked
            .bytes
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(&[][..], |end| &linked.bytes[end + 1..])
    } else {
        &linked.bytes
    };
    budget.charge(Resource::ScannedBytes, body.len())?;
    let mut modules = Vec::new();
    for statement in statements(body)? {
        budget.charge(Resource::Statements, 1)?;
        let line = statement.line + usize::from(skip_title);
        if is_declaration(&statement.text) && linked.dialect_at(line) == Some(SourceDialect::Qspice)
        {
            let origin = linked
                .origin(line)
                .context("missing external module source location")?;
            modules.push(parse_statement(&statement, origin, budget)?);
        }
    }
    Ok(modules)
}

pub(super) fn reject_unqualified(
    statement: &Statement,
    origin: &SourceOrigin,
    budget: &WorkBudget,
) -> Result<()> {
    if is_declaration(&statement.text) {
        let declaration = parse_statement(statement, origin, budget)?;
        return Err(reject(
            origin,
            Feature::ExternalModule,
            &format!(
                "{} requires an explicitly qualified implementation of module {}; source declaration is not executable",
                declaration.instance, declaration.module_reference
            ),
        ));
    }
    Ok(())
}
