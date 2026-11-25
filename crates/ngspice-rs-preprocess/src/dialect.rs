//! Shared dialect source preparation and artifact construction.
use super::analysis_plan::{
    self, AnalysisDialect, AnalysisPlan, DirectiveOrigin, PlanKind, SourceDeclaration,
};
use super::{
    AdmittedSource, Artifact, Cancellation, CodeModelRuntime, InputFormat, LibraryInput,
    LibrarySimulator, Options, ParsedLibrary, PreparedBundle, Provenance, Request, SourceSession,
    display_path,
};
use anyhow::{Context, Result, bail, ensure};
use model_normalize_ngspice::linked_source::{
    external_library_reference, select_named_section_lines,
};
use model_preprocess_command::AdmissionConfig;
use model_preprocess_simplis::source_dialect::{
    DeviceIdentity, DialectContext, ScopedStatement, SourceKind,
    model_mapping::{DeviceConfiguration, MappingTable, ModelSelection},
    resolve_device_identities,
};
use model_preprocess_simplis::{
    SubcircuitCatalog, lower_scoped_statements, parse_library_with_budget, parse_scoped_library,
    preprocess_netlist_linked, project_resource, source_include_target,
};
use model_translate_simplis::{
    capture_artifact_initial_conditions, capture_artifact_waveforms, capture_linked_waveforms,
    capture_passive_initial_conditions, lower_simetrix_linked_runtime,
    translate_simplis_deck_with_budget, translate_simplis_expanded_library_with_budget,
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
        current_scalings: Vec::new(),
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
        device_identities: Vec::new(),
        model_selections: Vec::new(),
        initial_conditions: None,
        analysis_plan: None,
        waveforms: Vec::new(),
    }
}

fn parse_root(
    root: &AdmittedSource,
    request: &Request,
    options: &Options,
    session: &mut SourceSession,
) -> Result<PreparedBundle> {
    let parsed =
        parse_admitted_library(root, request, options, session, DialectContext::default())?;
    let artifact = generated(
        "library.json".to_owned(),
        parsed.to_json()?,
        parsed.source_names().iter().cloned().collect(),
        Some(parsed.source_id().to_owned()),
        Vec::new(),
        Vec::new(),
    );
    let mut bundle = bundle(root, request.format, artifact, Vec::new());
    bundle.device_identities = parsed.device_identities().to_vec();
    Ok(bundle)
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

struct SourceExpansion<'a> {
    session: &'a mut SourceSession,
    output: Vec<u8>,
    sources: BTreeSet<String>,
    stack: Vec<(PathBuf, Option<String>)>,
    limit: usize,
    simulator: LibrarySimulator,
    search_paths: &'a [PathBuf],
    statements: Vec<ScopedStatement>,
    declarations: Vec<SourceDeclaration>,
    capture_plan: bool,
    scope_depth: usize,
}

impl<'a> SourceExpansion<'a> {
    fn new(session: &'a mut SourceSession, request: &'a Request, options: &Options) -> Self {
        Self {
            session,
            output: Vec::new(),
            sources: BTreeSet::new(),
            stack: Vec::new(),
            limit: options.output_bytes,
            simulator: projection(request.format),
            search_paths: &request.search_paths,
            statements: Vec::new(),
            declarations: Vec::new(),
            capture_plan: false,
            scope_depth: 0,
        }
    }

    fn append(&mut self, bytes: &[u8]) -> Result<()> {
        ensure!(
            bytes.len() <= self.limit.saturating_sub(self.output.len()),
            "source expansion byte budget exceeded"
        );
        self.session
            .budget()
            .charge(super::Resource::EmittedBytes, bytes.len())?;
        self.output.extend_from_slice(bytes);
        Ok(())
    }

