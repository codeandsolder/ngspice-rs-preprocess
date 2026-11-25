//! Shared dialect source preparation and artifact construction.
use super::{
    AdmittedSource, Artifact, Cancellation, CodeModelRuntime, InputFormat, LibraryInput,
    LibrarySimulator, Options, ParsedLibrary, PreparedBundle, Provenance, Request, SourceSession,
    display_path,
};
use anyhow::{Context, Result, bail, ensure};
use model_preprocess_command::AdmissionConfig;
use model_preprocess_simplis::{
    SubcircuitCatalog, parse_library_for_simulator, preprocess_netlist_linked, project_source,
    source_include_target,
};
use model_translate_simplis::{
    lower_simetrix_linked_runtime, translate_simplis_deck, translate_simplis_expanded_library,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

const fn projection(format: InputFormat) -> LibrarySimulator {
    if matches!(
        format,
        InputFormat::SimetrixDeck | InputFormat::SimetrixLibrary
    ) {
        LibrarySimulator::Simetrix
    } else {
        LibrarySimulator::Simplis
    }
}

const fn generated(
    filename: String,
    bytes: Vec<u8>,
    sources: Vec<String>,
    source_id: Option<String>,
    dependencies: Vec<String>,
    required_code_models: Vec<CodeModelRuntime>,
) -> Artifact {
    Artifact {
        filename,
        bytes,
        provenance: Provenance::Generated { sources, source_id },
        dependencies,
        required_code_models,
    }
}

fn bundle(
    root: &AdmittedSource,
    format: InputFormat,
    primary: Artifact,
    companions: Vec<Artifact>,
) -> PreparedBundle {
    PreparedBundle {
        format,
        origin: root.path.clone(),
        primary,
        companions,
        normalization: Vec::new(),
    }
}

fn parse_root(root: &AdmittedSource, format: InputFormat) -> Result<PreparedBundle> {
    let name = display_path(&root.path);
    let parsed = parse_library_for_simulator(&name, &root.bytes, projection(format))?;
    let artifact = generated(
        "library.json".to_owned(),
        parsed.to_json()?,
        vec![name],
        Some(parsed.source_id().to_owned()),
        Vec::new(),
        Vec::new(),
    );
    Ok(bundle(root, format, artifact, Vec::new()))
}

fn resolve_init(session: &SourceSession, parent: &Path, target: &Path) -> Result<PathBuf> {
    if target.is_absolute() {
        return Ok(session.resolve_path(target)?);
    }
    let base = parent.parent().context("source has no parent directory")?;
    let mut matches = BTreeSet::new();
    for ancestor in base.ancestors() {
        let candidate = ancestor.join(target);
        match session.resolve_path(&candidate) {
            Ok(path) => {
                matches.insert(path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("resolving init snapshot {}", candidate.display()));
            }
        }
    }
    ensure!(
        matches.len() == 1,
        "missing or ambiguous init snapshot {} from {}: {matches:?}",
        target.display(),
        parent.display()
    );
    matches
        .into_iter()
        .next()
        .context("init snapshot is missing")
}

struct InitExpansion<'a> {
    session: &'a mut SourceSession,
    output: Vec<u8>,
    sources: BTreeSet<String>,
    stack: Vec<PathBuf>,
    limit: usize,
    simulator: LibrarySimulator,
}

impl InitExpansion<'_> {
    fn append(&mut self, bytes: &[u8]) -> Result<()> {
        ensure!(
            bytes.len() <= self.limit.saturating_sub(self.output.len()),
            "init expansion byte budget exceeded"
        );
        self.output.extend_from_slice(bytes);
        Ok(())
    }

    fn expand(&mut self, source: &AdmittedSource) -> Result<()> {
        ensure!(self.stack.len() < 128, "init include depth exceeded");
        ensure!(
            !self.stack.contains(&source.path),
            "recursive init include {}",
            source.path.display()
        );
        self.stack.push(source.path.clone());
        self.sources.insert(display_path(&source.path));
        let result = self.expand_lines(source);
        self.stack.pop();
        result
    }

    fn expand_lines(&mut self, source: &AdmittedSource) -> Result<()> {
        let projected = project_source(&source.bytes, self.simulator)
            .with_context(|| format!("selecting simulator view of {}", source.path.display()))?;
        for line in projected.split_inclusive('\n') {
            if let Some(target) = source_include_target(line)? {
                let normalized = target.replace('\\', "/");
                let target = Path::new(&normalized);
                if target
                    .extension()
                    .and_then(|value| value.to_str())
                    .is_some_and(|value| value.eq_ignore_ascii_case("init"))
                {
                    let path = resolve_init(self.session, &source.path, target)?;
                    let snapshot = self.session.load(&path)?;
                    self.expand(&snapshot)?;
                    continue;
                }
            }
            self.append(line.as_bytes())?;
            if !line.ends_with('\n') {
                self.append(b"\n")?;
            }
        }
        Ok(())
    }
}

struct Catalog {
    catalog: SubcircuitCatalog,
    sources: BTreeMap<String, BTreeSet<String>>,
}

