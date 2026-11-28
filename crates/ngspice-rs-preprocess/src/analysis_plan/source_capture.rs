//! Direct source analysis extraction. The admitted graph supplies physical
//! source identity and effective dialect for each logical statement; this
//! pass owns only analysis semantics and line-preserving electrical edits.

use super::{
    AnalysisDialect, AnalysisPlan, Directive, DirectiveOrigin, Disposition, PlanKind, arguments,
    command, push_entry, source_title, tracked,
};
use crate::OriginLine;
use anyhow::{Context, Result, ensure};
use model_normalize_ngspice::{
    SourceStatement,
    linked_source::{LinkedSource, RootKind, SourceDialect},
    source_statements,
};
use model_preprocess_command::{Resource, WorkBudget};
use std::fmt::Write as _;

/// Location supplied by the admitted source occurrence graph.
#[derive(Debug)]
pub struct AnalysisLocation {
    /// Source occurrence, physical span, or generated artifact identity.
    pub origin: DirectiveOrigin,
    /// Effective source language at this statement.
    pub dialect: AnalysisDialect,
    /// Lexical electrical scope.
    pub scope: Vec<String>,
}

/// Source analysis plan and the corresponding line-preserving circuit.
#[derive(Debug)]
pub struct CapturedAnalysis {
    /// Ordered original analysis and observation intent.
    pub plan: AnalysisPlan,
    /// Electrical artifact with extracted declarations neutralized.
    /// Its physical line count and ordering are unchanged.
    pub electrical: Vec<u8>,
}

const fn dialect(source: SourceDialect) -> AnalysisDialect {
    match source {
        SourceDialect::Native => AnalysisDialect::Native,
        SourceDialect::Ltspice => AnalysisDialect::Ltspice,
        SourceDialect::Pspice => AnalysisDialect::Pspice,
        SourceDialect::Tina => AnalysisDialect::Tina,
        SourceDialect::Qspice => AnalysisDialect::Qspice,
        SourceDialect::Hspice => AnalysisDialect::Hspice,
        SourceDialect::Simetrix => AnalysisDialect::Simetrix,
    }
}

fn linked_location(
    linked: &LinkedSource,
    origins: &[OriginLine],
    first: usize,
    last: usize,
    budget: &WorkBudget,
) -> Result<AnalysisLocation> {
    let source_span = origins
        .get(
            first
                .checked_sub(1)
                .context("invalid analysis start line")?..last,
        )
        .context("analysis source span outside linked origin map")?;
    let start = source_span
        .first()
        .context("missing analysis start origin")?;
    let end = source_span.last().context("missing analysis end origin")?;
    let location = start.linked.context("missing analysis source occurrence")?;
    ensure!(
        source_span
            .iter()
            .all(|origin| origin.source == start.source && origin.linked == Some(location))
            && end.line >= start.line,
        "analysis declaration crosses source occurrence or scope: {}:{}",
        start.source.display(),
        start.line,
    );
    let mut scope = Vec::new();
    let mut current = Some(location.scope);
    while let Some(index) = current {
        budget.check_depth(scope.len())?;
        let definition = linked
            .scopes
            .get(index)
            .context("missing analysis lexical scope")?;
        if let Some(name) = &definition.name {
            budget.charge(Resource::GeneratedNodes, 1)?;
            budget.charge(Resource::EmittedBytes, name.len())?;
            scope.push(name.clone());
        }
        ensure!(
            definition.parent.is_none_or(|parent| parent < index),
            "invalid analysis scope ancestry"
        );
        current = definition.parent;
    }
    scope.reverse();
    let source_dialect = linked
        .visits
        .get(location.visit)
        .context("missing analysis dialect")?
        .dialect;
    budget.charge(Resource::EmittedBytes, start.source.as_os_str().len())?;
    Ok(AnalysisLocation {
        origin: DirectiveOrigin::SourceOccurrence {
            path: start.source.clone(),
            first_line: start.line,
            last_line: end.line,
            visit: location.visit,
            scope: location.scope,
        },
        dialect: dialect(source_dialect),
        scope,
    })
}

