use anyhow::{Context, Result, ensure};
use model_prepare_osdi::compiler::{Compiler, Target};
use model_preprocess_command::{Cancellation, ProcessLimits};
use std::{collections::BTreeMap, env, fs, path::PathBuf, time::Duration};

fn main() -> Result<()> {
    let mut args = env::args_os().skip(1);
    let usage =
        "usage: prepare <absolute compiler> <absolute scratch> <absolute root.va> <output.osdi>";
    let executable = PathBuf::from(args.next().context(usage)?);
    let scratch = PathBuf::from(args.next().context(usage)?);
    let source = PathBuf::from(args.next().context(usage)?);
    let output = PathBuf::from(args.next().context(usage)?);
    ensure!(args.next().is_none(), "{usage}");
    let compiler = Compiler {
        executable,
        scratch,
        include_directories: Vec::new(),
        defines: BTreeMap::new(),
        target: Target::X86_64,
        cpu: "generic".into(),
        optimization: 3,
        process: ProcessLimits {
            timeout: Duration::from_secs(180),
            ..ProcessLimits::default()
        },
        source_bytes: 64 * 1024 * 1024,
        artifact_bytes: 128 * 1024 * 1024,
    };
    let prepared = compiler.compile(&source, &Cancellation::default())?;
    fs::write(output, prepared.bytes())?;
    println!("{:#?}", prepared.identity());
    println!(
        "{} descriptors, {} expanded bytes, {} module bytes",
        prepared.schema().devices().len(),
        prepared.expanded().len(),
        prepared.bytes().len()
    );
    Ok(())
}
