//! Source analysis intent, independent of the prepared electrical circuit.
pub mod source_capture;
pub mod sweep;
use anyhow::{Context, Result, ensure};
use model_normalize_ngspice::ltspice::transient::TransientIntent;
use model_preprocess_command::{Resource, WorkBudget};
use serde::{Deserialize, Serialize};
use std::{fmt::Write as _, path::PathBuf};
use sweep::{BoundAxis, CoordinateOrder, SweepAxis, SweepExecution};

/// Analysis semantics (not the syntax of individual model expressions).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisDialect {
    /// Native ngspice analysis semantics.
    Native,
    /// LTspice analysis and independent parameter stepping.
    Ltspice,
    /// PSpice source analysis declarations.
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
    /// Numerical setting materialized in the circuit.
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
        /// Native transient UIC flag, removed from arguments.
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
    /// Ordered option settings.
    Options {
        /// Individual settings.
        settings: Vec<OptionSetting>,
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
    /// Execution policy.
    pub disposition: Disposition,
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
    pub(crate) const fn new(
        dialect: AnalysisDialect,
        kind: PlanKind,
        title: Option<String>,
    ) -> Self {
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

    /// Bind all independent axes in source order using a caller-provided
    /// lexical scalar resolver, then reserve their Cartesian product.
    /// No candidate deck or tuple table is allocated.
    ///
    /// # Errors
    /// Invalid or unbound coordinate, scoped/unsupported declaration,
    /// cardinality overflow, or exhausted request budget.
    pub fn bind_independent_sweeps(
        &self,
        mut resolve: impl FnMut(&PlanEntry, &str) -> Result<f64>,
        budget: &WorkBudget,
    ) -> Result<BoundIndependentSweeps<'_>> {
        self.validate_supported()?;
        let mut entries = Vec::new();
        let mut axes = Vec::new();
        for entry in &self.entries {
            if let Directive::Sweep { axis } = &entry.directive
                && axis.execution == SweepExecution::Independent
            {
                budget.charge(Resource::GeneratedNodes, 1)?;
                axes.push(
                    axis.bind(|scalar| resolve(entry, scalar), budget)
                        .with_context(|| format!("sweep declaration at {:?}", entry.origin))?,
                );
                entries.push(entry);
            }
        }
        let count = sweep::reserve_product(&axes, budget)?;
        Ok(BoundIndependentSweeps {
            entries,
            axes,
            count,
        })
    }

    // Select an actual member of a Monte Carlo sweep, not a nominal
    // substitute for the source analysis. Keep the original source statement
    // in the manifest and normalize only its selected electrical request.
    pub(crate) fn select_statistical_sample(
        &mut self,
        selected: Option<model_statistics::Realization>,
        budget: &WorkBudget,
    ) -> Result<()> {
        self.statistical_sample = selected;
        for entry in &mut self.entries {
            budget.charge(Resource::EvaluationSteps, 1)?;
            if let Directive::Options { settings } = &entry.directive {
                ensure!(
                    !settings.iter().any(|option| option.name == "MCHSPICE"),
                    "MCHSPICE statistical semantics require explicit source normalization"
                );
            }
            let Directive::Analysis { arguments, uic, .. } = &entry.directive else {
                ensure!(
                    !matches!(
                        entry.command.as_str(),
                        ".MC" | ".MONTE" | ".WCASE" | ".WORSTCASE"
                    ),
                    "SIMetrix statistical analysis is not implemented for {}",
                    entry.command
                );
                continue;
            };
            budget.charge(Resource::EvaluationSteps, arguments.len())?;
            let Some(sweep) = arguments.windows(2).position(|pair| {
                pair[0].eq_ignore_ascii_case("SWEEP") && pair[1].eq_ignore_ascii_case("MONTE")
            }) else {
                continue;
            };
            let realization = selected
                .context("Monte Carlo sweep requires explicit statistical sample SEED:INDEX")?;
            let tail = &arguments[sweep + 2..];
            let count = tail
                .first()
                .context("missing Monte Carlo sample count")?
                .parse::<u64>()?;
            ensure!(
                count > 0 && realization.sample < count,
                "statistical sample {} outside sweep of {count}",
                realization.sample
            );
            // NUMCORES affects scheduling of the original sweep only. All
            // nested sweeps and other modifiers still require orchestration.
            match &tail[1..] {
                [] => {}
                [name, equals, cores] if name.eq_ignore_ascii_case("NUMCORES") && equals == "=" => {
                    ensure!(cores.parse::<u64>()? > 0, "invalid NUMCORES");
                }
                [setting] => {
                    let (name, cores) = setting
                        .split_once('=')
                        .context("unsupported Monte Carlo sweep modifier")?;
                    ensure!(
                        name.eq_ignore_ascii_case("NUMCORES") && cores.parse::<u64>()? > 0,
                        "unsupported Monte Carlo sweep modifier"
                    );
                }
                _ => anyhow::bail!(
                    "nested or extended Monte Carlo sweep requires explicit orchestration"
                ),
            }
            ensure!(
                entry.scope.is_empty(),
                "scoped statistical analysis is not executable"
            );
            let length = arguments[..sweep].iter().try_fold(
                entry.command.len().saturating_add(5),
                |length, arg| {
                    length
                        .checked_add(arg.len().saturating_add(1))
                        .context("sample analysis length overflow")
                },
            )?;
            budget.charge(Resource::EmittedBytes, length.saturating_mul(3))?;
            budget.charge(Resource::GeneratedNodes, sweep)?;
            let mut single = format!("{} {}", entry.command, arguments[..sweep].join(" "));
            if *uic {
                single.push_str(" UIC");
            }
            let (directive, disposition) = classify(&entry.command, &single, entry.dialect, false)?;
            ensure!(
                disposition != Disposition::Unsupported,
                "unsupported selected Monte Carlo analysis: {single}"
            );
            entry.directive = directive;
            entry.disposition = disposition;
        }
        if selected.is_some() {
            self.validate_supported()?;
        }
        Ok(())
    }

    pub(crate) fn append_native_options(
        &self,
        bytes: &mut Vec<u8>,
        budget: &WorkBudget,
    ) -> Result<()> {
        let mut output = String::new();
        for entry in &self.entries {
            budget.charge(Resource::EvaluationSteps, 1)?;
            if entry.disposition == Disposition::Circuit
                && let Directive::Options { settings } = &entry.directive
            {
                let mut card = String::new();
                for option in settings.iter().filter(|option| {
                    option.disposition == Disposition::Circuit && option.name != "TEMP"
                }) {
                    budget.charge(Resource::EmittedBytes, option.name.len().saturating_add(1))?;
                    write!(card, " {}", option.name)?;
                    if let Some(value) = &option.value {
                        budget.charge(Resource::EmittedBytes, value.len().saturating_add(1))?;
                        write!(card, "={value}")?;
                    }
                }
                if !card.is_empty() {
                    budget.charge(Resource::EmittedBytes, card.len().saturating_add(9))?;
                    writeln!(output, ".options{card}")?;
                }
            }
        }
        if !output.is_empty() {
            let end = bytes
                .len()
                .checked_sub(b".end\n".len())
                .context("missing prepared deck terminator")?;
            ensure!(
                &bytes[end..] == b".end\n",
                "missing prepared deck terminator"
            );
            budget.charge(Resource::EmittedBytes, output.len())?;
            bytes.splice(end..end, output.bytes());
        }
        Ok(())
    }
}

