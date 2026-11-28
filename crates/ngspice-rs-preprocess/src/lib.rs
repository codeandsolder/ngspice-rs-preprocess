//! Unified source preparation. The caller explicitly selects the dialect and
//! artifact kind; this crate does not run an analysis or infer a file's dialect.

pub mod analysis_plan;
mod dialect;

use anyhow::{Context, Result, ensure};
use model_normalize_ngspice::{
    MappedNormalization, NormalizationPass,
    linked_source::{
        LinkLimits, LinkedSource, RootKind, SourceDialect, SourceLinker, SourceOrigin,
    },
    normalize_bound_mapped,
};
pub use model_normalize_ngspice::{
    linked_source::{LinkedLocation, SourceGraph},
    passive_ic, passive_storage, pwl_resistor, waveform,
};
pub use model_preprocess_command::{
    AdmissionConfig, AdmittedSource, Cancellation, CommandConfig, ProcessLimits, Resource,
    SourceSession, WorkBudget, WorkLimits,
};
pub use model_preprocess_simplis::{LibrarySimulator, ParsedLibrary, source_dialect::PowerTarget};
pub use model_translate_simplis::CodeModelRuntime;
use serde::Serialize;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

/// Input dialect and desired artifact, chosen explicitly by the caller.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum InputFormat {
    /// Complete native-compatible deck; preserve title and analysis commands.
    NativeDeck,
    /// Native-compatible library; emit include-safe text.
    NativeLibrary,
    /// Standalone HSPICE deck lowered to native SPICE.
    HspiceDeck,
    /// Parse a standalone HSPICE source library to a validated JSON artifact.
    HspiceLibrary,
    /// TINA plaintext SPICE deck.
    TinaDeck,
    /// TINA plaintext SPICE library; emit include-safe native SPICE.
    TinaLibrary,
    /// Complete `LTspice` source deck, emitted in native syntax.
    LtspiceDeck,
    /// Headless `LTspice` ASCII schematic and its symbol/hierarchy graph.
    LtspiceSchematic,
    /// `LTspice` source library, emitted as include-safe native syntax.
    LtspiceLibrary,
    /// Standalone `PSpice` deck lowered to native SPICE.
    PspiceDeck,
    /// Standalone `PSpice` library lowered to include-safe native SPICE.
    PspiceLibrary,
    /// QSPICE plaintext circuit deck.
    QspiceDeck,
    /// QSPICE plaintext model library.
    QspiceLibrary,
    /// SIMPLIS source deck with optional library catalog.
    SimplisDeck,
    /// `SIMetrix` source deck with optional library catalog.
    SimetrixDeck,
    /// Already-expanded dialect deck; no hierarchy specialization.
    ExpandedDeck,
    /// Already-expanded dialect library; no hierarchy specialization.
    ExpandedLibrary,
    /// Parse a SIMPLIS source library to a validated JSON artifact.
    SimplisLibrary,
    /// Parse a `SIMetrix` source library to a validated JSON artifact.
    SimetrixLibrary,
}

impl InputFormat {
    const fn is_hspice(self) -> bool {
        matches!(self, Self::HspiceDeck | Self::HspiceLibrary)
    }

    /// Parse the explicit CLI spelling.
    ///
    /// # Errors
    /// Unknown format.
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "native-deck" => Ok(Self::NativeDeck),
            "native-library" => Ok(Self::NativeLibrary),
            "hspice-deck" => Ok(Self::HspiceDeck),
            "hspice-library" => Ok(Self::HspiceLibrary),
            "tina-deck" => Ok(Self::TinaDeck),
            "tina-library" => Ok(Self::TinaLibrary),
            "ltspice-deck" => Ok(Self::LtspiceDeck),
            "ltspice-schematic" => Ok(Self::LtspiceSchematic),
            "ltspice-library" => Ok(Self::LtspiceLibrary),
            "pspice-deck" => Ok(Self::PspiceDeck),
            "pspice-library" => Ok(Self::PspiceLibrary),
            "qspice-deck" => Ok(Self::QspiceDeck),
            "qspice-library" => Ok(Self::QspiceLibrary),
            "simplis-deck" => Ok(Self::SimplisDeck),
            "simetrix-deck" => Ok(Self::SimetrixDeck),
            "expanded-deck" => Ok(Self::ExpandedDeck),
            "expanded-library" => Ok(Self::ExpandedLibrary),
            "simplis-library" => Ok(Self::SimplisLibrary),
            "simetrix-library" => Ok(Self::SimetrixLibrary),
            _ => anyhow::bail!("unknown input format {value:?}"),
        }
    }
}

