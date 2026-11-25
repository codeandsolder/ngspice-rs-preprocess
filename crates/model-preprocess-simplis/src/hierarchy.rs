use super::{
    Environment, decode_simplis_text, find_ascii_case_insensitive, parse_parameter_list,
    preprocess_control_flow, specialize_subcircuit, specialize_subcircuit_control_flow_with_base,
    specialize_subcircuit_with_base,
};
use anyhow::{Context, Result, bail, ensure};
use model_preprocess_command::SourceSession;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

mod runtime_params;
use runtime_params::RuntimeScope;

const PARSED_LIBRARY_FORMAT_VERSION: u32 = 6;

fn lower_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Definition {
    name: String,
    bytes: Vec<u8>,
    parameterized: bool,
    scope_id: String,
    owner_scope_id: Option<String>,
    source_id: Option<String>,
    source_name: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Alias {
    name: String,
    target: String,
    kind: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LibrarySimulator {
    Simplis,
    Simetrix,
    Hspice,
}

impl LibrarySimulator {
    #[must_use = "the returned name identifies the simulator"]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Simplis => "simplis",
            Self::Simetrix => "simetrix",
            Self::Hspice => "hspice",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ParsedLibrary {
    format_version: u32,
    simulator: LibrarySimulator,
    dialect: Option<crate::source_dialect::DialectContext>,
    device_identities: Vec<crate::source_dialect::DeviceIdentity>,
    source_id: String,
    scoped_statements: Vec<crate::source_dialect::ScopedStatement>,
    source_name: String,
    source_names: BTreeSet<String>,
    support_lines: Vec<String>,
    aliases: Vec<Alias>,
    definitions: Vec<Definition>,
}

impl ParsedLibrary {
    #[must_use = "the result identifies the library projection"]
    pub const fn simulator(&self) -> LibrarySimulator {
        self.simulator
    }

    /// Effective source language of a `SIMetrix` catalog resource.
    #[must_use = "the context determines inherited source semantics"]
    pub const fn dialect(&self) -> Option<crate::source_dialect::DialectContext> {
        self.dialect
    }

    /// Original projected declarations, before simulation-local mapping.
    #[must_use]
    pub fn scoped_statements(&self) -> &[crate::source_dialect::ScopedStatement] {
        &self.scoped_statements
    }

    /// Rebuild a `SIMetrix` catalog member with the request's device map.
    ///
    /// # Errors
    /// Invalid source selection or hierarchy.
    pub fn mapped(
        &self,
        table: &crate::source_dialect::model_mapping::MappingTable,
        limit: usize,
    ) -> Result<(
        Self,
        Vec<crate::source_dialect::model_mapping::ModelSelection>,
    )> {
        anyhow::ensure!(
            self.simulator != LibrarySimulator::Hspice,
            "standalone HSPICE library requires deck binding"
        );
        let Some(dialect) = self.dialect else {
            return Ok((self.clone(), Vec::new()));
        };
        let mapped = table.apply(&self.scoped_statements)?;
        let parsed = parse_scoped_parts(
            self.source_name.clone(),
            self.source_names.clone(),
            dialect,
            &mapped.statements,
            self.scoped_statements.clone(),
            limit,
        )?;
        Ok((parsed, mapped.selections))
    }

    /// Original declarations and linked tokens before specialization.
    #[must_use = "the mapping retains source-level device identities"]
    pub fn device_identities(&self) -> &[crate::source_dialect::DeviceIdentity] {
        &self.device_identities
    }

    #[must_use = "the identity distinguishes the source library"]
    pub fn source_id(&self) -> &str {
        &self.source_id
    }

    #[must_use = "the result identifies the original source name"]
    pub fn source_name(&self) -> &str {
        &self.source_name
    }

    /// Original admitted resources, including nested source files.
    #[must_use = "the result retains source graph provenance"]
    pub const fn source_names(&self) -> &BTreeSet<String> {
        &self.source_names
    }

    #[must_use = "the count reports the parsed definitions"]
    pub fn definitions(&self) -> usize {
        if self.simulator == LibrarySimulator::Hspice {
            self.scoped_statements
                .iter()
                .filter(|line| is_subcircuit_header(&line.text))
                .count()
        } else {
            self.definitions.len()
        }
    }

    /// Serialize this parsed library to the stable JSON interchange form.
    ///
    /// # Errors
    ///
    /// Returns an error if JSON serialization fails.
    pub fn to_json(&self) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec_pretty(self)?)
    }

    /// Load and validate a parsed-library JSON artifact.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed JSON, unsupported format versions, or a
    /// content-address mismatch.
    pub fn from_json(input: &[u8]) -> Result<Self> {
        let parsed: Self = serde_json::from_slice(input)?;
        parsed.validate()?;
        Ok(parsed)
    }

    fn validate(&self) -> Result<()> {
        if self.format_version != PARSED_LIBRARY_FORMAT_VERSION {
            bail!(
                "unsupported parsed source library format version {}",
                self.format_version
            );
        }
        anyhow::ensure!(
            self.source_names.contains(&self.source_name)
                && self.scoped_statements.iter().all(|statement| self
                    .source_names
                    .contains(&statement.source)
                    && statement.first_line > 0
                    && statement.last_line >= statement.first_line)
                && self
                    .device_identities
                    .iter()
                    .all(|identity| self.source_names.contains(&identity.source)),
            "parsed library source provenance is incomplete"
        );
        let actual = parsed_library_digest(
            self.simulator,
            self.dialect,
            &self.device_identities,
            &self.scoped_statements,
            &self.support_lines,
            &self.aliases,
            &self.definitions,
        );
        if actual != self.source_id {
            bail!("parsed library content-address mismatch");
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct SourceMetadata {
    source_name: String,
    source_names: BTreeSet<String>,
    support_lines: Vec<String>,
    aliases: Vec<Alias>,
}

#[derive(Clone, Debug)]
struct AliasBinding {
    target: String,
    kind: String,
    source_id: String,
}

#[derive(Clone, Debug, Default)]
pub struct SubcircuitCatalog {
    simulator: Option<LibrarySimulator>,
    definitions: BTreeMap<String, Vec<Definition>>,
    aliases: BTreeMap<String, Vec<AliasBinding>>,
    sources: BTreeMap<String, SourceMetadata>,
    next_anonymous_source: usize,
}

/// Parse a static dialect include path, with literal Windows separators.
///
/// # Errors
///
/// Malformed include operands or unterminated quotes.
pub fn source_include_target(statement: &str) -> Result<Option<&str>> {
    let trimmed = statement.trim_start();
    let mut fields = trimmed.splitn(2, char::is_whitespace);
    let directive = fields.next().unwrap_or_default();
    if !directive.eq_ignore_ascii_case(".include") && !directive.eq_ignore_ascii_case(".inc") {
        return Ok(None);
    }
    let rest = fields
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .context("include has no path")?;
    let (target, tail) = if let Some(quote) = rest
        .chars()
        .next()
        .filter(|character| matches!(character, '\'' | '"'))
    {
        let end = rest[1..]
            .find(quote)
            .context("unterminated quoted include path")?
            + 1;
        (&rest[1..end], &rest[end + 1..])
    } else {
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        (&rest[..end], &rest[end..])
    };
    ensure!(!target.is_empty(), "empty include path");
    ensure!(
        tail.trim().is_empty()
            || tail.trim_start().starts_with([';', '$'])
            || tail.trim_start().starts_with("//"),
        "unexpected text after include path"
    );
    Ok(Some(target))
}

fn path_suffix_matches(source: &str, include: &str) -> bool {
    fn components(path: &str) -> impl Iterator<Item = &str> {
        path.rsplit(['/', '\\'])
            .filter(|part| !part.is_empty() && *part != ".")
    }
    let source = components(source).collect::<Vec<_>>();
    let include = components(include).collect::<Vec<_>>();
    !include.is_empty()
        && include.len() <= source.len()
        && include
            .iter()
            .zip(source)
            .all(|(expected, actual)| expected.eq_ignore_ascii_case(actual))
}

impl SubcircuitCatalog {
    #[must_use = "the returned catalog owns the library definitions"]
    pub fn new() -> Self {
        Self::default()
    }

    /// Select the source simulator even when no external libraries are supplied.
    #[must_use = "the returned catalog selects the simulator"]
    pub fn for_simulator(simulator: LibrarySimulator) -> Self {
        Self {
            simulator: Some(simulator),
            ..Self::default()
        }
    }

    /// Add standalone `.SUBCKT` definitions from one anonymous library source.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed subcircuit nesting or active non-ASCII syntax.
    pub fn add_library(&mut self, input: &[u8]) -> Result<usize> {
        self.next_anonymous_source = self.next_anonymous_source.saturating_add(1);
        let source = format!("<anonymous:{}>", self.next_anonymous_source);
        self.add_library_named(source, input)
    }

    /// Parse and add standalone `.SUBCKT` definitions with explicit library provenance.
    ///
    /// Nested helper resolution prefers definitions from the same source library
    /// before falling back to the global catalog.
    ///
    /// # Errors
    ///
    /// Returns an error if the parsed artifact fails its integrity/version check.
    pub fn add_library_named(&mut self, source: impl Into<String>, input: &[u8]) -> Result<usize> {
        self.add_library_named_for_simulator(source, input, LibrarySimulator::Simplis)
    }

    /// Parse and add one library using an explicit simulator projection.
    ///
    /// # Errors
    ///
    /// Returns an error if the selected projection is malformed or conflicts
    /// with the simulator projection already present in this catalog.
    pub fn add_library_named_for_simulator(
        &mut self,
        source: impl Into<String>,
        input: &[u8],
        simulator: LibrarySimulator,
    ) -> Result<usize> {
        let parsed = parse_library_for_simulator(source, input, simulator)?;
        self.add_parsed_library(&parsed)
    }

    /// Admit and parse a source library with the same session as its root deck.
    ///
    /// # Errors
    ///
    /// Source admission, external command, or library parsing errors.
    pub fn add_library_file(
        &mut self,
        path: &Path,
        simulator: LibrarySimulator,
        admission: &mut SourceSession,
    ) -> Result<usize> {
        let parsed = parse_library_file(path, simulator, admission)?;
        self.add_parsed_library(&parsed)
    }

    /// Add a previously parsed content-addressed library artifact.
    ///
    /// # Errors
    ///
    /// Returns an error if the parsed artifact fails its integrity/version check.
    pub fn add_parsed_library(&mut self, parsed: &ParsedLibrary) -> Result<usize> {
        parsed.validate()?;
        anyhow::ensure!(
            parsed.simulator != LibrarySimulator::Hspice,
            "standalone HSPICE library requires native deck binding"
        );
        if let Some(existing) = self.simulator {
            if existing != parsed.simulator {
                bail!(
                    "cannot mix {} and {} parsed-library projections in one catalog",
                    existing.as_str(),
                    parsed.simulator.as_str()
                );
            }
        } else {
            self.simulator = Some(parsed.simulator);
        }
        let source_id = parsed.source_id.clone();
        let source_name = parsed.source_name.clone();
        self.sources
            .entry(source_id.clone())
            .or_insert_with(|| SourceMetadata {
                source_name: source_name.clone(),
                source_names: BTreeSet::new(),
                support_lines: parsed.support_lines.clone(),
                aliases: parsed.aliases.clone(),
            })
            .source_names
            .insert(source_name.clone());
        for alias in &parsed.aliases {
            let bindings = self
                .aliases
                .entry(alias.name.to_ascii_uppercase())
                .or_default();
            if !bindings.iter().any(|binding| {
                binding.source_id == source_id
                    && binding.target.eq_ignore_ascii_case(&alias.target)
                    && binding.kind.eq_ignore_ascii_case(&alias.kind)
            }) {
                bindings.push(AliasBinding {
                    target: alias.target.clone(),
                    kind: alias.kind.clone(),
                    source_id: source_id.clone(),
                });
            }
        }
        let mut added = 0usize;
        for definition in &parsed.definitions {
            let mut definition = definition.clone();
            definition.source_id = Some(source_id.clone());
            definition.source_name = Some(source_name.clone());
            let key = definition.name.to_ascii_uppercase();
            let definitions = self.definitions.entry(key).or_default();
            if definitions.iter().any(|existing| {
                existing.source_id.as_deref() == Some(source_id.as_str())
                    && existing.owner_scope_id == definition.owner_scope_id
                    && normalize_definition_for_equivalence(&existing.bytes)
                        == normalize_definition_for_equivalence(&definition.bytes)
            }) {
                continue;
            }
            definitions.push(definition);
            added = added.saturating_add(1);
        }
        Ok(added)
    }

    #[must_use = "the returned identities enumerate registered libraries"]
    pub fn source_ids(&self) -> Vec<String> {
        self.sources.keys().cloned().collect()
    }

    fn source_metadata(&self, source_id: &str) -> Option<&SourceMetadata> {
        self.sources.get(source_id)
    }

    fn path_basename(path: &str) -> &str {
        path.rsplit(['/', '\\']).next().unwrap_or(path)
    }

    fn matches_supplied_include(&self, statement: &str) -> Result<bool> {
        let Some(include) = source_include_target(statement)? else {
            return Ok(false);
        };
        let mut suffix_matches = BTreeSet::new();
        let mut basename_matches = BTreeSet::new();
        for (identity, metadata) in &self.sources {
            for name in &metadata.source_names {
                if path_suffix_matches(name, include) {
                    suffix_matches.insert(identity);
                }
                if Self::path_basename(name).eq_ignore_ascii_case(Self::path_basename(include)) {
                    basename_matches.insert(identity);
                }
            }
        }
        // Explicit path suffixes distinguish same-name libraries; basename
        // fallback supports relocated vendor source paths only when unique.
        let candidates = if suffix_matches.is_empty() {
            basename_matches
        } else {
            suffix_matches
        };
        anyhow::ensure!(
            candidates.len() <= 1,
            "ambiguous supplied library include {include:?}"
        );
        Ok(!candidates.is_empty())
    }

    fn model_statement<'a>(statement: &'a str, name: &str) -> Option<Vec<&'a str>> {
        let fields = statement.split_whitespace().collect::<Vec<_>>();
        (fields.len() >= 3
            && fields[0].eq_ignore_ascii_case(".MODEL")
            && fields[1].eq_ignore_ascii_case(name))
        .then_some(fields)
    }

    fn resolve_model_statement(&self, target: &str, preferred_source: &str) -> Result<String> {
        let same_source = self
            .sources
            .get(preferred_source)
            .into_iter()
            .flat_map(|metadata| metadata.support_lines.iter())
            .filter(|statement| Self::model_statement(statement, target).is_some())
            .cloned()
            .collect::<Vec<_>>();
        let candidates = if same_source.is_empty() {
            self.sources
                .values()
                .flat_map(|metadata| metadata.support_lines.iter())
                .filter(|statement| Self::model_statement(statement, target).is_some())
                .cloned()
                .collect::<Vec<_>>()
        } else {
            same_source
        };
        let Some(first) = candidates.first() else {
            bail!("SIMPLIS model alias target {target} was not found");
        };
        let normalized = first.replace('\r', "");
        if candidates
            .iter()
            .skip(1)
            .any(|candidate| candidate.replace('\r', "") != normalized)
        {
            bail!("ambiguous SIMPLIS model alias target {target}");
        }
        Ok(first.clone())
    }

    fn source_for_model(&self, target: &str) -> Result<Option<String>> {
        if let Some(alias) = self.resolve_alias(target, None)?
            && !alias.kind.eq_ignore_ascii_case("subckt")
        {
            return Ok(Some(alias.source_id.clone()));
        }
        let candidates = self
            .sources
            .iter()
            .flat_map(|(source_id, metadata)| {
                metadata.support_lines.iter().filter_map(move |statement| {
                    Self::model_statement(statement, target)
                        .map(|_| (source_id, statement.replace('\r', "")))
                })
            })
            .collect::<Vec<_>>();
        let Some((source_id, definition)) = candidates.first() else {
            return Ok(None);
        };
        if candidates
            .iter()
            .skip(1)
            .any(|(_, candidate)| candidate != definition)
        {
            bail!("ambiguous SIMPLIS device model {target}");
        }
        Ok(Some((*source_id).clone()))
    }

    fn materialized_support(&self, source_id: &str) -> Result<Vec<String>> {
        let metadata = self
            .sources
            .get(source_id)
            .with_context(|| format!("missing SIMPLIS source metadata {source_id}"))?;
        let mut output = metadata.support_lines.clone();
        for alias in &metadata.aliases {
            if alias.kind.eq_ignore_ascii_case("subckt") {
                continue;
            }
            let target = self.resolve_model_statement(&alias.target, source_id)?;
            let mut fields = Self::model_statement(&target, &alias.target)
                .context("resolved SIMPLIS model alias target is not a .MODEL statement")?;
            let model_type = fields[2]
                .split_once('(')
                .map_or(fields[2], |(head, _)| head);
            if !model_type.eq_ignore_ascii_case(&alias.kind) {
                bail!(
                    "SIMPLIS alias {} expects {} but target {} is model type {model_type}",
                    alias.name,
                    alias.kind,
                    alias.target
                );
            }
            fields[1] = &alias.name;
            output.push(fields.join(" "));
        }
        Ok(output)
    }

    fn resolve_alias(
        &self,
        name: &str,
        preferred_source: Option<&str>,
    ) -> Result<Option<&AliasBinding>> {
        let Some(bindings) = self.aliases.get(&name.to_ascii_uppercase()) else {
            return Ok(None);
        };
        let candidates = preferred_source.map_or_else(
            || bindings.iter().collect::<Vec<_>>(),
            |source| {
                let same_source = bindings
                    .iter()
                    .filter(|binding| binding.source_id == source)
                    .collect::<Vec<_>>();
                if same_source.is_empty() {
                    bindings.iter().collect::<Vec<_>>()
                } else {
                    same_source
                }
            },
        );
        let first = candidates.first().copied();
        if candidates.iter().skip(1).any(|binding| {
            first.is_some_and(|first| {
                !binding.target.eq_ignore_ascii_case(&first.target)
                    || !binding.kind.eq_ignore_ascii_case(&first.kind)
            })
        }) {
            bail!("ambiguous SIMPLIS alias {name}");
        }
        Ok(first)
    }

    fn unique_semantic<'a>(
        definitions: impl IntoIterator<Item = &'a Definition>,
    ) -> Option<&'a Definition> {
        let mut iterator = definitions.into_iter();
        let first = iterator.next()?;
        let normalized = normalize_definition_for_equivalence(&first.bytes);
        iterator
            .all(|definition| normalize_definition_for_equivalence(&definition.bytes) == normalized)
            .then_some(first)
    }

    fn contains_source_id(&self, source_id: &str) -> bool {
        self.sources.contains_key(source_id)
            || self
                .definitions
                .values()
                .flatten()
                .any(|definition| definition.source_id.as_deref() == Some(source_id))
    }

    fn resolve_scoped_direct(
        &self,
        name: &str,
        source_id: &str,
        owner_scope_id: &str,
    ) -> Result<Option<&Definition>> {
        let Some(definitions) = self.definitions.get(&name.to_ascii_uppercase()) else {
            return Ok(None);
        };
        let scoped = definitions
            .iter()
            .filter(|definition| {
                definition.source_id.as_deref() == Some(source_id)
                    && definition.owner_scope_id.as_deref() == Some(owner_scope_id)
            })
            .collect::<Vec<_>>();
        if scoped.is_empty() {
            return Ok(None);
        }
        if let Some(definition) = Self::unique_semantic(scoped.iter().copied()) {
            return Ok(Some(definition));
        }
        bail!(
            "ambiguous SIMPLIS subcircuit {name} inside lexical scope {owner_scope_id} of source library {source_id}"
        )
    }

    fn resolve_direct(
        &self,
        name: &str,
        preferred_source: Option<&str>,
    ) -> Result<Option<&Definition>> {
        let Some(all_definitions) = self.definitions.get(&name.to_ascii_uppercase()) else {
            return Ok(None);
        };
        let definitions = all_definitions
            .iter()
            .filter(|definition| definition.owner_scope_id.is_none())
            .collect::<Vec<_>>();
        if definitions.is_empty() {
            return Ok(None);
        }
        if let Some(source) = preferred_source {
            let same_source = definitions
                .iter()
                .copied()
                .filter(|definition| definition.source_id.as_deref() == Some(source))
                .collect::<Vec<_>>();
            if !same_source.is_empty() {
                if let Some(definition) = Self::unique_semantic(same_source.iter().copied()) {
                    return Ok(Some(definition));
                }
                bail!("ambiguous SIMPLIS subcircuit {name} inside source library {source}");
            }
        }
        if let Some(definition) = Self::unique_semantic(definitions.iter().copied()) {
            return Ok(Some(definition));
        }
        let parameterized = definitions
            .iter()
            .copied()
            .filter(|definition| definition.parameterized)
            .collect::<Vec<_>>();
        if let Some(definition) = Self::unique_semantic(parameterized.iter().copied()) {
            return Ok(Some(definition));
        }
        bail!(
            "ambiguous SIMPLIS subcircuit {name}: {} top-level source definitions",
            definitions.len()
        )
    }

    fn resolve(&self, name: &str, preferred_source: Option<&str>) -> Result<Option<&Definition>> {
        if let Some(definition) = self.resolve_direct(name, preferred_source)? {
            return Ok(Some(definition));
        }
        let Some(alias) = self.resolve_alias(name, preferred_source)? else {
            return Ok(None);
        };
        if !alias.kind.eq_ignore_ascii_case("subckt") {
            return Ok(None);
        }
        self.resolve_direct(&alias.target, Some(&alias.source_id))
    }
}

