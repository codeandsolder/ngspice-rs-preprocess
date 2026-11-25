//! Source admission for SPICE frontends. The optional command receives raw
//! bytes on stdin and returns the entire replacement on stdout. No shell
//! evaluation or invocation of a numerical solver is involved.

use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::HashMap,
    ffi::OsString,
    fs::OpenOptions,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use subprocess::{Exec, Job, Redirection};
#[cfg(unix)]
use subprocess::{ExecExt, unix::ProcessExt};

/// Independent ceilings for one external process.
#[derive(Clone, Copy, Debug)]
pub struct ProcessLimits {
    /// Wall-clock deadline for communication and exit.
    pub timeout: Duration,
    /// Maximum captured standard output bytes.
    pub output_bytes: usize,
    /// Maximum captured standard error bytes.
    pub diagnostic_bytes: usize,
}

impl Default for ProcessLimits {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            output_bytes: 128 * 1024 * 1024,
            diagnostic_bytes: 64 * 1024,
        }
    }
}

/// Shared cancellation flag. Cancellation is permanent for this token.
#[derive(Clone, Debug, Default)]
pub struct Cancellation(Arc<AtomicBool>);

impl Cancellation {
    /// Request cancellation of current and subsequent admission operations.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    /// Check whether cancellation was requested.
    #[must_use = "the result indicates whether processing should stop"]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    fn check(&self) -> Result<()> {
        ensure!(!self.is_cancelled(), "source processing cancelled");
        Ok(())
    }
}

/// Literal executable configuration. The caller's arguments are followed by
/// the canonical source pathname as one additional argument.
#[derive(Clone, Debug)]
pub struct CommandConfig {
    /// Absolute executable pathname; no shell or PATH lookup.
    pub executable: PathBuf,
    /// Literal arguments, without interpolation.
    pub arguments: Vec<OsString>,
    /// Absolute working directory, or the original source's containing directory.
    pub working_directory: Option<PathBuf>,
}

/// Source admission settings; the default never launches an external process.
#[derive(Clone, Debug)]
pub struct AdmissionConfig {
    /// Optional external transformation.
    pub command: Option<CommandConfig>,
    /// Maximum bytes read from any original source.
    pub input_bytes: usize,
    /// Per-process time and capture ceilings.
    pub process: ProcessLimits,
}

impl Default for AdmissionConfig {
    fn default() -> Self {
        Self {
            command: None,
            input_bytes: 64 * 1024 * 1024,
            process: ProcessLimits::default(),
        }
    }
}

/// One immutable source snapshot in its original path context.
#[derive(Clone, Debug)]
pub struct AdmittedSource {
    /// Canonical source pathname; also the base for relative includes.
    pub path: PathBuf,
    /// Original or transformed source bytes.
    pub bytes: Arc<[u8]>,
}

/// A source graph's admission cache.
///
/// Paths are canonicalized before admission;
/// repeated references share a snapshot, even across different library sections.
/// Distinct canonical paths remain distinct because relative sidecars may differ.
/// Failed admissions are cached too: retry with a new session, not a new invocation
/// within the same graph.
#[derive(Debug, Default)]
pub struct SourceSession {
    config: AdmissionConfig,
    cancellation: Cancellation,
    sources: HashMap<PathBuf, Result<Arc<[u8]>, String>>,
}

impl SourceSession {
    /// Start an empty source graph with explicit settings and cancellation.
    #[must_use = "the returned session owns admission state and source snapshots"]
    pub fn new(config: AdmissionConfig, cancellation: Cancellation) -> Self {
        Self {
            config,
            cancellation,
            sources: HashMap::new(),
        }
    }

    /// Admit a file once per canonical source identity.
    ///
    /// # Errors
    ///
    /// Missing/nonregular source, input or output ceiling exceeded, cancellation,
    /// invalid process configuration, child failure or timeout.
    pub fn load(&mut self, path: &Path) -> Result<AdmittedSource> {
        self.cancellation.check()?;
        let path = path
            .canonicalize()
            .with_context(|| format!("resolving {}", path.display()))?;
        self.admit(path, Self::read)
    }