/// Raw source supplied by path or by bytes with an explicit origin.
#[derive(Clone, Debug)]
pub enum SourceInput {
    /// Read and admit a source file.
    File(PathBuf),
    /// Admit caller-owned bytes, resolving relative references from the origin.
    Bytes {
        /// Original filename (the parent must exist).
        origin: PathBuf,
        /// Original source bytes.
        bytes: Arc<[u8]>,
    },
}

impl SourceInput {
    fn admit(&self, session: &mut SourceSession) -> Result<AdmittedSource> {
        match self {
            Self::File(path) => session.load(path),
            Self::Bytes { origin, bytes } => session.admit_bytes(origin, Arc::clone(bytes)),
        }
    }
}

/// A catalog member, with raw sources and compiled artifacts kept distinct.
#[derive(Clone, Debug)]
pub enum LibraryInput {
    /// A source requiring admission and parsing.
    Source(SourceInput),
    /// A previously compiled parsed-library JSON file, never transformed.
    ParsedFile(PathBuf),
    /// A previously compiled parsed-library value, never transformed.
    Parsed(ParsedLibrary),
}

/// One complete source graph preparation request.
#[derive(Clone, Debug)]
pub struct Request {
    /// Explicit dialect and output kind.
    pub format: InputFormat,
    /// Root source.
    pub input: SourceInput,
    /// Explicit catalog members for source decks.
    pub libraries: Vec<LibraryInput>,
    /// Directories searched after the containing directory for source includes.
    pub search_paths: Vec<PathBuf>,
}

/// Whether the consumer permits an equivalent internal-current coordinate change.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ConditioningPolicy {
    /// Retain the original internal current coordinates.
    Disabled,
    /// Scale electrically isolated differentiator loops.
    #[default]
    PrivateDifferentiators,
}

/// Explicit deterministic statistical sample selection.
pub use model_statistics::Realization as StatisticalSample;

/// Admission, conditioning and artifact publication options.
#[derive(Clone, Debug)]
pub struct Options {
    /// Optional source command and independent per-source limits.
    pub admission: AdmissionConfig,
    /// Ceiling on linked intermediate bytes and combined final artifact bytes.
    pub output_bytes: usize,
    /// Shared cumulative work ceilings across the entire request.
    pub work: WorkLimits,
    /// Disable conditioning when the consumer adds artificial topology
    /// (such as global shunts) or requires unchanged internal current names.
    pub conditioning: ConditioningPolicy,
    /// Explicit request-local device configuration file for `SIMetrix` decks.
    /// It is admitted as data, without the source transformation command.
    pub device_config: Option<PathBuf>,
    /// Explicit experiment seed and sample; absent means nominal.
    pub statistical_sample: Option<StatisticalSample>,
    /// Explicit consumer power-expression contract.
    pub power_target: PowerTarget,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            admission: AdmissionConfig::default(),
            output_bytes: 128 * 1024 * 1024,
            work: WorkLimits::default(),
            conditioning: ConditioningPolicy::default(),
            device_config: None,
            statistical_sample: None,
            power_target: PowerTarget::NativeNgspice,
        }
    }
}

/// Source path and admitted physical line.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct OriginLine {
    /// Exact original source path. JSON uses its display spelling.
    #[serde(serialize_with = "serialize_path")]
    pub source: PathBuf,
    /// One-based physical line in the admitted source, or zero for a generated dialect default.
    pub line: usize,
    /// Inclusive end of the admitted physical source span, or zero for a generated dialect default.
    pub last_line: usize,
    /// Admitted source occurrence and lexical scope, when linked directly.
    pub linked: Option<LinkedLocation>,
}