fn catalog(
    request: &Request,
    options: &Options,
    cancellation: &Cancellation,
    session: &mut SourceSession,
) -> Result<Catalog> {
    let simulator = projection(request.format);
    let mut parsed_session = SourceSession::new(
        AdmissionConfig {
            command: None,
            ..options.admission.clone()
        },
        cancellation.clone(),
    );
    let mut catalog = SubcircuitCatalog::for_simulator(simulator);
    let mut sources = BTreeMap::<String, BTreeSet<String>>::new();
    for library in &request.libraries {
        let (parsed, origin) = match library {
            LibraryInput::Source(input) => {
                let admitted = input.admit(session)?;
                let origin = display_path(&admitted.path);
                (
                    parse_library_for_simulator(&origin, &admitted.bytes, simulator)?,
                    origin,
                )
            }
            LibraryInput::ParsedFile(path) => {
                let admitted = parsed_session.load(path)?;
                (
                    ParsedLibrary::from_json(&admitted.bytes)?,
                    display_path(&admitted.path),
                )
            }
            LibraryInput::Parsed(parsed) => (parsed.clone(), parsed.source_name().to_owned()),
        };
        ensure!(
            parsed.simulator() == simulator,
            "catalog member {origin} uses {} projection, requested {}",
            parsed.simulator().as_str(),
            simulator.as_str()
        );
        catalog.add_parsed_library(&parsed)?;
        sources
            .entry(parsed.source_id().to_owned())
            .or_default()
            .insert(origin);
    }
    Ok(Catalog { catalog, sources })
}

fn check_closed_includes(bytes: &[u8], known: &BTreeSet<&str>) -> Result<()> {
    let text = std::str::from_utf8(bytes)?;
    for line in text.lines() {
        if let Some(target) = source_include_target(line)? {
            ensure!(
                known.contains(target),
                "unresolved source include {target:?}; supply the library in the catalog or use native source linking"
            );
        }
        if line
            .split_ascii_whitespace()
            .next()
            .is_some_and(|name| name.eq_ignore_ascii_case(".lib"))
        {
            bail!("unresolved .lib directive in translated artifact");
        }
    }
    Ok(())
}

fn expanded(root: &AdmittedSource, format: InputFormat) -> Result<PreparedBundle> {
    let include = format == InputFormat::ExpandedLibrary;
    let translated = if include {
        translate_simplis_expanded_library(&root.bytes)?
    } else {
        translate_simplis_deck(&root.bytes)?
    };
    check_closed_includes(&translated.bytes, &BTreeSet::new())?;
    let artifact = generated(
        if include { "model.lib" } else { "deck.cir" }.to_owned(),
        translated.bytes,
        vec![display_path(&root.path)],
        None,
        Vec::new(),
        translated.required_code_models,
    );
    Ok(bundle(root, format, artifact, Vec::new()))
}

fn linked(
    root: &AdmittedSource,
    request: &Request,
    options: &Options,
    cancellation: &Cancellation,
    session: &mut SourceSession,
) -> Result<PreparedBundle> {
    let mut init = InitExpansion {
        session,
        output: Vec::new(),
        sources: BTreeSet::new(),
        stack: Vec::new(),
        limit: options.output_bytes,
        simulator: projection(request.format),
    };
    init.expand(root)?;
    let root_sources = init.sources;
    let expanded_root = init.output;
    let Catalog { catalog, sources } = catalog(request, options, cancellation, session)?;
    let mut linked = preprocess_netlist_linked(&expanded_root, &catalog)?;
    if request.format == InputFormat::SimetrixDeck {
        lower_simetrix_linked_runtime(&mut linked)?;
    }
    let known = linked
        .libraries
        .iter()
        .map(|library| library.filename.as_str())
        .collect::<BTreeSet<_>>();
    let translated_root = translate_simplis_deck(&linked.root_bytes)?;
    check_closed_includes(&translated_root.bytes, &known)?;
    let mut remaining = options
        .output_bytes
        .checked_sub(translated_root.bytes.len())
        .context("combined artifact output byte budget exceeded")?;
    let mut companions = Vec::new();
    let mut dependencies = Vec::new();
    for library in &linked.libraries {
        let translated = translate_simplis_expanded_library(&library.bytes)
            .with_context(|| format!("translating linked library {}", library.source_id))?;
        check_closed_includes(&translated.bytes, &known)?;
        remaining = remaining
            .checked_sub(translated.bytes.len())
            .context("combined artifact output byte budget exceeded")?;
        let origins = sources
            .get(&library.source_id)
            .unwrap_or(&root_sources)
            .iter()
            .cloned()
            .collect();
        dependencies.push(library.source_id.clone());
        companions.push(generated(
            library.filename.clone(),
            translated.bytes,
            origins,
            Some(library.source_id.clone()),
            library.dependencies.clone(),
            translated.required_code_models,
        ));
    }
    let primary = generated(
        "deck.cir".to_owned(),
        translated_root.bytes,
        root_sources.into_iter().collect(),
        None,
        dependencies,
        translated_root.required_code_models,
    );
    Ok(bundle(root, request.format, primary, companions))
}

pub fn prepare(
    root: &AdmittedSource,
    request: &Request,
    options: &Options,
    cancellation: &Cancellation,
    session: &mut SourceSession,
) -> Result<PreparedBundle> {
    ensure!(
        !root.bytes.contains(&0),
        "dialect source contains a NUL byte"
    );
    match request.format {
        InputFormat::SimplisLibrary | InputFormat::SimetrixLibrary => {
            parse_root(root, request.format)
        }
        InputFormat::ExpandedDeck | InputFormat::ExpandedLibrary => expanded(root, request.format),
        InputFormat::SimplisDeck | InputFormat::SimetrixDeck => {
            linked(root, request, options, cancellation, session)
        }
        InputFormat::NativeDeck | InputFormat::NativeLibrary => {
            bail!("native input is not a dialect translation request")
        }
    }
}
