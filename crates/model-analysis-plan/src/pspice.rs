//! Validate source sweep combinations before binding physical identities.
use super::{AnalysisDialect, AnalysisPlan, Directive, sweep::SweepAxis, sweep::SweepTarget};
use anyhow::{Result, ensure};

fn same_target(left: &SweepTarget, right: &SweepTarget) -> bool {
    match (left, right) {
        (SweepTarget::Temperature, SweepTarget::Temperature) => true,
        (SweepTarget::Source { name: left }, SweepTarget::Source { name: right })
        | (SweepTarget::Parameter { name: left }, SweepTarget::Parameter { name: right }) => {
            left.eq_ignore_ascii_case(right)
        }
        (
            SweepTarget::ModelParameter {
                model: left_model,
                parameter: left_parameter,
                ..
            },
            SweepTarget::ModelParameter {
                model: right_model,
                parameter: right_parameter,
                ..
            },
        ) => {
            left_model.eq_ignore_ascii_case(right_model)
                && left_parameter.eq_ignore_ascii_case(right_parameter)
        }
        _ => false,
    }
}

fn check_dc_axes(
    entry: &super::PlanEntry,
    axes: &[SweepAxis],
    step: Option<(&super::PlanEntry, &SweepAxis)>,
    temperature: Option<&super::PlanEntry>,
) -> Result<()> {
    for (index, axis) in axes.iter().enumerate() {
        ensure!(
            !axes[..index]
                .iter()
                .any(|other| same_target(&axis.target, &other.target)),
            "duplicate PSpice DC target at {:?}",
            entry.origin
        );
        if let Some((step_entry, step_axis)) = step {
            ensure!(
                !same_target(&axis.target, &step_axis.target),
                "PSpice STEP and DC target the same value at {:?} and {:?}",
                step_entry.origin,
                entry.origin
            );
        }
        if let Some(temp) = temperature {
            ensure!(
                axis.target != SweepTarget::Temperature,
                "conflicting PSpice DC and TEMP declarations at {:?} and {:?}",
                entry.origin,
                temp.origin
            );
        }
    }
    Ok(())
}

/// Validate combinations rather than assuming STEP and TEMP form a product.
/// Actual lexical/model target binding (including aliases) is consumer-owned.
pub(super) fn validate(plan: &AnalysisPlan) -> Result<()> {
    let entries = plan
        .entries
        .iter()
        .filter(|entry| entry.dialect == AnalysisDialect::Pspice);
    let mut step = None;
    let mut temperature = None;
    for entry in entries.clone() {
        match (&*entry.command, &entry.directive) {
            (".STEP", Directive::Sweep { axis }) => {
                ensure!(
                    step.replace((entry, axis)).is_none(),
                    "multiple PSpice STEP declarations at {:?}",
                    entry.origin
                );
            }
            (".TEMP", _) => {
                ensure!(
                    temperature.replace(entry).is_none(),
                    "multiple PSpice TEMP declarations at {:?}",
                    entry.origin
                );
            }
            _ => {}
        }
    }
    if let Some((entry, axis)) = step
        && let Some(temp) = temperature
    {
        ensure!(
            axis.target != SweepTarget::Temperature
                && !matches!(temp.directive, Directive::Sweep { .. }),
            "conflicting PSpice STEP and TEMP declarations at {:?} and {:?}",
            entry.origin,
            temp.origin
        );
    }
    for entry in entries {
        if let Directive::DcSweep { axes } = &entry.directive {
            check_dc_axes(entry, axes, step, temperature)?;
        }
    }
    Ok(())
}
