//! Qualified native analysis forms from HSPICE E-2010.12.
use crate::OriginLine;
use crate::analysis_plan::{
    AnalysisDialect, AnalysisPlan, Directive, DirectiveOrigin, Disposition, PlanEntry, PlanKind,
    sweep::{
        Cardinality, CoordinateLaw, CoordinateOrder, LogDirection, LogInterval, SweepAxis,
        SweepExecution, SweepTarget,
    },
    timing,
};
use crate::analysis_plan::{
    AnalysisKind, analysis_kind, arguments, command, native_analysis_shape,
};
use anyhow::{Context, Result, ensure};
use model_normalize_ngspice::model_card::parse_parameter_fields;
use model_preprocess_command::{Resource, WorkBudget};
use model_preprocess_simplis::source_dialect::{ScopedStatement, SourceDialect};
use std::{collections::BTreeMap, fmt::Write as _};

pub fn lower(text: &str) -> Result<String> {
    let name = command(text);
    let kind = analysis_kind(&name);
    let args = arguments(text)?;
    if kind == AnalysisKind::Transient {
        let (timing, _) = split_sweeps(&args)?;
        timing::hspice(timing)?;
        return Ok(text.to_owned());
    }
    let (base, _) = if kind == AnalysisKind::Ac {
        split_sweeps(&args)?
    } else {
        (args.as_slice(), Vec::new())
    };
    ensure!(
        native_analysis_shape(kind, base),
        "unqualified HSPICE analysis {text}"
    );
    if kind == AnalysisKind::DcSweep {
        for sweep in args.as_chunks::<4>().0 {
            let source = sweep[0]
                .as_bytes()
                .first()
                .context("empty DC sweep target")?;
            ensure!(
                matches!(source.to_ascii_uppercase(), b'V' | b'I'),
                "HSPICE parameter/temperature sweep requires explicit analysis binding"
            );
        }
    }
    Ok(text.to_owned())
}

/// Ensure the requested sweep target is actually an independent source,
/// rather than a parameter whose name happens to begin with V or I.
pub fn validate_dc_sources(rows: &[(String, usize)]) -> Result<()> {
    let mut sources = std::collections::BTreeSet::new();
    let mut depth = 0_usize;
    for (row, _) in rows.iter().skip(1) {
        let name = command(row);
        match name.as_str() {
            ".SUBCKT" => depth += 1,
            ".ENDS" => depth = depth.checked_sub(1).context("unmatched .ENDS")?,
            _ if depth == 0 && name.starts_with(['V', 'I']) => {
                sources.insert(name);
            }
            _ => (),
        }
    }
    for (row, _) in rows.iter().skip(1) {
        if command(row) == ".DC" {
            for sweep in arguments(row)?.as_chunks::<4>().0 {
                ensure!(
                    sources.contains(&sweep[0].to_ascii_uppercase()),
                    "HSPICE DC target {} is not a root independent source; parameter sweeps require explicit analysis binding",
                    sweep[0]
                );
            }
        }
    }
    Ok(())
}

