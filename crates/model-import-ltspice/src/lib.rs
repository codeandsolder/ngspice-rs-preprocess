//! Typed headless `LTspice` schematic import. Schematic parsing, electrical
//! topology and source emission are independent of numerical simulation.

mod parse;
mod topology;

use anyhow::{Context, Result, bail, ensure};
use model_preprocess_command::{AdmittedSource, Resource, SourceSession, WorkBudget};
use parse::{Attributes, Placement, Sheet, Symbol};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::Arc,
};
use topology::Nets;

/// One generated physical netlist line's original schematic location.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Origin {
    /// Original source path.
    pub path: PathBuf,
    /// One-based physical source line.
    pub line: usize,
}

/// Source deck and its complete physical line map, before dialect normalization.
#[derive(Debug)]
pub struct ImportedDeck {
    /// Generated source in `LTspice` netlist syntax.
    pub bytes: Vec<u8>,
    /// Source location for every line of `bytes`.
    pub origins: Vec<Origin>,
}

#[derive(Default)]
struct Body {
    bytes: Vec<u8>,
    origins: Vec<Origin>,
}

impl Body {
    fn append(&mut self, mut other: Self) {
        self.bytes.append(&mut other.bytes);
        self.origins.append(&mut other.origins);
    }
}

struct Definition {
    name: String,
    ports: Vec<String>,
}

struct Import<'a> {
    session: &'a mut SourceSession,
    search: &'a [PathBuf],
    limit: usize,
    emitted: usize,
    symbols: BTreeMap<PathBuf, Arc<Symbol>>,
    sheets: BTreeMap<PathBuf, Definition>,
    stack: BTreeSet<PathBuf>,
    definitions: Vec<Body>,
    libraries: BTreeSet<PathBuf>,
    library_lines: Body,
}

fn path_text(raw: &str) -> Result<PathBuf> {
    let raw = raw.trim();
    let raw = if let Some(value) = raw.strip_prefix('"') {
        value
            .strip_suffix('"')
            .context("unclosed resource path quote")?
    } else {
        raw
    };
    ensure!(
        !raw.is_empty() && !raw.contains(['\n', '\r', '\0', '"']),
        "invalid source resource path"
    );
    ensure!(
        cfg!(windows) || (!raw.starts_with("\\\\") && raw.as_bytes().get(1) != Some(&b':')),
        "Windows absolute resource {raw} requires an explicit host path"
    );
    Ok(PathBuf::from(raw.replace('\\', "/")))
}

