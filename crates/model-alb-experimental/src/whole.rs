//! Explicit source-model inspection and experimental compilation.
mod alb_inline;
mod alb_kernels;
mod logical_ports;
use crate::alb::{
    KnownTruthTable, RegisterTimingDefaults, StatefulProgram, TimedCombinationalGraph,
    TimingEnvironment,
};
use anyhow::{Context, Result, bail, ensure};
pub use logical_ports::{
    AlbInputReceiver, AlbOutputProducer, CodeModelContract, DefinitionFamilyBoundary,
    DiscoveredAlbBoundary, FamilySpecialization, LogicalAlbBoundary, LogicalAlbExpansion,
    LookupTableShape, OccurrenceFamilyBoundary, OpaqueXInterface, OpaqueXPinKind,
    discover_original_alb_boundaries, discover_original_alb_boundaries_with_opaque_x_interfaces,
    expand_logical_alb_source, expand_logical_alb_source_with_opaque_x_interfaces,
    specialize_family_occurrences, verify_external_code_models, verify_external_native_libraries,
};
use model_normalize_ngspice::model_card::{FieldSpan, ModelTermination, statement_field_spans};
use model_translate_simplis::syntax::{
    Item, Subcircuit, electrical_statements, embedded_files, file_boundary, logical_statements,
    model_header, normalized_statement, statement_tokens,
};
use std::fmt::Write as _;

fn parse_items(source: &str) -> Result<Vec<Item>> {
    fn canonicalize(items: &mut [Item]) -> Result<()> {
        let mut embedded = false;
        for item in items {
            match item {
                Item::Statement(statement) => {
                    if let Some(start) = file_boundary(statement) {
                        embedded = start;
                    } else if !embedded {
                        *statement = alb_inline::canonicalize(std::mem::take(statement))?;
                    }
                }
                Item::Subcircuit(block) => canonicalize(&mut block.body)?,
            }
        }
        Ok(())
    }
    let mut items = model_translate_simplis::syntax::parse_items(source)?;
    canonicalize(&mut items)?;
    Ok(items)
}

fn embedded_alb_files(items: &[Item]) -> Result<std::collections::BTreeMap<String, String>> {
    Ok(embedded_files(items)?
        .into_iter()
        .map(|(name, lines)| (name, lines.join("\n")))
        .collect())
}

struct AlbFields<'a> {
    statement: &'a str,
    fields: Vec<FieldSpan>,
}

impl<'a> AlbFields<'a> {
    fn parse(statement: &'a str, termination: ModelTermination) -> Result<Self> {
        Ok(Self {
            statement,
            fields: statement_field_spans(statement, &["FILE", "USER"], termination)?,
        })
    }

    fn field(&self, name: &str) -> Option<&FieldSpan> {
        self.fields.iter().find(|field| {
            self.statement[field.name.clone()].eq_ignore_ascii_case(name) && field.value.is_some()
        })
    }

    fn value(&self, name: &str) -> Option<&'a str> {
        self.field(name)?
            .value
            .as_ref()
            .map(|span| &self.statement[span.clone()])
    }
}

#[derive(Clone, Debug, Default)]
struct AlbModelSemantics {
    input_load: Option<f64>,
    out_res_pos: Option<f64>,
    out_res_neg: Option<f64>,
    out_delay: Option<f64>,
    reg_delay: Option<f64>,
    setup_time: Option<f64>,
    hold_time: Option<f64>,
    min_clock: Option<f64>,
    user: Vec<f64>,
    user_scale: Option<f64>,
    family: Option<String>,
    in_family: Option<String>,
    out_family: Option<String>,
    min_sink: Option<f64>,
    max_source: Option<f64>,
    sink_current: Option<f64>,
    source_current: Option<f64>,
    supply_v: Option<f64>,
}

