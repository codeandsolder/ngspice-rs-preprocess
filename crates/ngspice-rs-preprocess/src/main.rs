//! Command-line adapter for the same public preparation API used by callers.
use anyhow::{Context, Result, bail, ensure};
use ngspice_rs_preprocess::{
    Cancellation, CommandConfig, ConditioningPolicy, InputFormat, LibraryInput, Options,
    PreparedBundle, Request, SourceInput, prepare,
};
use serde::Serialize;
use std::{
    ffi::{OsStr, OsString},
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

const USAGE: &str = "\
Usage: ngspice-rs-preprocess FORMAT INPUT OUTPUT_DIRECTORY [OPTIONS]
Formats: native-deck, native-library, simplis-deck, simetrix-deck,
         expanded-deck, expanded-library, simplis-library, simetrix-library
Options:
  --library PATH              Raw catalog library (repeatable)
  --parsed-library PATH       Compiled catalog library (repeatable)
  --search PATH               Native include search directory (repeatable)
  --command ABSOLUTE_PATH     Optional source transformation
  --command-arg ARG           Literal command argument (repeatable)
  --command-cwd ABSOLUTE_PATH Override source-parent working directory
  --max-input-bytes N         Per-source input ceiling
  --max-command-output-bytes N
  --max-diagnostic-bytes N
  --timeout-ms N              Per-command deadline in milliseconds
  --max-output-bytes N        Combined artifact bytes ceiling
  --conditioning MODE         private-differentiators (default) or disabled
The output directory must not already exist.
";

struct Invocation {
    request: Request,
    options: Options,
    output: PathBuf,
}

fn numeric(value: &OsStr) -> Result<usize> {
    Ok(value
        .to_str()
        .context("numeric option is not UTF-8")?
        .parse()?)
}

fn parse(mut args: impl Iterator<Item = OsString>) -> Result<Option<Invocation>> {
    let first = args.next().context(USAGE)?;
    if first == "--help" || first == "-h" {
        ensure!(args.next().is_none(), USAGE);
        return Ok(None);
    }
    let format = InputFormat::parse(first.to_str().context("input format is not UTF-8")?)?;
    let input = SourceInput::File(args.next().context(USAGE)?.into());
    let output = PathBuf::from(args.next().context(USAGE)?);
    let mut request = Request {
        format,
        input,
        libraries: Vec::new(),
        search_paths: Vec::new(),
    };
    let mut options = Options::default();
    let mut command = None;
    let mut command_args = Vec::new();
    let mut cwd = None;
    let mut seen = std::collections::BTreeSet::new();
    while let Some(flag) = args.next() {
        let name = flag.to_str().context("option is not UTF-8")?;
        let repeatable = matches!(
            name,
            "--library" | "--parsed-library" | "--search" | "--command-arg"
        );
        ensure!(
            repeatable || seen.insert(flag.clone()),
            "duplicate option {name}"
        );
        let value = args
            .next()
            .with_context(|| format!("missing value for {name}"))?;
        match name {
            "--library" => request
                .libraries
                .push(LibraryInput::Source(SourceInput::File(value.into()))),
            "--parsed-library" => request
                .libraries
                .push(LibraryInput::ParsedFile(value.into())),
            "--search" => request.search_paths.push(value.into()),
            "--command" => command = Some(PathBuf::from(value)),
            "--command-arg" => command_args.push(value),
            "--command-cwd" => cwd = Some(PathBuf::from(value)),
            "--max-input-bytes" => options.admission.input_bytes = numeric(&value)?,
            "--max-command-output-bytes" => {
                options.admission.process.output_bytes = numeric(&value)?;
            }
            "--max-diagnostic-bytes" => {
                options.admission.process.diagnostic_bytes = numeric(&value)?;
            }
            "--timeout-ms" => {
                options.admission.process.timeout =
                    Duration::from_millis(u64::try_from(numeric(&value)?)?);
            }
            "--max-output-bytes" => options.output_bytes = numeric(&value)?,
            "--conditioning" => {
                options.conditioning = match value.to_str() {
                    Some("private-differentiators") => ConditioningPolicy::PrivateDifferentiators,
                    Some("disabled") => ConditioningPolicy::Disabled,
                    _ => bail!("unknown conditioning policy {}", value.display()),
                };
            }
            _ => bail!("unknown option {name:?}\n{USAGE}"),
        }
    }
    if let Some(executable) = command {
        ensure!(
            executable.is_absolute(),
            "--command requires an absolute executable pathname"
        );
        ensure!(
            cwd.as_ref().is_none_or(|path| path.is_absolute()),
            "--command-cwd requires an absolute pathname"
        );
        options.admission.command = Some(CommandConfig {
            executable,
            arguments: command_args,
            working_directory: cwd,
        });
    } else {
        ensure!(
            command_args.is_empty() && cwd.is_none(),
            "command arguments and cwd require --command"
        );
    }
    Ok(Some(Invocation {
        request,
        options,
        output,
    }))
}

#[derive(Serialize)]
struct Manifest<'a> {
    version: u32,
    input_format: InputFormat,
    origin: String,
    primary: &'a str,
    artifacts: Vec<ManifestArtifact<'a>>,
    normalization: &'a [model_normalize_ngspice::NormalizationPass],
    required_code_models: Vec<ngspice_rs_preprocess::CodeModelRuntime>,
    device_identities: &'a [model_preprocess_simplis::source_dialect::DeviceIdentity],
    initial_conditions: &'a Option<ngspice_rs_preprocess::passive_ic::InitialConditions>,
    analysis_plan: &'a Option<ngspice_rs_preprocess::analysis_plan::AnalysisPlan>,
    waveforms: &'a [ngspice_rs_preprocess::waveform::PreparedWaveform],
}

