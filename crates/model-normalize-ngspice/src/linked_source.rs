//! Source-file linking for native ngspice `.include` and named `.lib` sections.
//!
//! This source-only stage expands library resources before `.param`/`.model`
//! parsing. Each output physical line retains its original file and line.

use crate::{
    SourceStatement,
    lexical::{CommentSyntax, normalize_nbsp, statements_with_syntax},
};
use anyhow::{Context, Result, bail, ensure};
use model_preprocess_command::{AdmittedSource, Resource, SourceSession, WorkBudget};
use std::path::{Path, PathBuf};

mod graph;
pub use graph::{GraphRemap, LinkedLocation, SourceGraph};

/// Physical source location corresponding to a linked output line.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SourceOrigin {
    /// Canonical input path whose bytes contained this statement.
    #[serde(serialize_with = "serialize_path")]
    pub path: PathBuf,
    /// 1-based admitted-source line.
    /// External commands may change line numbering; the path remains original.
    pub line: usize,
    /// Index into the linked source occurrence table.
    pub visit: usize,
    /// Electrical declaration scope, independent of the source occurrence.
    pub scope: usize,
}

/// Linked byte buffer and one origin for each physical output line.
#[derive(Clone, Debug)]
pub struct LinkedSource {
    /// Concatenated source, with original physical statements retained.
    pub bytes: Vec<u8>,
    /// Provenance ordered in exact output-line order.
    pub origins: Vec<SourceOrigin>,
    /// Source occurrences in traversal order, including repeated includes.
    pub visits: Vec<SourceVisit>,
    /// Lexical subcircuit scopes; index zero is the deck scope.
    pub scopes: Vec<SourceScope>,
}

/// A subcircuit declaration in the linked statement stream.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SourceScope {
    /// Parent lexical scope, absent at the deck level.
    pub parent: Option<usize>,
    /// Name of the declared subcircuit, absent at the deck level.
    pub name: Option<String>,
    /// Declaration location in the original resource.
    pub declaration: Option<SourceOrigin>,
}

/// One lexical source occurrence, independent of the cached source bytes.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SourceVisit {
    /// Canonical source identity.
    #[serde(serialize_with = "serialize_path")]
    pub path: PathBuf,
    /// Parent occurrence; absent for the root.
    pub parent: Option<usize>,
    /// Lexical scope at the resource reference.
    pub scope: usize,
    /// Selected named section.
    pub section: Option<String>,
    /// Effective source language.
    pub dialect: SourceDialect,
    /// Source-specific device identity separator.
    pub separator: Option<char>,
    /// Physical declaration line; absent for an inherited dialect.
    pub declaration: Option<usize>,
}

fn serialize_path<S: serde::Serializer>(
    path: &Path,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    serializer.serialize_str(&path.to_string_lossy())
}

impl LinkedSource {
    /// Source dialect of a linked physical line.
    #[must_use = "the source dialect selects the appropriate lowering"]
    pub fn dialect_at(&self, linked_line: usize) -> Option<SourceDialect> {
        self.visits
            .get(self.origin(linked_line)?.visit)
            .map(|visit| visit.dialect)
    }

    /// Map a linked 1-based source line back to the original file and line.
    #[must_use = "the returned origin maps a diagnostic to its admitted source"]
    pub fn origin(&self, linked_line: usize) -> Option<&SourceOrigin> {
        self.origins.get(linked_line.checked_sub(1)?)
    }
}

impl LinkedSource {
    /// Project linked physical rows into logical source statements. The title
    /// and standalone comments are retained. Line numbers index this graph,
    /// not the underlying resource; use `origin` for the physical source.
    /// Continuations cannot cross a resource occurrence or skipped source span.
    ///
    /// # Errors
    /// Invalid provenance, malformed continuation or exhausted work budget.
    pub fn logical_statements(
        &self,
        kind: RootKind,
        budget: &WorkBudget,
    ) -> Result<Vec<SourceStatement>> {
        budget.charge(Resource::ScannedBytes, self.bytes.len())?;
        budget.charge(Resource::Statements, self.origins.len())?;
        budget.charge(Resource::GeneratedNodes, self.origins.len())?;
        let mut offsets = Vec::with_capacity(self.origins.len().saturating_add(1));
        offsets.push(0usize);
        for line in self.bytes.split_inclusive(|byte| *byte == b'\n') {
            offsets.push(offsets.last().copied().context("missing linked offset")? + line.len());
        }
        ensure!(
            offsets.len() == self.origins.len().saturating_add(1),
            "linked physical source map mismatch"
        );
        let mut output = Vec::new();
        let mut begin = 0;
        if kind == RootKind::Deck {
            let end = *offsets.get(1).context("linked deck has no title")?;
            append_statement(
                &mut output,
                1,
                1,
                self.bytes[..end].trim_ascii_end(),
                budget,
            )?;
            begin = 1;
        }
        while begin < self.origins.len() {
            let origin = &self.origins[begin];
            let dialect = self
                .visits
                .get(origin.visit)
                .context("missing linked visit")?
                .dialect;
            let mut end = begin + 1;
            while end < self.origins.len() {
                let previous = &self.origins[end - 1];
                let next = &self.origins[end];
                if next.visit != previous.visit || next.line != previous.line.saturating_add(1) {
                    break;
                }
                end += 1;
            }
            let bytes = &self.bytes[offsets[begin]..offsets[end]];
            for record in records(bytes, begin + 1, dialect)? {
                if record.logical.is_empty() {
                    for index in record.physical {
                        let line = begin + index;
                        let raw = self.bytes[offsets[line]..offsets[line + 1]].trim_ascii_end();
                        if raw.trim_ascii_start().is_empty()
                            || raw.trim_ascii_start().starts_with(b"*")
                        {
                            append_statement(&mut output, line + 1, line + 1, raw, budget)?;
                        } else {
                            let comment = [b"* ".as_slice(), raw].concat();
                            append_statement(&mut output, line + 1, line + 1, &comment, budget)?;
                        }
                    }
                } else {
                    append_statement(
                        &mut output,
                        record.line,
                        begin + record.physical.end,
                        &record.logical,
                        budget,
                    )?;
                }
            }
            begin = end;
        }
        Ok(output)
    }
}

fn append_statement(
    output: &mut Vec<SourceStatement>,
    first_line: usize,
    last_line: usize,
    text: &[u8],
    budget: &WorkBudget,
) -> Result<()> {
    budget.charge(Resource::GeneratedNodes, 1)?;
    budget.charge(Resource::EmittedBytes, text.len())?;
    output.push(SourceStatement {
        first_line,
        last_line,
        text: text.to_vec(),
    });
    Ok(())
}

