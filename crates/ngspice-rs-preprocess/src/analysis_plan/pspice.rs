//! `PSpice` numerical sweeps. The reference grammar uses positive linear
//! increment magnitudes, ordered lists, and a distinct nested DC continuation.
//! Statistical realizations are not numerical sweep coordinates.

use super::{
    AnalysisDialect, AnalysisPlan, Directive,
    sweep::{
        Cardinality, CoordinateLaw, CoordinateOrder, LogDirection, RangeMode, SweepAxis,
        SweepExecution, SweepTarget, range_mode, target,
    },
};
use anyhow::{Context, Result, ensure};
use model_normalize_ngspice::{model_card::parse_spice_constant, statement_comma_field_spans};

pub(super) fn arguments(statement: &str) -> Result<Vec<String>> {
    Ok(statement_comma_field_spans(statement)?
        .into_iter()
        .skip(1)
        .map(|span| statement[span].to_owned())
        .collect())
}

fn scalar(value: &str) -> bool {
    parse_spice_constant(value).is_ok() || (value.starts_with('{') && value.ends_with('}'))
}

fn parse_axis(args: &[String], execution: SweepExecution) -> Result<(SweepAxis, &[String])> {
    let (first, rest) = args
        .split_first()
        .context("missing PSpice sweep operands")?;
    let mode = range_mode(first);
    let (target, fields) = target(if mode.is_some() { rest } else { args }, false)?;
    let (law, rest) = if fields
        .first()
        .is_some_and(|field| field.eq_ignore_ascii_case("LIST"))
    {
        ensure!(
            mode.is_none(),
            "PSpice LIST cannot be combined with a range mode"
        );
        let fields = &fields[1..];
        let count = fields.iter().take_while(|field| scalar(field)).count();
        ensure!(count > 0, "missing PSpice sweep list values");
        (
            CoordinateLaw::List {
                values: fields[..count].to_vec(),
            },
            &fields[count..],
        )
    } else {
        let (values, rest) = fields
            .split_first_chunk::<3>()
            .context("PSpice range requires start, stop and increment/density")?;
        ensure!(
            values.iter().all(|field| scalar(field)),
            "invalid PSpice sweep scalar"
        );
        let [start, stop, third] = (*values).clone();
        let law = match mode.unwrap_or(RangeMode::Linear) {
            RangeMode::Linear => CoordinateLaw::LinearMagnitude {
                start,
                stop,
                increment: third,
            },
            RangeMode::Logarithmic(interval) => CoordinateLaw::Logarithmic {
                start,
                stop,
                points_per_interval: third,
                interval,
                direction: if execution == SweepExecution::Continuation {
                    LogDirection::Increasing
                } else {
                    LogDirection::Either
                },
            },
        };
        (law, rest)
    };
    Ok((
        SweepAxis {
            target,
            law,
            execution,
            order: CoordinateOrder::Monotonic,
            cardinality: Cardinality::Nonempty,
        },
        rest,
    ))
}

pub(super) fn step(args: &[String]) -> Result<SweepAxis> {
    let (axis, rest) = parse_axis(args, SweepExecution::Independent)?;
    ensure!(
        rest.is_empty(),
        "PSpice STEP specifies one independent axis"
    );
    Ok(axis)
}

pub(super) fn dc_axes(args: &[String]) -> Result<Vec<SweepAxis>> {
    let (first, rest) = parse_axis(args, SweepExecution::Continuation)?;
    let mut axes = vec![first];
    if !rest.is_empty() {
        let (second, rest) = parse_axis(rest, SweepExecution::Continuation)?;
        ensure!(
            rest.is_empty(),
            "PSpice DC supports one inner and one outer axis"
        );
        axes.push(second);
    }
    Ok(axes)
}

