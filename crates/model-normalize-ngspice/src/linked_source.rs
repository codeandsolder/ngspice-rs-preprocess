//! Source-file linking for native ngspice `.include` and named `.lib` sections.
//!
//! This source-only stage expands library resources before `.param`/`.model`
//! parsing. Each output physical line retains its original file and line.

use crate::lexical::{normalize_nbsp, statements};
use anyhow::{Context, Result, bail, ensure};
use model_preprocess_command::{AdmittedSource, SourceSession};
use std::path::{Path, PathBuf};

/// Physical source location corresponding to a linked output line.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceOrigin {
    /// Canonical input path whose bytes contained this statement.
    pub path: PathBuf,
    /// 1-based admitted-source line.
    /// External commands may change line numbering; the path remains original.
    pub line: usize,
}

/// Linked byte buffer and one origin for each physical output line.
#[derive(Clone, Debug)]
pub struct LinkedSource {
    /// Concatenated source, with original physical statements retained.
    pub bytes: Vec<u8>,
    /// Provenance ordered in exact output-line order.
    pub origins: Vec<SourceOrigin>,
}

impl LinkedSource {
    /// Map a linked 1-based source line back to the original file and line.
    #[must_use = "the returned origin maps a diagnostic to its admitted source"]
    pub fn origin(&self, linked_line: usize) -> Option<&SourceOrigin> {
        self.origins.get(linked_line.checked_sub(1)?)
    }
}

/// Explicit source search paths; no process-environment or working-directory
/// dependence beyond the caller-supplied root and search directories.
#[derive(Clone, Debug, Default)]
pub struct SourceLinker {
    /// Directories to search after the including file's containing directory.
    pub search_paths: Vec<PathBuf>,
    /// Meaning of one-operand library references.
    pub library_syntax: LibrarySyntax,
}

/// Dialect-specific interpretation of library references.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LibrarySyntax {
    /// A single operand introduces a named section; references require a section.
    #[default]
    NamedSections,
    /// A single operand references an entire file outside a selected section.
    /// Two operands reference a named section in another file.
    WholeFileOrSection,
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

struct LinkState<'a> {
    settings: &'a SourceLinker,
    admission: &'a mut SourceSession,
    limits: LinkLimits,
    stack: Vec<(PathBuf, Option<String>)>,
    output: LinkedSource,
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

fn records(bytes: &[u8], first_line: usize) -> Result<Vec<Record>> {
    let physical_count = bytes.split_inclusive(|byte| *byte == b'\n').count();
    let normalized = normalize_nbsp(bytes);
    let normalized = normalized.as_deref().unwrap_or(bytes);
    let mut starts = vec![0];
    for line in normalized.split_inclusive(|byte| *byte == b'\n') {
        starts.push(starts.last().copied().unwrap_or_default() + line.len());
    }
    let mut result = Vec::new();
    let mut cursor = 0;
    for statement in statements(normalized)? {
        let start = statement.line - 1;
        let end = statement
            .source_ranges(0..statement.text.len())
            .last()
            .map_or(start + 1, |range| {
                starts.partition_point(|offset| *offset < range.end)
            });
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
        let mut state = LinkState {
            settings: self,
            admission,
            limits,
            stack: Vec::new(),
            output: LinkedSource {
                bytes: Vec::new(),
                origins: Vec::new(),
            },
        };
        state.expand_source(source, None, kind)?;
        Ok(state.output)
    }
}