/// Extract analysis before source lowering, retaining the already-admitted
/// graph. Line-preserving edits keep the electrical origin map valid. The
/// root's file format does not determine the language of included resources.
pub(crate) fn capture_linked(
    linked: &mut LinkedSource,
    kind: RootKind,
    origins: &[OriginLine],
    budget: &WorkBudget,
) -> Result<Option<AnalysisPlan>> {
    if !linked
        .visits
        .iter()
        .any(|visit| visit.dialect == SourceDialect::Ltspice)
    {
        return Ok(None);
    }
    let root_dialect = dialect(
        linked
            .visits
            .first()
            .context("missing root analysis dialect")?
            .dialect,
    );
    let kind = if kind == RootKind::Deck {
        PlanKind::Deck
    } else {
        PlanKind::Library
    };
    let Some(captured) = capture(
        &linked.bytes,
        kind,
        root_dialect,
        |first, last| linked_location(linked, origins, first, last, budget),
        budget,
    )?
    else {
        return Ok(None);
    };
    linked.bytes = captured.electrical;
    Ok(Some(captured.plan))
}

struct Candidate {
    first: usize,
    last: usize,
    location: AnalysisLocation,
    text: String,
    conditional: bool,
}

impl Candidate {
    fn activates_plan(&self) -> Result<bool> {
        if self.location.dialect != AnalysisDialect::Ltspice {
            return Ok(false);
        }
        let name = command(&self.text);
        Ok(match name.as_str() {
            ".OPTIONS" | ".OPTION" | ".OPT" | ".IC" | ".INIT" | ".NODESET" | ".CONTROL" => false,
            ".TEMP" => arguments(&self.text)?.len() > 1,
            _ => true,
        })
    }
}

fn collect_candidates(
    statements: Vec<SourceStatement>,
    physical: &[&[u8]],
    mut locate: impl FnMut(usize, usize) -> Result<AnalysisLocation>,
    budget: &WorkBudget,
) -> Result<Vec<Candidate>> {
    let mut candidates = Vec::new();
    let mut conditional_depth = 0usize;
    let mut control = None;
    for statement in statements {
        // Device statements need not be UTF-8 just to discover whether they
        // are analysis declarations. Only directive text is interpreted.
        if statement.text.first() != Some(&b'.') {
            continue;
        }
        let text = std::str::from_utf8(&statement.text)?;
        let name = command(text);
        if let Some(first) = control {
            if name == ".ENDC" {
                let last = statement.last_line;
                let lines = physical
                    .get(first - 1..last)
                    .context("invalid control source span")?;
                let size = lines.iter().map(|line| line.len()).sum();
                budget.charge(Resource::EmittedBytes, size)?;
                let text = String::from_utf8(lines.concat())?;
                candidates.push(Candidate {
                    first,
                    last,
                    location: locate(first, last)?,
                    text,
                    conditional: conditional_depth != 0,
                });
                control = None;
            }
            continue;
        }
        match name.as_str() {
            ".IF" => {
                conditional_depth = conditional_depth
                    .checked_add(1)
                    .context("analysis conditional depth overflow")?;
                budget.check_depth(conditional_depth)?;
            }
            ".ENDIF" => {
                conditional_depth = conditional_depth
                    .checked_sub(1)
                    .context("unmatched analysis .ENDIF")?;
            }
            ".CONTROL" => control = Some(statement.first_line),
            _ if tracked(&name) => {
                budget.charge(Resource::GeneratedNodes, 1)?;
                budget.charge(Resource::EmittedBytes, text.len())?;
                candidates.push(Candidate {
                    first: statement.first_line,
                    last: statement.last_line,
                    location: locate(statement.first_line, statement.last_line)?,
                    text: text.to_owned(),
                    conditional: conditional_depth != 0,
                });
            }
            _ => {}
        }
    }
    ensure!(control.is_none(), "unterminated source control block");
    ensure!(conditional_depth == 0, "unterminated source conditional");
    Ok(candidates)
}

struct Rewrite {
    first: usize,
    last: usize,
    native_options: String,
}

