//! Validate that an independently supplied native external X library really
//! declares the exact formal ABI promised by its model-level wiring schema.
//! This verifies a *port contract*, not behavioral equivalence of opaque code.

use super::super::logical_statements;
use super::{Item, OpaqueXInterface, normalized_statement, parse_items};
use anyhow::{Context, Result, ensure};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub(super) struct NativeDefinition {
    pub(super) formals: Vec<String>,
    pub(super) origin: PathBuf,
    pub(super) statements: Vec<String>,
}

#[derive(Default)]
struct Libraries {
    seen: BTreeSet<(PathBuf, Option<String>)>,
    active: Vec<(PathBuf, Option<String>)>,
    actual: BTreeMap<String, NativeDefinition>,
}

enum Directive {
    Include(String),
    SelectLibrary { path: String, section: String },
    BeginSection(String),
    EndSection(Option<String>),
}

fn directive_args(input: &str) -> Result<Vec<String>> {
    let mut chars = input.chars().peekable();
    let mut fields = Vec::new();
    while let Some(&character) = chars.peek() {
        if character.is_whitespace() {
            chars.next();
            continue;
        }
        if character == ';' {
            break;
        }
        let mut field = String::new();
        if matches!(character, '\'' | '"') {
            let quote = chars.next().context("missing SPICE library quote")?;
            loop {
                match chars.next() {
                    Some(value) if value == quote => break,
                    Some(value) => field.push(value),
                    None => anyhow::bail!("unterminated quoted SPICE library path"),
                }
            }
            ensure!(
                chars
                    .peek()
                    .is_none_or(|next| next.is_whitespace() || *next == ';'),
                "unexpected content after quoted SPICE library path"
            );
        } else {
            while let Some(&next) = chars.peek() {
                if next.is_whitespace() || next == ';' {
                    break;
                }
                field.push(next);
                chars.next();
            }
        }
        ensure!(!field.is_empty(), "empty SPICE library path or section");
        fields.push(field);
    }
    Ok(fields)
}

fn directive(statement: &str) -> Result<Option<Directive>> {
    let normalized = normalized_statement(statement);
    let trimmed = normalized.trim();
    let head = trimmed.split_ascii_whitespace().next().unwrap_or_default();
    let recognized = [".include", ".inc", ".lib", ".endl"]
        .iter()
        .any(|candidate| head.eq_ignore_ascii_case(candidate));
    if !recognized {
        return Ok(None);
    }
    let args = directive_args(trimmed[head.len()..].trim())?;
    if head.eq_ignore_ascii_case(".include") || head.eq_ignore_ascii_case(".inc") {
        ensure!(args.len() == 1, "SPICE include needs exactly one file path");
        return Ok(Some(Directive::Include(args[0].clone())));
    }
    if head.eq_ignore_ascii_case(".endl") {
        ensure!(args.len() <= 1, "SPICE .endl has too many arguments");
        return Ok(Some(Directive::EndSection(args.first().cloned())));
    }
    match args.as_slice() {
        [section] => Ok(Some(Directive::BeginSection(section.to_ascii_uppercase()))),
        [path, section] => Ok(Some(Directive::SelectLibrary {
            path: path.clone(),
            section: section.to_ascii_uppercase(),
        })),
        _ => anyhow::bail!("SPICE .lib needs either a section or a file and section"),
    }
}

fn definitions(
    contents: &str,
    origin: &Path,
    expected: &BTreeSet<String>,
    actual: &mut BTreeMap<String, NativeDefinition>,
) -> Result<()> {
    let items = parse_items(contents)
        .with_context(|| format!("parsing original native model file {}", origin.display()))?;
    for item in items {
        let Item::Subcircuit(block) = item else {
            continue;
        };
        let name = block.name.to_ascii_uppercase();
        if !expected.contains(&name) {
            continue;
        }
        let (ports, _) = super::header(&block.header)?;
        ensure!(
            actual
                .insert(
                    name.clone(),
                    NativeDefinition {
                        formals: ports,
                        origin: origin.to_path_buf(),
                        statements: block
                            .body
                            .iter()
                            .filter_map(|item| match item {
                                Item::Statement(stmt) => Some(normalized_statement(stmt)),
                                Item::Subcircuit(_) => None,
                            })
                            .collect(),
                    },
                )
                .is_none(),
            "multiple definitions of native external X model {name}; latest in {}",
            origin.display()
        );
    }
    Ok(())
}

