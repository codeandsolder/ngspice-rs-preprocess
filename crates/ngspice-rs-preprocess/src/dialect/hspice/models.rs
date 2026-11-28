//! Standalone nongeometric HSPICE junction diodes.
//!
//! HSPICE E-2010.12, chapter 3, specifies AREA*M scaling of IBV even
//! for LEVEL=1. Native level 3 provides this scaling without geometry
//! when the instance has only dimensionless AREA/PJ. DCAP=2 is the
//! zero-threshold special case of the native depletion-charge law.
use super::SelectedModels;
use anyhow::{Context, Result, bail, ensure};
use model_normalize_ngspice::{
    model_card::{ModelCard, parse_assignments, parse_model_card},
    parameter_scope::ModelLibrary,
};
use model_preprocess_command::{Resource, WorkBudget};
use model_preprocess_simplis::{
    lower_spice_expression,
    source_dialect::{
        DialectContext, ScopedStatement, SourceDialect,
        model_mapping::{Device, MappingTable, ModelSelection},
    },
};
use std::{collections::BTreeSet, fmt::Write as _};

fn canonical(name: &str) -> Result<&'static str> {
    Ok(match name.to_ascii_uppercase().as_str() {
        "IS" | "JS" => "IS",
        "JSW" | "ISW" | "ISP" => "JSW",
        "N" => "N",
        "RS" => "RS",
        "CJ" | "CJO" | "CJ0" | "CJA" => "CJ",
        "CJP" | "CJSW" => "CJP",
        "PB" | "VJ" | "PHI" | "PHA" => "PB",
        "PHP" => "PHP",
        "M" | "MJ" | "EXA" => "M",
        "MJSW" | "EXP" => "MJSW",
        "FC" => "FC",
        "FCS" => "FCS",
        "TT" => "TT",
        "BV" | "VB" | "VAR" | "VRB" => "BV",
        "IBV" | "IB" => "IBV",
        "NBV" => "NBV",
        "IK" | "IKF" | "JBF" => "IKF",
        "IKR" | "JBR" => "IKR",
        "EG" => "EG",
        "XTI" => "XTI",
        "TNOM" | "TREF" => "TNOM",
        "KF" => "KF",
        "AF" => "AF",
        "LEVEL" => "LEVEL",
        _ => bail!("unqualified HSPICE diode model parameter {name}"),
    })
}

pub fn render(
    card: &ModelCard,
    name: &str,
    budget: &WorkBudget,
    mut map: impl FnMut(&str) -> Result<String>,
) -> Result<String> {
    budget.charge(
        Resource::EmittedBytes,
        name.len()
            .saturating_add(card.family.len())
            .saturating_add(11),
    )?;
    let mut text = format!(".model {name} {} (", card.family);
    for parameter in &card.parameters {
        budget.charge(Resource::EvaluationSteps, 1)?;
        budget.charge(
            Resource::EmittedBytes,
            parameter.name.len().saturating_add(1),
        )?;
        write!(text, " {}", parameter.name)?;
        if let Some(value) = &parameter.expression {
            let mapped = map(value)?;
            budget.charge(Resource::EmittedBytes, mapped.len().saturating_add(1))?;
            write!(text, "={mapped}")?;
        }
    }
    text.push(')');
    Ok(text)
}

pub fn lower(
    statement: &ScopedStatement,
    scope: &[String],
    dcap: u8,
    limit: usize,
    budget: &WorkBudget,
) -> Result<(String, ModelSelection)> {
    ensure!(
        statement.context.language == SourceDialect::Hspice,
        "model lowering for this resource language is not qualified"
    );
    let selection = MappingTable::select_standalone_hspice(statement, scope, budget)?;
    budget.charge(Resource::GeneratedNodes, selection.card.parameters.len())?;
    budget.charge(Resource::EmittedBytes, statement.text.len())?;
    let mut selected = selection.clone();
    if selected.mapping.device == Device::Bsim4 {
        super::mos::qualify_model(&mut selected)?;
        let native = selected.lower()?;
        let card = parse_model_card(&native, statement.first_line, &[])?;
        let text = render(&card, &card.name, budget, |value| {
            lower_spice_expression(value, statement.context, limit, budget)
        })?;
        ensure!(
            text.len() <= limit,
            "HSPICE model expansion byte budget exceeded"
        );
        return Ok((text, selection));
    }
    let mut seen = BTreeSet::new();
    for parameter in &mut selected.card.parameters {
        parameter.name = canonical(&parameter.name)?.to_owned();
        ensure!(
            seen.insert(parameter.name.clone()),
            "duplicate HSPICE diode model parameter {}",
            parameter.name
        );
        ensure!(
            parameter.expression.is_some(),
            "model field requires a value"
        );
    }
    if dcap == 2 {
        selected
            .card
            .parameters
            .retain(|field| !matches!(field.name.as_str(), "FC" | "FCS"));
        selected
            .card
            .parameters
            .extend(parse_assignments("FC=0 FCS=0")?);
    }
    let native = selected.lower()?;
    let card = parse_model_card(&native, statement.first_line, &[])?;
    let text = render(&card, &card.name, budget, |value| {
        lower_spice_expression(
            value,
            DialectContext {
                language: SourceDialect::Hspice,
                separator: None,
            },
            limit,
            budget,
        )
    })?;
    ensure!(
        text.len() <= limit,
        "HSPICE model expansion byte budget exceeded"
    );
    Ok((text, selection))
}

/// Resolve HSPICE's zero-breakdown sentinel after occurrence binding. A
/// standalone include cannot assume values of parameters supplied by its caller.
pub(super) fn resolve_breakdown(
    rows: &mut [(String, usize)],
    selections: &SelectedModels,
    library: &ModelLibrary,
    budget: &WorkBudget,
) -> Result<()> {
    for (row, declaration) in rows.iter_mut().skip(1) {
        budget.charge(Resource::Statements, 1)?;
        budget.charge(Resource::ScannedBytes, row.len())?;
        if !selections.get(declaration).is_some_and(|selection| {
            selection.context.language == SourceDialect::Hspice
                && selection.mapping.device == Device::Diode3
        }) {
            continue;
        }
        let mut card = parse_model_card(row, *declaration + 1, &[])?;
        if let Some(position) = card
            .parameters
            .iter()
            .position(|field| field.name.eq_ignore_ascii_case("BV"))
        {
            let expression = card.parameters[position]
                .expression
                .as_deref()
                .context("BV requires a value")?;
            let value = library.evaluate_scalar(0, expression, &[])?;
            ensure!(
                value.is_finite() && value >= 0.,
                "invalid HSPICE BV {value}"
            );
            if value == 0. {
                card.parameters.remove(position);
                *row = render(&card, &card.name, budget, |value| Ok(value.to_owned()))?;
            }
        }
    }
    Ok(())
}
