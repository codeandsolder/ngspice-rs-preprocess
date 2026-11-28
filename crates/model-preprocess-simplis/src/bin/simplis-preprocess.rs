use anyhow::{Context, Result, bail};
use model_preprocess_command::SourceSession;
use model_preprocess_simplis::{
    LibrarySimulator, ParsedLibrary, SubcircuitCatalog, preprocess_netlist_file,
};
use std::{env, fs, path::Path};

fn is_parsed_library_path(path: &Path) -> bool {
    path.to_string_lossy().ends_with(".simplis.json")
}

fn main() -> Result<()> {
    let mut args = env::args_os().skip(1);
    let input = args
        .next()
        .context("usage: simplis-preprocess INPUT OUTPUT LIBRARY...")?;
    let output = args
        .next()
        .context("usage: simplis-preprocess INPUT OUTPUT LIBRARY...")?;
    let libraries = args.collect::<Vec<_>>();
    if libraries.is_empty() {
        bail!("at least one SIMPLIS library is required");
    }

    let mut admission = SourceSession::default();
    let mut catalog = SubcircuitCatalog::new();
    let mut definitions = 0usize;
    for library in &libraries {
        let display = library.to_string_lossy();
        let added = if is_parsed_library_path(Path::new(library)) {
            let bytes =
                fs::read(library).with_context(|| format!("reading {}", library.display()))?;
            let parsed = ParsedLibrary::from_json(&bytes)
                .with_context(|| format!("loading parsed SIMPLIS library {display}"))?;
            catalog.add_parsed_library(&parsed)?
        } else {
            catalog.add_library_file(
                Path::new(library),
                LibrarySimulator::Simplis,
                &mut admission,
            )?
        };
        definitions = definitions.saturating_add(added);
    }
    let preprocessed = preprocess_netlist_file(Path::new(&input), &catalog, &mut admission)?;
    fs::write(&output, &preprocessed.bytes)
        .with_context(|| format!("writing {}", output.display()))?;
    eprintln!(
        "catalog_definitions={definitions} root_specializations={} generated_subcircuits={} local_parameterized_removed={}",
        preprocessed.stats.root_specializations,
        preprocessed.stats.generated_subcircuits,
        preprocessed.stats.local_parameterized_definitions_removed,
    );
    Ok(())
}