fn extract_entry(
    plan: &mut AnalysisPlan,
    candidate: Candidate,
    budget: &WorkBudget,
) -> Result<Option<Rewrite>> {
    let Candidate {
        first,
        last,
        location,
        text,
        conditional,
    } = candidate;
    let name = command(&text);
    // Native circuit directives are already in the solver's source language.
    // They remain in the electrical artifact rather than being duplicated.
    if location.dialect != AnalysisDialect::Ltspice
        && matches!(
            name.as_str(),
            ".OPTIONS" | ".OPTION" | ".OPT" | ".TEMP" | ".IC" | ".NODESET" | ".INIT"
        )
    {
        return Ok(None);
    }
    ensure!(
        matches!(
            location.dialect,
            AnalysisDialect::Native | AnalysisDialect::Ltspice
        ),
        "mixed analysis binding for {:?} is not qualified at {:?}",
        location.dialect,
        location.origin,
    );
    let library = plan.kind == PlanKind::Library;
    push_entry(
        plan,
        location.origin,
        &location.scope,
        &text,
        library || conditional,
        budget,
        location.dialect,
    )?;
    let entry = plan
        .entries
        .last_mut()
        .context("missing captured analysis entry")?;
    if conditional {
        // A pre-binding capture must not choose a candidate-dependent branch.
        // The plan is explicitly non-executable until topology binding.
        entry.disposition = Disposition::Unsupported;
    }
    if let Directive::Transient { intent } = &entry.directive {
        // Validate known timing now; the occurrence-dependent fields are
        // evaluated by the candidate binder.
        let _ = intent
            .try_bind_closed(budget)
            .with_context(|| format!("transient timing at {:?}", entry.origin))?;
    }
    let mut native_options = String::new();
    match &entry.directive {
        Directive::Circuit if entry.disposition != Disposition::Unsupported => return Ok(None),
        Directive::Options { settings } if entry.disposition == Disposition::Circuit => {
            for setting in settings
                .iter()
                .filter(|setting| setting.disposition == Disposition::Circuit)
            {
                if native_options.is_empty() {
                    native_options.push_str(".options");
                }
                write!(native_options, " {}", setting.name)?;
                if let Some(value) = &setting.value {
                    write!(native_options, "={value}")?;
                }
            }
        }
        _ => {}
    }
    Ok(Some(Rewrite {
        first,
        last,
        native_options,
    }))
}

fn emit(physical: &[&[u8]], rewrites: Vec<Rewrite>, budget: &WorkBudget) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut rewrites = rewrites.into_iter().peekable();
    for (index, line) in physical.iter().enumerate() {
        let number = index + 1;
        if let Some(rewrite) = rewrites.peek()
            && (rewrite.first..=rewrite.last).contains(&number)
        {
            if number == rewrite.first && !rewrite.native_options.is_empty() {
                let ending = if line.ends_with(b"\r\n") {
                    b"\r\n".as_slice()
                } else if line.ends_with(b"\n") {
                    b"\n".as_slice()
                } else {
                    b"".as_slice()
                };
                budget.charge(
                    Resource::EmittedBytes,
                    rewrite.native_options.len().saturating_add(ending.len()),
                )?;
                output.extend_from_slice(rewrite.native_options.as_bytes());
                output.extend_from_slice(ending);
            } else {
                budget.charge(Resource::EmittedBytes, line.len().saturating_add(2))?;
                output.extend_from_slice(b"* ");
                output.extend_from_slice(line);
            }
            if number == rewrite.last {
                rewrites.next();
            }
        } else {
            budget.charge(Resource::EmittedBytes, line.len())?;
            output.extend_from_slice(line);
        }
    }
    ensure!(
        rewrites.next().is_none(),
        "analysis source edit outside physical artifact"
    );
    Ok(output)
}

