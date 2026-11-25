use anyhow::{Context, Result, bail};
use model_preprocess_command::SourceSession;
use model_preprocess_simplis::{LibrarySimulator, parse_library_file};
use std::{env, fs, path::PathBuf};

const USAGE: &str = "usage: simplis-parse-library [--simulator simplis|simetrix] INPUT OUTPUT.json";

fn parse_simulator(value: &std::ffi::OsStr) -> Result<LibrarySimulator> {
    match value.to_string_lossy().to_ascii_lowercase().as_str() {
        "simplis" => Ok(LibrarySimulator::Simplis),
        "simetrix" => Ok(LibrarySimulator::Simetrix),
        other => bail!("unsupported library simulator {other}; expected simplis or simetrix"),
    }
}

fn main() -> Result<()> {
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
    if args.len() != offset.saturating_add(2) {
        bail!(USAGE);
    }
    let input = PathBuf::from(&args[offset]);
    let output = PathBuf::from(&args[offset + 1]);
    let parsed = parse_library_file(&input, simulator, &mut SourceSession::default())?;
    fs::write(&output, parsed.to_json()?)
        .with_context(|| format!("writing {}", output.display()))?;
    eprintln!(
        "simulator={} source_id={} definitions={}",
        parsed.simulator().as_str(),
        parsed.source_id(),
        parsed.definitions()
    );
    Ok(())
}