/// Parse one SIMPLIS library into a stable content-addressed artifact.
///
/// The source name is diagnostic metadata only. Resolution identity is derived
/// from the semantic subcircuit contents, so moving or renaming a library does
/// not change its identity.
///
/// # Errors
///
/// Returns an error for malformed subcircuit nesting or active non-ASCII syntax.
pub fn parse_library(source: impl Into<String>, input: &[u8]) -> Result<ParsedLibrary> {
    parse_library_for_simulator(source, input, LibrarySimulator::Simplis)
}

/// Admit a source library before parsing its simulator-specific projection.
///
/// # Errors
///
/// Source admission, external command, or library parsing errors.
pub fn parse_library_file(
    path: &Path,
    simulator: LibrarySimulator,
    admission: &mut SourceSession,
) -> Result<ParsedLibrary> {
    let source = admission.load(path)?;
    parse_library_for_simulator(source.path.to_string_lossy(), &source.bytes, simulator)
}

/// Parse one library using an explicit simulator projection.
///
/// Unscoped and `.SIMULATOR DEFAULT` content is visible in both projections.
/// Simulator-specific sections are retained only when their selector contains
/// the requested simulator.
///
/// # Errors
///
/// Returns an error for malformed simulator selectors, malformed subcircuit
/// nesting, or active non-ASCII syntax.
pub fn parse_library_for_simulator(
    source: impl Into<String>,
    input: &[u8],
    simulator: LibrarySimulator,
) -> Result<ParsedLibrary> {
    use crate::source_dialect::{
        DEFAULT_PROJECTION_BYTES, DialectContext, ScopedStatement, SourceKind,
    };
    let source_name = source.into();
    let sources = BTreeSet::from([source_name.clone()]);
    if simulator == LibrarySimulator::Hspice {
        let (context, lines) = project_spice_resource(
            input,
            DialectContext {
                language: crate::source_dialect::SourceLanguage::Hspice,
                separator: None,
            },
            SourceKind::Include,
        )?;
        let statements = lines
            .into_iter()
            .map(|line| ScopedStatement {
                source: source_name.clone(),
                context,
                first_line: line.first_line,
                last_line: line.last_line,
                text: line.text,
            })
            .collect();
        return parse_standalone_library(source_name, sources, context, statements);
    }
    if simulator == LibrarySimulator::Simetrix {
        let (context, view) = project_resource(
            input,
            DialectContext::default(),
            SourceKind::Include,
            simulator,
        )?;
        let statements = view
            .into_iter()
            .map(|line| ScopedStatement {
                source: source_name.clone(),
                context,
                first_line: line.first_line,
                last_line: line.last_line,
                text: line.text,
            })
            .collect::<Vec<_>>();
        return parse_scoped_library(
            source_name,
            sources,
            context,
            &statements,
            DEFAULT_PROJECTION_BYTES,
        );
    }
    let view = project_source(input, simulator)?;
    parse_library_parts(
        source_name,
        sources,
        &view,
        simulator,
        None,
        Vec::new(),
        Vec::new(),
    )
}

/// Archive a linked standalone HSPICE source graph without caller-specific
/// model selection or parameter binding.
///
/// Reuse the parsed library identity
/// and admitted source span contract; do not run SIMPLIS specialization.
///
/// # Errors
/// Incomplete source graph, malformed hierarchy or unresolved resources.
pub fn parse_standalone_library(
    source_name: String,
    source_names: BTreeSet<String>,
    context: crate::source_dialect::DialectContext,
    statements: Vec<crate::source_dialect::ScopedStatement>,
) -> Result<ParsedLibrary> {
    let mut scopes = Vec::new();
    for line in &statements {
        let mut fields = line.text.split_ascii_whitespace();
        let command = fields.next().unwrap_or_default().to_ascii_uppercase();
        match command.as_str() {
            ".SUBCKT" => {
                anyhow::ensure!(
                    scopes.len() < 128,
                    "HSPICE library hierarchy depth exceeded"
                );
                scopes.push(fields.next().context("missing HSPICE subcircuit name")?);
            }
            ".ENDS" => {
                let name = scopes.pop().context("unmatched HSPICE .ENDS")?;
                anyhow::ensure!(
                    fields
                        .next()
                        .is_none_or(|closing| closing.eq_ignore_ascii_case(name)),
                    "mismatched HSPICE .ENDS {name}"
                );
            }
            ".INC" | ".INCLUDE" | ".INCPSLT" | ".LIB" | ".ENDL" => bail!(
                "unresolved standalone HSPICE library resource at {}:{}",
                line.source,
                line.first_line
            ),
            ".CONTROL" | ".ENDC" | ".FILE" | ".ENDF" | ".END" => {
                bail!("invalid HSPICE library directive {command}")
            }
            _ => (),
        }
    }
    anyhow::ensure!(scopes.is_empty(), "unclosed HSPICE library hierarchy");
    let simulator = LibrarySimulator::Hspice;
    let dialect = Some(context);
    let source_id = parsed_library_digest(simulator, dialect, &[], &statements, &[], &[], &[]);
    let parsed = ParsedLibrary {
        format_version: PARSED_LIBRARY_FORMAT_VERSION,
        simulator,
        dialect,
        device_identities: Vec::new(),
        source_id,
        scoped_statements: statements,
        source_name,
        source_names,
        support_lines: Vec::new(),
        aliases: Vec::new(),
        definitions: Vec::new(),
    };
    parsed.validate()?;
    Ok(parsed)
}

/// Parse an admitted, scoped source graph whose individual resources have
/// already been projected using their own language context.
///
/// # Errors
/// Invalid source identities, hierarchy, or exceeded expansion budget.
pub fn parse_scoped_library(
    source_name: String,
    source_names: BTreeSet<String>,
    dialect: crate::source_dialect::DialectContext,
    statements: &[crate::source_dialect::ScopedStatement],
    limit: usize,
) -> Result<ParsedLibrary> {
    parse_scoped_parts(
        source_name,
        source_names,
        dialect,
        statements,
        statements.to_vec(),
        limit,
    )
}

/// Lower each source's lexical dialect without losing its neutral card text.
///
/// # Errors
/// Invalid expression or output byte budget exceeded.
pub fn lower_scoped_statements(
    statements: &[crate::source_dialect::ScopedStatement],
    limit: usize,
) -> Result<Vec<crate::source_dialect::ScopedStatement>> {
    let mut remaining = limit;
    let mut opaque = crate::source_dialect::OpaqueSource::default();
    statements
        .iter()
        .map(|statement| {
            let mut statement = statement.clone();
            if !opaque.consume(&statement.text) {
                statement.text = crate::source_dialect::lower_statement(
                    &statement.text,
                    statement.context,
                    remaining,
                )?;
            }
            remaining = remaining
                .checked_sub(statement.text.len().saturating_add(1))
                .context("source dialect output byte budget exceeded")?;
            Ok(statement)
        })
        .collect()
}

fn parse_scoped_parts(
    source_name: String,
    source_names: BTreeSet<String>,
    dialect: crate::source_dialect::DialectContext,
    statements: &[crate::source_dialect::ScopedStatement],
    original: Vec<crate::source_dialect::ScopedStatement>,
    limit: usize,
) -> Result<ParsedLibrary> {
    use crate::source_dialect::resolve_device_identities;
    anyhow::ensure!(
        source_names.contains(&source_name)
            && statements
                .iter()
                .all(|line| source_names.contains(&line.source)),
        "incomplete admitted source graph"
    );
    let lowered = lower_scoped_statements(statements, limit)?;
    let resolved = resolve_device_identities(&lowered)?;
    anyhow::ensure!(
        resolved.text.len() <= limit,
        "device identity expansion byte budget exceeded"
    );
    parse_library_parts(
        source_name,
        source_names,
        &resolved.text,
        LibrarySimulator::Simetrix,
        Some(dialect),
        resolved.identities,
        original,
    )
}

fn parse_library_parts(
    source_name: String,
    source_names: BTreeSet<String>,
    selected_view: &str,
    simulator: LibrarySimulator,
    dialect: Option<crate::source_dialect::DialectContext>,
    device_identities: Vec<crate::source_dialect::DeviceIdentity>,
    scoped_statements: Vec<crate::source_dialect::ScopedStatement>,
) -> Result<ParsedLibrary> {
    let parsed = extract_definitions(selected_view)?;
    let (support_lines, aliases) = parse_library_root(&parsed.root_lines)?;
    let mut definitions = parsed.definitions;
    for definition in &mut definitions {
        definition.source_id = None;
        definition.source_name = None;
    }
    let source_id = parsed_library_digest(
        simulator,
        dialect,
        &device_identities,
        &scoped_statements,
        &support_lines,
        &aliases,
        &definitions,
    );
    Ok(ParsedLibrary {
        format_version: PARSED_LIBRARY_FORMAT_VERSION,
        simulator,
        dialect,
        device_identities,
        source_id,
        scoped_statements,
        source_name,
        source_names,
        support_lines,
        aliases,
        definitions,
    })
}

/// Select the requested simulator projection before source interpretation.
///
/// # Errors
///
/// Invalid source encoding, selector or subcircuit nesting.
pub fn project_source(input: &[u8], simulator: LibrarySimulator) -> Result<String> {
    let text = decode_simplis_text(input)?;
    select_library_view(&text, simulator)
}

/// Project one admitted source using its inherited file language, before
/// linking it into the parent. The returned context is passed to children.
///
/// # Errors
/// Invalid source encoding, simulator view, or source expression.
pub fn project_simetrix_source(
    input: &[u8],
    inherited: crate::source_dialect::DialectContext,
    kind: crate::source_dialect::SourceKind,
) -> Result<(crate::source_dialect::DialectContext, String)> {
    let (context, statements) =
        project_resource(input, inherited, kind, LibrarySimulator::Simetrix)?;
    Ok((context, render_projection(&statements)))
}

/// Render mapped statements without altering their logical boundaries.
#[must_use = "the result is the selected source text"]
pub fn render_projection(statements: &[crate::source_dialect::ProjectedStatement]) -> String {
    let mut output = String::new();
    for statement in statements {
        output.push_str(&statement.text);
        output.push('\n');
    }
    output
}

/// Project a resource with exact admitted physical source spans.
///
/// Only a complete `SIMetrix` deck has a mandatory first-line title. Included
/// files and SIMPLIS source streams retain their first electrical statement.
/// # Errors
/// Invalid source encoding, continuation, simulator view or source nesting.
pub fn project_resource(
    input: &[u8],
    inherited: crate::source_dialect::DialectContext,
    kind: crate::source_dialect::SourceKind,
    simulator: LibrarySimulator,
) -> Result<(
    crate::source_dialect::DialectContext,
    Vec<crate::source_dialect::ProjectedStatement>,
)> {
    use crate::source_dialect::{DialectContext, SourceKind, logical_source};
    if simulator == LibrarySimulator::Hspice {
        return project_spice_resource(input, inherited, kind);
    }
    let decoded = if kind == SourceKind::Deck && simulator == LibrarySimulator::Simetrix {
        // A title is data, even when it looks like a directive or a device.
        let mut protected = Vec::with_capacity(input.len().saturating_add(2));
        let first = input
            .split(|byte| *byte == b'\n')
            .next()
            .unwrap_or_default()
            .trim_ascii_start();
        if !first.starts_with(b"*") || first.starts_with(b"*#") {
            protected.extend_from_slice(b"* ");
        }
        protected.extend_from_slice(input);
        decode_simplis_text(&protected)?
    } else {
        decode_simplis_text(input)?
    };
    let context = if simulator == LibrarySimulator::Simetrix {
        DialectContext::resolve(&decoded, inherited, kind)
    } else {
        inherited
    };
    let statements = logical_source(&decoded, context)?;
    let selected =
        selected_library_lines(statements.iter().map(|line| line.text.as_str()), simulator)?;
    let selected = selected.into_iter().collect::<BTreeSet<_>>();
    Ok((
        context,
        statements
            .into_iter()
            .enumerate()
            .filter_map(|(i, line)| selected.contains(&i).then_some(line))
            .collect(),
    ))
}

/// Project a standalone SPICE-family resource.
///
/// Unlike multi-simulator projection, the deck title is retained verbatim
/// and cannot participate in comment, continuation, or directive processing.
///
/// # Errors
/// Invalid text or orphan continuation in the resource.
pub fn project_spice_resource(
    input: &[u8],
    inherited: crate::source_dialect::DialectContext,
    kind: crate::source_dialect::SourceKind,
) -> Result<(
    crate::source_dialect::DialectContext,
    Vec<crate::source_dialect::ProjectedStatement>,
)> {
    use crate::source_dialect::{DialectContext, ProjectedStatement, SourceKind, logical_source};
    let decoded = decode_simplis_text(input)?;
    let context = DialectContext::resolve(&decoded, inherited, kind);
    if kind == SourceKind::Include {
        return Ok((context, logical_source(&decoded, context)?));
    }
    let mut lines = decoded.splitn(2, '\n');
    let title = lines.next().context("SPICE deck has no title")?;
    ensure!(!input.is_empty(), "SPICE deck has no title");
    let mut statements = vec![ProjectedStatement {
        text: title.trim_end_matches('\r').to_owned(),
        first_line: 1,
        last_line: 1,
    }];
    for mut statement in logical_source(lines.next().unwrap_or_default(), context)? {
        statement.first_line += 1;
        statement.last_line += 1;
        statements.push(statement);
    }
    Ok((context, statements))
}

/// Rewrite parameter references in an expression or device value field list,
/// excluding assignment destinations, function names and electrical probes.
///
/// # Errors
/// Malformed strings or probe syntax.
pub fn rewrite_parameter_symbols(
    expression: &str,
    names: &BTreeMap<String, String>,
    preserve_assignments: bool,
) -> Result<String> {
    runtime_params::rewrite_symbols(expression, names, preserve_assignments)
}

/// Lower an expression in its original resource dialect. The caller must
/// isolate value syntax from device, node, model and resource identifiers.
///
/// # Errors
/// Invalid expression syntax or exceeded output byte budget.
pub fn lower_spice_expression(
    expression: &str,
    context: crate::source_dialect::DialectContext,
    limit: usize,
) -> Result<String> {
    crate::source_dialect::lower_expression(expression, context.language, limit)
}

/// Lower expressions only after simulator and named-section selection.
///
/// # Errors
/// Invalid expression syntax or exceeded output byte budget.
pub fn lower_simetrix_source_view(
    selected: &str,
    context: crate::source_dialect::DialectContext,
    limit: usize,
) -> Result<String> {
    use crate::source_dialect::{OpaqueSource, lower_statement};
    use std::fmt::Write as _;
    let mut output = String::new();
    let mut opaque = OpaqueSource::default();
    for statement in selected.lines() {
        let lowered = if opaque.consume(statement) {
            statement.to_owned()
        } else {
            lower_statement(statement, context, limit.saturating_sub(output.len()))?
        };
        anyhow::ensure!(
            lowered.len().saturating_add(1) <= limit.saturating_sub(output.len()),
            "source dialect output byte budget exceeded"
        );
        writeln!(output, "{lowered}")?;
    }
    opaque.finish()?;
    Ok(output)
}

fn select_library_view(input: &str, simulator: LibrarySimulator) -> Result<String> {
    let selected = selected_library_lines(input.lines(), simulator)?;
    let lines = input.lines().collect::<Vec<_>>();
    let mut output = String::new();
    for index in selected {
        output.push_str(lines[index]);
        output.push('\n');
    }
    Ok(output)
}

fn selected_library_lines<'a>(
    lines: impl Iterator<Item = &'a str>,
    simulator: LibrarySimulator,
) -> Result<Vec<usize>> {
    let target = simulator.as_str().to_ascii_uppercase();
    let mut output = Vec::new();
    let mut enabled = true;
    let mut subcircuit_depth = 0usize;
    let mut opaque = crate::source_dialect::OpaqueSource::default();
    for (line_index, line) in lines.enumerate() {
        if opaque.consume(line) {
            if enabled {
                output.push(line_index);
            }
            continue;
        }
        if is_directive(line, "SIMULATOR") {
            if subcircuit_depth != 0 {
                bail!(
                    ".SIMULATOR inside a subcircuit is unsupported at line {}",
                    line_index.saturating_add(1)
                );
            }
            let fields = line.split_whitespace().collect::<Vec<_>>();
            if fields.len() != 2 {
                bail!(
                    "malformed .SIMULATOR directive at line {}",
                    line_index.saturating_add(1)
                );
            }
            let selector = fields[1].to_ascii_uppercase();
            if selector == "DEFAULT" {
                enabled = true;
                continue;
            }
            let names = selector.split('|').collect::<Vec<_>>();
            if !names
                .iter()
                .all(|name| matches!(*name, "SIMPLIS" | "SIMETRIX"))
            {
                bail!("unsupported simulator selector {selector}");
            }
            enabled = names.iter().any(|name| *name == target);
            continue;
        }
        if is_subcircuit_header(line) {
            subcircuit_depth = subcircuit_depth.saturating_add(1);
        } else if is_directive(line, "ENDS") && subcircuit_depth != 0 {
            subcircuit_depth -= 1;
        }
        if enabled {
            output.push(line_index);
        }
    }
    opaque.finish()?;
    if subcircuit_depth != 0 {
        bail!(
            "unterminated subcircuit while selecting {} library view",
            simulator.as_str()
        );
    }
    Ok(output)
}

fn parse_library_root(root_lines: &[String]) -> Result<(Vec<String>, Vec<Alias>)> {
    let mut support_lines = Vec::new();
    let mut aliases = Vec::new();
    for statement in logical_statements(root_lines) {
        let trimmed = statement.trim();
        if trimmed.is_empty()
            || trimmed.starts_with('*')
            || trimmed.starts_with(';')
            || trimmed.starts_with('$')
            || trimmed.starts_with('/')
        {
            continue;
        }
        if is_directive(trimmed, "SYSTEMLIB")
            || is_directive(trimmed, "SIMULATOR")
            || is_directive(trimmed, "END")
            || trimmed.eq_ignore_ascii_case(".subckt")
        {
            continue;
        }
        if is_directive(trimmed, "ALIAS") {
            let fields = trimmed.split_whitespace().collect::<Vec<_>>();
            if fields.len() != 4 {
                bail!("malformed SIMPLIS .ALIAS statement: {trimmed}");
            }
            aliases.push(Alias {
                name: fields[1].to_owned(),
                target: fields[2].to_owned(),
                kind: fields[3].to_owned(),
            });
            continue;
        }
        support_lines.push(statement);
    }
    Ok((support_lines, aliases))
}