// Windows library spelling is case-insensitive. On a case-sensitive host,
// prefer an exact path; otherwise walk each missing component and refuse
// ambiguous case-folded matches instead of selecting arbitrary resources.
fn casefold_path(path: &Path, budget: &WorkBudget) -> Result<Option<PathBuf>> {
    let mut resolved = PathBuf::new();
    for component in path.components() {
        let direct = resolved.join(component.as_os_str());
        match direct.symlink_metadata() {
            Ok(_) => {
                resolved = direct;
                continue;
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let Some(name) = component.as_os_str().to_str() else {
            return Ok(None);
        };
        let parent = if resolved.as_os_str().is_empty() {
            Path::new(".")
        } else {
            resolved.as_path()
        };
        let entries = match fs::read_dir(parent) {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let mut found = None;
        for entry in entries {
            budget.charge(Resource::EvaluationSteps, 1)?;
            let entry = entry?;
            if entry
                .file_name()
                .to_str()
                .is_some_and(|candidate| candidate.eq_ignore_ascii_case(name))
            {
                ensure!(
                    found.is_none(),
                    "ambiguous case-insensitive resource {}",
                    direct.display()
                );
                found = Some(entry.path());
            }
        }
        let Some(candidate) = found else {
            return Ok(None);
        };
        resolved = candidate;
    }
    Ok(Some(resolved))
}

// A schematic text record can contain multiple physical SPICE lines. Keep
// literal Windows separators inside quoted operands, and in an unquoted
// library pathname unless the following fragment begins a new directive.
fn directive_lines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut quote = None;
    let mut chars = text.char_indices().peekable();
    while let Some((index, character)) = chars.next() {
        if let Some(active) = quote {
            if character == active {
                quote = None;
            }
        } else if matches!(character, '\'' | '"') {
            quote = Some(character);
        } else if character == '\\' && chars.peek().is_some_and(|&(_, next)| next == 'n') {
            let after = &text[index + 2..];
            let line = &text[start..index];
            let keyword = line
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();
            let is_path = matches!(keyword.as_str(), ".include" | ".inc" | ".lib");
            if !is_path || after.trim_start().starts_with(['.', '+', '*']) {
                lines.push(line);
                start = index + 2;
                chars.next();
            }
        }
    }
    lines.push(&text[start..]);
    lines
}

fn library_operands(input: &str) -> Result<(PathBuf, Option<&str>)> {
    let input = input.trim();
    let (path, rest) =
        if let Some(quote) = input.chars().next().filter(|ch| matches!(ch, '\'' | '"')) {
            let (path, rest) = input[1..]
                .split_once(quote)
                .context("unclosed library path quote")?;
            (path, rest.trim())
        } else {
            input
                .split_once(char::is_whitespace)
                .map_or((input, ""), |(path, rest)| (path, rest.trim()))
        };
    let section = if rest.is_empty() {
        None
    } else {
        let section = rest.trim_matches(['\'', '"']);
        identifier(section)?;
        Some(section)
    };
    Ok((path_text(path)?, section))
}

fn quoted(path: &Path) -> Result<String> {
    let text = path.to_str().context("non-UTF-8 source resource path")?;
    ensure!(
        !text.contains(['"', '\n', '\r']),
        "resource path cannot be represented in a netlist"
    );
    Ok(format!("\"{text}\""))
}

fn identifier(text: &str) -> Result<()> {
    ensure!(
        !text.is_empty()
            && !text.starts_with('$')
            && !text.chars().any(|ch| ch.is_whitespace()
                || matches!(
                    ch,
                    '(' | ')' | ',' | ';' | '"' | '\'' | '=' | '{' | '}' | '\0'
                )),
        "invalid netlist identifier {text:?}"
    );
    Ok(())
}

fn get<'a>(attrs: &'a Attributes, name: &str) -> Option<&'a str> {
    attrs
        .get(name)
        .map(String::as_str)
        .filter(|value| !value.is_empty())
}

fn prefix_name(prefix: &str, name: &str) -> Result<String> {
    identifier(name)?;
    ensure!(
        prefix.len() == 1 && prefix.as_bytes()[0].is_ascii_alphabetic(),
        "invalid SPICE prefix {prefix:?}"
    );
    // LTspice treats a subcircuit prefix as an unconditional namespace
    // marker. Even a displayed instance X1 is exported as XX1.
    if prefix.eq_ignore_ascii_case("x") {
        return Ok(format!("X{name}"));
    }
    if name.as_bytes()[0].eq_ignore_ascii_case(&prefix.as_bytes()[0]) {
        Ok(name.to_owned())
    } else {
        Ok(format!("{prefix}§{name}"))
    }
}