/// Independent sweep axes bound to source declarations. Coordinates are
/// represented by compact sequences; the execution layer owns candidate
/// invalidation, analysis scheduling and state reset.
#[derive(Debug)]
pub struct BoundIndependentSweeps<'a> {
    entries: Vec<&'a PlanEntry>,
    axes: Vec<BoundAxis>,
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
    ) -> Result<impl Iterator<Item = Result<(&'a PlanEntry, f64)>> + '_> {
        ensure!(
            ordinal < self.count,
            "candidate ordinal outside reserved sweep"
        );
        Ok(self.entries.iter().copied().zip(&self.axes).scan(
            ordinal,
            |remaining, (entry, axis)| {
                let index = *remaining % axis.len();
                *remaining /= axis.len();
                Some(
                    axis.value(index)
                        .map(|value| (entry, value))
                        .context("missing reserved sweep coordinate"),
                )
            },
        ))
    }

    /// The source-order axes, with declaration identity and coordinate law.
    pub fn axes(&self) -> impl ExactSizeIterator<Item = (&'a PlanEntry, &BoundAxis)> {
        self.entries.iter().copied().zip(&self.axes)
    }
}

pub(crate) const TAG_PREFIX: &str = "*#ngspice-rs-preprocess-plan:";

#[derive(Clone, Debug)]
pub(crate) struct SourceDeclaration {
    pub origin: DirectiveOrigin,
    pub text: String,
}