    /// Admit caller-owned bytes with an explicit original pathname. A virtual
    /// file need not exist, but its containing directory must exist. The
    /// first admission for a canonical path fixes its snapshot for the session.
    ///
    /// # Errors
    ///
    /// Invalid origin, cancellation, input/output budget or command failure.
    pub fn admit_bytes(&mut self, origin: &Path, bytes: Arc<[u8]>) -> Result<AdmittedSource> {
        self.cancellation.check()?;
        let path = match origin.canonicalize() {
            Ok(path) => {
                ensure!(
                    path.is_file(),
                    "source origin is not a regular file: {}",
                    path.display()
                );
                path
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let name = origin.file_name().context("origin must name a source")?;
                let directory = origin
                    .parent()
                    .filter(|path| !path.as_os_str().is_empty())
                    .unwrap_or_else(|| Path::new("."));
                directory.canonicalize()?.join(name)
            }
            Err(error) => return Err(error.into()),
        };
        self.admit(path, |session, path| {
            ensure!(
                bytes.len() <= session.config.input_bytes,
                "source input limit of {} bytes exceeded: {}",
                session.config.input_bytes,
                path.display()
            );
            Ok(bytes)
        })
    }

    fn admit(
        &mut self,
        path: PathBuf,
        read: impl FnOnce(&Self, &Path) -> Result<Arc<[u8]>>,
    ) -> Result<AdmittedSource> {
        if let Some(snapshot) = self.sources.get(&path) {
            let bytes = snapshot
                .as_ref()
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            return Ok(AdmittedSource {
                path,
                bytes: Arc::clone(bytes),
            });
        }
        match read(self, &path).and_then(|bytes| self.transform(&path, bytes)) {
            Ok(bytes) => {
                self.sources.insert(path.clone(), Ok(Arc::clone(&bytes)));
                Ok(AdmittedSource { path, bytes })
            }
            Err(error) => {
                self.sources.insert(path, Err(format!("{error:#}")));
                Err(error)
            }
        }
    }

    fn transform(&self, path: &Path, bytes: Arc<[u8]>) -> Result<Arc<[u8]>> {
        let bytes = if let Some(config) = &self.config.command {
            let directory = config
                .working_directory
                .as_deref()
                .or_else(|| path.parent())
                .context("source has no containing directory")?;
            ensure!(
                config.executable.is_absolute(),
                "preprocess executable must be absolute"
            );
            ensure!(
                directory.is_absolute(),
                "preprocess working directory must be absolute"
            );
            let mut arguments = config.arguments.clone();
            arguments.push(path.as_os_str().to_owned());
            let result = capture(
                &config.executable,
                directory,
                &arguments,
                bytes,
                self.config.process,
                &self.cancellation,
            )
            .with_context(|| format!("preprocessing {}", path.display()))?;
            ensure!(
                result.status.success(),
                "preprocessing {}: {}: {}",
                path.display(),
                result.status,
                String::from_utf8_lossy(&result.stderr)
            );
            Arc::from(result.stdout)
        } else {
            bytes
        };
        self.cancellation.check()?;
        Ok(bytes)
    }

    fn read(&self, path: &Path) -> Result<Arc<[u8]>> {
        ensure!(
            path.metadata()?.is_file(),
            "source is not a regular file: {}",
            path.display()
        );
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // Avoid blocking if a named file is replaced with a FIFO between
            // the metadata check and open. Regular file I/O is unchanged.
            options.custom_flags(libc::O_NONBLOCK);
        }
        let mut file = options
            .open(path)
            .with_context(|| format!("opening {}", path.display()))?;
        ensure!(
            file.metadata()?.is_file(),
            "source is not a regular file: {}",
            path.display()
        );
        let mut data = Vec::new();
        let mut buffer = [0_u8; 8192];
        loop {
            self.cancellation.check()?;
            let count = self
                .config
                .input_bytes
                .saturating_sub(data.len())
                .saturating_add(1)
                .min(buffer.len());
            let read = match file.read(&mut buffer[..count]) {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                result => result?,
            };
            if read == 0 {
                break;
            }
            ensure!(
                read <= self.config.input_bytes.saturating_sub(data.len()),
                "source input limit of {} bytes exceeded: {}",
                self.config.input_bytes,
                path.display()
            );
            data.extend_from_slice(&buffer[..read]);
        }
        Ok(Arc::from(data))
    }
}

/// Completed process with independently bounded streams.
#[derive(Debug)]
pub struct CapturedProcess {
    /// Exit status.
    pub status: subprocess::ExitStatus,
    /// Raw stdout.
    pub stdout: Vec<u8>,
    /// Raw stderr.
    pub stderr: Vec<u8>,
}

const CHECK_INTERVAL: Duration = Duration::from_millis(50);
const CLEANUP_GRACE: Duration = Duration::from_secs(5);

struct Running {
    job: Job,
    finished: bool,
}

