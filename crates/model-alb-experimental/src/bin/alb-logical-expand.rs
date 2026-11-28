//! Explicit source-only conversion of original logical `SIMetrix` A ALB
//! instances to their physically wired native XSPICE subcircuits.
//! Production `D_LOGIC_BLOCK` translation stays disabled.

use anyhow::{Context, Result, bail};
use model_alb_experimental::{
    annotations::{
        AnnotationDocument, BoundaryAnnotation, compiled_include_name, read_json,
        verify_source_contracts,
    },
    expand_logical_alb_source_with_opaque_x_interfaces,
};
use std::{env, fs, path::Path};

fn run() -> Result<()> {
    let args = env::args_os().skip(1).collect::<Vec<_>>();
    if args.len() != 4 {
        bail!("usage: alb-logical-expand SOURCE.cir BOUNDARIES.json OUTPUT.cir COMPILED.inc");
    }
    let source = fs::read_to_string(&args[0]).context("reading original logical ALB deck")?;
    let config: AnnotationDocument = read_json(Path::new(&args[1]))
        .context("decoding analog-to-digital boundary assignments")?;
    verify_source_contracts(
        Path::new(&args[0]),
        &source,
        &config.opaque_x_interfaces,
        &config.code_model_contracts,
    )?;
    let deck = Path::new(&args[2]);
    let compiled = Path::new(&args[3]);
    let name = compiled_include_name(deck, compiled)?;
    let bindings = config
        .connections
        .into_iter()
        .map(BoundaryAnnotation::into_logical)
        .collect::<Result<Vec<_>>>()?;
    let expanded = expand_logical_alb_source_with_opaque_x_interfaces(
        &source,
        &bindings,
        name,
        &config.opaque_x_interfaces,
    )?;
    fs::write(compiled, &expanded.library)
        .context("writing compiled ALB code-model declarations")?;
    fs::write(deck, &expanded.deck).context("writing rewritten original logical source")?;
    Ok(())
}
fn main() -> Result<()> {
    run()
}
