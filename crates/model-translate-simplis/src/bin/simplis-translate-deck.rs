use anyhow::{Context, Result, bail};
use model_translate_simplis::translate_simplis_deck_split;
use std::{env, fs, path::PathBuf};

fn main() -> Result<()> {
    let mut args = env::args_os().skip(1);
    let input = PathBuf::from(
        args.next()
            .context("usage: simplis-translate-deck INPUT OUTPUT_DECK OUTPUT_LIBRARY")?,
    );
    let output_deck = PathBuf::from(
        args.next()
            .context("usage: simplis-translate-deck INPUT OUTPUT_DECK OUTPUT_LIBRARY")?,
    );
    let output_library = PathBuf::from(
        args.next()
            .context("usage: simplis-translate-deck INPUT OUTPUT_DECK OUTPUT_LIBRARY")?,
    );
    if args.next().is_some() {
        bail!("usage: simplis-translate-deck INPUT OUTPUT_DECK OUTPUT_LIBRARY");
    }
    let include_path = if output_deck.parent() == output_library.parent() {
        output_library
            .file_name()
            .context("OUTPUT_LIBRARY has no file name")?
            .to_string_lossy()
            .into_owned()
    } else {
        output_library.to_string_lossy().into_owned()
    };
    let bytes = fs::read(&input).with_context(|| format!("reading {}", input.display()))?;
    let translated = translate_simplis_deck_split(&bytes, &include_path)?;
    fs::write(&output_deck, &translated.deck_bytes)
        .with_context(|| format!("writing {}", output_deck.display()))?;
    fs::write(&output_library, &translated.library_bytes)
        .with_context(|| format!("writing {}", output_library.display()))?;
    eprintln!(
        "primitives={} flattened_subcircuits={} stripped_directives={} library_bytes={}",
        translated.primitives.len(),
        translated.flattened_subcircuits,
        translated.stripped_directives,
        translated.library_bytes.len(),
    );
    Ok(())
}