/// HSPICE SWEEP repeats complete analyses; it does not extend the
/// continuation axes of a native .DC request.
fn split_sweeps(args: &[String]) -> Result<(&[String], Vec<SweepAxis>)> {
    let Some(first) = args
        .iter()
        .position(|field| field.eq_ignore_ascii_case("SWEEP"))
    else {
        return Ok((args, Vec::new()));
    };
    let mut axes = Vec::new();
    let mut rest = &args[first + 1..];
    loop {
        let end = rest
            .iter()
            .position(|field| field.eq_ignore_ascii_case("SWEEP"))
            .unwrap_or(rest.len());
        let [name, fields @ ..] = &rest[..end] else {
            anyhow::bail!("missing HSPICE SWEEP target");
        };
        let target = if name.eq_ignore_ascii_case("TEMP") {
            SweepTarget::Temperature
        } else {
            ensure!(
                !name.is_empty()
                    && name.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
                    && name
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'),
                "invalid HSPICE swept parameter {name}"
            );
            SweepTarget::Parameter { name: name.clone() }
        };
        let law = match fields {
            [mode, count, start, stop] if mode.eq_ignore_ascii_case("LIN") => {
                CoordinateLaw::LinearCount {
                    start: start.clone(),
                    stop: stop.clone(),
                    count: count.clone(),
                }
            }
            [mode, count, start, stop]
                if mode.eq_ignore_ascii_case("DEC") || mode.eq_ignore_ascii_case("OCT") =>
            {
                CoordinateLaw::Logarithmic {
                    start: start.clone(),
                    stop: stop.clone(),
                    points_per_interval: count.clone(),
                    interval: if mode.eq_ignore_ascii_case("DEC") {
                        LogInterval::Decade
                    } else {
                        LogInterval::Octave
                    },
                    direction: LogDirection::Increasing,
                }
            }
            [start, stop, increment] => CoordinateLaw::Linear {
                start: start.clone(),
                stop: stop.clone(),
                increment: increment.clone(),
            },
            [start, eq1, from, stop, eq2, to, step, eq3, increment]
                if start.eq_ignore_ascii_case("START")
                    && stop.eq_ignore_ascii_case("STOP")
                    && step.eq_ignore_ascii_case("STEP")
                    && eq1 == "="
                    && eq2 == "="
                    && eq3 == "=" =>
            {
                CoordinateLaw::Linear {
                    start: from.clone(),
                    stop: to.clone(),
                    increment: increment.clone(),
                }
            }
            _ => anyhow::bail!("unsupported HSPICE SWEEP coordinate syntax"),
        };
        axes.push(SweepAxis {
            target,
            law,
            execution: SweepExecution::Independent,
            order: CoordinateOrder::Declared,
            cardinality: Cardinality::Nonempty,
        });
        if end == rest.len() {
            break;
        }
        rest = &rest[end + 1..];
    }
    Ok((&args[..first], axes))
}

/// Keep structural keywords and sweep targets outside scalar normalization.
pub(super) fn structural_analysis_fields(args: &[&str]) -> Vec<bool> {
    let mut target = false;
    let mut after_sweep = false;
    args.iter()
        .enumerate()
        .map(|(index, field)| {
            let upper = field.to_ascii_uppercase();
            let structural = target
                || (index == 0 && matches!(upper.as_str(), "LIN" | "DEC" | "OCT"))
                || matches!(
                    upper.as_str(),
                    "UIC" | "SWEEP" | "START" | "STOP" | "STEP" | "="
                )
                || (after_sweep && matches!(upper.as_str(), "LIN" | "DEC" | "OCT"));
            target = upper == "SWEEP";
            after_sweep |= target;
            structural
        })
        .collect()
}

type OriginalStatements<'a> = BTreeMap<(std::path::PathBuf, usize, usize), &'a ScopedStatement>;

pub(super) fn original_statements<'a>(
    statements: &'a [ScopedStatement],
    origins: &[OriginLine],
    budget: &WorkBudget,
) -> Result<OriginalStatements<'a>> {
    let mut original = BTreeMap::new();
    for (statement, origin) in statements.iter().zip(origins) {
        budget.charge(Resource::ScannedBytes, statement.text.len())?;
        if matches!(
            command(&statement.text).as_str(),
            ".OP" | ".DC" | ".AC" | ".TRAN"
        ) {
            budget.charge(Resource::GeneratedNodes, 1)?;
            budget.charge(Resource::EmittedBytes, origin.source.as_os_str().len())?;
            original.insert(
                (origin.source.clone(), origin.line, origin.last_line),
                statement,
            );
        }
    }
    Ok(original)
}

