//! Unified source preparation. The caller explicitly selects the dialect and
//! artifact kind; this crate does not run an analysis or infer a file's dialect.

pub mod analysis_plan;
mod dialect;

use anyhow::{Context, Result, ensure};
use model_normalize_ngspice::{
    MappedNormalization, NormalizationPass,
    linked_source::{LibrarySyntax, LinkLimits, LinkedSource, RootKind, SourceLinker},
    normalize_mapped,
};
pub use model_normalize_ngspice::{passive_ic, waveform};
pub use model_preprocess_command::{
    AdmissionConfig, AdmittedSource, Cancellation, CommandConfig, ProcessLimits, Resource,
    SourceSession, WorkBudget, WorkLimits,
};
pub use model_preprocess_simplis::{LibrarySimulator, ParsedLibrary};
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
    /// TINA plaintext SPICE deck.
    TinaDeck,
    /// TINA plaintext SPICE library; emit include-safe native SPICE.
    TinaLibrary,
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
    /// Parse the explicit CLI spelling.
    ///
    /// # Errors
    /// Unknown format.
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "native-deck" => Ok(Self::NativeDeck),
            "native-library" => Ok(Self::NativeLibrary),
            "tina-deck" => Ok(Self::TinaDeck),
            "tina-library" => Ok(Self::TinaLibrary),
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
    /// Explicit catalog members (only for SIMPLIS/SIMetrix source decks).
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
}

impl Default for Options {
    fn default() -> Self {
        Self {
            admission: AdmissionConfig::default(),
            output_bytes: 128 * 1024 * 1024,
            work: WorkLimits::default(),
            conditioning: ConditioningPolicy::default(),
            device_config: None,
        }
    }
}

/// Source path and admitted physical line.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct OriginLine {
    /// Exact original source path. JSON uses its display spelling.
    #[serde(serialize_with = "serialize_path")]
    pub source: PathBuf,
    /// One-based physical line in the admitted source.
    pub line: usize,
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
    /// Original dialect analysis declarations; native decks retain executable cards.
    pub analysis_plan: Option<analysis_plan::AnalysisPlan>,
    /// Mandatory named independent-source forcing, bound once per
    /// elaborated occurrence and parameter candidate. Electrical cards
    /// retain the source identity but may contain only a DC placeholder.
    pub waveforms: Vec<waveform::PreparedWaveform>,
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

fn project_native(
    linked: &LinkedSource,
    kind: RootKind,
    format: InputFormat,
    budget: &WorkBudget,
) -> Result<NativeProjection> {
    match format {
        InputFormat::PspiceDeck
        | InputFormat::PspiceLibrary
        | InputFormat::TinaDeck
        | InputFormat::TinaLibrary => {
            let normalized = if matches!(format, InputFormat::TinaDeck | InputFormat::TinaLibrary) {
                model_normalize_ngspice::tina::normalize_mapped(&linked.bytes, kind, budget)?
            } else {
                model_normalize_ngspice::pspice::normalize_mapped(&linked.bytes, kind, budget)?
            };
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
            Ok(NativeProjection {
                normalized: normalized.mapped,
                device_identities: identities,
                waveforms: Vec::new(),
            })
        }
        InputFormat::QspiceDeck | InputFormat::QspiceLibrary => {
            let lowered = model_normalize_ngspice::qspice::lower_linked(linked, kind, budget)?;
            let mut normalized = normalize_mapped(
                lowered.bytes.as_deref().unwrap_or(&linked.bytes),
                kind,
                budget,
            )?;
            if lowered.bytes.is_some() {
                normalized
                    .model
                    .passes
                    .insert(0, NormalizationPass::QspiceSource);
            }
            Ok(NativeProjection {
                normalized,
                device_identities: Vec::new(),
                waveforms: lowered.waveforms,
            })
        }
        _ => Ok(NativeProjection {
            normalized: normalize_mapped(&linked.bytes, kind, budget)?,
            device_identities: Vec::new(),
            waveforms: Vec::new(),
        }),
    }
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
            | InputFormat::PspiceDeck
            | InputFormat::QspiceDeck
            | InputFormat::TinaDeck
    ) {
        RootKind::Deck
    } else {
        RootKind::Include
    };
    let origin = root.path.clone();
    if matches!(
        request.format,
        InputFormat::TinaDeck | InputFormat::TinaLibrary
    ) {
        model_normalize_ngspice::tina::validate_plaintext(&root.bytes)?;
    }
    let syntax = match request.format {
        InputFormat::PspiceDeck
        | InputFormat::PspiceLibrary
        | InputFormat::TinaDeck
        | InputFormat::TinaLibrary => LibrarySyntax::Pspice,
        InputFormat::QspiceDeck | InputFormat::QspiceLibrary => LibrarySyntax::Qspice,
        _ => LibrarySyntax::NamedSections,
    };
    let linked = SourceLinker {
        search_paths: request.search_paths.clone(),
        library_syntax: syntax,
    }
    .link_admitted(
        root,
        kind,
        LinkLimits {
            output_bytes: options.output_bytes,
        },
        session,
    )?;
    let NativeProjection {
        normalized,
        device_identities,
        waveforms,
    } = project_native(&linked, kind, request.format, session.budget())?;
    let lines = normalized
        .input_lines
        .into_iter()
        .map(|line| {
            let mapped = linked
                .origin(line)
                .context("missing linked source location")?;
            session.budget().charge(Resource::GeneratedNodes, 1)?;
            session
                .budget()
                .charge(Resource::EmittedBytes, mapped.path.as_os_str().len())?;
            Ok(OriginLine {
                source: mapped.path.clone(),
                line: mapped.line,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(PreparedBundle {
        format: request.format,
        origin,
        primary: Artifact {
            filename: if kind == RootKind::Deck {
                "deck.cir"
            } else {
                "model.lib"
            }
            .to_owned(),
            bytes: normalized.model.bytes,
            provenance: Provenance::Native { lines },
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
    })
}

fn validate_request(request: &Request) -> Result<()> {
    ensure!(
        request.libraries.is_empty()
            || matches!(
                request.format,
                InputFormat::SimplisDeck | InputFormat::SimetrixDeck
            ),
        "catalog libraries require simplis-deck or simetrix-deck"
    );
    ensure!(
        request.search_paths.is_empty()
            || matches!(
                request.format,
                InputFormat::NativeDeck
                    | InputFormat::NativeLibrary
                    | InputFormat::TinaDeck
                    | InputFormat::TinaLibrary
                    | InputFormat::PspiceDeck
                    | InputFormat::PspiceLibrary
                    | InputFormat::QspiceDeck
                    | InputFormat::QspiceLibrary
                    | InputFormat::SimetrixDeck
                    | InputFormat::SimetrixLibrary
            ),
        "search paths require native, QSPICE or SIMetrix source linking"
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
        options.device_config.is_none() || request.format == InputFormat::SimetrixDeck,
        "device configuration is only valid for SIMetrix decks"
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
        | InputFormat::PspiceDeck
        | InputFormat::PspiceLibrary
        | InputFormat::QspiceDeck
        | InputFormat::QspiceLibrary => native(&root, request, options, &mut session),
        _ => dialect::prepare(&root, request, options, cancellation, &mut session),
    }?;
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
    budget.check()?;
    let mut remaining = options.output_bytes;
    for artifact in result.artifacts() {
        budget.charge(Resource::EmittedBytes, artifact.bytes.len())?;
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
