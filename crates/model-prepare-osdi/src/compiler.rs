//! Explicit two-stage `OpenVAF` compilation of a frozen expanded source.

use crate::schema::{ModuleSchema, inspect_schema};
use anyhow::{Context, Result, ensure};
use model_preprocess_command::{
    AdmissionConfig, Cancellation, CapturedProcess, ProcessLimits, SourceSession, capture,
};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};

/// ELF targets supported by the compiler and static descriptor reader.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Target {
    X86_64,
    Aarch64,
    Riscv64,
}

impl Target {
    const fn triple(self) -> &'static str {
        match self {
            Self::X86_64 => "x86_64-unknown-linux",
            Self::Aarch64 => "aarch64-unknown-linux",
            Self::Riscv64 => "riscv64-unknown-linux",
        }
    }

    const fn architecture(self) -> object::Architecture {
        match self {
            Self::X86_64 => object::Architecture::X86_64,
            Self::Aarch64 => object::Architecture::Aarch64,
            Self::Riscv64 => object::Architecture::Riscv64,
        }
    }
}

/// The caller explicitly selects a compiler, source search paths and target.
#[derive(Clone, Debug)]
pub struct Compiler {
    pub executable: PathBuf,
    /// Existing directory under which a request-local temporary tree is created.
    pub scratch: PathBuf,
    pub include_directories: Vec<PathBuf>,
    pub defines: BTreeMap<String, Option<String>>,
    pub target: Target,
    /// Explicit compiler CPU selection; "generic" enables portable artifacts.
    pub cpu: String,
    /// Compiler optimization level, 0 through 3.
    pub optimization: u8,
    /// Per-process time and stdout/stderr ceilings.
    pub process: ProcessLimits,
    pub source_bytes: usize,
    pub artifact_bytes: usize,
}

/// Reproducible input identity independent of the original source filename.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompilationIdentity {
    pub root_sha256: [u8; 32],
    pub expanded_sha256: [u8; 32],
    pub compiler_sha256: [u8; 32],
    pub compiler_version: String,
    pub target: Target,
    pub cpu: String,
    pub optimization: u8,
}

/// Captured compiler output for successful stages.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostics {
    pub expansion_stderr: Vec<u8>,
    pub compiler_stdout: Vec<u8>,
    pub compiler_stderr: Vec<u8>,
}

/// Immutable compiler output with static schema and compilation provenance.
#[derive(Clone, Debug)]
pub struct PreparedModule {
    identity: CompilationIdentity,
    schema: ModuleSchema,
    expanded: Arc<[u8]>,
    bytes: Arc<[u8]>,
    diagnostics: Diagnostics,
}

impl PreparedModule {
    /// Reproducible source/compiler provenance.
    #[must_use = "the identity records the exact expanded source and compiler"]
    pub const fn identity(&self) -> &CompilationIdentity {
        &self.identity
    }

    /// Schema bound to the immutable module bytes.
    #[must_use = "the validated schema defines the device ABI"]
    pub const fn schema(&self) -> &ModuleSchema {
        &self.schema
    }

    /// Exact expanded source passed to the final compiler invocation.
    #[must_use = "the expanded bytes reproduce the compiled input"]
    pub fn expanded(&self) -> &[u8] {
        &self.expanded
    }

    /// Exact inspected module, ready for explicit runtime admission.
    #[must_use = "the returned artifact is required for explicit execution"]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Warnings and informational output from the compiler.
    #[must_use = "the diagnostics describe warnings emitted during preparation"]
    pub const fn diagnostics(&self) -> &Diagnostics {
        &self.diagnostics
    }
}

fn absolute_file(path: &Path) -> Result<PathBuf> {
    ensure!(
        path.is_absolute(),
        "compiler and root source paths must be absolute"
    );
    let path = path.canonicalize()?;
    ensure!(
        path.metadata()?.is_file(),
        "not a regular file: {}",
        path.display()
    );
    Ok(path)
}

fn absolute_directory(path: &Path) -> Result<PathBuf> {
    ensure!(path.is_absolute(), "scratch/include path must be absolute");
    let path = path.canonicalize()?;
    ensure!(path.is_dir(), "not a directory: {}", path.display());
    Ok(path)
}

fn snapshot(path: &Path, limit: usize, cancellation: &Cancellation) -> Result<Arc<[u8]>> {
    let config = AdmissionConfig {
        input_bytes: limit,
        ..AdmissionConfig::default()
    };
    Ok(SourceSession::new(config, cancellation.clone())
        .load(path)?
        .bytes)
}

