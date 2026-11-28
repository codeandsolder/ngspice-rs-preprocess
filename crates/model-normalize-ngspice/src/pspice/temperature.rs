//! Separate model measurement temperature from device operating temperature.
//!
//! Cadence `PSpice` Application Note, "Improve Simulation Accuracy When Using
//! Passive Components", temperature attributes: absolute, circuit-relative,
//! and AKO-relative to the inherited absolute temperature.

use super::{parameter, set_parameter};
use crate::model_card::ModelCard;
use anyhow::{Context, Result, ensure};
use model_preprocess_command::{Resource, WorkBudget};

const OPERATING: [&str; 3] = ["t_abs", "t_rel_global", "t_rel_local"];

fn operating_field(card: &ModelCard) -> Result<Option<(&str, &str)>> {
    let mut selected = None;
    for field in &card.parameters {
        if OPERATING
            .iter()
            .any(|name| field.name.eq_ignore_ascii_case(name))
        {
            ensure!(
                selected.is_none(),
                "multiple PSpice operating temperatures on model {}",
                card.name
            );
            selected = Some((
                field.name.as_str(),
                field
                    .expression
                    .as_deref()
                    .context("missing PSpice operating temperature")?,
            ));
        }
    }
    Ok(selected)
}

fn remove_operating(card: &mut ModelCard) {
    card.parameters.retain(|field| {
        !OPERATING
            .iter()
            .any(|name| field.name.eq_ignore_ascii_case(name))
    });
}

pub(super) fn validate_base(card: &ModelCard) -> Result<()> {
    if let Some((name, _)) = operating_field(card)? {
        ensure!(
            !name.eq_ignore_ascii_case("t_rel_local"),
            "PSpice T_REL_LOCAL requires an AKO base model"
        );
    }
    Ok(())
}

// Grouped input can itself contain a braced or quoted parameter expression.
// Both denote an expression, not a literal string.
pub(super) fn expression(value: &str) -> &str {
    let trimmed = value.trim();
    trimmed
        .strip_prefix('{')
        .and_then(|rest| rest.strip_suffix('}'))
        .or_else(|| {
            trimmed
                .strip_prefix('\'')
                .and_then(|rest| rest.strip_suffix('\''))
        })
        .unwrap_or(trimmed)
}

pub(super) fn inherit(
    card: &mut ModelCard,
    derived: &ModelCard,
    budget: &WorkBudget,
) -> Result<()> {
    let Some((name, delta)) = operating_field(derived)? else {
        card.parameters.extend(derived.parameters.iter().cloned());
        return Ok(());
    };
    // The operating-field replacement scans the accumulated model and
    // may retain a copy of its absolute temperature expression.
    budget.charge(
        Resource::EvaluationSteps,
        card.parameters.len().saturating_mul(3),
    )?;
    if let Some(field) = card
        .parameters
        .iter()
        .rev()
        .find(|field| field.name.eq_ignore_ascii_case("t_abs"))
    {
        budget.charge(
            Resource::EmittedBytes,
            field.expression.as_ref().map_or(0, String::len),
        )?;
    }
    let absolute = parameter(card, "t_abs");
    remove_operating(card);
    card.parameters.extend(
        derived
            .parameters
            .iter()
            .filter(|field| !field.name.eq_ignore_ascii_case("t_rel_local"))
            .cloned(),
    );
    if name.eq_ignore_ascii_case("t_rel_local")
        && let Some(base) = absolute
    {
        let base = expression(&base);
        let delta = expression(delta);
        budget.charge(
            Resource::EmittedBytes,
            base.len().saturating_add(delta.len()).saturating_add(7),
        )?;
        let value = format!("{{({base})+({delta})}}");
        set_parameter(card, "t_abs", &value)?;
    }
    // Without an absolute base, T_REL_LOCAL falls back to circuit temperature.
    Ok(())
}