/// Explicit source search paths; no process-environment or working-directory
/// dependence beyond the caller-supplied root and search directories.
#[derive(Clone, Debug, Default)]
pub struct SourceLinker {
    /// Directories to search after the including file's containing directory.
    pub search_paths: Vec<PathBuf>,
    /// Inherited root source language.
    pub dialect: SourceDialect,
}

/// Source language and its resource-reference syntax.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceDialect {
    /// A single operand introduces a named section; references require a section.
    #[default]
    Native,
    /// Whole-file imports suppress electrical cards outside subcircuits.
    Ltspice,
    /// A single operand references an entire file outside a selected section.
    /// Two operands reference a named section in another file.
    Qspice,
    /// Whole-file and named-section imports with `PSpice` path and header rules.
    Pspice,
    /// TINA resource-reference syntax.
    Tina,
    /// HSPICE named-section syntax.
    Hspice,
    /// `SIMetrix` named-section syntax.
    Simetrix,
}

impl SourceDialect {
    /// Default device identity separator for this source language.
    #[must_use = "the separator determines source device identity"]
    pub const fn default_separator(self) -> Option<char> {
        match self {
            Self::Simetrix => Some('$'),
            _ => None,
        }
    }

    /// Stable source-language identity used by serialized library digests.
    #[must_use = "the identifier contributes to the library digest"]
    pub const fn stable_id(self) -> u8 {
        match self {
            Self::Simetrix => 1,
            Self::Hspice => 2,
            Self::Pspice => 3,
            Self::Native => 4,
            Self::Ltspice => 5,
            Self::Qspice => 6,
            Self::Tina => 7,
        }
    }

    pub(crate) const fn is_pspice(self) -> bool {
        matches!(self, Self::Pspice | Self::Tina)
    }

    const fn named_sections(self) -> bool {
        matches!(self, Self::Native | Self::Hspice | Self::Simetrix)
    }

    const fn portable_paths(self) -> bool {
        matches!(
            self,
            Self::Pspice | Self::Ltspice | Self::Tina | Self::Hspice | Self::Simetrix
        )
    }

    const fn comment_syntax(self) -> CommentSyntax {
        match self {
            Self::Hspice => CommentSyntax::Hspice,
            Self::Simetrix => CommentSyntax::Simetrix,
            _ => CommentSyntax::Native,
        }
    }
}

/// Root source treatment. Included resources are always include-safe.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RootKind {
    /// First physical line is a SPICE deck title.
    Deck,
    /// All lines are library source, not a deck title.
    Include,
}

/// Limits on emitted linked source.
#[derive(Clone, Copy, Debug)]
pub struct LinkLimits {
    /// Maximum concatenated bytes, including repeated includes.
    pub output_bytes: usize,
}

impl Default for LinkLimits {
    fn default() -> Self {
        Self {
            output_bytes: 128 * 1024 * 1024,
        }
    }
}

/// A library import suppresses electrical cards outside its subcircuits.
/// Ordinary includes inherit the containing resource's projection.
#[derive(Debug, Default)]
enum Projection {
    #[default]
    All,
    Library {
        depth: usize,
    },
}

impl Projection {
    fn retain(&mut self, text: &str, dialect: SourceDialect) -> Result<bool> {
        let Self::Library { depth } = self else {
            return Ok(true);
        };
        if directive(text, ".subckt").is_some() {
            *depth = depth
                .checked_add(1)
                .context("LTspice library scope overflow")?;
        } else if directive(text, ".ends").is_some() {
            *depth = depth
                .checked_sub(1)
                .context("unmatched .ends in LTspice library")?;
        } else if *depth == 0
            && (text
                .trim_start()
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphabetic)
                || (dialect == SourceDialect::Qspice
                    && crate::qspice::external_module::is_declaration(text.as_bytes())))
        {
            return Ok(false);
        }
        Ok(true)
    }
}

struct LinkState<'a> {
    settings: &'a SourceLinker,
    root_separator: Option<char>,
    admission: &'a mut SourceSession,
    limits: LinkLimits,
    stack: Vec<VisitFrame>,
    scope: usize,
    output: LinkedSource,
}

struct VisitFrame {
    visit: usize,
    search_paths: Vec<PathBuf>,
}

fn directive<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let prefix = text.get(..name.len())?;
    (prefix.eq_ignore_ascii_case(name)
        && text.as_bytes()[name.len()..]
            .first()
            .is_none_or(u8::is_ascii_whitespace))
    .then_some(text[name.len()..].trim_start())
}

/// Shell-free path/section tokens with balanced quote handling.
/// The backslash is literal inside quoted paths, preserving Windows syntax.
fn operands(input: &str) -> Result<Vec<String>> {
    let mut result = Vec::new();
    let mut rest = input.trim();
    while !rest.is_empty() {
        let mut chars = rest.char_indices();
        let Some((_, first)) = chars.next() else {
            break;
        };
        let (word, consumed) = if matches!(first, '\'' | '"') {
            let Some((end, _)) = chars.find(|&(_, next)| next == first) else {
                bail!("unterminated library path quote in {input}")
            };
            (rest[1..end].to_owned(), end + first.len_utf8())
        } else {
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            (rest[..end].to_owned(), end)
        };
        ensure!(
            !word.is_empty(),
            "empty source library path/section in {input}"
        );
        result.push(word);
        rest = rest[consumed..].trim_start();
        ensure!(
            result.len() <= 2,
            "unexpected extra library operands in {input}"
        );
    }
    Ok(result)
}

fn library_marker(line: &str) -> Result<Option<Vec<String>>> {
    directive(line, ".lib").map(operands).transpose()
}

/// Decode an external named-section reference from an already lexed statement.
///
/// # Errors
/// A .lib directive without exactly a file and a section is invalid here.
pub fn external_library_reference(text: &str) -> Result<Option<(String, String)>> {
    let Some(args) = library_marker(text.trim_start())? else {
        return Ok(None);
    };
    let [path, section] = args
        .try_into()
        .map_err(|_| anyhow::anyhow!("expected .lib file section"))?;
    Ok(Some([path, section].into()))
}