#[derive(Serialize)]
struct ManifestArtifact<'a> {
    filename: &'a str,
    bytes: usize,
    provenance: &'a ngspice_rs_preprocess::Provenance,
    dependencies: &'a [String],
    required_code_models: &'a [ngspice_rs_preprocess::CodeModelRuntime],
    current_scalings: &'a [model_normalize_ngspice::conditioning::CurrentScaling],
}

fn manifest(bundle: &PreparedBundle) -> Manifest<'_> {
    Manifest {
        version: 6,
        input_format: bundle.format,
        origin: bundle.origin.to_string_lossy().into_owned(),
        primary: &bundle.primary.filename,
        artifacts: bundle
            .artifacts()
            .map(|artifact| ManifestArtifact {
                filename: &artifact.filename,
                bytes: artifact.bytes.len(),
                provenance: &artifact.provenance,
                dependencies: &artifact.dependencies,
                required_code_models: &artifact.required_code_models,
                current_scalings: &artifact.current_scalings,
            })
            .collect(),
        normalization: &bundle.normalization,
        required_code_models: bundle.required_code_models(),
        device_identities: &bundle.device_identities,
        initial_conditions: &bundle.initial_conditions,
        analysis_plan: &bundle.analysis_plan,
        waveforms: &bundle.waveforms,
    }
}

fn output_path(path: &Path) -> Result<PathBuf> {
    let name = path
        .file_name()
        .context("output must name a new directory")?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let destination = parent.canonicalize()?.join(name);
    match fs::symlink_metadata(&destination) {
        Ok(_) => bail!("output already exists: {}", destination.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(destination),
        Err(error) => Err(error.into()),
    }
}

fn publish(bundle: &PreparedBundle, destination: &Path) -> Result<()> {
    let parent = destination
        .parent()
        .context("output directory has no parent")?;
    let staging = tempfile::tempdir_in(parent)?;
    let mut names = std::collections::BTreeSet::new();
    for artifact in bundle.artifacts() {
        let name = &artifact.filename;
        ensure!(
            !name.is_empty()
                && name != "."
                && name != ".."
                && name != "manifest.json"
                && !name.contains(['/', '\\', ':', '\0'])
                && names.insert(name),
            "invalid or duplicate artifact filename {name:?}"
        );
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(staging.path().join(name))?;
        file.write_all(&artifact.bytes)?;
        file.sync_all()?;
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(staging.path().join("manifest.json"))?;
    serde_json::to_writer_pretty(&mut file, &manifest(bundle))?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    drop(file);
    #[cfg(target_os = "linux")]
    rustix::fs::renameat_with(
        rustix::fs::CWD,
        staging.path(),
        rustix::fs::CWD,
        destination,
        rustix::fs::RenameFlags::NOREPLACE,
    )?;
    #[cfg(not(target_os = "linux"))]
    {
        // A normal rename rejects an occupied nonempty directory. Concurrent
        // replacement of an empty destination is outside the CLI contract.
        output_path(destination)?;
        fs::rename(staging.path(), destination)?;
    }
    // The staging path no longer exists; TempDir cleanup is harmless.
    Ok(())
}

fn main() -> Result<()> {
    let Some(invocation) = parse(std::env::args_os().skip(1))? else {
        print!("{USAGE}");
        return Ok(());
    };
    let destination = output_path(&invocation.output)?;
    let bundle = prepare(
        &invocation.request,
        &invocation.options,
        &Cancellation::default(),
    )?;
    publish(&bundle, &destination)
        .with_context(|| format!("publishing {}", destination.display()))?;
    Ok(())
}
