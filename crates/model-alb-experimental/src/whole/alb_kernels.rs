//! Intern native implementation artifacts independently of lexical bindings.

use super::{Item, SourceAlbDefinition};
use crate::alb::{
    OutputResistance, TimingEnvironment, XspiceElectrical, XspiceTimedPlan,
    analyze_body_in_environment, render_stateful_contents,
};
use anyhow::{Context, Result, bail};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

#[derive(Debug)]
pub(super) struct NativeAlbKernel {
    pub(super) name: String,
    pub(super) input_width: usize,
    pub(super) output_width: usize,
    pub(super) contents: Arc<str>,
}

/// Memoize exact compiler inputs before parsing or evaluating a cloned body.
/// Effective floating-point inputs use their complete IEEE binary encoding,
/// not numeric comparison (which would conflate signed zeros and NaNs).
#[derive(Eq, PartialEq, Ord, PartialOrd)]
struct CompilerInput<'a> {
    body: &'a str,
    user: Vec<u64>,
    device: Option<Vec<u64>>,
    timing: [u64; 6],
    electrical: [u64; 8],
}

impl<'a> CompilerInput<'a> {
    fn new(body: &'a str, timing: TimingEnvironment<'_>, electrical: XspiceElectrical) -> Self {
        let TimingEnvironment {
            user,
            device,
            user_scale,
            default_register_delay,
            default_output_delay,
            register_timing_defaults,
        } = timing;
        let XspiceElectrical {
            input_load_f,
            input_sink_current_a,
            input_source_current_a,
            min_sink_a,
            max_source_a,
            stage_out_res_ohm,
            output_out_res_ohm,
        } = electrical;
        Self {
            body,
            user: user.iter().map(|value| value.to_bits()).collect(),
            device: device.map(|values| values.iter().map(|value| value.to_bits()).collect()),
            timing: [
                user_scale,
                default_register_delay,
                default_output_delay,
                register_timing_defaults.setup,
                register_timing_defaults.hold,
                register_timing_defaults.min_clock,
            ]
            .map(f64::to_bits),
            electrical: [
                input_load_f,
                input_sink_current_a,
                input_source_current_a,
                min_sink_a,
                max_source_a,
                stage_out_res_ohm,
                output_out_res_ohm.positive,
                output_out_res_ohm.negative,
            ]
            .map(f64::to_bits),
        }
    }
}

pub(super) struct KernelCompiler<'a> {
    inputs: BTreeMap<CompilerInput<'a>, Arc<NativeAlbKernel>>,
    implementations: BTreeMap<Arc<str>, Arc<NativeAlbKernel>>,
    occupied_names: BTreeSet<String>,
    next_name: usize,
}

fn reserve_names(items: &[Item], names: &mut BTreeSet<String>) {
    for item in items {
        if let Item::Subcircuit(block) = item {
            names.insert(block.name.to_ascii_uppercase());
            reserve_names(&block.body, names);
        }
    }
}

fn compile_contents(
    definition: &SourceAlbDefinition,
    timing: TimingEnvironment<'_>,
    electrical: XspiceElectrical,
) -> Result<(usize, usize, String)> {
    let body = &definition.body;
    let (table, stateful) = analyze_body_in_environment(body, timing)?.into_parts();
    let plan = if let Some(program) = stateful {
        if let Some(graph) = program.timed_combinational_graph(timing)? {
            graph.xspice_timed_plan()?
        } else {
            return Ok((
                program.input_width(),
                program.output_width(),
                render_stateful_contents(&program, body, timing, electrical)?,
            ));
        }
    } else if let Some(table) = table {
        XspiceTimedPlan::from_known_truth_table(&table, timing)?
    } else {
        bail!("ALB model does not contain a compilable logic program");
    };
    Ok((
        plan.input_width(),
        plan.output_width(),
        plan.render_contents(electrical)?,
    ))
}

impl<'a> KernelCompiler<'a> {
    pub(super) fn new(items: &[Item]) -> Self {
        let mut occupied_names = BTreeSet::new();
        reserve_names(items, &mut occupied_names);
        Self {
            inputs: BTreeMap::new(),
            implementations: BTreeMap::new(),
            occupied_names,
            next_name: 0,
        }
    }

    /// Always compare the entire name-independent implementation. This also
    /// merges different input keys that lower to the same actual native
    /// program, without merging their lexical model or DEVICE provenance.
    fn intern(
        &mut self,
        input_width: usize,
        output_width: usize,
        contents: String,
    ) -> Result<Arc<NativeAlbKernel>> {
        if let Some(existing) = self.implementations.get(contents.as_str()) {
            return Ok(Arc::clone(existing));
        }
        let name = loop {
            let candidate = format!("SRALB{}", self.next_name);
            self.next_name = self
                .next_name
                .checked_add(1)
                .context("exhausted native ALB names")?;
            if self.occupied_names.insert(candidate.clone()) {
                break candidate;
            }
        };
        let contents = Arc::<str>::from(contents);
        let kernel = Arc::new(NativeAlbKernel {
            name,
            input_width,
            output_width,
            contents: Arc::clone(&contents),
        });
        self.implementations.insert(contents, Arc::clone(&kernel));
        Ok(kernel)
    }

    pub(super) fn compile(
        &mut self,
        definition: &'a SourceAlbDefinition,
        device: Option<&[f64]>,
    ) -> Result<Arc<NativeAlbKernel>> {
        let semantics = &definition.semantics;
        let timing = semantics.timing_environment(device);
        let electrical = XspiceElectrical {
            input_load_f: semantics.effective_input_load(),
            input_sink_current_a: semantics.sink_current.unwrap_or(0.0),
            input_source_current_a: semantics.source_current.unwrap_or(0.0),
            min_sink_a: semantics.min_sink.unwrap_or(-1.0e-3),
            max_source_a: semantics.max_source.unwrap_or(1.0e-3),
            stage_out_res_ohm: 0.0,
            output_out_res_ohm: OutputResistance {
                positive: semantics.effective_out_res_pos(),
                negative: semantics.effective_out_res_neg(),
            },
        };
        let input = CompilerInput::new(&definition.body, timing, electrical);
        if let Some(existing) = self.inputs.get(&input) {
            return Ok(Arc::clone(existing));
        }
        let (inputs, outputs, contents) = compile_contents(definition, timing, electrical)?;
        let kernel = self.intern(inputs, outputs, contents)?;
        self.inputs.insert(input, Arc::clone(&kernel));
        Ok(kernel)
    }
}