pub(crate) fn source_title(bytes: &[u8]) -> String {
    String::from_utf8_lossy(
        bytes
            .split(|byte| *byte == b'\n')
            .next()
            .unwrap_or_default(),
    )
    .trim_end_matches('\r')
    .to_owned()
}

pub(crate) fn command(statement: &str) -> String {
    statement
        .split_ascii_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase()
}

pub(crate) fn tracked(command: &str) -> bool {
    matches!(
        command,
        ".OP"
            | ".DC"
            | ".AC"
            | ".TRAN"
            | ".POP"
            | ".NOISE"
            | ".TF"
            | ".PZ"
            | ".DISTO"
            | ".SENS"
            | ".SP"
            | ".PSS"
            | ".HB"
            | ".PAC"
            | ".PNOISE"
            | ".STEP"
            | ".MONTE"
            | ".MC"
            | ".WCASE"
            | ".SAVEBIAS"
            | ".LOADBIAS"
            | ".OPTIONS"
            | ".OPTION"
            | ".OPT"
            | ".TEMP"
            | ".IC"
            | ".INIT"
            | ".NODESET"
            | ".PRINT"
            | ".KEEP"
            | ".SAVE"
            | ".PROBE"
            | ".MEAS"
            | ".MEASURE"
            | ".FOUR"
            | ".PLOT"
            | ".NODE_MAP"
            | ".GRAPH"
            | ".POST_PROCESS"
            | ".LICENSE_FEATURE"
            | ".REQUIRED_VERSION"
            | ".CONTROL"
    )
}

fn option_disposition(name: &str, dialect: AnalysisDialect) -> Disposition {
    use Disposition::{
        Circuit, Initialization, Metadata, ModelSelection, RequiresBinding, Unsupported,
    };
    match (dialect, name) {
        (
            AnalysisDialect::Ltspice,
            "RELTOL" | "ABSTOL" | "VNTOL" | "CHGTOL" | "TRTOL" | "GMIN" | "METHOD" | "TNOM"
            | "MAXORD" | "ITL1" | "ITL2" | "ITL4",
        ) => Circuit,
        (
            AnalysisDialect::Ltspice,
            "MAXSTEP" | "TEMP" | "SSTOL" | "STSTDELAY" | "STSTCLOCKS" | "PLOTRELTOL" | "PLOTVNTOL"
            | "PLOTABSTOL" | "PLOTWINSIZE",
        ) => RequiresBinding,
        (
            AnalysisDialect::Ltspice,
            "NUMDGT" | "MEASDGT" | "LOGPARAMS" | "LOGOPINFO" | "DEBUGTRAN",
        ) => Metadata,
        (AnalysisDialect::Simetrix, "HSPICEMODELS" | "HSPICECOMPATIBILITY") => ModelSelection,
        (AnalysisDialect::Simetrix, "INITCONDMODE" | "ICRES") => Initialization,
        (
            AnalysisDialect::Simetrix,
            "TEMP" | "TNOM" | "RELTOL" | "ABSTOL" | "VNTOL" | "CHGTOL" | "TRTOL" | "GMIN"
            | "RSHUNT" | "CSHUNT" | "METHOD" | "MAXORD" | "ITL1" | "ITL2" | "ITL4",
        ) => Circuit,
        (
            AnalysisDialect::Simplis,
            "PSP_START"
            | "PSP_END"
            | "PSP_NPT"
            | "SNAPSHOT_INTVL"
            | "SNAPSHOT_NPT"
            | "NO_FORCED_DATA"
            | "POP_USE_TRAN_SNAPSHOT"
            | "POP_ITRMAX"
            | "POP_OUTPUT_CYCLES"
            | "FREQ_DOMAIN"
            | "NEW_ANALYSIS"
            | "MIN_AVG_TOPOLOGY_DUR"
            | "AVG_TOPOLOGY_DUR_MEASUREMENT_WINDOW",
        ) => RequiresBinding,
        (_, "EMSG_MAX" | "EXPAND" | "POP_SHOWDATA" | "LIST" | "NODE" | "NOMOD" | "NOPAGE") => {
            Metadata
        }
        _ => Unsupported,
    }
}