/// A supplied in-memory root need not have been written yet. Resolve
/// relative includes against its canonical parent; real includes must exist.
fn canonical_library_path(path: &Path, in_memory: bool) -> Result<PathBuf> {
    match fs::canonicalize(path) {
        Ok(canonical) => Ok(canonical),
        Err(error) if in_memory && error.kind() == std::io::ErrorKind::NotFound => {
            // A dangling symlink is not a fresh destination.
            if fs::symlink_metadata(path).is_ok() {
                return Err(error.into());
            }
            let name = path
                .file_name()
                .context("in-memory library path has no file name")?;
            let parent = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            Ok(fs::canonicalize(parent)?.join(name))
        }
        Err(error) => Err(error.into()),
    }
}

fn visit(
    path: &Path,
    source: Option<&str>,
    selected: Option<&str>,
    expected: &BTreeSet<String>,
    libraries: &mut Libraries,
) -> Result<()> {
    ensure!(
        libraries.active.len() < 128,
        "external X library nesting exceeds 128 files"
    );
    let canonical = canonical_library_path(path, source.is_some()).with_context(|| {
        format!(
            "cannot resolve original external X include {}",
            path.display()
        )
    })?;
    let key = (canonical.clone(), selected.map(str::to_ascii_uppercase));
    ensure!(
        !libraries.active.contains(&key),
        "cyclic original external X library reference: {} section {selected:?}",
        canonical.display()
    );
    if !libraries.seen.insert(key.clone()) {
        return Ok(());
    }
    let text = match source {
        Some(s) => s.to_owned(),
        None => fs::read_to_string(&canonical)
            .with_context(|| format!("reading original X library {}", canonical.display()))?,
    };
    libraries.active.push(key);
    scan_library(&text, &canonical, selected, expected, libraries)?;
    libraries.active.pop();
    Ok(())
}

fn scan_library(
    text: &str,
    canonical: &Path,
    selected: Option<&str>,
    expected: &BTreeSet<String>,
    libraries: &mut Libraries,
) -> Result<()> {
    let mut content = String::new();
    let mut section: Option<String> = None;
    let mut selected_count = 0usize;
    for statement in logical_statements(text) {
        match directive(&statement)
            .with_context(|| format!("parsing library directives in {}", canonical.display()))?
        {
            Some(Directive::BeginSection(name)) => {
                ensure!(
                    section.is_none(),
                    "nested .lib section {name} in {}",
                    canonical.display()
                );
                if selected.is_some_and(|requested| name.eq_ignore_ascii_case(requested)) {
                    selected_count += 1;
                    ensure!(
                        selected_count == 1,
                        "duplicate selected .lib section {name} in {}",
                        canonical.display()
                    );
                }
                section = Some(name);
            }
            Some(Directive::EndSection(end_name)) => {
                let start = section
                    .take()
                    .with_context(|| format!("unmatched .endl in {}", canonical.display()))?;
                ensure!(
                    end_name.is_none_or(|name| name.eq_ignore_ascii_case(&start)),
                    "mismatched .endl for .lib {start} in {}",
                    canonical.display()
                );
            }
            other => {
                let enabled = match (&section, selected) {
                    (None, None) => true,
                    (Some(current), Some(requested)) => current.eq_ignore_ascii_case(requested),
                    _ => false,
                };
                if !enabled {
                    continue;
                }
                match other {
                    Some(Directive::Include(next)) => {
                        let included = canonical
                            .parent()
                            .context("external X library path has no parent")?
                            .join(next);
                        visit(&included, None, None, expected, libraries)?;
                    }
                    Some(Directive::SelectLibrary { path, section }) => {
                        let included = canonical
                            .parent()
                            .context("external X library path has no parent")?
                            .join(path);
                        visit(&included, None, Some(&section), expected, libraries)?;
                    }
                    Some(Directive::BeginSection(_) | Directive::EndSection(_)) => {
                        unreachable!("library section markers handled above")
                    }
                    None => {
                        content.push_str(&statement);
                        content.push('\n');
                    }
                }
            }
        }
    }
    ensure!(
        section.is_none(),
        "unterminated .lib section in {}",
        canonical.display()
    );
    if let Some(requested) = selected {
        ensure!(
            selected_count == 1,
            ".lib section {requested} not found in {}",
            canonical.display()
        );
    }
    definitions(&content, canonical, expected, &mut libraries.actual)?;
    Ok(())
}