impl AlbModelSemantics {
    fn parsed_features(&self) -> Vec<&'static str> {
        let mut features = Vec::new();
        if self.input_load.is_some() {
            features.push("input_load");
        }
        if self.out_res_pos.is_some() || self.out_res_neg.is_some() {
            features.push("output_resistance");
        }
        if self.out_delay.is_some() {
            features.push("out_delay");
        }
        if self.reg_delay.is_some() {
            features.push("reg_delay");
        }
        if self.setup_time.is_some() {
            features.push("setup_time");
        }
        if self.hold_time.is_some() {
            features.push("hold_time");
        }
        if self.min_clock.is_some() {
            features.push("min_clock");
        }
        if !self.user.is_empty() {
            features.push("user_vector");
        }
        if self.user_scale.is_some() {
            features.push("user_scale");
        }
        if self.family.is_some() {
            features.push("family");
        }
        if self.in_family.is_some() {
            features.push("in_family");
        }
        if self.out_family.is_some() {
            features.push("out_family");
        }
        if self.min_sink.is_some() {
            features.push("min_sink");
        }
        if self.max_source.is_some() {
            features.push("max_source");
        }
        if self.sink_current.is_some() {
            features.push("sink_current");
        }
        if self.source_current.is_some() {
            features.push("source_current");
        }
        features
    }

    fn effective_input_load(&self) -> f64 {
        self.input_load.unwrap_or(1.0e-12)
    }

    fn effective_out_res_pos(&self) -> f64 {
        self.out_res_pos.unwrap_or(100.0)
    }

    fn effective_out_res_neg(&self) -> f64 {
        self.out_res_neg.unwrap_or(100.0)
    }

    fn effective_out_delay(&self) -> f64 {
        self.out_delay.unwrap_or(1.0e-9)
    }

    fn effective_reg_delay(&self) -> f64 {
        self.reg_delay.unwrap_or(1.0e-9)
    }

    fn effective_setup_time(&self) -> f64 {
        self.setup_time.unwrap_or(0.0)
    }

    fn effective_hold_time(&self) -> f64 {
        self.hold_time.unwrap_or(0.0)
    }

    fn effective_min_clock(&self) -> f64 {
        self.min_clock.unwrap_or(0.0)
    }

    fn effective_user_scale(&self) -> f64 {
        self.user_scale.unwrap_or(1.0)
    }

    fn timing_environment<'a>(&'a self, device: Option<&'a [f64]>) -> TimingEnvironment<'a> {
        TimingEnvironment {
            user: &self.user,
            device,
            user_scale: self.effective_user_scale(),
            default_register_delay: self.effective_reg_delay(),
            default_output_delay: self.effective_out_delay(),
            register_timing_defaults: RegisterTimingDefaults {
                setup: self.effective_setup_time(),
                hold: self.effective_hold_time(),
                min_clock: self.effective_min_clock(),
            },
        }
    }

    fn effective_families(&self) -> (&str, &str) {
        let family = self.family.as_deref().unwrap_or("UNIV");
        (
            self.in_family.as_deref().unwrap_or(family),
            self.out_family.as_deref().unwrap_or(family),
        )
    }

    fn exact_lowering_blockers(&self, table: Option<&KnownTruthTable>) -> Vec<&'static str> {
        let mut blockers = Vec::new();
        if table.is_none() {
            blockers.push("stateful/delayed ALB body");
            blockers.push("state/timing parameter environment");
        }
        if self.effective_out_res_pos() != 0.0 || self.effective_out_res_neg() != 0.0 {
            blockers.push("load-dependent output delay");
        }
        if self.min_sink.is_some()
            || self.max_source.is_some()
            || self.sink_current.is_some_and(|value| value != 0.0)
            || self.source_current.is_some_and(|value| value != 0.0)
        {
            blockers.push("output strength/fanout semantics");
        }
        let _ = self.effective_families();
        blockers.push("digital family bridge semantics");
        if table.is_some() {
            blockers.push("unproven UNKNOWN-state expression semantics");
        }
        blockers
    }
}

#[derive(Clone, Debug)]
struct AlbModel {
    table: Option<KnownTruthTable>,
    stateful: Option<StatefulProgram>,
    semantics: AlbModelSemantics,
}

fn optional_numeric_assignment(fields: &AlbFields<'_>, name: &str) -> Result<Option<f64>> {
    fields
        .value(name)
        .map(crate::parse_numeric)
        .transpose()
        .with_context(|| format!("parsing SIMetrix ALB model parameter {name}"))
}

fn numeric_vector_assignment(fields: &AlbFields<'_>, name: &str) -> Result<Vec<f64>> {
    let Some(value) = fields.value(name) else {
        return Ok(Vec::new());
    };
    let value = value
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .with_context(|| format!("SIMetrix model parameter {name} requires a bracketed vector"))?;
    let mut output = Vec::new();
    for group in value.split(',') {
        let mut found = false;
        for item in group.split_ascii_whitespace() {
            found = true;
            output.push(
                crate::parse_numeric(item)
                    .with_context(|| format!("parsing SIMetrix ALB model vector {name}"))?,
            );
        }
        if !found {
            bail!("empty value in SIMetrix ALB model vector {name}");
        }
    }
    Ok(output)
}

fn optional_family_assignment(fields: &AlbFields<'_>, name: &str) -> Result<Option<String>> {
    fields
        .value(name)
        .map(|value| {
            let quote = value
                .chars()
                .next()
                .filter(|character| matches!(character, '\'' | '"'))
                .with_context(|| format!("SIMetrix model parameter {name} must be quoted"))?;
            let text = value
                .strip_prefix(quote)
                .and_then(|tail| tail.strip_suffix(quote))
                .with_context(|| format!("SIMetrix model parameter {name} must be quoted"))?;
            Ok(text.to_ascii_uppercase())
        })
        .transpose()
}

fn alb_model_semantics(fields: &AlbFields<'_>) -> Result<AlbModelSemantics> {
    let out_res = optional_numeric_assignment(fields, "OUT_RES")?;
    let semantics = AlbModelSemantics {
        input_load: optional_numeric_assignment(fields, "INPUT_LOAD")?,
        out_res_pos: optional_numeric_assignment(fields, "OUT_RES_POS")?.or(out_res),
        out_res_neg: optional_numeric_assignment(fields, "OUT_RES_NEG")?.or(out_res),
        out_delay: optional_numeric_assignment(fields, "OUT_DELAY")?,
        reg_delay: optional_numeric_assignment(fields, "REG_DELAY")?,
        setup_time: optional_numeric_assignment(fields, "SETUP_TIME")?,
        hold_time: optional_numeric_assignment(fields, "HOLD_TIME")?,
        min_clock: optional_numeric_assignment(fields, "MIN_CLOCK")?,
        user: numeric_vector_assignment(fields, "USER")?,
        user_scale: optional_numeric_assignment(fields, "USER_SCALE")?,
        family: optional_family_assignment(fields, "FAMILY")?,
        in_family: optional_family_assignment(fields, "IN_FAMILY")?,
        out_family: optional_family_assignment(fields, "OUT_FAMILY")?,
        min_sink: optional_numeric_assignment(fields, "MIN_SINK")?,
        max_source: optional_numeric_assignment(fields, "MAX_SOURCE")?,
        sink_current: optional_numeric_assignment(fields, "SINK_CURRENT")?,
        source_current: optional_numeric_assignment(fields, "SOURCE_CURRENT")?,
        supply_v: optional_numeric_assignment(fields, "VSUPPLY")?,
    };
    for (name, value) in [
        ("REG_DELAY", semantics.effective_reg_delay()),
        ("SETUP_TIME", semantics.effective_setup_time()),
        ("HOLD_TIME", semantics.effective_hold_time()),
        ("MIN_CLOCK", semantics.effective_min_clock()),
        ("USER_SCALE", semantics.effective_user_scale()),
        ("OUT_DELAY", semantics.effective_out_delay()),
        ("INPUT_LOAD", semantics.effective_input_load()),
        ("OUT_RES_POS", semantics.effective_out_res_pos()),
        ("OUT_RES_NEG", semantics.effective_out_res_neg()),
    ] {
        if !value.is_finite() {
            bail!("invalid SIMetrix ALB model parameter {name}={value}");
        }
    }
    if semantics
        .supply_v
        .is_some_and(|value| !value.is_finite() || value <= 0.0)
    {
        bail!("invalid SIMetrix ALB model parameter VSUPPLY");
    }
    Ok(semantics)
}

