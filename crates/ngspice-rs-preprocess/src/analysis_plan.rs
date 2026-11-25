//! Source analysis intent, independent of the prepared electrical circuit.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{fmt::Write as _, path::PathBuf};

/// Analysis semantics (not the syntax of individual model expressions).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisDialect {
    /// Ordinary SPICE analyses with SIMetrix extensions.
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
        /// Unevaluated arguments in the declared dialect.
        arguments: Vec<String>,
        /// Native transient UIC flag, removed from arguments.
        uic: bool,
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
    /// Complete SIMetrix title, including an empty title.
    pub title: Option<String>,
    /// Declarations in source/linked scope order.
    pub entries: Vec<PlanEntry>,
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

    /// Whether source-specific execution/output binding is required.
    #[must_use = "the consumer must handle deferred analysis semantics"]
    pub fn requires_binding(&self) -> bool {
        self.entries.iter().any(|entry| entry.disposition == Disposition::RequiresBinding
            || matches!(&entry.directive, Directive::Options { settings } if settings.iter().any(|setting| setting.disposition == Disposition::RequiresBinding)))
    }

    pub(crate) fn append_native_options(&self, bytes: &mut Vec<u8>) -> Result<()> {
        let mut output = String::new();
        for entry in &self.entries {
            if entry.disposition == Disposition::Circuit
                && let Directive::Options { settings } = &entry.directive
            {
                let mut card = String::new();
                for option in settings.iter().filter(|option| {
                    option.disposition == Disposition::Circuit && option.name != "TEMP"
                }) {
                    write!(card, " {}", option.name)?;
                    if let Some(value) = &option.value {
                        write!(card, "={value}")?;
                    }
                }
                if !card.is_empty() {
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
            bytes.splice(end..end, output.bytes());
        }
        Ok(())
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
    use Disposition::{Circuit, Initialization, Metadata, RequiresBinding, Unsupported};
    match (dialect, name) {
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

fn arguments(statement: &str) -> Result<Vec<String>> {
    let fields = model_normalize_ngspice::statement_token_spans(statement)?;
    Ok(fields
        .into_iter()
        .skip(1)
        .map(|span| statement[span].to_owned())
        .collect())
}

fn analysis_kind(name: &str) -> AnalysisKind {
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

fn native_analysis_shape(kind: AnalysisKind, args: &[String]) -> bool {
    let sweep = |value: &str| matches!(value.to_ascii_uppercase().as_str(), "LIN" | "DEC" | "OCT");
    let scalar = |value: &str| {
        value.starts_with('{')
            || value.starts_with('\'')
            || value
                .as_bytes()
                .first()
                .is_some_and(|byte| byte.is_ascii_digit() || matches!(byte, b'+' | b'-' | b'.'))
    };
    match kind {
        AnalysisKind::OperatingPoint => args.is_empty(),
        AnalysisKind::Transient => {
            (2..=4).contains(&args.len()) && args.iter().all(|value| scalar(value))
        }
        AnalysisKind::Ac => {
            args.len() == 4 && sweep(&args[0]) && args[1..].iter().all(|value| scalar(value))
        }
        AnalysisKind::DcSweep => {
            matches!(args.len(), 4 | 8)
                && args
                    .chunks_exact(4)
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

fn classify(
    name: &str,
    statement: &str,
    dialect: AnalysisDialect,
    scoped: bool,
) -> Result<(Directive, Disposition)> {
    use Disposition::{
        Circuit, Initialization, Metadata, NativeRequest, RequiresBinding, Unsupported,
    };
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
    let uic = dialect == AnalysisDialect::Simetrix
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
            (AnalysisDialect::Simetrix, _) if native_analysis_shape(analysis, &args) => {
                NativeRequest
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
) -> Result<()> {
    let name = command(text);
    let (directive, disposition) =
        classify(&name, text, plan.dialect, library || !scope.is_empty())
            .with_context(|| format!("analysis declaration at {origin:?}"))?;
    plan.entries.push(PlanEntry {
        origin,
        scope: scope.to_vec(),
        command: name,
        statement: text.to_owned(),
        directive,
        disposition,
    });
    Ok(())
}

/// Capture active declarations from linked source. Source tags follow root
/// conditional preprocessing and never enter final artifact bytes.
pub(crate) fn capture(
    plan: &mut AnalysisPlan,
    bytes: &mut Vec<u8>,
    artifact: &str,
    source: Option<&std::path::Path>,
    declarations: &[SourceDeclaration],
) -> Result<()> {
    use model_preprocess_simplis::{
        LibrarySimulator, project_resource,
        source_dialect::{DialectContext, SourceKind},
    };
    use model_translate_simplis::syntax::file_boundary;
    let simulator = if plan.dialect == AnalysisDialect::Simetrix {
        LibrarySimulator::Simetrix
    } else {
        LibrarySimulator::Simplis
    };
    let (_, lines) = project_resource(
        bytes,
        DialectContext::default(),
        SourceKind::Include,
        simulator,
    )?;
    let library = artifact != "deck.cir" || plan.kind == PlanKind::Library;
    let text = std::str::from_utf8(bytes)?;
    let physical = text.split_inclusive('\n').collect::<Vec<_>>();
    let mut removed = vec![false; physical.len()];
    let mut scope = Vec::new();
    let mut pending: Option<&SourceDeclaration> = None;
    let mut embedded = false;
    let mut control = None;
    let origin_at = |first_line, last_line| {
        source.map_or_else(
            || DirectiveOrigin::Generated {
                artifact: artifact.to_owned(),
                first_line,
                last_line,
            },
            |path| DirectiveOrigin::Source {
                path: path.to_path_buf(),
                first_line,
                last_line,
            },
        )
    };
    for line in lines {
        let name = command(&line.text);
        if let Some(first) = control {
            if name == ".ENDC" {
                let statement = physical[first - 1..line.last_line].concat();
                push_entry(
                    plan,
                    origin_at(first, line.last_line),
                    &scope,
                    &statement,
                    library,
                )?;
                removed[first - 1..line.last_line].fill(true);
                control = None;
            }
            continue;
        }
        if let Some(start) = file_boundary(&line.text) {
            embedded = start;
        }
        if embedded || name == ".ENDF" {
            continue;
        }
        if let Some(tag) = line.text.strip_prefix(TAG_PREFIX) {
            ensure!(pending.is_none(), "lost analysis source declaration");
            let index = tag
                .parse::<usize>()
                .context("invalid internal analysis origin tag")?;
            let declaration = declarations
                .get(index)
                .context("missing internal analysis origin")?;
            removed[line.first_line - 1] = true;
            if matches!(command(&declaration.text).as_str(), ".INIT" | ".CONTROL") {
                push_entry(
                    plan,
                    declaration.origin.clone(),
                    &scope,
                    &declaration.text,
                    library,
                )?;
            } else {
                pending = Some(declaration);
            }
            continue;
        }
        if line.text.trim().is_empty() || line.text.trim_start().starts_with('*') {
            continue;
        }
        if name == ".SUBCKT" {
            scope.push(
                line.text
                    .split_ascii_whitespace()
                    .nth(1)
                    .context("missing subcircuit name")?
                    .to_owned(),
            );
            continue;
        }
        if name == ".ENDS" {
            scope.pop();
            continue;
        }
        if !tracked(&name) {
            continue;
        }
        // The linker reorders .INIT snapshot entries; original root declarations
        // were retained at their source tags. Keep the effective entries in
        // electrical text for the existing initialization pass.
        if name == ".INIT" && pending.is_none() && !declarations.is_empty() && scope.is_empty() {
            continue;
        }
        if name == ".CONTROL" {
            control = Some(line.first_line);
            continue;
        }
        let origin = if let Some(declaration) = pending.take() {
            ensure!(
                command(&declaration.text) == name,
                "analysis origin does not match {name}"
            );
            declaration.origin.clone()
        } else {
            origin_at(line.first_line, line.last_line)
        };
        push_entry(plan, origin, &scope, &line.text, library)?;
        if !matches!(
            name.as_str(),
            ".OPTIONS" | ".OPTION" | ".OPT" | ".TEMP" | ".IC" | ".INIT" | ".NODESET" | ".SAVE"
        ) {
            removed[line.first_line - 1..line.last_line].fill(true);
        }
    }
    ensure!(pending.is_none(), "lost final analysis source declaration");
    ensure!(control.is_none(), "unterminated .CONTROL block");
    *bytes = physical
        .into_iter()
        .zip(removed)
        .filter_map(|(line, remove)| (!remove).then_some(line))
        .collect::<String>()
        .into_bytes();
    Ok(())
}