pub(super) fn load_native_libraries(
    original_path: &Path,
    source: &str,
    expected: &BTreeSet<String>,
) -> Result<BTreeMap<String, NativeDefinition>> {
    let mut libraries = Libraries::default();
    visit(original_path, Some(source), None, expected, &mut libraries)?;
    Ok(libraries.actual)
}

/// Verify the native external X port ABI against a real `.subckt` declaration.
///
/// Recursively follows active SPICE includes and selected .lib sections. This is deliberately
/// filesystem-aware; the pure source translator has no host path context,
/// but the CLI must not invent missing native library definitions.
///
/// # Errors
/// A missing or cyclic library, a conflicting model, or missing/reordered/
/// duplicated/incorrectly-sized native analog/digital formal ports.
pub fn verify_external_native_libraries(
    original_path: &Path,
    source: &str,
    interfaces: &[OpaqueXInterface],
) -> Result<()> {
    let mut requested = BTreeMap::<String, Vec<String>>::new();
    for interface in interfaces {
        if let Some(native) = &interface.native_model {
            // Reuse the same model-level validation as the pure converter.
            super::discovery::opaque_x_interfaces(std::slice::from_ref(interface))?;
            let model = native.to_ascii_uppercase();
            let formals = interface
                .native_formals
                .iter()
                .map(|f| f.to_ascii_uppercase())
                .collect::<Vec<_>>();
            if let Some(previous) = requested.get(&model) {
                ensure!(
                    previous == &formals,
                    "conflicting formal ABI declarations for shared native external X model {model}"
                );
            } else {
                requested.insert(model, formals);
            }
        }
    }
    if requested.is_empty() {
        return Ok(());
    }
    let libraries =
        load_native_libraries(original_path, source, &requested.keys().cloned().collect())?;
    for (name, expected) in requested {
        let definition = libraries.get(&name).with_context(|| {
            format!(
                "native external X model {name} not defined in the original source or its reachable .include libraries"
            )
        })?;
        ensure!(
            definition.formals == expected,
            "native external X model {name} in {} has formals [{}], but the verified ABI requires [{}] in that exact order",
            definition.origin.display(),
            definition.formals.join(" "),
            expected.join(" ")
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::whole::OpaqueXPinKind::{Analog, Digital};

    fn sample(native: Option<&str>) -> OpaqueXInterface {
        OpaqueXInterface {
            model: "ORIGINAL".into(),
            pins: vec![Digital, Analog, Digital],
            native_model: native.map(str::to_owned),
            native_formals: native.map_or_else(Vec::new, |_| {
                ["D", "D_LOAD", "D_DRIVE", "AIN", "Q", "Q_LOAD", "Q_DRIVE"]
                    .into_iter()
                    .map(str::to_owned)
                    .collect()
            }),
        }
    }

    #[test]
    fn verify_unwritten_root_against_real_relative_library() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("new.cir");
        fs::write(
            temp.path().join("native.lib"),
            ".subckt NATIVE D D_LOAD D_DRIVE AIN Q Q_LOAD Q_DRIVE\n.ends NATIVE\n",
        )?;
        let source = ".include native.lib\n.end\n";
        verify_external_native_libraries(&root, source, &[sample(Some("NATIVE"))])?;
        assert!(!root.exists());
        fs::remove_file(temp.path().join("native.lib"))?;
        assert!(
            verify_external_native_libraries(&root, source, &[sample(Some("NATIVE"))]).is_err()
        );
        Ok(())
    }

    #[test]
    fn nested_includes_and_arbitrary_verified_port_names() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("original.cir");
        let nested = temp.path().join("deep libraries");
        fs::create_dir(&nested)?;
        fs::write(
            temp.path().join("first.lib"),
            ".include \"deep libraries/second.lib\"\n",
        )?;
        fs::write(
            nested.join("second.lib"),
            "\
.subckt IRRELEVANT in out\nR1 in out 1k\n.ends IRRELEVANT\n\
.subckt NATIVE D D_LOAD D_DRIVE AIN Q Q_LOAD Q_DRIVE params: X=1\n\
Xforward D D_LOAD D_DRIVE Q Q_LOAD Q_DRIVE BUFFER\n.ends NATIVE\n",
        )?;
        let source = ".include \"first.lib\"\n.end\n";
        fs::write(&root, source)?;
        verify_external_native_libraries(&root, source, &[sample(Some("NATIVE"))])
    }

    #[test]
    fn reordered_or_missing_real_library_port_rejected() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("original.cir");
        fs::write(
            temp.path().join("native.lib"),
            "\
.subckt NATIVE D D_LOAD D_DRIVE AIN Q Q_DRIVE Q_LOAD\n.ends NATIVE\n",
        )?;
        let source = ".include native.lib\n.end\n";
        fs::write(&root, source)?;
        let err = verify_external_native_libraries(&root, source, &[sample(Some("NATIVE"))])
            .err()
            .context("swapped sink/source formals must fail")?;
        assert!(format!("{err:#}").contains("exact order"), "{err:#}");
        fs::remove_file(temp.path().join("native.lib"))?;
        let err = verify_external_native_libraries(&root, source, &[sample(Some("NATIVE"))])
            .err()
            .context("missing native implementation must fail")?;
        assert!(format!("{err:#}").contains("cannot resolve"), "{err:#}");
        Ok(())
    }

    #[test]
    fn duplicates_cycles_and_missing_model_are_rejected() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("original.cir");
        let lib = temp.path().join("native.lib");
        let source = ".include native.lib\n.end\n";
        fs::write(&root, source)?;
        fs::write(
            &lib,
            ".subckt NATIVE A B C D E F G\n.ends\n.subckt NATIVE D D_LOAD D_DRIVE AIN Q Q_LOAD Q_DRIVE\n.ends\n",
        )?;
        let err = verify_external_native_libraries(&root, source, &[sample(Some("NATIVE"))])
            .err()
            .context("duplicate declarations must fail")?;
        assert!(
            format!("{err:#}").contains("multiple definitions"),
            "{err:#}"
        );
        fs::write(&lib, ".include original.cir\n")?;
        let err = verify_external_native_libraries(&root, source, &[sample(Some("NATIVE"))])
            .err()
            .context("recursive include must fail")?;
        assert!(format!("{err:#}").contains("cyclic"), "{err:#}");
        fs::write(&lib, ".subckt DIFFERENT x y\n.ends\n")?;
        let err = verify_external_native_libraries(&root, source, &[sample(Some("NATIVE"))])
            .err()
            .context("missing native implementation must fail")?;
        assert!(format!("{err:#}").contains("not defined"), "{err:#}");
        Ok(())
    }

    #[test]
    fn distinct_original_models_can_share_one_identical_native_abi() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("source.cir");
        let source = ".include native.lib\n.end\n";
        fs::write(&root, source)?;
        fs::write(
            temp.path().join("native.lib"),
            ".subckt NATIVE D D_LOAD D_DRIVE AIN Q Q_LOAD Q_DRIVE\n.ends NATIVE\n",
        )?;
        let first = sample(Some("NATIVE"));
        let mut second = first.clone();
        second.model = "ANOTHER_ORIGINAL".into();
        verify_external_native_libraries(&root, source, &[first.clone(), second.clone()])?;
        second.native_formals.swap(1, 2);
        let err = verify_external_native_libraries(&root, source, &[first, second])
            .err()
            .context("one native model cannot advertise conflicting event ABI orders")?;
        assert!(
            format!("{err:#}").contains("conflicting formal ABI"),
            "{err:#}"
        );
        Ok(())
    }

    #[test]
    fn selected_library_section_only_exposes_its_own_definitions_and_includes() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("original.cir");
        let subdir = temp.path().join("vendor libraries");
        fs::create_dir(&subdir)?;
        fs::write(
            subdir.join("modes.lib"),
            ".lib WRONG\n.subckt NATIVE Q Q_LOAD Q_DRIVE AIN D D_LOAD D_DRIVE\n.ends\n.include absent.lib\n.endl WRONG\n\
.lib GOOD\n.include correct.lib\n.endl GOOD\n",
        )?;
        fs::write(
            subdir.join("correct.lib"),
            ".subckt NATIVE D D_LOAD D_DRIVE AIN Q Q_LOAD Q_DRIVE\n.ends\n",
        )?;
        let source = ".lib \"vendor libraries/modes.lib\" good\n.end\n";
        fs::write(&root, source)?;
        verify_external_native_libraries(&root, source, &[sample(Some("NATIVE"))])?;

        let wrong = ".lib \"vendor libraries/modes.lib\" WRONG\n.end\n";
        let err = verify_external_native_libraries(&root, wrong, &[sample(Some("NATIVE"))])
            .err()
            .context("inactive GOOD section must not mask wrong native ABI")?;
        assert!(
            format!("{err:#}").contains("cannot resolve original external X include"),
            "{err:#}"
        );

        let unselected = ".include \"vendor libraries/modes.lib\"\n.end\n";
        let err = verify_external_native_libraries(&root, unselected, &[sample(Some("NATIVE"))])
            .err()
            .context("plain include must not activate any .lib sections")?;
        assert!(format!("{err:#}").contains("not defined"), "{err:#}");
        Ok(())
    }

    #[test]
    fn malformed_or_missing_library_sections_fail_closed() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("original.cir");
        let lib = temp.path().join("modes.lib");
        fs::write(&root, ".end\n")?;
        let source = ".lib modes.lib GOOD\n.end\n";
        for (contents, message) in [
            (".lib OTHER\n.endl OTHER\n", "not found"),
            (".lib GOOD\n.endl WRONG\n", "mismatched"),
            (".lib GOOD\n.lib OTHER\n.endl OTHER\n.endl GOOD\n", "nested"),
            (
                ".lib GOOD\n.endl GOOD\n.lib GOOD\n.endl GOOD\n",
                "duplicate selected",
            ),
            (".endl GOOD\n", "unmatched"),
            (".lib GOOD\n", "unterminated"),
        ] {
            fs::write(&lib, contents)?;
            let err = verify_external_native_libraries(&root, source, &[sample(Some("NATIVE"))])
                .err()
                .context("invalid library selection must fail")?;
            assert!(format!("{err:#}").contains(message), "{err:#}");
        }
        fs::write(&lib, ".lib GOOD\n.lib modes.lib GOOD\n.endl GOOD\n")?;
        let err = verify_external_native_libraries(&root, source, &[sample(Some("NATIVE"))])
            .err()
            .context("same selected library recursively referencing itself must fail")?;
        assert!(format!("{err:#}").contains("cyclic"), "{err:#}");
        Ok(())
    }

    #[test]
    fn distinct_selected_sections_in_same_file_do_not_collapse_visited_keys() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("original.cir");
        let source = ".lib modes.lib FIRST\n.lib modes.lib SECOND\n.end\n";
        fs::write(&root, source)?;
        fs::write(
            temp.path().join("modes.lib"),
            ".lib FIRST\n.subckt OTHER A B\n.ends\n.endl FIRST\n\
.lib SECOND\n.subckt NATIVE D D_LOAD D_DRIVE AIN Q Q_LOAD Q_DRIVE\n.ends\n.endl SECOND\n",
        )?;
        verify_external_native_libraries(&root, source, &[sample(Some("NATIVE"))])
    }

    #[test]
    fn analog_only_native_external_model_checks_actual_formals() -> Result<()> {
        // A native SPICE replacement can be all analog. No fictitious
        // event-state triples may be inserted for an analog-only model.
        verify_external_native_libraries(Path::new("/does/not/exist"), "", &[sample(None)])?;
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("original.cir");
        let library = temp.path().join("analog.lib");
        let source = ".include analog.lib\n.end\n";
        fs::write(&root, source)?;
        fs::write(
            &library,
            ".subckt NATIVE OUT IN\nR1 IN OUT 10\n.ends NATIVE\n",
        )?;
        let model = OpaqueXInterface {
            model: "VENDOR".into(),
            pins: vec![Analog, Analog],
            native_model: Some("NATIVE".into()),
            native_formals: vec!["OUT".into(), "IN".into()],
        };
        verify_external_native_libraries(&root, source, std::slice::from_ref(&model))?;
        let mut incorrect = model.clone();
        incorrect.native_formals.swap(0, 1);
        let err = verify_external_native_libraries(&root, source, &[incorrect])
            .err()
            .context("analog-only ABI with swapped ordered formals must fail")?;
        assert!(
            format!("{err:#}").contains("in that exact order"),
            "{err:#}"
        );
        fs::write(&library, ".subckt OTHER OUT IN\n.ends OTHER\n")?;
        let err = verify_external_native_libraries(&root, source, &[model])
            .err()
            .context("absent analog-only native model must fail")?;
        assert!(format!("{err:#}").contains("not defined"), "{err:#}");
        Ok(())
    }
}