fn alb_model_definition(
    statement: &str,
    files: &std::collections::BTreeMap<String, String>,
) -> Result<Option<(String, String, AlbModelSemantics)>> {
    let Some((name, kind)) = model_header(statement) else {
        return Ok(None);
    };
    if kind != "D_LOGIC_BLOCK" {
        return Ok(None);
    }
    let normalized = normalized_statement(statement);
    let inline = alb_inline::definition(&normalized)?;
    let parameters = inline.as_ref().map_or(normalized.as_str(), |definition| {
        &normalized[..definition.parameter_end]
    });
    let termination = if inline.is_some() {
        ModelTermination::DialectPrefix
    } else {
        ModelTermination::Complete
    };
    let fields = AlbFields::parse(parameters, termination)?;
    // FILE before DEF must resolve even though the inline body takes
    // precedence. A FILE after DEF is outside the effective parameter list.
    let file_body = fields
        .value("FILE")
        .map(|file| {
            let file = file.trim_matches(['\'', '"']).to_ascii_uppercase();
            let exact = files.get(&file);
            let legacy_prefixed = (exact.is_none()
                && fields
                    .field("FILE")
                    .is_some_and(|field| !field.explicit_equals)
                && !file.starts_with('_'))
            .then(|| format!("_{file}"))
            .and_then(|candidate| files.get(&candidate));
            exact.or(legacy_prefixed).with_context(|| {
                format!(
                    "SIMetrix D_LOGIC_BLOCK model {name} references missing embedded file {file}"
                )
            })
        })
        .transpose()?;
    let body = if let Some(definition) = inline {
        if let Some(file_body) = file_body {
            crate::alb::validate_referenced_body(file_body)
                .with_context(|| format!("validating leading FILE before inline DEF for {name}"))?;
        }
        let mut body = normalized[definition.body].to_owned();
        // The vendor accepts an inline single expression without a
        // terminator, unlike a FILE body or a multi-statement DEF.
        if !body.contains(';') && !body.trim().is_empty() {
            body.push(';');
        }
        body
    } else {
        file_body.cloned().with_context(|| {
            format!("SIMetrix D_LOGIC_BLOCK model {name} has neither FILE nor DEF")
        })?
    };
    let semantics = alb_model_semantics(&fields)
        .with_context(|| format!("parsing SIMetrix ALB model semantics for {name}"))?;
    Ok(Some((name, body, semantics)))
}

fn alb_model(
    statement: &str,
    files: &std::collections::BTreeMap<String, String>,
) -> Result<Option<(String, AlbModel)>> {
    let Some((name, body, semantics)) = alb_model_definition(statement, files)? else {
        return Ok(None);
    };
    let environment = semantics.timing_environment(None);
    let (table, stateful) = crate::alb::analyze_body_in_environment(&body, environment)
        .with_context(|| format!("analyzing SIMetrix ALB body for model {name}"))?
        .into_parts();
    Ok(Some((
        name,
        AlbModel {
            table,
            stateful,
            semantics,
        },
    )))
}

/// Lexical model binding. Multiple bindings may refer to an identical
/// immutable native kernel; each X instance still owns its simulator state.
#[derive(Clone, Debug)]
struct CompiledSourceAlbModel {
    scope: Vec<String>,
    model_name: String,
    kernel: std::sync::Arc<alb_kernels::NativeAlbKernel>,
    in_family: String,
    out_family: String,
    supply_v: Option<f64>,
    device_values: Option<Vec<f64>>,
}

#[derive(Clone, Debug)]
struct SourceAlbDefinition {
    scope: Vec<String>,
    name: String,
    body: String,
    semantics: AlbModelSemantics,
}

