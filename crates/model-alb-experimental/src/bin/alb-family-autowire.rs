//! Derive physical ALB bundle wiring directly from hierarchical SPICE
//! instance connectivity, then insert explicit native family bridges.
//! Production `D_LOGIC_BLOCK` translation is not enabled.

use anyhow::{Context, Result, bail, ensure};
use model_alb_experimental::ScopedAnalogDigitalConnection;
use std::{env, fs, path::Path};

#[path = "support/family.rs"]
mod family;

fn run() -> Result<()> {
    let args = env::args_os().skip(1).collect::<Vec<_>>();
    let [source, compiled, bindings, models, output] = args.as_slice() else {
        bail!(
            "usage: alb-family-autowire SOURCE.cir COMPILED_ALB.inc BOUNDARIES.json NATIVE_MODELS.json OUTPUT.cir"
        );
    };
    let source_path = Path::new(source);
    let source = fs::read_to_string(source_path).context("reading complete source SPICE deck")?;
    let compiled = fs::read_to_string(compiled).context("reading compiled ALB declarations")?;
    let config = family::FamilyConfig::read(Path::new(bindings), Path::new(models))?;
    ensure!(
        config.occurrences.is_empty(),
        "occurrence boundaries require alb-family-compile on original logical source"
    );
    config.verify(source_path, &source)?;
    let connections = config
        .definitions
        .iter()
        .map(|entry| ScopedAnalogDigitalConnection {
            scope: entry.scope.clone(),
            boundary: entry.boundary.clone(),
        })
        .collect::<Vec<_>>();
    let assembled = config.wire(&source, &compiled, &connections)?;
    fs::write(output, assembled).context("writing complete root native SPICE deck")?;
    Ok(())
}

fn main() -> Result<()> {
    run()
}
