//! Shared source analysis artifact and bounded numerical coordinate binding.
//! Source translation and native solver execution are independent clients.
pub mod measurement;
mod pspice;
pub mod sweep;
pub mod timing;

use anyhow::{Context, Result, ensure};
use measurement::MeasurementIntent;
use model_normalize_ngspice::ltspice::transient::TransientIntent;
use model_preprocess_command::{Resource, WorkBudget};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use sweep::{BoundAxis, SweepAxis, SweepExecution};

/// Analysis semantics (not the syntax of individual model expressions).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisDialect {
    /// Native ngspice analysis semantics.
    Native,
    /// `LTspice` analysis and independent parameter stepping.
    Ltspice,
    /// `PSpice` source analysis declarations.
    Pspice,
    /// TINA source analysis declarations.
    Tina,
    /// QSPICE source analysis declarations.
    Qspice,
    /// HSPICE source analysis declarations.
    Hspice,
    /// Ordinary SPICE analyses with `SIMetrix` extensions.
    Simetrix,
    /// SIMPLIS transient and periodic analyses.
    Simplis,
    /// Expanded source without an explicit original analysis dialect.
    Expanded,
}

/// Declaration location, without invented source maps for generated text.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DirectiveOrigin {
    /// Exact admitted physical source span.
    Source {
        /// Admitted path.
        path: PathBuf,
        /// First physical line, one-based.
        first_line: usize,
        /// Last physical line, inclusive.
        last_line: usize,
    },
    /// Exact declaration in a directly admitted source occurrence.
    SourceOccurrence {
        /// Original resource path.
        path: PathBuf,
        /// First physical line, one-based.
        first_line: usize,
        /// Last physical line, inclusive.
        last_line: usize,
        /// Admitted source visit index.
        visit: usize,
        /// Electrical lexical scope index.
        scope: usize,
    },
    /// Location in a linked artifact before primitive lowering.
    Generated {
        /// Artifact name.
        artifact: String,
        /// First physical line, one-based.
        first_line: usize,
        /// Last physical line, inclusive.
        last_line: usize,
    },
}

/// How a consumer must handle a declaration.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    /// Equivalent native analysis request; not embedded in the circuit.
    NativeRequest,
    /// Setting materialized in the artifact, including qualified output options.
    Circuit,
    /// Already applied by source model selection; never emitted as a native option.
    ModelSelection,
    /// Implemented by the initialization policy.
    Initialization,
    /// Requires explicit analysis/output binding.
    RequiresBinding,
    /// Unsupported by the current execution contract.
    Unsupported,
    /// Non-electrical UI or diagnostic metadata.
    Metadata,
}

/// Analysis type independent of dialect-specific arguments.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisKind {
    /// DC operating point.
    OperatingPoint,
    /// DC sweep.
    DcSweep,
    /// Small-signal frequency analysis.
    Ac,
    /// Time-domain transient.
    Transient,
    /// Periodic operating point.
    Pop,
    /// Noise.
    Noise,
    /// Transfer function.
    TransferFunction,
    /// Other analysis, with its original command retained.
    Other,
}

/// Individual option, retaining repeats and order.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OptionSetting {
    /// Case-normalized name.
    pub name: String,
    /// Symbolic/numeric argument, absent for a flag.
    pub value: Option<String>,
    /// Handling policy.
    pub disposition: Disposition,
}

/// Typed declaration.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Directive {
    /// Analysis request.
    Analysis {
        /// Analysis type.
        analysis: AnalysisKind,
        /// Canonical native arguments for native requests; otherwise source operands.
        arguments: Vec<String>,
        /// Skip transient bias; native UIC or PSpice SKIPBP, removed from arguments.
        uic: bool,
    },
    /// Independent analysis repetitions or continuation coordinates.
    Sweep {
        /// Typed, unresolved axis with source coordinate ordering.
        axis: SweepAxis,
    },
    /// Ordered continuation axes of one DC analysis.
    DcSweep {
        /// Inner axis first; do not treat these as independent STEP candidates.
        axes: Vec<SweepAxis>,
    },
    /// Source-specific transient timing and execution policy.
    Transient {
        /// Timing fields and initialization/termination modes.
        intent: TransientIntent,
    },
    /// Segmented transient timing with separate integration policy.
    ScheduledTransient {
        /// Source timing and bias-selection policy.
        intent: timing::ScheduleIntent,
    },
    /// Ordered option settings.
    Options {
        /// Individual settings.
        settings: Vec<OptionSetting>,
    },
    /// Source-specific waveform and previous-result measurement.
    Measurement {
        /// Ordered measurement operation and its source expression.
        intent: MeasurementIntent,
    },
    /// Output selection, measurement, or node mapping.
    Observation {
        /// Source coordinates/expressions, before consumer binding.
        arguments: Vec<String>,
    },
    /// Initialization or temperature directive.
    Circuit,
    /// UI metadata.
    Metadata,
    /// Unsupported imperative or dialect-specific behavior.
    Unsupported,
}

