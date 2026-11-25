use anyhow::{Context, Result};
use model_prepare_osdi::schema::{NodeBinding, ParameterKind, inspect_schema};
use std::{env, fs};

fn main() -> Result<()> {
    let path = env::args_os()
        .nth(1)
        .context("usage: inspect <module.osdi>")?;
    let bytes = fs::read(path)?;
    let schema = inspect_schema(&bytes)?;
    println!("{:#?}", schema.envelope());
    for device in schema.devices() {
        let terminals = device
            .nodes
            .iter()
            .filter(|node| node.binding == NodeBinding::Terminal)
            .count();
        let instances = device
            .parameters
            .iter()
            .filter(|param| param.kind == ParameterKind::Instance)
            .count();
        let models = device
            .parameters
            .iter()
            .filter(|param| param.kind == ParameterKind::Model)
            .count();
        println!(
            "{}: {} terminals, {} unknowns, {} model parameters, {} instance parameters, {} operating point variables, {} Jacobian entries, {} noise sources, charge={}",
            device.name,
            terminals,
            device.nodes.len(),
            models,
            instances,
            device.parameters.len() - models - instances,
            device.jacobian.len(),
            device.noise_sources.len(),
            device.charge
        );
    }
    Ok(())
}
