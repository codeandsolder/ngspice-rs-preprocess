//! SIMetrix sweep declarations. Its general sweep grammar is distinct from
//! the compatibility STEP grammar: LIN takes a point count, not a step size.
//! Inline SWEEP axes belong to their individual analysis request.

use super::{
    AnalysisKind,
    sweep::{
        Cardinality, CoordinateLaw, CoordinateOrder, LogInterval, SweepAxis, SweepExecution,
        SweepTarget, name,
    },
};
use anyhow::{Context, Result, ensure};
use model_normalize_ngspice::statement_token_spans;

fn keyword(raw: &str) -> bool {
    matches!(
        raw.to_ascii_uppercase().as_str(),
        "DEVICE"
            | "TEMP"
            | "PARAM"
            | "MODEL"
            | "FREQ"
            | "STP"
            | "LIN"
            | "DEC"
            | "OCT"
            | "LIST"
            | "SWEEP"
            | "NUMCORES"
    )
}

fn take<'a>(fields: &mut &'a [String]) -> Result<&'a str> {
    let (value, rest) = fields.split_first().context("missing sweep operand")?;
    *fields = rest;
    Ok(value)
}

fn assigned_value<'a>(fields: &mut &'a [String]) -> Result<&'a str> {
    if fields.first().is_some_and(|field| field == "=") {
        *fields = &fields[1..];
    }
    take(fields)
}

fn set_once<T>(slot: &mut Option<T>, value: T, description: &str) -> Result<()> {
    ensure!(slot.is_none(), "duplicate sweep {description}");
    *slot = Some(value);
    Ok(())
}

fn grouped_values(group: &str) -> Result<Vec<String>> {
    let inside = group
        .strip_prefix('(')
        .and_then(|value| value.strip_suffix(')'))
        .context("assigned sweep grid requires a parenthesized value list")?;
    Ok(statement_token_spans(inside)?
        .into_iter()
        .map(|span| inside[span].to_owned())
        .collect())
}

fn grid_fields(mode: &str, fields: &mut &[String]) -> Result<Vec<String>> {
    if fields.first().is_some_and(|field| field == "=") {
        return grouped_values(assigned_value(fields)?);
    }
    // A group is also legal without an equals sign. In positional syntax an
    // individual parenthesized scalar can be a coordinate, so only unpack a
    // multi-operand group when its arity actually matches the grid.
    if let Some(first) = fields.first()
        && first.starts_with('(')
        && let Ok(group) = grouped_values(first)
        && (mode == "LIST" || group.len() == 3)
    {
        *fields = &fields[1..];
        return Ok(group);
    }
    let count = if mode == "LIST" {
        fields
            .iter()
            .position(|field| keyword(field))
            .unwrap_or(fields.len())
    } else {
        3
    };
    let (values, rest) = fields
        .split_at_checked(count)
        .context("incomplete sweep grid")?;
    *fields = rest;
    Ok(values.to_vec())
}

fn grid(mode: &str, fields: &mut &[String]) -> Result<CoordinateLaw> {
    let values = grid_fields(mode, fields)?;
    if mode == "LIST" {
        ensure!(!values.is_empty(), "empty sweep LIST");
        return Ok(CoordinateLaw::List { values });
    }
    let [first, second, third] = values
        .try_into()
        .map_err(|_| anyhow::anyhow!("sweep grid needs exactly three operands"))?;
    Ok(match mode {
        "STP" => CoordinateLaw::Linear {
            start: first,
            stop: second,
            increment: third,
        },
        "LIN" => CoordinateLaw::LinearCount {
            count: first,
            start: second,
            stop: third,
        },
        "DEC" | "OCT" => CoordinateLaw::Logarithmic {
            direction: super::sweep::LogDirection::Either,
            points_per_interval: first,
            start: second,
            stop: third,
            interval: if mode == "DEC" {
                LogInterval::Decade
            } else {
                LogInterval::Octave
            },
        },
        _ => anyhow::bail!("unqualified sweep grid {mode}"),
    })
}