fn options(fields: &[String], dialect: AnalysisDialect) -> Result<Vec<OptionSetting>> {
    let mut result = Vec::new();
    let mut index = 0;
    while let Some(name) = fields.get(index) {
        ensure!(
            !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'),
            "invalid option name {name:?}"
        );
        index += 1;
        let value = if fields.get(index).is_some_and(|field| field == "=") {
            let value = fields
                .get(index + 1)
                .filter(|value| *value != "=")
                .context("missing option value")?
                .clone();
            index += 2;
            Some(value)
        } else {
            None
        };
        let name = name.to_ascii_uppercase();
        let disposition = option_disposition(&name, dialect);
        ensure!(
            !matches!(
                disposition,
                Disposition::Circuit | Disposition::Initialization
            ) || value.is_some(),
            "{name} requires an assignment"
        );
        result.push(OptionSetting {
            name,
            value,
            disposition,
        });
    }
    ensure!(!result.is_empty(), "empty options statement");
    Ok(result)
}

pub(crate) fn arguments(statement: &str) -> Result<Vec<String>> {
    let fields = model_normalize_ngspice::statement_token_spans(statement)?;
    Ok(fields
        .into_iter()
        .skip(1)
        .map(|span| statement[span].to_owned())
        .collect())
}

pub(crate) fn analysis_kind(name: &str) -> AnalysisKind {
    match name {
        ".OP" => AnalysisKind::OperatingPoint,
        ".DC" => AnalysisKind::DcSweep,
        ".AC" => AnalysisKind::Ac,
        ".TRAN" => AnalysisKind::Transient,
        ".POP" => AnalysisKind::Pop,
        ".NOISE" => AnalysisKind::Noise,
        ".TF" => AnalysisKind::TransferFunction,
        _ => AnalysisKind::Other,
    }
}

pub(crate) fn native_analysis_shape(kind: AnalysisKind, args: &[String]) -> bool {
    let sweep = |value: &str| matches!(value.to_ascii_uppercase().as_str(), "LIN" | "DEC" | "OCT");
    let scalar = |value: &str| {
        model_translate_simplis::syntax::parse_numeric(value).is_ok()
            || (value.starts_with('{') && value.ends_with('}'))
    };
    match kind {
        AnalysisKind::OperatingPoint => args.is_empty(),
        AnalysisKind::Transient => {
            (1..=4).contains(&args.len()) && args.iter().all(|value| scalar(value))
        }
        AnalysisKind::Ac => {
            args.len() == 4 && sweep(&args[0]) && args[1..].iter().all(|value| scalar(value))
        }
        AnalysisKind::DcSweep => {
            matches!(args.len(), 4 | 8)
                && args
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .all(|chunk| chunk[1..].iter().all(|value| scalar(value)))
        }
        AnalysisKind::Noise => {
            matches!(args.len(), 6 | 7)
                && sweep(&args[2])
                && args[3..].iter().all(|value| scalar(value))
        }
        AnalysisKind::TransferFunction => args.len() == 2,
        AnalysisKind::Pop | AnalysisKind::Other => false,
    }
}

/// `SIMetrix` defaults both output and maximum step to `(stop-start)/50`.
/// Do not delegate omitted or zero steps to a different simulator's defaults.
fn transient_arguments(args: &[String]) -> Option<Vec<String>> {
    use model_translate_simplis::syntax::parse_numeric;
    let number = |value: &str| parse_numeric(value).ok();
    let (interval, stop, start, maximum) = match args {
        [stop] => (None, stop, None, None),
        [interval, stop, rest @ ..] => (Some(interval), stop, rest.first(), rest.get(1)),
        _ => return None,
    };
    let start_number = start.map_or(Some(0.0), |v| number(v))?;
    let stop_number = number(stop)?;
    if start_number < 0.0 || stop_number <= start_number {
        return None;
    }
    let default = (stop_number - start_number) / 50.0;
    let normalize_step = |value: Option<&String>| -> Option<String> {
        match value {
            Some(value) => {
                let numeric = number(value)?;
                if numeric < 0.0 {
                    return None;
                }
                Some(if numeric == 0.0 {
                    default.to_string()
                } else {
                    value.clone()
                })
            }
            None => Some(default.to_string()),
        }
    };
    Some(vec![
        normalize_step(interval)?,
        stop.clone(),
        start.cloned().unwrap_or_else(|| "0".to_owned()),
        normalize_step(maximum)?,
    ])
}

