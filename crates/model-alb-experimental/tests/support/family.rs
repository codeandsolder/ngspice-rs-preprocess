use anyhow::Result;
use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
};

pub fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../runtime/xspice/fixtures/family")
}

pub fn run(
    executable: &str,
    inputs: &[&Path],
    directory: &Path,
    outputs: &[&str],
) -> Result<Output> {
    Ok(Command::new(executable)
        .args(inputs)
        .args(outputs.iter().map(|name| directory.join(name)))
        .output()?)
}