/// Capture source-specific analysis intent from a directly linked graph.
/// The location resolver is called on physical statement spans, never on a
/// reconstructed filename or guessed root dialect. A pure native circuit
/// without source-specific analysis keeps its ordinary native execution path.
///
/// # Errors
/// Invalid or unsupported source declaration, inconsistent source mapping,
/// malformed scope, cancellation or exhausted work budget.
pub fn capture(
    input: &[u8],
    kind: PlanKind,
    root_dialect: AnalysisDialect,
    locate: impl FnMut(usize, usize) -> Result<AnalysisLocation>,
    budget: &WorkBudget,
) -> Result<Option<CapturedAnalysis>> {
    let statements = source_statements(
        input,
        if kind == PlanKind::Deck {
            RootKind::Deck
        } else {
            RootKind::Include
        },
        budget,
    )?;
    let physical_lines = input.split_inclusive(|byte| *byte == b'\n');
    budget.charge(Resource::GeneratedNodes, physical_lines.clone().count())?;
    let physical = physical_lines.collect::<Vec<_>>();
    let candidates = collect_candidates(statements, &physical, locate, budget)?;
    let mut required = false;
    for candidate in &candidates {
        required |= candidate.activates_plan()?;
    }
    if !required {
        return Ok(None);
    }
    let mut plan = AnalysisPlan::new(
        root_dialect,
        kind,
        (kind == PlanKind::Deck).then(|| source_title(input)),
    );
    let mut rewrites = Vec::new();
    for candidate in candidates {
        if let Some(rewrite) = extract_entry(&mut plan, candidate, budget)? {
            rewrites.push(rewrite);
        }
    }
    let electrical = emit(&physical, rewrites, budget)?;
    Ok(Some(CapturedAnalysis { plan, electrical }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis_plan::{AnalysisKind, sweep::SweepExecution};

    fn location(
        first: usize,
        last: usize,
        dialect: AnalysisDialect,
        visit: usize,
    ) -> AnalysisLocation {
        AnalysisLocation {
            origin: DirectiveOrigin::SourceOccurrence {
                path: format!("source-{visit}.cir").into(),
                first_line: first,
                last_line: last,
                visit,
                scope: 0,
            },
            dialect,
            scope: Vec::new(),
        }
    }

    #[test]
    fn mixed_capture_preserves_title_physical_lines_and_analysis_order() -> Result<()> {
        let bytes = b".step not-a-directive-in-title\r\n.param duration=1m\r\n.step param duration list 5m\r\n+ 1m 2m\r\n.options RELTOL=1e-5 maxstep=1u\r\n.tran 0 {duration} startup\r\n.op\r\n.meas tran vmax MAX V(out)\r\nR1 out 0 1k\r\n.end\r\n";
        let result = capture(
            bytes,
            PlanKind::Deck,
            AnalysisDialect::Native,
            |first, last| {
                let dialect = if first == 7 {
                    AnalysisDialect::Native
                } else {
                    AnalysisDialect::Ltspice
                };
                Ok(location(
                    first,
                    last,
                    dialect,
                    usize::from(dialect == AnalysisDialect::Ltspice),
                ))
            },
            &WorkBudget::default(),
        )?
        .context("missing analysis plan")?;
        assert_eq!(
            result.plan.title.as_deref(),
            Some(".step not-a-directive-in-title")
        );
        assert_eq!(
            result
                .plan
                .entries
                .iter()
                .map(|entry| entry.command.as_str())
                .collect::<Vec<_>>(),
            [".STEP", ".OPTIONS", ".TRAN", ".OP", ".MEAS"]
        );
        assert!(matches!(
            result.plan.entries[0].origin,
            DirectiveOrigin::SourceOccurrence {
                first_line: 3,
                last_line: 4,
                visit: 1,
                ..
            }
        ));
        assert_eq!(result.plan.entries[3].dialect, AnalysisDialect::Native);
        assert!(matches!(
            result.plan.entries[3].directive,
            Directive::Analysis {
                analysis: AnalysisKind::OperatingPoint,
                ..
            }
        ));
        assert!(result.plan.requires_binding());
        let output = std::str::from_utf8(&result.electrical)?;
        assert_eq!(
            output.lines().count(),
            std::str::from_utf8(bytes)?.lines().count()
        );
        assert!(output.contains(".options RELTOL=1e-5\r\n"));
        assert!(output.contains("* + 1m 2m\r\n"));
        assert!(output.contains("* .tran 0 {duration} startup\r\n"));
        assert!(output.contains("R1 out 0 1k\r\n"));
        Ok(())
    }

    #[test]
    fn continuation_axes_do_not_multiply_independent_candidate_count() -> Result<()> {
        let bytes = b"title\n.step temp list 0 50\n.dc V1 1 0 -.5 V2 0 2 1\n.end\n";
        let budget = WorkBudget::default();
        let result = capture(
            bytes,
            PlanKind::Deck,
            AnalysisDialect::Ltspice,
            |first, last| Ok(location(first, last, AnalysisDialect::Ltspice, 0)),
            &budget,
        )?
        .context("missing plan")?;
        let Directive::DcSweep { axes } = &result.plan.entries[1].directive else {
            anyhow::bail!("missing DC axes")
        };
        assert_eq!(axes.len(), 2);
        assert!(
            axes.iter()
                .all(|axis| axis.execution == SweepExecution::Continuation)
        );
        let independent = result.plan.bind_independent_sweeps(
            |_, value| model_normalize_ngspice::model_card::parse_spice_constant(value),
            &budget,
        )?;
        assert_eq!(independent.count(), 2);
        Ok(())
    }

    #[test]
    fn source_conditions_and_control_are_not_silently_executed() -> Result<()> {
        let bytes = b"title\n.step param x list 1 2\n.if {x>1}\n.tran 1m\n.endif\n.control\nop\n.endc\n.end\n";
        let result = capture(
            bytes,
            PlanKind::Deck,
            AnalysisDialect::Ltspice,
            |first, last| Ok(location(first, last, AnalysisDialect::Ltspice, 0)),
            &WorkBudget::default(),
        )?
        .context("missing plan")?;
        assert_eq!(result.plan.entries[1].disposition, Disposition::Unsupported);
        assert_eq!(result.plan.entries[2].command, ".CONTROL");
        assert!(result.plan.validate_supported().is_err());
        assert!(std::str::from_utf8(&result.electrical)?.contains("* op\n"));
        Ok(())
    }
}