/// Select a named section from already projected, joined source statements.
/// Shares the native linker's section validation and duplicate policy.
///
/// # Errors
/// Missing, duplicate, mismatched, or nested section boundaries.
pub fn select_named_section(text: &str, path: &Path, section: &str) -> Result<String> {
    use std::fmt::Write as _;
    let lines = text.lines().collect::<Vec<_>>();
    let selected = select_named_section_lines(lines.iter().copied(), path, section)?;
    let mut output = String::new();
    for index in selected {
        writeln!(output, "{}", lines[index])?;
    }
    Ok(output)
}

/// Return selected logical line indices without discarding the caller's map.
/// # Errors
/// Missing, duplicate, mismatched, or nested section boundaries.
pub fn select_named_section_lines<'a>(
    lines: impl Iterator<Item = &'a str>,
    path: &Path,
    section: &str,
) -> Result<Vec<usize>> {
    let mut selection = SectionSelection::new(Some(section));
    let mut output = Vec::new();
    for (index, line) in lines.enumerate() {
        if selection.observe(line.trim(), path, index + 1)? && selection.selected() {
            output.push(index);
        }
    }
    selection.finish(path)?;
    Ok(output)
}

const fn name_matches(selected: &str, name: &str) -> bool {
    selected.eq_ignore_ascii_case(name)
}

// The lexical view recognizes directives and continuation boundaries, while
// the emitted bytes retain physical lines, comments and original spelling.
struct Record {
    line: usize,
    physical: std::ops::Range<usize>,
    logical: Vec<u8>,
}

fn records(bytes: &[u8], first_line: usize, dialect: SourceDialect) -> Result<Vec<Record>> {
    let physical_count = bytes.split_inclusive(|byte| *byte == b'\n').count();
    let normalized = normalize_nbsp(bytes);
    let normalized = normalized.as_deref().unwrap_or(bytes);
    let mut result = Vec::new();
    let mut cursor = 0;
    for statement in statements_with_syntax(normalized, dialect.comment_syntax())? {
        let start = statement.line - 1;
        let end = statement.last_line;
        if cursor < start {
            result.push(Record {
                line: first_line + cursor,
                physical: cursor..start,
                logical: Vec::new(),
            });
        }
        result.push(Record {
            line: first_line + start,
            physical: start..end,
            logical: statement.text,
        });
        cursor = end;
    }
    if cursor < physical_count {
        result.push(Record {
            line: first_line + cursor,
            physical: cursor..physical_count,
            logical: Vec::new(),
        });
    }
    Ok(result)
}
impl SourceLinker {
    /// Recursively load one native-ngspice source deck, expanding `.include`
    /// and `.inc`, and selected `.lib path section` references, with origin
    /// mapping, path search and recursive include detection.
    ///
    /// # Errors
    ///
    /// Errors on missing/ambiguous sections, include cycles, malformed
    /// directives, invalid text, and unsupported bare `.lib` references.
    pub fn link(&self, source: &Path) -> Result<LinkedSource> {
        self.link_with_session(source, &mut SourceSession::default())
    }

    /// Link through a caller-owned source admission session. Repeated files
    /// and named sections use one immutable transformed snapshot.
    ///
    /// # Errors
    ///
    /// As in [`Self::link`], plus source admission and external command errors.
    pub fn link_with_session(
        &self,
        source: &Path,
        admission: &mut SourceSession,
    ) -> Result<LinkedSource> {
        let source = admission.load(source)?;
        self.link_admitted(&source, RootKind::Include, LinkLimits::default(), admission)
    }

    /// Link an already-admitted root (including an in-memory source).
    ///
    /// # Errors
    ///
    /// Invalid source, missing or recursive resources, malformed sections,
    /// or exceeded linked-output budget.
    pub fn link_admitted(
        &self,
        source: &AdmittedSource,
        kind: RootKind,
        limits: LinkLimits,
        admission: &mut SourceSession,
    ) -> Result<LinkedSource> {
        self.link_admitted_with_separator(
            source,
            kind,
            limits,
            self.dialect.default_separator(),
            admission,
        )
    }

    /// Link a root with an explicitly inherited device identity separator.
    /// The resource's own legal header still overrides inherited context.
    ///
    /// # Errors
    /// Invalid source, missing or recursive resources, malformed sections,
    /// or exceeded linked-output budget.
    pub fn link_admitted_with_separator(
        &self,
        source: &AdmittedSource,
        kind: RootKind,
        limits: LinkLimits,
        separator: Option<char>,
        admission: &mut SourceSession,
    ) -> Result<LinkedSource> {
        let mut state = LinkState {
            settings: self,
            root_separator: separator,
            admission,
            limits,
            stack: Vec::new(),
            scope: 0,
            output: LinkedSource {
                bytes: Vec::new(),
                origins: Vec::new(),
                visits: Vec::new(),
                scopes: vec![SourceScope {
                    parent: None,
                    name: None,
                    declaration: None,
                }],
            },
        };
        state.expand_source(source, None, kind, &mut Projection::All)?;
        ensure!(state.scope == 0, "unclosed subcircuit in linked source");
        Ok(state.output)
    }
}

fn portable_qspice_path(path: &Path) -> Result<PathBuf> {
    let text = path.to_str().context("non-UTF-8 QSPICE resource path")?;
    if !cfg!(windows) {
        ensure!(
            !text.starts_with("\\\\") && text.as_bytes().get(1).is_none_or(|byte| *byte != b':'),
            "QSPICE Windows absolute resource path {} requires an explicit host path",
            path.display()
        );
    }
    Ok(PathBuf::from(text.replace('\\', "/")))
}

/// A language declaration in one of the source header positions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceDeclaration {
    /// Declared language.
    pub dialect: SourceDialect,
    /// Device identity separator selected by the declaration.
    pub separator: Option<char>,
    /// Original one-based physical line.
    pub line: usize,
}

