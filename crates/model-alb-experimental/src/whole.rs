//! Explicit source-model inspection and experimental compilation.
mod logical_ports;
use crate::alb::{
    KnownTruthTable, StatefulProgram, TimedCombinationalGraph, TimingEnvironment, analyze_body,
};
use anyhow::{Context, Result, bail};
pub use logical_ports::{
    AlbOutputProducer, FamilySpecialization, LogicalAlbBoundary, LogicalAlbExpansion,
    OccurrenceFamilyBoundary, expand_logical_alb_source, specialize_family_occurrences,
};
use model_translate_simplis::syntax::{
    Item, Subcircuit, electrical_statements, embedded_files, model_header, normalized_statement,
    parse_items, simple_assignment_value, statement_tokens,
};
use std::fmt::Write as _;

fn simple_assignment_or_positional_value(statement: &str, name: &str) -> Result<Option<String>> {
    if let Some(value) = simple_assignment_value(statement, name) {
        return Ok(Some(value));
    }
    let normalized = normalized_statement(statement);
    let tokens = statement_tokens(&normalized)?;
    let Some(index) = tokens
        .iter()
        .position(|token| token.eq_ignore_ascii_case(name))
    else {
        return Ok(None);
    };
    let value = match tokens.get(index.saturating_add(1)).map(String::as_str) {
        Some("=") => tokens.get(index.saturating_add(2)),
        Some(value) if !value.starts_with('=') => tokens.get(index.saturating_add(1)),
        Some(_) | None => None,
    }
    .with_context(|| format!("missing value after SIMetrix model parameter {name}"))?;
    Ok(Some(value.clone()))
}

