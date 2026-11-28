//! Source-only fixed-width built-in declarations, before physical binding.
use super::{Feature, reject};
use crate::{
    lexical::{Statement, tokens},
    linked_source::{LinkedSource, RootKind, SourceOrigin},
};
use anyhow::{Context, Result, ensure};
use model_preprocess_command::{Resource, WorkBudget};

/// Fixed-width QSPICE built-in device designator.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
pub enum BuiltinClass {
    /// Transconductance amplifier or rail-to-rail operational amplifier.
    Amplifier,
    /// Logic and other built-in functions.
    Function,
    /// Digital-to-analog converter.
    Dac,
    /// Dual gate driver.
    DualGateDriver,
}

impl BuiltinClass {
    fn from_statement(text: &[u8]) -> Option<Self> {
        [
            ("Ã", Self::Amplifier),
            ("¥", Self::Function),
            ("€", Self::Dac),
            ("£", Self::DualGateDriver),
        ]
        .into_iter()
        .find_map(|(prefix, class)| text.starts_with(prefix.as_bytes()).then_some(class))
    }

    /// Number of source slots, including unassigned ports.
    #[must_use = "the count determines the selector position"]
    pub const fn port_count(self) -> usize {
        match self {
            Self::Amplifier | Self::Function => 16,
            Self::Dac => 32,
            Self::DualGateDriver => 64,
        }
    }
}

/// One positional source port, before family-specific pin-role resolution.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub enum BuiltinPort {
    /// Explicit unassigned-slot marker, not a shared electrical net.
    Unassigned,
    /// Original node identity (including literal ground).
    Node(String),
}

/// Source declaration, not a prepared physical or event kernel.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct BuiltinDeclaration {
    /// Physical origin, lexical scope and source visit.
    pub origin: SourceOrigin,
    /// Original instance name, including the designator.
    pub instance: String,
    /// Device designator class, distinct from the model selector.
    pub class: BuiltinClass,
    /// Ordered source slots, including unused ports.
    pub ports: Vec<BuiltinPort>,
    /// Original model or function selector, without assuming model resolution.
    pub selector: String,
    /// Uninterpreted parameter tail. Composite values and duplicate fields
    /// retain source order until the selected family supplies their grammar.
    pub parameter_text: String,
}

pub(crate) fn is_declaration(text: &[u8]) -> bool {
    BuiltinClass::from_statement(text).is_some()
}

fn parse(text: &str, origin: &SourceOrigin) -> Result<BuiltinDeclaration> {
    let class = BuiltinClass::from_statement(text.as_bytes()).context("unknown built-in class")?;
    let fields = tokens(text.as_bytes())?;
    let name = fields.first().context("missing built-in instance")?;
    let instance = &text[name.clone()];
    ensure!(
        instance.chars().nth(1).is_some(),
        "missing built-in instance identity"
    );
    let selector_index = class.port_count() + 1;
    let selector = fields.get(selector_index).with_context(|| {
        format!(
            "expected {} ordered port slots followed by a model selector",
            class.port_count()
        )
    })?;
    let ports = fields
        .get(1..selector_index)
        .context("missing built-in port slots")?
        .iter()
        .map(|field| match &text[field.clone()] {
            "¥" => Ok(BuiltinPort::Unassigned),
            "=" => anyhow::bail!("assignment in positional port slots"),
            node => Ok(BuiltinPort::Node(node.to_owned())),
        })
        .collect::<Result<Vec<_>>>()?;
    let selector_text = &text[selector.clone()];
    ensure!(
        !matches!(selector_text, "=" | "¥" | "\"\"" | "''"),
        "missing built-in model selector"
    );
    ensure!(
        fields
            .get(selector_index + 1)
            .is_none_or(|field| &text[field.clone()] != "="),
        "assignment in model selector position"
    );
    Ok(BuiltinDeclaration {
        origin: origin.clone(),
        instance: instance.to_owned(),
        class,
        ports,
        selector: selector_text.to_owned(),
        parameter_text: text[selector.end..].trim_start().to_owned(),
    })
}

fn parse_statement(
    statement: &Statement,
    origin: &SourceOrigin,
    budget: &WorkBudget,
) -> Result<BuiltinDeclaration> {
    budget.charge(
        Resource::EmittedBytes,
        statement
            .text
            .len()
            .saturating_add(origin.path.as_os_str().len()),
    )?;
    let declaration = std::str::from_utf8(&statement.text)
        .context("built-in source encoding")
        .and_then(|text| parse(text, origin))
        .map_err(|error| {
            reject(
                origin,
                Feature::SpecialDevice,
                &format!("invalid built-in declaration: {error:#}"),
            )
        })?;
    budget.charge(
        Resource::GeneratedNodes,
        1usize.saturating_add(declaration.ports.len()),
    )?;
    Ok(declaration)
}

/// Inspect fixed-width built-in declarations in the selected linked graph.
/// This does not resolve family-specific pin roles or authorize simulation.
///
/// # Errors
/// Malformed source declarations, missing provenance, or exhausted work budget.
pub fn builtin_devices(
    linked: &LinkedSource,
    kind: RootKind,
    budget: &WorkBudget,
) -> Result<Vec<BuiltinDeclaration>> {
    super::source_declarations::inspect(linked, kind, budget, is_declaration, parse_statement)
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
            Feature::SpecialDevice,
            &format!(
                "{} selector {} requires a qualified native physical or event kernel",
                declaration.instance, declaration.selector
            ),
        ));
    }
    Ok(())
}