fn parse_source_header(header: &[u8], line: usize) -> Result<Option<SourceDeclaration>> {
    let mut fields = header
        .split(u8::is_ascii_whitespace)
        .filter(|field| !field.is_empty());
    let token = fields.next().unwrap_or_default();
    if token.eq_ignore_ascii_case(b"*#SIMPLIS") {
        bail!("line {line}: SIMPLIS source requires translation");
    }
    let declarations = [
        (b"*#NATIVE".as_slice(), SourceDialect::Native),
        (b"*#NGSPICE", SourceDialect::Native),
        (b"*#LTSPICE", SourceDialect::Ltspice),
        (b"*#PSPICE", SourceDialect::Pspice),
        (b"*#QSPICE", SourceDialect::Qspice),
        (b"*#TINA", SourceDialect::Tina),
        (b"*#HSPICE", SourceDialect::Hspice),
        (b"*#SIMETRIX", SourceDialect::Simetrix),
    ];
    let Some(dialect) = declarations
        .iter()
        .find_map(|&(name, dialect)| token.eq_ignore_ascii_case(name).then_some(dialect))
    else {
        return Ok(None);
    };
    let mut separator = dialect.default_separator();
    if dialect == SourceDialect::Simetrix {
        for field in fields {
            let Some((name, value)) = field.split_at_checked(4) else {
                continue;
            };
            if !name.eq_ignore_ascii_case(b"sep=") {
                continue;
            }
            if value.eq_ignore_ascii_case(b"none") {
                separator = None;
            } else if let Ok(value) = std::str::from_utf8(value) {
                let mut chars = value.chars();
                if let Some(character) = chars.next()
                    && chars.next().is_none()
                {
                    separator = Some(character);
                }
            }
        }
    }
    Ok(Some(SourceDeclaration {
        dialect,
        separator,
        line,
    }))
}

/// Resolve the legal resource language declaration, ignoring later comments.
///
/// A deck title never changes the language. Included resources may declare
/// it on the first line or after an initial comment/blank line.
///
/// # Errors
/// A SIMPLIS header requires the separate simulator-specific translation.
pub fn source_declaration(input: &[u8], kind: RootKind) -> Result<Option<SourceDeclaration>> {
    let input = input.strip_prefix(b"\xef\xbb\xbf").unwrap_or(input);
    let mut lines = input.split_inclusive(|byte| *byte == b'\n');
    let first = lines.next().unwrap_or_default().trim_ascii();
    let second = lines.next().unwrap_or_default().trim_ascii();
    if kind == RootKind::Deck {
        return parse_source_header(second, 2);
    }
    if let Some(declaration) = parse_source_header(first, 1)? {
        return Ok(Some(declaration));
    }
    if first.is_empty() || first.starts_with(b"*") {
        return parse_source_header(second, 2);
    }
    Ok(None)
}

