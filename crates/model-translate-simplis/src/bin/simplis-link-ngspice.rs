use anyhow::{Context, Result, bail};
use model_preprocess_simplis::{
    LibrarySimulator, ParsedLibrary, SubcircuitCatalog, preprocess_netlist_linked,
};
use model_translate_simplis::{translate_simplis_deck, translate_simplis_expanded_library};
use std::{
    env, fs,
    path::{Path, PathBuf},
};

fn parsed_library_path(path: &Path) -> bool {
    let path = path.to_string_lossy();
    path.ends_with(".simplis.json") || path.ends_with(".simetrix.json")
}

fn parse_simulator(value: &std::ffi::OsStr) -> Result<LibrarySimulator> {
    match value.to_string_lossy().to_ascii_lowercase().as_str() {
        "simplis" => Ok(LibrarySimulator::Simplis),
        "simetrix" => Ok(LibrarySimulator::Simetrix),
        other => bail!("unsupported library simulator {other}; expected simplis or simetrix"),
    }
}

fn include_target(statement: &str) -> Result<Option<String>> {
    let trimmed = statement.trim();
    if trimmed.is_empty() || trimmed.starts_with('*') {
        return Ok(None);
    }
    let mut fields = trimmed.splitn(2, char::is_whitespace);
    let directive = fields.next().unwrap_or_default();
    if !directive.eq_ignore_ascii_case(".include") {
        return Ok(None);
    }
    let rest = fields
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .context("SIMPLIS .include has no path")?;
    if let Some(quoted) = rest.strip_prefix('"') {
        let end = quoted
            .find('"')
            .context("unterminated quoted SIMPLIS .include path")?;
        if !quoted[end + 1..].trim().is_empty() {
            bail!("unexpected text after quoted SIMPLIS .include path");
        }
        return Ok(Some(quoted[..end].to_owned()));
    }
    Ok(Some(
        rest.split_whitespace()
            .next()
            .context("SIMPLIS .include has no path")?
            .to_owned(),
    ))
}