fn parsed_library_digest(
    simulator: LibrarySimulator,
    dialect: Option<crate::source_dialect::DialectContext>,
    identities: &[crate::source_dialect::DeviceIdentity],
    statements: &[crate::source_dialect::ScopedStatement],
    support_lines: &[String],
    aliases: &[Alias],
    definitions: &[Definition],
) -> String {
    let semantic_digest = library_digest(support_lines, aliases, definitions);
    let mut hasher = Sha256::new();
    hasher.update(b"ngspice-rs-preprocess-parsed-library-v6\0");
    if let Some(context) = dialect {
        hasher.update([match context.language {
            crate::source_dialect::SourceLanguage::Simetrix => 1,
            crate::source_dialect::SourceLanguage::Hspice => 2,
            crate::source_dialect::SourceLanguage::Pspice => 3,
        }]);
        hasher.update(context.separator.map_or(0, u32::from).to_be_bytes());
    } else {
        hasher.update([0]);
    }
    // Device display semantics affect the artifact identity but its original
    // pathname must not: identical resources can be relocated.
    for identity in identities {
        for scope in &identity.scope {
            hasher.update(scope.as_bytes());
            hasher.update([0]);
        }
        hasher.update([0xff]);
        for name in [&identity.declared, &identity.displayed, &identity.linked] {
            hasher.update(name.as_bytes());
            hasher.update([0]);
        }
        hasher.update([0xfe]);
    }
    for statement in statements {
        hasher.update([match statement.context.language {
            crate::source_dialect::SourceLanguage::Simetrix => 1,
            crate::source_dialect::SourceLanguage::Hspice => 2,
            crate::source_dialect::SourceLanguage::Pspice => 3,
        }]);
        hasher.update(
            statement
                .context
                .separator
                .map_or(0, u32::from)
                .to_be_bytes(),
        );
        hasher.update(statement.text.len().to_string().as_bytes());
        hasher.update([0]);
        hasher.update(statement.text.as_bytes());
    }
    hasher.update(simulator.as_str().as_bytes());
    hasher.update([0]);
    hasher.update(semantic_digest.as_bytes());
    lower_hex(&hasher.finalize())
}

fn library_digest(
    support_lines: &[String],
    aliases: &[Alias],
    definitions: &[Definition],
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"support\0");
    for statement in support_lines {
        hasher.update(statement.replace('\r', "").as_bytes());
        hasher.update([0xff]);
    }
    hasher.update(b"aliases\0");
    for alias in aliases {
        hasher.update(alias.name.to_ascii_uppercase().as_bytes());
        hasher.update([0]);
        hasher.update(alias.target.to_ascii_uppercase().as_bytes());
        hasher.update([0]);
        hasher.update(alias.kind.to_ascii_uppercase().as_bytes());
        hasher.update([0xff]);
    }
    hasher.update(b"definitions\0");
    for definition in definitions {
        hasher.update(definition.name.to_ascii_uppercase().as_bytes());
        hasher.update([0]);
        hasher.update(definition.scope_id.as_bytes());
        hasher.update([0]);
        if let Some(owner_scope_id) = &definition.owner_scope_id {
            hasher.update(owner_scope_id.as_bytes());
        }
        hasher.update([0]);
        hasher.update(normalize_definition(&definition.bytes));
        hasher.update([0xff]);
    }
    lower_hex(&hasher.finalize())
}

fn normalize_definition(bytes: &[u8]) -> Vec<u8> {
    bytes
        .iter()
        .copied()
        .filter(|byte| *byte != b'\r')
        .collect()
}

fn definition_scope_id(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"simplis-lexical-scope-v1\0");
    hasher.update(normalize_definition(bytes));
    lower_hex(&hasher.finalize())
}

fn normalize_definition_for_equivalence(bytes: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(bytes.len());
    for line in bytes.split(|byte| *byte == b'\n') {
        let start = line
            .iter()
            .position(|byte| !matches!(*byte, b' ' | b'\t' | b'\r'))
            .unwrap_or(line.len());
        let end = line
            .iter()
            .rposition(|byte| !matches!(*byte, b' ' | b'\t' | b'\r'))
            .map_or(start, |index| index.saturating_add(1));
        output.extend_from_slice(&line[start..end]);
        output.push(b'\n');
    }
    output
}

fn insert_local_definition(
    catalog: &mut SubcircuitCatalog,
    definition: Definition,
) -> Result<bool> {
    let key = definition.name.to_ascii_uppercase();
    let definitions = catalog.definitions.entry(key).or_default();
    let same_scope = definitions
        .iter()
        .filter(|existing| existing.owner_scope_id == definition.owner_scope_id)
        .collect::<Vec<_>>();
    if same_scope.iter().any(|existing| {
        normalize_definition_for_equivalence(&existing.bytes)
            == normalize_definition_for_equivalence(&definition.bytes)
    }) {
        return Ok(false);
    }
    if !same_scope.is_empty() {
        bail!(
            "conflicting duplicate local SIMPLIS subcircuit {} in lexical scope",
            definition.name
        );
    }
    definitions.push(definition);
    Ok(true)
}

#[derive(Clone, Debug)]
struct Extracted {
    root_lines: Vec<String>,
    definitions: Vec<Definition>,
}

fn directive_name(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    let rest = trimmed.strip_prefix('.')?;
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    Some(&rest[..end])
}

fn is_directive(line: &str, name: &str) -> bool {
    directive_name(line).is_some_and(|value| value.eq_ignore_ascii_case(name))
}

fn is_subcircuit_header(line: &str) -> bool {
    let trimmed = line.trim_start();
    let Some(rest) = trimmed.strip_prefix('.') else {
        return false;
    };
    let Some((directive, tail)) = rest.split_once(char::is_whitespace) else {
        return false;
    };
    directive.eq_ignore_ascii_case("SUBCKT") && !tail.trim().is_empty()
}

fn subcircuit_name(header: &str) -> Result<String> {
    let collapsed = collapse_continuations(header);
    let mut fields = collapsed.split_whitespace();
    let directive = fields.next().context("empty SIMPLIS .SUBCKT header")?;
    if !directive.eq_ignore_ascii_case(".SUBCKT") {
        bail!("expected SIMPLIS .SUBCKT header");
    }
    fields
        .next()
        .map(str::to_owned)
        .context("SIMPLIS .SUBCKT has no name")
}

fn header_is_parameterized(header: &str) -> bool {
    find_ascii_case_insensitive(&collapse_continuations(header), "vars:").is_some()
}

fn collapse_continuations(input: &str) -> String {
    let mut output = String::new();
    for (index, line) in input.lines().enumerate() {
        if index != 0 {
            output.push(' ');
        }
        let trimmed = line.trim_start();
        output.push_str(trimmed.strip_prefix('+').unwrap_or(trimmed).trim());
    }
    output
}

fn extract_definitions(input: &str) -> Result<Extracted> {
    extract_definitions_scoped(input, None)
}

fn extract_definitions_scoped(input: &str, owner_scope_id: Option<&str>) -> Result<Extracted> {
    let lines: Vec<&str> = input.lines().collect();
    let mut root_lines = Vec::new();
    let mut definitions = Vec::new();
    let mut index = 0usize;
    while index < lines.len() {
        if !is_subcircuit_header(lines[index]) {
            root_lines.push(lines[index].to_owned());
            index += 1;
            continue;
        }
        let start = index;
        let mut depth = 0usize;
        let mut header_end = start + 1;
        while lines
            .get(header_end)
            .is_some_and(|line| line.trim_start().starts_with('+'))
        {
            header_end += 1;
        }
        let mut header = String::new();
        for line in &lines[start..header_end] {
            header.push_str(line);
            header.push('\n');
        }
        loop {
            let line = lines
                .get(index)
                .with_context(|| format!("unterminated SIMPLIS .SUBCKT at line {}", start + 1))?;
            if is_subcircuit_header(line) {
                depth = depth.saturating_add(1);
            } else if is_directive(line, "ENDS") {
                depth = depth.checked_sub(1).context("unmatched SIMPLIS .ENDS")?;
                if depth == 0 {
                    index += 1;
                    break;
                }
            }
            index += 1;
        }
        let end_index = index
            .checked_sub(1)
            .context("SIMPLIS .SUBCKT ended before it started")?;

        let mut full_raw = String::new();
        for line in &lines[start..index] {
            full_raw.push_str(line);
            full_raw.push('\n');
        }
        let scope_id = definition_scope_id(full_raw.as_bytes());

        let mut nested_text = String::new();
        for line in &lines[header_end..end_index] {
            nested_text.push_str(line);
            nested_text.push('\n');
        }
        let nested = extract_definitions_scoped(&nested_text, Some(&scope_id))?;

        let mut raw = String::new();
        for line in &lines[start..header_end] {
            raw.push_str(line);
            raw.push('\n');
        }
        for line in nested.root_lines {
            raw.push_str(&line);
            raw.push('\n');
        }
        raw.push_str(lines[end_index]);
        raw.push('\n');
        definitions.push(Definition {
            name: subcircuit_name(&header)?,
            parameterized: header_is_parameterized(&header),
            scope_id,
            owner_scope_id: owner_scope_id.map(str::to_owned),
            bytes: raw.into_bytes(),
            source_id: None,
            source_name: None,
        });
        definitions.extend(nested.definitions);
    }
    Ok(Extracted {
        root_lines,
        definitions,
    })
}

fn logical_statements(lines: &[String]) -> Vec<String> {
    let mut statements = Vec::<String>::new();
    for line in lines {
        let trimmed = line.trim_start();
        if trimmed.starts_with('+') {
            let continuation = trimmed.trim_start_matches('+').trim_start();
            if let Some(statement) = statements.iter_mut().rev().find(|statement| {
                let candidate = statement.trim_start();
                !candidate.is_empty() && !candidate.starts_with('*')
            }) {
                statement.push(' ');
                statement.push_str(continuation);
                continue;
            }
        }
        statements.push(line.to_owned());
    }
    statements
}

fn parse_overrides(input: &str) -> Result<Environment> {
    let mut environment = Environment::new();
    parse_parameter_list(input, &mut environment)?;
    Ok(environment)
}

#[derive(Clone, Debug)]
struct Pending {
    definition: Definition,
    generated_name: String,
    overrides: Environment,
    local_key: Option<String>,
}

/// Portion of a logical statement preceding an unquoted semicolon comment.
#[must_use = "the active prefix excludes non-executable source text"]
pub fn active_statement_prefix(statement: &str) -> &str {
    crate::source_dialect::active_prefix(statement, ';')
}

/// Identify the assignment tail of a charge-defined capacitor, excluding comments.
/// A capacitance value and a charge expression are distinct device forms.
#[must_use = "the returned tail identifies the charge-defined capacitor law"]
pub fn capacitor_charge_assignments(statement: &str) -> Option<&str> {
    let active = active_statement_prefix(statement).trim_start();
    let mut fields = active.split_whitespace();
    if !fields
        .next()?
        .as_bytes()
        .first()?
        .eq_ignore_ascii_case(&b'C')
    {
        return None;
    }
    fields.next()?;
    let last_node = fields.next()?;
    let end = last_node.as_ptr().addr() - active.as_ptr().addr() + last_node.len();
    let tail = active[end..].trim_start();
    let (key, _) = tail.split_once('=')?;
    key.trim().eq_ignore_ascii_case("Q").then_some(tail)
}

fn legacy_colon_call(trimmed: &str) -> Result<Option<(&str, &str, bool)>> {
    let Some(colon) = find_ascii_case_insensitive(trimmed, " : ") else {
        return Ok(None);
    };
    let before = trimmed[..colon].trim_end();
    let model = before
        .split_whitespace()
        .next_back()
        .context("legacy schematic call before parameter separator has no model")?;
    let overrides = trimmed[colon + 3..].trim();
    if overrides.is_empty() {
        bail!("legacy schematic parameter separator has no assignments");
    }
    Ok(Some((model, overrides, false)))
}

fn call_target(statement: &str) -> Result<Option<(&str, &str, bool)>> {
    let trimmed = active_statement_prefix(statement).trim_start();
    if !trimmed
        .as_bytes()
        .first()
        .is_some_and(|byte| matches!(*byte, b'X' | b'x'))
    {
        return Ok(None);
    }
    let tokens = trimmed.split_whitespace().collect::<Vec<_>>();
    let metadata = |token: &str| {
        token.eq_ignore_ascii_case("vars:")
            || token.eq_ignore_ascii_case("params:")
            || token.eq_ignore_ascii_case("pinnames:")
    };
    if let Some(pinnames_index) = tokens
        .iter()
        .position(|token| token.eq_ignore_ascii_case("pinnames:"))
    {
        let pin_count = tokens[pinnames_index + 1..]
            .iter()
            .take_while(|token| !metadata(token))
            .count();
        let model_index = 1usize.saturating_add(pin_count);
        let first_metadata = tokens
            .iter()
            .position(|token| metadata(token))
            .unwrap_or(tokens.len());
        let Some(model) = tokens
            .get(model_index)
            .copied()
            .filter(|_| model_index < first_metadata)
        else {
            return Ok(None);
        };
        if model.contains('=') {
            return Ok(None);
        }
        if tokens[model_index + 1..pinnames_index].contains(&":") {
            let colon = find_ascii_case_insensitive(trimmed, " : ")
                .context("legacy schematic parameter separator has no source span")?;
            let pinnames = find_ascii_case_insensitive(trimmed, " pinnames:")
                .context("legacy schematic call has no pinnames source span")?;
            if colon >= pinnames {
                bail!("legacy schematic parameter separator follows pinnames metadata");
            }
            let overrides = trimmed[colon + 3..pinnames].trim();
            if overrides.is_empty() {
                bail!("legacy schematic parameter separator has no assignments");
            }
            return Ok(Some((model, overrides, false)));
        }
        for (marker, compile_time) in [(" vars:", true), (" params:", false)] {
            if let Some(offset) = find_ascii_case_insensitive(trimmed, marker) {
                let rest = &trimmed[offset + marker.len()..];
                let end = [" pinnames:", " vars:", " params:"]
                    .iter()
                    .filter_map(|next| find_ascii_case_insensitive(rest, next))
                    .min()
                    .unwrap_or(rest.len());
                return Ok(Some((model, rest[..end].trim(), compile_time)));
            }
        }
        let start = model.as_ptr().addr() - trimmed.as_ptr().addr() + model.len();
        let end = tokens[pinnames_index].as_ptr().addr() - trimmed.as_ptr().addr();
        return Ok(Some((model, trimmed[start..end].trim(), false)));
    }
    for (marker, compile_time) in [(" vars:", true), (" params:", false)] {
        if let Some(offset) = find_ascii_case_insensitive(trimmed, marker) {
            let before = trimmed[..offset].trim_end();
            let model = before
                .split_whitespace()
                .next_back()
                .context("SIMPLIS X instance before parameter metadata has no model")?;
            let overrides = trimmed[offset + marker.len()..].trim();
            return Ok(Some((model, overrides, compile_time)));
        }
    }
    if let Some(call) = legacy_colon_call(trimmed)? {
        return Ok(Some(call));
    }
    plain_call_target(trimmed, &tokens)
}

fn plain_call_target<'a>(
    trimmed: &'a str,
    fields: &[&'a str],
) -> Result<Option<(&'a str, &'a str, bool)>> {
    if let Some(first_assignment) = fields.iter().position(|token| token.contains('=')) {
        if first_assignment <= 1 {
            return Ok(None);
        }
        let assignment = first_assignment - usize::from(fields[first_assignment].starts_with('='));
        if assignment <= 1 {
            return Ok(None);
        }
        let model = fields[assignment - 1];
        let overrides =
            trimmed[model.as_ptr().addr() - trimmed.as_ptr().addr() + model.len()..].trim_start();
        if overrides.is_empty() {
            bail!("inline-parameterized SIMPLIS X instance has no assignments");
        }
        return Ok(Some((model, overrides, false)));
    }
    let Some(model) = fields.last().copied() else {
        return Ok(None);
    };
    Ok(Some((model, "", false)))
}

fn resolve_definition(
    model: &str,
    local: &SubcircuitCatalog,
    external: &SubcircuitCatalog,
    preferred_source: Option<&str>,
    preferred_scope: Option<&str>,
) -> Result<Option<(Definition, bool)>> {
    if let (Some(source_id), Some(owner_scope_id)) = (preferred_source, preferred_scope) {
        if let Some(definition) = local.resolve_scoped_direct(model, source_id, owner_scope_id)? {
            return Ok(Some((definition.clone(), true)));
        }
        if let Some(definition) =
            external.resolve_scoped_direct(model, source_id, owner_scope_id)?
        {
            return Ok(Some((definition.clone(), false)));
        }
    }

    if let Some(source_id) = preferred_source {
        if local.contains_source_id(source_id)
            && let Some(definition) = local.resolve(model, Some(source_id))?
        {
            return Ok(Some((definition.clone(), true)));
        }
        if external.contains_source_id(source_id)
            && let Some(definition) = external.resolve(model, Some(source_id))?
        {
            return Ok(Some((definition.clone(), false)));
        }
    }

    if let Some(definition) = local.resolve(model, None)? {
        return Ok(Some((definition.clone(), true)));
    }
    Ok(external
        .resolve(model, None)?
        .map(|definition| (definition.clone(), false)))
}

// A plain wrapper may call a parameterized or controlled helper several
// levels down. Detect that reachability rather than only inspecting the
// wrapper itself. The visited set also makes this query finite on cycles.
fn definition_needs_expansion(
    definition: &Definition,
    local: &SubcircuitCatalog,
    external: &SubcircuitCatalog,
) -> Result<bool> {
    let mut pending = vec![definition.clone()];
    let mut visited = BTreeSet::new();
    while let Some(current) = pending.pop() {
        if !visited.insert((current.source_id.clone(), current.scope_id.clone())) {
            continue;
        }
        if current.parameterized
            || current.owner_scope_id.is_some()
            || definition_uses_preprocessor(&current)
        {
            return Ok(true);
        }
        let text = std::str::from_utf8(&current.bytes)?;
        let lines = text.lines().map(str::to_owned).collect::<Vec<_>>();
        for statement in logical_statements(&lines) {
            let Some((model, _, explicit_vars)) = call_target(&statement)? else {
                continue;
            };
            if explicit_vars {
                return Ok(true);
            }
            if let Some((child, _)) = resolve_definition(
                model,
                local,
                external,
                current.source_id.as_deref(),
                Some(&current.scope_id),
            )? {
                pending.push(child);
            }
        }
    }
    Ok(false)
}

fn rewrite_call(
    statement: &str,
    local: &SubcircuitCatalog,
    external: &SubcircuitCatalog,
    next_id: &mut usize,
    caller: (Option<&str>, Option<&str>),
) -> Result<Option<(String, Pending)>> {
    let Some((model, override_text, explicit_vars)) = call_target(statement)? else {
        return Ok(None);
    };
    let Some((definition, is_local)) =
        resolve_definition(model, local, external, caller.0, caller.1)?
    else {
        if explicit_vars {
            bail!("unresolved parameterized SIMPLIS subcircuit {model}");
        }
        return Ok(None);
    };
    if !explicit_vars && !definition_needs_expansion(&definition, local, external)? {
        return Ok(None);
    }
    *next_id = next_id.saturating_add(1);
    let generated_name = format!("{}${}", definition.name, *next_id);
    let compile_time_vars = explicit_vars || definition.parameterized;
    let rewritten = replace_call_target(statement, model, &generated_name, compile_time_vars)?;
    let local_key = is_local.then(|| definition.scope_id.clone());
    Ok(Some((
        rewritten,
        Pending {
            definition,
            generated_name,
            overrides: if compile_time_vars {
                parse_overrides(override_text)?
            } else {
                Environment::new()
            },
            local_key,
        },
    )))
}