/// One active declaration.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PlanEntry {
    /// Source or generated location.
    pub origin: DirectiveOrigin,
    /// Effective analysis language at this declaration, independent of the root.
    pub dialect: AnalysisDialect,
    /// Lexical definition scope.
    pub scope: Vec<String>,
    /// Uppercase directive including leading period.
    pub command: String,
    /// Logical statement after active preprocessing.
    pub statement: String,
    /// Typed declaration.
    pub directive: Directive,
    /// Analysis-local independent axes, in innermost-first order. Deck-wide
    /// STEP declarations are separate entries and do not belong to this list.
    pub independent_axes: Vec<SweepAxis>,
    /// Original scheduling hint, not a physical or numerical policy.
    pub parallelism_hint: Option<u32>,
    /// Execution policy.
    pub disposition: Disposition,
}

impl PlanEntry {
    /// Whether this entry requests an analysis rather than a setting or observation.
    #[must_use = "independent axes attach only to an actual analysis"]
    pub const fn is_analysis(&self) -> bool {
        matches!(
            self.directive,
            Directive::Analysis { .. }
                | Directive::Transient { .. }
                | Directive::ScheduledTransient { .. }
                | Directive::DcSweep { .. }
        )
    }
}

/// Whether the plan describes a complete deck or an include-only artifact.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanKind {
    /// Complete translated circuit and ordered source analysis requests.
    Deck,
    /// Include-only declarations; not an independently executable analysis.
    Library,
}

/// Ordered intent for a translated dialect deck. Bind this plan and the
/// initialization policy separately from its electrical artifacts.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AnalysisPlan {
    /// Explicit source analysis semantics.
    pub dialect: AnalysisDialect,
    /// Execution scope of the artifact.
    pub kind: PlanKind,
    /// Complete `SIMetrix` title, including an empty title.
    pub title: Option<String>,
    /// Declarations in source/linked scope order.
    pub entries: Vec<PlanEntry>,
    /// Explicit selected realization; a sampled circuit requires occurrence binding.
    pub statistical_sample: Option<model_statistics::Realization>,
}

impl AnalysisPlan {
    /// Construct an empty ordered source analysis artifact.
    #[must_use]
    pub const fn new(dialect: AnalysisDialect, kind: PlanKind, title: Option<String>) -> Self {
        Self {
            dialect,
            kind,
            title,
            entries: Vec::new(),
            statistical_sample: None,
        }
    }

    /// Reject unsupported original-deck behavior. Binding-required entries
    /// still need explicit consumer binding.
    /// # Errors
    /// Unsupported request or option, with its origin.
    pub fn validate_supported(&self) -> Result<()> {
        for entry in &self.entries {
            ensure!(
                entry.disposition != Disposition::Unsupported,
                "unsupported analysis directive {} at {:?}",
                entry.statement,
                entry.origin
            );
            if let Directive::Options { settings } = &entry.directive {
                for setting in settings {
                    ensure!(
                        setting.disposition != Disposition::Unsupported,
                        "unsupported analysis option {} at {:?}",
                        setting.name,
                        entry.origin
                    );
                }
            }
        }
        let steps = self
            .entries
            .iter()
            .filter(|entry| {
                entry.dialect == AnalysisDialect::Simetrix
                    && matches!(entry.directive, Directive::Sweep { .. })
            })
            .count();
        if steps > 0 {
            ensure!(
                steps == 1,
                "SIMetrix STEP supports one compatibility axis; nested sweeps belong to an analysis"
            );
            let mut analyses = self.entries.iter().filter(|entry| entry.is_analysis());
            let request = analyses
                .next()
                .context("SIMetrix STEP requires a companion analysis")?;
            ensure!(
                analyses.next().is_none()
                    && request.independent_axes.is_empty()
                    && matches!(
                        request.command.as_str(),
                        ".DC" | ".AC" | ".TF" | ".NOISE" | ".TRAN"
                    ),
                "SIMetrix STEP requires one compatible analysis without inline sweeps"
            );
        }
        pspice::validate(self)?;
        Ok(())
    }

    /// Native-equivalent requests. Does not bind output coordinates.
    pub fn native_requests(&self) -> impl Iterator<Item = &PlanEntry> {
        self.entries
            .iter()
            .filter(|entry| entry.disposition == Disposition::NativeRequest)
    }

    /// Validate execution without a dialect-specific binding implementation.
    /// # Errors
    /// The plan is a library, contains unsupported declarations, or requires binding.
    pub fn validate_unbound_execution(&self) -> Result<()> {
        self.validate_supported()?;
        ensure!(
            self.kind == PlanKind::Deck,
            "library plan is not executable"
        );
        ensure!(
            !self.requires_binding(),
            "analysis plan requires explicit dialect binding"
        );
        Ok(())
    }