impl Running {
    fn terminate(&mut self) -> Result<()> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        self.job.detach();
        // Unix children run in a new process group. On Windows the child
        // itself is terminated; adapters must not spawn persistent children.
        #[cfg(unix)]
        let group_result = self
            .job
            .processes
            .first()
            .context("missing child")?
            .send_signal_group(libc::SIGKILL);
        let kill_result = self.job.kill();
        let waited = self.job.wait_timeout(CLEANUP_GRACE)?;
        ensure!(waited.is_some(), "child did not exit within cleanup grace");
        kill_result?;
        #[cfg(unix)]
        group_result?;
        Ok(())
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Err(error) = self.terminate() {
            eprintln!("preprocess child cleanup failed: {error:#}");
        }
    }
}

struct BoundedStream<'a> {
    bytes: &'a mut Vec<u8>,
    limit: usize,
    label: &'static str,
    deadline: Instant,
    cancellation: &'a Cancellation,
}

impl Write for BoundedStream<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.cancellation.is_cancelled() {
            return Err(io::Error::other("source processing cancelled"));
        }
        if Instant::now() >= self.deadline {
            return Err(io::Error::other("process deadline elapsed"));
        }
        let count = bytes.len().min(self.limit.saturating_sub(self.bytes.len()));
        self.bytes.extend_from_slice(&bytes[..count]);
        if count != bytes.len() {
            return Err(io::Error::other(format!(
                "{} limit of {} bytes exceeded",
                self.label, self.limit
            )));
        }
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Execute a direct child with bounded concurrent I/O.
///
/// Concurrently supplying stdin and capturing stdout
/// and stderr. This low-level helper also supports simulator diagnostics;
/// only the source command configuration requires an absolute executable.
///
/// The deadline includes process exit after pipes close. Cancellation is
/// checked during data exchange and every 50 ms while waiting for I/O/exit.
/// On failure, terminate and reap the child (with a bounded cleanup grace).
///
/// # Errors
///
/// Invalid limits, launch failure, I/O or capture budget failure, cancellation,
/// timeout, or failure to terminate/reap.
pub fn capture(
    program: &Path,
    directory: &Path,
    arguments: &[OsString],
    input: Arc<[u8]>,
    limits: ProcessLimits,
    cancellation: &Cancellation,
) -> Result<CapturedProcess> {
    cancellation.check()?;
    ensure!(!limits.timeout.is_zero(), "process timeout must be nonzero");
    let deadline = Instant::now()
        .checked_add(limits.timeout)
        .context("process timeout is unrepresentable")?;
    let exec = Exec::cmd(program)
        .args(arguments)
        .cwd(directory)
        .stdin(subprocess::InputData::from_bytes(input))
        .stdout(Redirection::Pipe)
        .stderr(Redirection::Pipe);
    #[cfg(unix)]
    let exec = exec.setpgid();
    let job = exec
        .start()
        .with_context(|| format!("starting {}", program.display()))?;
    let mut child = Running {
        job,
        finished: false,
    };
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let result = collect(
        &mut child.job,
        deadline,
        limits,
        cancellation,
        &mut stdout,
        &mut stderr,
    );
    match result {
        Ok(status) => {
            child.finished = true;
            Ok(CapturedProcess {
                status,
                stdout,
                stderr,
            })
        }
        Err(error) => {
            let cleanup = child.terminate();
            let diagnostic = String::from_utf8_lossy(&stderr);
            if let Err(cleanup) = cleanup {
                bail!("{error:#}; child cleanup failed: {cleanup:#}; stderr: {diagnostic}");
            }
            bail!("{error:#}; stderr: {diagnostic}")
        }
    }
}

fn remaining(deadline: Instant, cancellation: &Cancellation) -> Result<Duration> {
    cancellation.check()?;
    let duration = deadline.saturating_duration_since(Instant::now());
    ensure!(!duration.is_zero(), "process deadline elapsed");
    Ok(duration.min(CHECK_INTERVAL))
}

fn collect(
    job: &mut Job,
    deadline: Instant,
    limits: ProcessLimits,
    cancellation: &Cancellation,
    stdout: &mut Vec<u8>,
    stderr: &mut Vec<u8>,
) -> Result<subprocess::ExitStatus> {
    let mut communication = job.communicate()?;
    loop {
        communication = communication.limit_time(remaining(deadline, cancellation)?);
        match communication.read_to(
            BoundedStream {
                bytes: stdout,
                limit: limits.output_bytes,
                label: "stdout",
                deadline,
                cancellation,
            },
            BoundedStream {
                bytes: stderr,
                limit: limits.diagnostic_bytes,
                label: "stderr",
                deadline,
                cancellation,
            },
        ) {
            Ok(()) => break,
            Err(error) if error.kind() == io::ErrorKind::TimedOut => (),
            Err(error) => return Err(error.into()),
        }
    }
    drop(communication);
    loop {
        if let Some(status) = job.wait_timeout(remaining(deadline, cancellation)?)? {
            return Ok(status);
        }
    }
}