#[derive(Default)]
struct AxisParts {
    device: Option<String>,
    model: Option<String>,
    parameter: Option<String>,
    special: Option<SweepTarget>,
    law: Option<CoordinateLaw>,
}

impl AxisParts {
    fn field(&mut self, field: &str, fields: &mut &[String]) -> Result<()> {
        match field.to_ascii_uppercase().as_str() {
            "DEVICE" => set_once(&mut self.device, name(assigned_value(fields)?)?, "device")?,
            "PARAM" => set_once(
                &mut self.parameter,
                name(assigned_value(fields)?)?,
                "parameter",
            )?,
            "MODEL" => {
                set_once(&mut self.model, name(assigned_value(fields)?)?, "model")?;
                if fields.first().is_some_and(|field| !keyword(field)) {
                    set_once(&mut self.parameter, name(take(fields)?)?, "model parameter")?;
                }
            }
            "TEMP" => set_once(&mut self.special, SweepTarget::Temperature, "target")?,
            "FREQ" => set_once(&mut self.special, SweepTarget::Frequency, "target")?,
            mode @ ("STP" | "LIN" | "DEC" | "OCT" | "LIST") => {
                set_once(&mut self.law, grid(mode, fields)?, "grid")?;
            }
            _ => anyhow::bail!("unqualified SIMetrix sweep field {field}"),
        }
        Ok(())
    }

    fn finish(self, execution: SweepExecution) -> Result<SweepAxis> {
        let target = match (self.device, self.model, self.parameter, self.special) {
            (Some(name), None, None, None) => SweepTarget::Device { name },
            (None, None, Some(name), None) => SweepTarget::Parameter { name },
            (None, Some(model), Some(parameter), None) => SweepTarget::ModelParameter {
                family: None,
                model,
                parameter,
            },
            (None, None, None, Some(target)) => target,
            _ => anyhow::bail!("missing or conflicting SIMetrix sweep target"),
        };
        Ok(SweepAxis {
            target,
            law: self.law.context("missing SIMetrix sweep grid")?,
            execution,
            order: CoordinateOrder::Declared,
            cardinality: Cardinality::Nonempty,
        })
    }
}

fn clause_boundary(field: &str) -> bool {
    field.eq_ignore_ascii_case("SWEEP") || field.eq_ignore_ascii_case("NUMCORES")
}

fn axis_prefix(fields: &mut &[String], execution: SweepExecution) -> Result<SweepAxis> {
    let mut parts = AxisParts::default();
    while fields.first().is_some_and(|field| !clause_boundary(field)) {
        let field = take(fields)?;
        parts.field(field, fields)?;
    }
    parts.finish(execution)
}

fn axis(mut fields: &[String], execution: SweepExecution) -> Result<SweepAxis> {
    let axis = axis_prefix(&mut fields, execution)?;
    ensure!(fields.is_empty(), "unexpected trailing sweep clause");
    Ok(axis)
}

fn base_shape(analysis: AnalysisKind, args: &[String]) -> bool {
    let args = if analysis == AnalysisKind::Transient
        && args
            .last()
            .is_some_and(|field| field.eq_ignore_ascii_case("UIC"))
    {
        &args[..args.len() - 1]
    } else {
        args
    };
    super::native_analysis_shape(analysis, args)
}

/// Locate the first independent clause at a grammar boundary, not inside a
/// displayed source name or a target operand.
pub(super) fn sweep_start(analysis: AnalysisKind, args: &[String]) -> Option<usize> {
    if base_shape(analysis, args) {
        return None;
    }
    if analysis == AnalysisKind::DcSweep {
        for index in [8, 4] {
            if args
                .get(index)
                .is_some_and(|field| field.eq_ignore_ascii_case("SWEEP"))
                && base_shape(analysis, &args[..index])
            {
                return Some(index);
            }
        }
        let mut rest = args;
        if axis_prefix(&mut rest, SweepExecution::Continuation).is_ok()
            && rest
                .first()
                .is_some_and(|field| field.eq_ignore_ascii_case("SWEEP"))
        {
            return Some(args.len() - rest.len());
        }
        return None;
    }
    // Ordinary analysis requests have at most seven operands before SWEEP.
    args.iter().take(8).enumerate().find_map(|(index, field)| {
        (field.eq_ignore_ascii_case("SWEEP") && base_shape(analysis, &args[..index]))
            .then_some(index)
    })
}