    /// Whether source-specific execution/output binding is required.
    #[must_use = "the consumer must handle deferred analysis semantics"]
    pub fn requires_binding(&self) -> bool {
        self.statistical_sample.is_some() || self.entries.iter().any(|entry| entry.disposition == Disposition::RequiresBinding
            || matches!(&entry.directive, Directive::Options { settings } if settings.iter().any(|setting| setting.disposition == Disposition::RequiresBinding)))
    }

    /// Bind the independent axes of a specific analysis in innermost-first
    /// order, using the declaration's lexical scalar resolver. Analysis-local
    /// axes do not leak into unrelated requests. Reserve the complete product
    /// before executing any candidate.
    ///
    /// # Errors
    /// Invalid analysis index, scoped/unsupported declaration, invalid or
    /// unbound coordinate, cardinality overflow, or exhausted request budget.
    pub fn bind_independent_sweeps(
        &self,
        analysis: usize,
        mut resolve: impl FnMut(&PlanEntry, &SweepAxis, &str) -> Result<f64>,
        budget: &WorkBudget,
    ) -> Result<BoundIndependentSweeps<'_>> {
        self.validate_supported()?;
        ensure!(
            self.kind == PlanKind::Deck,
            "library analysis plan is not executable"
        );
        let request = self
            .entries
            .get(analysis)
            .filter(|entry| entry.is_analysis())
            .context("independent sweep must select an actual analysis entry")?;
        let local = request.independent_axes.iter().map(|axis| (request, axis));
        let global = self.entries.iter().filter_map(|entry| {
            if let Directive::Sweep { axis } = &entry.directive {
                Some((entry, axis))
            } else {
                None
            }
        });
        let mut axes = Vec::new();
        for (entry, axis) in local.chain(global) {
            ensure!(
                axis.execution == SweepExecution::Independent,
                "non-independent axis in candidate product"
            );
            budget.charge(Resource::GeneratedNodes, 1)?;
            let coordinates = axis
                .bind(|scalar| resolve(entry, axis, scalar), budget)
                .with_context(|| format!("sweep declaration at {:?}", entry.origin))?;
            axes.push(BoundSweep {
                entry,
                axis,
                coordinates,
            });
        }
        let count = sweep::reserve_product(axes.iter().map(|axis| &axis.coordinates), budget)?;
        Ok(BoundIndependentSweeps { axes, count })
    }
}

/// A bound axis retains both declaration identity and the precise typed
/// target. Several independent axes may come from the same statement.
#[derive(Debug)]
pub struct BoundSweep<'a> {
    /// Original declaration and lexical context.
    pub entry: &'a PlanEntry,
    /// Source target and coordinate policy.
    pub axis: &'a SweepAxis,
    /// Compact validated coordinate sequence.
    pub coordinates: BoundAxis,
}

/// One candidate's override for a particular bound axis.
#[derive(Debug)]
pub struct BoundCoordinate<'a> {
    /// Original declaration.
    pub entry: &'a PlanEntry,
    /// Source target; resolution and invalidation belong to the consumer.
    pub axis: &'a SweepAxis,
    /// Physical candidate value.
    pub value: f64,
}

/// Independent sweep axes bound to source declarations. Coordinates are
/// represented by compact sequences; the execution layer owns candidate
/// invalidation, analysis scheduling and state reset.
#[derive(Debug)]
pub struct BoundIndependentSweeps<'a> {
    axes: Vec<BoundSweep<'a>>,
    count: usize,
}

impl<'a> BoundIndependentSweeps<'a> {
    /// Count of combinations, including the nominal case with no axes.
    #[must_use = "the count is the reserved candidate work"]
    pub const fn count(&self) -> usize {
        self.count
    }

    /// Coordinates of a candidate in first-axis-innermost order.
    /// The returned iterator borrows the compact axis sequences.
    ///
    /// # Errors
    /// Candidate ordinal or bound coordinate outside the reserved product.
    pub fn candidate(
        &self,
        ordinal: usize,
    ) -> Result<impl Iterator<Item = Result<BoundCoordinate<'a>>> + '_> {
        ensure!(
            ordinal < self.count,
            "candidate ordinal outside reserved sweep"
        );
        Ok(self.axes.iter().scan(ordinal, |remaining, bound| {
            let index = *remaining % bound.coordinates.len();
            *remaining /= bound.coordinates.len();
            Some(
                bound
                    .coordinates
                    .value(index)
                    .map(|value| BoundCoordinate {
                        entry: bound.entry,
                        axis: bound.axis,
                        value,
                    })
                    .context("missing reserved sweep coordinate"),
            )
        }))
    }

    /// The source-order axes, with declaration identity and coordinate law.
    pub fn axes(&self) -> impl ExactSizeIterator<Item = &BoundSweep<'a>> {
        self.axes.iter()
    }
}

/// Extract logical analysis operands without interpreting source scalars.
///
/// # Errors
/// Invalid or unbalanced source tokens.
pub fn arguments(statement: &str) -> Result<Vec<String>> {
    let fields = model_normalize_ngspice::statement_token_spans(statement)?;
    Ok(fields
        .into_iter()
        .skip(1)
        .map(|span| statement[span].to_owned())
        .collect())
}