fn rename_specialized(bytes: &[u8], generated_name: &str) -> Result<(String, Vec<String>)> {
    let text = std::str::from_utf8(bytes)?;
    let mut lines = text.lines();
    let header = lines
        .next()
        .context("specialized SIMPLIS subcircuit is empty")?;
    let collapsed = collapse_continuations(header);
    let mut fields = collapsed.split_whitespace();
    let directive = fields
        .next()
        .context("specialized SIMPLIS header is empty")?;
    if !directive.eq_ignore_ascii_case(".SUBCKT") {
        bail!("specialized SIMPLIS output does not begin with .SUBCKT");
    }
    let _old_name = fields
        .next()
        .context("specialized SIMPLIS .SUBCKT has no name")?;
    let nodes = fields.collect::<Vec<_>>().join(" ");
    let mut body = lines.map(str::to_owned).collect::<Vec<_>>();
    let ends = body
        .pop()
        .context("specialized SIMPLIS output has no .ENDS")?;
    if !is_directive(&ends, "ENDS") {
        bail!("specialized SIMPLIS output does not end with .ENDS");
    }
    Ok((format!(".subckt {generated_name} {nodes}"), body))
}

fn override_number(environment: &Environment, name: &str) -> Result<f64> {
    let value = environment
        .get(name)
        .with_context(|| format!("missing Complementary PWM parameter {name}"))?;
    super::Evaluation::new(environment).number(value)
}

fn write_pwm_number(output: &mut String, value: f64) -> Result<()> {
    use std::fmt::Write as _;
    if !value.is_finite() {
        bail!("non-finite Complementary PWM parameter");
    }
    write!(output, "{value:.17e}")?;
    Ok(())
}

fn is_microchip_complementary_pwm(pending: &Pending) -> bool {
    pending
        .definition
        .name
        .eq_ignore_ascii_case("Complemetary_PWM")
        && String::from_utf8_lossy(&pending.definition.bytes)
            .contains("MICROCHIP COMPLEMENTARY_PWM Encryption")
}

fn emit_pwm_delay(
    output: &mut String,
    stem: &str,
    input: &str,
    delayed: &str,
    delay: f64,
) -> Result<()> {
    use std::fmt::Write as _;
    if delay == 0.0 {
        return Ok(());
    }
    let model = format!("__PWM_{stem}_DELAY_M");
    writeln!(output, "A__PWM_{stem}_DELAY {input} {delayed} 0 {model}")?;
    write!(output, ".model {model} delay(delay=")?;
    write_pwm_number(output, delay)?;
    output.push_str(" buffer_size=2048)\n");
    Ok(())
}

fn emit_microchip_complementary_pwm(pending: &Pending, output: &mut String) -> Result<()> {
    use std::fmt::Write as _;

    let frequency = override_number(&pending.overrides, "Frequency")?;
    let phase = override_number(&pending.overrides, "Phase")?;
    let duty_min = override_number(&pending.overrides, "DutyRatioMin")? / 100.0;
    let duty_max = override_number(&pending.overrides, "DutyRatioMax")? / 100.0;
    let rising_dead_time = override_number(&pending.overrides, "RisingDeadTime")?;
    let falling_dead_time = override_number(&pending.overrides, "FallingDeadTime")?;
    let soc = override_number(&pending.overrides, "SOC_CTRL_IN")?;
    let eoc = override_number(&pending.overrides, "EOC_CTRL_IN")?;
    let period = 1.0 / frequency;
    if !(frequency > 0.0
        && matches!(phase, 0.0 | 180.0)
        && (0.0..=1.0).contains(&duty_min)
        && (0.0..=1.0).contains(&duty_max)
        && duty_min <= duty_max
        && rising_dead_time >= 0.0
        && falling_dead_time >= 0.0
        && rising_dead_time < period
        && falling_dead_time < period
        && soc.is_finite()
        && eoc.is_finite()
        && eoc > soc)
    {
        bail!("invalid or unsupported public Microchip Complementary PWM parameters");
    }

    let span = eoc - soc;
    let clamp_low = span.mul_add(duty_min, soc);
    let clamp_high = span.mul_add(duty_max, soc);
    let phase_offset = period * phase / 360.0;
    writeln!(output, ".subckt {} 22 23 24", pending.generated_name)?;
    output.push_str(
        "* ngspice-rs-preprocess: public behavioral replacement for Microchip Complementary PWM\n",
    );
    output.push_str("B__PWM_CTRL __PWM_CTRL 0 V='(v(24)<=");
    write_pwm_number(output, clamp_low)?;
    output.push_str(" ? ");
    write_pwm_number(output, clamp_low)?;
    output.push_str(" : (v(24)>=");
    write_pwm_number(output, clamp_high)?;
    output.push_str(" ? ");
    write_pwm_number(output, clamp_high)?;
    output.push_str(" : v(24)))'\n");
    output.push_str("V__PWM_RAMP __PWM_RAMP 0 PWL(0 ");
    write_pwm_number(output, soc)?;
    output.push(' ');
    write_pwm_number(output, period)?;
    output.push(' ');
    write_pwm_number(output, eoc)?;
    output.push_str(") R=0 TD=-");
    write_pwm_number(output, phase_offset)?;
    output.push('\n');
    output.push_str("E__PWM_DIFF __PWM_DIFF 0 __PWM_CTRL __PWM_RAMP 1\n");
    output.push_str("A__PWM_COMP __PWM_DIFF __PWM_RAW __PWM_COMP_M\n");
    output.push_str(".model __PWM_COMP_M hyst(in_low=0 in_high=0 hyst=0 out_lower_limit=0 out_upper_limit=1 input_domain=0 fraction=false)\n");
    output.push_str("B__PWM_NOT __PWM_NOT 0 V='1-v(__PWM_RAW)'\n");

    let raw_high = if rising_dead_time == 0.0 {
        "__PWM_RAW"
    } else {
        emit_pwm_delay(
            output,
            "RISE",
            "__PWM_RAW",
            "__PWM_RAW_RISE",
            rising_dead_time,
        )?;
        "__PWM_RAW_RISE"
    };
    let raw_low = if falling_dead_time == 0.0 {
        "__PWM_NOT"
    } else {
        emit_pwm_delay(
            output,
            "FALL",
            "__PWM_NOT",
            "__PWM_NOT_FALL",
            falling_dead_time,
        )?;
        "__PWM_NOT_FALL"
    };
    writeln!(output, "B__PWMH 22 0 V='5*v(__PWM_RAW)*v({raw_high})'")?;
    writeln!(output, "B__PWML 23 0 V='5*v(__PWM_NOT)*v({raw_low})'")?;
    writeln!(output, ".ends {}", pending.generated_name)?;
    output.push('\n');
    Ok(())
}

#[derive(Default)]
struct FlatExpansion {
    next_id: usize,
    generated_subcircuits: usize,
    specialized_local: BTreeSet<String>,
    active: Vec<(Option<String>, String, Environment)>,
}

fn emit_pending(
    pending: &Pending,
    local: &SubcircuitCatalog,
    external: &SubcircuitCatalog,
    output: &mut String,
    state: &mut FlatExpansion,
) -> Result<()> {
    if is_microchip_complementary_pwm(pending) {
        emit_microchip_complementary_pwm(pending, output)?;
        state.generated_subcircuits = state.generated_subcircuits.saturating_add(1);
        return Ok(());
    }
    let identity = (
        pending.definition.source_id.clone(),
        pending.definition.scope_id.clone(),
        pending.overrides.clone(),
    );
    ensure!(
        !state.active.contains(&identity),
        "recursive SIMPLIS specialization of {} with unchanged actuals",
        pending.definition.name
    );
    state.active.push(identity);
    let specialized = if pending.definition.parameterized || !pending.overrides.is_empty() {
        specialize_subcircuit(&pending.definition.bytes, &pending.overrides)
    } else {
        specialize_subcircuit_control_flow_with_base(
            &pending.definition.bytes,
            &Environment::new(),
            &pending.overrides,
        )
    }
    .with_context(|| {
        format!(
            "specializing SIMPLIS subcircuit {}",
            pending.definition.name
        )
    })?;
    let (header, body_lines) = rename_specialized(&specialized.bytes, &pending.generated_name)?;
    let body_statements = logical_statements(&body_lines);
    let mut rewritten_body = Vec::new();
    let mut children = Vec::new();
    for statement in body_statements {
        if let Some((rewritten, child)) = rewrite_call(
            &statement,
            local,
            external,
            &mut state.next_id,
            (
                pending.definition.source_id.as_deref(),
                Some(&pending.definition.scope_id),
            ),
        )? {
            if let Some(key) = &child.local_key {
                state.specialized_local.insert(key.clone());
            }
            rewritten_body.push(rewritten);
            children.push(child);
        } else {
            rewritten_body.push(statement);
        }
    }
    output.push_str(&header);
    output.push('\n');
    for statement in rewritten_body {
        output.push_str(&statement);
        output.push('\n');
    }
    state.generated_subcircuits = state.generated_subcircuits.saturating_add(1);
    for child in children {
        emit_pending(&child, local, external, output, state)?;
    }
    output.push_str(".ends ");
    output.push_str(&pending.generated_name);
    output.push_str("\n\n");
    state.active.pop();
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct NetlistPreprocessStats {
    pub root_specializations: usize,
    pub generated_subcircuits: usize,
    pub local_parameterized_definitions_removed: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PreprocessedNetlist {
    pub bytes: Vec<u8>,
    pub stats: NetlistPreprocessStats,
}

/// Admit the root source and expand it using already parsed libraries.
///
/// # Errors
///
/// Source admission, external command, or preprocessing errors.
pub fn preprocess_netlist_file(
    path: &Path,
    external: &SubcircuitCatalog,
    admission: &mut SourceSession,
) -> Result<PreprocessedNetlist> {
    let source = admission.load(path)?;
    preprocess_netlist(&source.bytes, external)
}

/// Expand parameterized SIMPLIS subcircuit calls into static `$N` definitions.
///
/// Root calls are numbered first in source order, matching SIMPLIS deck structure;
/// generated root definitions are emitted in reverse call order, and nested
/// parameterized calls are specialized recursively afterwards.
///
/// # Errors
///
/// Returns an error for conflicting/unresolved model definitions, malformed local
/// subcircuits, unsupported preprocessing expressions/control flow, or malformed
/// parameterized X instances.
pub fn preprocess_netlist(
    input: &[u8],
    external: &SubcircuitCatalog,
) -> Result<PreprocessedNetlist> {
    let text = decode_simplis_text(input)?;
    let mut extracted = extract_definitions(&text)?;
    let (root_support, root_aliases) = parse_library_root(&extracted.root_lines)?;
    let root_source_id = library_digest(&root_support, &root_aliases, &extracted.definitions);
    assign_local_source(&mut extracted.definitions, &root_source_id, "<root>");
    let mut local = SubcircuitCatalog::new();
    let mut retained_local = Vec::new();
    let mut removed_local = 0usize;
    for definition in extracted.definitions {
        let parameterized = definition.parameterized;
        let retained =
            (!parameterized && definition.owner_scope_id.is_none()).then(|| definition.clone());
        if !insert_local_definition(&mut local, definition)? {
            continue;
        }
        if parameterized {
            removed_local = removed_local.saturating_add(1);
        } else if let Some(definition) = retained {
            retained_local.push(definition);
        }
    }

    let root_statements = preprocess_root_statements(&extracted.root_lines)?;
    let mut output_root = Vec::new();
    let mut pending = Vec::new();
    let mut end_statement = None;
    let mut state = FlatExpansion::default();
    for statement in root_statements {
        if is_directive(&statement, "END") {
            end_statement = Some(statement);
            continue;
        }
        if let Some((rewritten, root_pending)) = rewrite_call(
            &statement,
            &local,
            external,
            &mut state.next_id,
            (None, None),
        )? {
            if let Some(key) = &root_pending.local_key {
                state.specialized_local.insert(key.clone());
            }
            output_root.push(rewritten);
            pending.push(root_pending);
        } else {
            output_root.push(statement);
        }
    }

    let root_specializations = pending.len();
    let mut generated = String::new();
    for root_pending in pending.into_iter().rev() {
        emit_pending(&root_pending, &local, external, &mut generated, &mut state)?;
    }

    let mut output = String::new();
    for statement in output_root {
        output.push_str(&statement);
        output.push('\n');
    }
    for definition in retained_local {
        if state.specialized_local.contains(&definition.scope_id) {
            continue;
        }
        output.push_str(std::str::from_utf8(&definition.bytes)?);
        if !output.ends_with('\n') {
            output.push('\n');
        }
    }
    output.push('\n');
    output.push_str(&generated);
    if let Some(end) = end_statement {
        output.push_str(&end);
        output.push('\n');
    }
    Ok(PreprocessedNetlist {
        bytes: output.into_bytes(),
        stats: NetlistPreprocessStats {
            root_specializations,
            generated_subcircuits: state.generated_subcircuits,
            local_parameterized_definitions_removed: removed_local,
        },
    })
}

#[derive(Clone, Debug, Default)]
struct InitTree {
    local: BTreeMap<String, String>,
    children: BTreeMap<String, Self>,
}

impl InitTree {
    fn is_empty(&self) -> bool {
        self.local.is_empty() && self.children.is_empty()
    }

    fn insert(&mut self, path: &[String], value: String) -> Result<()> {
        let Some((head, tail)) = path.split_first() else {
            bail!("empty SIMPLIS .INIT path");
        };
        let head = head.to_ascii_uppercase();
        if tail.is_empty() {
            if let Some(existing) = self.local.insert(head.clone(), value.clone())
                && existing != value
            {
                bail!("conflicting SIMPLIS .INIT values for {head}");
            }
            return Ok(());
        }
        self.children.entry(head).or_default().insert(tail, value)
    }

    fn overlay(&mut self, ancestor: Self) {
        self.local.extend(ancestor.local);
        for (name, child) in ancestor.children {
            self.children.entry(name).or_default().overlay(child);
        }
    }

    fn hash_into(&self, hasher: &mut Sha256) {
        for (name, value) in &self.local {
            hasher.update(*b"L");
            hasher.update(name.as_bytes());
            hasher.update(*b"=");
            hasher.update(value.as_bytes());
            hasher.update([0xff]);
        }
        for (name, child) in &self.children {
            hasher.update(*b"C");
            hasher.update(name.as_bytes());
            hasher.update([0]);
            child.hash_into(hasher);
            hasher.update([0xfe]);
        }
    }

    fn flatten(&self, prefix: &str, output: &mut Vec<String>) {
        for (name, value) in &self.local {
            if prefix.is_empty() {
                output.push(format!("{name}={value}"));
            } else {
                output.push(format!("{prefix}.{name}={value}"));
            }
        }
        for (name, child) in &self.children {
            let next = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}.{name}")
            };
            child.flatten(&next, output);
        }
    }
}

/// Parse a SIMPLIS `.INIT` target into its hierarchical path components.
///
/// Reactive wrappers (`V(...)`/`I(...)`) and the trailing `:IC` selector are syntax, not
/// hierarchy components, so they are stripped before splitting the dotted path.
///
/// # Errors
///
/// Returns an error for malformed reactive wrappers or an empty target.
pub fn parse_init_target_path(name: &str) -> Result<Vec<String>> {
    let raw = name.trim();
    let upper = raw.to_ascii_uppercase();
    let (target, reactive) = if upper.starts_with("V(") || upper.starts_with("I(") {
        let inner = raw[2..]
            .strip_suffix(')')
            .context("malformed reactive SIMPLIS .INIT target")?;
        (inner, Some(upper.as_bytes()[0]))
    } else {
        (raw, None)
    };
    let target = if target.to_ascii_uppercase().ends_with(":IC") {
        &target[..target.len() - 3]
    } else {
        target
    };
    let path = target
        .split('.')
        .map(str::trim)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    anyhow::ensure!(
        path.iter().all(|part| !part.is_empty()
            && part
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'$' | b'!'))),
        "invalid SIMPLIS .INIT path {name}"
    );
    let leaf = path.last().context("empty SIMPLIS .INIT target")?;
    if let Some(kind) = reactive {
        let device = leaf
            .trim_start_matches('!')
            .as_bytes()
            .first()
            .map(u8::to_ascii_uppercase);
        anyhow::ensure!(
            matches!((kind, device), (b'V', Some(b'C')) | (b'I', Some(b'L'))),
            "reactive SIMPLIS .INIT target {name} has wrong device kind"
        );
    }
    Ok(path)
}

type InitAssignment = (Vec<String>, String);

/// Parse all assignments of one logical source .INIT directive, checking
/// reactive target orientation before canonicalizing to the device path.
///
/// # Errors
/// Invalid target, assignment, or expression.
pub fn parse_init_statement(statement: &str) -> Result<Option<Vec<InitAssignment>>> {
    if !is_directive(statement, "INIT") {
        return Ok(None);
    }
    let active = active_statement_prefix(statement).trim_start();
    let body = active
        .get(5..)
        .context("malformed SIMPLIS .INIT directive")?;
    let assignments = crate::named_assignments(body, true)?;
    anyhow::ensure!(!assignments.is_empty(), "empty SIMPLIS .INIT directive");
    assignments
        .into_iter()
        .map(|(name, value)| Ok((parse_init_target_path(name)?, value.to_owned())))
        .collect::<Result<Vec<_>>>()
        .map(Some)
}

fn merge_local_init(bytes: &[u8], inherited: InitTree) -> Result<(Vec<u8>, InitTree)> {
    use std::fmt::Write as _;
    let text = std::str::from_utf8(bytes)?;
    let lines = text.lines().map(str::to_owned).collect::<Vec<_>>();
    let mut local = InitTree::default();
    let mut retained = Vec::new();
    for statement in logical_statements(&lines) {
        if let Some(assignments) = parse_init_statement(&statement)? {
            for (path, value) in assignments {
                local.insert(&path, value)?;
            }
        } else {
            retained.push(statement);
        }
    }
    if local.is_empty() && inherited.is_empty() {
        return Ok((bytes.to_vec(), local));
    }
    local.overlay(inherited);
    let header = retained
        .first()
        .context("SIMPLIS subcircuit with .INIT state has no header")?;
    let mut output = String::new();
    writeln!(output, "{header}")?;
    for (name, value) in &local.local {
        writeln!(output, ".INIT {name}={value}")?;
    }
    for statement in retained.iter().skip(1) {
        writeln!(output, "{statement}")?;
    }
    Ok((output.into_bytes(), local))
}

fn instance_name(statement: &str) -> Option<String> {
    let trimmed = statement.trim_start();
    let first = trimmed.split_whitespace().next()?;
    first
        .as_bytes()
        .first()
        .is_some_and(|byte| matches!(*byte, b'X' | b'x'))
        .then(|| first.to_ascii_uppercase())
}

fn annotate_unlinked_init(statement: &str, init: &InitTree) -> String {
    if init.is_empty() {
        return statement.to_owned();
    }
    let mut entries = Vec::new();
    init.flatten("", &mut entries);
    format!("{statement} ngspice_preprocess_init: {}", entries.join(" "))
}

#[derive(Clone, Debug)]
enum LinkedBody {
    Netlist(Vec<u8>),
    Microchip(Environment),
}

#[derive(Clone, Debug)]
struct LinkedUnit {
    definition: Definition,
    emitted_name: String,
    body: LinkedBody,
    environment: Environment,
    runtime: Option<RuntimeScope>,
    init: InitTree,
}