pub(super) fn temperature(args: &[String]) -> Result<SweepAxis> {
    ensure!(
        !args.is_empty() && args.iter().all(|field| scalar(field)),
        "invalid PSpice temperature list"
    );
    Ok(SweepAxis {
        target: SweepTarget::Temperature,
        law: CoordinateLaw::List {
            values: args.to_vec(),
        },
        execution: SweepExecution::Independent,
        order: CoordinateOrder::Declared,
        cardinality: Cardinality::Nonempty,
    })
}

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

pub(super) fn statistical_directive(name: &str) -> bool {
    matches!(
        name,
        ".MC" | ".MONTE" | ".WCASE" | ".WORSTCASE" | ".DISTRIBUTION"
    )
}

/// Preserve the original transient operands. The print interval, bias policy,
/// output gate and adaptive step ceiling (possibly SCHEDULE) are not a native
/// .TRAN default. They must be bound by the source-aware consumer.
pub(super) fn validate_transient(args: &[String]) -> Result<()> {
    let timing = if args
        .last()
        .is_some_and(|field| field.eq_ignore_ascii_case("SKIPBP"))
    {
        &args[..args.len() - 1]
    } else {
        args
    };
    ensure!(
        (2..=4).contains(&timing.len()) && timing.iter().all(|field| scalar(field)),
        "PSpice TRAN requires print step, stop, optional output start/ceiling, and optional SKIPBP"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use model_preprocess_command::WorkBudget;

    #[test]
    fn descending_linear_magnitude_and_ordered_lists() -> Result<()> {
        let budget = WorkBudget::default();
        let axis = step(&arguments(".step LIN I2 5mA,-2mA,1mA")?)?;
        let bound = axis.bind(parse_spice_constant, &budget)?;
        assert_eq!(bound.len(), 8);
        assert_eq!(bound.value(0).map(f64::to_bits), Some(5e-3_f64.to_bits()));
        assert_eq!(
            bound.value(7).map(f64::to_bits),
            Some((-2e-3_f64).to_bits())
        );
        let axis = step(&arguments(".step RES RMOD(R) LIST 3k 2k 1k")?)?;
        assert_eq!(
            axis.target,
            SweepTarget::ModelParameter {
                family: Some("RES".into()),
                model: "RMOD".into(),
                parameter: "R".into()
            }
        );
        let bound = axis.bind(parse_spice_constant, &budget)?;
        assert_eq!(bound.len(), 3);
        assert_eq!(bound.value(0).map(f64::to_bits), Some(3000_f64.to_bits()));
        for invalid in [".step param r 3 1 -1", ".step param r list 2 1 3"] {
            assert!(
                step(&arguments(invalid)?)?
                    .bind(parse_spice_constant, &budget)
                    .is_err(),
                "{invalid}"
            );
        }
        Ok(())
    }

    #[test]
    fn nested_dc_list_and_parameter_bounds_remain_distinct_from_step() -> Result<()> {
        let budget = WorkBudget::default();
        let axes = dc_axes(&arguments(
            ".dc TEMP LIST 0 20 27 50 80 100 PARAM Vsupply {low} 15 .5",
        )?)?;
        assert_eq!(axes.len(), 2);
        assert_eq!(axes[0].target, SweepTarget::Temperature);
        assert_eq!(axes[0].execution, SweepExecution::Continuation);
        assert_eq!(axes[0].bind(parse_spice_constant, &budget)?.len(), 6);
        assert_eq!(
            axes[1].target,
            SweepTarget::Parameter {
                name: "Vsupply".into()
            }
        );
        let bound = axes[1].bind(
            |value| {
                if value == "{low}" {
                    Ok(7.5)
                } else {
                    parse_spice_constant(value)
                }
            },
            &budget,
        )?;
        assert_eq!(bound.len(), 16);
        let axes = dc_axes(&arguments(".dc DEC NPN QFAST(IS) 1e-14 1e-18 5")?)?;
        assert!(axes[0].bind(parse_spice_constant, &budget).is_err());
        let axes = dc_axes(&arguments(".dc V1 0 2 1 V2 0 2 1 V3 0 2 1")?);
        assert!(axes.is_err());
        Ok(())
    }
}