    fn expand(
        &mut self,
        source: &AdmittedSource,
        inherited: DialectContext,
        kind: SourceKind,
        section: Option<&str>,
    ) -> Result<DialectContext> {
        let key = (source.path.clone(), section.map(str::to_ascii_lowercase));
        ensure!(
            !self.stack.contains(&key),
            "recursive source include {}",
            source.path.display()
        );
        let budget = self.session.budget().clone();
        let _depth = budget.enter()?;
        budget.charge(super::Resource::SourceVisits, 1)?;
        budget.charge(super::Resource::ScannedBytes, source.bytes.len())?;
        budget.charge(
            super::Resource::Statements,
            source.bytes.split_inclusive(|byte| *byte == b'\n').count(),
        )?;
        self.stack.push(key);
        self.sources.insert(display_path(&source.path));
        let result = self.expand_lines(source, inherited, kind, section);
        self.stack.pop();
        result.with_context(|| format!("expanding source {}", source.path.display()))
    }

    fn resolve_resource(&self, parent: &Path, target: &Path) -> Result<Option<PathBuf>> {
        let candidates = parent
            .parent()
            .into_iter()
            .map(|dir| dir.join(target))
            .chain(self.search_paths.iter().map(|dir| dir.join(target)));
        for candidate in candidates {
            match self.session.resolve_path(&candidate) {
                Ok(path) => return Ok(Some(path)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("resolving {}", candidate.display()));
                }
            }
        }
        Ok(None)
    }

    fn project_lines(
        &self,
        source: &AdmittedSource,
        inherited: DialectContext,
        kind: SourceKind,
        section: Option<&str>,
    ) -> Result<(
        DialectContext,
        Vec<model_preprocess_simplis::source_dialect::ProjectedStatement>,
    )> {
        let (context, mut projected) =
            project_resource(&source.bytes, inherited, kind, self.simulator)?;
        if let Some(section) = section {
            let selected = select_named_section_lines(
                projected.iter().map(|line| line.text.as_str()),
                &source.path,
                section,
            )?
            .into_iter()
            .collect::<BTreeSet<_>>();
            projected = projected
                .into_iter()
                .enumerate()
                .filter_map(|(index, line)| selected.contains(&index).then_some(line))
                .collect();
        }
        Ok((context, projected))
    }

    fn expand_lines(
        &mut self,
        source: &AdmittedSource,
        inherited: DialectContext,
        kind: SourceKind,
        section: Option<&str>,
    ) -> Result<DialectContext> {
        let (context, projected) = self.project_lines(source, inherited, kind, section)?;

        let mut embedded = false;
        let mut projected = projected.into_iter();
        while let Some(line) = projected.next() {
            let name = analysis_plan::command(&line.text);
            if let Some(start) = model_translate_simplis::syntax::file_boundary(&line.text) {
                embedded = start;
            }
            if !embedded && name != ".ENDF" {
                ensure!(
                    !line.text.starts_with(analysis_plan::TAG_PREFIX),
                    "reserved internal analysis marker at {}:{}",
                    source.path.display(),
                    line.first_line
                );
                if name == ".ENDS" {
                    self.scope_depth = self
                        .scope_depth
                        .checked_sub(1)
                        .context("unmatched .ENDS in source expansion")?;
                }
                if self.capture_plan && self.scope_depth == 0 && analysis_plan::tracked(&name) {
                    let mut declaration = SourceDeclaration {
                        origin: DirectiveOrigin::Source {
                            path: source.path.clone(),
                            first_line: line.first_line,
                            last_line: line.last_line,
                        },
                        text: line.text.clone(),
                    };
                    if name == ".CONTROL" {
                        let mut end = None;
                        for body in projected.by_ref() {
                            declaration.text.push('\n');
                            declaration.text.push_str(&body.text);
                            if analysis_plan::command(&body.text) == ".ENDC" {
                                end = Some(body.last_line);
                                break;
                            }
                        }
                        let last_line = end.context("unterminated .CONTROL block")?;
                        declaration.origin = DirectiveOrigin::Source {
                            path: source.path.clone(),
                            first_line: line.first_line,
                            last_line,
                        };
                    }
                    let tag = format!("{}{}", analysis_plan::TAG_PREFIX, self.declarations.len());
                    self.declarations.push(declaration);
                    self.append_projected(source, context, &line, &tag)?;
                    if name == ".CONTROL" {
                        continue;
                    }
                }
                if self.expand_reference(source, context, &line.text)? {
                    continue;
                }
                if name == ".SUBCKT" {
                    self.scope_depth = self
                        .scope_depth
                        .checked_add(1)
                        .context("source scope depth overflow")?;
                    self.session.budget().check_depth(self.scope_depth)?;
                }
            }
            if kind == SourceKind::Include && name == ".END" && !embedded {
                self.append_projected(source, context, &line, &format!("* {}", line.text))?;
            } else {
                self.append_projected(source, context, &line, &line.text)?;
            }
        }
        Ok(context)
    }

    fn append_projected(
        &mut self,
        source: &AdmittedSource,
        context: DialectContext,
        original: &model_preprocess_simplis::source_dialect::ProjectedStatement,
        line: &str,
    ) -> Result<()> {
        if self.simulator == LibrarySimulator::Simetrix {
            self.statements.push(ScopedStatement {
                source: display_path(&source.path),
                context,
                first_line: original.first_line,
                last_line: original.last_line,
                text: line.to_owned(),
            });
        }
        self.append(line.as_bytes())?;
        self.append(b"\n")
    }

    fn expand_reference(
        &mut self,
        source: &AdmittedSource,
        context: DialectContext,
        line: &str,
    ) -> Result<bool> {
        if let Some(target) = source_include_target(line)? {
            let normalized = target.replace('\\', "/");
            let target = Path::new(&normalized);
            let init = target
                .extension()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value.eq_ignore_ascii_case("init"));
            let path = if init {
                Some(resolve_init(self.session, &source.path, target)?)
            } else if self.simulator == LibrarySimulator::Simetrix {
                self.resolve_resource(&source.path, target)?
            } else {
                None
            };
            if let Some(path) = path {
                let snapshot = self.session.load(&path)?;
                self.expand(&snapshot, context, SourceKind::Include, None)?;
                return Ok(true);
            }
        }
        if self.simulator == LibrarySimulator::Simetrix
            && let Some((target, section)) = external_library_reference(line)?
        {
            let normalized = target.replace('\\', "/");
            let path = self
                .resolve_resource(&source.path, Path::new(&normalized))?
                .with_context(|| format!("cannot resolve named library {target}"))?;
            let snapshot = self.session.load(&path)?;
            self.expand(&snapshot, context, SourceKind::Include, Some(&section))?;
            return Ok(true);
        }
        Ok(false)
    }
}