impl LinkState<'_> {
    fn dialect(&self) -> SourceDialect {
        self.stack.last().map_or(self.settings.dialect, |frame| {
            self.output.visits[frame.visit].dialect
        })
    }

    fn qspice_root(&self) -> Option<&VisitFrame> {
        self.stack
            .iter()
            .rev()
            .take_while(|frame| self.output.visits[frame.visit].dialect == SourceDialect::Qspice)
            .last()
    }

    fn is_qspice_root(&self) -> bool {
        self.qspice_root()
            .zip(self.stack.last())
            .is_some_and(|(root, current)| root.visit == current.visit)
    }

    fn resolve(&self, path: &Path, parent: &Path) -> Result<PathBuf> {
        let portable;
        let qspice = self.dialect() == SourceDialect::Qspice;
        let path = if qspice {
            portable = portable_qspice_path(path)?;
            portable.as_path()
        } else if self.dialect().portable_paths() {
            portable = PathBuf::from(
                path.to_str()
                    .context("non-UTF-8 source resource path")?
                    .replace('\\', "/"),
            );
            portable.as_path()
        } else {
            path
        };
        // QSPICE's library search is rooted in the original deck, not the
        // current nested resource. Source-declared paths are ordered and the
        // explicit caller directories are additional search roots.
        let domain = if qspice { self.qspice_root() } else { None };
        let local = domain.map_or_else(
            || parent.parent(),
            |frame| self.output.visits[frame.visit].path.parent(),
        );
        let candidates = local
            .into_iter()
            .chain(
                domain
                    .into_iter()
                    .flat_map(|frame| frame.search_paths.iter().map(PathBuf::as_path)),
            )
            .chain(self.settings.search_paths.iter().map(PathBuf::as_path))
            .map(|dir| dir.join(path));
        for candidate in candidates {
            match self.admission.resolve_path(&candidate) {
                Ok(canonical) => return Ok(canonical),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!("resolving linked source {}", candidate.display())
                    });
                }
            }
        }
        bail!(
            "cannot resolve source file {} relative to {}",
            path.display(),
            parent.display()
        )
    }

    fn emit(&mut self, path: &Path, physical_line: usize, text: &[u8]) -> Result<()> {
        let frame = self
            .stack
            .last()
            .context("missing linked source occurrence")?;
        let visit = &self.output.visits[frame.visit];
        // Consume a legal dialect declaration once. Retain its physical line
        // as a neutral comment so a native output cannot be re-admitted as
        // the source dialect by a subsequent linker.
        let neutralized;
        let text = if visit.declaration == Some(physical_line) {
            let trimmed = text.trim_ascii_start();
            let position = text.len() - trimmed.len() + 1;
            neutralized = [&text[..position], b" ", &text[position..]].concat();
            neutralized.as_slice()
        } else {
            text
        };
        let extra = usize::from(!text.ends_with(b"\n"));
        ensure!(
            text.len().saturating_add(extra)
                <= self
                    .limits
                    .output_bytes
                    .saturating_sub(self.output.bytes.len()),
            "linked output byte budget exceeded at {}:{physical_line}",
            path.display()
        );
        let budget = self.admission.budget();
        budget.charge(Resource::EmittedBytes, text.len().saturating_add(extra))?;
        budget.charge(Resource::GeneratedNodes, 1)?;
        budget.charge(Resource::EmittedBytes, path.as_os_str().len())?;
        self.output.bytes.extend_from_slice(text);
        if extra != 0 {
            self.output.bytes.push(b'\n');
        }
        self.output.origins.push(SourceOrigin {
            path: path.to_path_buf(),
            line: physical_line,
            visit: frame.visit,
            scope: self.scope,
        });
        Ok(())
    }

    fn expand(
        &mut self,
        requested: &Path,
        parent: &Path,
        section: Option<&str>,
        projection: &mut Projection,
    ) -> Result<()> {
        let path = self.resolve(requested, parent)?;
        // Resolve an existing admitted snapshot before interpreting its syntax.
        let source = self.admission.load(&path)?;
        self.expand_source(&source, section, RootKind::Include, projection)
    }

    fn expand_library(
        &mut self,
        requested: &Path,
        parent: &Path,
        section: Option<&str>,
        containing: &mut Projection,
    ) -> Result<()> {
        if self.dialect() != SourceDialect::Ltspice {
            return self.expand(requested, parent, section, containing);
        }
        let mut projection = Projection::Library { depth: 0 };
        self.expand(requested, parent, section, &mut projection)?;
        ensure!(
            matches!(projection, Projection::Library { depth: 0 }),
            "unterminated subcircuit in LTspice library {}",
            requested.display()
        );
        Ok(())
    }

    fn expand_source(
        &mut self,
        source: &AdmittedSource,
        section: Option<&str>,
        kind: RootKind,
        projection: &mut Projection,
    ) -> Result<()> {
        let path = &source.path;
        let section_key = section.map(str::to_ascii_lowercase);
        ensure!(
            !self.stack.iter().any(|frame| {
                let visit = &self.output.visits[frame.visit];
                visit.path == *path && visit.section == section_key
            }),
            "recursive ngspice source include: {}{}",
            path.display(),
            section.map_or_else(String::new, |name| format!(" section {name}"))
        );
        let budget = self.admission.budget().clone();
        let _depth = budget.enter()?;
        budget.charge(Resource::SourceVisits, 1)?;
        budget.charge(Resource::ScannedBytes, source.bytes.len())?;
        budget.charge(
            Resource::Statements,
            source.bytes.split_inclusive(|byte| *byte == b'\n').count(),
        )?;
        let inherited = self.dialect();
        let visit = self.output.visits.len();
        budget.charge(Resource::GeneratedNodes, 1)?;
        budget.charge(
            Resource::EmittedBytes,
            path.as_os_str()
                .len()
                .saturating_add(section_key.as_ref().map_or(0, String::len)),
        )?;
        self.output.visits.push(SourceVisit {
            path: path.clone(),
            parent: self.stack.last().map(|frame| frame.visit),
            scope: self.scope,
            section: section_key,
            dialect: inherited,
            separator: self.stack.last().map_or_else(
                || self.root_separator,
                |frame| self.output.visits[frame.visit].separator,
            ),
            declaration: None,
        });
        self.stack.push(VisitFrame {
            visit,
            search_paths: Vec::new(),
        });
        let result = self.expand_inner(source, section, kind, projection);
        self.stack.pop();
        result.with_context(|| {
            format!(
                "linking {}{}",
                path.display(),
                section.map_or_else(String::new, |name| format!(" section {name}"))
            )
        })
    }

    fn expand_inner(
        &mut self,
        source: &AdmittedSource,
        section: Option<&str>,
        kind: RootKind,
        projection: &mut Projection,
    ) -> Result<()> {
        let path = &source.path;
        let raw = source
            .bytes
            .strip_prefix(b"\xef\xbb\xbf")
            .unwrap_or(&source.bytes);
        if let Some(declaration) = source_declaration(raw, kind)
            .with_context(|| format!("source header in {}", path.display()))?
        {
            let frame = self
                .stack
                .last()
                .context("missing linked source occurrence")?;
            self.output.visits[frame.visit].dialect = declaration.dialect;
            self.output.visits[frame.visit].separator = declaration.separator;
            self.output.visits[frame.visit].declaration = Some(declaration.line);
        }
        let decoded = if self.dialect() == SourceDialect::Qspice {
            crate::qspice::decode_resource(path, &source.bytes, self.admission.budget())?
        } else {
            std::borrow::Cow::Borrowed(raw)
        };
        crate::lexical::ensure_textual(&decoded)?;
        let (body, first_line) = if kind == RootKind::Deck {
            let title = decoded
                .split_inclusive(|byte| *byte == b'\n')
                .next()
                .context("native deck has no title line")?;
            self.emit(path, 1, title)?;
            (&decoded[title.len()..], 2)
        } else {
            (&decoded[..], 1)
        };
        let records = records(body, first_line, self.dialect())?;
        if self.is_qspice_root() {
            self.collect_source_paths(path, &records, section)?;
        }
        let mut selection = SectionSelection::new(section);
        let mut in_control = false;
        let physical = body
            .split_inclusive(|byte| *byte == b'\n')
            .collect::<Vec<_>>();
        for record in records {
            // Retain external-module declarations for source-only inspection; other
            // non-ASCII device designators require a qualified kernel.
            if self.dialect() == SourceDialect::Qspice
                && record.logical.first().is_some_and(|byte| !byte.is_ascii())
                && !crate::qspice::external_module::is_declaration(&record.logical)
            {
                if selection.selected() {
                    crate::qspice::reject_special_device(path, record.line)?;
                }
                continue;
            }
            let text = std::str::from_utf8(&record.logical).with_context(|| {
                format!(
                    "{}:{}: invalid source encoding",
                    path.display(),
                    record.line
                )
            })?;
            let control_start = directive(text, ".control").is_some();
            let control_end = directive(text, ".endc").is_some();
            if !in_control
                && !control_start
                && !selection.observe_with_syntax(text, path, record.line, self.dialect())?
            {
                continue;
            }
            let selected = selection.selected();
            if selected && self.settings.dialect == SourceDialect::Hspice {
                ensure!(
                    ![".control", ".endc", ".file", ".endf"]
                        .iter()
                        .any(|name| directive(text, name).is_some()),
                    "unsupported standalone SPICE command at {}:{}: {text}",
                    path.display(),
                    record.line
                );
            }
            if control_start {
                in_control = true;
            }
            if selected {
                if in_control || control_end {
                    self.emit_record(path, &record, &physical)?;
                } else if projection.retain(text, self.dialect())? {
                    if self.dialect() == SourceDialect::Qspice {
                        crate::qspice::check_topology_directive(path, record.line, text)?;
                    }
                    self.expand_record(path, &record, &physical, text, kind, projection)?;
                }
            }
            if control_end {
                in_control = false;
            }
        }
        ensure!(
            !in_control,
            "{}: unterminated .control block",
            path.display()
        );
        selection.finish(path)
    }

    fn collect_source_paths(
        &mut self,
        path: &Path,
        records: &[Record],
        section: Option<&str>,
    ) -> Result<()> {
        let mut depth = 0usize;
        let mut selection = SectionSelection::new(section);
        let mut in_control = false;
        for record in records {
            let text = std::str::from_utf8(&record.logical)?;
            let control_start = directive(text, ".control").is_some();
            let control_end = directive(text, ".endc").is_some();
            if !in_control
                && !control_start
                && !selection.observe_with_syntax(text, path, record.line, SourceDialect::Qspice)?
            {
                continue;
            }
            if control_start {
                in_control = true;
            }
            if !selection.selected() {
                if control_end {
                    in_control = false;
                }
                continue;
            }
            if !in_control {
                crate::qspice::check_topology_directive(path, record.line, text)?;
            }
            if control_end {
                in_control = false;
            }
            if [".subckt", ".control"]
                .iter()
                .any(|name| directive(text, name).is_some())
            {
                depth = depth
                    .checked_add(1)
                    .context("QSPICE source scope overflow")?;
            } else if [".ends", ".endc"]
                .iter()
                .any(|name| directive(text, name).is_some())
            {
                depth = depth.saturating_sub(1);
            }
            if let Some(rest) = directive(text, ".libpath") {
                if depth != 0 {
                    return Err(crate::qspice::reject_nested_search(path, record.line));
                }
                let args = operands(rest)?;
                let [directory] = args.as_slice() else {
                    bail!(
                        "{}:{}: .libpath requires one directory",
                        path.display(),
                        record.line
                    );
                };
                let portable = portable_qspice_path(Path::new(directory))
                    .with_context(|| format!("{}:{}: .libpath", path.display(), record.line))?;
                let root = path
                    .parent()
                    .context("QSPICE source has no parent directory")?;
                let search = root.join(portable);
                self.admission
                    .budget()
                    .charge(Resource::EmittedBytes, search.as_os_str().len())?;
                self.stack
                    .last_mut()
                    .context("missing QSPICE source occurrence")?
                    .search_paths
                    .push(search);
            }
        }
        selection.finish(path)
    }

    fn emit_record(&mut self, path: &Path, record: &Record, physical: &[&[u8]]) -> Result<()> {
        for (offset, line) in physical[record.physical.clone()].iter().enumerate() {
            self.emit(path, record.line + offset, line)?;
        }
        Ok(())
    }

    fn expand_record(
        &mut self,
        path: &Path,
        record: &Record,
        physical: &[&[u8]],
        text: &str,
        kind: RootKind,
        projection: &mut Projection,
    ) -> Result<()> {
        let physical_line = record.line;
        if let Some(rest) = directive(text, ".include").or_else(|| directive(text, ".inc")) {
            let args = operands(rest)?;
            ensure!(
                args.len() == 1,
                "{}:{physical_line}: .include requires one pathname",
                path.display()
            );
            return self.expand(Path::new(&args[0]), path, None, projection);
        }
        if let Some(args) = library_marker(text)? {
            if !self.dialect().named_sections() && args.len() == 1 {
                return self.expand_library(Path::new(&args[0]), path, None, projection);
            }
            ensure!(
                args.len() == 2,
                "{}:{physical_line}: native .lib requires a file and section",
                path.display()
            );
            return self.expand_library(Path::new(&args[0]), path, Some(&args[1]), projection);
        }
        if (kind == RootKind::Include && text.eq_ignore_ascii_case(".end"))
            || (self.is_qspice_root() && directive(text, ".libpath").is_some())
        {
            // Consumed search paths and included terminators stay mapped as comments.
            // Keep one mapped comment for each physical source line.
            for (offset, line) in physical[record.physical.clone()].iter().enumerate() {
                let mut comment = b"* ".to_vec();
                comment.extend_from_slice(line);
                self.emit(path, physical_line + offset, &comment)?;
            }
            return Ok(());
        }
        if let Some(rest) = directive(text, ".subckt") {
            let name = rest
                .split_whitespace()
                .next()
                .context("missing subcircuit name")?;
            let visit = self
                .stack
                .last()
                .context("missing linked source occurrence")?
                .visit;
            self.admission
                .budget()
                .charge(Resource::GeneratedNodes, 1)?;
            self.admission.budget().charge(
                Resource::EmittedBytes,
                name.len().saturating_add(path.as_os_str().len()),
            )?;
            let scope = self.output.scopes.len();
            self.output.scopes.push(SourceScope {
                parent: Some(self.scope),
                name: Some(name.to_owned()),
                declaration: Some(SourceOrigin {
                    path: path.to_path_buf(),
                    line: physical_line,
                    visit,
                    scope,
                }),
            });
            self.scope = scope;
        }
        self.emit_record(path, record, physical)?;
        if directive(text, ".ends").is_some() {
            self.scope = self.output.scopes[self.scope]
                .parent
                .with_context(|| format!("{}:{physical_line}: unmatched .ends", path.display()))?;
        }
        Ok(())
    }
}

