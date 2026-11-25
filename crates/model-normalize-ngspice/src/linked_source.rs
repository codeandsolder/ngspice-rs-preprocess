//! Source-file linking for native ngspice `.include` and named `.lib` sections.
//!
//! This source-only stage expands library resources before `.param`/`.model`
//! parsing. Each output logical line retains its original file and line.

use crate::model_card::logical_statements;
use anyhow::{Context, Result, bail, ensure};
use model_preprocess_command::SourceSession;
use std::path::{Path, PathBuf};

/// Physical source location corresponding to a linked logical statement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceOrigin {
    /// Canonical input path whose bytes contained this statement.
    pub path: PathBuf,
    /// 1-based admitted-source line, before `+` continuation merging.
    /// External commands may change line numbering; the path remains original.
    pub line: usize,
}

/// Linked byte buffer and one origin for each `\n`-separated logical line.
#[derive(Clone, Debug)]
pub struct LinkedSource {
    /// Normalized concatenated source accepted by `ModelLibrary::parse`.
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
}

struct LinkState<'a> {
    settings: &'a SourceLinker,
    admission: &'a mut SourceSession,
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

const fn name_matches(selected: &str, name: &str) -> bool {
    selected.eq_ignore_ascii_case(name)
}

/// Select one named section, or retain only the statements outside all
/// section definitions. Nested two-operand library references are left
/// intact for recursive expansion by the caller.
fn select_statements(
    lines: Vec<(usize, Vec<u8>)>,
    section: Option<&str>,
    path: &Path,
) -> Result<Vec<(usize, Vec<u8>)>> {
    let mut selected = Vec::new();
    let mut active: Option<String> = None;
    let mut found = section.is_none();
    for (number, bytes) in lines {
        let text = std::str::from_utf8(&bytes)
            .with_context(|| format!("{}:{number}: invalid source encoding", path.display()))?;
        if let Some(args) = library_marker(text)?
            && args.len() == 1
        {
            ensure!(
                active.is_none(),
                "{}:{number}: nested .lib section definitions",
                path.display()
            );
            if section.is_some_and(|name| name_matches(name, &args[0])) {
                ensure!(
                    !found,
                    "{}:{number}: duplicate section {}",
                    path.display(),
                    args[0]
                );
                found = true;
            }
            active = args.into_iter().next();
            continue;
        }
        if let Some(rest) = directive(text, ".endl") {
            let ending = operands(rest)?;
            ensure!(
                ending.len() <= 1,
                "{}:{number}: invalid .endl",
                path.display()
            );
            let opened = active
                .take()
                .with_context(|| format!("{}:{number}: .endl without .lib", path.display()))?;
            ensure!(
                ending.is_empty() || name_matches(&opened, &ending[0]),
                "{}:{number}: .endl does not match .lib {opened}",
                path.display()
            );
            continue;
        }
        let include = match (section, active.as_deref()) {
            (None, None) => true,
            (Some(wanted), Some(opened)) => name_matches(wanted, opened),
            _ => false,
        };
        if include {
            selected.push((number, bytes));
        }
    }
    ensure!(
        active.is_none(),
        "{}: unterminated .lib section",
        path.display()
    );
    if let Some(name) = section {
        ensure!(found, "{}: no .lib section named {name}", path.display());
    }
    Ok(selected)
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
        let mut state = LinkState {
            settings: self,
            admission,
            stack: Vec::new(),
            output: LinkedSource {
                bytes: Vec::new(),
                origins: Vec::new(),
            },
        };
        state.expand(source, None, None)?;
        Ok(state.output)
    }
}

impl LinkState<'_> {
    fn resolve(&self, path: &Path, parent: Option<&Path>) -> Result<PathBuf> {
        let local = parent
            .into_iter()
            .filter_map(Path::parent)
            .map(|dir| dir.join(path));
        let candidates = local
            .chain((parent.is_none()).then(|| path.to_path_buf()))
            .chain(self.settings.search_paths.iter().map(|dir| dir.join(path)));
        for candidate in candidates {
            if let Ok(canonical) = candidate.canonicalize() {
                ensure!(
                    canonical.is_file(),
                    "linked source is not a regular file: {}",
                    canonical.display()
                );
                return Ok(canonical);
            }
        }
        bail!(
            "cannot resolve source file {} relative to {}",
            path.display(),
            parent.map_or_else(|| "<root>".to_owned(), |p| p.display().to_string())
        )
    }

    fn emit(&mut self, path: &Path, physical_line: usize, text: &[u8]) {
        self.output.bytes.extend_from_slice(text);
        self.output.bytes.push(b'\n');
        self.output.origins.push(SourceOrigin {
            path: path.to_path_buf(),
            line: physical_line,
        });
    }

    fn expand(
        &mut self,
        requested: &Path,
        parent: Option<&Path>,
        section: Option<&str>,
    ) -> Result<()> {
        let path = self.resolve(requested, parent)?;
        ensure!(
            self.stack.len() < 128,
            "ngspice source linking depth exceeded 128"
        );
        let section_key = section.map(str::to_ascii_lowercase);
        ensure!(
            !self
                .stack
                .iter()
                .any(|(candidate, selected)| candidate == &path && selected == &section_key),
            "recursive ngspice source include: {}{}",
            path.display(),
            section.map_or_else(String::new, |name| format!(" section {name}"))
        );
        self.stack.push((path.clone(), section_key));
        let result = self.expand_inner(&path, section);
        self.stack.pop();
        result.with_context(|| {
            format!(
                "linking {}{}",
                path.display(),
                section.map_or_else(String::new, |name| format!(" section {name}"))
            )
        })
    }

    fn expand_inner(&mut self, path: &Path, section: Option<&str>) -> Result<()> {
        let source = self.admission.load(path)?;
        let lines = logical_statements(&source.bytes)?;
        let selected = select_statements(lines, section, path)?;
        let mut in_control = false;
        for (physical_line, bytes) in selected {
            let text = std::str::from_utf8(&bytes).with_context(|| {
                format!("{}:{physical_line}: non-UTF-8 statement", path.display())
            })?;
            if directive(text, ".control").is_some() {
                in_control = true;
            }
            if directive(text, ".endc").is_some() {
                in_control = false;
            }
            if in_control || directive(text, ".endc").is_some() {
                self.emit(path, physical_line, &bytes);
                continue;
            }
            if let Some(rest) = directive(text, ".include").or_else(|| directive(text, ".inc")) {
                let args = operands(rest)?;
                ensure!(
                    args.len() == 1,
                    "{}:{physical_line}: .include requires one pathname",
                    path.display()
                );
                self.expand(Path::new(&args[0]), Some(path), None)?;
                continue;
            }
            if let Some(args) = library_marker(text)? {
                ensure!(
                    args.len() == 2,
                    "{}:{physical_line}: native .lib requires a file and section",
                    path.display()
                );
                self.expand(Path::new(&args[0]), Some(path), Some(&args[1]))?;
                continue;
            }
            self.emit(path, physical_line, &bytes);
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