fn ltspice_directive(
    name: &str,
    statement: &str,
    scoped: bool,
) -> Result<Option<(Directive, Disposition)>> {
    let disposition = if scoped {
        Disposition::Unsupported
    } else {
        Disposition::RequiresBinding
    };
    let directive = match name {
        ".STEP" => Directive::Sweep {
            axis: SweepAxis::ltspice_step(&arguments(statement)?)?,
        },
        ".TEMP" => {
            let args = arguments(statement)?;
            if args.len() <= 1 {
                return Ok(None);
            }
            Directive::Sweep {
                axis: SweepAxis::ltspice_temperature(&args)?,
            }
        }
        ".TRAN" => {
            let args = arguments(statement)?;
            let fields = args.iter().map(String::as_str).collect::<Vec<_>>();
            Directive::Transient {
                intent: TransientIntent::parse(&fields)?,
            }
        }
        ".DC" => Directive::DcSweep {
            axes: SweepAxis::ltspice_dc(&arguments(statement)?)?,
        },
        _ => return Ok(None),
    };
    Ok(Some((directive, disposition)))
}

fn classify(
    name: &str,
    statement: &str,
    dialect: AnalysisDialect,
    scoped: bool,
) -> Result<(Directive, Disposition)> {
    use Disposition::{
        Circuit, Initialization, Metadata, NativeRequest, RequiresBinding, Unsupported,
    };
    if dialect == AnalysisDialect::Ltspice
        && let Some(directive) = ltspice_directive(name, statement, scoped)?
    {
        return Ok(directive);
    }
    if matches!(
        name,
        ".CONTROL"
            | ".POST_PROCESS"
            | ".STEP"
            | ".MONTE"
            | ".MC"
            | ".WCASE"
            | ".SAVEBIAS"
            | ".LOADBIAS"
    ) {
        return Ok((Directive::Unsupported, Unsupported));
    }
    if matches!(name, ".GRAPH" | ".LICENSE_FEATURE" | ".REQUIRED_VERSION") {
        return Ok((Directive::Metadata, Metadata));
    }
    let args = arguments(statement)?;
    if matches!(name, ".OPTIONS" | ".OPTION" | ".OPT") {
        return Ok((
            Directive::Options {
                settings: options(&args, dialect)?,
            },
            if scoped { Unsupported } else { Circuit },
        ));
    }
    if matches!(name, ".TEMP" | ".NODESET" | ".IC" | ".INIT") {
        let disposition = match (name, dialect) {
            (".INIT", _) | (".IC", AnalysisDialect::Simetrix) => Initialization,
            _ => Circuit,
        };
        return Ok((Directive::Circuit, disposition));
    }
    if matches!(
        name,
        ".PRINT"
            | ".KEEP"
            | ".SAVE"
            | ".PROBE"
            | ".MEAS"
            | ".MEASURE"
            | ".FOUR"
            | ".PLOT"
            | ".NODE_MAP"
    ) {
        return Ok((Directive::Observation { arguments: args }, RequiresBinding));
    }
    let analysis = analysis_kind(name);
    let mut args = args;
    let uic = matches!(dialect, AnalysisDialect::Simetrix | AnalysisDialect::Native)
        && analysis == AnalysisKind::Transient
        && args
            .last()
            .is_some_and(|arg| arg.eq_ignore_ascii_case("UIC"));
    if uic {
        args.pop();
    }
    let disposition = if scoped {
        Unsupported
    } else {
        match (dialect, analysis) {
            (AnalysisDialect::Simetrix, AnalysisKind::Transient)
                if native_analysis_shape(analysis, &args) =>
            {
                transient_arguments(&args).map_or(RequiresBinding, |native| {
                    args = native;
                    NativeRequest
                })
            }
            (AnalysisDialect::Native | AnalysisDialect::Ltspice | AnalysisDialect::Simetrix, _)
                if native_analysis_shape(analysis, &args) =>
            {
                if matches!(
                    analysis,
                    AnalysisKind::DcSweep | AnalysisKind::Noise | AnalysisKind::TransferFunction
                ) || args.iter().any(|arg| arg.starts_with('{'))
                {
                    // Named sweep stimuli and probes belong to the original
                    // device namespace, which may differ after primitive lowering.
                    RequiresBinding
                } else {
                    NativeRequest
                }
            }
            (
                AnalysisDialect::Pspice
                | AnalysisDialect::Tina
                | AnalysisDialect::Qspice
                | AnalysisDialect::Hspice,
                _,
            ) if analysis != AnalysisKind::Other => RequiresBinding,
            (AnalysisDialect::Ltspice, AnalysisKind::Ac)
                if args.first().is_some_and(|arg| {
                    arg.eq_ignore_ascii_case("LIST") || arg.eq_ignore_ascii_case("FILE")
                }) =>
            {
                RequiresBinding
            }
            (AnalysisDialect::Simplis, AnalysisKind::Transient) if args.len() == 2 => {
                RequiresBinding
            }
            // SIMPLIS AC depends on a periodic operating point, not DC AC.
            _ => Unsupported,
        }
    };
    Ok((
        Directive::Analysis {
            analysis,
            arguments: args,
            uic,
        },
        disposition,
    ))
}