#[derive(Clone, Debug, Default)]
struct LinkedLibraryBuilder {
    source_name: String,
    support_lines: Vec<String>,
    dependencies: BTreeSet<String>,
    output: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct LinkedGeneratedLibrary {
    pub source_id: String,
    pub source_name: String,
    pub filename: String,
    pub dependencies: Vec<String>,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct LinkedPreprocessedNetlist {
    pub root_bytes: Vec<u8>,
    pub libraries: Vec<LinkedGeneratedLibrary>,
    pub stats: NetlistPreprocessStats,
}

/// Deterministic companion filename for one parsed SIMPLIS library identity.
#[must_use = "the filename is used to link the generated library"]
pub fn linked_library_filename(source_id: &str) -> String {
    format!("simplis-{source_id}.generated.lib")
}

fn update_value_hash(hasher: &mut Sha256, value: &super::Value) {
    match value {
        super::Value::Number(number) => {
            hasher.update(*b"N");
            hasher.update(number.to_bits().to_be_bytes());
        }
        super::Value::Text(text) => {
            hasher.update(*b"T");
            hasher.update(text.as_bytes());
            hasher.update([0]);
        }
        super::Value::Vector(values) => {
            hasher.update(*b"V");
            for item in values {
                update_value_hash(hasher, item);
                hasher.update([0xfe]);
            }
        }
    }
}

fn ngspice_safe_generated_base(name: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::from("sr_");
    for byte in name.bytes() {
        if byte.is_ascii_alphanumeric() || byte == b'_' {
            output.push(char::from(byte));
        } else {
            output.push('_');
            output.push('x');
            output.push(char::from(HEX[usize::from(byte >> 4)]));
            output.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
    output
}

fn linked_specialization_name(
    definition: &Definition,
    specialized: Option<&[u8]>,
    overrides: &Environment,
    init: &InitTree,
) -> String {
    let mut hasher = Sha256::new();
    if let Some(source_id) = &definition.source_id {
        hasher.update(source_id.as_bytes());
    }
    hasher.update([0]);
    hasher.update(definition.name.to_ascii_uppercase().as_bytes());
    hasher.update([0]);
    hasher.update(definition.scope_id.as_bytes());
    hasher.update([0]);
    if let Some(bytes) = specialized {
        hasher.update(normalize_definition(bytes));
    } else {
        for (name, value) in &overrides.values {
            hasher.update(name.as_bytes());
            hasher.update(*b"=");
            update_value_hash(&mut hasher, value);
            hasher.update([0xff]);
        }
    }
    init.hash_into(&mut hasher);
    let digest = lower_hex(&hasher.finalize());
    format!(
        "{}_sp_{}",
        ngspice_safe_generated_base(&definition.name),
        &digest[..24]
    )
}

fn linked_plain_name(definition: &Definition) -> Result<String> {
    let source_id = definition
        .source_id
        .as_deref()
        .context("linked SIMPLIS definition has no source identity")?;
    let identity = definition
        .owner_scope_id
        .as_ref()
        .map_or(source_id, |_| definition.scope_id.as_str());
    Ok(format!(
        "{}_lib_{}",
        ngspice_safe_generated_base(&definition.name),
        &identity[..24]
    ))
}

fn replace_call_target(
    statement: &str,
    model: &str,
    emitted_name: &str,
    compile_time_vars: bool,
) -> Result<String> {
    let trimmed = active_statement_prefix(statement).trim_start();
    let (parsed_model, actuals, _) = call_target(statement)?.context("missing subcircuit call")?;
    if !parsed_model.eq_ignore_ascii_case(model) {
        bail!("inconsistent SIMPLIS model {model} in X instance");
    }
    let model_offset = parsed_model.as_ptr().addr() - trimmed.as_ptr().addr();
    let prefix = trimmed[..model_offset].trim_end();
    if compile_time_vars || actuals.is_empty() {
        Ok(format!("{prefix} {emitted_name}"))
    } else {
        Ok(format!("{prefix} {emitted_name} params: {actuals}"))
    }
}

fn is_preprocessor_control(line: &str) -> bool {
    directive_name(line).is_some_and(|name| {
        matches!(
            name.to_ascii_uppercase().as_str(),
            "VAR"
                | "GLOBALVAR"
                | "IF"
                | "ELSE"
                | "ENDIF"
                | "WHILE"
                | "ENDWHILE"
                | "FOR"
                | "ENDFOR"
                | "ERROR"
        )
    })
}

fn definition_uses_preprocessor(definition: &Definition) -> bool {
    String::from_utf8_lossy(&definition.bytes)
        .lines()
        .any(is_preprocessor_control)
}

fn preprocess_root_statements(root_lines: &[String]) -> Result<Vec<String>> {
    if !root_lines.iter().any(|line| is_preprocessor_control(line)) {
        return Ok(logical_statements(root_lines));
    }
    let processed = preprocess_control_flow(root_lines.join("\n").as_bytes(), &Environment::new())
        .context("preprocessing root SIMPLIS controls")?;
    let text = std::str::from_utf8(&processed.bytes)?;
    let lines = text.lines().map(str::to_owned).collect::<Vec<_>>();
    Ok(logical_statements(&lines))
}

fn definition_has_runtime_parameters(definition: &Definition) -> bool {
    let text = String::from_utf8_lossy(&definition.bytes);
    let mut header = String::new();
    for (index, line) in text.lines().enumerate() {
        if index != 0 && !line.trim_start().starts_with('+') {
            break;
        }
        if !header.is_empty() {
            header.push(' ');
        }
        let trimmed = line.trim_start();
        header.push_str(trimmed.strip_prefix('+').unwrap_or(trimmed).trim());
    }
    find_ascii_case_insensitive(&header, " params:").is_some()
}

fn make_linked_unit(
    definition: Definition,
    explicit_vars: bool,
    override_text: &str,
    inherited: &Environment,
    init: InitTree,
    parent_runtime: Option<&RuntimeScope>,
    external: &SubcircuitCatalog,
) -> Result<LinkedUnit> {
    let overrides = if explicit_vars {
        parse_overrides(override_text)?
    } else {
        Environment::new()
    };
    let protected_pwm = definition.name.eq_ignore_ascii_case("Complemetary_PWM")
        && String::from_utf8_lossy(&definition.bytes)
            .contains("MICROCHIP COMPLEMENTARY_PWM Encryption");
    if protected_pwm {
        if !init.is_empty() {
            bail!("SIMPLIS .INIT is not supported inside protected Microchip PWM replacement");
        }
        let emitted_name = linked_specialization_name(&definition, None, &overrides, &init);
        return Ok(LinkedUnit {
            definition,
            emitted_name,
            body: LinkedBody::Microchip(overrides),
            environment: inherited.clone(),
            runtime: None,
            init,
        });
    }

    let scoped = definition.owner_scope_id.is_some();
    let captures_environment = scoped
        && !definition_has_runtime_parameters(&definition)
        && (!inherited.is_empty() || !overrides.is_empty());
    let needs_substitution = definition.parameterized || explicit_vars || captures_environment;
    let needs_control_flow = definition_uses_preprocessor(&definition);
    let needs_preprocessing = needs_substitution || needs_control_flow;
    let (bytes, environment) = if needs_preprocessing {
        let specialized = if needs_substitution {
            specialize_subcircuit_with_base(&definition.bytes, inherited, &overrides)
        } else {
            specialize_subcircuit_control_flow_with_base(&definition.bytes, inherited, &overrides)
        }
        .with_context(|| format!("specializing SIMPLIS subcircuit {}", definition.name))?;
        (specialized.bytes, specialized.environment)
    } else {
        (definition.bytes.clone(), inherited.clone())
    };
    let (bytes, init) = merge_local_init(&bytes, init)?;
    let (bytes, runtime) = if let Some(parent) = parent_runtime {
        let actuals = if explicit_vars { "" } else { override_text };
        let (prepared, mut runtime) = RuntimeScope::prepare(&bytes, &definition.scope_id, actuals)
            .with_context(|| format!("binding SIMetrix parameters for {}", definition.name))?;
        let support = definition
            .source_id
            .as_deref()
            .and_then(|source| external.source_metadata(source))
            .map_or(&[][..], |metadata| metadata.support_lines.as_slice());
        runtime.set_source_scope(parent, support)?;
        (prepared, Some(runtime))
    } else {
        (bytes, None)
    };
    let specialized =
        needs_preprocessing || scoped || runtime.as_ref().is_some_and(RuntimeScope::has_bindings);
    let emitted_name = if specialized || !init.is_empty() {
        linked_specialization_name(&definition, Some(&bytes), &overrides, &init)
    } else {
        linked_plain_name(&definition)?
    };
    Ok(LinkedUnit {
        definition,
        emitted_name,
        body: LinkedBody::Netlist(bytes),
        environment,
        runtime,
        init,
    })
}

#[derive(Clone, Copy)]
struct LinkCaller<'a> {
    source: Option<&'a str>,
    scope: Option<&'a str>,
    environment: &'a Environment,
    runtime: Option<&'a RuntimeScope>,
}

fn link_call(
    statement: &str,
    local: &SubcircuitCatalog,
    external: &SubcircuitCatalog,
    caller: LinkCaller<'_>,
    init: InitTree,
) -> Result<Option<(String, LinkedUnit)>> {
    let Some((model, override_text, explicit_vars)) = call_target(statement)? else {
        return Ok(None);
    };
    let Some((definition, _is_local)) =
        resolve_definition(model, local, external, caller.source, caller.scope)?
    else {
        bail!("unresolved SIMPLIS subcircuit {model} in linkable preprocessing");
    };
    let inherits_lexically = caller.scope.is_some()
        && definition.owner_scope_id.as_deref() == caller.scope
        && definition.source_id.as_deref() == caller.source;
    let empty = Environment::new();
    let inherited = if inherits_lexically {
        caller.environment
    } else {
        &empty
    };
    let compile_time_vars = explicit_vars || definition.parameterized;
    let unit = make_linked_unit(
        definition,
        compile_time_vars,
        override_text,
        inherited,
        init,
        caller.runtime,
        external,
    )?;
    let rewritten = if !compile_time_vars
        && let (Some(caller), Some(callee)) = (caller.runtime, &unit.runtime)
    {
        let instance = instance_name(statement).context("missing runtime call instance")?;
        let (declarations, arguments) = caller.bind_actuals(callee, &instance, override_text)?;
        let prefix = replace_call_target(statement, model, &unit.emitted_name, true)?;
        if arguments.is_empty() {
            prefix
        } else {
            format!("{declarations}{prefix} params:{arguments}")
        }
    } else {
        replace_call_target(statement, model, &unit.emitted_name, compile_time_vars)?
    };
    Ok(Some((rewritten, unit)))
}

fn initialize_linked_builder(
    builder: &mut LinkedLibraryBuilder,
    source_id: &str,
    source_name: &str,
    external: &SubcircuitCatalog,
) -> Result<()> {
    if builder.source_name.is_empty() {
        source_name.clone_into(&mut builder.source_name);
        if external.source_metadata(source_id).is_some() {
            builder.support_lines = external.materialized_support(source_id)?;
        }
    }
    Ok(())
}

fn linked_source(unit: &LinkedUnit) -> Result<(&str, &str)> {
    let source_id = unit
        .definition
        .source_id
        .as_deref()
        .context("linked SIMPLIS definition has no source identity")?;
    let source_name = unit
        .definition
        .source_name
        .as_deref()
        .unwrap_or("<unknown>");
    Ok((source_id, source_name))
}

fn emit_linked_unit(
    unit: &LinkedUnit,
    local: &SubcircuitCatalog,
    external: &SubcircuitCatalog,
    builders: &mut BTreeMap<String, LinkedLibraryBuilder>,
    seen: &mut BTreeSet<(String, String)>,
    generated_subcircuits: &mut usize,
) -> Result<()> {
    let (source_id, source_name) = linked_source(unit)?;
    let source_id = source_id.to_owned();
    let source_name = source_name.to_owned();
    let seen_key = (source_id.clone(), unit.emitted_name.clone());
    if !seen.insert(seen_key) {
        return Ok(());
    }

    if let LinkedBody::Microchip(overrides) = &unit.body {
        let pending = Pending {
            definition: unit.definition.clone(),
            generated_name: unit.emitted_name.clone(),
            overrides: overrides.clone(),
            local_key: None,
        };
        let builder = builders.entry(source_id.clone()).or_default();
        initialize_linked_builder(builder, &source_id, &source_name, external)?;
        emit_microchip_complementary_pwm(&pending, &mut builder.output)?;
        *generated_subcircuits = generated_subcircuits.saturating_add(1);
        return Ok(());
    }

    let LinkedBody::Netlist(bytes) = &unit.body else {
        unreachable!("handled Microchip body above");
    };
    let (header, body_lines) = rename_specialized(bytes, &unit.emitted_name)?;
    let mut rewritten_body = Vec::new();
    let mut children = Vec::new();
    let mut dependencies = BTreeSet::new();
    let mut init_children = unit.init.children.clone();
    for statement in logical_statements(&body_lines) {
        let child_init = instance_name(&statement)
            .and_then(|name| init_children.remove(&name))
            .unwrap_or_default();
        if let Some((rewritten, child)) = link_call(
            &statement,
            local,
            external,
            LinkCaller {
                source: unit.definition.source_id.as_deref(),
                scope: Some(&unit.definition.scope_id),
                environment: &unit.environment,
                runtime: unit.runtime.as_ref(),
            },
            child_init.clone(),
        )? {
            let (child_source, _) = linked_source(&child)?;
            if child_source != source_id {
                dependencies.insert(child_source.to_owned());
            }
            rewritten_body.push(rewritten);
            children.push(child);
        } else {
            let statement = match &unit.runtime {
                Some(runtime) => {
                    let rewritten = runtime.rewrite_bare_passive(&statement, |name| {
                        Ok(external.source_for_model(name)?.is_some())
                    })?;
                    runtime.rewrite_statement(&rewritten)?
                }
                None => statement,
            };
            rewritten_body.push(annotate_unlinked_init(&statement, &child_init));
        }
    }
    for (name, stale) in init_children {
        let mut entries = Vec::new();
        stale.flatten(&name, &mut entries);
        for entry in entries {
            rewritten_body.push(format!(
                "* ngspice-rs-preprocess: ignored stale SIMPLIS .INIT snapshot entry {entry}"
            ));
        }
    }

    {
        let builder = builders.entry(source_id.clone()).or_default();
        initialize_linked_builder(builder, &source_id, &source_name, external)?;
        builder.dependencies.extend(dependencies);
        builder.output.push_str(&header);
        builder.output.push('\n');
        for statement in rewritten_body {
            builder.output.push_str(&statement);
            builder.output.push('\n');
        }
        builder.output.push_str(".ends ");
        builder.output.push_str(&unit.emitted_name);
        builder.output.push_str("\n\n");
    }
    *generated_subcircuits = generated_subcircuits.saturating_add(1);
    for child in children {
        emit_linked_unit(
            &child,
            local,
            external,
            builders,
            seen,
            generated_subcircuits,
        )?;
    }
    Ok(())
}

fn assign_local_source(definitions: &mut [Definition], source_id: &str, source_name: &str) {
    for definition in definitions {
        definition.source_id = Some(source_id.to_owned());
        definition.source_name = Some(source_name.to_owned());
    }
}

struct ModelNamespace<'a> {
    globals: &'a BTreeSet<String>,
    local: &'a BTreeSet<String>,
    external: &'a SubcircuitCatalog,
}

impl ModelNamespace<'_> {
    fn defined(&self, name: &str) -> bool {
        let name = name.to_ascii_uppercase();
        self.globals.contains(&name) || self.local.contains(&name)
    }

    fn known(&self, name: &str) -> Result<bool> {
        Ok(self.defined(name) || self.external.source_for_model(name)?.is_some())
    }

    fn select<'a>(
        &self,
        first: Option<&'a str>,
        second: Option<&'a str>,
        family: &str,
        statement: &str,
    ) -> Result<Option<&'a str>> {
        let first = match first {
            Some(name) if self.known(name)? => Some(name),
            _ => None,
        };
        let second = match second {
            Some(name) if self.known(name)? => Some(name),
            _ => None,
        };
        match (first, second) {
            (Some(first), Some(second)) => bail!(
                "ambiguous {family} model position in {statement}: both {first} and {second} name available models"
            ),
            (Some(name), None) | (None, Some(name)) => Ok(Some(name)),
            (None, None) => Ok(None),
        }
    }

    fn device_model<'a>(&self, statement: &'a str) -> Result<Option<&'a str>> {
        let fields = statement.split_whitespace().collect::<Vec<_>>();
        let Some(device) = fields.first().and_then(|name| name.as_bytes().first()) else {
            return Ok(None);
        };
        let field = |index| fields.get(index).copied();
        match device.to_ascii_uppercase() {
            b'D' => Ok(field(3)),
            // Passive devices may place the model before or after the value.
            b'R' | b'C' | b'L' => self.select(field(3), field(4), "passive", statement),
            b'J' | b'Z' | b'U' | b'W' => Ok(field(4)),
            b'S' | b'O' => Ok(field(5)),
            // Resolve optional substrate/bulk terminals against actual model
            // names, not a guess based on node spelling. Unknown models stay
            // unchanged for the simulator's explicit diagnostic.
            b'M' => self.select(field(4), field(5), "MOS", statement),
            b'Q' => self.select(field(4), field(5), "BJT", statement),
            _ => Ok(None),
        }
    }
}

fn direct_device_models(
    netlist: &str,
    globals: &BTreeSet<String>,
    external: &SubcircuitCatalog,
) -> Result<BTreeSet<String>> {
    // The linker emits flattened subcircuit definitions. Collect the local
    // declarations first so forward references do not pull unrelated global
    // catalog cards with the same name.
    let mut scopes = vec![BTreeSet::new()];
    let mut scope = 0;
    let mut statements = Vec::new();
    let mut control = false;
    for statement in netlist.lines() {
        if is_directive(statement, "CONTROL") {
            control = true;
        } else if is_directive(statement, "ENDC") {
            control = false;
        } else if control {
            // Control-language statements are not electrical devices.
        } else if is_directive(statement, "SUBCKT") {
            scopes.push(BTreeSet::new());
            scope = scopes.len() - 1;
        } else if is_directive(statement, "ENDS") {
            scope = 0;
        } else if is_directive(statement, "MODEL") {
            if let Some(name) = statement.split_whitespace().nth(1) {
                scopes[scope].insert(name.to_ascii_uppercase());
            }
        } else {
            statements.push((scope, statement));
        }
    }
    let mut found = BTreeSet::new();
    for (scope, statement) in statements {
        let namespace = ModelNamespace {
            globals,
            local: &scopes[scope],
            external,
        };
        if let Some(model) = namespace.device_model(statement)?
            && !namespace.defined(model)
        {
            found.insert(model.to_ascii_uppercase());
        }
    }
    Ok(found)
}

fn validate_linked_model_namespace(
    builders: &BTreeMap<String, LinkedLibraryBuilder>,
) -> Result<()> {
    let mut models = BTreeMap::<String, (String, String)>::new();
    for (source_id, builder) in builders {
        for statement in &builder.support_lines {
            let fields = statement.split_whitespace().collect::<Vec<_>>();
            if fields.len() < 3 || !fields[0].eq_ignore_ascii_case(".MODEL") {
                continue;
            }
            let name = fields[1].to_ascii_uppercase();
            let normalized = statement.replace('\r', "");
            if let Some((existing, existing_source)) = models.get(&name) {
                if existing != &normalized {
                    bail!(
                        "conflicting linked ngspice .MODEL {name} from SIMPLIS sources {existing_source} and {source_id}"
                    );
                }
            } else {
                models.insert(name, (normalized, source_id.clone()));
            }
        }
    }
    Ok(())
}