fn parse_admitted_library(
    source: &AdmittedSource,
    request: &Request,
    options: &Options,
    session: &mut SourceSession,
    inherited: DialectContext,
) -> Result<ParsedLibrary> {
    let origin = display_path(&source.path);
    if projection(request.format) != LibrarySimulator::Simetrix {
        return parse_library_with_budget(
            origin,
            &source.bytes,
            projection(request.format),
            session.budget(),
        );
    }
    let mut expansion = SourceExpansion::new(session, request, options);
    let dialect = expansion.expand(source, inherited, SourceKind::Include, None)?;
    parse_scoped_library(
        origin,
        expansion.sources,
        dialect,
        &expansion.statements,
        options.output_bytes,
        expansion.session.budget(),
    )
}

struct ResolvedLibraries {
    catalog: SubcircuitCatalog,
    sources: BTreeMap<String, BTreeSet<String>>,
    device_identities: Vec<DeviceIdentity>,
    model_selections: Vec<ModelSelection>,
}

fn catalog_members(
    request: &Request,
    options: &Options,
    cancellation: &Cancellation,
    session: &mut SourceSession,
    context: DialectContext,
) -> Result<Vec<(ParsedLibrary, String)>> {
    let simulator = projection(request.format);
    let mut parsed_session = SourceSession::with_budget(
        AdmissionConfig {
            command: None,
            ..options.admission.clone()
        },
        cancellation.clone(),
        session.budget().clone(),
    );
    let mut members = Vec::new();

    for library in &request.libraries {
        let (parsed, origin) = match library {
            LibraryInput::Source(input) => {
                let admitted = input.admit(session)?;
                let origin = display_path(&admitted.path);
                (
                    parse_admitted_library(&admitted, request, options, session, context)?,
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
        members.push((parsed, origin));
    }
    Ok(members)
}

fn catalog(
    simulator: LibrarySimulator,
    members: Vec<(ParsedLibrary, String)>,
    mapping: Option<&MappingTable>,
    limit: usize,
    budget: &super::WorkBudget,
) -> Result<ResolvedLibraries> {
    let mut catalog = SubcircuitCatalog::for_simulator(simulator).with_budget(budget.clone());
    let mut sources = BTreeMap::<String, BTreeSet<String>>::new();
    let mut device_identities = Vec::new();
    let mut model_selections = Vec::new();
    for (parsed, origin) in members {
        let parsed = if let Some(mapping) = mapping {
            let (parsed, selections) = parsed.mapped(mapping, limit, budget)?;
            model_selections.extend(selections);
            parsed
        } else {
            parsed
        };
        catalog.add_parsed_library(&parsed)?;
        device_identities.extend(parsed.device_identities().iter().cloned());
        let origins = sources.entry(parsed.source_id().to_owned()).or_default();
        origins.extend(parsed.source_names().iter().cloned());
        origins.insert(origin);
    }
    Ok(ResolvedLibraries {
        catalog,
        sources,
        device_identities,
        model_selections,
    })
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

fn expanded(
    root: &AdmittedSource,
    format: InputFormat,
    budget: &super::WorkBudget,
) -> Result<PreparedBundle> {
    budget.charge(super::Resource::SourceVisits, 1)?;
    budget.charge(super::Resource::ScannedBytes, root.bytes.len())?;
    budget.charge(
        super::Resource::Statements,
        root.bytes.split_inclusive(|byte| *byte == b'\n').count(),
    )?;
    let mut depth = 0usize;
    let mut embedded = false;
    for line in std::str::from_utf8(&root.bytes)?.lines() {
        if let Some(start) = model_translate_simplis::syntax::file_boundary(line) {
            embedded = start;
            continue;
        }
        if embedded {
            continue;
        }
        match analysis_plan::command(line).as_str() {
            ".SUBCKT" if line.split_ascii_whitespace().nth(1).is_some() => {
                depth += 1;
                budget.check_depth(depth)?;
            }
            ".ENDS" => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    let include = format == InputFormat::ExpandedLibrary;
    let mut plan = AnalysisPlan::new(
        AnalysisDialect::Expanded,
        if include {
            PlanKind::Library
        } else {
            PlanKind::Deck
        },
        None,
    );
    let mut source = root.bytes.to_vec();
    analysis_plan::capture(
        &mut plan,
        &mut source,
        if include { "model.lib" } else { "deck.cir" },
        Some(&root.path),
        &[],
    )?;
    let mut waveforms = Vec::new();
    capture_artifact_waveforms(
        &mut source,
        if include { "model.lib" } else { "deck.cir" },
        &mut waveforms,
    )?;
    let translated = if include {
        translate_simplis_expanded_library_with_budget(&source, budget)?
    } else {
        translate_simplis_deck_with_budget(&source, budget)?
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
    let mut bundle = bundle(root, format, artifact, Vec::new());
    bundle.analysis_plan = Some(plan);
    bundle.waveforms = waveforms;
    Ok(bundle)
}

fn translate_companions(
    linked: &model_preprocess_simplis::LinkedPreprocessedNetlist,
    known: &BTreeSet<&str>,
    sources: &BTreeMap<String, BTreeSet<String>>,
    root_sources: &BTreeSet<String>,
    initial_conditions: &mut Option<super::passive_ic::InitialConditions>,
    mut remaining: usize,
    budget: &model_preprocess_command::WorkBudget,
) -> Result<(Vec<Artifact>, Vec<String>)> {
    let mut companions = Vec::new();
    let mut dependencies = Vec::new();
    for library in &linked.libraries {
        let mut translated = translate_simplis_expanded_library_with_budget(&library.bytes, budget)
            .with_context(|| format!("translating linked library {}", library.source_id))?;
        if let Some(policy) = &mut *initial_conditions {
            capture_artifact_initial_conditions(&mut translated.bytes, &library.filename, policy)?;
        }
        check_closed_includes(&translated.bytes, known)?;
        remaining = remaining
            .checked_sub(translated.bytes.len())
            .context("combined artifact output byte budget exceeded")?;
        let origins = sources
            .get(&library.source_id)
            .unwrap_or(root_sources)
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
    Ok((companions, dependencies))
}

fn capture_linked_plan(
    linked: &mut model_preprocess_simplis::LinkedPreprocessedNetlist,
    root: &AdmittedSource,
    format: InputFormat,
    declarations: &[analysis_plan::SourceDeclaration],
) -> Result<AnalysisPlan> {
    let dialect = if format == InputFormat::SimetrixDeck {
        AnalysisDialect::Simetrix
    } else {
        AnalysisDialect::Simplis
    };
    let mut plan = AnalysisPlan::new(
        dialect,
        PlanKind::Deck,
        (dialect == AnalysisDialect::Simetrix).then(|| analysis_plan::source_title(&root.bytes)),
    );
    analysis_plan::capture(
        &mut plan,
        &mut linked.root_bytes,
        "deck.cir",
        None,
        declarations,
    )?;
    for library in &mut linked.libraries {
        analysis_plan::capture(&mut plan, &mut library.bytes, &library.filename, None, &[])?;
    }
    Ok(plan)
}

struct ResolvedDeck {
    root: Vec<u8>,
    root_sources: BTreeSet<String>,
    declarations: Vec<SourceDeclaration>,
    libraries: ResolvedLibraries,
}

fn resolve_deck(
    root: &AdmittedSource,
    request: &Request,
    options: &Options,
    cancellation: &Cancellation,
    session: &mut SourceSession,
) -> Result<ResolvedDeck> {
    let mut init = SourceExpansion::new(session, request, options);
    init.capture_plan = true;
    let context = init.expand(root, DialectContext::default(), SourceKind::Deck, None)?;
    let SourceExpansion {
        sources: root_sources,
        declarations,
        statements,
        output,
        ..
    } = init;
    let members = catalog_members(request, options, cancellation, session, context)?;
    let mapping = if request.format == InputFormat::SimetrixDeck {
        let configuration = if let Some(path) = &options.device_config {
            let mut config_session = SourceSession::with_budget(
                AdmissionConfig {
                    command: None,
                    ..options.admission.clone()
                },
                cancellation.clone(),
                session.budget().clone(),
            );
            let admitted = config_session.load(path)?;
            DeviceConfiguration::parse(
                &display_path(&admitted.path),
                std::str::from_utf8(&admitted.bytes)?,
                session.budget(),
            )?
        } else {
            DeviceConfiguration::default()
        };
        // Only netlist resources explicitly included by the root can
        // change the simulation-wide table. An independently indexed
        // library cannot introduce global options conditionally.
        for (parsed, _) in &members {
            model_preprocess_simplis::source_dialect::model_mapping::validate_catalog_directives(
                parsed.scoped_statements(),
                session.budget(),
            )?;
        }
        Some(MappingTable::resolve(
            &configuration,
            &statements,
            session.budget(),
        )?)
    } else {
        None
    };
    let (expanded_root, mut device_identities, mut model_selections) = if let Some(mapping) =
        &mapping
    {
        let mapped = mapping.apply(&statements, session.budget())?;
        let lowered =
            lower_scoped_statements(&mapped.statements, options.output_bytes, session.budget())?;
        let resolved = resolve_device_identities(&lowered)?;
        session
            .budget()
            .charge(super::Resource::EmittedBytes, resolved.text.len())?;
        ensure!(
            resolved.text.len() <= options.output_bytes,
            "device identity expansion byte budget exceeded"
        );
        (
            resolved.text.into_bytes(),
            resolved.identities,
            mapped.selections,
        )
    } else {
        (output, Vec::new(), Vec::new())
    };
    let ResolvedLibraries {
        catalog,
        sources,
        device_identities: catalog_identities,
        model_selections: catalog_selections,
    } = catalog(
        projection(request.format),
        members,
        mapping.as_ref(),
        options.output_bytes,
        session.budget(),
    )?;
    device_identities.extend(catalog_identities);
    model_selections.extend(catalog_selections);
    Ok(ResolvedDeck {
        root: expanded_root,
        root_sources,
        declarations,
        libraries: ResolvedLibraries {
            catalog,
            sources,
            device_identities,
            model_selections,
        },
    })
}

fn linked(
    root: &AdmittedSource,
    request: &Request,
    options: &Options,
    cancellation: &Cancellation,
    session: &mut SourceSession,
) -> Result<PreparedBundle> {
    let ResolvedDeck {
        root: expanded_root,
        root_sources,
        declarations,
        libraries:
            ResolvedLibraries {
                catalog,
                sources,
                device_identities,
                model_selections,
            },
    } = resolve_deck(root, request, options, cancellation, session)?;
    let mut linked = preprocess_netlist_linked(&expanded_root, &catalog)?;
    let plan = capture_linked_plan(&mut linked, root, request.format, &declarations)?;
    let mut initial_conditions = if request.format == InputFormat::SimetrixDeck {
        lower_simetrix_linked_runtime(&mut linked)?;
        Some(capture_passive_initial_conditions(&mut linked)?)
    } else {
        None
    };
    let waveforms = capture_linked_waveforms(&mut linked)?;
    let known = linked
        .libraries
        .iter()
        .map(|library| library.filename.as_str())
        .collect::<BTreeSet<_>>();
    let mut translated_root =
        translate_simplis_deck_with_budget(&linked.root_bytes, session.budget())?;
    if request.format == InputFormat::SimplisDeck {
        initial_conditions = Some(super::passive_ic::InitialConditions {
            mode: Some("1".to_owned()),
            ..Default::default()
        });
    }
    if let Some(policy) = &mut initial_conditions {
        capture_artifact_initial_conditions(&mut translated_root.bytes, "deck.cir", policy)?;
    }
    plan.append_native_options(&mut translated_root.bytes)?;
    check_closed_includes(&translated_root.bytes, &known)?;
    let remaining = options
        .output_bytes
        .checked_sub(translated_root.bytes.len())
        .context("combined artifact output byte budget exceeded")?;
    let (companions, dependencies) = translate_companions(
        &linked,
        &known,
        &sources,
        &root_sources,
        &mut initial_conditions,
        remaining,
        session.budget(),
    )?;
    let primary = generated(
        "deck.cir".to_owned(),
        translated_root.bytes,
        root_sources.into_iter().collect(),
        None,
        dependencies,
        translated_root.required_code_models,
    );
    let mut bundle = bundle(root, request.format, primary, companions);
    bundle.device_identities = device_identities;
    bundle.model_selections = model_selections;
    bundle.analysis_plan = Some(plan);
    if let Some(initialization) = &initial_conditions {
        for artifact in bundle.artifacts() {
            initialization.validate_artifact(&artifact.filename, &artifact.bytes)?;
        }
    }
    bundle.initial_conditions = initial_conditions.filter(|policy| {
        request.format == InputFormat::SimetrixDeck
            || !policy.devices.is_empty()
            || !policy.nodes.is_empty()
    });
    bundle.waveforms = waveforms;
    Ok(bundle)
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
            parse_root(root, request, options, session)
        }
        InputFormat::ExpandedDeck | InputFormat::ExpandedLibrary => {
            expanded(root, request.format, session.budget())
        }
        InputFormat::SimplisDeck | InputFormat::SimetrixDeck => {
            linked(root, request, options, cancellation, session)
        }
        InputFormat::NativeDeck | InputFormat::NativeLibrary => {
            bail!("native input is not a dialect translation request")
        }
    }
}