fn inline_init_includes(input: &Path, input_bytes: &[u8]) -> Result<Vec<u8>> {
    let text = std::str::from_utf8(input_bytes)
        .with_context(|| format!("SIMPLIS input {} is not UTF-8/ASCII", input.display()))?;
    let base = input
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut output = String::with_capacity(text.len());
    for line in text.lines() {
        let Some(target) = include_target(line)? else {
            output.push_str(line);
            output.push('\n');
            continue;
        };
        let normalized = target.replace('\\', "/");
        let target_path = Path::new(&normalized);
        if !target_path
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value.eq_ignore_ascii_case("init"))
        {
            output.push_str(line);
            output.push('\n');
            continue;
        }
        let resolved = if target_path.is_absolute() {
            target_path.to_path_buf()
        } else {
            let mut matches = Vec::new();
            for ancestor in base.ancestors() {
                let candidate = ancestor.join(target_path);
                if candidate.is_file() {
                    matches.push(candidate);
                }
            }
            match matches.as_slice() {
                [only] => only.clone(),
                [] => bail!(
                    "cannot resolve SIMPLIS init snapshot {target} from {} or its ancestors",
                    base.display()
                ),
                many => bail!(
                    "ambiguous SIMPLIS init snapshot {target}: {}",
                    many.iter()
                        .map(|path| path.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            }
        };
        let snapshot = fs::read_to_string(&resolved)
            .with_context(|| format!("reading SIMPLIS init snapshot {}", resolved.display()))?;
        output.push_str("* ngspice-rs-preprocess: consumed SIMPLIS init snapshot ");
        output.push_str(line.trim());
        output.push('\n');
        output.push_str(&snapshot);
        if !snapshot.ends_with('\n') {
            output.push('\n');
        }
    }
    Ok(output.into_bytes())
}

fn main() -> Result<()> {
    const USAGE: &str =
        "usage: simplis-link-ngspice [--simulator simplis|simetrix] INPUT OUTPUT_ROOT LIBRARY...";
    let args = env::args_os().skip(1).collect::<Vec<_>>();
    let (simulator, offset) = if args
        .first()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value == "--simulator")
    {
        let value = args.get(1).context(USAGE)?;
        (parse_simulator(value)?, 2usize)
    } else {
        (LibrarySimulator::Simplis, 0usize)
    };
    if args.len() < offset.saturating_add(3) {
        bail!(USAGE);
    }
    let input = PathBuf::from(&args[offset]);
    let output_root = PathBuf::from(&args[offset + 1]);
    let libraries = args[offset + 2..]
        .iter()
        .map(PathBuf::from)
        .collect::<Vec<_>>();

    let input_bytes = fs::read(&input).with_context(|| format!("reading {}", input.display()))?;
    let input_bytes = inline_init_includes(&input, &input_bytes)?;
    let mut catalog = SubcircuitCatalog::new();
    let mut definitions = 0usize;
    for library in &libraries {
        let bytes = fs::read(library).with_context(|| format!("reading {}", library.display()))?;
        let added = if parsed_library_path(library) {
            let parsed = ParsedLibrary::from_json(&bytes)
                .with_context(|| format!("loading parsed library {}", library.display()))?;
            if parsed.simulator() != simulator {
                bail!(
                    "parsed library {} uses {} projection but linker requested {}",
                    library.display(),
                    parsed.simulator().as_str(),
                    simulator.as_str()
                );
            }
            catalog.add_parsed_library(&parsed)?
        } else {
            catalog.add_library_named_for_simulator(library.to_string_lossy(), &bytes, simulator)?
        };
        definitions = definitions.saturating_add(added);
    }

    let linked = preprocess_netlist_linked(&input_bytes, &catalog)?;
    let translated_root = translate_simplis_deck(&linked.root_bytes)?;
    let output_dir = output_root
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(output_dir).with_context(|| format!("creating {}", output_dir.display()))?;
    fs::write(&output_root, &translated_root.bytes)
        .with_context(|| format!("writing {}", output_root.display()))?;

    let mut translated_primitives = translated_root.primitives.len();
    for library in &linked.libraries {
        let translated = translate_simplis_expanded_library(&library.bytes)
            .with_context(|| format!("translating linked library {}", library.source_id))?;
        translated_primitives = translated_primitives.saturating_add(translated.primitives.len());
        let path = output_dir.join(&library.filename);
        fs::write(&path, &translated.bytes)
            .with_context(|| format!("writing {}", path.display()))?;
    }

    eprintln!(
        "simulator={} catalog_definitions={definitions} generated_libraries={} generated_subcircuits={} translated_primitives={translated_primitives}",
        simulator.as_str(),
        linked.libraries.len(),
        linked.stats.generated_subcircuits,
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn include_parser_ignores_comments_and_accepts_windows_separators() -> Result<()> {
        assert!(include_target("*.include \"ignored.init\"")?.is_none());
        assert_eq!(
            include_target(".INCLUDE \".\\\\DVM_SUPPORT\\\\state.init\"")?,
            Some(".\\\\DVM_SUPPORT\\\\state.init".to_owned())
        );
        assert_eq!(
            include_target(".include models.lb")?,
            Some("models.lb".to_owned())
        );
        Ok(())
    }

    #[test]
    fn inlines_relative_init_snapshot_only() -> Result<()> {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "ngspice-rs-preprocess-init-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(dir.join("DVM_SUPPORT"))?;
        fs::create_dir_all(dir.join("SIMPLIS_Data"))?;
        let input = dir.join("SIMPLIS_Data/model.net");
        fs::write(dir.join("DVM_SUPPORT/state.init"), ".INIT X1.S1=CLOSE\n")?;
        let source = b".include \".\\\\DVM_SUPPORT\\\\state.init\"\n.include models.lb\n.end\n";
        let inlined = inline_init_includes(&input, source)?;
        let text = std::str::from_utf8(&inlined)?;
        assert!(text.contains("* ngspice-rs-preprocess: consumed SIMPLIS init snapshot"));
        assert!(text.contains(".INIT X1.S1=CLOSE"));
        assert!(text.contains(".include models.lb"));
        fs::remove_dir_all(dir)?;
        Ok(())
    }
}