fn quoted_assignment_value(statement: &str, name: &str) -> Result<Option<String>> {
    let normalized = normalized_statement(statement);
    let upper = normalized.to_ascii_uppercase();
    let needle = name.to_ascii_uppercase();
    let mut offset = 0usize;
    while let Some(found) = upper[offset..].find(&needle) {
        let start = offset + found;
        let boundary = start == 0
            || upper.as_bytes()[start - 1].is_ascii_whitespace()
            || matches!(upper.as_bytes()[start - 1], b'(' | b',');
        let mut cursor = start + needle.len();
        if !boundary
            || upper
                .as_bytes()
                .get(cursor)
                .is_some_and(|byte| !byte.is_ascii_whitespace() && *byte != b'=')
        {
            offset = cursor;
            continue;
        }
        while upper
            .as_bytes()
            .get(cursor)
            .is_some_and(u8::is_ascii_whitespace)
        {
            cursor += 1;
        }
        if upper.as_bytes().get(cursor) != Some(&b'=') {
            offset = cursor;
            continue;
        }
        cursor += 1;
        while upper
            .as_bytes()
            .get(cursor)
            .is_some_and(u8::is_ascii_whitespace)
        {
            cursor += 1;
        }
        let quote = normalized
            .as_bytes()
            .get(cursor)
            .copied()
            .filter(|byte| matches!(*byte, b'\'' | b'"'))
            .with_context(|| format!("SIMetrix model parameter {name} must be quoted"))?;
        cursor += 1;
        let value_start = cursor;
        while let Some(byte) = normalized.as_bytes().get(cursor).copied() {
            if byte == quote {
                return Ok(Some(normalized[value_start..cursor].to_owned()));
            }
            cursor += 1;
        }
        bail!("unterminated quoted SIMetrix model parameter {name}");
    }
    Ok(None)
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

fn optional_numeric_assignment(statement: &str, name: &str) -> Result<Option<f64>> {
    simple_assignment_value(statement, name)
        .map(|value| crate::parse_numeric(&value))
        .transpose()
        .with_context(|| format!("parsing SIMetrix ALB model parameter {name}"))
}

fn bracketed_assignment_value(statement: &str, name: &str) -> Result<Option<String>> {
    let normalized = normalized_statement(statement);
    let upper = normalized.to_ascii_uppercase();
    let needle = name.to_ascii_uppercase();
    let mut offset = 0usize;
    while let Some(found) = upper[offset..].find(&needle) {
        let start = offset + found;
        let boundary = start == 0
            || upper.as_bytes()[start - 1].is_ascii_whitespace()
            || matches!(upper.as_bytes()[start - 1], b'(' | b',');
        let mut cursor = start + needle.len();
        if !boundary
            || upper
                .as_bytes()
                .get(cursor)
                .is_some_and(|byte| !byte.is_ascii_whitespace() && *byte != b'=')
        {
            offset = cursor;
            continue;
        }
        while upper
            .as_bytes()
            .get(cursor)
            .is_some_and(u8::is_ascii_whitespace)
        {
            cursor += 1;
        }
        if upper.as_bytes().get(cursor) == Some(&b'=') {
            cursor += 1;
            while upper
                .as_bytes()
                .get(cursor)
                .is_some_and(u8::is_ascii_whitespace)
            {
                cursor += 1;
            }
        }
        if normalized.as_bytes().get(cursor) != Some(&b'[') {
            offset = cursor;
            continue;
        }
        let value_start = cursor + 1;
        cursor = value_start;
        while let Some(byte) = normalized.as_bytes().get(cursor).copied() {
            if byte == b']' {
                return Ok(Some(normalized[value_start..cursor].to_owned()));
            }
            cursor += 1;
        }
        bail!("unterminated bracketed SIMetrix model parameter {name}");
    }
    Ok(None)
}

fn numeric_vector_assignment(statement: &str, name: &str) -> Result<Vec<f64>> {
    let Some(value) = bracketed_assignment_value(statement, name)? else {
        return Ok(Vec::new());
    };
    let mut output = Vec::new();
    for item in value.split(',') {
        let item = item.trim();
        if item.is_empty() {
            bail!("empty value in SIMetrix ALB model vector {name}");
        }
        output.push(
            crate::parse_numeric(item)
                .with_context(|| format!("parsing SIMetrix ALB model vector {name}"))?,
        );
    }
    Ok(output)
}

fn optional_family_assignment(statement: &str, name: &str) -> Result<Option<String>> {
    quoted_assignment_value(statement, name)
        .map(|value| value.map(|value| value.to_ascii_uppercase()))
}

fn alb_model_semantics(statement: &str) -> Result<AlbModelSemantics> {
    let out_res = optional_numeric_assignment(statement, "OUT_RES")?;
    let semantics = AlbModelSemantics {
        input_load: optional_numeric_assignment(statement, "INPUT_LOAD")?,
        out_res_pos: optional_numeric_assignment(statement, "OUT_RES_POS")?.or(out_res),
        out_res_neg: optional_numeric_assignment(statement, "OUT_RES_NEG")?.or(out_res),
        out_delay: optional_numeric_assignment(statement, "OUT_DELAY")?,
        reg_delay: optional_numeric_assignment(statement, "REG_DELAY")?,
        setup_time: optional_numeric_assignment(statement, "SETUP_TIME")?,
        hold_time: optional_numeric_assignment(statement, "HOLD_TIME")?,
        min_clock: optional_numeric_assignment(statement, "MIN_CLOCK")?,
        user: numeric_vector_assignment(statement, "USER")?,
        user_scale: optional_numeric_assignment(statement, "USER_SCALE")?,
        family: optional_family_assignment(statement, "FAMILY")?,
        in_family: optional_family_assignment(statement, "IN_FAMILY")?,
        out_family: optional_family_assignment(statement, "OUT_FAMILY")?,
        min_sink: optional_numeric_assignment(statement, "MIN_SINK")?,
        max_source: optional_numeric_assignment(statement, "MAX_SOURCE")?,
        sink_current: optional_numeric_assignment(statement, "SINK_CURRENT")?,
        source_current: optional_numeric_assignment(statement, "SOURCE_CURRENT")?,
    };
    let bounded_nonnegative = [
        ("REG_DELAY", semantics.effective_reg_delay()),
        ("SETUP_TIME", semantics.effective_setup_time()),
        ("HOLD_TIME", semantics.effective_hold_time()),
        ("MIN_CLOCK", semantics.effective_min_clock()),
        ("USER_SCALE", semantics.effective_user_scale()),
        ("OUT_RES_POS", semantics.effective_out_res_pos()),
        ("OUT_RES_NEG", semantics.effective_out_res_neg()),
    ];
    for (name, value) in bounded_nonnegative {
        if !value.is_finite() || value < 0.0 {
            bail!("invalid SIMetrix ALB model parameter {name}={value}");
        }
    }
    if !semantics.effective_out_delay().is_finite() || semantics.effective_out_delay() < 1.0e-12 {
        bail!(
            "invalid SIMetrix ALB model parameter OUT_DELAY={}",
            semantics.effective_out_delay()
        );
    }
    if !semantics.effective_input_load().is_finite() {
        bail!("invalid SIMetrix ALB model INPUT_LOAD");
    }
    Ok(semantics)
}

fn alb_model(
    statement: &str,
    files: &std::collections::BTreeMap<String, Vec<&str>>,
) -> Result<Option<(String, AlbModel)>> {
    let Some((name, kind)) = model_header(statement) else {
        return Ok(None);
    };
    if kind != "D_LOGIC_BLOCK" {
        return Ok(None);
    }
    let file = simple_assignment_or_positional_value(statement, "FILE")?;
    let definition = quoted_assignment_value(statement, "DEF")?;
    let body = match (file, definition) {
        (Some(_), Some(_)) => {
            bail!("SIMetrix D_LOGIC_BLOCK model {name} specifies both FILE and DEF")
        }
        (Some(file), None) => {
            let file = file.trim_matches(['\'', '"']).to_ascii_uppercase();
            let exact = files.get(&file);
            let legacy_prefixed = (exact.is_none()
                && simple_assignment_value(statement, "FILE").is_none()
                && !file.starts_with('_'))
            .then(|| format!("_{file}"))
            .and_then(|candidate| files.get(&candidate));
            exact.or(legacy_prefixed).map(|lines| lines.join("\n")).with_context(|| {
                format!(
                    "SIMetrix D_LOGIC_BLOCK model {name} references missing embedded file {file}"
                )
            })?
        }
        (None, Some(definition)) => definition,
        (None, None) => bail!("SIMetrix D_LOGIC_BLOCK model {name} has neither FILE nor DEF"),
    };
    let (table, stateful) = analyze_body(&body)
        .with_context(|| format!("analyzing SIMetrix ALB body for model {name}"))?
        .into_parts();
    let semantics = alb_model_semantics(statement)
        .with_context(|| format!("parsing SIMetrix ALB model semantics for {name}"))?;
    Ok(Some((
        name,
        AlbModel {
            table,
            stateful,
            semantics,
        },
    )))
}

/// One explicitly compiled original `SIMetrix` ALB model in lexical scope.
/// This experimental stage does not change the production translator.
#[derive(Clone, Debug)]
pub struct CompiledSourceAlbModel {
    pub scope: Vec<String>,
    pub model_name: String,
    pub subcircuit_name: String,
    pub input_width: usize,
    pub output_width: usize,
    pub source: String,
}

fn collect_explicit_alb_sources(
    items: &[Item],
    scope: &[String],
    compiled: &mut Vec<CompiledSourceAlbModel>,
) -> Result<()> {
    let files = embedded_files(items)?;
    let mut names = std::collections::BTreeSet::new();
    for statement in electrical_statements(items) {
        if let Some((name, alb)) = alb_model(statement, &files)? {
            if !names.insert(name.clone()) {
                bail!("duplicate ALB model {name} in scope {scope:?}");
            }
            let subcircuit_name = format!("SRALB{}", compiled.len());
            let timing = TimingEnvironment {
                user: &alb.semantics.user,
                device: None,
                user_scale: alb.semantics.effective_user_scale(),
                default_register_delay: alb.semantics.effective_reg_delay(),
                default_output_delay: alb.semantics.effective_out_delay(),
            };
            let plan = if let Some(program) = alb.stateful.as_ref() {
                program
                    .timed_combinational_graph(timing)?
                    .context("unsupported stateful or feedback ALB body")?
                    .xspice_timed_plan()?
            } else if let Some(table) = alb.table.as_ref() {
                crate::alb::XspiceTimedPlan::from_known_truth_table(
                    table,
                    alb.semantics.effective_out_delay(),
                )?
            } else {
                bail!("ALB model {name} does not contain a compilable logic program");
            };
            let pos = alb.semantics.effective_out_res_pos();
            let neg = alb.semantics.effective_out_res_neg();
            if pos.to_bits() != neg.to_bits() {
                bail!(
                    "ALB {name} unequal OUT_RES_POS/OUT_RES_NEG is not representable by the native LUT"
                );
            }
            let native = crate::alb::XspiceElectrical {
                input_load_f: alb.semantics.effective_input_load(),
                input_sink_current_a: alb.semantics.sink_current.unwrap_or(0.0),
                input_source_current_a: alb.semantics.source_current.unwrap_or(0.0),
                min_sink_a: alb.semantics.min_sink.unwrap_or(-1.0e99),
                max_source_a: alb.semantics.max_source.unwrap_or(1.0e99),
                stage_out_res_ohm: 0.0,
                output_out_res_ohm: pos,
            };
            let source = plan
                .render_subcircuit(&subcircuit_name, native)
                .with_context(|| format!("compiling original ALB model {name}"))?;
            compiled.push(CompiledSourceAlbModel {
                scope: scope.to_vec(),
                model_name: name,
                subcircuit_name,
                input_width: plan.input_width(),
                output_width: plan.output_width(),
                source,
            });
        }
    }
    for item in items {
        if let Item::Subcircuit(block) = item {
            let mut next = scope.to_vec();
            next.push(block.name.to_ascii_uppercase());
            collect_explicit_alb_sources(&block.body, &next, compiled)?;
        }
    }
    Ok(())
}

/// Compile original embedded `D_LOGIC_BLOCK` definitions (including `.file`
/// bodies) using the existing ALB parser and experimental XSPICE compiler.
///
/// # Errors
/// Missing files, unsupported ALB semantics or native physical parameters.
pub fn compile_explicit_alb_source_models(source: &str) -> Result<Vec<CompiledSourceAlbModel>> {
    let items = parse_items(source)?;
    let mut compiled = Vec::new();
    collect_explicit_alb_sources(&items, &[], &mut compiled)?;
    Ok(compiled)
}

fn resolved_comb_timing_summary(model: &AlbModel) -> Result<Option<String>> {
    let Some(program) = model.stateful.as_ref() else {
        return Ok(None);
    };
    let environment = TimingEnvironment {
        user: &model.semantics.user,
        device: None,
        user_scale: model.semantics.effective_user_scale(),
        default_register_delay: model.semantics.effective_reg_delay(),
        default_output_delay: model.semantics.effective_out_delay(),
    };
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

/// Inspect ALB models in lexical source order without generating code.
///
/// Each report describes the model and the limitations of exact lowering.
///
/// # Errors
/// Returns an error for malformed or unsupported model declarations or bodies.
pub fn inspect_alb_source_models(source: &str) -> Result<Vec<String>> {
    fn visit(items: &[Item], reports: &mut Vec<String>) -> Result<()> {
        let files = embedded_files(items)?;
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

    #[test]
    fn embedded_stateful_vendor_oracles_parse_but_fail_closed_in_xspice() -> Result<()> {
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
            let embedded = embedded_files(&items)?;
            assert_eq!(embedded.len(), 1);
            let body = embedded
                .values()
                .next()
                .context("missing ALB definition body")?;
            let (_, program) = crate::alb::analyze_body(&body.join("\n"))?.into_parts();
            assert!(program.is_some(), "source must contain EDGE/LEVEL");
            let err = compile_explicit_alb_source_models(deck)
                .err()
                .context("stateful proprietary source unexpectedly compiled to COMB")?;
            assert!(
                format!("{err:#}").contains("unsupported stateful or feedback ALB body"),
                "{err:#}"
            );
        }
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
    fn alb_model_rejects_file_and_def_together() {
        let source = br#".subckt T a y
A1 [a] [y] DL
.model DL d_logic_block file=logic def="out[0]=in[0];"
.file logic
PORT A in[0] ;
PORT Y out[0] ;
Y=A ;
.endf
.ends T
.end
"#;
        let error = diagnostic(source);
        assert!(error.is_some_and(|value| value.contains("specifies both FILE and DEF")));
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
        let model = alb_model_semantics(
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
        let semantics = alb_model_semantics(
            r#".model DL d_logic_block file=logic user [1.4n, 7.4n, 1n, 1.4n] family="HC""#,
        )?;
        assert_eq!(semantics.user.len(), 4);
        assert!((semantics.user[0] - 1.4e-9).abs() < 1.0e-20);
        assert!((semantics.user[3] - 1.4e-9).abs() < 1.0e-20);
        Ok(())
    }

    #[test]
    fn alb_model_semantics_apply_documented_defaults_and_family_precedence() -> Result<()> {
        let defaulted = alb_model_semantics(".model DL d_logic_block file=logic")?;
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

        let overridden = alb_model_semantics(
            r#".model DL d_logic_block file=logic family="HC" in_family="LS" out_res=0 out_res_pos=4"#,
        )?;
        assert_eq!(overridden.effective_families(), ("LS", "HC"));
        assert_eq!(overridden.effective_out_res_pos(), 4.0);
        assert_eq!(overridden.effective_out_res_neg(), 0.0);
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