fn push_entry(
    plan: &mut AnalysisPlan,
    origin: DirectiveOrigin,
    scope: &[String],
    text: &str,
    library: bool,
    budget: &WorkBudget,
    dialect: AnalysisDialect,
) -> Result<()> {
    budget.charge(Resource::GeneratedNodes, 1)?;
    budget.charge(Resource::EmittedBytes, text.len())?;
    budget.charge(Resource::GeneratedNodes, scope.len())?;
    for part in scope {
        budget.charge(Resource::EmittedBytes, part.len())?;
    }
    let name = command(text);
    budget.charge(Resource::ScannedBytes, text.len())?;
    budget.charge(
        Resource::EmittedBytes,
        text.len().saturating_add(name.len()),
    )?;
    let (mut directive, disposition) = classify(&name, text, dialect, library || !scope.is_empty())
        .with_context(|| format!("analysis declaration at {origin:?}"))?;
    if let Directive::Sweep { axis } = &mut directive
        && axis.execution == SweepExecution::Independent
        && dialect == AnalysisDialect::Ltspice
        && !plan.entries.iter().any(|entry| matches!(&entry.directive, Directive::Sweep { axis } if axis.execution == SweepExecution::Independent))
    {
        axis.order = CoordinateOrder::Ascending;
    }
    plan.entries.push(PlanEntry {
        origin,
        dialect,
        scope: scope.to_vec(),
        command: name,
        statement: text.to_owned(),
        directive,
        disposition,
    });
    Ok(())
}

struct Capture<'a> {
    budget: &'a WorkBudget,
    plan: &'a mut AnalysisPlan,
    artifact: &'a str,
    source: Option<&'a std::path::Path>,
    declarations: &'a [SourceDeclaration],
    physical: Vec<&'a str>,
    removed: Vec<bool>,
    scope: Vec<String>,
    pending: Option<&'a SourceDeclaration>,
    embedded: bool,
    control: Option<usize>,
}