struct SectionSelection<'a> {
    wanted: Option<&'a str>,
    active: Option<String>,
    found: bool,
}

impl<'a> SectionSelection<'a> {
    const fn new(wanted: Option<&'a str>) -> Self {
        Self {
            wanted,
            active: None,
            found: wanted.is_none(),
        }
    }

    /// Return false for section boundary records; they are never emitted.
    fn observe(&mut self, text: &str, path: &Path, line: usize) -> Result<bool> {
        self.observe_with_syntax(text, path, line, SourceDialect::Native)
    }

    fn observe_with_syntax(
        &mut self,
        text: &str,
        path: &Path,
        line: usize,
        syntax: SourceDialect,
    ) -> Result<bool> {
        if let Some(args) = library_marker(text)?
            && args.len() == 1
            && (syntax.named_sections() || (self.wanted.is_some() && self.active.is_none()))
        {
            ensure!(
                self.active.is_none(),
                "{}:{line}: nested .lib section definitions",
                path.display()
            );
            if self.wanted.is_some_and(|name| name_matches(name, &args[0])) {
                ensure!(
                    !self.found,
                    "{}:{line}: duplicate section {}",
                    path.display(),
                    args[0]
                );
                self.found = true;
            }
            self.active = args.into_iter().next();
            return Ok(false);
        }
        if let Some(rest) = directive(text, ".endl") {
            let ending = operands(rest)?;
            ensure!(
                ending.len() <= 1,
                "{}:{line}: invalid .endl",
                path.display()
            );
            let opened = self
                .active
                .take()
                .with_context(|| format!("{}:{line}: .endl without .lib", path.display()))?;
            ensure!(
                ending.is_empty() || name_matches(&opened, &ending[0]),
                "{}:{line}: .endl does not match .lib {opened}",
                path.display()
            );
            return Ok(false);
        }
        Ok(true)
    }

    fn selected(&self) -> bool {
        match (self.wanted, self.active.as_deref()) {
            (None, None) => true,
            (Some(wanted), Some(opened)) => name_matches(wanted, opened),
            _ => false,
        }
    }

