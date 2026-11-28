//! Explicit standalone compiler for an experimental ALB/XSPICE subcircuit.
//! Not a mode of normal `SIMetrix` model translation.

use anyhow::{Context, Result, bail};
use model_alb_experimental::{
    OutputResistance, TimingEnvironment, XspiceElectrical, compile_alb_xspice_subcircuit,
};
use serde::Deserialize;
use std::{env, fs, path::Path};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    user: Vec<f64>,
    device: Option<Vec<f64>>,
    user_scale: f64,
    default_register_delay: f64,
    default_output_delay: f64,
    input_load_f: f64,
    input_sink_current_a: f64,
    input_source_current_a: f64,
    min_sink_a: f64,
    max_source_a: f64,
    stage_out_res_ohm: f64,
    output_out_res_ohm: OutputResistance,
}

fn main() -> Result<()> {
    let arguments = env::args_os().skip(1).collect::<Vec<_>>();
    if arguments.len() != 4 {
        bail!("usage: alb-xspice-compile BODY.alb SETTINGS.json OUTPUT.inc SUBCKT_NAME");
    }
    let body_path = Path::new(&arguments[0]);
    let config_path = Path::new(&arguments[1]);
    let output_path = Path::new(&arguments[2]);
    let name = arguments[3]
        .to_str()
        .context("subcircuit name must be valid Unicode")?;

    let body = fs::read_to_string(body_path)
        .with_context(|| format!("reading ALB body {}", body_path.display()))?;
    let config: Config = serde_json::from_slice(
        &fs::read(config_path)
            .with_context(|| format!("reading settings {}", config_path.display()))?,
    )
    .context("decoding ALB compiler configuration")?;

    let text = compile_alb_xspice_subcircuit(
        &body,
        name,
        TimingEnvironment {
            user: &config.user,
            device: config.device.as_deref(),
            user_scale: config.user_scale,
            default_register_delay: config.default_register_delay,
            default_output_delay: config.default_output_delay,
            register_timing_defaults: model_alb_experimental::RegisterTimingDefaults::default(),
        },
        XspiceElectrical {
            input_load_f: config.input_load_f,
            input_sink_current_a: config.input_sink_current_a,
            input_source_current_a: config.input_source_current_a,
            min_sink_a: config.min_sink_a,
            max_source_a: config.max_source_a,
            stage_out_res_ohm: config.stage_out_res_ohm,
            output_out_res_ohm: config.output_out_res_ohm,
        },
    )?;
    fs::write(output_path, text).with_context(|| format!("writing {}", output_path.display()))?;
    Ok(())
}
