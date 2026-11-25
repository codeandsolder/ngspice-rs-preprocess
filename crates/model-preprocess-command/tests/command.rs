//! Exercising the real process protocol, including simultaneous pipe pressure.
#![cfg(unix)]

use anyhow::{Context, Result};
use model_preprocess_command::{
    AdmissionConfig, Cancellation, CommandConfig, ProcessLimits, SourceSession,
};
use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

fn settings(script: &str) -> AdmissionConfig {
    AdmissionConfig {
        command: Some(CommandConfig {
            executable: PathBuf::from("/bin/sh"),
            arguments: vec!["-c".into(), script.into(), "fixture".into()],
            working_directory: None,
        }),
        ..AdmissionConfig::default()
    }
}

fn failure(result: Result<model_preprocess_command::AdmittedSource>) -> Result<String> {
    Ok(format!(
        "{:#}",
        result.err().context("expected admission to fail")?
    ))
}

#[test]
fn literal_arguments_canonical_origin_relative_sidecar_and_cached_snapshot() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("a source; literal.cir");
    fs::write(&path, b"binary\0input\xff")?;
    fs::write(dir.path().join("sidecar"), b"output\0\xff")?;
    let mut config = settings(
        r#"test "$1" = '$(touch unwanted); *' || exit 5
test "$2" = "$(pwd)/a source; literal.cir" || exit 6
printf x >> calls
cat >/dev/null
cat sidecar"#,
    );
    config
        .command
        .as_mut()
        .context("missing command")?
        .arguments
        .push(OsString::from("$(touch unwanted); *"));
    let mut admission = SourceSession::new(config, Cancellation::default());
    let first = admission.load(&path)?;
    assert_eq!(first.path, path.canonicalize()?);
    assert_eq!(&*first.bytes, b"output\0\xff");
    let alias = dir.path().join("alias");
    std::os::unix::fs::symlink(&path, &alias)?;
    fs::write(&path, b"changed after admission")?;
    let second = admission.load(&alias)?;
    assert!(Arc::ptr_eq(&first.bytes, &second.bytes));
    assert_eq!(fs::read(dir.path().join("calls"))?, b"x");
    assert!(!dir.path().join("unwanted").exists());
    let original = SourceSession::default().load(&path)?;
    assert_eq!(&*original.bytes, b"changed after admission");
    Ok(())
}

#[test]
fn large_input_and_both_output_streams_are_drained_concurrently() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("large");
    let bytes = vec![0x9a; 1024 * 1024];
    fs::write(&path, &bytes)?;
    let mut config = settings("head -c 262144 /dev/zero >&2; cat");
    config.process.diagnostic_bytes = 262_144;
    config.process.output_bytes = bytes.len();
    let mut admission = SourceSession::new(config, Cancellation::default());
    assert_eq!(&*admission.load(&path)?.bytes, &bytes);
    Ok(())
}

#[test]
fn exact_independent_limits_and_nonzero_status() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("source");
    fs::write(&path, b"1234")?;
    let mut config = settings("cat; printf err >&2");
    config.input_bytes = 4;
    config.process.output_bytes = 4;
    config.process.diagnostic_bytes = 3;
    let mut admission = SourceSession::new(config.clone(), Cancellation::default());
    assert_eq!(&*admission.load(&path)?.bytes, b"1234");
    for (input, output, diagnostic, expected) in [
        (3, 4, 3, "source input limit"),
        (4, 3, 3, "stdout limit"),
        (4, 4, 2, "stderr limit"),
    ] {
        config.input_bytes = input;
        config.process.output_bytes = output;
        config.process.diagnostic_bytes = diagnostic;
        let error =
            failure(SourceSession::new(config.clone(), Cancellation::default()).load(&path))?;
        assert!(error.contains(expected), "{error}");
    }
    let mut admission = SourceSession::new(
        settings("printf invalid; printf 'failed diagnostic' >&2; exit 7"),
        Cancellation::default(),
    );
    let error = failure(admission.load(&path))?;
    assert!(error.contains("failed diagnostic"), "{error}");
    // Failure cannot become an accepted snapshot.
    assert!(admission.load(&path).is_err());
    Ok(())
}

#[test]
fn deadline_covers_stalled_stdin_inherited_pipes_and_closed_pipes() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("source");
    fs::write(&path, vec![0; 1024 * 1024])?;
    for script in [
        "sleep 30",
        "cat >/dev/null; sleep 30 &",
        "exec 1>&- 2>&-; sleep 30",
    ] {
        let mut config = settings(script);
        config.process.timeout = Duration::from_millis(150);
        let start = Instant::now();
        let error = failure(SourceSession::new(config, Cancellation::default()).load(&path))?;
        assert!(error.contains("deadline"), "{error}");
        assert!(start.elapsed() < Duration::from_secs(3), "{error}");
    }
    Ok(())
}

#[test]
fn cancellation_interrupts_idle_and_continuously_readable_processes() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("source");
    fs::write(&path, b"input")?;
    for script in ["sleep 30", "while :; do printf x; sleep 0.001; done"] {
        let token = Cancellation::default();
        let signal = token.clone();
        let mut config = settings(script);
        config.process = ProcessLimits {
            timeout: Duration::from_secs(30),
            output_bytes: usize::MAX,
            ..ProcessLimits::default()
        };
        let worker = thread::spawn(move || {
            thread::sleep(Duration::from_millis(75));
            signal.cancel();
        });
        let mut admission = SourceSession::new(config, token);
        let start = Instant::now();
        let error = failure(admission.load(&path))?;
        worker
            .join()
            .map_err(|_| anyhow::anyhow!("cancellation worker panicked"))?;
        assert!(error.contains("cancelled"), "{error}");
        assert!(start.elapsed() < Duration::from_secs(3));
        assert!(admission.load(&path).is_err());
    }
    Ok(())
}

#[test]
fn configuration_and_special_file_rejection() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("source");
    fs::write(&path, b"input")?;
    let mut config = settings("cat");
    config
        .command
        .as_mut()
        .context("missing command")?
        .executable = PathBuf::from("cat");
    let error = failure(SourceSession::new(config, Cancellation::default()).load(&path))?;
    assert!(error.contains("absolute"));
    assert!(
        SourceSession::default()
            .load(Path::new("/dev/zero"))
            .is_err()
    );
    Ok(())
}

#[test]
fn failed_admission_and_virtual_origin_are_each_invoked_once() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let source = directory.path().join("virtual.cir");
    let config = settings(r#"printf x >> calls; test -n "$1"; cat; exit 7"#);
    let mut session = SourceSession::new(config, Cancellation::default());
    for _ in 0..2 {
        assert!(
            session
                .admit_bytes(&source, Arc::from(&b"first"[..]))
                .is_err()
        );
    }
    assert_eq!(fs::read(directory.path().join("calls"))?, b"x");
    let mut session = SourceSession::new(settings("cat"), Cancellation::default());
    let first = session.admit_bytes(&source, Arc::from(&b"first"[..]))?;
    let next = session.admit_bytes(&source, Arc::from(&b"second"[..]))?;
    assert_eq!(&*first.bytes, b"first");
    assert!(Arc::ptr_eq(&first.bytes, &next.bytes));
    Ok(())
}