pub(super) fn dc_axes(args: &[String]) -> Result<Vec<SweepAxis>> {
    if super::native_analysis_shape(AnalysisKind::DcSweep, args) {
        return args
            .as_chunks::<4>()
            .0
            .iter()
            .map(|[source, start, stop, increment]| {
                let target = if source.eq_ignore_ascii_case("TEMP") {
                    SweepTarget::Temperature
                } else {
                    // In SIMetrix the original displayed name need not begin
                    // with the eventual native device designator.
                    SweepTarget::Source {
                        name: name(source)?,
                    }
                };
                Ok(SweepAxis {
                    target,
                    law: CoordinateLaw::Linear {
                        start: start.clone(),
                        stop: stop.clone(),
                        increment: increment.clone(),
                    },
                    execution: SweepExecution::Continuation,
                    order: CoordinateOrder::Declared,
                    cardinality: Cardinality::Nonempty,
                })
            })
            .collect();
    }
    let axis = axis(args, SweepExecution::Continuation)?;
    ensure!(
        axis.target != SweepTarget::Frequency,
        "frequency is not a DC sweep target"
    );
    Ok(vec![axis])
}

pub(super) struct InlineSweeps {
    pub arguments: Vec<String>,
    pub axes: Vec<SweepAxis>,
    pub parallelism_hint: Option<u32>,
}