/// Extract the lexical model identity and per-instance USER vector from an
/// original logical A device. The colon is a parameter separator, not part
/// of the model name. The vector is provided by the instance and maps to
/// ALB DEVICE[n], not to the model's USER[n].
fn original_alb_instance_signature(statement: &str) -> Result<Option<(String, Option<Vec<f64>>)>> {
    let normalized = normalized_statement(statement);
    let Some(first) = normalized.split_ascii_whitespace().next() else {
        return Ok(None);
    };
    if first.len() < 2 || !first.starts_with(['A', 'a']) {
        return Ok(None);
    }
    let raw = normalized[first.len()..].trim();
    let mut depth = 0usize;
    let mut separator = None;
    for (index, byte) in raw.bytes().enumerate() {
        match byte {
            b'[' => depth += 1,
            b']' => depth = depth.saturating_sub(1),
            b':' if depth == 0 => {
                separator = Some(index);
                break;
            }
            _ => {}
        }
    }
    let (signature, tail) = separator.map_or((raw, None), |index| {
        (raw[..index].trim(), Some(raw[index + 1..].trim()))
    });
    let model = signature
        .split_ascii_whitespace()
        .last()
        .context("original A device has no model reference")?;
    let model = model
        .strip_prefix("MODEL=")
        .or_else(|| model.strip_prefix("model="))
        .unwrap_or(model);
    let device = if let Some(tail) = tail {
        let fields = AlbFields::parse(tail, ModelTermination::Complete)?;
        ensure!(
            fields.value("USER").is_some(),
            "unsupported original A-device parameters {tail}"
        );
        ensure!(
            fields.fields.len() == 1,
            "unsupported original A-device trailing parameters {tail}"
        );
        Some(numeric_vector_assignment(&fields, "USER")?)
    } else {
        None
    };
    Ok(Some((model.to_ascii_uppercase(), device)))
}

fn same_device_values(first: Option<&[f64]>, second: Option<&[f64]>) -> bool {
    match (first, second) {
        (None, None) => true,
        (Some(first), Some(second)) => {
            first.len() == second.len()
                && first
                    .iter()
                    .zip(second)
                    .all(|(left, right)| left.to_bits() == right.to_bits())
        }
        _ => false,
    }
}

fn collect_alb_model_definitions(
    items: &[Item],
    scope: &[String],
    definitions: &mut Vec<SourceAlbDefinition>,
) -> Result<()> {
    let files = embedded_alb_files(items)?;
    let mut names = std::collections::BTreeSet::new();
    for statement in electrical_statements(items) {
        if let Some((name, body, semantics)) = alb_model_definition(statement, &files)? {
            if !names.insert(name.clone()) {
                bail!("duplicate ALB model {name} in scope {scope:?}");
            }
            definitions.push(SourceAlbDefinition {
                scope: scope.to_vec(),
                name,
                body,
                semantics,
            });
        }
    }
    for item in items {
        if let Item::Subcircuit(block) = item {
            let mut next = scope.to_vec();
            next.push(block.name.to_ascii_uppercase());
            collect_alb_model_definitions(&block.body, &next, definitions)?;
        }
    }
    Ok(())
}

/// Walk original A occurrences by lexical scope, not by subcircuit
/// instantiation. Reusable X definitions compile once per distinct DEVICE
/// vector; the parent X copies retain their own individual wire bundles.
fn collect_alb_instance_variants(
    items: &[Item],
    scope: &[String],
    definitions: &[SourceAlbDefinition],
    used: &mut std::collections::BTreeMap<usize, Vec<Option<Vec<f64>>>>,
) -> Result<()> {
    let mut embedded = false;
    for item in items {
        match item {
            Item::Statement(statement) => {
                if let Some(start) = file_boundary(statement) {
                    embedded = start;
                    continue;
                }
                if embedded {
                    continue;
                }
                let Some((name, device)) = original_alb_instance_signature(statement)? else {
                    continue;
                };
                let owner = definitions
                    .iter()
                    .enumerate()
                    .filter(|(_, definition)| {
                        definition.name == name && scope.starts_with(&definition.scope)
                    })
                    .max_by_key(|(_, definition)| definition.scope.len())
                    .map(|(index, _)| index);
                if let Some(owner) = owner {
                    let variants = used.entry(owner).or_default();
                    if !variants
                        .iter()
                        .any(|existing| same_device_values(existing.as_deref(), device.as_deref()))
                    {
                        variants.push(device);
                    }
                }
            }
            Item::Subcircuit(block) => {
                let mut child_scope = scope.to_vec();
                child_scope.push(block.name.to_ascii_uppercase());
                collect_alb_instance_variants(&block.body, &child_scope, definitions, used)?;
            }
        }
    }
    Ok(())
}

/// Compile original embedded `D_LOGIC_BLOCK` definitions (including `.file`
/// bodies) using the existing ALB parser and experimental XSPICE compiler.
///
/// # Errors
/// Missing files, unsupported ALB semantics or native physical parameters.
fn compile_explicit_alb_source_models(source: &str) -> Result<Vec<CompiledSourceAlbModel>> {
    let items = parse_items(source)?;
    let mut definitions = Vec::new();
    collect_alb_model_definitions(&items, &[], &mut definitions)?;
    let mut used = std::collections::BTreeMap::new();
    collect_alb_instance_variants(&items, &[], &definitions, &mut used)?;
    let mut compiled = Vec::new();
    let mut kernels = alb_kernels::KernelCompiler::new(&items);
    for (index, definition) in definitions.iter().enumerate() {
        let instances = used.remove(&index).unwrap_or_else(|| vec![None]);
        for device in instances {
            let kernel = kernels
                .compile(definition, device.as_deref())
                .with_context(|| {
                    format!(
                        "compiling ALB {} in scope {:?} for DEVICE={device:?}",
                        definition.name, definition.scope
                    )
                })?;
            let (in_family, out_family) = definition.semantics.effective_families();
            compiled.push(CompiledSourceAlbModel {
                scope: definition.scope.clone(),
                model_name: definition.name.clone(),
                kernel,
                in_family: in_family.to_owned(),
                out_family: out_family.to_owned(),
                supply_v: definition.semantics.supply_v,
                device_values: device,
            });
        }
    }
    Ok(compiled)
}