impl Import<'_> {
    fn emit(&mut self, body: &mut Body, path: &Path, line: usize, text: &str) -> Result<()> {
        ensure!(
            !text.contains(['\n', '\r', '\0']),
            "invalid physical netlist line"
        );
        let count = text.len().saturating_add(1);
        ensure!(
            count <= self.limit.saturating_sub(self.emitted),
            "schematic output byte budget exceeded"
        );
        self.session.budget().charge(
            Resource::EmittedBytes,
            count.saturating_add(path.as_os_str().len()),
        )?;
        self.session.budget().charge(Resource::GeneratedNodes, 1)?;
        self.emitted += count;
        body.bytes.extend_from_slice(text.as_bytes());
        body.bytes.push(b'\n');
        body.origins.push(Origin {
            path: path.to_path_buf(),
            line,
        });
        Ok(())
    }

    fn resolve(&self, target: &Path, parents: &[&Path], symbol: bool) -> Result<PathBuf> {
        let mut candidates = Vec::new();
        if target.is_absolute() {
            candidates.push(target.to_path_buf());
        } else {
            candidates.extend(
                parents
                    .iter()
                    .filter_map(|path| path.parent())
                    .map(|dir| dir.join(target)),
            );
            for dir in self.search {
                candidates.push(dir.join(target));
                if symbol {
                    candidates.push(dir.join("sym").join(target));
                    candidates.push(dir.join("lib/sym").join(target));
                } else {
                    candidates.push(dir.join("sub").join(target));
                    candidates.push(dir.join("lib/sub").join(target));
                }
            }
        }
        for candidate in candidates {
            self.session.budget().charge(Resource::EvaluationSteps, 1)?;
            match self.session.resolve_path(&candidate) {
                Ok(path) => return Ok(path),
                Err(error) if error.kind() == ErrorKind::NotFound => {
                    if let Some(path) = casefold_path(&candidate, self.session.budget())? {
                        return self.session.resolve_path(&path).map_err(Into::into);
                    }
                }
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("resolving {}", candidate.display()));
                }
            }
        }
        bail!(
            "cannot resolve resource {} from {parents:?}",
            target.display()
        )
    }

    fn symbol(&mut self, path: &Path, name: &str) -> Result<(PathBuf, Arc<Symbol>)> {
        let mut target = path_text(name)?;
        if !target
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("asy"))
        {
            target.as_mut_os_string().push(".asy");
        }
        let canonical = self.resolve(&target, &[path], true)?;
        self.session.budget().charge(Resource::SourceVisits, 1)?;
        if let Some(symbol) = self.symbols.get(&canonical) {
            return Ok((canonical, Arc::clone(symbol)));
        }
        let admitted = self.session.load(&canonical)?;
        self.session
            .budget()
            .charge(Resource::ScannedBytes, admitted.bytes.len())?;
        let symbol = Arc::new(
            Symbol::parse(&parse::decode(&admitted.bytes)?, self.session.budget())
                .with_context(|| format!("parsing {}", canonical.display()))?,
        );
        self.symbols.insert(canonical.clone(), Arc::clone(&symbol));
        Ok((canonical, symbol))
    }

    fn library(&mut self, raw: &str, path: &Path, symbol: &Path, line: usize) -> Result<()> {
        let target = self.resolve(&path_text(raw)?, &[path, symbol], false)?;
        if self.libraries.insert(target.clone()) {
            let mut body = std::mem::take(&mut self.library_lines);
            self.emit(&mut body, path, line, &format!(".lib {}", quoted(&target)?))?;
            self.library_lines = body;
        }
        Ok(())
    }

    fn primitive(
        &mut self,
        attrs: &Attributes,
        path: &Path,
        symbol_path: &Path,
        line: usize,
    ) -> Result<(String, String)> {
        let prefix = get(attrs, "prefix").context("cell symbol has no Prefix")?;
        let name = prefix_name(
            prefix,
            get(attrs, "instname").context("symbol has no InstName")?,
        )?;
        let subcircuit = prefix.eq_ignore_ascii_case("x");
        let model = get(attrs, "spicemodel");
        let value = get(attrs, "value");
        let value2 = get(attrs, "value2");
        let mut fields = Vec::new();
        if let Some(file) = get(attrs, "modelfile") {
            self.library(file, path, symbol_path, line)?;
            fields.extend([model, value, value2].into_iter().flatten());
        } else if subcircuit && value.is_some() && value2.is_some() && model.is_some() {
            // Fixed subcircuit symbol: SpiceModel names the file, Value is
            // display-only, Value2 is the complete subcircuit call tail.
            self.library(
                model.context("missing subcircuit library")?,
                path,
                symbol_path,
                line,
            )?;
            fields.extend(value2);
        } else {
            fields.extend([model, value, value2].into_iter().flatten());
        }
        fields.extend(
            [get(attrs, "spiceline"), get(attrs, "spiceline2")]
                .into_iter()
                .flatten(),
        );
        ensure!(
            !fields.is_empty(),
            "cell {name} has no electrical value or model"
        );
        let tail = fields.join(" ").replace('µ', "u");
        ensure!(
            !tail.to_ascii_lowercase().contains("$g_"),
            "LTspice global-node identifiers require dialect-aware global binding"
        );
        Ok((name, tail))
    }

    fn directive(
        &mut self,
        body: &mut Body,
        path: &Path,
        line: usize,
        text: &str,
        child: bool,
    ) -> Result<()> {
        for part in directive_lines(text) {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            let (keyword, rest) = part.split_once(char::is_whitespace).unwrap_or((part, ""));
            let keyword = keyword.to_ascii_lowercase();
            ensure!(
                !matches!(keyword.as_str(), ".end" | ".ends" | ".subckt" | ".backanno"),
                "schematic text cannot alter generated sheet boundaries at {}:{line}",
                path.display()
            );
            ensure!(
                !child
                    || !matches!(
                        keyword.as_str(),
                        ".op"
                            | ".ac"
                            | ".dc"
                            | ".tran"
                            | ".noise"
                            | ".step"
                            | ".temp"
                            | ".save"
                            | ".meas"
                            | ".measure"
                            | ".four"
                            | ".tf"
                    ),
                "analysis directive in hierarchical child at {}:{line}",
                path.display()
            );
            if matches!(keyword.as_str(), ".include" | ".inc" | ".lib") {
                let (resource, section) = library_operands(rest)?;
                ensure!(
                    keyword == ".lib" || section.is_none(),
                    "include accepts one resource path"
                );
                let target = self.resolve(&resource, &[path], false)?;
                let directive = if let Some(section) = section {
                    format!("{keyword} {} {section}", quoted(&target)?)
                } else {
                    if keyword == ".lib" && !child && !self.libraries.insert(target.clone()) {
                        continue;
                    }
                    format!("{keyword} {}", quoted(&target)?)
                };
                self.emit(body, path, line, &directive)?;
            } else {
                ensure!(
                    !part.to_ascii_lowercase().contains("$g_"),
                    "LTspice global-node identifiers require dialect-aware global binding"
                );
                self.emit(body, path, line, &part.replace('µ', "u"))?;
            }
        }
        Ok(())
    }

    fn child(&mut self, symbol_path: &Path, symbol: &Symbol) -> Result<String> {
        let child = self.resolve(&symbol_path.with_extension("asc"), &[symbol_path], false)?;
        self.session.budget().charge(Resource::SourceVisits, 1)?;
        let ports = symbol
            .pins
            .iter()
            .map(|pin| {
                pin.name
                    .as_deref()
                    .context("hierarchical symbol pin lacks PinName")
                    .map(str::to_ascii_lowercase)
            })
            .collect::<Result<Vec<_>>>()?;
        ensure!(
            ports.iter().collect::<BTreeSet<_>>().len() == ports.len(),
            "duplicate hierarchical symbol port"
        );
        if let Some(definition) = self.sheets.get(&child) {
            ensure!(
                definition.ports == ports,
                "inconsistent port order for {}",
                child.display()
            );
            ensure!(
                !self.stack.contains(&child),
                "recursive schematic hierarchy at {}",
                child.display()
            );
            return Ok(definition.name.clone());
        }
        let name = format!("__lt_sheet_{}", self.sheets.len() + 1);
        self.sheets.insert(
            child.clone(),
            Definition {
                name: name.clone(),
                ports: ports.clone(),
            },
        );
        let admitted = self.session.load(&child)?;
        let body = self.sheet(&admitted, Some((&name, &ports)))?;
        self.definitions.push(body);
        Ok(name)
    }

    fn component(
        &mut self,
        body: &mut Body,
        path: &Path,
        placement: &Placement,
        resolved: &(PathBuf, Arc<Symbol>),
        nets: &Nets,
        instances: &mut BTreeSet<String>,
    ) -> Result<()> {
        let (symbol_path, symbol) = resolved;
        let mut attributes = symbol.attributes.clone();
        attributes.extend(placement.attributes.clone());
        let (name, tail) = if symbol.block {
            let name = prefix_name(
                "X",
                get(&attributes, "instname").context("hierarchical symbol has no InstName")?,
            )?;
            let child = self.child(symbol_path, symbol)?;
            let args = [
                Some(child.as_str()),
                get(&attributes, "spiceline"),
                get(&attributes, "spiceline2"),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ");
            (name, args)
        } else {
            self.primitive(&attributes, path, symbol_path, placement.line)?
        };
        ensure!(
            instances.insert(name.to_ascii_lowercase()),
            "duplicate instance {name}"
        );
        let mut fields = vec![name];
        for pin in &symbol.pins {
            fields.push(
                nets.name(placement.orientation.place(placement.origin, pin.point))?
                    .to_owned(),
            );
        }
        fields.push(tail);
        self.emit(body, path, placement.line, &fields.join(" "))
    }

    fn sheet(
        &mut self,
        source: &AdmittedSource,
        definition: Option<(&str, &[String])>,
    ) -> Result<Body> {
        let _depth = self.session.budget().enter()?;
        self.session.budget().charge(Resource::SourceVisits, 1)?;
        ensure!(
            self.stack.insert(source.path.clone()),
            "recursive schematic hierarchy at {}",
            source.path.display()
        );
        self.session
            .budget()
            .charge(Resource::ScannedBytes, source.bytes.len())?;
        let sheet = Sheet::parse(&parse::decode(&source.bytes)?, self.session.budget())
            .with_context(|| format!("parsing {}", source.path.display()))?;
        let symbols = sheet
            .placements
            .iter()
            .map(|placement| {
                self.symbol(&source.path, &placement.name)
                    .with_context(|| format!("{}:{}", source.path.display(), placement.line))
            })
            .collect::<Result<Vec<_>>>()?;
        let pins = sheet
            .placements
            .iter()
            .zip(&symbols)
            .flat_map(|(placement, (_, symbol))| {
                symbol
                    .pins
                    .iter()
                    .map(|pin| placement.orientation.place(placement.origin, pin.point))
            });
        let nets = Nets::build(&sheet, pins, self.session.budget())?;
        let mut body = Body::default();
        if let Some((name, ports)) = definition {
            let actual = sheet
                .ports
                .keys()
                .map(|point| -> Result<String> {
                    ensure!(
                        sheet.labels.iter().any(|label| label.point == *point),
                        "hierarchical IOPIN lacks matching FLAG"
                    );
                    Ok(nets.name(*point)?.to_owned())
                })
                .collect::<Result<BTreeSet<_>>>()?;
            ensure!(
                actual.len() == sheet.ports.len()
                    && actual
                        .iter()
                        .eq(ports.iter().collect::<BTreeSet<_>>().into_iter()),
                "hierarchical port mismatch in {}: expected {ports:?}, found {actual:?}",
                source.path.display()
            );
            ensure!(
                !actual.contains("0"),
                "ground cannot be a hierarchical formal port"
            );
            self.emit(
                &mut body,
                &source.path,
                1,
                &format!(".subckt {name} {}", ports.join(" ")),
            )?;
        } else {
            self.emit(&mut body, &source.path, 1, "LTspice schematic")?;
        }
        let mut instances = BTreeSet::new();
        for (placement, resolved) in sheet.placements.iter().zip(&symbols) {
            self.session.budget().charge(Resource::EvaluationSteps, 1)?;
            self.component(
                &mut body,
                &source.path,
                placement,
                resolved,
                &nets,
                &mut instances,
            )
            .with_context(|| format!("{}:{}", source.path.display(), placement.line))?;
        }
        for (line, text) in &sheet.directives {
            self.directive(&mut body, &source.path, *line, text, definition.is_some())?;
        }
        if let Some((name, _)) = definition {
            self.emit(&mut body, &source.path, 1, &format!(".ends {name}"))?;
        }
        self.stack.remove(&source.path);
        Ok(body)
    }
}