fn link_root_statements(
    root_lines: &[String],
    local: &SubcircuitCatalog,
    external: &SubcircuitCatalog,
    runtime: Option<&RuntimeScope>,
) -> Result<(String, Vec<LinkedUnit>, Option<String>)> {
    use std::fmt::Write as _;

    let root_statements = preprocess_root_statements(root_lines)?;
    let mut root_init = InitTree::default();
    for statement in &root_statements {
        if let Some(assignments) = parse_init_statement(statement)? {
            for (path, value) in assignments {
                root_init.insert(&path, value)?;
            }
        }
    }

    let mut root_output = String::new();
    let mut root_units = Vec::new();
    let mut end_statement = None;
    for statement in root_statements {
        if is_directive(&statement, "END") {
            end_statement = Some(statement);
            continue;
        }
        if is_directive(&statement, "INIT") {
            continue;
        }
        if external.matches_supplied_include(&statement)? {
            root_output.push_str("* ngspice-rs-preprocess: consumed supplied SIMPLIS include ");
            root_output.push_str(statement.trim());
            root_output.push('\n');
            continue;
        }
        let child_init = instance_name(&statement)
            .and_then(|name| root_init.children.remove(&name))
            .unwrap_or_default();
        if let Some((rewritten, unit)) = link_call(
            &statement,
            local,
            external,
            LinkCaller {
                source: None,
                scope: None,
                environment: &Environment::new(),
                runtime,
            },
            child_init.clone(),
        )? {
            root_output.push_str(&rewritten);
            root_output.push('\n');
            root_units.push(unit);
        } else {
            let statement = match runtime {
                Some(runtime) => runtime.rewrite_bare_passive(&statement, |name| {
                    Ok(external.source_for_model(name)?.is_some())
                })?,
                None => statement,
            };
            root_output.push_str(&annotate_unlinked_init(&statement, &child_init));
            root_output.push('\n');
        }
    }

    for (name, value) in &root_init.local {
        writeln!(root_output, ".INIT {name}={value}")?;
    }
    for (name, stale) in &root_init.children {
        let mut entries = Vec::new();
        stale.flatten(name, &mut entries);
        for entry in entries {
            writeln!(
                root_output,
                "* ngspice-rs-preprocess: ignored stale SIMPLIS .INIT snapshot entry {entry}"
            )?;
        }
    }
    Ok((root_output, root_units, end_statement))
}

fn include_referenced_device_model_libraries(
    root_output: &str,
    builders: &mut BTreeMap<String, LinkedLibraryBuilder>,
    external: &SubcircuitCatalog,
) -> Result<()> {
    // Device-model dependencies can appear directly in a root deck or
    // inside a specialized subcircuit supplied by another vendor library.
    // Keep models already supplied in the root or the source of a linked
    // subcircuit. In particular, two independent libraries with conflicting
    // global model cards must reach the namespace validator, not be reported
    // as an ambiguous unrelated catalog lookup.
    let defined_models = root_output
        .lines()
        .chain(
            builders
                .values()
                .flat_map(|builder| builder.support_lines.iter().map(String::as_str)),
        )
        .filter_map(|statement| {
            let mut fields = statement.split_whitespace();
            if !fields.next()?.eq_ignore_ascii_case(".MODEL") {
                return None;
            }
            fields.next().map(str::to_ascii_uppercase)
        })
        .collect::<BTreeSet<_>>();
    let mut models = BTreeSet::new();
    for netlist in
        std::iter::once(root_output).chain(builders.values().map(|builder| builder.output.as_str()))
    {
        models.extend(direct_device_models(netlist, &defined_models, external)?);
    }
    for model in models {
        if defined_models.contains(&model) {
            continue;
        }
        let Some(source_id) = external.source_for_model(&model)? else {
            continue;
        };
        let source_name = external
            .source_metadata(&source_id)
            .with_context(|| format!("missing SIMPLIS source metadata {source_id}"))?
            .source_name
            .clone();
        let builder = builders.entry(source_id.clone()).or_default();
        initialize_linked_builder(builder, &source_id, &source_name, external)?;
    }
    Ok(())
}

/// Admit the root source before generating independently linkable libraries.
///
/// # Errors
///
/// Source admission, external command, or preprocessing errors.
pub fn preprocess_netlist_linked_file(
    path: &Path,
    external: &SubcircuitCatalog,
    admission: &mut SourceSession,
) -> Result<LinkedPreprocessedNetlist> {
    let source = admission.load(path)?;
    preprocess_netlist_linked(&source.bytes, external)
}