fn resolved_comb_timing_summary(model: &AlbModel) -> Result<Option<String>> {
    let Some(program) = model.stateful.as_ref() else {
        return Ok(None);
    };
    let environment = model.semantics.timing_environment(None);
    let Some(timing) = program.resolve_combinational_timing(environment)? else {
        return Ok(None);
    };
    let delay_range = |values: &std::collections::BTreeMap<String, f64>| {
        values
            .values()
            .copied()
            .fold(None, |range: Option<(f64, f64)>, value| {
                Some(match range {
                    None => (value, value),
                    Some((minimum, maximum)) => (minimum.min(value), maximum.max(value)),
                })
            })
    };
    let register_range = delay_range(timing.register_delays());
    let output_range = delay_range(timing.output_delays());
    let register_count = timing.register_delays().len();
    let output_count = timing.output_delays().len();
    let graph = program.timed_combinational_graph(environment)?;
    let graph_summary = graph
        .as_ref()
        .map(TimedCombinationalGraph::structural_summary);
    let lut_summary = graph
        .as_ref()
        .map(TimedCombinationalGraph::known_state_luts)
        .transpose()?
        .map(|luts| luts.structural_summary());
    let suffix = [graph_summary, lut_summary]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("; ");
    Ok(Some(if suffix.is_empty() {
        format!(
            "resolved COMB timing: {register_count} registers {register_range:?}, {output_count} outputs {output_range:?}",
        )
    } else {
        format!(
            "resolved COMB timing: {register_count} registers {register_range:?}, {output_count} outputs {output_range:?}; {suffix}",
        )
    }))
}

fn describe_model(name: &str, model: &AlbModel) -> Result<String> {
    let blockers = model
        .semantics
        .exact_lowering_blockers(model.table.as_ref());
    let parsed_features = model.semantics.parsed_features();
    if let Some(table) = model.table.as_ref() {
        let lut_values = table.d_genlut_values();
        return Ok(format!(
            "SIMetrix D_LOGIC_BLOCK model {name} has a compiled {}-input/{}-output known-state ALB truth table ({} d_genlut values, {} delayed output bits; parsed semantics: {}), but exact lowering is blocked by: {}",
            table.input_width(),
            table.output_width(),
            lut_values.len(),
            table.delayed_output_bits(),
            parsed_features.join(", "),
            blockers.join(", ")
        ));
    }
    let mut structure = model.stateful.as_ref().map_or_else(
        || "unclassified dynamic body".to_owned(),
        StatefulProgram::structural_summary,
    );
    if let Some(timing) = resolved_comb_timing_summary(model)? {
        write!(structure, "; {timing}")?;
    }
    Ok(format!(
        "SIMetrix D_LOGIC_BLOCK model {name} requires explicit ALB lowering ({structure}; parsed semantics: {}); blockers: {}",
        parsed_features.join(", "),
        blockers.join(", ")
    ))
}