/// Keep Monte Carlo selection on its existing explicit realization path.
/// Other numerical sweeps use one typed axis per repeated SWEEP clause.
/// Unqualified snapshot, script, sensitivity and optimization commands
/// cannot be mistaken for numerical axes.
pub(super) fn inline_sweeps(
    analysis: AnalysisKind,
    args: &[String],
) -> Result<Option<InlineSweeps>> {
    let Some(first) = sweep_start(analysis, args) else {
        return Ok(None);
    };
    if args
        .get(first + 1)
        .is_some_and(|field| field.eq_ignore_ascii_case("MONTE"))
    {
        return Ok(None);
    }
    ensure!(
        matches!(
            analysis,
            AnalysisKind::DcSweep
                | AnalysisKind::Ac
                | AnalysisKind::Transient
                | AnalysisKind::Noise
                | AnalysisKind::TransferFunction
        ),
        "SIMetrix analysis does not support independent sweeps"
    );
    let mut rest = &args[first + 1..];
    let mut axes = Vec::new();
    let parallelism_hint = loop {
        let axis = axis_prefix(&mut rest, SweepExecution::Independent)?;
        ensure!(
            !matches!(analysis, AnalysisKind::DcSweep | AnalysisKind::Transient)
                || axis.target != SweepTarget::Frequency,
            "frequency is not an independent DC/transient sweep target"
        );
        axes.push(axis);
        if rest.is_empty() {
            break None;
        }
        let clause = take(&mut rest)?;
        if clause.eq_ignore_ascii_case("NUMCORES") {
            let count = assigned_value(&mut rest)?.parse::<u32>()?;
            ensure!(
                count > 0 && rest.is_empty(),
                "invalid trailing NUMCORES scheduling hint"
            );
            break Some(count);
        }
        ensure!(
            clause.eq_ignore_ascii_case("SWEEP"),
            "unexpected sweep clause"
        );
    };
    Ok(Some(InlineSweeps {
        arguments: args[..first].to_vec(),
        axes,
        parallelism_hint,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use model_normalize_ngspice::model_card::parse_spice_constant;
    use model_preprocess_command::WorkBudget;

    #[test]
    fn point_count_and_step_increment_are_different_laws() -> Result<()> {
        let args = super::super::arguments(
            ".tran 10m SWEEP PARAM=r LIN=(5 1k 3k) SWEEP MODEL model PARAM=bf LIST 40 80 NUMCORES=3",
        )?;
        let sweeps = inline_sweeps(AnalysisKind::Transient, &args)?.context("missing sweeps")?;
        assert_eq!(sweeps.arguments, ["10m"]);
        assert_eq!(sweeps.parallelism_hint, Some(3));
        assert_eq!(
            sweeps.axes[0].target,
            SweepTarget::Parameter { name: "r".into() }
        );
        assert_eq!(
            sweeps.axes[1].target,
            SweepTarget::ModelParameter {
                family: None,
                model: "model".into(),
                parameter: "bf".into()
            }
        );
        let budget = WorkBudget::default();
        let grid = sweeps.axes[0].bind(parse_spice_constant, &budget)?;
        assert_eq!(grid.len(), 5);
        assert_eq!(grid.value(0).map(f64::to_bits), Some(1000_f64.to_bits()));
        assert_eq!(grid.value(1).map(f64::to_bits), Some(1500_f64.to_bits()));
        assert_eq!(grid.value(4).map(f64::to_bits), Some(3000_f64.to_bits()));
        let args = super::super::arguments(".dc DEVICE=load STP 1k 3k 2k")?;
        let axes = dc_axes(&args)?;
        assert_eq!(axes[0].execution, SweepExecution::Continuation);
        assert_eq!(
            axes[0].target,
            SweepTarget::Device {
                name: "load".into()
            }
        );
        assert_eq!(axes[0].bind(parse_spice_constant, &budget)?.len(), 2);
        Ok(())
    }

    #[test]
    fn clause_keywords_are_legal_target_operands() -> Result<()> {
        let args = super::super::arguments(
            ".tran 1m SWEEP PARAM sweep STP 0 1 1 SWEEP DEVICE numcores LIST 1 2 NUMCORES=2",
        )?;
        let sweeps = inline_sweeps(AnalysisKind::Transient, &args)?.context("missing sweeps")?;
        assert_eq!(
            sweeps.axes[0].target,
            SweepTarget::Parameter {
                name: "sweep".into()
            }
        );
        assert_eq!(
            sweeps.axes[1].target,
            SweepTarget::Device {
                name: "numcores".into()
            }
        );
        assert_eq!(sweeps.parallelism_hint, Some(2));
        for text in [
            ".dc sweep 0 2 1",
            ".dc v1 0 1 1 sweep 0 2 1",
            ".dc PARAM sweep STP 0 2 1",
        ] {
            let args = super::super::arguments(text)?;
            assert!(inline_sweeps(AnalysisKind::DcSweep, &args)?.is_none());
            assert!(!dc_axes(&args)?.is_empty());
        }
        let args = super::super::arguments(".dc PARAM sweep STP 0 2 1 SWEEP TEMP LIST 20 40")?;
        let sweeps = inline_sweeps(AnalysisKind::DcSweep, &args)?.context("missing sweep")?;
        assert_eq!(
            dc_axes(&sweeps.arguments)?[0].target,
            SweepTarget::Parameter {
                name: "sweep".into()
            }
        );
        assert_eq!(sweeps.axes[0].target, SweepTarget::Temperature);
        Ok(())
    }

    #[test]
    fn unqualified_sweep_modes_are_not_nominally_substituted() -> Result<()> {
        for text in [
            ".tran 1m SWEEP PARAM p LIN 3 1 2 DEVICE V1",
            ".tran 1m SWEEP PARAM p LIN 3 1",
            ".tran 1m SWEEP SNAPSHOT STP 0 2 1",
            ".tran 1m SWEEP FREQ LIST 1k 2k",
            ".tran 1m SWEEP PARAM p STP 0 1 1 NUMCORES=0",
            ".tran 1m SWEEP PARAM p STP 0 1 1 SWEEP",
        ] {
            assert!(
                inline_sweeps(AnalysisKind::Transient, &super::super::arguments(text)?).is_err(),
                "{text}"
            );
        }
        let args = super::super::arguments(".tran 1m SWEEP MONTE 10")?;
        assert!(inline_sweeps(AnalysisKind::Transient, &args)?.is_none());
        Ok(())
    }
}
