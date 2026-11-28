//! Capture source declarations into the shared analysis artifact.
mod pspice;
mod simetrix;
pub mod source_capture;
use anyhow::{Context, Result, ensure};
use measurement::MeasurementIntent;
pub(crate) use model_analysis_plan::arguments;
pub use model_analysis_plan::{
    AnalysisDialect, AnalysisKind, AnalysisPlan, BoundCoordinate, BoundIndependentSweeps,
    BoundSweep, Directive, DirectiveOrigin, Disposition, OptionSetting, PlanEntry, PlanKind,
    measurement, sweep, timing,
};
use model_normalize_ngspice::ltspice::transient::TransientIntent;
use model_preprocess_command::{Resource, WorkBudget};
use std::fmt::Write as _;
use sweep::{CoordinateOrder, SweepAxis, SweepExecution};

// Select an actual member of a Monte Carlo sweep, not a nominal
// substitute for the source analysis. Keep the original source statement
// in the manifest and normalize only its selected electrical request.
pub(crate) fn select_statistical_sample(
    plan: &mut AnalysisPlan,
    selected: Option<model_statistics::Realization>,
    budget: &WorkBudget,
) -> Result<()> {
    plan.statistical_sample = selected;
    for entry in &mut plan.entries {
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
        plan.validate_supported()?;
    }
    Ok(())
}

pub(crate) fn append_native_options(
    plan: &AnalysisPlan,
    bytes: &mut Vec<u8>,
    budget: &WorkBudget,
) -> Result<()> {
    let mut output = String::new();
    for entry in &plan.entries {
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
            | ".TRAN/OP"
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
            | ".WORSTCASE"
            | ".DISTRIBUTION"
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
            | ".PROBE/CSDF"
            | ".WATCH"
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
    if dialect == AnalysisDialect::Ltspice
        && model_normalize_ngspice::ltspice::output_options::SOURCE_NAMES
            .iter()
            .any(|source| name.eq_ignore_ascii_case(source))
    {
        return Circuit;
    }
    match (dialect, name) {
        (
            AnalysisDialect::Ltspice | AnalysisDialect::Simetrix,
            "RELTOL" | "ABSTOL" | "VNTOL" | "CHGTOL" | "TRTOL" | "GMIN" | "TNOM" | "MAXORD"
            | "ITL1" | "ITL2" | "ITL4",
        )
        | (AnalysisDialect::Simetrix, "TEMP" | "RSHUNT" | "CSHUNT" | "METHOD") => Circuit,
        (
            AnalysisDialect::Ltspice,
            "MAXSTEP" | "TEMP" | "METHOD" | "SSTOL" | "STSTDELAY" | "STSTCLOCKS",
        )
        | (
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
        (AnalysisDialect::Ltspice, "MEASDGT" | "LOGPARAMS" | "LOGOPINFO" | "DEBUGTRAN") => Metadata,
        (AnalysisDialect::Simetrix, "HSPICEMODELS" | "HSPICECOMPATIBILITY") => ModelSelection,
        (AnalysisDialect::Simetrix, "INITCONDMODE" | "ICRES") => Initialization,
        (_, "EMSG_MAX" | "EXPAND" | "POP_SHOWDATA" | "LIST" | "NODE" | "NOMOD" | "NOPAGE") => {
            Metadata
        }
        _ => Unsupported,
    }
}

fn option_fields(
    statement: &str,
    dialect: AnalysisDialect,
) -> Result<Vec<(OptionSetting, std::ops::Range<usize>)>> {
    use model_normalize_ngspice::{model_card::parse_parameter_fields, statement_token_spans};
    let fields = if dialect == AnalysisDialect::Ltspice {
        model_normalize_ngspice::ltspice::output_options::source_fields(statement)?
    } else {
        let offset = statement_token_spans(statement)?
            .into_iter()
            .next()
            .context("missing options directive")?
            .end;
        let mut fields = parse_parameter_fields(&statement[offset..], &[])?;
        for field in &mut fields {
            field.span.name = field.span.name.start + offset..field.span.name.end + offset;
            if let Some(span) = &mut field.span.value {
                *span = span.start + offset..span.end + offset;
            }
        }
        fields
    };
    ensure!(!fields.is_empty(), "empty options statement");
    fields
        .into_iter()
        .map(|field| {
            let name = field.name.to_ascii_uppercase();
            ensure!(
                !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'),
                "invalid option name {name:?}"
            );
            let disposition = option_disposition(&name, dialect);
            ensure!(
                !matches!(
                    disposition,
                    Disposition::Circuit | Disposition::Initialization
                ) || field.expression.is_some(),
                "{name} requires an assignment"
            );
            let span = field.span.name.start
                ..field
                    .span
                    .value
                    .map_or(field.span.name.end, |span| span.end);
            Ok((
                OptionSetting {
                    name,
                    value: field.expression,
                    disposition,
                },
                span,
            ))
        })
        .collect()
}

fn options(statement: &str, dialect: AnalysisDialect) -> Result<Vec<OptionSetting>> {
    Ok(option_fields(statement, dialect)?
        .into_iter()
        .map(|(setting, _)| setting)
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
        ".MEAS" | ".MEASURE" => Directive::Measurement {
            intent: MeasurementIntent::parse_ltspice(statement)?,
        },
        _ => return Ok(None),
    };
    Ok(Some((directive, disposition)))
}

fn pspice_directive(
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
            axis: pspice::step(&pspice::arguments(statement)?)?,
        },
        ".DC" => Directive::DcSweep {
            axes: pspice::dc_axes(&pspice::arguments(statement)?)?,
        },
        ".TEMP" => {
            let args = pspice::arguments(statement)?;
            let axis = pspice::temperature(&args)?;
            if args.len() == 1 {
                return Ok(None);
            }
            Directive::Sweep { axis }
        }
        ".TRAN" | ".TRAN/OP" => {
            let (arguments, uic) = pspice::transient(pspice::arguments(statement)?)?;
            Directive::Analysis {
                analysis: AnalysisKind::Transient,
                arguments,
                uic,
            }
        }
        _ => return Ok(None),
    };
    Ok(Some((directive, disposition)))
}