/// Preprocess a SIMPLIS root netlist into independently linkable source-library artifacts.
///
/// Every referenced subcircuit is renamed into a content-addressed namespace. Parameterized
/// definitions use a specialization hash; plain definitions use their parsed library identity.
/// Cross-library dependency edges are retained as metadata, while the root emits every generated
/// companion exactly once. Keeping companion files free of recursive `.include` edges avoids cycles
/// when two source libraries specialize helpers from each other.
///
/// # Errors
///
/// Returns an error for unresolved/ambiguous subcircuits, unsupported preprocessing semantics,
/// malformed hierarchy, or invalid protected-model parameters.
pub fn preprocess_netlist_linked(
    input: &[u8],
    external: &SubcircuitCatalog,
) -> Result<LinkedPreprocessedNetlist> {
    use std::fmt::Write as _;

    let text = decode_simplis_text(input)?;
    let mut extracted = extract_definitions(&text)?;
    let (root_support, root_aliases) = parse_library_root(&extracted.root_lines)?;
    let root_source_id = library_digest(&root_support, &root_aliases, &extracted.definitions);
    assign_local_source(&mut extracted.definitions, &root_source_id, "<root>");

    let mut local = SubcircuitCatalog::new();
    for definition in &extracted.definitions {
        insert_local_definition(&mut local, definition.clone())?;
    }

    let runtime = (external.simulator == Some(LibrarySimulator::Simetrix))
        .then(|| RuntimeScope::root_with_symbols(&root_source_id, &root_support))
        .transpose()?;
    let (mut root_output, root_units, end_statement) =
        link_root_statements(&extracted.root_lines, &local, external, runtime.as_ref())?;

    let root_specializations = root_units
        .iter()
        .filter(|unit| unit.emitted_name.contains("_sp_"))
        .count();
    let mut builders = BTreeMap::new();
    let mut seen = BTreeSet::new();
    let mut generated_subcircuits = 0usize;
    for unit in root_units {
        emit_linked_unit(
            &unit,
            &local,
            external,
            &mut builders,
            &mut seen,
            &mut generated_subcircuits,
        )?;
    }
    include_referenced_device_model_libraries(&root_output, &mut builders, external)?;

    validate_linked_model_namespace(&builders)?;

    // Include all generated source libraries from the root as well. This makes the
    // root self-contained even if a simulator does not recursively discover include
    // dependencies until later in parsing.
    for source_id in builders.keys() {
        writeln!(
            root_output,
            ".include \"{}\"",
            linked_library_filename(source_id)
        )?;
    }
    if let Some(end) = end_statement {
        root_output.push_str(&end);
        root_output.push('\n');
    }

    let mut libraries = Vec::new();
    for (source_id, builder) in builders {
        let mut output = String::new();
        for statement in &builder.support_lines {
            output.push_str(statement);
            output.push('\n');
        }
        output.push_str(&builder.output);
        libraries.push(LinkedGeneratedLibrary {
            filename: linked_library_filename(&source_id),
            source_id,
            source_name: builder.source_name,
            dependencies: builder.dependencies.into_iter().collect(),
            bytes: output.into_bytes(),
        });
    }

    let local_parameterized_definitions_removed = extracted
        .definitions
        .iter()
        .filter(|definition| definition.parameterized)
        .count();
    Ok(LinkedPreprocessedNetlist {
        root_bytes: root_output.into_bytes(),
        libraries,
        stats: NetlistPreprocessStats {
            root_specializations,
            generated_subcircuits,
            local_parameterized_definitions_removed,
        },
    })
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[test]
    fn file_admission_transforms_root_and_library_before_simulator_projection() -> Result<()> {
        use model_preprocess_command::{
            AdmissionConfig, Cancellation, CommandConfig, SourceSession,
        };
        let dir = tempfile::tempdir()?;
        let source = dir.path().join("root.net");
        let library = dir.path().join("library.lb");
        std::fs::write(&source, b"X1 1 0 PLACEHOLDER\n.end\n")?;
        std::fs::write(
            &library,
            b".subckt PLACEHOLDER 1 2\nR1 1 2 VALUE\n.ends PLACEHOLDER\n",
        )?;
        let config = AdmissionConfig {
            command: Some(CommandConfig {
                executable: "/bin/sh".into(),
                arguments: vec![
                    "-c".into(),
                    "printf x >> calls; sed 's/PLACEHOLDER/HELPER/g; s/VALUE/47/g'".into(),
                    "fixture".into(),
                ],
                working_directory: None,
            }),
            ..AdmissionConfig::default()
        };
        let mut admission = SourceSession::new(config, Cancellation::default());
        let mut catalog = SubcircuitCatalog::new();
        assert_eq!(
            catalog.add_library_file(&library, LibrarySimulator::Simplis, &mut admission)?,
            1
        );
        let result = preprocess_netlist_linked_file(&source, &catalog, &mut admission)?;
        assert_eq!(result.libraries.len(), 1);
        assert!(std::str::from_utf8(&result.libraries[0].bytes)?.contains("R1 1 2 47"));
        assert!(std::str::from_utf8(&result.root_bytes)?.contains("sr_HELPER_lib_"));
        let parsed = parse_library_file(&library, LibrarySimulator::Simplis, &mut admission)?;
        assert_eq!(parsed.definitions(), 1);
        assert_eq!(std::fs::read(dir.path().join("calls"))?, b"xx");
        Ok(())
    }

    use super::*;

    #[test]
    fn catalog_deduplicates_format_only_repeated_definitions() -> Result<()> {
        let library = b".subckt T 1 2 vars: A=1
  R1 1 2 {A}
.ends T
.subckt T 1 2 vars: A=1
\tR1 1 2 {A}
.ends T
";
        let parsed = parse_library("repeat.lb", library)?;
        assert_eq!(parsed.definitions(), 2);
        let mut catalog = SubcircuitCatalog::new();
        assert_eq!(catalog.add_parsed_library(&parsed)?, 1);
        assert_eq!(catalog.definitions.get("T").map(Vec::len), Some(1));
        Ok(())
    }

    #[test]
    fn linked_preprocessing_deduplicates_identical_local_definitions() -> Result<()> {
        let source = br"X1 1 0 HELPER
.subckt HELPER 1 2
R1 1 2 3
.ends HELPER
.subckt HELPER 1 2
R1 1 2 3
.ends HELPER
.end
";
        let result = preprocess_netlist_linked(source, &SubcircuitCatalog::new())?;
        assert_eq!(result.stats.generated_subcircuits, 1);
        assert_eq!(result.libraries.len(), 1);
        Ok(())
    }

    #[test]
    fn linked_preprocessing_rejects_conflicting_local_duplicates() -> Result<()> {
        let source = br"X1 1 0 HELPER
.subckt HELPER 1 2
R1 1 2 3
.ends HELPER
.subckt HELPER 1 2
R1 1 2 4
.ends HELPER
.end
";
        let error = preprocess_netlist_linked(source, &SubcircuitCatalog::new())
            .err()
            .context("conflicting local duplicate unexpectedly succeeded")?;
        assert!(
            error
                .to_string()
                .contains("conflicting duplicate local SIMPLIS subcircuit HELPER")
        );
        Ok(())
    }

    #[test]
    fn nested_loop_specialization_passes_iteration_local_actuals() -> Result<()> {
        let source = br"X1 1 0 OUTER vars: N=2
X2 2 0 OUTER vars: N=3
.subckt OUTER 1 2 vars: N=1
.VAR I=0
.WHILE {I<N} {N}
XH{I} 1 2 HELPER vars: V={I+1}
.VAR I={I+1}
.ENDWHILE
.subckt HELPER 1 2 vars: V=1
.VAR J=0
.WHILE {J<V} {V}
R{J} 1 2 {V+J}
.VAR J={J+1}
.ENDWHILE
.ends HELPER
.ends OUTER
.end
";
        let catalog = SubcircuitCatalog::new();
        let linked = preprocess_netlist_linked(source, &catalog)?;
        let mut text = std::str::from_utf8(&linked.root_bytes)?.to_owned();
        for library in &linked.libraries {
            text.push_str(std::str::from_utf8(&library.bytes)?);
        }
        let legacy = preprocess_netlist(source, &catalog)?;
        for output in [&text, std::str::from_utf8(&legacy.bytes)?] {
            let definitions = extract_definitions(output)?;
            for (root, count) in [("X1", 2), ("X2", 3)] {
                let statement = definitions
                    .root_lines
                    .iter()
                    .find(|line| line.starts_with(root))
                    .context("missing root call")?;
                let (target, _, _) = call_target(statement)?.context("root target")?;
                let parent = definitions
                    .definitions
                    .iter()
                    .find(|definition| definition.name == target)
                    .context("parent definition")?;
                let parent_text = std::str::from_utf8(&parent.bytes)?;
                let child_calls = parent_text
                    .lines()
                    .filter(|line| line.starts_with("XH"))
                    .collect::<Vec<_>>();
                assert_eq!(child_calls.len(), count);
                for (iteration, statement) in child_calls.into_iter().enumerate() {
                    let (target, _, _) = call_target(statement)?.context("child target")?;
                    let child = definitions
                        .definitions
                        .iter()
                        .find(|definition| definition.name == target)
                        .context("child definition")?;
                    let body = std::str::from_utf8(&child.bytes)?;
                    let resistors = body
                        .lines()
                        .filter(|line| line.starts_with('R'))
                        .collect::<Vec<_>>();
                    assert_eq!(resistors.len(), iteration + 1);
                    for (index, resistor) in resistors.into_iter().enumerate() {
                        assert_eq!(resistor, format!("R{index} 1 2 {}", iteration + index + 1));
                    }
                }
            }
            assert!(!output.contains(".WHILE"));
            assert!(!output.contains(".VAR "));
        }
        Ok(())
    }

    #[test]
    fn plain_wrappers_reach_controlled_helpers() -> Result<()> {
        let source = br"X1 1 0 TOP
.subckt TOP 1 2
XM 1 2 MIDDLE
.ends TOP
.subckt MIDDLE 1 2
XH 1 2 HELPER
.ends MIDDLE
.subckt HELPER 1 2
.VAR I=0
.WHILE {I<2} 2
R{I} 1 2 {I+1}
.VAR I={I+1}
.ENDWHILE
.ends HELPER
.end
";
        let catalog = SubcircuitCatalog::new();
        let flat = preprocess_netlist(source, &catalog)?;
        let flat_text = std::str::from_utf8(&flat.bytes)?;
        assert_eq!(flat.stats.generated_subcircuits, 3);
        assert!(flat_text.contains("R0 1 2 1"));
        assert!(flat_text.contains("R1 1 2 2"));
        assert!(!flat_text.contains(".WHILE"));
        let linked = preprocess_netlist_linked(source, &catalog)?;
        assert_eq!(linked.stats.generated_subcircuits, 3);
        assert!(linked.libraries.iter().any(|library| {
            std::str::from_utf8(&library.bytes)
                .is_ok_and(|text| text.contains("R0 1 2 1") && text.contains("R1 1 2 2"))
        }));
        Ok(())
    }

    #[test]
    fn recursive_controlled_wrapper_reports_cycle() -> Result<()> {
        let source = br"X1 1 0 TOP
.subckt TOP 1 2
XM 1 2 HELPER
.ends TOP
.subckt HELPER 1 2
.VAR I=0
.WHILE {I<1} 1
XT 1 2 TOP
.VAR I={I+1}
.ENDWHILE
.ends HELPER
.end
";
        let error = preprocess_netlist(source, &SubcircuitCatalog::new())
            .err()
            .context("expected recursive specialization error")?;
        assert!(format!("{error:#}").contains("recursive SIMPLIS specialization"));
        Ok(())
    }

    #[test]
    fn root_and_plain_definition_loops_expand_without_losing_runtime_expressions() -> Result<()> {
        let source = br".VAR I=0
.WHILE {I<2} 2
X{I} {I+1} 0 T
.VAR I={I+1}
.ENDWHILE
.subckt T 1 2
.VAR J=0
.WHILE {J<2} 2
R{J} 1 2 {J+1}
.VAR J={J+1}
.ENDWHILE
B1 1 2 V={V(1)}
.ends T
.end
";
        let catalog = SubcircuitCatalog::new();
        let linked = preprocess_netlist_linked(source, &catalog)?;
        let root = std::str::from_utf8(&linked.root_bytes)?;
        assert!(root.contains("X0 1 0 sr_T_sp_"));
        assert!(root.contains("X1 2 0 sr_T_sp_"));
        assert_eq!(linked.stats.generated_subcircuits, 1);
        let body = std::str::from_utf8(&linked.libraries[0].bytes)?;
        assert!(body.contains("R0 1 2 1"));
        assert!(body.contains("R1 1 2 2"));
        assert!(body.contains("B1 1 2 V={V(1)}"));
        assert!(!body.contains(".WHILE"));
        let legacy = preprocess_netlist(source, &catalog)?;
        let text = std::str::from_utf8(&legacy.bytes)?;
        assert!(text.contains("X0 1 0 T$1"));
        assert!(text.contains("X1 2 0 T$2"));
        assert!(text.contains("R0 1 2 1"));
        assert!(text.contains("R1 1 2 2"));
        assert!(!text.contains(".WHILE"));
        Ok(())
    }

    #[test]
    fn linked_preprocessing_flattens_nested_local_helpers() -> Result<()> {
        let source = br"XTOP 1 0 OUTER
.subckt OUTER 1 2
XH 1 2 HELPER
.subckt HELPER 1 2
R1 1 2 3
.ends HELPER
.ends OUTER
.end
";
        let result = preprocess_netlist_linked(source, &SubcircuitCatalog::new())?;
        assert_eq!(result.stats.generated_subcircuits, 2);
        assert_eq!(result.libraries.len(), 1);
        let root = std::str::from_utf8(&result.root_bytes)?;
        let library = std::str::from_utf8(&result.libraries[0].bytes)?;
        assert!(root.contains("sr_OUTER_lib_"));
        assert!(library.contains(".subckt sr_OUTER_lib_"));
        assert!(library.contains(".subckt sr_HELPER_sp_"));
        assert!(library.contains("XH 1 2 sr_HELPER_sp_"));
        assert_eq!(library.matches(".subckt ").count(), 2);
        Ok(())
    }

    #[test]
    fn linked_nested_helpers_are_lexically_scoped_and_capture_parent_parameters() -> Result<()> {
        let source = br"X1 1 0 PARENT_A vars: A=3
X2 2 0 PARENT_B vars: A=7
.subckt PARENT_A 1 2 vars: A=1
XH 1 2 HELPER
.subckt HELPER 1 2
RA 1 2 {A}
.ends HELPER
.ends PARENT_A
.subckt PARENT_B 1 2 vars: A=1
XH 1 2 HELPER
.subckt HELPER 1 2
RB 1 2 {A*10}
.ends HELPER
.ends PARENT_B
.end
";
        let linked = preprocess_netlist_linked(source, &SubcircuitCatalog::new())?;
        let combined = linked
            .libraries
            .iter()
            .map(|library| std::str::from_utf8(&library.bytes))
            .collect::<Result<Vec<_>, _>>()?
            .join("\n");
        assert!(combined.contains("RA 1 2 3"));
        assert!(combined.contains("RB 1 2 70"));
        assert_eq!(combined.matches(".subckt sr_HELPER_sp_").count(), 2);
        Ok(())
    }

    #[test]
    fn linked_runtime_params_shadow_parent_compile_time_environment() -> Result<()> {
        let source = br"XTOP 1 0 PARENT vars: THRESHOLD=9
.subckt PARENT 1 2 vars: THRESHOLD=1
XH 1 2 HELPER params: THRESHOLD=4
.subckt HELPER 1 2 params: THRESHOLD=2.5
B1 1 2 V={THRESHOLD}
.ends HELPER
.ends PARENT
.end
";
        let linked = preprocess_netlist_linked(source, &SubcircuitCatalog::new())?;
        let combined = linked
            .libraries
            .iter()
            .map(|library| std::str::from_utf8(&library.bytes))
            .collect::<Result<Vec<_>, _>>()?
            .join("\n");
        assert!(combined.contains(".subckt sr_HELPER_sp_"));
        assert!(combined.contains("params: THRESHOLD=2.5"));
        assert!(combined.contains("B1 1 2 V={THRESHOLD}"));
        assert!(combined.contains("params: THRESHOLD=4"));
        assert!(!combined.contains("B1 1 2 V=9"));
        Ok(())
    }

    #[test]
    fn linked_nested_helper_is_not_visible_at_root_scope() -> Result<()> {
        let source = br"XBAD 1 0 HELPER
.subckt PARENT 1 2
XH 1 2 HELPER
.subckt HELPER 1 2
R1 1 2 1
.ends HELPER
.ends PARENT
.end
";
        let error = preprocess_netlist_linked(source, &SubcircuitCatalog::new())
            .err()
            .context("nested helper unexpectedly leaked into root scope")?;
        assert!(
            error
                .to_string()
                .contains("unresolved SIMPLIS subcircuit HELPER")
        );
        Ok(())
    }

    #[test]
    fn linked_preprocessing_is_load_order_independent_and_namespaces_helpers() -> Result<()> {
        let library_a = br".subckt OUTERA 1 2 vars: A=1
XH 1 2 HELPER vars: V={A}
.ends OUTERA
.subckt HELPER 1 2 vars: V=1
RA 1 2 {V}
.ends HELPER
";
        let library_b = br".subckt OUTERB 1 2 vars: A=1
XH 1 2 HELPER vars: V={A}
.ends OUTERB
.subckt HELPER 1 2 vars: V=1
RB 1 2 {V}
.ends HELPER
";
        let source = b"XA 1 0 OUTERA vars: A=3\nXB 2 0 OUTERB vars: A=4\n.end\n";

        let mut ab = SubcircuitCatalog::new();
        ab.add_library_named("a.lb", library_a)?;
        ab.add_library_named("b.lb", library_b)?;
        let mut ba = SubcircuitCatalog::new();
        ba.add_library_named("renamed-b.lb", library_b)?;
        ba.add_library_named("renamed-a.lb", library_a)?;

        let first = preprocess_netlist_linked(source, &ab)?;
        let second = preprocess_netlist_linked(source, &ba)?;
        assert_eq!(first.root_bytes, second.root_bytes);
        assert_eq!(first.libraries.len(), second.libraries.len());
        for (left, right) in first.libraries.iter().zip(&second.libraries) {
            assert_eq!(left.source_id, right.source_id);
            assert_eq!(left.filename, right.filename);
            assert_eq!(left.dependencies, right.dependencies);
            assert_eq!(left.bytes, right.bytes);
        }
        assert_eq!(first.libraries.len(), 2);

        let joined = first
            .libraries
            .iter()
            .map(|library| String::from_utf8_lossy(&library.bytes))
            .collect::<String>();
        assert!(joined.contains("RA 1 2 3"));
        assert!(joined.contains("RB 1 2 4"));
        let helper_names = joined
            .lines()
            .filter_map(|line| {
                let mut fields = line.split_whitespace();
                (fields.next()?.eq_ignore_ascii_case(".subckt")).then_some(fields.next()?)
            })
            .filter(|name| name.starts_with("sr_HELPER_sp_"))
            .collect::<Vec<_>>();
        assert_eq!(helper_names.len(), 2);
        assert_ne!(helper_names[0], helper_names[1]);
        Ok(())
    }

    #[test]
    fn linked_preprocessing_emits_cross_library_dependency_edges() -> Result<()> {
        let parent = br".subckt PARENT 1 2 vars: A=1
XC 1 2 CHILD vars: V={A}
.ends PARENT
";
        let child = br".subckt CHILD 1 2 vars: V=1
R1 1 2 {V}
.ends CHILD
";
        let parsed_parent = parse_library("parent.lb", parent)?;
        let parsed_child = parse_library("child.lb", child)?;
        let mut catalog = SubcircuitCatalog::new();
        catalog.add_parsed_library(&parsed_parent)?;
        catalog.add_parsed_library(&parsed_child)?;

        let linked = preprocess_netlist_linked(b"X1 1 0 PARENT vars: A=9\n.end\n", &catalog)?;
        assert_eq!(linked.libraries.len(), 2);
        let parent_output = linked
            .libraries
            .iter()
            .find(|library| library.source_id == parsed_parent.source_id())
            .context("missing parent generated library")?;
        assert_eq!(
            parent_output.dependencies,
            vec![parsed_child.source_id().to_owned()]
        );
        let parent_text = std::str::from_utf8(&parent_output.bytes)?;
        assert!(!parent_text.contains(".include \""));
        let root_text = std::str::from_utf8(&linked.root_bytes)?;
        assert!(root_text.contains(&linked_library_filename(parsed_parent.source_id())));
        assert!(root_text.contains(&linked_library_filename(parsed_child.source_id())));
        let child_output = linked
            .libraries
            .iter()
            .find(|library| library.source_id == parsed_child.source_id())
            .context("missing child generated library")?;
        assert!(std::str::from_utf8(&child_output.bytes)?.contains("R1 1 2 9"));
        Ok(())
    }

    #[test]
    fn linked_init_accepts_multiple_reactive_assignments() -> Result<()> {
        let source = br".INIT V(X1.C1)=3 I(X2.L1)=-2m
X1 a b CELL
X2 c d CELL
.subckt CELL p n
C1 p n 1u IC=0
L1 p n 1u IC=0
.ends CELL
.end
";
        let linked = preprocess_netlist_linked(source, &SubcircuitCatalog::new())?;
        let text = std::str::from_utf8(&linked.libraries[0].bytes)?;
        assert!(text.contains(".INIT C1=3"));
        assert!(text.contains(".INIT L1=-2m"));
        assert!(
            preprocess_netlist_linked(b".INIT V(L1)=1\n.end\n", &SubcircuitCatalog::new()).is_err()
        );
        Ok(())
    }

    #[test]
    fn linked_init_state_participates_in_specialization_identity() -> Result<()> {
        let source = br".INIT X1.S1=CLOSE
X1 1 0 ctrl CHILD
X2 2 0 ctrl CHILD
.subckt CHILD p n ctrl
S1 p n ctrl 0 MS IC=OPEN
.MODEL MS VCSW RON=1 ROFF=1Meg TH=2 HYSTWD=0.2 LOGIC=POS
.ends CHILD
.end
";
        let linked = preprocess_netlist_linked(source, &SubcircuitCatalog::new())?;
        assert_eq!(linked.libraries.len(), 1);
        let root = std::str::from_utf8(&linked.root_bytes)?;
        let calls = root
            .lines()
            .filter(|line| line.starts_with("X1 ") || line.starts_with("X2 "))
            .collect::<Vec<_>>();
        assert_eq!(calls.len(), 2);
        let x1_target = calls[0]
            .split_whitespace()
            .next_back()
            .context("missing X1 target")?;
        let x2_target = calls[1]
            .split_whitespace()
            .next_back()
            .context("missing X2 target")?;
        assert_ne!(x1_target, x2_target);
        assert!(x1_target.contains("_sp_"));
        assert!(x2_target.contains("_lib_"));

        let generated = std::str::from_utf8(&linked.libraries[0].bytes)?;
        assert_eq!(generated.matches(".INIT S1=CLOSE").count(), 1);
        let initialized_header = format!(".subckt {x1_target} ");
        let initialized = generated
            .split(&initialized_header)
            .nth(1)
            .context("missing initialized specialization")?;
        assert!(initialized.starts_with("p n ctrl\n.INIT S1=CLOSE\n"));
        Ok(())
    }

    #[test]
    fn parsed_library_selects_explicit_simulator_views() -> Result<()> {
        let library = br".SIMULATOR SIMETRIX
.subckt T 1 2 vars: A=1
Rsimetrix 1 2 {A}
.ends T
.SIMULATOR DEFAULT
.subckt SHARED 1 2
Rshared 1 2 11
.ends SHARED
.SIMULATOR SIMETRIX|SIMPLIS
.subckt BOTH 1 2
Rboth 1 2 12
.ends BOTH
.SIMULATOR SIMPLIS
.subckt T 1 2 vars: A=1
Rsimplis 1 2 {A}
.ends T
.SIMULATOR DEFAULT
";
        let simplis = parse_library("mixed.lb", library)?;
        let simetrix =
            parse_library_for_simulator("mixed.lb", library, LibrarySimulator::Simetrix)?;
        assert_eq!(simplis.simulator(), LibrarySimulator::Simplis);
        assert_eq!(simetrix.simulator(), LibrarySimulator::Simetrix);
        assert_eq!(simplis.definitions(), 3);
        assert_eq!(simetrix.definitions(), 3);
        assert_ne!(simplis.source_id(), simetrix.source_id());

        let round_trip = ParsedLibrary::from_json(&simetrix.to_json()?)?;
        assert_eq!(round_trip.simulator(), LibrarySimulator::Simetrix);
        assert_eq!(round_trip.source_id(), simetrix.source_id());

        for (parsed, expected, rejected) in [
            (&simplis, "Rsimplis 1 2 7", "Rsimetrix"),
            (&simetrix, "Rsimetrix 1 2 7", "Rsimplis"),
        ] {
            let mut catalog = SubcircuitCatalog::new();
            catalog.add_parsed_library(parsed)?;
            let linked = preprocess_netlist_linked(
                b"X1 1 0 T vars: A=7\nX2 2 0 SHARED\nX3 3 0 BOTH\n.end\n",
                &catalog,
            )?;
            let generated = linked
                .libraries
                .iter()
                .map(|library| std::str::from_utf8(&library.bytes))
                .collect::<Result<Vec<_>, _>>()?
                .join("\n");
            assert!(generated.contains(expected));
            assert!(!generated.contains(rejected));
            assert!(generated.contains("Rshared 1 2 11"));
            assert!(generated.contains("Rboth 1 2 12"));
        }
        Ok(())
    }

    #[test]
    fn catalog_rejects_mixed_simulator_projections() -> Result<()> {
        let library = br".SIMULATOR SIMPLIS
.subckt S 1 2
R1 1 2 1
.ends S
.SIMULATOR SIMETRIX
.subckt X 1 2
R1 1 2 2
.ends X
";
        let simplis = parse_library("mixed.lb", library)?;
        let simetrix =
            parse_library_for_simulator("mixed.lb", library, LibrarySimulator::Simetrix)?;
        let mut catalog = SubcircuitCatalog::new();
        catalog.add_parsed_library(&simplis)?;
        let error = match catalog.add_parsed_library(&simetrix) {
            Ok(_) => String::new(),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("cannot mix simplis and simetrix"));
        Ok(())
    }

    #[test]
    fn linked_libraries_reject_conflicting_global_model_names() -> Result<()> {
        let a = b".model SHARED D IS=1e-12\n.subckt A 1 2\nD1 1 2 SHARED\n.ends A\n";
        let b = b".model SHARED D IS=2e-12\n.subckt B 1 2\nD1 1 2 SHARED\n.ends B\n";
        let mut catalog = SubcircuitCatalog::new();
        catalog.add_library_named("a.lb", a)?;
        catalog.add_library_named("b.lb", b)?;
        let error = match preprocess_netlist_linked(b"X1 1 0 A\nX2 2 0 B\n.end\n", &catalog) {
            Ok(_) => String::new(),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("conflicting linked ngspice .MODEL SHARED"));
        Ok(())
    }

    #[test]
    fn linked_library_materializes_device_model_aliases() -> Result<()> {
        let library = br".model BASE NPN IS=1e-15 BF=100
.alias ALT BASE NPN
.subckt T 1 2 3
Q1 1 2 3 ALT
.ends T
";
        let parsed = parse_library("bipolar.lb", library)?;
        let mut catalog = SubcircuitCatalog::new();
        catalog.add_parsed_library(&parsed)?;
        let linked = preprocess_netlist_linked(b"X1 1 2 0 T\n.end\n", &catalog)?;
        assert_eq!(linked.libraries.len(), 1);
        let generated = std::str::from_utf8(&linked.libraries[0].bytes)?;
        assert!(generated.contains(".model BASE NPN IS=1e-15 BF=100"));
        assert!(generated.contains(".model ALT NPN IS=1e-15 BF=100"));
        assert!(generated.contains("Q1 1 2 3 ALT"));
        Ok(())
    }

    #[test]
    fn links_model_cards_for_all_spice_device_families() -> Result<()> {
        let mut catalog = SubcircuitCatalog::new();
        let cases = [
            ("R1 a b 1k RECT", ".model RECT R (RSH=2)"),
            ("C1 a b CEXT", ".model CEXT C (CJ=1e-12)"),
            ("L1 a b LEXT", ".model LEXT L (CPL=0)"),
            ("D1 a b DEXT", ".model DEXT D (IS=1e-14)"),
            (
                "Q1 a b c QALIAS 2 OFF\nQ2 a b c 0 QALIAS",
                ".model QB PNP (IS=1e-15 BF=100)\n.alias QALIAS QB PNP",
            ),
            (
                "M1 a b c d MEXT",
                ".model MEXT NMOS (LEVEL=1 KP=1e-3 VTO=1)",
            ),
            ("M2 a b c VEXT", ".model VEXT VDMOS (VTO=2)"),
            ("J1 a b c JEXT", ".model JEXT NJF (BETA=1e-4 VTO=-1)"),
            ("Z1 a b c ZEXT", ".model ZEXT NMF (BETA=1e-4 VTO=-1)"),
            (
                "S1 a b c d SEXT",
                ".model SEXT SW (RON=1 ROFF=1e6 VT=0.5 VH=0)",
            ),
            (
                "W1 a b VSENSE WEXT",
                ".model WEXT CSW (RON=1 ROFF=1e6 IT=0.5 IH=0)",
            ),
            ("U1 a b c UEXT", ".model UEXT URC (RPERL=1 CPERL=1e-12)"),
            ("O1 a b c d OEXT", ".model OEXT LTRA (R=1 L=1u C=1p LEN=1)"),
        ];
        for (index, (_, card)) in cases.iter().enumerate() {
            catalog.add_library_named(format!("model-{index}.lb"), card.as_bytes())?;
        }
        for (device, card) in cases {
            let linked =
                preprocess_netlist_linked(format!("{device}\n.end\n").as_bytes(), &catalog)?;
            assert_eq!(linked.libraries.len(), 1, "{device}");
            let library = linked
                .libraries
                .first()
                .context("missing model dependency")?;
            let generated = std::str::from_utf8(&library.bytes)?;
            assert!(
                generated.contains(card.lines().next().context("missing model card")?),
                "{device}"
            );
            if device.contains("QALIAS") {
                assert!(generated.contains(".model QALIAS PNP"));
            }
            assert!(std::str::from_utf8(&linked.root_bytes)?.contains(&library.filename));
        }
        Ok(())
    }

    #[test]
    fn local_model_shadows_unrelated_catalog_card() -> Result<()> {
        let mut catalog = SubcircuitCatalog::new();
        catalog.add_library_named("unrelated.lb", b".model LOCAL NPN IS=9e-12\n")?;
        let linked = preprocess_netlist_linked(
            b"X1 a b T\n.subckt T c e\nQ1 c c e LOCAL\n.model LOCAL NPN IS=1e-15\n.ends\n.end\n",
            &catalog,
        )?;
        assert_eq!(linked.libraries.len(), 1);
        assert!(
            std::str::from_utf8(&linked.libraries[0].bytes)?.contains(".model LOCAL NPN IS=1e-15")
        );
        Ok(())
    }

    #[test]
    fn bjt_model_selection_uses_the_actual_model_namespace() -> Result<()> {
        let mut catalog = SubcircuitCatalog::new();
        catalog.add_library_named(
            "transistors.lb",
            b".model THREE PNP IS=1e-15\n.model FOUR NPN IS=1e-15\n",
        )?;
        // Neither the substrate node's spelling nor an unrelated
        // trailing node determines whether the BJT has three or four
        // nodes. Both positions must be resolved against actual models.
        let linked = preprocess_netlist_linked(
            b"Q1 c b e THREE 2 OFF\nQ2 c b e substrate FOUR\n.end\n",
            &catalog,
        )?;
        assert_eq!(linked.libraries.len(), 1);
        let generated = std::str::from_utf8(&linked.libraries[0].bytes)?;
        assert!(generated.contains(".model THREE PNP"));
        assert!(generated.contains(".model FOUR NPN"));

        // Both syntactic positions naming known model cards cannot be
        // distinguished without guessing the author's meaning.
        let result = preprocess_netlist_linked(b"Q1 c b e THREE FOUR\n.end\n", &catalog);
        assert!(result.is_err_and(|err| err.to_string().contains("ambiguous BJT model position")));
        Ok(())
    }

    #[test]
    fn nested_subcircuit_model_dependency_imports_external_model_library() -> Result<()> {
        let mut catalog = SubcircuitCatalog::new();
        catalog.add_library_named(
            "subcircuits.lb",
            b".subckt DRIVER p n\nQ1 p n 0 0 VENDOR_PNP\n.ends DRIVER\n",
        )?;
        catalog.add_library_named("devices.lb", b".model VENDOR_PNP PNP IS=1e-15 BF=100\n")?;
        let linked = preprocess_netlist_linked(b"X1 out input DRIVER\n.end\n", &catalog)?;
        assert_eq!(linked.libraries.len(), 2);
        let root = std::str::from_utf8(&linked.root_bytes)?;
        for library in &linked.libraries {
            assert!(root.contains(&library.filename));
        }
        assert!(linked.libraries.iter().any(|library| {
            std::str::from_utf8(&library.bytes)
                .is_ok_and(|text| text.contains(".model VENDOR_PNP PNP"))
        }));
        Ok(())
    }

    #[test]
    fn parsed_library_preserves_root_support_and_subcircuit_aliases() -> Result<()> {
        let library = br".systemlib
.model DM D IS=1e-12
.alias ALT REAL subckt
.subckt REAL 1 2
D1 1 2 DM
.ends REAL
";
        let parsed = parse_library("models.lb", library)?;
        let mut catalog = SubcircuitCatalog::new();
        catalog.add_parsed_library(&parsed)?;
        let linked = preprocess_netlist_linked(b"X1 1 0 ALT\n.end\n", &catalog)?;
        assert_eq!(linked.libraries.len(), 1);
        let root = std::str::from_utf8(&linked.root_bytes)?;
        let generated = std::str::from_utf8(&linked.libraries[0].bytes)?;
        assert!(root.contains("REAL_lib_"));
        assert!(generated.contains(".model DM D IS=1e-12"));
        assert!(generated.contains("D1 1 2 DM"));
        assert!(!generated.to_ascii_lowercase().contains(".alias"));
        Ok(())
    }

    #[test]
    fn parsed_library_identity_includes_root_support() -> Result<()> {
        let first = parse_library(
            "a.lb",
            b".model DM D IS=1e-12\n.subckt T 1 2\nD1 1 2 DM\n.ends T\n",
        )?;
        let second = parse_library(
            "renamed.lb",
            b".model DM D IS=2e-12\n.subckt T 1 2\nD1 1 2 DM\n.ends T\n",
        )?;
        assert_ne!(first.source_id(), second.source_id());
        Ok(())
    }

    #[test]
    fn parsed_library_round_trip_is_path_independent_and_link_compatible() -> Result<()> {
        let library = b".subckt T 1 2 vars: A=1\nR1 1 2 {A}\n.ends T\n";
        let first = parse_library("/one/path/a.lb", library)?;
        let second = parse_library("/different/path/renamed.lb", library)?;
        assert_eq!(first.source_id(), second.source_id());

        let json = first.to_json()?;
        let loaded = ParsedLibrary::from_json(&json)?;
        assert_eq!(loaded.source_id(), first.source_id());

        let mut raw_catalog = SubcircuitCatalog::new();
        raw_catalog.add_library_named("raw.lb", library)?;
        let mut parsed_catalog = SubcircuitCatalog::new();
        parsed_catalog.add_parsed_library(&loaded)?;

        let source = b"X1 1 0 T vars: A=7\n.end\n";
        let raw = preprocess_netlist(source, &raw_catalog)?;
        let parsed = preprocess_netlist(source, &parsed_catalog)?;
        assert_eq!(raw.bytes, parsed.bytes);
        Ok(())
    }

    #[test]
    fn ignores_bare_subckt_library_marker() -> Result<()> {
        let mut catalog = SubcircuitCatalog::new();
        let added =
            catalog.add_library(b".subckt\n.subckt REAL 1 2 vars: A=1\nR1 1 2 {A}\n.ends\n")?;
        assert_eq!(added, 1);
        assert!(catalog.resolve("REAL", None)?.is_some());
        Ok(())
    }

    #[test]
    fn continuation_skips_intervening_comments() {
        let lines = vec![
            ".MODEL M VPWLR NSEG=1".to_owned(),
            "*** generated points".to_owned(),
            "+ X0=0 Y0=0 X1=1 Y1=1".to_owned(),
        ];
        let statements = logical_statements(&lines);
        assert_eq!(statements[0], ".MODEL M VPWLR NSEG=1 X0=0 Y0=0 X1=1 Y1=1");
        assert_eq!(statements[1], "*** generated points");
    }

    #[test]
    fn expands_root_first_then_nested_hierarchy() -> Result<()> {
        let library = br".subckt OUTER 1 2 vars: GAIN=1
R1 1 3 {GAIN}
XCH 3 2 INNER vars: SCALE={GAIN+1}
.ends OUTER
.subckt INNER 1 2 vars: SCALE=1
R2 1 2 {SCALE}
.ends INNER
";
        let source = br"X1 1 0 OUTER vars: GAIN=2
X2 2 0 OUTER vars: GAIN=4
.end
";
        let mut catalog = SubcircuitCatalog::new();
        assert_eq!(catalog.add_library(library)?, 2);
        let result = preprocess_netlist(source, &catalog)?;
        let text = std::str::from_utf8(&result.bytes)?;
        assert!(text.contains("X1 1 0 OUTER$1"));
        assert!(text.contains("X2 2 0 OUTER$2"));
        let outer2 = text.find(".subckt OUTER$2").context("missing OUTER$2")?;
        let inner3 = text.find(".subckt INNER$3").context("missing INNER$3")?;
        let outer1 = text.find(".subckt OUTER$1").context("missing OUTER$1")?;
        let inner4 = text.find(".subckt INNER$4").context("missing INNER$4")?;
        assert!(outer2 < inner3 && inner3 < outer1 && outer1 < inner4);
        let outer2_end = text.find(".ends OUTER$2").context("missing OUTER$2 end")?;
        let outer1_end = text.find(".ends OUTER$1").context("missing OUTER$1 end")?;
        assert!(inner3 < outer2_end);
        assert!(inner4 < outer1_end);
        assert!(text.contains("R1 1 3 4"));
        assert!(text.contains("R2 1 2 5"));
        assert!(
            text.lines()
                .next_back()
                .is_some_and(|line| line.eq_ignore_ascii_case(".end"))
        );
        assert_eq!(result.stats.root_specializations, 2);
        assert_eq!(result.stats.generated_subcircuits, 4);
        Ok(())
    }

    #[test]
    fn local_parameterized_definition_shadows_external() -> Result<()> {
        let source = br"X1 1 0 T vars: A=3
.subckt T 1 2 vars: A=1
R1 1 2 {A}
.ends T
.end
";
        let mut external = SubcircuitCatalog::new();
        external.add_library(b".subckt T 1 2 vars: A=9\nR9 1 2 {A}\n.ends T\n")?;
        let result = preprocess_netlist(source, &external)?;
        let text = std::str::from_utf8(&result.bytes)?;
        assert!(text.contains("R1 1 2 3"));
        assert!(!text.contains("R9"));
        assert_eq!(result.stats.local_parameterized_definitions_removed, 1);
        Ok(())
    }

    #[test]
    fn explicit_vars_specializes_plain_local_definition() -> Result<()> {
        let source = br"X1 1 0 2 3 H2$TP_CCVS vars: gain=2
.subckt H2$TP_CCVS 1 2 3 4
H1 1 2 V1 {gain}
V1 3 4 0
.ends
.end
";
        let result = preprocess_netlist(source, &SubcircuitCatalog::new())?;
        let text = std::str::from_utf8(&result.bytes)?;
        assert!(text.contains("X1 1 0 2 3 H2$TP_CCVS$1"));
        assert!(text.contains("H1 1 2 V1 2"));
        assert!(!text.contains(".subckt H2$TP_CCVS 1 2 3 4"));
        assert!(text.contains(".subckt H2$TP_CCVS$1 1 2 3 4"));
        Ok(())
    }

    #[test]
    fn unique_vars_definition_wins_over_plain_simetrix_variant() -> Result<()> {
        let mut catalog = SubcircuitCatalog::new();
        catalog.add_library(b".subckt PARAM_SOURCE 1 2 params: A=1\nR1 1 2 {A}\n.ends\n")?;
        catalog.add_library(b".subckt PARAM_SOURCE 1 2 vars: A=1\nR2 1 2 {A}\n.ends\n")?;
        let result = preprocess_netlist(b"X1 1 0 PARAM_SOURCE vars: A=3\n.end\n", &catalog)?;
        let text = std::str::from_utf8(&result.bytes)?;
        assert!(text.contains("R2 1 2 3"));
        assert!(!text.contains("R1 1 2"));
        Ok(())
    }

    #[test]
    fn nested_helper_prefers_parent_source_library() -> Result<()> {
        let parent_a = br".subckt OUTER 1 2 vars: A=1
X1 1 2 HELPER vars: V={A}
.ends OUTER
.subckt HELPER 1 2 vars: V=1
RA 1 2 {V}
.ends HELPER
";
        let parent_b = br".subckt HELPER 1 2 vars: V=1
RB 1 2 {V}
.ends HELPER
";
        let source = br"X1 1 0 OUTER vars: A=3
.end
";
        let mut catalog = SubcircuitCatalog::new();
        catalog.add_library_named("a.lb", parent_a)?;
        catalog.add_library_named("b.lb", parent_b)?;
        let result = preprocess_netlist(source, &catalog)?;
        let text = std::str::from_utf8(&result.bytes)?;
        assert!(text.contains("RA 1 2 3"));
        assert!(!text.contains("RB 1 2 3"));
        Ok(())
    }

    #[test]
    fn replaces_public_microchip_complementary_pwm() -> Result<()> {
        let source = br#"X1 1 2 3 Complemetary_PWM vars: Frequency=350k Phase=0 DutyRatioMax=90 DutyRatioMin=7 RisingDeadTime=50n FallingDeadTime=50n SOC_CTRL_IN=0 EOC_CTRL_IN=500m
.subckt Complemetary_PWM 22 23 24
?@@--START ENCRYPTION: \"MICROCHIP COMPLEMENTARY_PWM Encryption\"
?@@opaque?##
?@@--END ENCRYPTION: \"MICROCHIP COMPLEMENTARY_PWM Encryption\", 1 lines
.ends Complemetary_PWM
.end
"#;
        let result = preprocess_netlist(source, &SubcircuitCatalog::new())?;
        let text = std::str::from_utf8(&result.bytes)?;
        assert!(text.contains("X1 1 2 3 Complemetary_PWM$1"));
        assert!(text.contains(".subckt Complemetary_PWM$1 22 23 24"));
        assert!(text.contains("V__PWM_RAMP __PWM_RAMP 0 PWL("));
        assert!(text.contains("A__PWM_COMP"));
        assert!(text.contains("A__PWM_RISE_DELAY"));
        assert!(text.contains("A__PWM_FALL_DELAY"));
        assert!(text.contains("B__PWMH 22 0"));
        assert!(text.contains("B__PWML 23 0"));
        assert!(!text.contains("?@@opaque"));
        assert_eq!(result.stats.generated_subcircuits, 1);
        Ok(())
    }

    #[test]
    fn supports_public_microchip_pwm_half_cycle_phase() -> Result<()> {
        let source = br#"X1 1 2 3 Complemetary_PWM vars: Frequency=100k Phase=180 DutyRatioMax=90 DutyRatioMin=10 RisingDeadTime=0 FallingDeadTime=0 SOC_CTRL_IN=0 EOC_CTRL_IN=1
.subckt Complemetary_PWM 22 23 24
?@@--START ENCRYPTION: "MICROCHIP COMPLEMENTARY_PWM Encryption"
?@@opaque?##
?@@--END ENCRYPTION: "MICROCHIP COMPLEMENTARY_PWM Encryption", 1 lines
.ends Complemetary_PWM
.end
"#;
        let result = preprocess_netlist(source, &SubcircuitCatalog::new())?;
        let text = std::str::from_utf8(&result.bytes)?;
        assert!(text.contains("R=0 TD=-5.00000000000000041e-6"));
        assert!(!text.contains("A__PWM_RISE_DELAY"));
        assert!(!text.contains("A__PWM_FALL_DELAY"));
        Ok(())
    }

    #[test]
    fn rejects_invalid_public_microchip_complementary_pwm() {
        let source = br#"X1 1 2 3 Complemetary_PWM vars: Frequency=350k Phase=0 DutyRatioMax=5 DutyRatioMin=7 RisingDeadTime=50n FallingDeadTime=50n SOC_CTRL_IN=0 EOC_CTRL_IN=500m
.subckt Complemetary_PWM 22 23 24
?@@--START ENCRYPTION: \"MICROCHIP COMPLEMENTARY_PWM Encryption\"
?@@opaque?##
?@@--END ENCRYPTION: \"MICROCHIP COMPLEMENTARY_PWM Encryption\", 1 lines
.ends Complemetary_PWM
.end
"#;
        let error = preprocess_netlist(source, &SubcircuitCatalog::new())
            .err()
            .map(|value| value.to_string());
        assert!(
            error.is_some_and(|value| value.contains("invalid or unsupported public Microchip"))
        );
    }

    #[test]
    fn conflicting_library_definitions_fail_closed() -> Result<()> {
        let mut catalog = SubcircuitCatalog::new();
        catalog.add_library(b".subckt T 1 2 vars: A=1\nR1 1 2 {A}\n.ends\n")?;
        catalog.add_library(b".subckt T 1 2 vars: A=2\nR1 1 2 {A}\n.ends\n")?;
        let error = preprocess_netlist(b"X1 1 0 T vars: A=3\n.end\n", &catalog)
            .err()
            .map(|value| value.to_string());
        assert!(error.is_some_and(|value| value.contains("ambiguous SIMPLIS")));
        Ok(())
    }
    #[test]
    fn legacy_schematic_call_target_uses_pinnames_arity() -> Result<()> {
        let inline = "X$U1 out 0 inp inn IC=0 RIN=10Meg pinnames: OUT RTN INP INN";
        assert!(call_target(inline)?.is_none());

        let parameterized =
            "X2 p n vsp vsn out PARAM_OPAMP params: LEVEL=2 A0=100k pinnames: inp inn vsp vsn out";
        let Some((model, overrides, explicit)) = call_target(parameterized)? else {
            bail!("expected PARAM_OPAMP call");
        };
        assert_eq!(model, "PARAM_OPAMP");
        assert!(!explicit);
        assert!(overrides.contains("LEVEL=2"));
        assert!(overrides.contains("A0=100k"));

        let reverse =
            "X$I1 p n sp sn POWER_SUPPLY_LOAD_RES pinnames: P N SENSE_P SENSE_N vars: AXIS=''";
        let Some((model, overrides, explicit)) = call_target(reverse)? else {
            bail!("expected POWER_SUPPLY_LOAD_RES call");
        };
        assert_eq!(model, "POWER_SUPPLY_LOAD_RES");
        assert!(explicit);
        assert_eq!(overrides, "AXIS=''");

        let legacy = "X$S1 p n cp cn gen_switch : RON=1 ROFF=1Meg VON=3 VOFF=2 pinnames: P N CP CN";
        assert_eq!(
            call_target(legacy)?,
            Some(("gen_switch", "RON=1 ROFF=1Meg VON=3 VOFF=2", false))
        );

        let legacy_without_pinnames =
            "X$S1 Vdrv GND U2_OUT GND gen_switch : RON=50 ROFF=1Meg VON=3 VOFF=2";
        assert_eq!(
            call_target(legacy_without_pinnames)?,
            Some(("gen_switch", "RON=50 ROFF=1Meg VON=3 VOFF=2", false))
        );

        let commented = "X1 1 10 11 12 14 MUX1 ; One of four muxes";
        assert_eq!(call_target(commented)?, Some(("MUX1", "", false)));

        let inline_params = "X1 1 2 3 4 VC_SWITCH_V3 Roff={Roff} Ron={Ron} Threshold={Threshold} switch_time_off={switch_time}";
        let Some((model, overrides, explicit)) = call_target(inline_params)? else {
            bail!("expected inline-parameterized VC_SWITCH_V3 call");
        };
        assert_eq!(model, "VC_SWITCH_V3");
        assert!(!explicit);
        assert!(overrides.contains("Roff={Roff}"));
        assert!(overrides.contains("switch_time_off={switch_time}"));
        Ok(())
    }

    #[test]
    fn linked_call_rewrite_strips_schematic_pin_metadata() -> Result<()> {
        assert_eq!(
            replace_call_target(
                "X1 p n LOSSYL pinnames: P N",
                "LOSSYL",
                "sr_LOSSYL_sp_123",
                false,
            )?,
            "X1 p n sr_LOSSYL_sp_123"
        );
        assert_eq!(
            replace_call_target(
                "X2 p n MODEL params: A=1 B={B} pinnames: P N",
                "MODEL",
                "sr_MODEL_sp_123",
                false,
            )?,
            "X2 p n sr_MODEL_sp_123 params: A=1 B={B}"
        );
        assert_eq!(
            replace_call_target(
                "X3 p n MODEL : A=1 B={B} pinnames: P N",
                "MODEL",
                "sr_MODEL_sp_123",
                false,
            )?,
            "X3 p n sr_MODEL_sp_123 params: A=1 B={B}"
        );
        assert_eq!(
            replace_call_target(
                "X4 p n MODEL pinnames: P N vars: A=1",
                "MODEL",
                "sr_MODEL_sp_123",
                true,
            )?,
            "X4 p n sr_MODEL_sp_123"
        );
        assert_eq!(
            replace_call_target("X$Z1 VSN J3_D Z1", "Z1", "sr_Z1_sp_123", false)?,
            "X$Z1 VSN J3_D sr_Z1_sp_123"
        );
        Ok(())
    }

    #[test]
    fn linked_call_preserves_actuals_after_pin_metadata_and_repeated_model_name() -> Result<()> {
        for call in [
            "X1 MODEL n MODEL R = 2k",
            "X1 MODEL n MODEL R=2k pinnames: P N",
            "X1 MODEL n MODEL pinnames: P N params: R = 2k",
        ] {
            let (model, actuals, vars) = call_target(call)?.context("expected call")?;
            assert_eq!(model, "MODEL");
            assert!(!vars);
            assert_eq!(
                parse_overrides(actuals)?.get("R"),
                Some(&crate::Value::Number(2000.0))
            );
            let rewritten = replace_call_target(call, model, "generated", false)?;
            assert!(rewritten.starts_with("X1 MODEL n generated params: R"));
            assert!(!rewritten.contains("pinnames:"));
        }
        Ok(())
    }

    #[test]
    fn linked_vars_definition_consumes_legacy_params_call_overrides() -> Result<()> {
        let source = br"X1 1 0 VDEF params: A=7 pinnames: P N
.subckt VDEF 1 2 vars: A=1
R1 1 2 {A}
.ends VDEF
.end
";
        let linked = preprocess_netlist_linked(source, &SubcircuitCatalog::new())?;
        let root = std::str::from_utf8(&linked.root_bytes)?;
        assert!(!root.contains("params:"));
        assert!(!root.contains("pinnames:"));
        let combined = linked
            .libraries
            .iter()
            .map(|library| std::str::from_utf8(&library.bytes))
            .collect::<Result<Vec<_>, _>>()?
            .join("\n");
        assert!(combined.contains("R1 1 2 7"));
        Ok(())
    }

    #[test]
    fn linked_runtime_subcircuit_parameters_are_preserved() -> Result<()> {
        let source = br"XTOP 1 0 PARENT
.subckt PARENT 1 2 params: Roff=1Meg Ron=1
X1 1 2 CHILD Roff={Roff} Ron={Ron}
.ends PARENT
.subckt CHILD 1 2 params: Roff=1Meg Ron=1
R1 1 2 {Roff}
.ends CHILD
.end
";
        let linked = preprocess_netlist_linked(source, &SubcircuitCatalog::new())?;
        let combined = linked
            .libraries
            .iter()
            .map(|library| std::str::from_utf8(&library.bytes))
            .collect::<Result<Vec<_>, _>>()?
            .join(
                "
",
            );
        assert!(combined.contains("Roff={Roff} Ron={Ron}"));
        assert!(combined.contains("params: Roff=1Meg Ron=1"));
        assert!(combined.contains("R1 1 2 {Roff}"));
        Ok(())
    }

    #[test]
    fn linked_plain_definition_with_preprocessor_directives_is_evaluated() -> Result<()> {
        let source = br#"X1 1 VERSION_CHECK
.subckt VERSION_CHECK 1
.VAR current='FALSE'
.IF { current != 'FALSE' }
.ERROR "inactive"
.ENDIF
R1 1 0 {V(1)}
.ends VERSION_CHECK
.end
"#;
        let linked = preprocess_netlist_linked(source, &SubcircuitCatalog::new())?;
        let combined = linked
            .libraries
            .iter()
            .map(|library| std::str::from_utf8(&library.bytes))
            .collect::<Result<Vec<_>, _>>()?
            .join(
                "
",
            );
        assert!(!combined.contains(".VAR"));
        assert!(!combined.contains(".IF"));
        assert!(combined.contains("R1 1 0 {V(1)}"));
        Ok(())
    }

    #[test]
    fn linked_unresolved_primitive_preserves_init_annotation() -> Result<()> {
        let source = br".INIT X$U1.!DCOMP=1
X$U1 out 0 inp inn IC=0 RIN=10Meg pinnames: OUT RTN INP INN
.end
";
        let linked = preprocess_netlist_linked(source, &SubcircuitCatalog::new())?;
        let root = std::str::from_utf8(&linked.root_bytes)?;
        assert!(root.contains("ngspice_preprocess_init: !DCOMP=1"));
        assert!(!root.contains(".INIT X$U1.!DCOMP"));
        Ok(())
    }

    #[test]
    fn linked_root_direct_diode_pulls_model_support_source() -> Result<()> {
        let mut catalog = SubcircuitCatalog::new();
        catalog.add_library_named("diode.lb", b".model DM D IS=1e-12\n")?;
        let linked = preprocess_netlist_linked(b"D1 a 0 DM\n.end\n", &catalog)?;
        assert_eq!(linked.libraries.len(), 1);
        let generated = std::str::from_utf8(&linked.libraries[0].bytes)?;
        assert!(generated.contains(".model DM D IS=1e-12"));
        Ok(())
    }
}