impl Capture<'_> {
    fn library(&self) -> bool {
        self.artifact != "deck.cir" || self.plan.kind == PlanKind::Library
    }

    fn origin(&self, first_line: usize, last_line: usize) -> Result<DirectiveOrigin> {
        self.budget.charge(
            Resource::EmittedBytes,
            self.source
                .map_or(self.artifact.len(), |path| path.as_os_str().len()),
        )?;
        Ok(self.source.map_or_else(
            || DirectiveOrigin::Generated {
                artifact: self.artifact.to_owned(),
                first_line,
                last_line,
            },
            |path| DirectiveOrigin::Source {
                path: path.to_path_buf(),
                first_line,
                last_line,
            },
        ))
    }

    fn entry(&mut self, origin: DirectiveOrigin, text: &str) -> Result<()> {
        let library = self.library();
        push_entry(
            self.plan,
            origin,
            &self.scope,
            text,
            library,
            self.budget,
            self.plan.dialect,
        )
    }

    fn clone_origin(&self, origin: &DirectiveOrigin) -> Result<DirectiveOrigin> {
        let bytes = match origin {
            DirectiveOrigin::Source { path, .. }
            | DirectiveOrigin::SourceOccurrence { path, .. } => path.as_os_str().len(),
            DirectiveOrigin::Generated { artifact, .. } => artifact.len(),
        };
        self.budget.charge(Resource::EmittedBytes, bytes)?;
        Ok(origin.clone())
    }

    fn tag(&mut self, tag: &str, first_line: usize) -> Result<()> {
        ensure!(self.pending.is_none(), "lost analysis source declaration");
        let index = tag
            .parse::<usize>()
            .context("invalid internal analysis origin tag")?;
        let declaration = self
            .declarations
            .get(index)
            .context("missing internal analysis origin")?;
        self.removed[first_line - 1] = true;
        if matches!(command(&declaration.text).as_str(), ".INIT" | ".CONTROL") {
            self.entry(self.clone_origin(&declaration.origin)?, &declaration.text)?;
        } else {
            self.pending = Some(declaration);
        }
        Ok(())
    }

    fn consume(
        &mut self,
        line: &model_preprocess_simplis::source_dialect::ProjectedStatement,
    ) -> Result<()> {
        use model_translate_simplis::syntax::file_boundary;
        self.budget.charge(Resource::Statements, 1)?;
        self.budget
            .charge(Resource::ScannedBytes, line.text.len())?;
        let name = command(&line.text);
        if let Some(first) = self.control {
            if name == ".ENDC" {
                let lines = &self.physical[first - 1..line.last_line];
                self.budget.charge(
                    Resource::EmittedBytes,
                    lines.iter().map(|line| line.len()).sum(),
                )?;
                let statement = lines.concat();
                self.entry(self.origin(first, line.last_line)?, &statement)?;
                self.removed[first - 1..line.last_line].fill(true);
                self.control = None;
            }
            return Ok(());
        }
        if self.embedded {
            if file_boundary(&line.text) == Some(false) {
                self.embedded = false;
            }
            return Ok(());
        }
        if file_boundary(&line.text) == Some(true) {
            self.embedded = true;
            return Ok(());
        }
        if let Some(tag) = line.text.strip_prefix(TAG_PREFIX) {
            return self.tag(tag, line.first_line);
        }
        if line.text.trim().is_empty() || line.text.trim_start().starts_with('*') {
            return Ok(());
        }
        if name == ".SUBCKT" {
            let name = line
                .text
                .split_ascii_whitespace()
                .nth(1)
                .context("missing subcircuit name")?;
            self.budget
                .check_depth(self.scope.len().saturating_add(1))?;
            self.budget.charge(Resource::EmittedBytes, name.len())?;
            self.budget.charge(Resource::GeneratedNodes, 1)?;
            self.scope.push(name.to_owned());
            return Ok(());
        }
        if name == ".ENDS" {
            self.scope
                .pop()
                .context("unmatched subcircuit end in analysis capture")?;
            return Ok(());
        }
        if !tracked(&name) {
            return Ok(());
        }
        // The linker reorders .INIT snapshot entries; original root declarations
        // were retained at their source tags. Keep effective entries for initialization.
        if name == ".INIT"
            && self.pending.is_none()
            && !self.declarations.is_empty()
            && self.scope.is_empty()
        {
            return Ok(());
        }
        if name == ".CONTROL" {
            self.control = Some(line.first_line);
            return Ok(());
        }
        let origin = if let Some(declaration) = self.pending.take() {
            ensure!(
                command(&declaration.text) == name,
                "analysis origin does not match {name}"
            );
            self.clone_origin(&declaration.origin)?
        } else {
            self.origin(line.first_line, line.last_line)?
        };
        self.entry(origin, &line.text)?;
        if !matches!(
            name.as_str(),
            ".OPTIONS" | ".OPTION" | ".OPT" | ".TEMP" | ".IC" | ".INIT" | ".NODESET" | ".SAVE"
        ) {
            self.removed[line.first_line - 1..line.last_line].fill(true);
        }
        Ok(())
    }

    fn finish(self) -> Result<Vec<u8>> {
        ensure!(
            self.pending.is_none(),
            "lost final analysis source declaration"
        );
        ensure!(self.control.is_none(), "unterminated .CONTROL block");
        ensure!(
            self.scope.is_empty(),
            "unterminated subcircuit in analysis capture"
        );
        let length = self
            .physical
            .iter()
            .zip(&self.removed)
            .filter_map(|(line, remove)| (!remove).then_some(line.len()))
            .sum();
        self.budget.charge(Resource::EmittedBytes, length)?;
        Ok(self
            .physical
            .into_iter()
            .zip(self.removed)
            .filter_map(|(line, remove)| (!remove).then_some(line))
            .collect::<String>()
            .into_bytes())
    }
}