/// Inspect ALB models by lexical scope without generating code.
///
/// Each report describes the model and the limitations of exact lowering.
///
/// # Errors
/// Returns an error for malformed or unsupported model declarations or bodies.
pub fn inspect_alb_source_models(source: &str) -> Result<Vec<String>> {
    fn visit(items: &[Item], reports: &mut Vec<String>) -> Result<()> {
        let files = embedded_alb_files(items)?;
        for statement in electrical_statements(items) {
            if let Some((name, model)) = alb_model(statement, &files)? {
                reports.push(describe_model(&name, &model)?);
            }
        }
        for item in items {
            if let Item::Subcircuit(block) = item {
                visit(&block.body, reports)?;
            }
        }
        Ok(())
    }
    let mut reports = Vec::new();
    visit(&parse_items(source)?, &mut reports)?;
    Ok(reports)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diagnostic(source: &[u8]) -> Option<String> {
        let text = std::str::from_utf8(source).ok()?;
        Some(match inspect_alb_source_models(text) {
            Ok(reports) => reports.join("\n"),
            Err(error) => error.to_string(),
        })
    }

    fn parse_semantics(statement: &str) -> Result<AlbModelSemantics> {
        alb_model_semantics(&AlbFields::parse(statement, ModelTermination::Complete)?)
    }

    #[test]
    fn embedded_stateful_vendor_oracles_compile_with_original_electrical_parameters() -> Result<()>
    {
        for deck in [
            include_str!("../../../runtime/xspice/fixtures/vendor-stateful/edge-basic.net"),
            include_str!("../../../runtime/xspice/fixtures/vendor-stateful/level-basic.net"),
            include_str!("../../../runtime/xspice/fixtures/vendor-stateful/edge-hold-minclock.net"),
            include_str!("../../../runtime/xspice/fixtures/vendor-stateful/level-setup.net"),
            include_str!("../../../runtime/xspice/fixtures/vendor-stateful/edge-async-reset.net"),
            include_str!(
                "../../../runtime/xspice/fixtures/vendor-stateful/controls/edge-minclock-only.net"
            ),
            include_str!(
                "../../../runtime/xspice/fixtures/vendor-stateful/controls/edge-hold-only.net"
            ),
            include_str!(
                "../../../runtime/xspice/fixtures/vendor-stateful/controls/edge-no-qualifiers.net"
            ),
            include_str!(
                "../../../runtime/xspice/fixtures/vendor-stateful/controls/level-setup-no-setup.net"
            ),
        ] {
            let items = parse_items(deck)?;
            let embedded = embedded_alb_files(&items)?;
            assert_eq!(embedded.len(), 1);
            let body = embedded
                .values()
                .next()
                .context("missing ALB definition body")?;
            let (_, program) = crate::alb::analyze_body(body)?.into_parts();
            assert!(program.is_some(), "source must contain EDGE/LEVEL");
            let models = compile_explicit_alb_source_models(deck)?;
            assert!(models[0].kernel.contents.contains("out_res_pos=1e2"));
            assert_eq!(models.len(), 1);
            assert!(models[0].kernel.contents.contains("sr_alb_stateful("));
            assert_eq!(
                models[0].kernel.input_width,
                program
                    .as_ref()
                    .context("missing parsed register program")?
                    .input_width()
            );
            assert_eq!(models[0].kernel.output_width, 1);
        }
        Ok(())
    }

    #[test]
    fn explicit_alb_compiles_and_deduplicates_original_instance_device_vectors() -> Result<()> {
        let source =
            include_str!("../../../runtime/xspice/fixtures/family/logical-device-variants.cir");
        let variants = compile_explicit_alb_source_models(source)?;
        assert_eq!(variants.len(), 2);
        assert_eq!(variants[0].scope, ["TOP"]);
        assert_eq!(variants[0].model_name, "DL");
        assert_eq!(variants[0].device_values.as_deref(), Some([1.0].as_slice()));
        assert_eq!(variants[1].device_values.as_deref(), Some([0.0].as_slice()));
        assert!(variants[0].kernel.contents.contains(r#"table_values="01""#));
        assert!(variants[1].kernel.contents.contains(r#"table_values="00""#));
        assert_eq!(variants[0].kernel.name, "SRALB0");
        assert_eq!(variants[1].kernel.name, "SRALB1");

        let absent = source.replace(" : USER=[1]", "");
        let error = compile_explicit_alb_source_models(&absent)
            .err()
            .context("missing DEVICE must not silently compile a zero LUT")?;
        assert!(format!("{error:#}").contains("DEVICE[0]"), "{error:#}");

        let malformed = source.replace(": USER=[0]", ": USER=[0] UNUSED=1");
        let error = compile_explicit_alb_source_models(&malformed)
            .err()
            .context("unsupported original ALB instance fields must fail closed")?;
        assert!(
            format!("{error:#}").contains("trailing parameters"),
            "{error:#}"
        );
        Ok(())
    }

    #[test]
    fn equivalent_device_results_share_kernel_without_losing_vector_binding() -> Result<()> {
        let source =
            include_str!("../../../runtime/xspice/fixtures/family/logical-device-variants.cir")
                .replace("USER=[0]", "USER=[2]");
        let variants = compile_explicit_alb_source_models(&source)?;
        assert_eq!(variants.len(), 2);
        assert_eq!(variants[0].device_values.as_deref(), Some([1.0].as_slice()));
        assert_eq!(variants[1].device_values.as_deref(), Some([2.0].as_slice()));
        assert!(std::sync::Arc::ptr_eq(
            &variants[0].kernel,
            &variants[1].kernel
        ));
        Ok(())
    }

    #[test]
    fn source_alb_current_limits_default_independently_and_share_equivalent_kernels() -> Result<()>
    {
        let source = r#"
.model DEFAULT_LIMITS d_logic_block def="out[0]=in[0];"
.model EXPLICIT_DEFAULTS d_logic_block min_sink=-1m max_source=1m def="out[0]=in[0];"
.model SINK_OVERRIDE d_logic_block min_sink=-2m def="out[0]=in[0];"
.model SOURCE_OVERRIDE d_logic_block max_source=2m def="out[0]=in[0];"
.model ZERO_LIMITS d_logic_block min_sink=0 max_source=0 def="out[0]=in[0];"
"#;
        let models = compile_explicit_alb_source_models(source)?;
        assert_eq!(models.len(), 5);
        for (model, parameters) in models.iter().zip([
            "min_sink=-1e-3 max_source=1e-3",
            "min_sink=-1e-3 max_source=1e-3",
            "min_sink=-2e-3 max_source=1e-3",
            "min_sink=-1e-3 max_source=2e-3",
            "min_sink=0e0 max_source=0e0",
        ]) {
            assert!(
                model.kernel.contents.contains(parameters),
                "{}",
                model.kernel.contents
            );
        }
        assert!(std::sync::Arc::ptr_eq(&models[0].kernel, &models[1].kernel));
        Ok(())
    }

    #[test]
    fn native_kernel_identity_preserves_family_bindings_and_electrical_semantics() -> Result<()> {
        let mut source = String::new();
        for (scope, family, load, resistance, delay) in [
            ("SRALB0", "HC", "2p", "40", "2n"),
            ("SECOND", "UNIV", "2p", "40", "2n"),
            ("THIRD", "HC", "3p", "40", "2n"),
            ("FOURTH", "HC", "2p", "41", "2n"),
            ("FIFTH", "HC", "2p", "40", "4n"),
        ] {
            write!(
                source,
                ".subckt {scope} a y\nA1 [a] [y] DL\n.model DL d_logic_block file=logic family=\"{family}\" input_load={load} out_res={resistance} out_delay={delay}\n.file logic\nPORT A in[0] ;\nPORT Y out[0] ;\nY = A ;\n.endf\n.ends {scope}\n"
            )?;
        }
        let models = compile_explicit_alb_source_models(&source)?;
        assert_eq!(models.len(), 5);
        assert_eq!(models[0].scope, ["SRALB0"]);
        assert_eq!(models[1].scope, ["SECOND"]);
        assert_eq!(models[0].kernel.name, "SRALB1");
        assert_eq!(models[0].in_family, "HC");
        assert_eq!(models[1].in_family, "UNIV");
        assert!(std::sync::Arc::ptr_eq(&models[0].kernel, &models[1].kernel));
        let unique = models
            .iter()
            .map(|m| m.kernel.name.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(unique.len(), 4);
        assert!(models[2].kernel.contents.contains("capacitance=3e-12"));
        assert!(models[3].kernel.contents.contains("out_res_pos=4.1e1"));
        assert!(models[4].kernel.contents.contains("delay=4e-9"));
        Ok(())
    }

    #[test]
    fn explicit_stateless_alb_compiles_model_user_parameters_into_distinct_luts() -> Result<()> {
        let source = r#"
.model DL d_logic_block file=logic family="UNIV" user=[0.75] user_scale=3 out_delay=2n
.file logic
PORT A in[0] ;
PORT Y out[0] ;
Y = (USER[0]>1) ? A : 0 ;
.endf
"#;
        let on = compile_explicit_alb_source_models(source)?;
        assert_eq!(on.len(), 1);
        assert!(
            on[0].kernel.contents.contains("table_values=\"01\""),
            "{}",
            on[0].kernel.contents
        );
        let off_source = source.replace("user_scale=3", "user_scale=1");
        let off = compile_explicit_alb_source_models(&off_source)?;
        assert_eq!(off.len(), 1);
        assert!(
            off[0].kernel.contents.contains("table_values=\"00\""),
            "{}",
            off[0].kernel.contents
        );
        assert_ne!(on[0].kernel.contents, off[0].kernel.contents);
        Ok(())
    }

    #[test]
    fn stateless_inline_alb_def_compiles_before_mixed_signal_fail_closed() {
        let source = br#".subckt T a b y
A1 [a b] [y] DL
.model DL d_logic_block def="out[0]=in[0]&in[1];"
.ends T
.end
"#;
        let error = diagnostic(source);
        assert!(error.is_some_and(|value| {
            value.contains("compiled 2-input/1-output known-state ALB truth table")
                && value.contains("4 d_genlut values")
        }));
    }

    #[test]
    fn alb_inline_definition_overrides_existing_leading_file() -> Result<()> {
        let files = std::collections::BTreeMap::from([
            (
                "LOGIC".to_owned(),
                "PORT D in[0]; PORT Q out[0]; Q=!D;".to_owned(),
            ),
            ("BAD".to_owned(), "invalid code;".to_owned()),
        ]);
        for (statement, expected_body) in [
            (
                r#".model DL d_logic_block file=LOGIC def="out[0]=in[0];""#,
                "out[0]=in[0];",
            ),
            (
                r#".model DL d_logic_block def="out[0]=in[0];" file=MISSING"#,
                "out[0]=in[0];",
            ),
            (
                ".model DL d_logic_block file=LOGIC",
                "PORT D in[0]; PORT Q out[0]; Q=!D;",
            ),
        ] {
            let (_, body, _) = alb_model_definition(statement, &files)?.context("missing ALB")?;
            assert_eq!(body, expected_body);
        }
        for file in ["MISSING", "BAD"] {
            let statement = format!(r#".model DL d_logic_block file={file} def="out[0]=in[0];""#);
            assert!(alb_model_definition(&statement, &files).is_err());
        }
        Ok(())
    }

    #[test]
    fn alb_inline_definition_terminates_electrical_parameters() -> Result<()> {
        for (raw, expected_family, expected_supply, expected_load) in [
            (
                r#".model DL d_logic_block def="out[0]=in[0];" family="HC" vsupply=3.3 input_load=2p"#,
                "UNIV",
                None,
                1e-12_f64,
            ),
            (
                r#".model DL d_logic_block family="HC" def="out[0]=in[0];" vsupply=3.3 input_load=2p"#,
                "HC",
                None,
                1e-12,
            ),
            (
                r#".model DL d_logic_block family="HC" vsupply=3.3 input_load=2p def="out[0]=in[0];" input_load=7p"#,
                "HC",
                Some(3.3_f64),
                2e-12,
            ),
            (
                r#".model DL d_logic_block def="out[0]=in[0]; VSUPPLY=3.3 FAMILY='HC'""#,
                "UNIV",
                None,
                1e-12,
            ),
        ] {
            let (_, _, semantics) = alb_model_definition(raw, &std::collections::BTreeMap::new())?
                .context("missing ALB")?;
            assert_eq!(
                semantics.effective_families(),
                (expected_family, expected_family)
            );
            assert_eq!(
                semantics.supply_v.map(f64::to_bits),
                expected_supply.map(f64::to_bits)
            );
            assert_eq!(
                semantics.effective_input_load().to_bits(),
                expected_load.to_bits()
            );
        }
        Ok(())
    }

    #[test]
    fn legacy_bare_alb_file_repairs_unique_missing_leading_underscore() {
        let source = br".subckt T a y
A1 [a] [y] DL
.model DL d_logic_block file logic
.file _logic
PORT A in[0] ;
PORT Y out[0] ;
Y = A ;
.endf
.ends T
.end
";
        let error = diagnostic(source);
        assert!(error.is_some_and(|value| {
            value.contains("compiled 1-input/1-output known-state ALB truth table")
                && value.contains("2 d_genlut values")
        }));
    }

    #[test]
    fn stateless_embedded_alb_accepts_legacy_bare_file_parameter() {
        let source = br".subckt T a b y
A1 [a b] [y] DL
.model DL d_logic_block file logic
.file logic
PORT A in[0:1] ;
PORT Y out[0] ;
Y = A==3 ;
.endf
.ends T
.end
";
        let error = diagnostic(source);
        assert!(error.is_some_and(|value| {
            value.contains("compiled 2-input/1-output known-state ALB truth table")
                && value.contains("4 d_genlut values")
        }));
    }

    #[test]
    fn stateless_embedded_alb_compiles_before_mixed_signal_fail_closed() {
        let source = br".subckt T a b y
A1 [a b] [y] DL
.model DL d_logic_block file=logic
.file logic
PORT A in[0:1] ;
PORT Y out[0] ;
Y = A==3 ;
.endf
.ends T
.end
";
        let error = diagnostic(source);
        assert!(error.is_some_and(|value| {
            value.contains("compiled 2-input/1-output known-state ALB truth table")
                && value.contains("4 d_genlut values")
                && value.contains("unproven UNKNOWN-state expression semantics")
        }));
    }

    #[test]
    fn parses_common_alb_model_semantics_and_reports_exactness_blockers() -> Result<()> {
        let model = parse_semantics(
            r#".model DL d_logic_block file=logic user=[5n, 0, 7n, 5n] input_load=4.5p out_res=40 out_delay=12.4n family="HC""#,
        )?;
        assert_eq!(model.input_load, Some(4.5e-12));
        assert_eq!(model.out_res_pos, Some(40.0));
        assert_eq!(model.out_res_neg, Some(40.0));
        assert!(
            model
                .out_delay
                .is_some_and(|value| (value - 12.4e-9).abs() < 1.0e-20)
        );
        for (actual, expected) in model.user.iter().zip([5e-9, 0.0, 7e-9, 5e-9]) {
            assert!((*actual - expected).abs() < 1.0e-20);
        }
        assert_eq!(model.family.as_deref(), Some("HC"));
        assert_eq!(model.effective_families(), ("HC", "HC"));
        assert_eq!(model.effective_reg_delay(), 1.0e-9);
        assert_eq!(model.effective_setup_time(), 0.0);
        assert_eq!(model.effective_hold_time(), 0.0);
        assert_eq!(model.effective_min_clock(), 0.0);
        assert_eq!(model.effective_user_scale(), 1.0);
        let features = model.parsed_features();
        assert!(features.contains(&"input_load"));
        assert!(features.contains(&"output_resistance"));
        assert!(features.contains(&"out_delay"));
        assert!(features.contains(&"user_vector"));
        assert!(features.contains(&"family"));
        let blockers = model.exact_lowering_blockers(None);
        assert!(blockers.contains(&"stateful/delayed ALB body"));
        assert!(blockers.contains(&"load-dependent output delay"));
        assert!(blockers.contains(&"digital family bridge semantics"));
        Ok(())
    }

    #[test]
    fn alb_model_semantics_accept_legacy_bare_user_vector() -> Result<()> {
        let semantics = parse_semantics(
            r#".model DL d_logic_block file=logic user [1.4n, 7.4n, 1n, 1.4n] family="HC""#,
        )?;
        assert_eq!(semantics.user.len(), 4);
        let spaced = parse_semantics(
            r#".model DL d_logic_block file=logic user=[1.4n 7.4n, 1n 1.4n] family="HC""#,
        )?;
        assert_eq!(spaced.user, semantics.user);
        assert!((semantics.user[0] - 1.4e-9).abs() < 1.0e-20);
        assert!((semantics.user[3] - 1.4e-9).abs() < 1.0e-20);
        Ok(())
    }

    #[test]
    fn alb_model_semantics_apply_documented_defaults_and_family_precedence() -> Result<()> {
        let defaulted = parse_semantics(".model DL d_logic_block file=logic")?;
        assert_eq!(defaulted.effective_input_load(), 1.0e-12);
        assert_eq!(defaulted.effective_out_res_pos(), 100.0);
        assert_eq!(defaulted.effective_out_res_neg(), 100.0);
        assert_eq!(defaulted.effective_out_delay(), 1.0e-9);
        assert_eq!(defaulted.effective_reg_delay(), 1.0e-9);
        assert_eq!(defaulted.effective_families(), ("UNIV", "UNIV"));
        assert!(
            defaulted
                .exact_lowering_blockers(None)
                .contains(&"load-dependent output delay")
        );

        let overridden = parse_semantics(
            r#".model DL d_logic_block file=logic family="HC" in_family="LS" out_res=-1000 out_res_pos=4"#,
        )?;
        assert_eq!(overridden.effective_families(), ("LS", "HC"));
        assert_eq!(overridden.effective_out_res_pos(), 4.0);
        assert_eq!(overridden.effective_out_res_neg(), -1000.0);
        Ok(())
    }

    #[test]
    fn stateful_alb_diagnostic_names_timing_and_fanout_blockers() {
        let source = br#".subckt T a y
A1 [a] [y] DL
.model DL d_logic_block file=logic user=[5n, 7n] input_load=5p out_res=80 family="LS" sink_current=-220u source_current=-10u min_sink=-.02 max_source=.02
.file logic
PORT A in[0] ;
PORT Y out[0] ;
COMB (WIDTH=1, DELAY=USER[0]) D ;
D=A ;
Y=D ;
.endf
.ends T
.end
"#;
        let error = diagnostic(source);
        assert!(error.is_some_and(|value| {
            value.contains("stateful/delayed ALB body")
                && value.contains("state/timing parameter environment")
                && value.contains("load-dependent output delay")
                && value.contains("output strength/fanout semantics")
                && value.contains("digital family bridge semantics")
                && value.contains("user_vector")
        }));
    }
}
