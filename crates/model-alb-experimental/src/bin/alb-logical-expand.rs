//! Explicit source-only conversion of original logical `SIMetrix` A ALB
//! instances to their physically wired native XSPICE subcircuits.
//! Production `D_LOGIC_BLOCK` translation stays disabled.

use anyhow::{Context, Result, bail, ensure};
use model_alb_experimental::{
    AlbInputReceiver, AlbOutputProducer, CodeModelContract, DigitalPinDirection,
    LogicalAlbBoundary, OpaqueXInterface, expand_logical_alb_source_with_opaque_x_interfaces,
    verify_external_code_models, verify_external_native_libraries,
};
use serde::Deserialize;
use std::{env, fs, path::Path};

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Direction {
    Input,
    Output,
}
impl From<Direction> for DigitalPinDirection {
    fn from(value: Direction) -> Self {
        match value {
            Direction::Input => Self::Input,
            Direction::Output => Self::Output,
        }
    }
}
#[derive(Deserialize)]
struct Mapping {
    #[serde(default)]
    scope: Vec<String>,
    occurrence: Option<Vec<String>>,
    analog_net: String,
    digital_net: String,
    direction: Direction,
    producer: Option<AlbOutputProducer>,
    #[serde(default)]
    receivers: Vec<AlbInputReceiver>,
}
#[derive(Deserialize)]
struct Config {
    connections: Vec<Mapping>,
    #[serde(default)]
    opaque_x_interfaces: Vec<OpaqueXInterface>,
    #[serde(default)]
    code_model_contracts: Vec<CodeModelContract>,
}

fn logical_bindings(connections: Vec<Mapping>) -> Result<Vec<LogicalAlbBoundary>> {
    connections
        .into_iter()
        .map(|mapping| {
            ensure!(
                mapping.occurrence.is_none(),
                "occurrence boundaries require alb-family-compile on original logical source"
            );
            Ok(LogicalAlbBoundary {
                scope: mapping.scope,
                analog_net: mapping.analog_net,
                digital_net: mapping.digital_net,
                direction: mapping.direction.into(),
                producer: mapping.producer,
                receivers: mapping.receivers,
            })
        })
        .collect()
}

fn run() -> Result<()> {
    let args = env::args_os().skip(1).collect::<Vec<_>>();
    if args.len() != 4 {
        bail!("usage: alb-logical-expand SOURCE.cir BOUNDARIES.json OUTPUT.cir COMPILED.inc");
    }
    let source = fs::read_to_string(&args[0]).context("reading original logical ALB deck")?;
    let config: Config = serde_json::from_slice(&fs::read(&args[1])?)
        .context("decoding analog-to-digital boundary assignments")?;
    verify_external_native_libraries(Path::new(&args[0]), &source, &config.opaque_x_interfaces)?;
    verify_external_code_models(
        Path::new(&args[0]),
        &source,
        &config.code_model_contracts,
        &config.opaque_x_interfaces,
    )?;
    let deck = Path::new(&args[2]);
    let compiled = Path::new(&args[3]);
    ensure!(
        deck != compiled && deck.parent() == compiled.parent(),
        "compiled ALB include and lowered source must share a directory"
    );
    let name = compiled
        .file_name()
        .and_then(|s| s.to_str())
        .context("compiled ALB file name is not UTF-8")?;
    let bindings = logical_bindings(config.connections)?;
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