/// Capture active declarations from linked source. Source tags follow root
/// conditional preprocessing and never enter final artifact bytes.
pub(crate) fn capture(
    plan: &mut AnalysisPlan,
    bytes: &mut Vec<u8>,
    artifact: &str,
    source: Option<&std::path::Path>,
    declarations: &[SourceDeclaration],
    budget: &WorkBudget,
) -> Result<()> {
    use model_preprocess_simplis::{
        LibrarySimulator, project_resource,
        source_dialect::{DialectContext, RootKind},
    };
    budget.charge(Resource::ScannedBytes, bytes.len())?;
    let simulator = if plan.dialect == AnalysisDialect::Simetrix {
        LibrarySimulator::Simetrix
    } else {
        LibrarySimulator::Simplis
    };
    let (_, lines) = project_resource(
        bytes,
        DialectContext::default(),
        RootKind::Include,
        simulator,
    )?;
    let physical = std::str::from_utf8(bytes)?
        .split_inclusive('\n')
        .collect::<Vec<_>>();
    let removed = vec![false; physical.len()];
    let mut capture = Capture {
        budget,
        plan,
        artifact,
        source,
        declarations,
        physical,
        removed,
        scope: Vec::new(),
        pending: None,
        embedded: false,
        control: None,
    };
    for line in lines {
        capture.consume(&line)?;
    }
    *bytes = capture.finish()?;
    Ok(())
}

#[cfg(test)]
mod typed_analysis_tests {
    use super::*;
    use model_normalize_ngspice::model_card::parse_spice_constant;

    fn add(
        plan: &mut AnalysisPlan,
        statement: &str,
        line: usize,
        budget: &WorkBudget,
    ) -> Result<()> {
        push_entry(
            plan,
            DirectiveOrigin::Generated {
                artifact: "deck.cir".into(),
                first_line: line,
                last_line: line,
            },
            &[],
            statement,
            false,
            budget,
            AnalysisDialect::Ltspice,
        )
    }

    #[test]
    fn typed_steps_and_timing_preserve_declaration_order() -> Result<()> {
        let budget = WorkBudget::default();
        let mut plan = AnalysisPlan::new(
            AnalysisDialect::Ltspice,
            PlanKind::Deck,
            Some("test".into()),
        );
        add(&mut plan, ".step param duration list 5m 1m 2m", 2, &budget)?;
        add(&mut plan, ".temp 25 -40 85", 3, &budget)?;
        add(&mut plan, ".tran 0 {duration} -1m 0 startup", 4, &budget)?;
        add(&mut plan, ".meas tran vmax MAX V(out)", 5, &budget)?;
        assert_eq!(plan.entries.len(), 4);
        assert!(plan.requires_binding());
        assert!(plan.validate_unbound_execution().is_err());
        let sweeps = plan.bind_independent_sweeps(|_, text| parse_spice_constant(text), &budget)?;
        assert_eq!(sweeps.count(), 9);
        let mut axes = sweeps.axes();
        let (entry, axis) = axes.next().context("missing duration sweep")?;
        assert_eq!(entry.command, ".STEP");
        assert_eq!(axis.value(0).map(f64::to_bits), Some(1e-3_f64.to_bits()));
        let (entry, axis) = axes.next().context("missing temperature sweep")?;
        assert_eq!(entry.command, ".TEMP");
        assert_eq!(axis.value(0).map(f64::to_bits), Some(25_f64.to_bits()));
        assert_eq!(axis.value(1).map(f64::to_bits), Some((-40_f64).to_bits()));
        let Directive::Transient { intent } = &plan.entries[2].directive else {
            anyhow::bail!("missing typed transient");
        };
        let timing = intent.bind(|text| {
            if text == "{duration}" {
                Ok(5e-3)
            } else {
                parse_spice_constant(text)
            }
        })?;
        assert_eq!(timing.max_step.to_bits(), (5e-3_f64 / 1024.).to_bits());
        assert!(timing.modes.startup && !timing.modes.uic);
        assert!(timing.native_timing().is_err());
        assert!(matches!(
            plan.entries[3].directive,
            Directive::Observation { .. }
        ));
        Ok(())
    }
}
