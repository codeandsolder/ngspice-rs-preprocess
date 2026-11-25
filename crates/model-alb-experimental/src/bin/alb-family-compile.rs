//! Compile original ALB source, specializing occurrences before logical
//! cuts and physical pin triples are assigned.
use anyhow::{Context, Result, bail, ensure};
use model_alb_experimental::{
    expand_logical_alb_source_with_opaque_x_interfaces, specialize_family_occurrences,
};
use std::{env, fs, path::Path};

#[path = "support/family.rs"]
mod family;

fn run() -> Result<()> {
    let args = env::args_os().skip(1).collect::<Vec<_>>();
    let [source, bindings, models, output, compiled] = args.as_slice() else {
        bail!(
            "usage: alb-family-compile SOURCE.cir BOUNDARIES.json NATIVE_MODELS.json OUTPUT.cir COMPILED.inc"
        );
    };
    let source_path = Path::new(source);
    let source = fs::read_to_string(source_path).context("reading original logical ALB source")?;
    let config = family::FamilyConfig::read(Path::new(bindings), Path::new(models))?;
    config.verify(source_path, &source)?;
    let output = Path::new(output);
    let compiled = Path::new(compiled);
    ensure!(
        output != compiled && output.parent() == compiled.parent(),
        "compiled ALB include and final source must be distinct files in the same directory"
    );
    let include_name = compiled
        .file_name()
        .and_then(|name| name.to_str())
        .context("compiled ALB file name is not UTF-8")?;
    let plan = specialize_family_occurrences(&source, &config.occurrences, &config.definitions)?;
    // Resolve contracts in the directory from which the final deck is run,
    // before creating its generated include. The specialized original deck
    // has not yet gained a reference to that include.
    config.verify(output, &plan.deck)?;
    let expanded = expand_logical_alb_source_with_opaque_x_interfaces(
        &plan.deck,
        &plan.logical_boundaries,
        include_name,
        &config.interfaces,
    )?;
    let assembled = config.wire(&expanded.deck, &expanded.library, &plan.connections)?;
    fs::write(compiled, expanded.library).context("writing compiled ALB declarations")?;
    fs::write(output, assembled).context("writing specialized native SPICE circuit")?;
    Ok(())
}

fn main() -> Result<()> {
    run()
}