/// Provenance of an artifact. Generated dialect output is not falsely given
/// a one-to-one line map when specialization changes its structure.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Provenance {
    /// One original location per emitted physical line.
    Native {
        /// Exact ordered line mapping.
        lines: Vec<OriginLine>,
        /// Graph underlying the line map for directly linked artifacts.
        source_graph: Option<SourceGraph>,
    },
    /// Generated source or parsed library.
    Generated {
        /// Original source identities, in stable order.
        sources: Vec<String>,
        /// Content-addressed library identity, if applicable.
        source_id: Option<String>,
    },
}

/// Artifact bytes, portable filename and source provenance.
#[derive(Clone, Debug)]
pub struct Artifact {
    /// Filename relative to the bundle's directory.
    pub filename: String,
    /// Exact artifact bytes.
    pub bytes: Vec<u8>,
    /// Origin information.
    pub provenance: Provenance,
    /// Companion library identities required by this artifact.
    pub dependencies: Vec<String>,
    /// Supplemental code-model schemas introduced by this translation.
    /// Native input is preserved without inferring arbitrary external models.
    pub required_code_models: Vec<CodeModelRuntime>,
    /// Internal current-coordinate changes; original branch current is the
    /// emitted current divided by the recorded factor.
    pub current_scalings: Vec<model_normalize_ngspice::conditioning::CurrentScaling>,
}

/// Deterministic primary artifact and any generated companion libraries.
#[derive(Clone, Debug)]
pub struct PreparedBundle {
    /// Explicit input format.
    pub format: InputFormat,
    /// Power-expression contract required by the emitted artifact.
    pub power_target: PowerTarget,
    /// Canonical root origin.
    pub origin: PathBuf,
    /// Main artifact (native deck, include, or parsed-library JSON).
    pub primary: Artifact,
    /// Generated companions, in stable order.
    pub companions: Vec<Artifact>,
    /// Native compatibility normalization passes actually applied.
    pub normalization: Vec<NormalizationPass>,
    /// Source declaration identities, before native primitive lowering or
    /// subcircuit specialization. Unused catalog declarations may be present.
    pub device_identities: Vec<model_preprocess_simplis::source_dialect::DeviceIdentity>,
    /// Source model declarations and selected physical implementation, before
    /// specialization. Unused catalog declarations may be present.
    pub model_selections:
        Vec<model_preprocess_simplis::source_dialect::model_mapping::ModelSelection>,
    /// Source-specific initialization policy; absent for native decks.
    /// Consumers must bind it per instance and enclosing analysis.
    pub initial_conditions: Option<passive_ic::InitialConditions>,
    /// Original dialect analysis declarations. Pure native decks retain executable cards;
    /// mixed source decks with extracted analysis require consumer binding.
    pub analysis_plan: Option<analysis_plan::AnalysisPlan>,
    /// Mandatory named independent-source forcing, bound once per
    /// elaborated occurrence and parameter candidate. Electrical cards
    /// retain the source identity but may contain only a DC placeholder.
    pub waveforms: Vec<waveform::PreparedWaveform>,
    /// Exact resistor laws with occurrence-local advisory startup segments.
    /// A consumer must bind the emitted branch anchors before solving.
    pub pwl_resistors: Vec<pwl_resistor::PreparedPwlResistor>,
}

impl PreparedBundle {
    /// Iterate primary first, then companions in deterministic order.
    pub fn artifacts(&self) -> impl Iterator<Item = &Artifact> {
        std::iter::once(&self.primary).chain(&self.companions)
    }

    /// Unique supplemental model runtimes required across the closed bundle.
    #[must_use = "the result describes the prepared bundle runtime requirements"]
    pub fn required_code_models(&self) -> Vec<CodeModelRuntime> {
        let mut runtimes = self
            .artifacts()
            .flat_map(|artifact| artifact.required_code_models.iter().copied())
            .collect::<Vec<_>>();
        runtimes.sort_unstable();
        runtimes.dedup();
        runtimes
    }
}