/// Extract analyses after lexical binding, retaining original physical spans.
/// The artifact has no embedded executable analyses or DELMAX option.
pub(super) fn extract(
    bytes: &[u8],
    origins: &[OriginLine],
    original: &OriginalStatements<'_>,
    title: &str,
    limit: usize,
    budget: &WorkBudget,
) -> Result<(Vec<u8>, Option<AnalysisPlan>)> {
    let text = std::str::from_utf8(bytes)?;
    ensure!(
        text.lines().count() == origins.len(),
        "inconsistent HSPICE analysis source map"
    );
    let mut plan = AnalysisPlan::new(
        AnalysisDialect::Hspice,
        PlanKind::Deck,
        Some(title.to_owned()),
    );
    let mut electrical = String::new();
    let mut delmax = None;
    for (index, (line, origin)) in text.lines().zip(origins).enumerate() {
        budget.charge(Resource::ScannedBytes, line.len())?;
        let name = command(line);
        if index != 0 && matches!(name.as_str(), ".OP" | ".DC" | ".AC" | ".TRAN") {
            let source = original
                .get(&(origin.source.clone(), origin.line, origin.last_line))
                .context("missing original HSPICE analysis declaration")?;
            ensure!(
                source.context.language == SourceDialect::Hspice,
                "analysis in an included non-HSPICE dialect requires explicit binding"
            );
            let args = arguments(line)?;
            let (directive, independent_axes) = if name == ".TRAN" {
                let (base, axes) = split_sweeps(&args)?;
                (
                    Directive::ScheduledTransient {
                        intent: timing::hspice(base)?,
                    },
                    axes,
                )
            } else {
                let (base, axes) = split_sweeps(&args)?;
                (
                    Directive::Analysis {
                        analysis: analysis_kind(&name),
                        arguments: base.to_vec(),
                        uic: false,
                    },
                    axes,
                )
            };
            budget.charge(Resource::GeneratedNodes, 1 + independent_axes.len())?;
            budget.charge(
                Resource::EmittedBytes,
                source
                    .text
                    .len()
                    .saturating_add(line.len())
                    .saturating_add(origin.source.as_os_str().len()),
            )?;
            let disposition = if name == ".TRAN" || !independent_axes.is_empty() {
                Disposition::RequiresBinding
            } else {
                Disposition::NativeRequest
            };
            plan.entries.push(PlanEntry {
                origin: DirectiveOrigin::Source {
                    path: origin.source.clone(),
                    first_line: origin.line,
                    last_line: origin.last_line,
                },
                dialect: AnalysisDialect::Hspice,
                scope: Vec::new(),
                command: name,
                statement: source.text.clone(),
                directive,
                independent_axes,
                parallelism_hint: None,
                disposition,
            });
            writeln!(electrical, "* {line}")?;
        } else if index != 0 && matches!(name.as_str(), ".OPTIONS" | ".OPTION" | ".OPT") {
            let tail = line
                .split_once(char::is_whitespace)
                .map_or("", |(_, tail)| tail);
            let mut retained = String::from(".options");
            for field in parse_parameter_fields(tail, &[])? {
                if field.name.eq_ignore_ascii_case("DELMAX") {
                    delmax = Some(timing::SymbolicCeiling {
                        value: field.expression.context("missing DELMAX value")?,
                        origin: DirectiveOrigin::Source {
                            path: origin.source.clone(),
                            first_line: origin.line,
                            last_line: origin.last_line,
                        },
                    });
                } else {
                    write!(retained, " {}", field.name)?;
                    if let Some(value) = field.expression {
                        write!(retained, "={value}")?;
                    }
                }
            }
            if retained == ".options" {
                writeln!(electrical, "* {line}")?;
            } else {
                writeln!(electrical, "{retained}")?;
            }
        } else {
            writeln!(electrical, "{line}")?;
        }
    }
    for entry in &mut plan.entries {
        if let Directive::ScheduledTransient { intent } = &mut entry.directive {
            intent.delmax.clone_from(&delmax);
            intent.try_bind_closed(budget)?;
        }
    }
    ensure!(
        electrical.len() <= limit,
        "standalone SPICE output byte budget exceeded"
    );
    budget.charge(Resource::EmittedBytes, electrical.len())?;
    Ok((
        electrical.into_bytes(),
        (!plan.entries.is_empty()).then_some(plan),
    ))
}