/// Import one admitted schematic and all symbols/hierarchical children using
/// the caller's immutable source session and explicit resource search roots.
///
/// # Errors
/// Invalid geometry, ambiguous connectivity, missing resources, inconsistent
/// hierarchy, unsupported electrical records or exhausted work/output budget.
pub fn import(
    root: &AdmittedSource,
    search_paths: &[PathBuf],
    session: &mut SourceSession,
    output_bytes: usize,
) -> Result<ImportedDeck> {
    let mut importer = Import {
        session,
        search: search_paths,
        limit: output_bytes,
        emitted: 0,
        symbols: BTreeMap::new(),
        sheets: BTreeMap::new(),
        stack: BTreeSet::new(),
        definitions: Vec::new(),
        libraries: BTreeSet::new(),
        library_lines: Body::default(),
    };
    let mut body = importer.sheet(root, None)?;
    body.append(std::mem::take(&mut importer.library_lines));
    for child in std::mem::take(&mut importer.definitions) {
        body.append(child);
    }
    importer.emit(&mut body, &root.path, 1, ".end")?;
    Ok(ImportedDeck {
        bytes: body.bytes,
        origins: body.origins,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_keeps_windows_paths_and_named_sections() -> Result<()> {
        assert_eq!(
            directive_lines(r#".include "dir\new.lib"\n.lib dir\new.lib\n.op"#),
            [r#".include "dir\new.lib""#, r".lib dir\new.lib", ".op"]
        );
        let (path, section) = library_operands(r#""model dir\new.lib" "typical""#)?;
        assert_eq!(path, Path::new("model dir/new.lib"));
        assert_eq!(section, Some("typical"));
        assert!(library_operands("x.lib a b").is_err());
        Ok(())
    }
}