    fn finish(self, path: &Path) -> Result<()> {
        ensure!(
            self.active.is_none(),
            "{}: unterminated .lib section",
            path.display()
        );
        if let Some(name) = self.wanted {
            ensure!(
                self.found,
                "{}: no .lib section named {name}",
                path.display()
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parameter_scope::ModelLibrary;
    use std::fs;

    #[test]
    fn declarations_are_occurrence_scoped_and_restore_the_parent_dialect() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path();
        let root = path.join("root.cir");
        fs::write(
            &root,
            b"title\n.include vendor.lib\n.include shared.inc\n.lib choices.lib enabled\n.end\n",
        )?;
        fs::write(
            path.join("vendor.lib"),
            b"* vendor library\n*#PSPICE\n.lib child.inc\n.include shared.inc\n.param vendor=1\n",
        )?;
        fs::write(path.join("child.inc"), b"*#NATIVE\n.param native_child=1\n.lib disabled\n.include missing.inc\n.endl disabled\n")?;
        fs::write(path.join("shared.inc"), b".param shared=1\n*#LTSPICE\n")?;
        fs::write(path.join("choices.lib"), b"*#LTSPICE\n.lib disabled\n.include nonexistent.inc\n.endl disabled\n.lib enabled\n.param selected=1\n.endl enabled\n")?;
        let mut session = SourceSession::default();
        let admitted = session.load(&root)?;
        let linked = SourceLinker::default().link_admitted(
            &admitted,
            RootKind::Deck,
            LinkLimits::default(),
            &mut session,
        )?;
        let visits = linked
            .visits
            .iter()
            .map(|visit| {
                (
                    visit
                        .path
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                    visit.parent,
                    visit.dialect,
                    visit.declaration,
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            visits,
            [
                ("root.cir".into(), None, SourceDialect::Native, None),
                ("vendor.lib".into(), Some(0), SourceDialect::Pspice, Some(2)),
                ("child.inc".into(), Some(1), SourceDialect::Native, Some(1)),
                ("shared.inc".into(), Some(1), SourceDialect::Pspice, None),
                ("shared.inc".into(), Some(0), SourceDialect::Native, None),
                (
                    "choices.lib".into(),
                    Some(0),
                    SourceDialect::Ltspice,
                    Some(1)
                ),
            ]
        );
        assert_eq!(linked.visits[5].section.as_deref(), Some("enabled"));
        let text = std::str::from_utf8(&linked.bytes)?;
        let physical = text.lines().zip(&linked.origins).collect::<Vec<_>>();
        let mut shared = Vec::new();
        for (index, (line, origin)) in physical.iter().enumerate() {
            if *line == ".param shared=1" {
                assert_eq!(origin.line, 1);
                shared.push(linked.dialect_at(index + 1));
            }
            if *line == ".param native_child=1" {
                assert_eq!(linked.dialect_at(index + 1), Some(SourceDialect::Native));
                assert_eq!(origin.line, 2);
            }
            if *line == ".param selected=1" {
                assert_eq!(linked.dialect_at(index + 1), Some(SourceDialect::Ltspice));
                assert_eq!(origin.line, 6);
            }
        }
        assert_eq!(
            shared,
            [Some(SourceDialect::Pspice), Some(SourceDialect::Native)]
        );
        assert!(text.contains("* #PSPICE"));
        assert!(!text.contains("missing.inc"));
        assert_eq!(linked.origins.len(), text.lines().count());
        Ok(())
    }

    #[test]
    fn repeated_resources_keep_distinct_electrical_scopes() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("root.cir");
        fs::write(&root, b"title\n.subckt a p n\n.include shared.inc\n.ends a\n.subckt b p n\n.include shared.inc\n.ends b\n.end\n")?;
        fs::write(
            temp.path().join("shared.inc"),
            b"*#PSPICE\n.param value=2\n",
        )?;
        let mut session = SourceSession::default();
        let admitted = session.load(&root)?;
        let linked = SourceLinker::default().link_admitted(
            &admitted,
            RootKind::Deck,
            LinkLimits::default(),
            &mut session,
        )?;
        assert_eq!(linked.scopes.len(), 3);
        assert_eq!(linked.scopes[1].name.as_deref(), Some("a"));
        assert_eq!(linked.scopes[2].name.as_deref(), Some("b"));
        assert_eq!(linked.visits[1].scope, 1);
        assert_eq!(linked.visits[2].scope, 2);
        let parameters = linked
            .bytes
            .split_inclusive(|&byte| byte == b'\n')
            .zip(&linked.origins)
            .filter(|(line, _)| line.starts_with(b".param"))
            .map(|(_, origin)| (origin.visit, origin.scope, origin.line))
            .collect::<Vec<_>>();
        assert_eq!(parameters, [(1, 1, 2), (2, 2, 2)]);
        Ok(())
    }

    #[test]
    fn header_options_follow_resource_occurrences_and_not_late_comments() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("root.cir");
        fs::write(&root, b"*#NATIVE title\n*#SIMETRIX sep=:\n.include inherited.inc\n.include explicit.inc\n.end\n")?;
        fs::write(
            temp.path().join("inherited.inc"),
            b".param a=1\n*#SIMETRIX sep=none\n",
        )?;
        fs::write(
            temp.path().join("explicit.inc"),
            b"*#not_a_dialect\n*#SIMETRIX sep=none\n.include inherited.inc\n",
        )?;
        let mut session = SourceSession::default();
        let admitted = session.load(&root)?;
        let linked = SourceLinker::default().link_admitted(
            &admitted,
            RootKind::Deck,
            LinkLimits::default(),
            &mut session,
        )?;
        let separators = linked
            .visits
            .iter()
            .map(|visit| visit.separator)
            .collect::<Vec<_>>();
        assert_eq!(separators, [Some(':'), Some(':'), None, None]);
        assert_eq!(linked.visits[2].declaration, Some(2));
        assert_eq!(linked.visits[1].declaration, None);
        Ok(())
    }

    #[test]
    fn deck_title_does_not_declare_a_dialect() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("root.cir");
        fs::write(&root, b"*#PSPICE\n*#LTSPICE\n.param a=1\n.end\n")?;
        let mut session = SourceSession::default();
        let admitted = session.load(&root)?;
        let linked = SourceLinker::default().link_admitted(
            &admitted,
            RootKind::Deck,
            LinkLimits::default(),
            &mut session,
        )?;
        assert_eq!(linked.visits[0].dialect, SourceDialect::Ltspice);
        assert_eq!(linked.visits[0].declaration, Some(2));
        assert!(linked.bytes.starts_with(b"*#PSPICE\n* #LTSPICE\n"));
        assert_eq!(linked.origin(2).context("header origin")?.line, 2);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn command_runs_once_per_original_source_across_includes_and_sections() -> Result<()> {
        use model_preprocess_command::{AdmissionConfig, Cancellation, CommandConfig};
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("root.cir");
        let library = temp.path().join("library.lib");
        fs::write(
            &root,
            b"title\n.lib library.lib one\n.lib library.lib two\n.inc replacement.inc\n.inc alias.inc\n",
        )?;
        fs::write(
            &library,
            b".lib one\n.param a=1\n.endl one\n.lib two\n.param b=REPLACE_VALUE\n.endl two\n",
        )?;
        let common = temp.path().join("common.inc");
        fs::write(&common, b".param c=3\n")?;
        std::os::unix::fs::symlink(&common, temp.path().join("alias.inc"))?;
        let config = AdmissionConfig {
            command: Some(CommandConfig {
                executable: "/bin/sh".into(),
                arguments: vec![
                    "-c".into(),
                    r#"printf '%s\n' "$1" >> calls; sed "s/replacement.inc/common.inc/g; s/REPLACE_VALUE/2/g""#.into(),
                    "fixture".into(),
                ],
                working_directory: None,
            }),
            ..AdmissionConfig::default()
        };
        let mut admission = SourceSession::new(config, Cancellation::default());
        let linked = SourceLinker::default().link_with_session(&root, &mut admission)?;
        let output = String::from_utf8(linked.bytes)?;
        assert!(output.contains(".param a=1"));
        assert!(output.contains(".param b=2"));
        assert_eq!(output.matches(".param c=3").count(), 2);
        let calls = fs::read_to_string(temp.path().join("calls"))?;
        assert_eq!(calls.lines().count(), 3);
        assert_eq!(calls.matches("library.lib").count(), 1);
        assert_eq!(calls.matches("common.inc").count(), 1);
        let again = SourceLinker::default().link_with_session(&root, &mut admission)?;
        assert_eq!(again.bytes, output.as_bytes());
        assert_eq!(fs::read_to_string(temp.path().join("calls"))?, calls);
        Ok(())
    }

    #[test]
    fn nested_includes_named_sections_provenance_and_scope() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        fs::create_dir(tmp.path().join("libs"))?;
        let common = tmp.path().join("libs/common.inc");
        let library = tmp.path().join("libs/selector.lib");
        let root = tmp.path().join("top.cir");
        fs::write(&common, b".param gain=2\n.func twice(x) {x*2}\n")?;
        fs::write(&library,b".lib unwanted\n.model bad vdmos(vto=1 kp=999)\n.endl unwanted\n.lib chosen\n.param base={twice(gain)}\n.model active vdmos(vto=3 kp={base})\n.endl chosen\n.lib other\n.model bad2 vdmos(vto=1 kp=123)\n.endl other\n")?;
        fs::write(
            &root,
            b"Source title\n.inc 'libs/common.inc'\n.lib \"libs/selector.lib\" chosen\n",
        )?;
        let linked = SourceLinker::default().link(&root)?;
        let parsed = ModelLibrary::parse(&linked.bytes)?;
        assert_eq!(parsed.models.len(), 1);
        assert_eq!(parsed.models[0].card.name, "active");
        let resolved = parsed
            .resolve_vdmos(0, &[])?
            .context("missing linked model")?;
        assert_eq!(
            resolved
                .parameters
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case("kp"))
                .map(|(_, value)| *value),
            Some(4.0)
        );
        let origin = linked
            .origin(parsed.models[0].card.source_line)
            .context("missing model origin")?;
        assert_eq!(origin.path, library.canonicalize()?);
        assert_eq!(origin.line, 6);
        Ok(())
    }