fn file_digest(path: &Path, cancellation: &Cancellation) -> Result<[u8; 32]> {
    let mut file = File::open(path)?;
    ensure!(
        file.metadata()?.is_file(),
        "compiler must be a regular file"
    );
    let mut hash = Sha256::new();
    let mut buffer = [0; 8192];
    loop {
        ensure!(!cancellation.is_cancelled(), "compilation cancelled");
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    Ok(hash.finalize().into())
}

fn valid_define(name: &str) -> bool {
    let mut chars = name.bytes();
    chars
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && chars.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

impl Compiler {
    fn run(
        &self,
        executable: &Path,
        cwd: &Path,
        args: &[OsString],
        cancellation: &Cancellation,
    ) -> Result<CapturedProcess> {
        let result = capture(executable, cwd, args, &[], self.process, cancellation)?;
        ensure!(
            result.status.success(),
            "OpenVAF failed: {}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        Ok(result)
    }

    fn expansion_args(&self, source: &Path) -> Result<Vec<OsString>> {
        let mut args = vec![OsString::from("--print-expansion")];
        for directory in &self.include_directories {
            args.push("-I".into());
            args.push(absolute_directory(directory)?.into_os_string());
        }
        for (name, value) in &self.defines {
            ensure!(valid_define(name), "invalid Verilog-A macro name {name:?}");
            let argument = value
                .as_ref()
                .map_or_else(|| name.clone(), |value| format!("{name}={value}"));
            ensure!(!argument.contains('\0'), "NUL in Verilog-A macro argument");
            args.push("-D".into());
            args.push(argument.into());
        }
        args.push(source.as_os_str().to_owned());
        Ok(args)
    }

    /// Compile an explicitly requested root Verilog-A source.
    ///
    /// The preprocessor resolves includes and defines once; the exact expanded
    /// bytes are compiled from a request-local directory. No ambient source
    /// search paths are added by this API. The module is inspected statically.
    ///
    /// # Errors
    /// Invalid paths/settings, compiler failures, capture or artifact limits,
    /// cancellation, changed source/compiler identity, or invalid OSDI schema.
    pub fn compile(&self, source: &Path, cancellation: &Cancellation) -> Result<PreparedModule> {
        ensure!(
            self.optimization <= 3 && !self.cpu.is_empty(),
            "invalid compiler optimization/CPU"
        );
        let executable = absolute_file(&self.executable)?;
        let source = absolute_file(source)?;
        let directory = source
            .parent()
            .context("source must have a parent directory")?;
        let scratch = absolute_directory(&self.scratch)?;
        let work = tempfile::Builder::new()
            .prefix("osdi-")
            .tempdir_in(scratch)?;
        let compiler_sha256 = file_digest(&executable, cancellation)?;
        let root_sha256: [u8; 32] =
            Sha256::digest(snapshot(&source, self.source_bytes, cancellation)?).into();
        let version = self.run(
            &executable,
            work.path(),
            &["--version".into()],
            cancellation,
        )?;
        let compiler_version = String::from_utf8(version.stdout)?.trim().to_owned();
        ensure!(
            !compiler_version.is_empty(),
            "compiler returned an empty version"
        );
        let expansion = self.run(
            &executable,
            directory,
            &self.expansion_args(&source)?,
            cancellation,
        )?;
        let expanded = expansion.stdout;
        ensure!(
            !expanded.is_empty() && expanded.len() <= self.source_bytes,
            "expanded source exceeds configured budget or is empty"
        );
        let expanded_sha256 = Sha256::digest(&expanded).into();
        let current_root: [u8; 32] =
            Sha256::digest(snapshot(&source, self.source_bytes, cancellation)?).into();
        ensure!(
            root_sha256 == current_root,
            "root Verilog-A source changed during expansion"
        );
        let staged = work.path().join("source.va");
        let output = work.path().join("module.osdi");
        fs::write(&staged, &expanded)?;
        let args = [
            "--target".into(),
            self.target.triple().into(),
            "--target_cpu".into(),
            self.cpu.clone().into(),
            "-O".into(),
            self.optimization.to_string().into(),
            "-o".into(),
            output.as_os_str().to_owned(),
            staged.as_os_str().to_owned(),
        ];
        let compilation = self.run(&executable, work.path(), &args, cancellation)?;
        ensure!(
            compiler_sha256 == file_digest(&executable, cancellation)?,
            "compiler executable changed during compilation"
        );
        let bytes = snapshot(&output, self.artifact_bytes, cancellation)?;
        let schema = inspect_schema(&bytes)?;
        ensure!(
            schema.envelope().architecture == self.target.architecture(),
            "compiler emitted the wrong target architecture"
        );
        Ok(PreparedModule {
            identity: CompilationIdentity {
                root_sha256,
                expanded_sha256,
                compiler_sha256,
                compiler_version,
                target: self.target,
                cpu: self.cpu.clone(),
                optimization: self.optimization,
            },
            schema,
            expanded: Arc::from(expanded),
            bytes,
            diagnostics: Diagnostics {
                expansion_stderr: expansion.stderr,
                compiler_stdout: compilation.stdout,
                compiler_stderr: compilation.stderr,
            },
        })
    }
}