fn dialect_directive(
    name: &str,
    statement: &str,
    dialect: AnalysisDialect,
    scoped: bool,
) -> Result<Option<(Directive, Disposition)>> {
    match dialect {
        AnalysisDialect::Simetrix => {
            let directive = match name {
                ".STEP" => Some(Directive::Sweep {
                    axis: SweepAxis::simetrix_step(&arguments(statement)?)?,
                }),
                ".DC" => {
                    let args = arguments(statement)?;
                    // Statistical selection owns the original unbound MONTE form.
                    if simetrix::sweep_start(AnalysisKind::DcSweep, &args)
                        .and_then(|index| args.get(index + 1))
                        .is_some_and(|field| field.eq_ignore_ascii_case("MONTE"))
                    {
                        None
                    } else {
                        Some(Directive::DcSweep {
                            axes: simetrix::dc_axes(&args)?,
                        })
                    }
                }
                _ => None,
            };
            Ok(directive.map(|directive| {
                (
                    directive,
                    if scoped {
                        Disposition::Unsupported
                    } else {
                        Disposition::RequiresBinding
                    },
                )
            }))
        }
        AnalysisDialect::Ltspice => ltspice_directive(name, statement, scoped),
        AnalysisDialect::Pspice => pspice_directive(name, statement, scoped),
        _ => Ok(None),
    }
}

