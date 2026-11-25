use anyhow::{Context, Result};
use std::{env, fs};

fn main() -> Result<()> {
    let path = env::args_os()
        .nth(1)
        .context("usage: inspect <module.osdi>")?;
    let bytes = fs::read(path)?;
    println!("{:#?}", model_prepare_osdi::inspect(&bytes)?);
    Ok(())
}