fn serialize_path<S: serde::Serializer>(
    path: &Path,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    serializer.serialize_str(&path.to_string_lossy())
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

struct NativeProjection {
    normalized: MappedNormalization,
    device_identities: Vec<model_preprocess_simplis::source_dialect::DeviceIdentity>,
    waveforms: Vec<waveform::PreparedWaveform>,
}

impl NativeProjection {
    fn bound(
        bytes: &[u8],
        kind: RootKind,
        dialects: &[SourceDialect],
        budget: &WorkBudget,
    ) -> Result<Self> {
        Ok(Self {
            normalized: normalize_bound_mapped(bytes, kind, dialects, budget)?,
            device_identities: Vec::new(),
            waveforms: Vec::new(),
        })
    }
}

fn project_native(
    linked: &LinkedSource,
    kind: RootKind,
    budget: &WorkBudget,
) -> Result<NativeProjection> {
    let normalized = model_normalize_ngspice::normalize_linked(linked, kind, budget)?;
    let identities = normalized
        .renamed
        .into_iter()
        .map(|device| {
            let origin = linked
                .origin(device.input_line)
                .context("missing renamed device origin")?;
            budget.charge(Resource::GeneratedNodes, 1)?;
            budget.charge(
                Resource::EmittedBytes,
                origin
                    .path
                    .as_os_str()
                    .len()
                    .saturating_add(device.declared.len())
                    .saturating_add(device.declared.len())
                    .saturating_add(device.linked.len()),
            )?;
            for part in &device.scope {
                budget.charge(Resource::EmittedBytes, part.len())?;
            }
            Ok(model_preprocess_simplis::source_dialect::DeviceIdentity {
                source: display_path(&origin.path),
                scope: device.scope,
                displayed: device.declared.clone(),
                declared: device.declared,
                linked: device.linked,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let mut mapped = normalized.mapped;
    let waveforms = std::mem::take(&mut mapped.waveforms);
    Ok(NativeProjection {
        normalized: mapped,
        device_identities: identities,
        waveforms,
    })
}

fn native(
    root: &AdmittedSource,
    request: &Request,
    options: &Options,
    session: &mut SourceSession,
) -> Result<PreparedBundle> {
    let kind = if matches!(
        request.format,
        InputFormat::NativeDeck
            | InputFormat::LtspiceDeck
            | InputFormat::LtspiceSchematic
            | InputFormat::PspiceDeck
            | InputFormat::QspiceDeck
            | InputFormat::TinaDeck
    ) {
        RootKind::Deck
    } else {
        RootKind::Include
    };
    let origin = root.path.clone();
    let imported = if request.format == InputFormat::LtspiceSchematic {
        let deck = model_import_ltspice::import(
            root,
            &request.search_paths,
            session,
            options.output_bytes,
        )?;
        Some((
            AdmittedSource {
                path: origin.clone(),
                bytes: Arc::from(deck.bytes),
            },
            deck.origins,
        ))
    } else {
        None
    };
    let root = imported.as_ref().map_or(root, |(source, _)| source);
    if matches!(
        request.format,
        InputFormat::TinaDeck | InputFormat::TinaLibrary
    ) {
        model_normalize_ngspice::tina::validate_plaintext(&root.bytes)?;
    }
    let syntax = match request.format {
        InputFormat::LtspiceDeck | InputFormat::LtspiceLibrary | InputFormat::LtspiceSchematic => {
            SourceDialect::Ltspice
        }
        InputFormat::PspiceDeck | InputFormat::PspiceLibrary => SourceDialect::Pspice,
        InputFormat::TinaDeck | InputFormat::TinaLibrary => SourceDialect::Tina,
        InputFormat::QspiceDeck | InputFormat::QspiceLibrary => SourceDialect::Qspice,
        _ => SourceDialect::Native,
    };
    let mut linked = SourceLinker {
        search_paths: request.search_paths.clone(),
        dialect: syntax,
    }
    .link_admitted(
        root,
        kind,
        LinkLimits {
            output_bytes: options.output_bytes,
        },
        session,
    )?;
    let origins = linked
        .origins
        .iter()
        .cloned()
        .map(|mapped| {
            linked_origin(
                mapped,
                &origin,
                imported.as_ref().map(|(_, origins)| origins.as_slice()),
            )
        })
        .collect::<Result<Vec<_>>>()?;
    let analysis_plan = analysis_plan::source_capture::capture_linked(
        &mut linked,
        kind,
        &origins,
        session.budget(),
    )?;
    let projected = project_native(&linked, kind, session.budget())?;
    let mut bundle = native_bundle(
        origin,
        request.format,
        kind,
        projected,
        &origins,
        Some(SourceGraph {
            visits: linked.visits,
            scopes: linked.scopes,
        }),
        session.budget(),
    )?;
    bundle.analysis_plan = analysis_plan;
    Ok(bundle)
}

fn linked_origin(
    mapped: SourceOrigin,
    root: &Path,
    schematic: Option<&[model_import_ltspice::Origin]>,
) -> Result<OriginLine> {
    let (source, line) = if let Some(origins) = schematic
        && mapped.path == root
    {
        let original = origins
            .get(
                mapped
                    .line
                    .checked_sub(1)
                    .context("invalid schematic source line")?,
            )
            .context("missing schematic source origin")?;
        (original.path.clone(), original.line)
    } else {
        (mapped.path, mapped.line)
    };
    Ok(OriginLine {
        source,
        line,
        last_line: line,
        linked: Some(LinkedLocation {
            visit: mapped.visit,
            scope: mapped.scope,
        }),
    })
}

fn native_bundle(
    origin: PathBuf,
    format: InputFormat,
    kind: RootKind,
    projection: NativeProjection,
    origins: &[OriginLine],
    source_graph: Option<SourceGraph>,
    budget: &WorkBudget,
) -> Result<PreparedBundle> {
    let NativeProjection {
        normalized,
        device_identities,
        waveforms,
    } = projection;
    let lines = normalized
        .input_lines
        .into_iter()
        .map(|line| {
            let mapped = origins
                .get(
                    line.checked_sub(1)
                        .context("invalid linked source location")?,
                )
                .context("missing linked source location")?;
            budget.charge(Resource::GeneratedNodes, 1)?;
            budget.charge(Resource::EmittedBytes, mapped.source.as_os_str().len())?;
            Ok(mapped.clone())
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(PreparedBundle {
        format,
        power_target: PowerTarget::NativeNgspice,
        origin,
        primary: Artifact {
            filename: if kind == RootKind::Deck {
                "deck.cir"
            } else {
                "model.lib"
            }
            .to_owned(),
            bytes: normalized.model.bytes,
            provenance: Provenance::Native {
                lines,
                source_graph,
            },
            dependencies: Vec::new(),
            required_code_models: Vec::new(),
            current_scalings: Vec::new(),
        },
        companions: Vec::new(),
        normalization: normalized.model.passes,
        device_identities,
        model_selections: Vec::new(),
        initial_conditions: None,
        analysis_plan: None,
        waveforms,
        pwl_resistors: Vec::new(),
    })
}

fn validate_request(request: &Request) -> Result<()> {
    ensure!(
        request.libraries.is_empty()
            || matches!(
                request.format,
                InputFormat::SimplisDeck | InputFormat::SimetrixDeck | InputFormat::HspiceDeck
            ),
        "catalog libraries require a source deck format"
    );
    ensure!(
        request.search_paths.is_empty()
            || matches!(
                request.format,
                InputFormat::NativeDeck
                    | InputFormat::NativeLibrary
                    | InputFormat::HspiceDeck
                    | InputFormat::HspiceLibrary
                    | InputFormat::TinaDeck
                    | InputFormat::TinaLibrary
                    | InputFormat::LtspiceDeck
                    | InputFormat::LtspiceSchematic
                    | InputFormat::LtspiceLibrary
                    | InputFormat::PspiceDeck
                    | InputFormat::PspiceLibrary
                    | InputFormat::QspiceDeck
                    | InputFormat::QspiceLibrary
                    | InputFormat::SimetrixDeck
                    | InputFormat::SimetrixLibrary
            ),
        "search paths require a source-linking input format"
    );
    Ok(())
}

/// Prepare one graph with a fresh source-admission session. Each admitted
/// identity is transformed once. The function returns no partial bundle on
/// failure, and never writes output files.
///
/// # Errors
///
/// Invalid request, admission/link/translation errors, cancellation, or
/// artifact output limit exceeded.
pub fn prepare(
    request: &Request,
    options: &Options,
    cancellation: &Cancellation,
) -> Result<PreparedBundle> {
    validate_request(request)?;
    ensure!(
        options.power_target == PowerTarget::NativeNgspice
            || request.format == InputFormat::SimetrixDeck,
        "prepared power target requires a SIMetrix deck"
    );
    ensure!(
        options.device_config.is_none() || request.format == InputFormat::SimetrixDeck,
        "device configuration is only valid for SIMetrix decks"
    );
    ensure!(
        options.statistical_sample.is_none() || request.format == InputFormat::SimetrixDeck,
        "statistical sample selection is only valid for SIMetrix decks"
    );
    ensure!(!cancellation.is_cancelled(), "source processing cancelled");
    let budget = WorkBudget::new(options.work, cancellation.clone());
    let mut session = SourceSession::with_budget(
        options.admission.clone(),
        cancellation.clone(),
        budget.clone(),
    );
    let root = request.input.admit(&mut session)?;
    let mut result = match request.format {
        InputFormat::NativeDeck
        | InputFormat::NativeLibrary
        | InputFormat::TinaDeck
        | InputFormat::TinaLibrary
        | InputFormat::LtspiceDeck
        | InputFormat::LtspiceSchematic
        | InputFormat::LtspiceLibrary
        | InputFormat::PspiceDeck
        | InputFormat::PspiceLibrary
        | InputFormat::QspiceDeck
        | InputFormat::QspiceLibrary => native(&root, request, options, &mut session),
        _ => dialect::prepare(&root, request, options, cancellation, &mut session),
    }?;
    result.power_target = options.power_target;
    if request.format == InputFormat::SimetrixDeck
        && options.conditioning == ConditioningPolicy::PrivateDifferentiators
    {
        condition_generated_bundle(&mut result, &budget)?;
    }
    ensure!(!cancellation.is_cancelled(), "source processing cancelled");
    let artifact_names = result
        .artifacts()
        .map(|artifact| artifact.filename.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    for declaration in &result.waveforms {
        budget.charge(Resource::EvaluationSteps, 1)?;
        ensure!(
            artifact_names.contains(declaration.artifact.as_str()),
            "missing waveform artifact {}",
            declaration.artifact
        );
    }
    for declaration in &result.pwl_resistors {
        budget.charge(Resource::EvaluationSteps, 1)?;
        ensure!(
            artifact_names.contains(declaration.artifact.as_str()),
            "missing PWL resistor artifact {}",
            declaration.artifact
        );
    }
    budget.check()?;
    let mut remaining = options.output_bytes;
    for artifact in result.artifacts() {
        budget.charge(Resource::EmittedBytes, artifact.bytes.len())?;
        pwl_resistor::validate_artifact(
            &result.pwl_resistors,
            &artifact.filename,
            &artifact.bytes,
            &budget,
        )?;
        waveform::validate_artifact(
            &result.waveforms,
            &artifact.filename,
            &artifact.bytes,
            &budget,
        )?;
        remaining = remaining
            .checked_sub(artifact.bytes.len())
            .context("combined artifact output byte budget exceeded")?;
    }
    Ok(result)
}

fn condition_generated_bundle(bundle: &mut PreparedBundle, budget: &WorkBudget) -> Result<()> {
    use model_normalize_ngspice::conditioning::{
        ConditioningInput, condition_private_differentiators_with_budget,
    };
    let inputs = bundle
        .artifacts()
        .map(|artifact| ConditioningInput {
            filename: &artifact.filename,
            bytes: &artifact.bytes,
        })
        .collect::<Vec<_>>();
    let outputs = condition_private_differentiators_with_budget(
        &inputs,
        bundle.initial_conditions.as_ref(),
        &bundle.waveforms,
        budget,
    )?;
    let mut changed = false;
    for (artifact, output) in std::iter::once(&mut bundle.primary)
        .chain(&mut bundle.companions)
        .zip(outputs)
    {
        changed |= !output.scalings.is_empty();
        artifact.bytes = output.bytes;
        artifact.current_scalings = output.scalings;
    }
    if changed {
        bundle
            .normalization
            .push(NormalizationPass::PrivateDifferentiatorConditioning);
    }
    Ok(())
}