impl LinkState<'_> {
    fn resolve(&self, path: &Path, parent: &Path) -> Result<PathBuf> {
        // Source paths in whole-file library dialects commonly use Windows
        // separators. Normalize relative paths without inventing a drive mapping.
        let portable;
        let path = if self.settings.library_syntax == LibrarySyntax::WholeFileOrSection {
            portable = path
                .to_str()
                .context("non-UTF-8 resource path")?
                .replace('\\', "/");
            Path::new(&portable)
        } else {
            path
        };
        let local = parent.parent().into_iter().map(|dir| dir.join(path));
        let candidates = local.chain(self.settings.search_paths.iter().map(|dir| dir.join(path)));
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
        self.output.bytes.extend_from_slice(text);
        if extra != 0 {
            self.output.bytes.push(b'\n');
        }
        self.output.origins.push(SourceOrigin {
            path: path.to_path_buf(),
            line: physical_line,
        });
        Ok(())
    }

    fn expand(&mut self, requested: &Path, parent: &Path, section: Option<&str>) -> Result<()> {
        let path = self.resolve(requested, parent)?;
        // Resolve an existing admitted snapshot before interpreting its syntax.
        let source = self.admission.load(&path)?;
        self.expand_source(&source, section, RootKind::Include)
    }

    fn expand_source(
        &mut self,
        source: &AdmittedSource,
        section: Option<&str>,
        kind: RootKind,
    ) -> Result<()> {
        let path = &source.path;
        ensure!(
            self.stack.len() < 128,
            "ngspice source linking depth exceeded 128"
        );
        let section_key = section.map(str::to_ascii_lowercase);
        ensure!(
            !self
                .stack
                .iter()
                .any(|(candidate, selected)| candidate == path && selected == &section_key),
            "recursive ngspice source include: {}{}",
            path.display(),
            section.map_or_else(String::new, |name| format!(" section {name}"))
        );
        self.stack.push((path.clone(), section_key));
        let result = self.expand_inner(source, section, kind);
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
    ) -> Result<()> {
        let path = &source.path;
        if self.settings.library_syntax == LibrarySyntax::WholeFileOrSection {
            crate::qspice::validate_resource(path, &source.bytes)?;
        }
        crate::lexical::ensure_textual(&source.bytes)?;
        let (body, first_line) = if kind == RootKind::Deck {
            let title = source
                .bytes
                .split_inclusive(|byte| *byte == b'\n')
                .next()
                .context("native deck has no title line")?;
            self.emit(path, 1, title)?;
            (&source.bytes[title.len()..], 2)
        } else {
            (&source.bytes[..], 1)
        };
        let mut selection = SectionSelection::new(section);
        let mut in_control = false;
        let physical = body
            .split_inclusive(|byte| *byte == b'\n')
            .collect::<Vec<_>>();
        for record in records(body, first_line)? {
            // Recognize non-ASCII device classes before decoding the statement;
            // QSPICE netlists may use either ANSI or UTF-8 device prefixes.
            if self.settings.library_syntax == LibrarySyntax::WholeFileOrSection
                && record.logical.first().is_some_and(|byte| !byte.is_ascii())
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
                && !selection.observe_with_syntax(
                    text,
                    path,
                    record.line,
                    self.settings.library_syntax,
                )?
            {
                continue;
            }
            let selected = selection.selected();
            if control_start {
                in_control = true;
            }
            if selected {
                if in_control || control_end {
                    self.emit_record(path, &record, &physical)?;
                } else {
                    self.expand_record(path, &record, &physical, text, kind)?;
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
    ) -> Result<()> {
        let physical_line = record.line;
        if let Some(rest) = directive(text, ".include").or_else(|| directive(text, ".inc")) {
            let args = operands(rest)?;
            ensure!(
                args.len() == 1,
                "{}:{physical_line}: .include requires one pathname",
                path.display()
            );
            return self.expand(Path::new(&args[0]), path, None);
        }
        if let Some(args) = library_marker(text)? {
            return match args.as_slice() {
                [file, section] => self.expand(Path::new(file), path, Some(section)),
                [file] if self.settings.library_syntax == LibrarySyntax::WholeFileOrSection => {
                    self.expand(Path::new(file), path, None)
                }
                _ => bail!(
                    "{}:{physical_line}: .lib requires a file and section in this dialect",
                    path.display()
                ),
            };
        }
        if kind == RootKind::Include && text.eq_ignore_ascii_case(".end") {
            // An included terminator must not terminate the consuming deck.
            // Keep one mapped comment for each physical source line.
            for (offset, line) in physical[record.physical.clone()].iter().enumerate() {
                let mut comment = b"* ".to_vec();
                comment.extend_from_slice(line);
                self.emit(path, physical_line + offset, &comment)?;
            }
            return Ok(());
        }
        self.emit_record(path, record, physical)
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
        self.observe_with_syntax(text, path, line, LibrarySyntax::NamedSections)
    }

    fn observe_with_syntax(
        &mut self,
        text: &str,
        path: &Path,
        line: usize,
        syntax: LibrarySyntax,
    ) -> Result<bool> {
        if let Some(args) = library_marker(text)?
            && args.len() == 1
            && (syntax == LibrarySyntax::NamedSections
                || (self.wanted.is_some() && self.active.is_none()))
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
            library_syntax: LibrarySyntax::NamedSections,
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