    #[test]
    fn recursive_include_section_mismatch_and_missing_path_are_errors() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let root = tmp.path().join("a.cir");
        let included = tmp.path().join("b.inc");
        fs::write(&root, b"title\n.include b.inc\n")?;
        fs::write(&included, b".include a.cir\n")?;
        assert!(SourceLinker::default().link(&root).is_err());
        fs::write(
            &included,
            b".lib corner\n.model x vdmos(vto=3)\n.endl corner\n",
        )?;
        fs::write(&root, b"title\n.lib b.inc missing\n")?;
        assert!(SourceLinker::default().link(&root).is_err());
        fs::write(&root, b"title\n.lib b.inc corner\n")?;
        assert_eq!(
            ModelLibrary::parse(&SourceLinker::default().link(&root)?.bytes)?
                .models
                .len(),
            1
        );
        fs::write(&root, b"title\n.include missing.inc\n")?;
        assert!(SourceLinker::default().link(&root).is_err());
        Ok(())
    }

    #[test]
    fn same_library_can_reference_another_named_section_without_path_cycle() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("root.cir");
        let file = temp.path().join("parts.lib");
        fs::write(&root, b"title\n.lib parts.lib merged\n")?;
        fs::write(&file, b".lib shared\n.param gain=4\n.endl shared\n.lib merged\n.lib parts.lib shared\n.model selected vdmos(kp={gain})\n.endl merged\n")?;
        let linked = SourceLinker::default().link(&root)?;
        let parsed = ModelLibrary::parse(&linked.bytes)?;
        assert_eq!(parsed.models.len(), 1);
        let model = parsed.resolve_vdmos(0, &[])?.context("selected model")?;
        assert_eq!(
            model
                .parameters
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("kp"))
                .map(|(_, value)| *value),
            Some(4.0)
        );
        Ok(())
    }

    #[test]
    fn search_paths_are_explicit_and_skip_unselected_section_bodies() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let root = tmp.path().join("deck.cir");
        let libs = tmp.path().join("models");
        fs::create_dir(&libs)?;
        fs::write(libs.join("part.inc"), b".param scale=3\n")?;
        fs::write(&root,b"title\n.include part.inc\n.lib disabled\n.model invalid vdmos(vto=1)\n.endl disabled\n.model accepted vdmos (kp={scale})\n")?;
        let linked = SourceLinker {
            search_paths: vec![libs],
            dialect: SourceDialect::Native,
        }
        .link(&root)?;
        let library = ModelLibrary::parse(&linked.bytes)?;
        assert_eq!(library.models.len(), 1);
        assert_eq!(library.models[0].card.name, "accepted");
        let parsed = library.resolve_vdmos(0, &[])?.context("linked model")?;
        assert_eq!(
            parsed
                .parameters
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case("kp"))
                .map(|(_, value)| *value),
            Some(3.0)
        );
        Ok(())
    }
}
