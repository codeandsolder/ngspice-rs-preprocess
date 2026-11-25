//! Explicit source-only conversion of original logical `SIMetrix` A ALB
//! instances to their physically wired native XSPICE subcircuits.
//! Production `D_LOGIC_BLOCK` translation stays disabled.

use anyhow::{Context, Result, bail, ensure};
use model_translate_simplis::{
    AlbOutputProducer, DigitalPinDirection, LogicalAlbBoundary, expand_logical_alb_source,
};
use serde::Deserialize;
use std::{collections::BTreeSet, env, fs, path::Path};

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
}
#[derive(Deserialize)]
struct Config {
    connections: Vec<Mapping>,
}

fn run() -> Result<()> {
    let args = env::args_os().skip(1).collect::<Vec<_>>();
    if args.len() != 4 {
        bail!("usage: alb-logical-expand SOURCE.cir BOUNDARIES.json OUTPUT.cir COMPILED.inc");
    }
    let source = fs::read_to_string(&args[0]).context("reading original logical ALB deck")?;
    let config: Config = serde_json::from_slice(&fs::read(&args[1])?)
        .context("decoding analog-to-digital boundary assignments")?;
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
    let mut seen = BTreeSet::new();
    let bindings = config
        .connections
        .into_iter()
        .filter_map(|m| {
            let k = (
                m.scope
                    .iter()
                    .map(|s| s.to_ascii_uppercase())
                    .collect::<Vec<_>>(),
                m.analog_net.to_ascii_uppercase(),
                m.digital_net.to_ascii_uppercase(),
                match m.direction {
                    Direction::Input => 0u8,
                    Direction::Output => 1u8,
                },
            );
            let producer = m.producer;
            (m.occurrence.is_none() || seen.insert(k)).then_some(LogicalAlbBoundary {
                scope: m.scope,
                analog_net: m.analog_net,
                digital_net: m.digital_net,
                direction: m.direction.into(),
                producer,
            })
        })
        .collect::<Vec<_>>();
    let expanded = expand_logical_alb_source(&source, &bindings, name)?;
    fs::write(compiled, &expanded.library)
        .context("writing compiled ALB code-model declarations")?;
    fs::write(deck, &expanded.deck).context("writing rewritten original logical source")?;
    Ok(())
}
fn main() -> Result<()> {
    run()
}