fn classify(
    name: &str,
    statement: &str,
    dialect: AnalysisDialect,
    scoped: bool,
) -> Result<(Directive, Disposition)> {
    use Disposition::{Circuit, Initialization, Metadata, RequiresBinding, Unsupported};
    if let Some(directive) = dialect_directive(name, statement, dialect, scoped)? {
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
            | ".WORSTCASE"
            | ".DISTRIBUTION"
            | ".SAVEBIAS"
            | ".LOADBIAS"
    ) {
        return Ok((Directive::Unsupported, Unsupported));
    }
    if matches!(name, ".GRAPH" | ".LICENSE_FEATURE" | ".REQUIRED_VERSION") {
        return Ok((Directive::Metadata, Metadata));
    }
    let args = if dialect == AnalysisDialect::Pspice {
        pspice::arguments(statement)?
    } else {
        arguments(statement)?
    };
    if matches!(name, ".OPTIONS" | ".OPTION" | ".OPT") {
        return Ok((
            Directive::Options {
                settings: options(statement, dialect)?,
            },
            if scoped { Unsupported } else { Circuit },
        ));
    }
    if matches!(name, ".TEMP" | ".NODESET" | ".IC" | ".INIT") {
        let disposition = match (name, dialect) {
            (".INIT", AnalysisDialect::Ltspice | AnalysisDialect::Pspice) => Unsupported,
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
            | ".PROBE/CSDF"
            | ".WATCH"
            | ".MEAS"
            | ".MEASURE"
            | ".FOUR"
            | ".PLOT"
            | ".NODE_MAP"
    ) {
        return Ok((Directive::Observation { arguments: args }, RequiresBinding));
    }
    Ok(classify_request(name, args, dialect, scoped))
}

fn classify_request(
    name: &str,
    args: Vec<String>,
    dialect: AnalysisDialect,
    scoped: bool,
) -> (Directive, Disposition) {
    use Disposition::{NativeRequest, RequiresBinding, Unsupported};
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
    (
        Directive::Analysis {
            analysis,
            arguments: args,
            uic,
        },
        disposition,
    )
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
    let inline = if dialect == AnalysisDialect::Simetrix
        && matches!(
            analysis_kind(&name),
            AnalysisKind::DcSweep
                | AnalysisKind::Ac
                | AnalysisKind::Transient
                | AnalysisKind::Noise
                | AnalysisKind::TransferFunction
                | AnalysisKind::OperatingPoint
        ) {
        simetrix::inline_sweeps(analysis_kind(&name), &arguments(text)?)
            .with_context(|| format!("analysis sweep at {origin:?}"))?
    } else {
        None
    };
    let (mut directive, mut disposition) = if let Some(inline) = &inline {
        budget.charge(Resource::GeneratedNodes, inline.axes.len())?;
        let base = format!("{name} {}", inline.arguments.join(" "));
        classify(&name, &base, dialect, library || !scope.is_empty())
    } else {
        classify(&name, text, dialect, library || !scope.is_empty())
    }
    .with_context(|| format!("analysis declaration at {origin:?}"))?;
    let (independent_axes, parallelism_hint) = inline.map_or_else(
        || (Vec::new(), None),
        |inline| (inline.axes, inline.parallelism_hint),
    );
    if !independent_axes.is_empty() && disposition != Disposition::Unsupported {
        disposition = Disposition::RequiresBinding;
    }
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
        independent_axes,
        parallelism_hint,
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
    fn pspice_transient_operating_point_is_dialect_specific() -> Result<()> {
        let statement = ".TRAN/OP 1u 1m";
        let (directive, disposition) =
            classify(".TRAN/OP", statement, AnalysisDialect::Pspice, false)?;
        assert_eq!(disposition, Disposition::RequiresBinding);
        assert!(matches!(
            directive,
            Directive::Analysis {
                analysis: AnalysisKind::Transient,
                ..
            }
        ));
        for dialect in [
            AnalysisDialect::Native,
            AnalysisDialect::Ltspice,
            AnalysisDialect::Hspice,
            AnalysisDialect::Simetrix,
            AnalysisDialect::Simplis,
            AnalysisDialect::Tina,
            AnalysisDialect::Qspice,
            AnalysisDialect::Expanded,
        ] {
            let (_, disposition) = classify(".TRAN/OP", statement, dialect, false)?;
            assert_eq!(disposition, Disposition::Unsupported, "{dialect:?}");
        }
        Ok(())
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
        let sweeps =
            plan.bind_independent_sweeps(2, |_, _, text| parse_spice_constant(text), &budget)?;
        assert_eq!(sweeps.count(), 9);
        let mut axes = sweeps.axes();
        let axis = axes.next().context("missing duration sweep")?;
        assert_eq!(axis.entry.command, ".STEP");
        assert_eq!(
            axis.coordinates.value(0).map(f64::to_bits),
            Some(1e-3_f64.to_bits())
        );
        let axis = axes.next().context("missing temperature sweep")?;
        assert_eq!(axis.entry.command, ".TEMP");
        assert_eq!(
            axis.coordinates.value(0).map(f64::to_bits),
            Some(25_f64.to_bits())
        );
        assert_eq!(
            axis.coordinates.value(1).map(f64::to_bits),
            Some((-40_f64).to_bits())
        );
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
        assert!(
            timing
                .modes
                .contains(model_normalize_ngspice::ltspice::transient::TransientModifier::Startup)
                && !timing
                    .modes
                    .contains(model_normalize_ngspice::ltspice::transient::TransientModifier::Uic)
        );
        assert!(timing.native_timing().is_err());
        assert!(matches!(
            plan.entries[3].directive,
            Directive::Measurement { .. }
        ));
        Ok(())
    }
}
