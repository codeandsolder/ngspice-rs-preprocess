mod passive_ic;
mod pwls;
pub use passive_ic::{capture_artifact_initial_conditions, capture_passive_initial_conditions};
mod simetrix_runtime;
use super::{
    CodeModelRuntime, DigitalBufferKind, DigitalBufferSpec, LogicGateKind, LogicGateMode,
    LogicGateSpec, LogicIo, LogicOperation, LogicOutput, LogicPolarity, LogicReceiver,
    LogicThresholds, LogicTiming, Point, Translation, TranslationKind, emit_digital_buffer,
    emit_logic_event, emit_logic_gate, emit_logic_input_loads, emit_logic_output,
    emit_logic_voltage, indexed_points, integer_parameter, model_statement_tokens,
    normalized_statement, numeric_parameter, parse_assignments, sanitized_name, statement_tokens,
    translate_charge_capacitor, translate_flux_inductor, translate_ipwlr, translate_vpwlr,
    validate_node, write_number,
};
#[cfg(test)]
use crate::syntax::logical_statements;
use crate::syntax::{
    Item, Subcircuit, electrical_statements, file_boundary, matching_behavioral_delimiter,
    model_header, parse_items, simple_assignment_span, simple_assignment_value,
};
use anyhow::{Context, Result, bail};
use model_normalize_ngspice::passive_ic::PassiveKind;
use serde::Serialize;
pub use simetrix_runtime::lower_simetrix_linked_runtime;
use std::fmt::Write as _;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WholeModelTranslation {
    pub bytes: Vec<u8>,
    pub primitives: Vec<TranslationKind>,
    pub required_code_models: Vec<CodeModelRuntime>,
    pub stripped_directives: usize,
    pub flattened_subcircuits: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SplitDeckTranslation {
    pub deck_bytes: Vec<u8>,
    pub library_bytes: Vec<u8>,
    pub primitives: Vec<TranslationKind>,
    pub required_code_models: Vec<CodeModelRuntime>,
    pub stripped_directives: usize,
    pub flattened_subcircuits: usize,
}

fn render_native_subcircuit(block: &Subcircuit, output: &mut String) -> Result<()> {
    output.push_str(&block.header);
    output.push('\n');
    for item in &block.body {
        match item {
            Item::Statement(statement) => {
                output.push_str(statement);
                output.push('\n');
            }
            Item::Subcircuit(child) => render_native_subcircuit(child, output)?,
        }
    }
    writeln!(output, ".ends {}", block.name)?;
    output.push('\n');
    Ok(())
}

fn split_native_deck(input: &[u8], include_path: &str) -> Result<(Vec<u8>, Vec<u8>)> {
    if include_path.is_empty()
        || include_path
            .as_bytes()
            .iter()
            .any(|byte| matches!(*byte, b'"' | b'\r' | b'\n'))
    {
        bail!("invalid ngspice include path");
    }
    let text = std::str::from_utf8(input).context("translated ngspice deck is not UTF-8")?;
    let items = parse_items(text)?;
    let mut deck = String::new();
    let mut library =
        String::from("* ngspice-rs-preprocess: translated SIMPLIS generated subcircuits\n");
    let mut inserted_include = false;
    let mut subcircuits = 0usize;
    for item in &items {
        match item {
            Item::Statement(statement) => {
                if directive_name(statement).as_deref() == Some(".END") {
                    continue;
                }
                deck.push_str(statement);
                deck.push('\n');
            }
            Item::Subcircuit(block) => {
                if !inserted_include {
                    writeln!(deck, ".include \"{include_path}\"")?;
                    inserted_include = true;
                }
                render_native_subcircuit(block, &mut library)?;
                subcircuits = subcircuits.saturating_add(1);
            }
        }
    }
    if subcircuits == 0 {
        bail!("translated ngspice deck contains no subcircuits to split");
    }
    deck.push_str(".end\n");
    Ok((deck.into_bytes(), library.into_bytes()))
}

trait WhitespaceProbe {
    fn is_empty_or_whitespace(&self) -> bool;
}

impl WhitespaceProbe for str {
    fn is_empty_or_whitespace(&self) -> bool {
        self.is_empty() || self.as_bytes().iter().all(u8::is_ascii_whitespace)
    }
}

fn directive_name(statement: &str) -> Option<String> {
    let normalized = normalized_statement(statement);
    let first = normalized.split_whitespace().next()?;
    first.starts_with('.').then(|| first.to_ascii_uppercase())
}

fn native_nodeset_target(target: &str) -> bool {
    target.eq_ignore_ascii_case("all")
        || (target
            .get(..2)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("V("))
            && target.ends_with(')'))
}

fn native_nodeset_assignment_starts(fields: &[String]) -> bool {
    let Some(first) = fields.first() else {
        return false;
    };
    if let Some((target, _)) = first.split_once('=') {
        return native_nodeset_target(target);
    }
    fields.get(1).is_some_and(|field| field.starts_with('=')) && native_nodeset_target(first)
}

fn rewrite_simetrix_nodeset(statement: &str) -> Result<Option<String>> {
    if directive_name(statement).as_deref() != Some(".NODESET") {
        return Ok(None);
    }
    let tokens = statement_tokens(&normalized_statement(statement))?;
    let fields = &tokens[1..];
    if native_nodeset_assignment_starts(fields) {
        return Ok(None);
    }
    if fields.is_empty() || !fields.len().is_multiple_of(2) {
        bail!("malformed SIMetrix .NODESET shorthand");
    }
    let mut output = String::from(".nodeset");
    for [node, value] in fields.as_chunks::<2>().0 {
        validate_node(node)?;
        if value.starts_with('=') {
            bail!("malformed SIMetrix .NODESET shorthand value {value:?}");
        }
        write!(output, " V({node})={value}")?;
    }
    Ok(Some(output))
}

fn strip_directive(statement: &str) -> bool {
    matches!(
        directive_name(statement).as_deref(),
        Some(
            ".NODE_MAP"
                | ".LICENSE_FEATURE"
                | ".GRAPH"
                | ".PRINT"
                | ".POST_PROCESS"
                | ".OPTIONS"
                | ".OPTION"
                | ".OPT"
                | ".AC"
                | ".POP"
                | ".TRAN"
                | ".KEEP"
                | ".REQUIRED_VERSION"
                | ".INIT"
                | ".END"
        )
    )
}

fn unsupported_directive(statement: &str) -> bool {
    matches!(
        directive_name(statement).as_deref(),
        Some(
            ".VAR" | ".GLOBALVAR" | ".IF" | ".ELSE" | ".ENDIF" | ".WHILE" | ".ENDWHILE" | ".ERROR"
        )
    )
}

fn magic_comment(statement: &str) -> bool {
    statement.trim_start().starts_with("*#")
}

fn residual_digital_a_device(trimmed: &str) -> bool {
    trimmed
        .as_bytes()
        .first()
        .is_some_and(|byte| matches!(*byte, b'A' | b'a'))
        && trimmed.contains("%gnd_ref")
        && trimmed.to_ascii_uppercase().contains("MODEL=")
}

fn residual_source_waveform(trimmed: &str) -> bool {
    let fields = trimmed.split_ascii_whitespace().collect::<Vec<_>>();
    let source = fields.first().is_some_and(|field| {
        field
            .as_bytes()
            .first()
            .is_some_and(|byte| matches!(*byte, b'V' | b'v' | b'I' | b'i'))
    });
    let Some(kind) = fields.get(3).map(|kind| kind.to_ascii_uppercase()) else {
        return false;
    };
    if source
        && matches!(kind.as_str(), "PWL" | "SIN")
        && fields.get(4).is_some_and(|token| !token.contains('='))
    {
        return false;
    }
    source && matches!(kind.as_str(), "PUL" | "SAW" | "PWL" | "SIN" | "PWLS")
}

fn generated_simplis_identifier(token: &str) -> bool {
    token.rsplit_once('$').is_some_and(|(_, suffix)| {
        (!suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit()))
            || ["SP_", "LIB_"].iter().any(|prefix| {
                suffix.strip_prefix(prefix).is_some_and(|hash| {
                    hash.len() == 24 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
                })
            })
    })
}

fn residual_simplis_reason(statement: &str) -> Option<&'static str> {
    let trimmed = statement.trim();
    if trimmed.is_empty() || trimmed.starts_with('*') {
        return None;
    }
    if trimmed.starts_with("?@@") {
        return Some("protected SIMPLIS body");
    }
    if trimmed.starts_with('!') {
        return Some("SIMPLIS bang device");
    }
    if residual_digital_a_device(trimmed) {
        return Some("unlowered SIMPLIS DIGITAL1 A-device");
    }
    if residual_source_waveform(trimmed) {
        return Some("unlowered SIMPLIS source waveform");
    }
    if trimmed.split_ascii_whitespace().any(|token| {
        let upper = token.to_ascii_uppercase();
        upper.starts_with("SIMPLIS_") && !generated_simplis_identifier(&upper)
    }) {
        return Some("unlowered SIMPLIS helper/macro");
    }
    if trimmed.to_ascii_lowercase().contains(" vars:") {
        return Some("SIMPLIS vars: syntax");
    }
    let mut fields = trimmed.split_ascii_whitespace();
    if fields
        .next()
        .is_some_and(|field| field.eq_ignore_ascii_case(".MODEL"))
    {
        let _name = fields.next();
        if let Some(model_type) = fields.next() {
            let model_type = model_type
                .split_once('(')
                .map_or(model_type, |(head, _)| head)
                .to_ascii_uppercase();
            let numbered_logic = ["OR", "NOR", "AND", "NAND"].iter().any(|prefix| {
                model_type.strip_prefix(prefix).is_some_and(|suffix| {
                    suffix
                        .parse::<u8>()
                        .is_ok_and(|inputs| (2..=9).contains(&inputs))
                })
            });
            if numbered_logic
                || matches!(
                    model_type.as_str(),
                    "VCSW"
                        | "ICSW"
                        | "VCQPOS"
                        | "VCQNEG"
                        | "ICQPOS"
                        | "ICQNEG"
                        | "VPWLR"
                        | "IPWLR"
                        | "PWLL"
                        | "PWLC"
                        | "INV"
                        | "COMP"
                        | "XOR"
                        | "SRFF"
                        | "CLK_SRFF"
                        | "CLK_JKFF"
                        | "CLK_DFF"
                        | "CLK_TFF"
                        | "LATCH"
                        | "DIGITAL1"
                        | "D_LOGIC_BLOCK"
                        | "ADC_SCHMITT"
                )
            {
                return Some("unlowered SIMPLIS model type");
            }
        }
    }
    None
}

fn reject_residual_simplis(output: &str) -> Result<()> {
    for (index, line) in output.lines().enumerate() {
        if let Some(reason) = residual_simplis_reason(line) {
            bail!(
                "SIMPLIS translation left {reason} at output line {}: {}",
                index.saturating_add(1),
                line.trim()
            );
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RawPwlKind {
    Vpwlr,
    Ipwlr,
}

#[derive(Clone, Debug)]
struct RawPwlModel {
    kind: RawPwlKind,
    points: Vec<Point>,
}

#[derive(Clone, Debug)]
struct RawReactiveModel {
    points: Vec<Point>,
    kind: PassiveKind,
}

fn raw_reactive_model(statement: &str) -> Result<Option<(String, RawReactiveModel)>> {
    let trimmed = statement.trim_start();
    if !trimmed
        .get(..6)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(".MODEL"))
        || !trimmed.get(6..7).is_some_and(str::is_empty_or_whitespace)
    {
        return Ok(None);
    }
    let tokens = statement_tokens(&normalized_statement(statement))?;
    if tokens.len() < 4 {
        return Ok(None);
    }
    let kind = match tokens[2].to_ascii_uppercase().as_str() {
        "PWLC" => PassiveKind::Capacitor,
        "PWLL" => PassiveKind::Inductor,
        _ => return Ok(None),
    };
    let parameters = parse_assignments(&tokens[3..])?;
    let segments = integer_parameter(&parameters, "NSEG")?;
    if segments == 0 {
        bail!("SIMPLIS reactive model {} has zero segments", tokens[1]);
    }
    let points = indexed_points(&parameters, segments, "X", "Y")
        .with_context(|| format!("parsing SIMPLIS reactive model {}", tokens[1]))?;
    Ok(Some((
        tokens[1].to_ascii_uppercase(),
        RawReactiveModel { points, kind },
    )))
}

fn translate_raw_reactive_device(
    statement: &str,
    models: &std::collections::BTreeMap<String, RawReactiveModel>,
) -> Result<Option<Translation>> {
    let tokens = statement_tokens(&normalized_statement(statement))?;
    let Some(name) = tokens.first() else {
        return Ok(None);
    };
    if !name.get(..2).is_some_and(|prefix| {
        prefix.eq_ignore_ascii_case("!C") || prefix.eq_ignore_ascii_case("!L")
    }) || tokens.len() < 4
    {
        return Ok(None);
    }
    let Some(model) = models.get(&tokens[3].to_ascii_uppercase()) else {
        return Ok(None);
    };
    let flux = name
        .get(..2)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("!L"));
    anyhow::ensure!(
        (model.kind == PassiveKind::Inductor) == flux,
        "reactive device {name} has mismatched model type"
    );
    anyhow::ensure!(
        model
            .points
            .windows(2)
            .all(|pair| pair[1].value > pair[0].value),
        "SIMPLIS reactive model requires positive differential storage"
    );
    let parameters = parse_assignments(&tokens[4..])?;
    anyhow::ensure!(
        parameters.keys().all(|key| key == "IC"),
        "unsupported reactive instance parameter on {name}"
    );
    let initial = parameters
        .get("IC")
        .map(|value| crate::parse_numeric(value))
        .transpose()?;
    let instance = name.trim_start_matches('!');
    if model.kind == PassiveKind::Inductor {
        translate_flux_inductor(instance, &tokens[1], &tokens[2], &model.points, initial).map(Some)
    } else {
        translate_charge_capacitor(instance, &tokens[1], &tokens[2], &model.points, initial)
            .map(Some)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MosPolarity {
    Nmos,
    Pmos,
}

#[derive(Clone, Debug)]
struct RawLevel17Mos {
    polarity: MosPolarity,
    vto: f64,
    kp: f64,
    rs: f64,
    rd: f64,
    tc1rd: f64,
    tc2rd: f64,
    rg: f64,
    isat: f64,
    cbd: f64,
    cgdmax: f64,
    cgdmin: f64,
    xg1cgd: f64,
    xg2cgd: f64,
    vtcgd: f64,
    gamma: f64,
    phi: f64,
    lambda: f64,
    vds_limit: Option<String>,
}

fn strip_level17_vds_limit(body: &str) -> Result<(String, Option<String>)> {
    let upper = body.to_ascii_uppercase();
    let Some(start) = upper.find("LIMIT(VDS)=") else {
        return Ok((body.to_owned(), None));
    };
    let value_start = start + "LIMIT(VDS)=".len();
    let open = body[value_start..]
        .find('(')
        .map(|offset| value_start + offset)
        .context("SIMetrix LEVEL=17 LIMIT(VDS) has no opening parenthesis")?;
    let close = body[open + 1..]
        .find(')')
        .map(|offset| open + 1 + offset)
        .context("SIMetrix LEVEL=17 LIMIT(VDS) has no closing parenthesis")?;
    let mut output = String::with_capacity(body.len());
    output.push_str(&body[..start]);
    output.push_str(&body[close + 1..]);
    Ok((
        output,
        Some(
            body[start..=close]
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" "),
        ),
    ))
}

fn optional_numeric(
    parameters: &std::collections::BTreeMap<String, String>,
    name: &str,
    default: f64,
) -> Result<f64> {
    parameters
        .contains_key(name)
        .then(|| numeric_parameter(parameters, name))
        .transpose()
        .map(|value| value.unwrap_or(default))
}

fn raw_level17_model(statement: &str) -> Result<Option<(String, RawLevel17Mos)>> {
    let normalized = normalized_statement(statement);
    let upper = normalized.to_ascii_uppercase();
    if !upper.starts_with(".MODEL ") || !upper.contains("LEVEL=17") {
        return Ok(None);
    }
    let open = normalized
        .find('(')
        .context("SIMetrix LEVEL=17 .MODEL has no parameter list")?;
    let close = normalized
        .rfind(')')
        .context("SIMetrix LEVEL=17 .MODEL has no closing parenthesis")?;
    if close <= open {
        bail!("malformed SIMetrix LEVEL=17 .MODEL parameter list");
    }
    let header = normalized[..open].split_whitespace().collect::<Vec<_>>();
    if header.len() != 3 || !header[0].eq_ignore_ascii_case(".MODEL") {
        bail!("malformed SIMetrix LEVEL=17 .MODEL header");
    }
    let polarity = if header[2].eq_ignore_ascii_case("NMOS") {
        MosPolarity::Nmos
    } else if header[2].eq_ignore_ascii_case("PMOS") {
        MosPolarity::Pmos
    } else {
        return Ok(None);
    };
    let (body, vds_limit) = strip_level17_vds_limit(&normalized[open + 1..close])?;
    let tokens = statement_tokens(&body)?;
    let parameters = parse_assignments(&tokens)?;
    let allowed = [
        "LEVEL", "VTO", "VT0", "KP", "GAMMA", "PHI", "LAMBDA", "RD", "RS", "CBD", "IS", "RG",
        "CGDMAX", "CGDMIN", "XG1CGD", "XG2CGD", "VTCGD", "TC1RD", "TC2RD",
    ];
    if let Some(unknown) = parameters.keys().find(|name| {
        !allowed
            .iter()
            .any(|allowed| name.eq_ignore_ascii_case(allowed))
    }) {
        bail!("unsupported SIMetrix LEVEL=17 MOS parameter {unknown}");
    }
    if integer_parameter(&parameters, "LEVEL")? != 17 {
        return Ok(None);
    }
    let vto = if parameters.contains_key("VTO") {
        numeric_parameter(&parameters, "VTO")?
    } else {
        numeric_parameter(&parameters, "VT0")?
    };
    let model = RawLevel17Mos {
        polarity,
        vto,
        kp: numeric_parameter(&parameters, "KP")?,
        rs: numeric_parameter(&parameters, "RS")?,
        rd: numeric_parameter(&parameters, "RD")?,
        tc1rd: optional_numeric(&parameters, "TC1RD", 0.0)?,
        tc2rd: optional_numeric(&parameters, "TC2RD", 0.0)?,
        rg: numeric_parameter(&parameters, "RG")?,
        isat: numeric_parameter(&parameters, "IS")?,
        cbd: numeric_parameter(&parameters, "CBD")?,
        cgdmax: numeric_parameter(&parameters, "CGDMAX")?,
        cgdmin: numeric_parameter(&parameters, "CGDMIN")?,
        xg1cgd: numeric_parameter(&parameters, "XG1CGD")?,
        xg2cgd: numeric_parameter(&parameters, "XG2CGD")?,
        vtcgd: numeric_parameter(&parameters, "VTCGD")?,
        gamma: optional_numeric(&parameters, "GAMMA", 0.0)?,
        phi: optional_numeric(&parameters, "PHI", 0.6)?,
        lambda: optional_numeric(&parameters, "LAMBDA", 0.0)?,
        vds_limit,
    };
    if !model.vto.is_finite()
        || !model.kp.is_finite()
        || model.kp <= 0.0
        || !model.rs.is_finite()
        || !model.rd.is_finite()
        || !model.tc1rd.is_finite()
        || !model.tc2rd.is_finite()
        || !model.rg.is_finite()
        || model.rg < 0.0
        || !model.isat.is_finite()
        || model.isat < 0.0
        || !model.cbd.is_finite()
        || model.cbd < 0.0
        || !model.cgdmax.is_finite()
        || model.cgdmax < 0.0
        || !model.cgdmin.is_finite()
        || model.cgdmin < 0.0
        || !model.xg1cgd.is_finite()
        || !model.xg2cgd.is_finite()
        || !model.vtcgd.is_finite()
        || !model.gamma.is_finite()
        || !model.phi.is_finite()
        || model.phi <= 0.0
        || !model.lambda.is_finite()
    {
        bail!("invalid SIMetrix LEVEL=17 MOS parameters for {}", header[1]);
    }
    Ok(Some((header[1].to_ascii_uppercase(), model)))
}

fn level17_core_name(name: &str) -> String {
    format!("__L17_{}", sanitized_name(name))
}

fn emit_level17_core_model(name: &str, model: &RawLevel17Mos) -> Result<String> {
    let mut output = String::new();
    if let Some(limit) = &model.vds_limit {
        writeln!(
            output,
            "* ngspice-rs-preprocess: omitted SIMetrix LEVEL=17 {limit} SOA annotation for {name}"
        )?;
    }
    write!(
        output,
        ".model {} {}(level=1 VTO=",
        level17_core_name(name),
        match model.polarity {
            MosPolarity::Nmos => "NMOS",
            MosPolarity::Pmos => "PMOS",
        }
    )?;
    write_number(&mut output, model.vto)?;
    output.push_str(" KP=");
    write_number(&mut output, model.kp)?;
    output.push_str(" RS=");
    write_number(&mut output, model.rs)?;
    output.push_str(" RD=0 IS=");
    write_number(&mut output, model.isat)?;
    output.push_str(" CBD=");
    write_number(&mut output, model.cbd)?;
    output.push_str(" GAMMA=");
    write_number(&mut output, model.gamma)?;
    output.push_str(" PHI=");
    write_number(&mut output, model.phi)?;
    output.push_str(" LAMBDA=");
    write_number(&mut output, model.lambda)?;
    output.push_str(")\n");
    Ok(output)
}

fn write_level17_charge_term(
    output: &mut String,
    capacitance: f64,
    gradient: f64,
    threshold: f64,
    voltage: &str,
) -> Result<()> {
    if capacitance == 0.0 {
        output.push('0');
        return Ok(());
    }
    write_number(output, capacitance)?;
    if gradient == 0.0 {
        write!(output, "*(0.5*{voltage})")?;
        return Ok(());
    }
    let mut gradient_text = String::new();
    write_number(&mut gradient_text, gradient)?;
    let mut threshold_text = String::new();
    write_number(&mut threshold_text, threshold)?;
    let u = format!("(({threshold_text}-{voltage})*{gradient_text})");
    write!(
        output,
        "*(0.5*{voltage}+(({u})*atan({u})-0.5*log(1+({u})^2))/(pi*{gradient_text}))"
    )?;
    Ok(())
}

fn translate_level17_mos(
    statement: &str,
    models: &std::collections::BTreeMap<String, RawLevel17Mos>,
) -> Result<Option<Translation>> {
    let tokens = statement_tokens(&normalized_statement(statement))?;
    let Some(name) = tokens.first() else {
        return Ok(None);
    };
    if !name
        .as_bytes()
        .first()
        .is_some_and(|byte| matches!(byte.to_ascii_uppercase(), b'M'))
        || tokens.len() < 6
    {
        return Ok(None);
    }
    let Some(model) = models.get(&tokens[5].to_ascii_uppercase()) else {
        return Ok(None);
    };
    if tokens.len() != 6 {
        bail!("unsupported SIMetrix LEVEL=17 MOS instance parameters: {statement}");
    }
    for node in &tokens[1..5] {
        validate_node(node)?;
    }
    let stem = sanitized_name(name);
    let drain = if model.rd == 0.0 {
        tokens[1].clone()
    } else {
        format!("__{stem}_DINT")
    };
    let gate = if model.rg == 0.0 {
        tokens[2].clone()
    } else {
        format!("__{stem}_GINT")
    };
    let mut output = String::new();
    if model.rd != 0.0 {
        write!(output, "R__{stem}_RD {} {drain} ", tokens[1])?;
        write_number(&mut output, model.rd)?;
        output.push_str(" tc1=");
        write_number(&mut output, model.tc1rd)?;
        output.push_str(" tc2=");
        write_number(&mut output, model.tc2rd)?;
        output.push('\n');
    }
    if model.rg != 0.0 {
        write!(output, "R__{stem}_RG {} {gate} ", tokens[2])?;
        write_number(&mut output, model.rg)?;
        output.push('\n');
    }
    writeln!(
        output,
        "{name} {drain} {gate} {} {} {} L=1 W=1",
        tokens[3],
        tokens[4],
        level17_core_name(&tokens[5])
    )?;
    let voltage = format!("v({gate},{drain})");
    write!(output, "C__{stem}_CGD {gate} {drain} Q='")?;
    write_level17_charge_term(
        &mut output,
        model.cgdmin,
        model.xg1cgd,
        model.vtcgd,
        &voltage,
    )?;
    output.push('+');
    write_level17_charge_term(
        &mut output,
        model.cgdmax,
        model.xg2cgd,
        model.vtcgd,
        &voltage,
    )?;
    output.push_str("'\n");
    Ok(Some(Translation {
        bytes: output.into_bytes(),
        kind: TranslationKind::SimetrixMosfetLevel17,
    }))
}

#[derive(Clone, Debug)]
struct RawAdcSchmittModel {
    in_low: RuntimeNumeric,
    in_high: RuntimeNumeric,
    rise_delay: RuntimeNumeric,
    fall_delay: RuntimeNumeric,
    init_cond: RuntimeNumeric,
    out_family: Option<String>,
}

fn raw_adc_schmitt_model(statement: &str) -> Result<Option<(String, RawAdcSchmittModel)>> {
    let normalized = normalized_statement(statement);
    let tokens = statement_tokens(&normalized)?;
    if tokens.len() < 3
        || !tokens[0].eq_ignore_ascii_case(".MODEL")
        || !tokens[2].eq_ignore_ascii_case("ADC_SCHMITT")
    {
        return Ok(None);
    }
    let parameters = parse_assignments(&tokens[3..])?;
    let supported = [
        "IN_LOW",
        "IN_HIGH",
        "RISE_DELAY",
        "FALL_DELAY",
        "INIT_COND",
        "OUT_FAMILY",
    ];
    if let Some(parameter) = parameters.keys().find(|name| {
        !supported
            .iter()
            .any(|supported| name.eq_ignore_ascii_case(supported))
    }) {
        bail!("unsupported SIMetrix adc_schmitt parameter {parameter}");
    }
    Ok(Some((
        tokens[1].to_ascii_uppercase(),
        RawAdcSchmittModel {
            in_low: runtime_numeric_or(&parameters, "IN_LOW", 0.1)?,
            in_high: runtime_numeric_or(&parameters, "IN_HIGH", 0.9)?,
            rise_delay: runtime_numeric_or(&parameters, "RISE_DELAY", 1.0e-9)?,
            fall_delay: runtime_numeric_or(&parameters, "FALL_DELAY", 1.0e-9)?,
            init_cond: runtime_numeric_or(&parameters, "INIT_COND", 0.0)?,
            out_family: parameters
                .get("OUT_FAMILY")
                .map(|value| value.trim().trim_matches('\'').trim_matches('"').to_owned()),
        },
    )))
}

fn translate_adc_schmitt_device(
    statement: &str,
    models: &std::collections::BTreeMap<String, RawAdcSchmittModel>,
) -> Result<Option<Translation>> {
    let tokens = statement_tokens(&normalized_statement(statement))?;
    if tokens.len() != 4
        || !tokens
            .first()
            .and_then(|name| name.as_bytes().first())
            .is_some_and(|byte| matches!(*byte, b'A' | b'a'))
    {
        return Ok(None);
    }
    let Some(model) = models.get(&tokens[3].to_ascii_uppercase()) else {
        return Ok(None);
    };
    validate_node(&tokens[1])?;
    validate_node(&tokens[2])?;
    let low = runtime_numeric_expression(&model.in_low)?;
    let high = runtime_numeric_expression(&model.in_high)?;
    let init = runtime_numeric_expression(&model.init_cond)?;
    let stem = sanitized_name(&tokens[0]);
    let conditioned = format!("__{stem}_SCHMITT_IN");
    let analog_state = format!("__{stem}_SCHMITT_STATE");
    let digital_state = format!("__{stem}_SCHMITT_DIGITAL");
    let hyst_model = format!("__{stem}_SCHMITT_HYST");
    let adc_model = format!("__{stem}_SCHMITT_ADC");
    let delay_model = format!("__{stem}_SCHMITT_DELAY");
    let mut output = String::new();
    writeln!(
        output,
        "B__{stem}_SCHMITT_INIT {conditioned} 0 V='(time<=0 ? (({init})>=0.5 ? ({high})+1 : ({low})-1) : v({}))'",
        tokens[1]
    )?;
    writeln!(
        output,
        "A__{stem}_SCHMITT_HYST {conditioned} {analog_state} {hyst_model}"
    )?;
    writeln!(
        output,
        ".model {hyst_model} hyst(in_low={{((({low})+({high}))/2)}} in_high={{((({low})+({high}))/2)}} hyst={{((({high})-({low}))/2)}} out_lower_limit=0 out_upper_limit=1 input_domain=0 fraction=false)"
    )?;
    writeln!(
        output,
        "A__{stem}_SCHMITT_ADC [{analog_state}] [{digital_state}] {adc_model}"
    )?;
    writeln!(
        output,
        ".model {adc_model} adc_bridge(in_low=0.25 in_high=0.75)"
    )?;
    writeln!(
        output,
        "A__{stem}_SCHMITT_DELAY {digital_state} {} {delay_model}",
        tokens[2]
    )?;
    write!(output, ".model {delay_model} d_buffer(rise_delay=")?;
    write_runtime_numeric(&mut output, &model.rise_delay)?;
    output.push_str(" fall_delay=");
    write_runtime_numeric(&mut output, &model.fall_delay)?;
    if let Some(family) = &model.out_family {
        if family.is_empty() || family.chars().any(char::is_whitespace) {
            bail!("invalid SIMetrix adc_schmitt OUT_FAMILY={family}");
        }
        write!(output, " family={family}")?;
    }
    output.push_str(")\n");
    Ok(Some(Translation {
        bytes: output.into_bytes(),
        kind: TranslationKind::Comparator,
    }))
}

#[derive(Clone, Debug)]
struct RawCompModel {
    rin: f64,
    rout: f64,
    hystwd: f64,
    vol: f64,
    voh: f64,
    delay: f64,
}

fn raw_pwl_model(statement: &str) -> Result<Option<(String, RawPwlModel)>> {
    let trimmed = statement.trim_start();
    if !trimmed
        .get(..6)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(".MODEL"))
        || !trimmed.get(6..7).is_some_and(str::is_empty_or_whitespace)
    {
        return Ok(None);
    }
    let normalized = normalized_statement(statement);
    let tokens = statement_tokens(&normalized)?;
    if tokens.len() < 3 {
        bail!("malformed SIMPLIS .MODEL statement {statement}");
    }
    let kind = if tokens[2].eq_ignore_ascii_case("VPWLR") {
        RawPwlKind::Vpwlr
    } else if tokens[2].eq_ignore_ascii_case("IPWLR") {
        RawPwlKind::Ipwlr
    } else {
        return Ok(None);
    };
    let parameters = parse_assignments(&tokens[3..])?;
    let segments = integer_parameter(&parameters, "NSEG")?;
    if segments == 0 {
        bail!("SIMPLIS PWL model {} has zero segments", tokens[1]);
    }
    let points = indexed_points(&parameters, segments, "X", "Y")
        .with_context(|| format!("parsing SIMPLIS PWL model {}", tokens[1]))?;
    Ok(Some((
        tokens[1].to_ascii_uppercase(),
        RawPwlModel { kind, points },
    )))
}

fn raw_comp_model(statement: &str) -> Result<Option<(String, RawCompModel)>> {
    let trimmed = statement.trim_start();
    if !trimmed
        .get(..6)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(".MODEL"))
        || !trimmed.get(6..7).is_some_and(str::is_empty_or_whitespace)
    {
        return Ok(None);
    }
    let normalized = normalized_statement(statement);
    let tokens = statement_tokens(&normalized)?;
    if tokens.len() < 3 || !tokens[2].eq_ignore_ascii_case("COMP") {
        return Ok(None);
    }
    let parameters = parse_assignments(&tokens[3..])?;
    let rin = numeric_parameter(&parameters, "RIN")?;
    let rout = numeric_parameter(&parameters, "ROUT")?;
    let hystwd = numeric_parameter(&parameters, "HYSTWD")?;
    let vol = numeric_parameter(&parameters, "VOL")?;
    let voh = numeric_parameter(&parameters, "VOH")?;
    let delay = if parameters.contains_key("DELAY") {
        numeric_parameter(&parameters, "DELAY")?
    } else {
        0.0
    };
    if !(rin > 0.0 && rout > 0.0 && hystwd >= 0.0 && delay >= 0.0 && voh > vol) {
        bail!("invalid SIMPLIS COMP model {}", tokens[1]);
    }
    Ok(Some((
        tokens[1].to_ascii_uppercase(),
        RawCompModel {
            rin,
            rout,
            hystwd,
            vol,
            voh,
            delay,
        },
    )))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RawQControl {
    Voltage,
    Current,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RawQPolarity {
    Positive,
    Negative,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RawQFlow {
    Unidirectional,
    Bidirectional,
}

#[derive(Clone, Debug)]
struct RawVcqModel {
    control: RawQControl,
    polarity: RawQPolarity,
    vsat: f64,
    rsat: f64,
    roff: f64,
    gain: f64,
    threshold: f64,
    hystwd: f64,
    logic: String,
    level: usize,
    iflow: RawQFlow,
}

fn raw_vcq_model(statement: &str) -> Result<Option<(String, RawVcqModel)>> {
    let trimmed = statement.trim_start();
    if !trimmed
        .get(..6)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(".MODEL"))
        || !trimmed.get(6..7).is_some_and(str::is_empty_or_whitespace)
    {
        return Ok(None);
    }
    let normalized = normalized_statement(statement);
    let tokens = statement_tokens(&normalized)?;
    if tokens.len() < 3 {
        return Ok(None);
    }
    let (control, polarity) = match tokens[2].to_ascii_uppercase().as_str() {
        "VCQPOS" => (RawQControl::Voltage, RawQPolarity::Positive),
        "VCQNEG" => (RawQControl::Voltage, RawQPolarity::Negative),
        "ICQPOS" => (RawQControl::Current, RawQPolarity::Positive),
        "ICQNEG" => (RawQControl::Current, RawQPolarity::Negative),
        _ => return Ok(None),
    };
    let parameters = parse_assignments(&tokens[3..])?;
    let vsat = numeric_parameter(&parameters, "VSAT")?;
    let rsat = numeric_parameter(&parameters, "RSAT")?;
    let roff = numeric_parameter(&parameters, "ROFF")?;
    let gain = numeric_parameter(&parameters, "GAIN")?;
    let threshold = numeric_parameter(&parameters, "TH")?;
    let hystwd = numeric_parameter(&parameters, "HYSTWD")?;
    let level = integer_parameter(&parameters, "LEVEL")?;
    let logic = parameters.get("LOGIC").map_or_else(
        || "POS".to_owned(),
        |value| value.trim_matches('\'').to_ascii_uppercase(),
    );
    let iflow = match parameters
        .get("IFLOW")
        .map(|value| value.trim_matches('\'').to_ascii_uppercase())
        .as_deref()
    {
        None | Some("UNIDIR") => RawQFlow::Unidirectional,
        Some("BIDIR") => RawQFlow::Bidirectional,
        Some(other) => bail!("unsupported SIMPLIS transistor-switch IFLOW={other}"),
    };
    if parameters.keys().any(|name| {
        !matches!(
            name.as_str(),
            "VSAT" | "RSAT" | "ROFF" | "GAIN" | "TH" | "HYSTWD" | "LOGIC" | "LEVEL" | "IFLOW"
        )
    }) {
        bail!(
            "unsupported SIMPLIS transistor-switch model parameter on {}",
            tokens[1]
        );
    }
    if iflow == RawQFlow::Bidirectional && vsat != 0.0 {
        bail!("SIMPLIS IFLOW=BIDIR requires VSAT=0");
    }
    if (polarity == RawQPolarity::Positive && vsat < 0.0)
        || (polarity == RawQPolarity::Negative && vsat > 0.0)
    {
        bail!("SIMPLIS transistor-switch VSAT has the wrong polarity");
    }
    if !(rsat > 0.0 && roff > rsat && gain > 0.0 && hystwd > 0.0) {
        bail!("invalid SIMPLIS transistor-switch model {}", tokens[1]);
    }
    if !matches!(logic.as_str(), "POS" | "NEG") {
        bail!("unsupported SIMPLIS transistor-switch LOGIC={logic}");
    }
    if !matches!(level, 1 | 2) {
        bail!("unsupported SIMPLIS transistor-switch LEVEL={level}");
    }
    Ok(Some((
        tokens[1].to_ascii_uppercase(),
        RawVcqModel {
            control,
            polarity,
            vsat,
            rsat,
            roff,
            gain,
            threshold,
            hystwd,
            logic,
            level,
            iflow,
        },
    )))
}

#[derive(Clone, Debug)]
enum RuntimeNumeric {
    Constant(f64),
    Expression(String),
}

fn runtime_numeric_value(name: &str, value: &str) -> Result<RuntimeNumeric> {
    if let Ok(number) = crate::parse_numeric(value) {
        if !number.is_finite() {
            bail!("non-finite parameter {name}={value}");
        }
        return Ok(RuntimeNumeric::Constant(number));
    }
    if value
        .strip_prefix('{')
        .and_then(|inner| inner.strip_suffix('}'))
        .is_some_and(|inner| !inner.trim().is_empty())
    {
        return Ok(RuntimeNumeric::Expression(value.to_owned()));
    }
    bail!("unsupported runtime numeric parameter {name}={value}")
}

fn runtime_numeric_parameter(
    parameters: &std::collections::BTreeMap<String, String>,
    name: &str,
) -> Result<RuntimeNumeric> {
    let value = parameters
        .get(&name.to_ascii_uppercase())
        .with_context(|| format!("missing SIMPLIS parameter {name}"))?;
    runtime_numeric_value(name, value)
}

fn write_runtime_numeric(output: &mut String, value: &RuntimeNumeric) -> Result<()> {
    match value {
        RuntimeNumeric::Constant(value) => write_number(output, *value),
        RuntimeNumeric::Expression(value) => {
            output.push_str(value);
            Ok(())
        }
    }
}

fn runtime_numeric_or(
    parameters: &std::collections::BTreeMap<String, String>,
    name: &str,
    default: f64,
) -> Result<RuntimeNumeric> {
    if parameters.contains_key(&name.to_ascii_uppercase()) {
        runtime_numeric_parameter(parameters, name)
    } else {
        Ok(RuntimeNumeric::Constant(default))
    }
}

fn runtime_numeric_expression(value: &RuntimeNumeric) -> Result<String> {
    match value {
        RuntimeNumeric::Constant(value) => spice_number(*value),
        RuntimeNumeric::Expression(value) => Ok(value
            .strip_prefix('{')
            .and_then(|inner| inner.strip_suffix('}'))
            .unwrap_or(value)
            .trim()
            .to_owned()),
    }
}

#[derive(Clone, Debug)]
struct SimetrixTimedVswitch {
    ron: RuntimeNumeric,
    roff: RuntimeNumeric,
    vt: RuntimeNumeric,
    vh: RuntimeNumeric,
    ton: RuntimeNumeric,
    toff: RuntimeNumeric,
    td_on: RuntimeNumeric,
    td_off: RuntimeNumeric,
    timetol: RuntimeNumeric,
    gin: RuntimeNumeric,
}

#[derive(Clone, Debug)]
enum VcswTransfer {
    ShortTransition {
        ron: f64,
        roff: f64,
        threshold: f64,
        hystwd: f64,
        logic: String,
    },
    PspiceVariableResistance {
        ron: RuntimeNumeric,
        roff: RuntimeNumeric,
        von: RuntimeNumeric,
        voff: RuntimeNumeric,
    },
    SimetrixTimedHysteresis(SimetrixTimedVswitch),
}

#[derive(Clone, Debug)]
struct RawVcswModel {
    transfer: VcswTransfer,
}

fn parse_vswitch_transfer(
    parameters: &std::collections::BTreeMap<String, String>,
    model_name: &str,
) -> Result<VcswTransfer> {
    let ron = runtime_numeric_or(parameters, "RON", 1.0)?;
    if let RuntimeNumeric::Constant(value) = ron
        && value <= 0.0
    {
        bail!("invalid VSWITCH RON on model {model_name}");
    }
    if !parameters.contains_key("VT") {
        let roff = runtime_numeric_or(parameters, "ROFF", 1.0e6)?;
        if let RuntimeNumeric::Constant(value) = roff
            && value <= 0.0
        {
            bail!("invalid VSWITCH ROFF on model {model_name}");
        }
        return Ok(VcswTransfer::PspiceVariableResistance {
            ron,
            roff,
            von: runtime_numeric_or(parameters, "VON", 1.0)?,
            voff: runtime_numeric_or(parameters, "VOFF", 0.0)?,
        });
    }
    let roff = runtime_numeric_or(parameters, "ROFF", 1.0e12)?;
    if let RuntimeNumeric::Constant(value) = roff
        && value <= 0.0
    {
        bail!("invalid VSWITCH ROFF on model {model_name}");
    }
    let vt = runtime_numeric_parameter(parameters, "VT")?;
    let vh = runtime_numeric_or(parameters, "VH", 1.0)?;
    let ton = runtime_numeric_or(parameters, "TON", 1.0e-9)?;
    let toff = runtime_numeric_or(parameters, "TOFF", 1.0e-9)?;
    let td = runtime_numeric_or(parameters, "TD", 0.0)?;
    let td_on = if parameters.contains_key("TD_ON") {
        runtime_numeric_parameter(parameters, "TD_ON")?
    } else {
        td.clone()
    };
    let td_off = if parameters.contains_key("TD_OFF") {
        runtime_numeric_parameter(parameters, "TD_OFF")?
    } else {
        td
    };
    let gin = runtime_numeric_or(parameters, "GIN", 0.0)?;
    let timetol = runtime_numeric_or(parameters, "TIMETOL", 10.0e-12)?;
    for (name, value, allow_zero) in [
        ("VH", &vh, true),
        ("TON", &ton, false),
        ("TOFF", &toff, false),
        ("TD_ON", &td_on, true),
        ("TD_OFF", &td_off, true),
        ("GIN", &gin, true),
        ("TIMETOL", &timetol, true),
    ] {
        if let RuntimeNumeric::Constant(value) = value
            && (*value < 0.0 || (!allow_zero && *value == 0.0))
        {
            bail!("invalid timed VSWITCH {name} on model {model_name}");
        }
    }
    if let RuntimeNumeric::Constant(value) = timetol
        && value < 1.0e-15
    {
        bail!("timed VSWITCH TIMETOL below 1 fs on {model_name}");
    }
    for name in parameters.keys() {
        if !matches!(
            name.as_str(),
            "RON"
                | "ROFF"
                | "VT"
                | "VH"
                | "TON"
                | "TOFF"
                | "TD"
                | "TD_ON"
                | "TD_OFF"
                | "TIMETOL"
                | "GIN"
        ) {
            bail!("unsupported timed VSWITCH model parameter {name} on {model_name}");
        }
    }
    Ok(VcswTransfer::SimetrixTimedHysteresis(
        SimetrixTimedVswitch {
            ron,
            roff,
            vt,
            vh,
            ton,
            toff,
            td_on,
            td_off,
            timetol,
            gin,
        },
    ))
}

fn raw_vcsw_model(statement: &str) -> Result<Option<(String, RawVcswModel)>> {
    let trimmed = statement.trim_start();
    if !trimmed
        .get(..6)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(".MODEL"))
        || !trimmed.get(6..7).is_some_and(str::is_empty_or_whitespace)
    {
        return Ok(None);
    }
    let normalized = normalized_statement(statement);
    let tokens = model_statement_tokens(&normalized)?;
    if tokens.len() < 3 {
        return Ok(None);
    }
    let kind = tokens[2].to_ascii_uppercase();
    if !matches!(kind.as_str(), "VCSW" | "VSWITCH") {
        return Ok(None);
    }
    let parameters = parse_assignments(&tokens[3..])?;
    let transfer = if kind == "VCSW" {
        let ron = numeric_parameter(&parameters, "RON")?;
        let roff = numeric_parameter(&parameters, "ROFF")?;
        if !(ron > 0.0 && roff > ron) {
            bail!("invalid {kind} model {}", tokens[1]);
        }
        let threshold = numeric_parameter(&parameters, "TH")?;
        let hystwd = numeric_parameter(&parameters, "HYSTWD")?;
        let logic = parameters.get("LOGIC").map_or_else(
            || "POS".to_owned(),
            |value| value.trim_matches('\'').to_ascii_uppercase(),
        );
        if hystwd <= 0.0 {
            bail!("invalid SIMPLIS VCSW model {}", tokens[1]);
        }
        if !matches!(logic.as_str(), "POS" | "NEG") {
            bail!("unsupported SIMPLIS VCSW LOGIC={logic}");
        }
        if parameters
            .keys()
            .any(|name| !matches!(name.as_str(), "RON" | "ROFF" | "TH" | "HYSTWD" | "LOGIC"))
        {
            bail!("unsupported SIMPLIS VCSW model parameter on {}", tokens[1]);
        }
        VcswTransfer::ShortTransition {
            ron,
            roff,
            threshold,
            hystwd,
            logic,
        }
    } else {
        parse_vswitch_transfer(&parameters, &tokens[1])?
    };
    Ok(Some((
        tokens[1].to_ascii_uppercase(),
        RawVcswModel { transfer },
    )))
}

#[derive(Clone, Debug)]
struct RawSrffModel {
    rin: f64,
    rout: f64,
    threshold: f64,
    hystwd: f64,
    vol: f64,
    voh: f64,
    logic: String,
    delay: f64,
}

fn raw_srff_model(statement: &str) -> Result<Option<(String, RawSrffModel)>> {
    let trimmed = statement.trim_start();
    if !trimmed
        .get(..6)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(".MODEL"))
        || !trimmed.get(6..7).is_some_and(str::is_empty_or_whitespace)
    {
        return Ok(None);
    }
    let tokens = statement_tokens(&normalized_statement(statement))?;
    if tokens.len() < 3 || !tokens[2].eq_ignore_ascii_case("SRFF") {
        return Ok(None);
    }
    let parameters = parse_assignments(&tokens[3..])?;
    let model = RawSrffModel {
        rin: numeric_parameter(&parameters, "RIN")?,
        rout: numeric_parameter(&parameters, "ROUT")?,
        threshold: numeric_parameter(&parameters, "TH")?,
        hystwd: numeric_parameter(&parameters, "HYSTWD")?,
        vol: numeric_parameter(&parameters, "VOL")?,
        voh: numeric_parameter(&parameters, "VOH")?,
        delay: if parameters.contains_key("DELAY") {
            numeric_parameter(&parameters, "DELAY")?
        } else {
            0.0
        },
        logic: parameters.get("LOGIC").map_or_else(
            || "POS".to_owned(),
            |value| value.trim_matches('\'').to_ascii_uppercase(),
        ),
    };
    if !(model.rin > 0.0
        && model.rout > 0.0
        && model.hystwd >= 0.0
        && model.delay >= 0.0
        && model.voh > model.vol)
    {
        bail!("invalid SIMPLIS SRFF model {}", tokens[1]);
    }
    Ok(Some((tokens[1].to_ascii_uppercase(), model)))
}

#[derive(Clone, Debug)]
struct RawNumberedLogicModel {
    kind: LogicGateKind,
    polarity: LogicPolarity,
    inputs: usize,
    rin: f64,
    rout: f64,
    threshold: f64,
    hystwd: f64,
    vol: f64,
    voh: f64,
    delay: f64,
}

fn numbered_logic_kind(token: &str) -> Option<(LogicGateKind, usize)> {
    let upper = token.to_ascii_uppercase();
    for (prefix, kind) in [
        ("NAND", LogicGateKind::Nand),
        ("NOR", LogicGateKind::Nor),
        ("AND", LogicGateKind::And),
        ("OR", LogicGateKind::Or),
    ] {
        let Some(suffix) = upper.strip_prefix(prefix) else {
            continue;
        };
        let Ok(inputs) = suffix.parse::<usize>() else {
            continue;
        };
        if (2..=9).contains(&inputs) {
            return Some((kind, inputs));
        }
    }
    None
}

fn raw_numbered_logic_model(statement: &str) -> Result<Option<(String, RawNumberedLogicModel)>> {
    let trimmed = statement.trim_start();
    if !trimmed
        .get(..6)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(".MODEL"))
        || !trimmed.get(6..7).is_some_and(str::is_empty_or_whitespace)
    {
        return Ok(None);
    }
    let tokens = statement_tokens(&normalized_statement(statement))?;
    if tokens.len() < 4 {
        return Ok(None);
    }
    let Some((kind, inputs)) = numbered_logic_kind(&tokens[2]) else {
        return Ok(None);
    };
    let parameters = parse_assignments(&tokens[3..])?;
    let rin = numeric_parameter(&parameters, "RIN")?;
    let rout = numeric_parameter(&parameters, "ROUT")?;
    let threshold = numeric_parameter(&parameters, "TH")?;
    let hystwd = numeric_parameter(&parameters, "HYSTWD")?;
    let vol = numeric_parameter(&parameters, "VOL")?;
    let voh = numeric_parameter(&parameters, "VOH")?;
    let delay = if parameters.contains_key("DELAY") {
        numeric_parameter(&parameters, "DELAY")?
    } else {
        0.0
    };
    let logic = parameters.get("LOGIC").map_or_else(
        || "POS".to_owned(),
        |value| value.trim_matches('\'').to_ascii_uppercase(),
    );
    let polarity = LogicPolarity::parse(&logic)?;
    if !(rin > 0.0 && rout > 0.0 && hystwd >= 0.0 && delay >= 0.0 && voh > vol) {
        bail!("invalid numbered SIMPLIS logic model {}", tokens[1]);
    }
    Ok(Some((
        tokens[1].to_ascii_uppercase(),
        RawNumberedLogicModel {
            kind,
            polarity,
            inputs,
            rin,
            rout,
            threshold,
            hystwd,
            vol,
            voh,
            delay,
        },
    )))
}

fn translate_raw_numbered_logic_device(
    statement: &str,
    models: &std::collections::BTreeMap<String, RawNumberedLogicModel>,
) -> Result<Option<Translation>> {
    let tokens = statement_tokens(&normalized_statement(statement))?;
    let Some(name) = tokens.first() else {
        return Ok(None);
    };
    if !name.starts_with('!') || tokens.len() < 6 {
        return Ok(None);
    }
    let Some((model_index, model)) =
        tokens
            .iter()
            .enumerate()
            .skip(3)
            .find_map(|(index, token)| {
                models
                    .get(&token.to_ascii_uppercase())
                    .map(|model| (index, model))
            })
    else {
        return Ok(None);
    };
    if model_index != model.inputs.saturating_add(3) {
        bail!("malformed numbered SIMPLIS logic device {name}");
    }
    let out = tokens[1].clone();
    let rtn = tokens[2].clone();
    let inputs = tokens[3..model_index].to_vec();
    for node in std::iter::once(&out)
        .chain(std::iter::once(&rtn))
        .chain(inputs.iter())
    {
        validate_node(node)?;
    }
    let parameters = parse_assignments(&tokens[model_index + 1..])?;
    let initial = if parameters.contains_key("IC") {
        integer_parameter(&parameters, "IC")?
    } else {
        0
    };
    if initial > 1 {
        bail!("numbered SIMPLIS logic IC={initial} must be 0 or 1");
    }
    emit_logic_gate(&LogicGateSpec {
        kind: model.kind,
        stem: sanitized_name(name),
        out,
        out_bar: None,
        rtn,
        inputs,
        mode: LogicGateMode::Classic {
            polarity: model.polarity,
            initial: initial == 1,
        },
        io: LogicIo {
            rin: model.rin,
            rout: model.rout,
            hystwd: model.hystwd,
            vol: model.vol,
            voh: model.voh,
            delay: model.delay,
            threshold: model.threshold,
        },
    })
    .map(Some)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RawDigitalKind {
    Buffer,
    And,
    Or,
    SrLatch,
}

#[derive(Clone, Debug)]
struct RawDigitalModel {
    kind: RawDigitalKind,
    rin: f64,
    rout: f64,
    threshold: f64,
    hystwd: f64,
    vol: f64,
    voh: f64,
    delay: f64,
    num_inverted_inputs: usize,
    set_reset_level: usize,
    dominance: String,
}

fn raw_digital_model(statement: &str) -> Result<Option<(String, RawDigitalModel)>> {
    let trimmed = statement.trim_start();
    if !trimmed
        .get(..6)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(".MODEL"))
        || !trimmed.get(6..7).is_some_and(str::is_empty_or_whitespace)
    {
        return Ok(None);
    }
    let normalized = normalized_statement(statement);
    let tokens = statement_tokens(&normalized)?;
    if tokens.len() < 4 || !tokens[2].eq_ignore_ascii_case("DIGITAL1") {
        return Ok(None);
    }
    let kind = match tokens[3].to_ascii_uppercase().as_str() {
        "D_BUFFER" => RawDigitalKind::Buffer,
        "D_AND" => RawDigitalKind::And,
        "D_OR" => RawDigitalKind::Or,
        "D_SRLATCH" => RawDigitalKind::SrLatch,
        other => bail!("unsupported SIMPLIS DIGITAL1 model type {other}"),
    };
    let parameters = parse_assignments(&tokens[4..])?;
    let rin = numeric_parameter(&parameters, "RIN")?;
    let rout = numeric_parameter(&parameters, "ROUT")?;
    let threshold = numeric_parameter(&parameters, "TH")?;
    let hystwd = numeric_parameter(&parameters, "HYSTWD")?;
    let vol = numeric_parameter(&parameters, "VOL")?;
    let voh = numeric_parameter(&parameters, "VOH")?;
    let delay = numeric_parameter(&parameters, "OUT_DELAY")?;
    let num_inverted_inputs = if parameters.contains_key("NUM_INVERTED_INPUTS") {
        integer_parameter(&parameters, "NUM_INVERTED_INPUTS")?
    } else {
        0
    };
    let set_reset_level = if parameters.contains_key("SET_RESET_LEVEL") {
        integer_parameter(&parameters, "SET_RESET_LEVEL")?
    } else {
        1
    };
    let dominance = parameters.get("DOM").map_or_else(
        || "NONE".to_owned(),
        |value| value.trim_matches('\'').to_ascii_uppercase(),
    );
    if !(rin > 0.0 && rout > 0.0 && hystwd >= 0.0 && delay >= 0.0 && voh > vol) {
        bail!("invalid SIMPLIS DIGITAL1 model {}", tokens[1]);
    }
    Ok(Some((
        tokens[1].to_ascii_uppercase(),
        RawDigitalModel {
            kind,
            rin,
            rout,
            threshold,
            hystwd,
            vol,
            voh,
            delay,
            num_inverted_inputs,
            set_reset_level,
            dominance,
        },
    )))
}

fn bracket_groups(tokens: &[String]) -> Result<Vec<Vec<String>>> {
    let mut groups = Vec::<Vec<String>>::new();
    let mut current: Option<Vec<String>> = None;
    for token in tokens {
        match token.as_str() {
            "[" => {
                if current.is_some() {
                    bail!("nested bracket group in SIMPLIS DIGITAL1 A-device");
                }
                current = Some(Vec::new());
            }
            "]" => {
                let group = current
                    .take()
                    .context("closing bracket without opener in SIMPLIS DIGITAL1 A-device")?;
                groups.push(group);
            }
            _ => {
                let group = current
                    .as_mut()
                    .context("token outside brackets in SIMPLIS DIGITAL1 A-device")?;
                group.push(token.clone());
            }
        }
    }
    if current.is_some() {
        bail!("unterminated bracket group in SIMPLIS DIGITAL1 A-device");
    }
    Ok(groups)
}

#[derive(Clone, Copy)]
struct SrStateSpec<'a> {
    stem: &'a str,
    out: &'a str,
    out_bar: &'a str,
    rtn: &'a str,
    inputs: &'a [String],
    initial: usize,
    operation: LogicOperation,
    input_polarity: LogicPolarity,
    output_polarity: LogicPolarity,
    kind: TranslationKind,
}

fn emit_sr_state(model: &RawDigitalModel, spec: &SrStateSpec<'_>) -> Result<Translation> {
    let SrStateSpec {
        stem,
        out,
        out_bar,
        rtn,
        inputs,
        initial,
        operation,
        input_polarity,
        output_polarity,
        kind,
    } = *spec;
    if inputs.len() != 2 || initial > 1 {
        bail!("invalid SIMPLIS S-R state element");
    }
    let set = &inputs[0];
    let reset = &inputs[1];
    for node in [out, out_bar, rtn, set, reset] {
        validate_node(node)?;
    }
    let io = LogicIo {
        rin: model.rin,
        rout: model.rout,
        hystwd: model.hystwd,
        vol: model.vol,
        voh: model.voh,
        delay: model.delay,
        threshold: model.threshold,
    };
    let mut output = String::new();
    let inertial = kind == TranslationKind::SrLatch;
    let receiver = if inertial {
        LogicReceiver::digital1(input_polarity)
    } else {
        LogicReceiver {
            polarity: input_polarity,
            initial: false,
        }
    };
    emit_logic_input_loads(&mut output, stem, rtn, inputs, io.rin)?;
    let state = emit_logic_event(
        &mut output,
        stem,
        rtn,
        inputs,
        &[receiver; 2],
        io.thresholds(),
        LogicTiming {
            operation,
            delay: model.delay,
            inertial,
            initial: initial == 1,
        },
    )?;
    let negative = output_polarity.negative();
    emit_logic_output(
        &mut output,
        io,
        stem,
        out,
        rtn,
        &state,
        LogicOutput {
            negative,
            complement: false,
        },
    )?;
    emit_logic_output(
        &mut output,
        io,
        stem,
        out_bar,
        rtn,
        &state,
        LogicOutput {
            negative,
            complement: true,
        },
    )?;
    Ok(Translation {
        bytes: output.into_bytes(),
        kind,
    })
}

fn emit_raw_sr_latch(
    model: &RawDigitalModel,
    stem: &str,
    out: &str,
    out_bar: &str,
    rtn: &str,
    inputs: &[String],
    initial: usize,
) -> Result<Translation> {
    let input_polarity = match model.set_reset_level {
        0 => LogicPolarity::Negative,
        1 => LogicPolarity::Positive,
        other => bail!("invalid SIMPLIS DIGITAL1 D_SRLATCH SET_RESET_LEVEL={other}"),
    };
    let operation = match model.dominance.as_str() {
        "R" => LogicOperation::ResetDominant,
        "S" => LogicOperation::SetDominant,
        "NONE" => LogicOperation::HoldWhenEqual,
        other => bail!("invalid SIMPLIS DIGITAL1 D_SRLATCH DOM={other}"),
    };
    emit_sr_state(
        model,
        &SrStateSpec {
            stem,
            out,
            out_bar,
            rtn,
            inputs,
            initial,
            operation,
            input_polarity,
            output_polarity: LogicPolarity::Positive,
            kind: TranslationKind::SrLatch,
        },
    )
}

fn emit_raw_digital_model(
    model: &RawDigitalModel,
    stem: String,
    out: String,
    out_bar: String,
    rtn: String,
    inputs: Vec<String>,
    initial: Option<usize>,
) -> Result<Translation> {
    match model.kind {
        RawDigitalKind::Buffer => {
            if inputs.len() != 1 {
                bail!("SIMPLIS DIGITAL1 D_BUFFER must have one input");
            }
            emit_digital_buffer(&DigitalBufferSpec {
                stem,
                out,
                out_bar,
                rtn: rtn.clone(),
                input_p: inputs[0].clone(),
                input_n: rtn,
                kind: DigitalBufferKind::Buffer,
                rin: model.rin,
                rout: model.rout,
                hystwd: model.hystwd,
                vol: model.vol,
                voh: model.voh,
                delay: model.delay,
                threshold: model.threshold,
                initial,
            })
        }
        RawDigitalKind::And | RawDigitalKind::Or => {
            if inputs.len() < 2 || model.num_inverted_inputs >= inputs.len() {
                bail!("unsupported SIMPLIS DIGITAL1 logic input/inversion configuration");
            }
            emit_logic_gate(&LogicGateSpec {
                kind: match model.kind {
                    RawDigitalKind::And => LogicGateKind::And,
                    RawDigitalKind::Or => LogicGateKind::Or,
                    RawDigitalKind::Buffer | RawDigitalKind::SrLatch => unreachable!(),
                },
                stem,
                out,
                out_bar: Some(out_bar),
                rtn,
                inputs,
                mode: LogicGateMode::Digital1 {
                    inverted_inputs: model.num_inverted_inputs,
                },
                io: LogicIo {
                    rin: model.rin,
                    rout: model.rout,
                    hystwd: model.hystwd,
                    vol: model.vol,
                    voh: model.voh,
                    delay: model.delay,
                    threshold: model.threshold,
                },
            })
        }
        RawDigitalKind::SrLatch => emit_raw_sr_latch(
            model,
            &stem,
            &out,
            &out_bar,
            &rtn,
            &inputs,
            initial.unwrap_or(0),
        ),
    }
}

fn translate_raw_digital_device(
    statement: &str,
    models: &std::collections::BTreeMap<String, RawDigitalModel>,
) -> Result<Option<Translation>> {
    let normalized = normalized_statement(statement);
    let tokens = statement_tokens(&normalized)?;
    let Some(name) = tokens.first() else {
        return Ok(None);
    };
    if !name
        .as_bytes()
        .first()
        .is_some_and(|byte| matches!(*byte, b'A' | b'a'))
        || tokens.len() < 8
    {
        return Ok(None);
    }
    let Some(model_index) = tokens
        .iter()
        .position(|token| token.to_ascii_uppercase().starts_with("MODEL="))
    else {
        return Ok(None);
    };
    let (_, model_name) = tokens[model_index]
        .split_once('=')
        .context("malformed MODEL= on SIMPLIS DIGITAL1 A-device")?;
    let Some(model) = models.get(&model_name.to_ascii_uppercase()) else {
        return Ok(None);
    };
    if tokens.len() < 4
        || !(tokens[1].eq_ignore_ascii_case("%gnd_ref")
            || tokens[1].eq_ignore_ascii_case("%%gnd_ref"))
    {
        bail!("unexpected SIMPLIS DIGITAL1 A-device ground-reference syntax");
    }
    let rtn = tokens[2].clone();
    validate_node(&rtn)?;
    let groups = bracket_groups(&tokens[3..model_index])?;
    if groups.len() != 2 || groups[0].len() != 2 {
        bail!("SIMPLIS DIGITAL1 A-device must have two outputs and one input group");
    }
    let parameters = parse_assignments(&tokens[model_index + 1..])?;
    let initial = if parameters.contains_key("IC") {
        Some(integer_parameter(&parameters, "IC")?)
    } else {
        None
    };
    if initial.is_some_and(|value| value > 1) {
        bail!("raw SIMPLIS DIGITAL1 IC must be 0 or 1");
    }
    let out = groups[0][0].clone();
    let out_bar = groups[0][1].clone();
    let inputs = groups[1].clone();
    for node in std::iter::once(&out)
        .chain(std::iter::once(&out_bar))
        .chain(inputs.iter())
    {
        validate_node(node)?;
    }
    let stem = sanitized_name(name);
    emit_raw_digital_model(model, stem, out, out_bar, rtn, inputs, initial).map(Some)
}

fn branch_voltage_nodes(
    items: &[Item],
) -> Result<std::collections::BTreeMap<String, (String, String)>> {
    let mut branches = std::collections::BTreeMap::new();
    for statement in electrical_statements(items) {
        let trimmed = statement.trim_start();
        let Some(first) = trimmed.as_bytes().first().copied() else {
            continue;
        };
        let eligible = matches!(
            first.to_ascii_uppercase(),
            b'R' | b'L' | b'C' | b'V' | b'I' | b'E' | b'G' | b'H' | b'F' | b'S' | b'Q'
        ) || trimmed.starts_with("!R")
            || trimmed.starts_with("!r")
            || trimmed.starts_with("!L")
            || trimmed.starts_with("!l")
            || trimmed.starts_with("!C")
            || trimmed.starts_with("!c");
        if !eligible {
            continue;
        }
        let tokens = statement_tokens(&normalized_statement(statement))?;
        if tokens.len() < 3 {
            bail!("malformed two-terminal branch statement {statement}");
        }
        branches.insert(
            tokens[0].to_ascii_uppercase(),
            (tokens[1].clone(), tokens[2].clone()),
        );
    }
    Ok(branches)
}

fn parenthesized_node_pair(token: &str) -> Option<(String, String)> {
    let inner = token.strip_prefix('(')?.strip_suffix(')')?;
    let (positive, negative) = inner.split_once(',')?;
    let positive = positive.trim();
    let negative = negative.trim();
    (!positive.is_empty() && !negative.is_empty())
        .then(|| (positive.to_owned(), negative.to_owned()))
}

fn matching_behavioral_parenthesis(input: &str, open: usize) -> Result<usize> {
    matching_behavioral_delimiter(
        input,
        open,
        '(',
        ')',
        "behavioral expression parenthesis underflow",
        "unterminated behavioral function call",
    )
}

fn behavioral_function_arguments(input: &str) -> Result<Vec<&str>> {
    let mut arguments = Vec::new();
    let mut start = 0usize;
    let mut paren_depth = 0usize;
    let mut brace_depth = 0usize;
    let mut bracket_depth = 0usize;
    let mut quote = false;
    for (index, character) in input.char_indices() {
        if character == '\'' {
            quote = !quote;
            continue;
        }
        if quote {
            continue;
        }
        match character {
            '(' => paren_depth = paren_depth.saturating_add(1),
            ')' => {
                paren_depth = paren_depth
                    .checked_sub(1)
                    .context("behavioral function parenthesis underflow")?;
            }
            '{' => brace_depth = brace_depth.saturating_add(1),
            '}' => {
                brace_depth = brace_depth
                    .checked_sub(1)
                    .context("behavioral function brace underflow")?;
            }
            '[' => bracket_depth = bracket_depth.saturating_add(1),
            ']' => {
                bracket_depth = bracket_depth
                    .checked_sub(1)
                    .context("behavioral function bracket underflow")?;
            }
            ',' if paren_depth == 0 && brace_depth == 0 && bracket_depth == 0 => {
                arguments.push(input[start..index].trim());
                start = index.saturating_add(1);
            }
            _ => {}
        }
    }
    if quote || paren_depth != 0 || brace_depth != 0 || bracket_depth != 0 {
        bail!("unterminated behavioral function argument")
    }
    arguments.push(input[start..].trim());
    Ok(arguments)
}

fn matching_behavioral_bracket(input: &str, open: usize) -> Result<usize> {
    matching_behavioral_delimiter(
        input,
        open,
        '[',
        ']',
        "behavioral TABLE bracket underflow",
        "unterminated SIMetrix TABLE[...] point list",
    )
}

fn lower_simetrix_table_expression(input: &str, open: usize) -> Result<(String, usize)> {
    let close_bracket = matching_behavioral_bracket(input, open)?;
    let values = behavioral_function_arguments(&input[open + 1..close_bracket])?;
    if values.len() < 4 || values.len() % 2 != 0 {
        bail!("SIMetrix TABLE[...] requires at least two x,y point pairs");
    }

    let mut points = Vec::with_capacity(values.len() / 2);
    let mut previous_x = None;
    for pair in values.as_chunks::<2>().0 {
        let x = crate::parse_numeric(pair[0])
            .with_context(|| format!("invalid SIMetrix TABLE x value {}", pair[0]))?;
        let y = crate::parse_numeric(pair[1])
            .with_context(|| format!("invalid SIMetrix TABLE y value {}", pair[1]))?;
        if previous_x.is_some_and(|previous| x <= previous) {
            bail!("SIMetrix TABLE x values must be strictly increasing");
        }
        previous_x = Some(x);
        points.push((x, y));
    }

    let bytes = input.as_bytes();
    let mut argument_open = close_bracket.saturating_add(1);
    while bytes
        .get(argument_open)
        .is_some_and(u8::is_ascii_whitespace)
    {
        argument_open += 1;
    }
    if bytes.get(argument_open) != Some(&b'(') {
        bail!("SIMetrix TABLE[...] has no input expression");
    }
    let close = matching_behavioral_parenthesis(input, argument_open)?;
    let expression = lower_simetrix_behavioral_expression(input[argument_open + 1..close].trim())?;

    let mut point_text = String::new();
    for (index, (x, y)) in points.iter().enumerate() {
        if index != 0 {
            point_text.push(',');
        }
        write_number(&mut point_text, *x)?;
        point_text.push(',');
        write_number(&mut point_text, *y)?;
    }
    let (first_x, _) = points[0];
    let (last_x, _) = points[points.len() - 1];
    let mut lower_bound_text = String::new();
    let mut upper_bound_text = String::new();
    write_number(&mut lower_bound_text, first_x)?;
    write_number(&mut upper_bound_text, last_x)?;

    Ok((
        format!("pwl(max({lower_bound_text},min({upper_bound_text},{expression})),{point_text})"),
        close.saturating_add(1),
    ))
}

fn lower_simetrix_behavioral_expression(input: &str) -> Result<String> {
    lower_simetrix_expression_regions(input, true)
}

fn lower_simetrix_conditional(input: &str, name: &str, open: usize) -> Result<(String, usize)> {
    let close = matching_behavioral_parenthesis(input, open)?;
    let arguments = behavioral_function_arguments(&input[open + 1..close])?;
    let [first, second, third] = arguments.as_slice() else {
        bail!("{name} behavioral function requires exactly three arguments");
    };
    let first = lower_simetrix_behavioral_expression(first)?;
    let second = lower_simetrix_behavioral_expression(second)?;
    let third = lower_simetrix_behavioral_expression(third)?;
    let lowered = if name.eq_ignore_ascii_case("IF") || name.eq_ignore_ascii_case("IFF") {
        format!("(({first}) ? ({second}) : ({third}))")
    } else {
        format!(
            "(({first}) > ({second}) ? ({second}) : (({first}) < ({third}) ? ({third}) : ({first})))"
        )
    };
    Ok((lowered, close + 1))
}

fn source_operator(bytes: &[u8], offset: usize, expression: bool) -> Option<(&'static str, usize)> {
    let next = bytes.get(offset + 1);
    match bytes[offset] {
        b'&' => Some(("&&", 1 + usize::from(next == Some(&b'&')))),
        b'|' => Some(("||", 1 + usize::from(next == Some(&b'|')))),
        b'~' => Some(("!", 1)),
        b'=' if expression
            && !matches!(
                offset.checked_sub(1).and_then(|index| bytes.get(index)),
                Some(b'=' | b'!' | b'<' | b'>')
            )
            && next != Some(&b'=') =>
        {
            Some(("==", 1))
        }
        _ => None,
    }
}

fn lower_simetrix_expression_regions(input: &str, expression: bool) -> Result<String> {
    let bytes = input.as_bytes();
    let mut output = String::with_capacity(input.len());
    let mut offset = 0usize;
    let mut quote = false;
    let mut brace_depth = 0usize;
    while offset < bytes.len() {
        if bytes[offset] == b'\'' {
            quote = !quote;
            output.push('\'');
            offset += 1;
            continue;
        }
        if !quote
            && bytes[offset].is_ascii_alphabetic()
            && (offset == 0
                || !bytes[offset - 1].is_ascii_alphanumeric() && bytes[offset - 1] != b'_')
        {
            let name_start = offset;
            let mut name_end = offset.saturating_add(1);
            while bytes
                .get(name_end)
                .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
            {
                name_end += 1;
            }
            let mut open = name_end;
            while bytes.get(open).is_some_and(u8::is_ascii_whitespace) {
                open += 1;
            }
            let name = &input[name_start..name_end];
            if bytes.get(open) == Some(&b'(')
                && (name.eq_ignore_ascii_case("V") || name.eq_ignore_ascii_case("I"))
            {
                let close = matching_behavioral_parenthesis(input, open)?;
                output.push_str(&input[name_start..=close]);
                offset = close + 1;
                continue;
            }
            if (expression || brace_depth > 0)
                && name.eq_ignore_ascii_case("TEMP")
                && bytes.get(open) != Some(&b'(')
                && bytes.get(open) != Some(&b'=')
            {
                output.push_str("temper");
                offset = name_end;
                continue;
            }
            if name.eq_ignore_ascii_case("TABLE") && bytes.get(open) == Some(&b'[') {
                let (lowered, next) = lower_simetrix_table_expression(input, open)?;
                output.push_str(&lowered);
                offset = next;
                continue;
            }
            if bytes.get(open) == Some(&b'(')
                && (name.eq_ignore_ascii_case("IF")
                    || name.eq_ignore_ascii_case("IFF")
                    || name.eq_ignore_ascii_case("LIMIT"))
            {
                let (lowered, next) = lower_simetrix_conditional(input, name, open)?;
                output.push_str(&lowered);
                offset = next;
                continue;
            }
        }
        let character = input[offset..]
            .chars()
            .next()
            .context("behavioral expression character boundary")?;
        if !quote {
            match character {
                '{' => brace_depth = brace_depth.saturating_add(1),
                '}' => brace_depth = brace_depth.saturating_sub(1),
                _ => {}
            }
        }
        if !quote
            && let Some((operator, width)) =
                source_operator(bytes, offset, expression || brace_depth > 0)
        {
            output.push_str(operator);
            offset += width;
            continue;
        }
        output.push(character);
        offset = offset.saturating_add(character.len_utf8());
    }
    Ok(output)
}

fn lower_simetrix_behavioral_statement(statement: &str) -> Result<String> {
    let trimmed = statement.trim_start();
    if trimmed
        .get(..6)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(".MODEL"))
    {
        return Ok(statement.to_owned());
    }
    let runtime_parameter = trimmed
        .get(..6)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(".PARAM"));
    let runtime_function = trimmed
        .get(..5)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(".FUNC"))
        && trimmed.get(5..6).is_none_or(str::is_empty_or_whitespace);
    let first = trimmed
        .as_bytes()
        .first()
        .copied()
        .map(|byte| byte.to_ascii_uppercase());
    let runtime_device = matches!(
        first,
        Some(
            b'R' | b'L'
                | b'C'
                | b'V'
                | b'I'
                | b'E'
                | b'G'
                | b'B'
                | b'F'
                | b'H'
                | b'S'
                | b'Q'
                | b'M'
                | b'D'
                | b'K'
                | b'A'
        )
    );
    if !runtime_parameter && !runtime_function && !runtime_device {
        return Ok(statement.to_owned());
    }
    if let Some((prefix, expression)) = simetrix_runtime::behavioral_rhs(statement) {
        return Ok(format!(
            "{prefix}{}",
            lower_simetrix_behavioral_expression(expression)?
        ));
    }
    // Only explicit expression regions may contain a temperature symbol on
    // ordinary device cards: terminals and model names are electrical names.
    lower_simetrix_expression_regions(statement, false)
}

fn translate_device_voltage_controlled_source(
    statement: &str,
    branches: &std::collections::BTreeMap<String, (String, String)>,
) -> Result<Option<Translation>> {
    let trimmed = statement.trim_start();
    let Some(kind) = trimmed.as_bytes().first().map(u8::to_ascii_uppercase) else {
        return Ok(None);
    };
    if !matches!(kind, b'E' | b'G') {
        return Ok(None);
    }
    let tokens = statement_tokens(&normalized_statement(statement))?;
    if tokens.len() != 5
        || tokens[1].starts_with('(')
        || tokens[2].ends_with(')')
        || tokens[3].eq_ignore_ascii_case("VALUE")
        || tokens[3].eq_ignore_ascii_case("TABLE")
        || tokens[3]
            .get(..6)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("VALUE="))
        || tokens[3]
            .get(..6)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("TABLE="))
    {
        return Ok(None);
    }
    validate_node(&tokens[1])?;
    validate_node(&tokens[2])?;
    let (control_p, control_n) = if let Some(pair) = parenthesized_node_pair(&tokens[3]) {
        pair
    } else {
        branches
            .get(&tokens[3].to_ascii_uppercase())
            .cloned()
            .with_context(|| format!("SIMPLIS source references unknown branch {}", tokens[3]))?
    };
    validate_node(&control_p)?;
    validate_node(&control_n)?;
    let gain = crate::parse_numeric(&tokens[4])?;
    if !gain.is_finite() {
        bail!("SIMPLIS device-voltage-controlled source has non-finite gain");
    }
    let stem = sanitized_name(&tokens[0]);
    let mut output = String::new();
    write!(
        output,
        "B__{stem} {} {} {}='",
        tokens[1],
        tokens[2],
        if kind == b'E' { "V" } else { "I" }
    )?;
    write_number(&mut output, gain)?;
    writeln!(output, "*v({control_p},{control_n})'")?;
    Ok(Some(Translation {
        bytes: output.into_bytes(),
        kind: TranslationKind::DeviceVoltageControlledSource,
    }))
}

fn current_controlled_source_stem(name: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut output = String::with_capacity(name.len());
    for byte in name.bytes() {
        if byte.is_ascii_alphanumeric() {
            output.push(char::from(byte));
        } else {
            output.push('_');
            output.push(char::from(HEX[usize::from(byte >> 4)]));
            output.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
    output
}

fn translated_current_branches(
    items: &[Item],
    pwl_models: &std::collections::BTreeMap<String, RawPwlModel>,
) -> Result<std::collections::BTreeMap<String, String>> {
    let mut branches = std::collections::BTreeMap::new();
    for item in items {
        let Item::Statement(statement) = item else {
            continue;
        };
        let trimmed = statement.trim_start();
        if trimmed.starts_with('!') {
            let tokens = statement_tokens(&normalized_statement(statement))?;
            if tokens.len() >= 4
                && let Some(model) = pwl_models.get(&tokens[3].to_ascii_uppercase())
            {
                let instance = tokens[0].trim_start_matches('!');
                let branch = match model.kind {
                    RawPwlKind::Vpwlr => format!("B{instance}"),
                    RawPwlKind::Ipwlr => {
                        format!("V__{}_IPWLR_SENSE", sanitized_name(instance))
                    }
                };
                branches.insert(tokens[0].to_ascii_uppercase(), branch);
            }
            continue;
        }
        let Some(first) = trimmed.as_bytes().first().map(u8::to_ascii_uppercase) else {
            continue;
        };
        if first == b'V' {
            let tokens = statement_tokens(&normalized_statement(statement))?;
            if let Some(name) = tokens.first() {
                branches.insert(name.to_ascii_uppercase(), name.clone());
                if let Some(alias) = name.get(2..).filter(|_| {
                    name.get(..2)
                        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("V$"))
                }) {
                    branches.insert(alias.to_ascii_uppercase(), name.clone());
                }
            }
            continue;
        }
        if matches!(first, b'F' | b'H') {
            let tokens = statement_tokens(&normalized_statement(statement))?;
            if let Some(name) = tokens.first() {
                branches.insert(
                    name.to_ascii_uppercase(),
                    format!("B__{}", current_controlled_source_stem(name)),
                );
            }
        }
    }
    Ok(branches)
}

fn poly_dimension(token: &str) -> Option<usize> {
    let upper = token.to_ascii_uppercase();
    upper
        .strip_prefix("POLY(")
        .and_then(|value| value.strip_suffix(')'))
        .and_then(|value| value.parse::<usize>().ok())
}

fn parse_poly_coefficients(tokens: &[String]) -> Result<Vec<f64>> {
    let joined = tokens.join(" ");
    let coefficients = joined
        .split(|character: char| {
            character.is_ascii_whitespace() || matches!(character, ',' | '(' | ')')
        })
        .filter(|token| !token.is_empty())
        .map(crate::parse_numeric)
        .collect::<Result<Vec<_>>>()?;
    if coefficients.is_empty() {
        bail!("PSpice POLY source has no coefficients");
    }
    if coefficients.len() > 256 {
        bail!(
            "PSpice POLY source has too many coefficients: {}",
            coefficients.len()
        );
    }
    if coefficients.iter().any(|value| !value.is_finite()) {
        bail!("PSpice POLY source has non-finite coefficient");
    }
    Ok(coefficients)
}

fn append_poly_monomials(
    monomials: &mut Vec<Vec<usize>>,
    current: &mut Vec<usize>,
    dimension: usize,
    degree: usize,
    start: usize,
    needed: usize,
) {
    if monomials.len() >= needed {
        return;
    }
    if current.len() == degree {
        monomials.push(current.clone());
        return;
    }
    for index in start..dimension {
        current.push(index);
        append_poly_monomials(monomials, current, dimension, degree, index, needed);
        current.pop();
        if monomials.len() >= needed {
            return;
        }
    }
}

fn poly_monomials(dimension: usize, count: usize) -> Result<Vec<Vec<usize>>> {
    if dimension == 0 || dimension > 16 {
        bail!("unsupported PSpice POLY dimension {dimension}");
    }
    let mut monomials = Vec::with_capacity(count);
    let mut current = Vec::new();
    for degree in 1..=32 {
        append_poly_monomials(&mut monomials, &mut current, dimension, degree, 0, count);
        if monomials.len() >= count {
            return Ok(monomials);
        }
    }
    bail!("PSpice POLY expansion exceeds supported degree 32")
}

fn write_poly_expression_with(
    output: &mut String,
    dimension: usize,
    coefficients: &[f64],
    mut write_factor: impl FnMut(&mut String, usize) -> Result<()>,
) -> Result<()> {
    write_number(output, coefficients[0])?;
    let monomials = poly_monomials(dimension, coefficients.len().saturating_sub(1))?;
    for (coefficient, monomial) in coefficients.iter().skip(1).zip(monomials) {
        output.push_str("+(");
        write_number(output, *coefficient)?;
        output.push(')');
        for index in monomial {
            write_factor(output, index)?;
        }
    }
    Ok(())
}

fn write_poly_expression(
    output: &mut String,
    controls: &[String],
    coefficients: &[f64],
) -> Result<()> {
    write_poly_expression_with(output, controls.len(), coefficients, |output, index| {
        write!(output, "*i({})", controls[index])?;
        Ok(())
    })
}

fn write_voltage_poly_expression(
    output: &mut String,
    controls: &[(String, String)],
    coefficients: &[f64],
) -> Result<()> {
    write_poly_expression_with(output, controls.len(), coefficients, |output, index| {
        write!(output, "*v({},{})", controls[index].0, controls[index].1)?;
        Ok(())
    })
}

fn legacy_poly_dimension(tokens: &[String], index: usize) -> Option<(usize, usize)> {
    if !tokens.get(index)?.eq_ignore_ascii_case("POLY") {
        return None;
    }
    let dimension = tokens.get(index.saturating_add(1))?.parse::<usize>().ok()?;
    Some((dimension, index.saturating_add(2)))
}

fn translate_legacy_voltage_poly_source(statement: &str) -> Result<Option<Translation>> {
    let tokens = statement_tokens(&normalized_statement(statement))?;
    let Some(kind) = tokens
        .first()
        .and_then(|name| name.as_bytes().first())
        .copied()
        .map(|byte| byte.to_ascii_uppercase())
    else {
        return Ok(None);
    };
    if !matches!(kind, b'E' | b'G') {
        return Ok(None);
    }
    let Some((dimension, control_start)) = legacy_poly_dimension(&tokens, 3) else {
        return Ok(None);
    };
    if dimension == 0 || dimension > 16 {
        bail!("unsupported PSpice POLY dimension {dimension}");
    }
    let control_tokens = dimension
        .checked_mul(2)
        .context("PSpice voltage POLY control count overflow")?;
    let control_end = control_start
        .checked_add(control_tokens)
        .context("PSpice voltage POLY control index overflow")?;
    if tokens.len() <= control_end {
        bail!(
            "PSpice POLY {dimension} voltage source has too few controls or coefficients: {statement}"
        );
    }
    validate_node(&tokens[1])?;
    validate_node(&tokens[2])?;
    let mut controls = Vec::with_capacity(dimension);
    for pair in tokens[control_start..control_end].as_chunks::<2>().0 {
        validate_node(&pair[0])?;
        validate_node(&pair[1])?;
        controls.push((pair[0].clone(), pair[1].clone()));
    }
    let coefficients = parse_poly_coefficients(&tokens[control_end..])?;
    let stem = sanitized_name(&tokens[0]);
    let mut output = String::new();
    write!(
        output,
        "B__{stem} {} {} {}='",
        tokens[1],
        tokens[2],
        if kind == b'E' { "V" } else { "I" }
    )?;
    write_voltage_poly_expression(&mut output, &controls, &coefficients)?;
    output.push_str("'\n");
    Ok(Some(Translation {
        bytes: output.into_bytes(),
        kind: TranslationKind::DeviceVoltageControlledSource,
    }))
}

fn write_current_controlled_gain(
    output: &mut String,
    tokens: &[String],
    branches: &std::collections::BTreeMap<String, String>,
) -> Result<()> {
    if tokens.len() != 5 {
        bail!(
            "unsupported SIMPLIS current-controlled source syntax: {}",
            tokens.join(" ")
        );
    }
    let control = branches
        .get(&tokens[3].to_ascii_uppercase())
        .map_or(tokens[3].as_str(), String::as_str);
    if let Ok(gain) = crate::parse_numeric(&tokens[4]) {
        if !gain.is_finite() {
            bail!("SIMPLIS current-controlled source has non-finite gain");
        }
        write_number(output, gain)?;
    } else if tokens[4]
        .strip_prefix('{')
        .and_then(|value| value.strip_suffix('}'))
        .is_some_and(|value| !value.trim().is_empty())
    {
        output.push_str(&tokens[4]);
    } else {
        bail!("unsupported current-controlled source gain {}", tokens[4]);
    }
    write!(output, "*i({control})")?;
    Ok(())
}

fn translate_current_controlled_source(
    statement: &str,
    branches: &std::collections::BTreeMap<String, String>,
) -> Result<Option<Translation>> {
    let trimmed = statement.trim_start();
    let Some(kind) = trimmed.as_bytes().first().map(u8::to_ascii_uppercase) else {
        return Ok(None);
    };
    if !matches!(kind, b'F' | b'H') {
        return Ok(None);
    }
    let tokens = statement_tokens(&normalized_statement(statement))?;
    validate_node(
        tokens
            .get(1)
            .context("current-controlled source has no positive node")?,
    )?;
    validate_node(
        tokens
            .get(2)
            .context("current-controlled source has no negative node")?,
    )?;
    let stem = current_controlled_source_stem(
        tokens
            .first()
            .context("current-controlled source has no name")?,
    );
    let mut output = String::new();
    write!(
        output,
        "B__{stem} {} {} {}='",
        tokens[1],
        tokens[2],
        if kind == b'F' { "I" } else { "V" }
    )?;

    let value_expression = tokens.get(3).and_then(|token| {
        token
            .split_once('=')
            .filter(|(key, _)| key.eq_ignore_ascii_case("VALUE"))
            .map(|(_, value)| value)
    });
    let poly = tokens
        .get(3)
        .and_then(|token| poly_dimension(token))
        .map(|dimension| (dimension, 4usize))
        .or_else(|| legacy_poly_dimension(&tokens, 3));
    if let Some(value) = value_expression {
        if tokens.len() != 4 {
            bail!("unsupported PSpice VALUE current-controlled source syntax: {statement}");
        }
        let expression = value
            .strip_prefix('{')
            .and_then(|inner| inner.strip_suffix('}'))
            .context("PSpice VALUE source expression must be braced")?;
        output.push_str(&lower_simetrix_behavioral_expression(expression)?);
    } else if let Some((dimension, control_start)) = poly {
        if dimension == 0 || dimension > 16 {
            bail!("unsupported PSpice POLY dimension {dimension}");
        }
        let control_end = control_start
            .checked_add(dimension)
            .context("PSpice POLY control count overflow")?;
        if tokens.len() <= control_end {
            bail!(
                "PSpice POLY({dimension}) source has too few controls or coefficients: {statement}"
            );
        }
        let controls = tokens[control_start..control_end]
            .iter()
            .map(|control| {
                branches
                    .get(&control.to_ascii_uppercase())
                    .cloned()
                    .unwrap_or_else(|| control.clone())
            })
            .collect::<Vec<_>>();
        let coefficients = parse_poly_coefficients(&tokens[control_end..])?;
        write_poly_expression(&mut output, &controls, &coefficients)?;
    } else {
        write_current_controlled_gain(&mut output, &tokens, branches)?;
    }
    output.push_str("'\n");
    Ok(Some(Translation {
        bytes: output.into_bytes(),
        kind: TranslationKind::CurrentControlledSource,
    }))
}

fn translate_raw_pwl_device(
    statement: &str,
    models: &std::collections::BTreeMap<String, RawPwlModel>,
) -> Result<Option<Translation>> {
    if !statement.trim_start().starts_with('!') {
        return Ok(None);
    }
    let normalized = normalized_statement(statement);
    let tokens = statement_tokens(&normalized)?;
    let Some(name) = tokens.first() else {
        return Ok(None);
    };
    if tokens.len() < 4 {
        return Ok(None);
    }

    let instance = name.trim_start_matches('!');
    let (kind, points) = if let Some(model) = models.get(&tokens[3].to_ascii_uppercase()) {
        (model.kind, model.points.as_slice())
    } else if name
        .as_bytes()
        .get(1)
        .is_some_and(|byte| matches!(*byte, b'R' | b'r'))
        && tokens[3].contains('=')
    {
        let parameters = parse_assignments(&tokens[3..])?;
        let segments = integer_parameter(&parameters, "NSEG")?;
        if segments == 0 {
            bail!("inline SIMPLIS PWL resistor {instance} has zero segments");
        }
        let points = indexed_points(&parameters, segments, "X", "Y")
            .with_context(|| format!("parsing inline SIMPLIS PWL resistor {instance}"))?;
        let translation = translate_vpwlr(instance, &tokens[1], &tokens[2], &points)?;
        return Ok(Some(translation));
    } else {
        return Ok(None);
    };

    let translation = match kind {
        RawPwlKind::Vpwlr => translate_vpwlr(instance, &tokens[1], &tokens[2], points)?,
        RawPwlKind::Ipwlr => translate_ipwlr(instance, &tokens[1], &tokens[2], points)?,
    };
    Ok(Some(translation))
}

fn translate_raw_comp_device(
    statement: &str,
    models: &std::collections::BTreeMap<String, RawCompModel>,
) -> Result<Option<Translation>> {
    if !statement.trim_start().starts_with('!') {
        return Ok(None);
    }
    let normalized = normalized_statement(statement);
    let tokens = statement_tokens(&normalized)?;
    if tokens.len() < 6 {
        return Ok(None);
    }
    let Some(model) = models.get(&tokens[5].to_ascii_uppercase()) else {
        return Ok(None);
    };
    let parameters = parse_assignments(&tokens[6..])?;
    let initial = if parameters.contains_key("IC") {
        integer_parameter(&parameters, "IC")?
    } else {
        0
    };
    if initial > 1 {
        bail!("raw SIMPLIS COMP IC={initial} is invalid");
    }
    let out = &tokens[1];
    let rtn = &tokens[2];
    let input_p = &tokens[3];
    let input_n = &tokens[4];
    for node in [out, rtn, input_p, input_n] {
        validate_node(node)?;
    }
    let stem = sanitized_name(tokens[0].trim_start_matches('!'));
    let diff = format!("__{stem}_DIFF");
    let state_input = format!("__{stem}_STATE_INPUT");
    let state = format!("__{stem}_STATE");
    let delayed = if model.delay == 0.0 {
        state.clone()
    } else {
        format!("__{stem}_DELAYED")
    };
    let out_int = format!("__{stem}_OUT_INT");
    let hyst_model = format!("__{stem}_HYST_M");
    let delay_model = format!("__{stem}_DELAY_M");
    let mut output = String::new();
    write!(output, "R__{stem}_RINP {input_p} {rtn} ")?;
    write_number(&mut output, model.rin)?;
    output.push('\n');
    write!(output, "R__{stem}_RINN {input_n} {rtn} ")?;
    write_number(&mut output, model.rin)?;
    output.push('\n');
    writeln!(output, "E__{stem}_DIFF {diff} 0 {input_p} {input_n} 1")?;
    write!(
        output,
        "B__{stem}_STATE_INPUT {state_input} 0 V='(time<=0 ? "
    )?;
    let initial_force = if initial == 0 {
        -(model.hystwd / 2.0 + 1.0)
    } else {
        model.hystwd / 2.0 + 1.0
    };
    write_number(&mut output, initial_force)?;
    writeln!(output, " : v({diff}))'")?;
    writeln!(output, "A__{stem}_HYST {state_input} {state} {hyst_model}")?;
    write!(output, ".model {hyst_model} hyst(in_low=0 in_high=0 hyst=")?;
    write_number(&mut output, model.hystwd / 2.0)?;
    output.push_str(" out_lower_limit=0 out_upper_limit=1 input_domain=0 fraction=false)\n");
    if model.delay != 0.0 {
        writeln!(output, "A__{stem}_DELAY {state} {delayed} 0 {delay_model}")?;
        write!(output, ".model {delay_model} delay(delay=")?;
        write_number(&mut output, model.delay)?;
        output.push_str(" buffer_size=2048)\n");
    }
    write!(output, "B__{stem}_OUTV {out_int} {rtn} V='")?;
    write_number(&mut output, model.vol)?;
    output.push_str("+(");
    write_number(&mut output, model.voh - model.vol)?;
    writeln!(output, ")*v({delayed})'")?;
    write!(output, "R__{stem}_OUT {out_int} {out} ")?;
    write_number(&mut output, model.rout)?;
    output.push('\n');
    Ok(Some(Translation {
        bytes: output.into_bytes(),
        kind: TranslationKind::Comparator,
    }))
}

fn spice_number(value: f64) -> Result<String> {
    let mut output = String::new();
    write_number(&mut output, value)?;
    Ok(output)
}

fn voltage_switch_control(
    tokens: &[String],
    model_index: usize,
    branches: &std::collections::BTreeMap<String, (String, String)>,
) -> Result<(String, String)> {
    let pair = switch_control_and_model(tokens)
        .filter(|(_, _, index)| *index == model_index)
        .map(|(positive, negative, _)| (positive, negative));
    let (positive, negative) = match pair {
        Some(pair) => pair,
        None if model_index == 4 => branches
            .get(&tokens[3].to_ascii_uppercase())
            .cloned()
            .with_context(|| {
                format!(
                    "SIMPLIS switch references unknown voltage branch {}",
                    tokens[3]
                )
            })?,
        None => bail!("invalid voltage-controlled switch terminal syntax"),
    };
    validate_node(&positive)?;
    validate_node(&negative)?;
    Ok((positive, negative))
}

fn q_control_expression(
    model: &RawVcqModel,
    tokens: &[String],
    model_index: usize,
    scope: &ScopeAnalysis,
) -> Result<String> {
    match model.control {
        RawQControl::Voltage => {
            let (positive, negative) =
                voltage_switch_control(tokens, model_index, &scope.voltage_branches)?;
            Ok(format!("v({positive},{negative})"))
        }
        RawQControl::Current => {
            let control_name = tokens
                .get(3)
                .context("SIMPLIS current-controlled transistor switch has no control branch")?;
            let branch = scope
                .current_branches
                .get(&control_name.to_ascii_uppercase())
                .with_context(|| {
                    format!(
                        "SIMPLIS transistor switch references unknown current branch {control_name}"
                    )
                })?;
            Ok(format!("i({branch})"))
        }
    }
}

fn q_closed_current_expression(
    model: &RawVcqModel,
    control: &str,
    branch_voltage: &str,
) -> Result<(String, String)> {
    let roff = spice_number(model.roff)?;
    let vsat = spice_number(model.vsat)?;
    let rsat = spice_number(model.rsat)?;
    let leak = format!("({branch_voltage}/{roff})");
    let saturated = format!("(({branch_voltage}-{vsat})/{rsat})");
    if model.level == 1 {
        return Ok((leak, saturated));
    }

    let logic_sign = if model.logic == "POS" { 1.0 } else { -1.0 };
    let gain = spice_number(logic_sign * model.gain)?;
    let offset = spice_number(model.threshold - logic_sign * model.hystwd / 2.0)?;
    let amplitude = format!("max(0,{gain}*(({control})-{offset}))");
    let lower = format!("({leak}-{amplitude})");
    let upper = format!("({leak}+{amplitude})");
    let closed = match (model.iflow, model.polarity) {
        (RawQFlow::Bidirectional, _) => format!("max({lower},min({upper},{saturated}))"),
        (RawQFlow::Unidirectional, RawQPolarity::Positive) => {
            format!("max({leak},min({upper},{saturated}))")
        }
        (RawQFlow::Unidirectional, RawQPolarity::Negative) => {
            format!("min({leak},max({lower},{saturated}))")
        }
    };
    Ok((leak, closed))
}

/// Strict Schmitt state shared by simple and transistor switches.
fn emit_switch_state(
    output: &mut String,
    stem: &str,
    control: &str,
    thresholds: LogicThresholds,
    polarity: LogicPolarity,
    closed: bool,
) -> Result<String> {
    let control_node = format!("__{stem}_CTRL");
    writeln!(output, "B__{stem}_CTRL {control_node} 0 V='{control}'")?;
    let state = emit_logic_event(
        output,
        stem,
        "0",
        &[control_node],
        &[LogicReceiver {
            polarity,
            initial: closed,
        }],
        thresholds,
        LogicTiming {
            operation: LogicOperation::And,
            delay: 0.0,
            inertial: true,
            initial: closed,
        },
    )?;
    let voltage = format!("__{stem}_STATE");
    emit_logic_voltage(
        output,
        &format!("{stem}_STATE"),
        &voltage,
        "0",
        &state,
        [0.0, 1.0],
    )?;
    Ok(voltage)
}

fn emit_qswitch(
    model: &RawVcqModel,
    tokens: &[String],
    control: &str,
    closed: bool,
) -> Result<Translation> {
    let stem = sanitized_name(&tokens[0]);
    let mut output = String::new();
    let state = emit_switch_state(
        &mut output,
        &stem,
        control,
        LogicThresholds {
            center: model.threshold,
            width: model.hystwd,
        },
        LogicPolarity::parse(&model.logic)?,
        closed,
    )?;
    let branch_voltage = format!("v({},{})", tokens[1], tokens[2]);
    let (leak, closed_current) = q_closed_current_expression(model, control, &branch_voltage)?;
    writeln!(
        output,
        "B__{stem}_BRANCH {} {} I='(1-v({state}))*{leak}+v({state})*({closed_current})'",
        tokens[1], tokens[2]
    )?;
    Ok(Translation {
        bytes: output.into_bytes(),
        kind: if model.level == 1 {
            TranslationKind::TransistorSwitchLevel1
        } else {
            TranslationKind::TransistorSwitchLevel2
        },
    })
}

fn switch_closed_hint(value: Option<&str>) -> Result<bool> {
    match value
        .context("SIMPLIS switch requires IC=OPEN or IC=CLOSE")?
        .trim_matches('\'')
        .to_ascii_uppercase()
        .as_str()
    {
        "OPEN" => Ok(false),
        "CLOSE" => Ok(true),
        other => bail!("unsupported SIMPLIS switch IC={other}"),
    }
}

fn translate_raw_vcq_device(statement: &str, scope: &ScopeAnalysis) -> Result<Option<Translation>> {
    let tokens = statement_tokens(&normalized_statement(statement))?;
    if !tokens
        .first()
        .and_then(|name| name.as_bytes().first())
        .is_some_and(|byte| matches!(*byte, b'Q' | b'q'))
    {
        return Ok(None);
    }
    let candidate = [5usize, 4].into_iter().find_map(|model_index| {
        tokens
            .get(model_index)
            .and_then(|name| scope.vcq.get(&name.to_ascii_uppercase()))
            .map(|model| (model_index, model))
    });
    let Some((model_index, model)) = candidate else {
        return Ok(None);
    };
    if matches!(model.control, RawQControl::Current) && model_index != 4 {
        bail!("SIMPLIS current-controlled transistor switch requires one control branch");
    }
    for node in &tokens[1..3] {
        validate_node(node)?;
    }
    let parameters = parse_assignments(&tokens[model_index + 1..])?;
    if parameters.keys().any(|name| name != "IC") {
        bail!(
            "unsupported SIMPLIS transistor-switch instance parameter on {}",
            tokens[0]
        );
    }
    let closed = switch_closed_hint(parameters.get("IC").map(String::as_str))?;
    let control = q_control_expression(model, &tokens, model_index, scope)?;
    emit_qswitch(model, &tokens, &control, closed).map(Some)
}

fn switch_control_and_model(tokens: &[String]) -> Option<(String, String, usize)> {
    if tokens.len() < 5 {
        return None;
    }
    if let Some((positive, negative)) = parenthesized_node_pair(&tokens[3]) {
        return Some((positive, negative, 4));
    }
    if tokens.len() >= 6 && tokens[3].starts_with('(') && tokens[4].ends_with(')') {
        let positive = tokens[3]
            .trim_start_matches('(')
            .trim_end_matches(',')
            .trim();
        let negative = tokens[4]
            .trim_start_matches(',')
            .trim_end_matches(')')
            .trim();
        if !positive.is_empty() && !negative.is_empty() {
            return Some((positive.to_owned(), negative.to_owned(), 5));
        }
    }
    (tokens.len() >= 6).then(|| (tokens[3].clone(), tokens[4].clone(), 5))
}

struct ParsedVcswInstance<'a> {
    tokens: &'a [String],
    control_p: &'a str,
    control_n: &'a str,
    parameters: &'a std::collections::BTreeMap<String, String>,
}

fn emit_pspice_vswitch(
    instance: &ParsedVcswInstance<'_>,
    ron: &RuntimeNumeric,
    roff: &RuntimeNumeric,
    von: &RuntimeNumeric,
    voff: &RuntimeNumeric,
) -> Result<Translation> {
    if !instance.parameters.is_empty() {
        bail!(
            "unsupported PSpice VSWITCH instance parameters on {}",
            instance.tokens[0]
        );
    }
    let stem = sanitized_name(&instance.tokens[0]);
    let model_name = format!("__{stem}_ASW");
    let mut output = String::new();
    writeln!(
        output,
        "A__{stem} %vd({} {}) %gd({} {}) {model_name}",
        instance.control_p, instance.control_n, instance.tokens[1], instance.tokens[2]
    )?;
    write!(output, ".model {model_name} aswitch(cntl_off=")?;
    write_runtime_numeric(&mut output, voff)?;
    output.push_str(" cntl_on=");
    write_runtime_numeric(&mut output, von)?;
    output.push_str(" r_off=");
    write_runtime_numeric(&mut output, roff)?;
    output.push_str(" r_on=");
    write_runtime_numeric(&mut output, ron)?;
    output.push_str(" log=TRUE limit=TRUE)\n");
    Ok(Translation {
        bytes: output.into_bytes(),
        kind: TranslationKind::VoltageControlledSwitch,
    })
}

fn emit_simetrix_timed_vswitch(
    instance: &ParsedVcswInstance<'_>,
    model: &SimetrixTimedVswitch,
) -> Result<Translation> {
    if instance.parameters.keys().any(|name| name != "IC") {
        bail!(
            "unsupported timed VSWITCH instance parameters on {}",
            instance.tokens[0]
        );
    }
    let ic = instance.parameters.get("IC").map_or_else(
        || Ok(RuntimeNumeric::Constant(0.0)),
        |value| runtime_numeric_value("IC", value),
    )?;
    if let RuntimeNumeric::Constant(value) = ic
        && !matches!(value, 0.0 | 1.0)
    {
        bail!("timed VSWITCH IC must be 0 or 1");
    }
    let stem = sanitized_name(&instance.tokens[0]);
    let model_name = format!("__{stem}_TIMED_M");
    let mut output = String::new();
    writeln!(
        output,
        "A__{stem}_TIMED %vd({} {}) %gd({} {}) {model_name}",
        instance.control_p, instance.control_n, instance.tokens[1], instance.tokens[2]
    )?;
    write!(output, ".model {model_name} preprocess_timed_switch(")?;
    for (index, (name, value)) in [
        ("ron", &model.ron),
        ("roff", &model.roff),
        ("vt", &model.vt),
        ("vh", &model.vh),
        ("ton", &model.ton),
        ("toff", &model.toff),
        ("td_on", &model.td_on),
        ("td_off", &model.td_off),
        ("timetol", &model.timetol),
        ("ic", &ic),
    ]
    .into_iter()
    .enumerate()
    {
        if index != 0 {
            output.push(' ');
        }
        write!(output, "{name}=")?;
        write_runtime_numeric(&mut output, value)?;
    }
    output.push_str(")\n");
    write!(
        output,
        "G__{stem}_TIMED_GIN {} {} {} {} ",
        instance.control_p, instance.control_n, instance.control_p, instance.control_n
    )?;
    write_runtime_numeric(&mut output, &model.gin)?;
    output.push('\n');
    Ok(Translation {
        bytes: output.into_bytes(),
        kind: TranslationKind::TimedVoltageControlledSwitch,
    })
}

fn emit_simplis_vcsw(
    instance: &ParsedVcswInstance<'_>,
    ron: f64,
    roff: f64,
    threshold: f64,
    hystwd: f64,
    logic: &str,
) -> Result<Translation> {
    if instance.parameters.keys().any(|name| name != "IC") {
        bail!(
            "unsupported SIMPLIS VCSW instance parameter on {}",
            instance.tokens[0]
        );
    }
    let closed = switch_closed_hint(instance.parameters.get("IC").map(String::as_str))?;
    let stem = sanitized_name(&instance.tokens[0]);
    let control = format!("v({},{})", instance.control_p, instance.control_n);
    let mut output = String::new();
    let state = emit_switch_state(
        &mut output,
        &stem,
        &control,
        LogicThresholds {
            center: threshold,
            width: hystwd,
        },
        LogicPolarity::parse(logic)?,
        closed,
    )?;
    let voltage = format!("v({},{})", instance.tokens[1], instance.tokens[2]);
    writeln!(
        output,
        "B__{stem}_BRANCH {} {} I='{voltage}*((1-v({state}))/{}+v({state})/{})'",
        instance.tokens[1],
        instance.tokens[2],
        spice_number(roff)?,
        spice_number(ron)?
    )?;
    Ok(Translation {
        bytes: output.into_bytes(),
        kind: TranslationKind::HystereticSwitch,
    })
}

fn translate_raw_vcsw_device(
    statement: &str,
    scope: &ScopeAnalysis,
) -> Result<Option<Translation>> {
    let tokens = statement_tokens(&normalized_statement(statement))?;
    if !tokens
        .first()
        .and_then(|name| name.as_bytes().first())
        .is_some_and(|byte| matches!(*byte, b'S' | b's'))
    {
        return Ok(None);
    }
    let direct = switch_control_and_model(&tokens).and_then(|(_, _, index)| {
        scope
            .vcsw
            .get(&tokens[index].to_ascii_uppercase())
            .map(|model| (index, model))
    });
    let named = || {
        tokens
            .get(4)
            .and_then(|name| scope.vcsw.get(&name.to_ascii_uppercase()))
            .filter(|model| matches!(model.transfer, VcswTransfer::ShortTransition { .. }))
            .map(|model| (4, model))
    };
    let Some((model_index, model)) = direct.or_else(named) else {
        return Ok(None);
    };
    let (control_p, control_n) =
        voltage_switch_control(&tokens, model_index, &scope.voltage_branches)?;
    for node in [&tokens[1], &tokens[2], &control_p, &control_n] {
        validate_node(node)?;
    }
    let parameters = parse_assignments(&tokens[model_index + 1..])?;
    let instance = ParsedVcswInstance {
        tokens: &tokens,
        control_p: &control_p,
        control_n: &control_n,
        parameters: &parameters,
    };
    let translation = match &model.transfer {
        VcswTransfer::PspiceVariableResistance {
            ron,
            roff,
            von,
            voff,
        } => emit_pspice_vswitch(&instance, ron, roff, von, voff)?,
        VcswTransfer::ShortTransition {
            ron,
            roff,
            threshold,
            hystwd,
            logic,
        } => emit_simplis_vcsw(&instance, *ron, *roff, *threshold, *hystwd, logic)?,
        VcswTransfer::SimetrixTimedHysteresis(model) => {
            emit_simetrix_timed_vswitch(&instance, model)?
        }
    };
    Ok(Some(translation))
}

fn translate_raw_srff_device(
    statement: &str,
    models: &std::collections::BTreeMap<String, RawSrffModel>,
) -> Result<Option<Translation>> {
    if !statement.trim_start().starts_with('!') {
        return Ok(None);
    }
    let tokens = statement_tokens(&normalized_statement(statement))?;
    if tokens.len() < 7 {
        return Ok(None);
    }
    let Some(model) = models.get(&tokens[6].to_ascii_uppercase()) else {
        return Ok(None);
    };
    let polarity = LogicPolarity::parse(&model.logic)?;
    let parameters = parse_assignments(&tokens[7..])?;
    let initial = if parameters.contains_key("IC") {
        integer_parameter(&parameters, "IC")?
    } else {
        0
    };
    let io = RawDigitalModel {
        kind: RawDigitalKind::SrLatch,
        rin: model.rin,
        rout: model.rout,
        threshold: model.threshold,
        hystwd: model.hystwd,
        vol: model.vol,
        voh: model.voh,
        delay: model.delay,
        num_inverted_inputs: 0,
        set_reset_level: 1,
        dominance: "HOLD".to_owned(),
    };
    let stem = sanitized_name(tokens[0].trim_start_matches('!'));
    emit_sr_state(
        &io,
        &SrStateSpec {
            stem: &stem,
            out: &tokens[1],
            out_bar: &tokens[2],
            rtn: &tokens[3],
            inputs: &tokens[4..6],
            initial,
            operation: LogicOperation::HoldWhenEqual,
            input_polarity: polarity,
            output_polarity: polarity,
            kind: TranslationKind::SrFlipFlop,
        },
    )
    .map(Some)
}

fn source_kind(name: &str) -> Result<char> {
    match name.as_bytes().first().map(u8::to_ascii_uppercase) {
        Some(b'V') => Ok('V'),
        Some(b'I') => Ok('I'),
        _ => bail!("SIMPLIS waveform source must be voltage or current source: {name}"),
    }
}

fn no_parameter(parameters: &std::collections::BTreeMap<String, String>, name: &str) -> Result<()> {
    let value = parameters
        .get(name)
        .with_context(|| format!("missing SIMPLIS waveform parameter {name}"))?;
    if !value.eq_ignore_ascii_case("NO") {
        bail!("SIMPLIS waveform {name}={value} is not yet supported");
    }
    Ok(())
}

fn optional_no_parameter(
    parameters: &std::collections::BTreeMap<String, String>,
    name: &str,
) -> Result<()> {
    if let Some(value) = parameters.get(name)
        && !value.eq_ignore_ascii_case("NO")
    {
        bail!("SIMPLIS waveform {name}={value} is not yet supported");
    }
    Ok(())
}

fn phase_expression(delay: f64, period: f64) -> Result<String> {
    let mut delay_text = String::new();
    write_number(&mut delay_text, delay)?;
    let mut period_text = String::new();
    write_number(&mut period_text, period)?;
    Ok(format!(
        "((time-{delay_text})-floor((time-{delay_text})/{period_text})*{period_text})"
    ))
}

struct WaveformHeader<'a> {
    name: &'a str,
    kind: char,
    node_p: &'a str,
    node_n: &'a str,
    waveform: String,
}

fn waveform_header(tokens: &[String]) -> Result<Option<WaveformHeader<'_>>> {
    if tokens.len() < 4 {
        return Ok(None);
    }
    let waveform = tokens[3].to_ascii_uppercase();
    if !matches!(waveform.as_str(), "PUL" | "SAW" | "PWL" | "SIN") {
        return Ok(None);
    }
    let name = tokens[0].as_str();
    let kind = source_kind(name)?;
    let node_p = tokens[1].as_str();
    let node_n = tokens[2].as_str();
    validate_node(node_p)?;
    validate_node(node_n)?;
    Ok(Some(WaveformHeader {
        name,
        kind,
        node_p,
        node_n,
        waveform,
    }))
}

fn write_time_pwl_behavioral(
    output: &mut String,
    kind: char,
    stem: &str,
    node_p: &str,
    node_n: &str,
    points: &[Point],
) -> Result<()> {
    write!(output, "B__{stem} {node_p} {node_n} {kind}='")?;
    write_time_pwl_expr(output, points, 0)?;
    output.push_str("'\n");
    Ok(())
}

fn write_time_pwl_expr(output: &mut String, points: &[Point], index: usize) -> Result<()> {
    let left = points[index];
    let Some(right) = points.get(index.saturating_add(1)).copied() else {
        write_number(output, left.value)?;
        return Ok(());
    };
    if right.voltage.to_bits() == left.voltage.to_bits() {
        return write_time_pwl_expr(output, points, index.saturating_add(1));
    }
    output.push_str("(time<");
    write_number(output, right.voltage)?;
    output.push_str(" ? ");
    if index == 0 {
        output.push_str("(time<");
        write_number(output, left.voltage)?;
        output.push_str(" ? ");
        write_number(output, left.value)?;
        output.push_str(" : ");
    }
    write_number(output, left.value)?;
    output.push_str("+(");
    write_number(output, right.value - left.value)?;
    output.push_str(")*(time-");
    write_number(output, left.voltage)?;
    output.push_str(")/(");
    write_number(output, right.voltage - left.voltage)?;
    output.push(')');
    if index == 0 {
        output.push(')');
    }
    output.push_str(" : ");
    write_time_pwl_expr(output, points, index.saturating_add(1))?;
    output.push(')');
    Ok(())
}

fn translate_time_pwl(
    name: &str,
    node_p: &str,
    node_n: &str,
    parameters: &std::collections::BTreeMap<String, String>,
) -> Result<Translation> {
    let segments = integer_parameter(parameters, "NSEG")?;
    if segments == 0 {
        bail!("SIMPLIS PWL source requires at least one segment");
    }
    let mut points = Vec::with_capacity(segments.saturating_add(1));
    for index in 0..=segments {
        points.push(Point {
            voltage: numeric_parameter(parameters, &format!("X{index}"))?,
            value: numeric_parameter(parameters, &format!("Y{index}"))?,
        });
    }
    if points
        .iter()
        .any(|point| !point.voltage.is_finite() || !point.value.is_finite() || point.voltage < 0.0)
        || points
            .windows(2)
            .any(|window| window[0].voltage > window[1].voltage)
    {
        bail!(
            "SIMPLIS PWL source time coordinates must be finite, non-negative, and nondecreasing"
        );
    }
    let mut output = String::new();
    if points
        .windows(2)
        .any(|window| window[0].voltage.to_bits() == window[1].voltage.to_bits())
    {
        let kind = source_kind(name)?;
        let stem = sanitized_name(name);
        write_time_pwl_behavioral(&mut output, kind, &stem, node_p, node_n, &points)?;
    } else {
        write!(output, "{name} {node_p} {node_n} PWL(")?;
        for (index, point) in points.iter().enumerate() {
            if index != 0 {
                output.push(' ');
            }
            write_number(&mut output, point.voltage)?;
            output.push(' ');
            write_number(&mut output, point.value)?;
        }
        output.push_str(")\n");
    }
    Ok(Translation {
        bytes: output.into_bytes(),
        kind: TranslationKind::PiecewiseLinearSource,
    })
}

fn translate_saw_waveform(
    name: &str,
    kind: char,
    node_p: &str,
    node_n: &str,
    parameters: &std::collections::BTreeMap<String, String>,
) -> Result<Translation> {
    no_parameter(parameters, "OFF_UNTIL_DELAY")?;
    optional_no_parameter(parameters, "IDLE_IN_POP")?;
    let v1 = numeric_parameter(parameters, "V1")?;
    let v2 = numeric_parameter(parameters, "V2")?;
    let frequency = numeric_parameter(parameters, "FREQ")?;
    let delay = numeric_parameter(parameters, "DELAY")?;
    if !(frequency > 0.0 && delay >= 0.0) {
        bail!("invalid SIMPLIS SAW timing parameters");
    }
    let period = 1.0 / frequency;
    let phase = phase_expression(delay, period)?;
    let stem = sanitized_name(name);
    let mut output = String::new();
    write!(output, "B__{stem} {node_p} {node_n} {kind}='")?;
    write_number(&mut output, v1)?;
    output.push_str("+(");
    write_number(&mut output, v2 - v1)?;
    output.push_str(")*(");
    output.push_str(&phase);
    output.push_str("/(");
    write_number(&mut output, period)?;
    output.push_str("))'\n");
    Ok(Translation {
        bytes: output.into_bytes(),
        kind: TranslationKind::SawSource,
    })
}

fn write_native_pulse(name: &str, node_p: &str, node_n: &str, values: [f64; 7]) -> Result<Vec<u8>> {
    let mut output = String::new();
    write!(output, "{name} {node_p} {node_n} PULSE(")?;
    for (index, value) in values.into_iter().enumerate() {
        if index != 0 {
            output.push(' ');
        }
        write_number(&mut output, value)?;
    }
    output.push_str(")\n");
    Ok(output.into_bytes())
}

fn translate_pulse_waveform(
    name: &str,
    kind: char,
    node_p: &str,
    node_n: &str,
    parameters: &std::collections::BTreeMap<String, String>,
) -> Result<Translation> {
    no_parameter(parameters, "OFF_UNTIL_DELAY")?;
    optional_no_parameter(parameters, "IDLE_IN_POP")?;
    let v1 = numeric_parameter(parameters, "V1")?;
    let v2 = numeric_parameter(parameters, "V2")?;
    let frequency = numeric_parameter(parameters, "FREQ")?;
    let rise = numeric_parameter(parameters, "T_RISE")?;
    let fall = numeric_parameter(parameters, "T_FALL")?;
    let width = numeric_parameter(parameters, "PWIDTH")?;
    let delay = numeric_parameter(parameters, "DELAY")?;
    if !(frequency > 0.0 && rise >= 0.0 && fall >= 0.0 && width >= 0.0 && delay >= 0.0) {
        bail!("invalid SIMPLIS PUL timing parameters");
    }
    let period = 1.0 / frequency;
    if rise + width + fall > period {
        bail!("SIMPLIS PUL high/rise/fall interval exceeds its period");
    }
    let bytes = if delay == 0.0 {
        write_native_pulse(
            name,
            node_p,
            node_n,
            [v1, v2, 0.0, rise, fall, width, period],
        )?
    } else {
        if rise == 0.0 || fall == 0.0 {
            bail!("phase-shifted SIMPLIS PUL with zero rise/fall is not yet supported");
        }
        let phase = phase_expression(delay, period)?;
        let stem = sanitized_name(name);
        let mut output = String::new();
        write!(output, "B__{stem} {node_p} {node_n} {kind}=pwl({phase}")?;
        for (x, y) in [
            (0.0, v1),
            (rise, v2),
            (rise + width, v2),
            (rise + width + fall, v1),
            (period, v1),
        ] {
            output.push_str(", ");
            write_number(&mut output, x)?;
            output.push_str(", ");
            write_number(&mut output, y)?;
        }
        output.push_str(")\n");
        output.into_bytes()
    };
    Ok(Translation {
        bytes,
        kind: TranslationKind::PulseSource,
    })
}

fn translate_sine_waveform(
    name: &str,
    kind: char,
    node_p: &str,
    node_n: &str,
    parameters: &std::collections::BTreeMap<String, String>,
) -> Result<Translation> {
    optional_no_parameter(parameters, "IDLE_IN_POP")?;
    let offset = numeric_parameter(parameters, "VOFFSET")?;
    let amplitude = numeric_parameter(parameters, "APEAK")?;
    let frequency = numeric_parameter(parameters, "FREQ")?;
    let damping = parameters
        .get("DAMP_COEF")
        .map_or(Ok(0.0), |value| crate::parse_numeric(value))?;
    if !(amplitude >= 0.0 && frequency > 0.0 && damping >= 0.0) {
        bail!("invalid SIMPLIS SIN amplitude/frequency/damping");
    }
    let time_delay = match (parameters.get("TDELAY"), parameters.get("PDELAY")) {
        (Some(_), Some(_)) => bail!("SIMPLIS SIN TDELAY and PDELAY are mutually exclusive"),
        (Some(value), None) => crate::parse_numeric(value)?,
        (None, Some(value)) => crate::parse_numeric(value)? / (360.0 * frequency),
        (None, None) => bail!("SIMPLIS SIN requires TDELAY or PDELAY"),
    };
    if !time_delay.is_finite() {
        bail!("invalid SIMPLIS SIN delay");
    }
    let off_until_delay = match parameters
        .get("OFF_UNTIL_DELAY")
        .context("missing SIMPLIS waveform parameter OFF_UNTIL_DELAY")?
        .to_ascii_uppercase()
        .as_str()
    {
        "YES" => true,
        "NO" => false,
        value => bail!("SIMPLIS waveform OFF_UNTIL_DELAY={value} is not supported"),
    };

    let stem = sanitized_name(name);
    let mut output = String::new();
    write!(output, "B__{stem} {node_p} {node_n} {kind}='")?;
    if off_until_delay {
        output.push_str("(time<");
        write_number(&mut output, time_delay)?;
        output.push_str(" ? ");
        write_number(&mut output, offset)?;
        output.push_str(" : ");
    }
    write_number(&mut output, offset)?;
    output.push_str("+(");
    write_number(&mut output, amplitude)?;
    output.push_str(")*exp(-(");
    write_number(&mut output, damping)?;
    output.push_str(")*(time-(");
    write_number(&mut output, time_delay)?;
    output.push_str(")))*sin(");
    write_number(&mut output, 2.0 * std::f64::consts::PI * frequency)?;
    output.push_str("*(time-(");
    write_number(&mut output, time_delay)?;
    output.push_str(")))");
    if off_until_delay {
        output.push(')');
    }
    output.push_str("'\n");
    Ok(Translation {
        bytes: output.into_bytes(),
        kind: TranslationKind::SineSource,
    })
}

fn translate_raw_waveform(statement: &str) -> Result<Option<Translation>> {
    let normalized = normalized_statement(statement);
    let tokens = statement_tokens(&normalized)?;
    let Some(header) = waveform_header(&tokens)? else {
        return Ok(None);
    };
    if matches!(header.waveform.as_str(), "PWL" | "SIN")
        && tokens.get(4).is_some_and(|token| !token.contains('='))
    {
        return Ok(None);
    }
    let parameters = parse_assignments(&tokens[4..])?;
    let translation = match header.waveform.as_str() {
        "PWL" => translate_time_pwl(header.name, header.node_p, header.node_n, &parameters)?,
        "SAW" => translate_saw_waveform(
            header.name,
            header.kind,
            header.node_p,
            header.node_n,
            &parameters,
        )?,
        "PUL" => translate_pulse_waveform(
            header.name,
            header.kind,
            header.node_p,
            header.node_n,
            &parameters,
        )?,
        "SIN" => translate_sine_waveform(
            header.name,
            header.kind,
            header.node_p,
            header.node_n,
            &parameters,
        )?,
        _ => unreachable!(),
    };
    Ok(Some(translation))
}

fn raw_inductor_values(items: &[Item]) -> Result<std::collections::BTreeMap<String, f64>> {
    let mut values = std::collections::BTreeMap::new();
    for item in items {
        let Item::Statement(statement) = item else {
            continue;
        };
        let trimmed = statement.trim_start();
        if !trimmed
            .as_bytes()
            .first()
            .is_some_and(|byte| matches!(*byte, b'L' | b'l'))
        {
            continue;
        }
        let normalized = normalized_statement(statement);
        let tokens = statement_tokens(&normalized)?;
        let Some(name) = tokens.first() else {
            continue;
        };
        if tokens.len() < 4
            || validate_node(&tokens[1]).is_err()
            || validate_node(&tokens[2]).is_err()
        {
            continue;
        }
        let value_token = tokens[3]
            .split_once('=')
            .filter(|(parameter, _)| parameter.eq_ignore_ascii_case("L"))
            .map_or(tokens[3].as_str(), |(_, value)| value);
        let Ok(value) = crate::parse_numeric(value_token) else {
            continue;
        };
        if !value.is_finite() {
            bail!("invalid inductance on {name}");
        }
        let key = name.to_ascii_uppercase();
        if values.insert(key.clone(), value).is_some() {
            bail!("duplicate inductor {key} in one SIMPLIS scope");
        }
    }
    Ok(values)
}

fn translate_raw_mutual_inductance(
    statement: &str,
    inductors: &std::collections::BTreeMap<String, f64>,
) -> Result<Option<Translation>> {
    let normalized = normalized_statement(statement);
    let tokens = statement_tokens(&normalized)?;
    if tokens.len() != 2 {
        return Ok(None);
    }
    let Some(pair) = tokens[0]
        .strip_prefix("M-")
        .or_else(|| tokens[0].strip_prefix("m-"))
    else {
        return Ok(None);
    };
    let Some((left_name, right_name)) = pair.split_once('-') else {
        bail!("malformed SIMPLIS mutual inductance name {}", tokens[0]);
    };
    let left_key = left_name.to_ascii_uppercase();
    let right_key = right_name.to_ascii_uppercase();
    let left = inductors
        .get(&left_key)
        .with_context(|| format!("SIMPLIS mutual inductance references missing {left_name}"))?;
    let right = inductors
        .get(&right_key)
        .with_context(|| format!("SIMPLIS mutual inductance references missing {right_name}"))?;
    if *left <= 0.0 || *right <= 0.0 {
        bail!(
            "SIMPLIS mutual inductance requires positive self-inductances: {left_name}={left}, {right_name}={right}"
        );
    }
    let mutual = crate::parse_numeric(&tokens[1])?;
    let coefficient = mutual / (left * right).sqrt();
    if !coefficient.is_finite() || coefficient.abs() > 1.0 {
        bail!("SIMPLIS mutual inductance yields invalid coupling coefficient {coefficient}");
    }
    let stem = sanitized_name(&tokens[0]);
    let mut output = String::new();
    write!(output, "K__{stem} {left_name} {right_name} ")?;
    write_number(&mut output, coefficient)?;
    output.push('\n');
    Ok(Some(Translation {
        bytes: output.into_bytes(),
        kind: TranslationKind::MutualInductance,
    }))
}

#[derive(Clone, Debug)]
struct TransformerWinding {
    p: String,
    n: String,
    turns: f64,
}

fn translate_raw_transformer(statement: &str) -> Result<Option<Translation>> {
    if !statement.trim_start().starts_with("!T") {
        return Ok(None);
    }
    let normalized = normalized_statement(statement);
    let tokens = statement_tokens(&normalized)?;
    if tokens.len() < 5 {
        bail!("malformed SIMPLIS ideal transformer {statement}");
    }
    let Some((nwind_name, nwind_value)) = tokens[1].split_once('=') else {
        bail!("SIMPLIS transformer is missing N_WIND");
    };
    if !nwind_name.eq_ignore_ascii_case("N_WIND") {
        bail!("SIMPLIS transformer first parameter must be N_WIND");
    }
    let winding_count_f = crate::parse_numeric(nwind_value)?;
    if winding_count_f.fract() != 0.0 || !(2.0..=32.0).contains(&winding_count_f) {
        bail!("SIMPLIS transformer N_WIND must be an integer in 2..=32");
    }
    let winding_count: usize = winding_count_f.to_string().parse()?;
    if tokens.len() != 2 + winding_count.saturating_mul(3) {
        bail!("SIMPLIS transformer token count does not match N_WIND");
    }
    let mut windings = Vec::with_capacity(winding_count);
    for index in 0..winding_count {
        let base = 2 + index * 3;
        let p = tokens[base].clone();
        let n = tokens[base + 1].clone();
        validate_node(&p)?;
        validate_node(&n)?;
        let turns_token = &tokens[base + 2];
        let Some((turn_name, turn_value)) = turns_token.split_once('=') else {
            bail!("SIMPLIS transformer winding is missing turns assignment");
        };
        if !turn_name.eq_ignore_ascii_case(&format!("N{}", index + 1)) {
            bail!("SIMPLIS transformer winding turns are out of order");
        }
        let turns = crate::parse_numeric(turn_value)?;
        if !(turns.is_finite() && turns > 0.0) {
            bail!("SIMPLIS transformer turns must be positive");
        }
        windings.push(TransformerWinding { p, n, turns });
    }
    let primary = &windings[0];
    let stem = sanitized_name(tokens[0].trim_start_matches('!'));
    let mut output = String::new();
    for (index, secondary) in windings.iter().enumerate().skip(1) {
        let id = index + 1;
        let ratio = secondary.turns / primary.turns;
        let middle = format!("__{stem}_W{id}_MID");
        let sense = format!("V__{stem}_W{id}_SENSE");
        writeln!(output, "{sense} {} {middle} 0", secondary.p)?;
        write!(
            output,
            "E__{stem}_W{id} {middle} {} {} {} ",
            secondary.n, primary.p, primary.n
        )?;
        write_number(&mut output, ratio)?;
        output.push('\n');
        write!(
            output,
            "F__{stem}_W{id}_PRI {} {} {sense} ",
            primary.n, primary.p
        )?;
        write_number(&mut output, ratio)?;
        output.push('\n');
    }
    Ok(Some(Translation {
        bytes: output.into_bytes(),
        kind: TranslationKind::IdealTransformer,
    }))
}

fn collect_subcircuit_names(
    items: &[Item],
    names: &mut std::collections::BTreeSet<String>,
) -> Result<()> {
    for item in items {
        let Item::Subcircuit(block) = item else {
            continue;
        };
        let name = block.name.to_ascii_uppercase();
        if !names.insert(name.clone()) {
            bail!("duplicate expanded SIMPLIS subcircuit name {name}");
        }
        collect_subcircuit_names(&block.body, names)?;
    }
    Ok(())
}

fn called_subcircuit_name_from_tokens(
    tokens: &[String],
    names: &std::collections::BTreeSet<String>,
) -> Option<String> {
    if !tokens
        .first()
        .and_then(|name| name.as_bytes().first())
        .is_some_and(|byte| matches!(*byte, b'X' | b'x'))
    {
        return None;
    }
    tokens
        .iter()
        .skip(1)
        .rev()
        .map(|token| token.to_ascii_uppercase())
        .find(|token| names.contains(token))
}

#[derive(Clone, Debug, Default)]
struct InitScopeIndex {
    statements: std::collections::BTreeMap<String, Option<String>>,
}

#[derive(Clone, Debug, Default)]
struct InitRoutingIndex {
    root: InitScopeIndex,
    scopes: std::collections::BTreeMap<String, InitScopeIndex>,
    references: std::collections::BTreeMap<String, usize>,
}

fn index_init_scope(
    items: &[Item],
    names: &std::collections::BTreeSet<String>,
    scope_name: Option<&str>,
    index: &mut InitRoutingIndex,
) -> Result<()> {
    let mut scope = InitScopeIndex::default();
    for item in items {
        match item {
            Item::Statement(statement) => {
                let tokens = statement_tokens(&normalized_statement(statement))?;
                let Some(name) = tokens.first() else {
                    continue;
                };
                let target = called_subcircuit_name_from_tokens(&tokens, names);
                if let Some(target) = &target {
                    *index.references.entry(target.clone()).or_default() += 1;
                }
                scope
                    .statements
                    .entry(name.to_ascii_uppercase())
                    .or_insert(target);
            }
            Item::Subcircuit(block) => {
                index_init_scope(&block.body, names, Some(&block.name), index)?;
            }
        }
    }
    if let Some(name) = scope_name {
        index.scopes.insert(name.to_ascii_uppercase(), scope);
    } else {
        index.root = scope;
    }
    Ok(())
}

fn build_init_routing_index(items: &[Item]) -> Result<InitRoutingIndex> {
    let mut names = std::collections::BTreeSet::new();
    collect_subcircuit_names(items, &mut names)?;
    let mut index = InitRoutingIndex::default();
    index_init_scope(items, &names, None, &mut index)?;
    Ok(index)
}

#[derive(Clone, Debug)]
struct RoutedInit {
    scope: Option<String>,
    target: String,
    value: String,
}

fn resolve_init_route(
    index: &InitRoutingIndex,
    name: &str,
    value: &str,
) -> Result<Option<RoutedInit>> {
    let path = model_preprocess_simplis::parse_init_target_path(name)?;
    let Some(first_target) = index.root.statements.get(&path[0].to_ascii_uppercase()) else {
        return Ok(None);
    };
    if path.len() == 1 {
        return Ok(Some(RoutedInit {
            scope: None,
            target: path[0].clone(),
            value: value.to_owned(),
        }));
    }

    let mut scope = first_target.clone().with_context(|| {
        format!(
            "SIMPLIS .INIT hierarchy component {} is not a subcircuit instance",
            path[0]
        )
    })?;
    if index.references.get(&scope).copied().unwrap_or(0) != 1 {
        bail!(
            "SIMPLIS .INIT path {name} enters shared subcircuit {scope}; refusing global mutation"
        );
    }

    for component in &path[1..path.len() - 1] {
        let scope_index = index
            .scopes
            .get(&scope)
            .with_context(|| format!("missing expanded SIMPLIS subcircuit {scope}"))?;
        let Some(target) = scope_index.statements.get(&component.to_ascii_uppercase()) else {
            return Ok(None);
        };
        scope = target.clone().with_context(|| {
            format!(
                "SIMPLIS .INIT hierarchy component {component} in {name} is not a subcircuit instance"
            )
        })?;
        if index.references.get(&scope).copied().unwrap_or(0) != 1 {
            bail!(
                "SIMPLIS .INIT path {name} enters shared subcircuit {scope}; refusing global mutation"
            );
        }
    }

    let scope_index = index
        .scopes
        .get(&scope)
        .with_context(|| format!("missing expanded SIMPLIS subcircuit {scope}"))?;
    let target = path.last().context("empty routed SIMPLIS .INIT path")?;
    if !scope_index
        .statements
        .contains_key(&target.to_ascii_uppercase())
    {
        return Ok(None);
    }
    Ok(Some(RoutedInit {
        scope: Some(scope),
        target: target.clone(),
        value: value.to_owned(),
    }))
}

fn merge_routed_initializations(items: &mut Vec<Item>, ancestor: Vec<String>) -> Result<()> {
    let mut merged = std::collections::BTreeMap::new();
    let mut retained = Vec::with_capacity(items.len());
    for item in std::mem::take(items) {
        if let Item::Statement(statement) = &item
            && let Some(assignments) =
                model_preprocess_simplis::parse_init_statement(&normalized_statement(statement))?
        {
            for (path, value) in assignments {
                let name = path.join(".").to_ascii_uppercase();
                anyhow::ensure!(
                    merged.insert(name.clone(), value).is_none(),
                    "duplicate SIMPLIS .INIT override for {name}"
                );
            }
        } else {
            retained.push(item);
        }
    }
    for statement in ancestor {
        for (path, value) in model_preprocess_simplis::parse_init_statement(&statement)?
            .context("missing routed initialization")?
        {
            merged.insert(path.join(".").to_ascii_uppercase(), value);
        }
    }
    retained.extend(
        merged
            .into_iter()
            .map(|(name, value)| Item::Statement(format!(".INIT {name}={value}"))),
    );
    *items = retained;
    Ok(())
}

fn apply_scoped_init_overrides(
    items: &mut [Item],
    scoped: &mut std::collections::BTreeMap<String, Vec<String>>,
) -> Result<()> {
    for item in items {
        let Item::Subcircuit(block) = item else {
            continue;
        };
        if let Some(statements) = scoped.remove(&block.name.to_ascii_uppercase()) {
            merge_routed_initializations(&mut block.body, statements)?;
        }
        apply_scoped_init_overrides(&mut block.body, scoped)?;
    }
    Ok(())
}

fn apply_routed_init_overrides(items: &mut Vec<Item>, routed: Vec<RoutedInit>) -> Result<()> {
    let mut root = Vec::new();
    let mut scoped = std::collections::BTreeMap::<String, Vec<String>>::new();
    for route in routed {
        let statement = format!(".INIT {}={}", route.target, route.value);
        if let Some(scope) = route.scope {
            scoped.entry(scope).or_default().push(statement);
        } else {
            root.push(statement);
        }
    }

    for statement in root {
        items.insert(0, Item::Statement(statement));
    }
    apply_scoped_init_overrides(items, &mut scoped)?;
    if let Some(scope) = scoped.keys().next() {
        bail!("missing mutable SIMPLIS init scope {scope}");
    }
    Ok(())
}

fn distribute_root_init_overrides(items: &mut Vec<Item>) -> Result<()> {
    let mut init_statements = Vec::new();
    let mut retained = Vec::with_capacity(items.len());
    for item in std::mem::take(items) {
        match item {
            Item::Statement(statement)
                if directive_name(&statement).as_deref() == Some(".INIT") =>
            {
                init_statements.push(statement);
            }
            other => retained.push(other),
        }
    }
    *items = retained;
    if init_statements.is_empty() {
        return Ok(());
    }

    let routing = build_init_routing_index(items)?;

    let mut routed = Vec::new();
    let mut stale = Vec::new();
    for statement in init_statements {
        let assignments =
            model_preprocess_simplis::parse_init_statement(&normalized_statement(&statement))?
                .context("missing .INIT assignments")?;
        for (path, value) in assignments {
            let name = path.join(".");
            if let Some(route) = resolve_init_route(&routing, &name, &value)? {
                routed.push(route);
            } else {
                stale.push(format!("{name}={value}"));
            }
        }
    }

    apply_routed_init_overrides(items, routed)?;
    for entry in stale {
        items.insert(
            0,
            Item::Statement(format!(
                "* ngspice-rs-preprocess: ignored stale SIMPLIS .INIT snapshot entry {entry}"
            )),
        );
    }
    Ok(())
}

fn init_overrides(items: &[Item]) -> Result<std::collections::BTreeMap<String, String>> {
    let mut overrides = std::collections::BTreeMap::new();
    for item in items {
        let Item::Statement(statement) = item else {
            continue;
        };
        if directive_name(statement).as_deref() != Some(".INIT") {
            continue;
        }
        let assignments =
            model_preprocess_simplis::parse_init_statement(&normalized_statement(statement))?
                .context("missing .INIT assignments")?;
        for (path, value) in assignments {
            let [name] = path.as_slice() else {
                bail!(
                    "unrouted hierarchical SIMPLIS .INIT override: {}",
                    path.join(".")
                );
            };
            let name = name.to_ascii_uppercase();
            if overrides.insert(name.clone(), value).is_some() {
                bail!("duplicate SIMPLIS .INIT override for {name}");
            }
        }
    }
    Ok(overrides)
}

fn statement_with_init_override(
    statement: &str,
    overrides: &std::collections::BTreeMap<String, String>,
) -> Result<String> {
    let normalized = normalized_statement(statement);
    let tokens = statement_tokens(&normalized)?;
    let Some(name) = tokens.first() else {
        return Ok(statement.to_owned());
    };
    let Some(value) = overrides.get(&name.to_ascii_uppercase()) else {
        return Ok(statement.to_owned());
    };
    let mut output = Vec::with_capacity(tokens.len().saturating_add(1));
    let mut replaced = false;
    for token in tokens {
        if token
            .split_once('=')
            .is_some_and(|(key, _)| key.eq_ignore_ascii_case("IC"))
        {
            output.push(format!("IC={value}"));
            replaced = true;
        } else {
            output.push(token);
        }
    }
    if !replaced {
        output.push(format!("IC={value}"));
    }
    Ok(output.join(" "))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NativeBridgeKind {
    Adc,
    Dac,
}

#[derive(Clone, Debug)]
struct NativeBridgeModel {
    kind: NativeBridgeKind,
    parameters: std::collections::BTreeMap<String, String>,
}

fn bridge_runtime_numeric(
    parameters: &std::collections::BTreeMap<String, String>,
    name: &str,
    default: f64,
) -> Result<RuntimeNumeric> {
    let Some(value) = parameters.get(name) else {
        return Ok(RuntimeNumeric::Constant(default));
    };
    if let Ok(value) = runtime_numeric_value(name, value) {
        return Ok(value);
    }
    let value = value.trim();
    if !value.is_empty()
        && value.bytes().enumerate().all(|(index, byte)| {
            byte == b'_'
                || byte == b'$'
                || (byte.is_ascii_alphanumeric() && (index != 0 || !byte.is_ascii_digit()))
        })
    {
        return Ok(RuntimeNumeric::Expression(format!("{{{value}}}")));
    }
    bail!("unsupported runtime numeric parameter {name}={value}")
}

fn bridge_ports(header: Option<&str>) -> Result<std::collections::BTreeSet<String>> {
    let Some(header) = header else {
        return Ok(std::collections::BTreeSet::new());
    };
    let tokens = statement_tokens(&normalized_statement(header))?;
    Ok(tokens
        .iter()
        .skip(2)
        .take_while(|token| {
            !token.eq_ignore_ascii_case("params:")
                && !token.eq_ignore_ascii_case("vars:")
                && !token.contains('=')
        })
        .map(|token| token.to_ascii_uppercase())
        .collect())
}

fn native_bridge_model(statement: &str) -> Result<Option<(String, NativeBridgeModel)>> {
    let tokens = statement_tokens(&normalized_statement(statement))?;
    if tokens.len() < 3 || !tokens[0].eq_ignore_ascii_case(".MODEL") {
        return Ok(None);
    }
    let kind = if tokens[2].eq_ignore_ascii_case("ADC_BRIDGE") {
        NativeBridgeKind::Adc
    } else if tokens[2].eq_ignore_ascii_case("DAC_BRIDGE") {
        NativeBridgeKind::Dac
    } else {
        return Ok(None);
    };
    let parameters = parse_assignments(&tokens[3..])?;
    if kind == NativeBridgeKind::Adc {
        const SUPPORTED: &[&str] = &["IN_LOW", "IN_HIGH", "RISE_DELAY", "FALL_DELAY"];
        if let Some(parameter) = parameters.keys().find(|name| {
            !SUPPORTED
                .iter()
                .any(|supported| name.eq_ignore_ascii_case(supported))
        }) {
            bail!(
                "SIMetrix ADC_BRIDGE parameter {parameter} on model {} requires explicit analog-load/clamp compatibility lowering",
                tokens[1]
            );
        }
    }
    if kind == NativeBridgeKind::Dac {
        const SUPPORTED: &[&str] = &[
            "OUT_LOW",
            "OUT_HIGH",
            "G_PULLUP",
            "G_PULLDOWN",
            "G_HIZ",
            "INPUT_LOAD",
            "T_RISE",
            "T_FALL",
            "KNEE_HIGH",
            "KNEE_LOW",
            "V_SMOOTH",
            "IN_FAMILY",
        ];
        if let Some(parameter) = parameters.keys().find(|name| {
            !SUPPORTED
                .iter()
                .any(|supported| name.eq_ignore_ascii_case(supported))
        }) {
            bail!(
                "unsupported SIMetrix DAC_BRIDGE parameter {parameter} on model {}",
                tokens[1]
            );
        }
    }
    Ok(Some((
        tokens[1].to_ascii_uppercase(),
        NativeBridgeModel { kind, parameters },
    )))
}

#[derive(Clone, Debug)]
struct NativeDacBridgeOutput {
    instance: String,
    model: String,
    node: String,
}

fn validate_native_bridge_outputs(
    items: &[Item],
    header: Option<&str>,
    models: &std::collections::BTreeMap<String, NativeBridgeModel>,
) -> Result<()> {
    let ports = bridge_ports(header)?;
    let mut outputs = std::collections::BTreeMap::<String, Vec<NativeDacBridgeOutput>>::new();
    for item in items {
        let Item::Statement(statement) = item else {
            continue;
        };
        let tokens = statement_tokens(&normalized_statement(statement))?;
        if tokens.len() != 4
            || !tokens
                .first()
                .and_then(|name| name.as_bytes().first())
                .is_some_and(|byte| matches!(*byte, b'A' | b'a'))
        {
            continue;
        }
        let Some(model) = models.get(&tokens[3].to_ascii_uppercase()) else {
            continue;
        };
        if model.kind != NativeBridgeKind::Dac {
            continue;
        }
        let node_key = tokens[2].to_ascii_uppercase();
        if ports.contains(&node_key) {
            bail!(
                "SIMetrix DAC_BRIDGE model {} drives loaded or externally visible node {}; exact output-stage lowering is required",
                tokens[3],
                tokens[2]
            );
        }
        outputs
            .entry(node_key)
            .or_default()
            .push(NativeDacBridgeOutput {
                instance: tokens[0].to_ascii_uppercase(),
                model: tokens[3].clone(),
                node: tokens[2].clone(),
            });
    }
    if outputs.is_empty() {
        return Ok(());
    }

    for item in items {
        let Item::Statement(statement) = item else {
            continue;
        };
        let tokens = statement_tokens(&normalized_statement(statement))?;
        let Some(name) = tokens.first() else {
            continue;
        };
        let statement_name = name.to_ascii_uppercase();
        let voltage_control = name
            .as_bytes()
            .first()
            .is_some_and(|byte| matches!(*byte, b'S' | b's'));
        for (index, token) in tokens.iter().enumerate() {
            let Some(candidates) = outputs.get(&token.to_ascii_uppercase()) else {
                continue;
            };
            if voltage_control && matches!(index, 3 | 4) {
                continue;
            }
            for candidate in candidates {
                if statement_name == candidate.instance {
                    continue;
                }
                bail!(
                    "SIMetrix DAC_BRIDGE model {} drives loaded or externally visible node {}; exact output-stage lowering is required",
                    candidate.model,
                    candidate.node
                );
            }
        }
    }
    Ok(())
}

fn rewrite_native_bridge_model(
    statement: &str,
    models: &std::collections::BTreeMap<String, NativeBridgeModel>,
) -> Result<Option<String>> {
    let tokens = statement_tokens(&normalized_statement(statement))?;
    if tokens.len() < 3 || !tokens[0].eq_ignore_ascii_case(".MODEL") {
        return Ok(None);
    }
    let Some(model) = models.get(&tokens[1].to_ascii_uppercase()) else {
        return Ok(None);
    };
    if model.kind == NativeBridgeKind::Adc {
        let in_low = bridge_runtime_numeric(&model.parameters, "IN_LOW", 0.1)?;
        let in_high = bridge_runtime_numeric(&model.parameters, "IN_HIGH", 0.9)?;
        let rise_delay = bridge_runtime_numeric(&model.parameters, "RISE_DELAY", 1.0e-9)?;
        let fall_delay = bridge_runtime_numeric(&model.parameters, "FALL_DELAY", 1.0e-9)?;
        let mut output = format!(".model {} adc_bridge(in_low=", tokens[1]);
        write_runtime_numeric(&mut output, &in_low)?;
        output.push_str(" in_high=");
        write_runtime_numeric(&mut output, &in_high)?;
        output.push_str(" rise_delay=");
        write_runtime_numeric(&mut output, &rise_delay)?;
        output.push_str(" fall_delay=");
        write_runtime_numeric(&mut output, &fall_delay)?;
        output.push(')');
        return Ok(Some(output));
    }
    let out_low = bridge_runtime_numeric(&model.parameters, "OUT_LOW", 0.0)?;
    let out_high = bridge_runtime_numeric(&model.parameters, "OUT_HIGH", 5.0)?;
    let input_load = bridge_runtime_numeric(&model.parameters, "INPUT_LOAD", 1.0e-12)?;
    let t_rise = bridge_runtime_numeric(&model.parameters, "T_RISE", 1.0e-9)?;
    let t_fall = bridge_runtime_numeric(&model.parameters, "T_FALL", 1.0e-9)?;
    let mut output = format!(".model {} dac_bridge(out_low=", tokens[1]);
    write_runtime_numeric(&mut output, &out_low)?;
    output.push_str(" out_high=");
    write_runtime_numeric(&mut output, &out_high)?;
    output.push_str(" input_load=");
    write_runtime_numeric(&mut output, &input_load)?;
    output.push_str(" t_rise=");
    write_runtime_numeric(&mut output, &t_rise)?;
    output.push_str(" t_fall=");
    write_runtime_numeric(&mut output, &t_fall)?;
    output.push(')');
    Ok(Some(output))
}

fn rewrite_native_bridge_instance(
    statement: &str,
    models: &std::collections::BTreeMap<String, NativeBridgeModel>,
) -> Result<Option<String>> {
    let tokens = statement_tokens(&normalized_statement(statement))?;
    if tokens.len() != 4
        || !tokens
            .first()
            .and_then(|name| name.as_bytes().first())
            .is_some_and(|byte| matches!(*byte, b'A' | b'a'))
        || !models.contains_key(&tokens[3].to_ascii_uppercase())
    {
        return Ok(None);
    }
    validate_node(&tokens[1])?;
    validate_node(&tokens[2])?;
    Ok(Some(format!(
        "{} [{}] [{}] {}",
        tokens[0], tokens[1], tokens[2], tokens[3]
    )))
}

struct ScopeAnalysis {
    pwls_prefix: String,
    init: std::collections::BTreeMap<String, String>,
    pwl: std::collections::BTreeMap<String, RawPwlModel>,
    reactive: std::collections::BTreeMap<String, RawReactiveModel>,
    inductors: std::collections::BTreeMap<String, f64>,
    current_branches: std::collections::BTreeMap<String, String>,
    voltage_branches: std::collections::BTreeMap<String, (String, String)>,
    vcq: std::collections::BTreeMap<String, RawVcqModel>,
    comp: std::collections::BTreeMap<String, RawCompModel>,
    adc_schmitt: std::collections::BTreeMap<String, RawAdcSchmittModel>,
    native_bridges: std::collections::BTreeMap<String, NativeBridgeModel>,
    srff: std::collections::BTreeMap<String, RawSrffModel>,
    digital: std::collections::BTreeMap<String, RawDigitalModel>,
    numbered_logic: std::collections::BTreeMap<String, RawNumberedLogicModel>,
    vcsw: std::collections::BTreeMap<String, RawVcswModel>,
    level17: std::collections::BTreeMap<String, RawLevel17Mos>,
    pspice_mos_series_resistances: std::collections::BTreeMap<String, PspiceMosSeriesResistance>,
    simetrix_diode_temperatures: std::collections::BTreeMap<String, f64>,
    simetrix_resistor_temperatures: std::collections::BTreeMap<String, f64>,
}

fn insert_unique_model<T>(
    models: &mut std::collections::BTreeMap<String, T>,
    parsed: Option<(String, T)>,
    label: &str,
) -> Result<()> {
    if let Some((name, model)) = parsed
        && models.insert(name.clone(), model).is_some()
    {
        bail!("duplicate {label} {name} in one scope");
    }
    Ok(())
}

impl ScopeAnalysis {
    fn analyze(items: &[Item], header: Option<&str>) -> Result<Self> {
        let mut models = Self {
            pwls_prefix: pwls::scope_prefix(items, header),
            init: init_overrides(items)?,
            pwl: std::collections::BTreeMap::new(),
            reactive: std::collections::BTreeMap::new(),
            inductors: raw_inductor_values(items)?,
            current_branches: std::collections::BTreeMap::new(),
            voltage_branches: branch_voltage_nodes(items)?,
            vcq: std::collections::BTreeMap::new(),
            comp: std::collections::BTreeMap::new(),
            adc_schmitt: std::collections::BTreeMap::new(),
            native_bridges: std::collections::BTreeMap::new(),
            srff: std::collections::BTreeMap::new(),
            digital: std::collections::BTreeMap::new(),
            numbered_logic: std::collections::BTreeMap::new(),
            vcsw: std::collections::BTreeMap::new(),
            level17: std::collections::BTreeMap::new(),
            pspice_mos_series_resistances: std::collections::BTreeMap::new(),
            simetrix_diode_temperatures: std::collections::BTreeMap::new(),
            simetrix_resistor_temperatures: std::collections::BTreeMap::new(),
        };
        models.collect_raw_models(items)?;
        validate_native_bridge_outputs(items, header, &models.native_bridges)?;
        models.current_branches = translated_current_branches(items, &models.pwl)?;
        Ok(models)
    }

    fn collect_raw_models(&mut self, items: &[Item]) -> Result<()> {
        for statement in electrical_statements(items) {
            let Some((name, kind)) = model_header(statement) else {
                if directive_name(statement).as_deref() == Some(".MODEL") {
                    bail!("malformed SIMPLIS .MODEL statement {statement}");
                }
                continue;
            };
            self.collect_model_compatibility(statement, &name, &kind)?;
            match kind.as_str() {
                "PWLC" | "PWLL" => insert_unique_model(
                    &mut self.reactive,
                    raw_reactive_model(statement)?,
                    "SIMPLIS reactive model",
                )?,
                "VPWLR" | "IPWLR" => insert_unique_model(
                    &mut self.pwl,
                    raw_pwl_model(statement)?,
                    "SIMPLIS PWL model",
                )?,
                "COMP" => insert_unique_model(
                    &mut self.comp,
                    raw_comp_model(statement)?,
                    "SIMPLIS COMP model",
                )?,
                "ADC_SCHMITT" => insert_unique_model(
                    &mut self.adc_schmitt,
                    raw_adc_schmitt_model(statement)?,
                    "SIMetrix adc_schmitt model",
                )?,
                "ADC_BRIDGE" | "DAC_BRIDGE" => insert_unique_model(
                    &mut self.native_bridges,
                    native_bridge_model(statement)?,
                    "mixed-signal bridge model",
                )?,
                "VCQPOS" | "VCQNEG" | "ICQPOS" | "ICQNEG" => insert_unique_model(
                    &mut self.vcq,
                    raw_vcq_model(statement)?,
                    "SIMPLIS transistor-switch model",
                )?,
                "VCSW" | "VSWITCH" => insert_unique_model(
                    &mut self.vcsw,
                    raw_vcsw_model(statement)?,
                    "SIMPLIS VCSW model",
                )?,
                "SRFF" => insert_unique_model(
                    &mut self.srff,
                    raw_srff_model(statement)?,
                    "SIMPLIS SRFF model",
                )?,
                "DIGITAL1" => insert_unique_model(
                    &mut self.digital,
                    raw_digital_model(statement)?,
                    "SIMPLIS DIGITAL1 model",
                )?,
                "NMOS" | "PMOS" => insert_unique_model(
                    &mut self.level17,
                    raw_level17_model(statement)?,
                    "SIMetrix LEVEL=17 MOS model",
                )?,
                numbered if numbered_logic_kind(numbered).is_some() => insert_unique_model(
                    &mut self.numbered_logic,
                    raw_numbered_logic_model(statement)?,
                    "numbered SIMPLIS logic model",
                )?,
                _ => {}
            }
        }
        Ok(())
    }

    fn collect_model_compatibility(
        &mut self,
        statement: &str,
        name: &str,
        kind: &str,
    ) -> Result<()> {
        if matches!(kind, "D" | "R" | "RES")
            && let Some(value) = simple_assignment_value(statement, "T_ABS")
        {
            let temperature = crate::parse_numeric(&value)?;
            let (temperatures, label) = if kind == "D" {
                if !temperature.is_finite() || temperature <= -273.15 {
                    bail!("invalid SIMetrix diode T_ABS={value} on model {name}");
                }
                (&mut self.simetrix_diode_temperatures, "diode")
            } else {
                if !temperature.is_finite() {
                    bail!("invalid SIMetrix resistor T_ABS={value} on model {name}");
                }
                (&mut self.simetrix_resistor_temperatures, "resistor")
            };
            if temperatures.insert(name.to_owned(), temperature).is_some() {
                bail!("duplicate SIMetrix {label} model temperature for {name}");
            }
        }

        if matches!(kind, "NMOS" | "PMOS") {
            let rg = simple_assignment_value(statement, "RG")
                .map(|value| crate::parse_numeric(&value))
                .transpose()?
                .unwrap_or(0.0);
            let rb = simple_assignment_value(statement, "RB")
                .map(|value| crate::parse_numeric(&value))
                .transpose()?
                .unwrap_or(0.0);
            if !(rg.is_finite() && rb.is_finite() && rg >= 0.0 && rb >= 0.0) {
                bail!("invalid PSpice MOS RG/RB on model {name}");
            }
            if (rg != 0.0 || rb != 0.0)
                && self
                    .pspice_mos_series_resistances
                    .insert(name.to_owned(), PspiceMosSeriesResistance { rg, rb })
                    .is_some()
            {
                bail!("duplicate PSpice MOS model parasitics for {name}");
            }
        }
        Ok(())
    }
}

fn inline_schematic_init(tokens: &[String]) -> Result<std::collections::BTreeMap<String, String>> {
    let Some(index) = tokens
        .iter()
        .position(|token| token.eq_ignore_ascii_case("ngspice_preprocess_init:"))
    else {
        return Ok(std::collections::BTreeMap::new());
    };
    parse_assignments(&tokens[index + 1..])
}

fn pin_names_match(actual: &[String], expected: &[&str]) -> bool {
    actual.len() == expected.len()
        && actual
            .iter()
            .zip(expected)
            .all(|(actual, expected)| actual.eq_ignore_ascii_case(expected))
}

fn switch_initial_state(value: Option<&str>, default: &str) -> Result<&'static str> {
    let value = value.unwrap_or(default);
    if value.eq_ignore_ascii_case("CLOSE") {
        Ok("ON")
    } else if value.eq_ignore_ascii_case("OPEN") {
        Ok("OFF")
    } else {
        bail!("legacy SIMPLIS switch initial state must be OPEN or CLOSE, got {value}")
    }
}

fn binary_initial(value: &str, label: &str) -> Result<usize> {
    let mut parameters = std::collections::BTreeMap::new();
    parameters.insert("IC".to_owned(), value.to_owned());
    let initial = integer_parameter(&parameters, "IC")?;
    if initial > 1 {
        bail!("legacy SIMPLIS {label} IC must be 0 or 1");
    }
    Ok(initial)
}

fn translate_inline_comparator(
    name: &str,
    tokens: &[String],
    pinnames_index: usize,
    init: &std::collections::BTreeMap<String, String>,
) -> Result<Translation> {
    if pinnames_index < 5 {
        bail!("legacy SIMPLIS comparator has too few connection nodes");
    }
    let parameters = parse_assignments(&tokens[5..pinnames_index])?;
    let model = RawCompModel {
        rin: numeric_parameter(&parameters, "RIN")?,
        rout: numeric_parameter(&parameters, "ROUT")?,
        hystwd: numeric_parameter(&parameters, "HYSTWD")?,
        vol: numeric_parameter(&parameters, "VOL")?,
        voh: numeric_parameter(&parameters, "VOH")?,
        delay: if parameters.contains_key("DELAY") {
            numeric_parameter(&parameters, "DELAY")?
        } else {
            0.0
        },
    };
    if !(model.rin > 0.0
        && model.rout > 0.0
        && model.hystwd >= 0.0
        && model.delay >= 0.0
        && model.voh > model.vol)
    {
        bail!("invalid legacy SIMPLIS comparator parameters");
    }
    let initial = if let Some(value) = init.get("!DCOMP") {
        binary_initial(value, "comparator")?
    } else if let Some(value) = parameters.get("IC") {
        binary_initial(value, "comparator")?
    } else {
        0
    };
    let mut models = std::collections::BTreeMap::new();
    models.insert("__INLINE_COMP".to_owned(), model);
    let synthetic = format!(
        "!{name} {} {} {} {} __INLINE_COMP IC={initial}",
        tokens[1], tokens[2], tokens[3], tokens[4]
    );
    translate_raw_comp_device(&synthetic, &models)?
        .context("synthetic legacy SIMPLIS comparator did not translate")
}

fn translate_inline_totem(
    name: &str,
    tokens: &[String],
    pinnames_index: usize,
    init: &std::collections::BTreeMap<String, String>,
) -> Result<Translation> {
    if pinnames_index != 6 {
        bail!("legacy SIMPLIS totem output has unexpected parameters");
    }
    let top_state = switch_initial_state(init.get("SW_TOP").map(String::as_str), "OPEN")?;
    let bot_state = switch_initial_state(init.get("SW_BOT").map(String::as_str), "CLOSE")?;
    for node in &tokens[1..6] {
        validate_node(node)?;
    }
    let stem = sanitized_name(name);
    let top_mid = format!("__{stem}_TOP_MID");
    let bot_mid = format!("__{stem}_BOT_MID");
    let top_model = format!("__{stem}_TOP_SW");
    let bot_model = format!("__{stem}_BOT_SW");
    let mut output = String::new();
    writeln!(output, "V__{stem}_SAT_TOP {} {top_mid} 0.1", tokens[1])?;
    writeln!(
        output,
        "S__{stem}_TOP {top_mid} {} {} {} {top_model} {top_state}",
        tokens[2], tokens[4], tokens[5]
    )?;
    writeln!(
        output,
        ".model {top_model} SW(Ron=0.1 Roff=1e7 Vt=2.5 Vh=0.5)"
    )?;
    writeln!(
        output,
        "S__{stem}_BOT {} {bot_mid} {} {} {bot_model} {bot_state}",
        tokens[2], tokens[5], tokens[4]
    )?;
    writeln!(
        output,
        ".model {bot_model} SW(Ron=0.1 Roff=1e7 Vt=-2.5 Vh=0.5)"
    )?;
    writeln!(output, "V__{stem}_SAT_BOT {bot_mid} {} 0.1", tokens[3])?;
    Ok(Translation {
        bytes: output.into_bytes(),
        kind: TranslationKind::VoltageControlledSwitch,
    })
}

fn translate_inline_schematic_x(statement: &str) -> Result<Option<Translation>> {
    let normalized = normalized_statement(statement);
    let tokens = statement_tokens(&normalized)?;
    let Some(name) = tokens.first() else {
        return Ok(None);
    };
    if !name
        .as_bytes()
        .first()
        .is_some_and(|byte| matches!(*byte, b'X' | b'x'))
    {
        return Ok(None);
    }
    let Some(pinnames_index) = tokens
        .iter()
        .position(|token| token.eq_ignore_ascii_case("pinnames:"))
    else {
        return Ok(None);
    };
    let init_index = tokens
        .iter()
        .position(|token| token.eq_ignore_ascii_case("ngspice_preprocess_init:"))
        .unwrap_or(tokens.len());
    if init_index < pinnames_index {
        bail!("legacy SIMPLIS schematic metadata has init state before pinnames");
    }
    let pin_names = &tokens[pinnames_index + 1..init_index];
    let init = inline_schematic_init(&tokens)?;
    if pin_names_match(pin_names, &["OUT", "RTN", "INP", "INN"]) {
        return translate_inline_comparator(name, &tokens, pinnames_index, &init).map(Some);
    }
    if pin_names_match(pin_names, &["TOP", "VOUT", "BOT", "IN", "RTN"]) {
        return translate_inline_totem(name, &tokens, pinnames_index, &init).map(Some);
    }
    Ok(None)
}

fn translate_deck_primitive(
    statement: &str,
    models: &ScopeAnalysis,
) -> Result<Option<Translation>> {
    // Evaluate in source-dispatch precedence order. Neither a successful
    // translation nor an error may fall through to a later translator.
    macro_rules! first_translation {
        ($($candidate:expr),+ $(,)?) => {{
            $(
                if let Some(value) = $candidate? {
                    return Ok(Some(value));
                }
            )+
            Ok(None)
        }};
    }
    first_translation!(
        translate_level17_mos(statement, &models.level17),
        translate_adc_schmitt_device(statement, &models.adc_schmitt),
        translate_pspice_mos_series_resistance(statement, &models.pspice_mos_series_resistances),
        translate_inline_schematic_x(statement),
        translate_legacy_voltage_poly_source(statement),
        translate_device_voltage_controlled_source(statement, &models.voltage_branches),
        translate_current_controlled_source(statement, &models.current_branches),
        pwls::translate(statement, &models.pwls_prefix),
        translate_raw_waveform(statement),
        translate_raw_transformer(statement),
        translate_raw_mutual_inductance(statement, &models.inductors),
        translate_raw_pwl_device(statement, &models.pwl),
        translate_raw_reactive_device(statement, &models.reactive),
        translate_raw_srff_device(statement, &models.srff),
        translate_raw_comp_device(statement, &models.comp),
        translate_raw_digital_device(statement, &models.digital),
        translate_raw_numbered_logic_device(statement, &models.numbered_logic),
        translate_raw_vcq_device(statement, models),
        translate_raw_vcsw_device(statement, models),
    )
}

fn remove_simple_assignment(statement: &str, name: &str) -> String {
    let normalized = normalized_statement(statement);
    let Some((start, _, value_end)) = simple_assignment_span(&normalized, name) else {
        return normalized;
    };
    let mut remove_start = start;
    while remove_start > 0 && normalized.as_bytes()[remove_start - 1].is_ascii_whitespace() {
        remove_start -= 1;
    }
    let mut remove_end = value_end;
    while normalized
        .as_bytes()
        .get(remove_end)
        .is_some_and(u8::is_ascii_whitespace)
    {
        remove_end += 1;
    }
    if remove_start > 0 && normalized.as_bytes()[remove_start - 1] == b',' {
        remove_start -= 1;
    } else if normalized.as_bytes().get(remove_end) == Some(&b',') {
        remove_end += 1;
        while normalized
            .as_bytes()
            .get(remove_end)
            .is_some_and(u8::is_ascii_whitespace)
        {
            remove_end += 1;
        }
    }
    let prefix = normalized[..remove_start].trim_end();
    let suffix = normalized[remove_end..].trim_start();
    let mut output = String::with_capacity(normalized.len());
    output.push_str(prefix);
    if !prefix.is_empty()
        && !prefix.ends_with('(')
        && !prefix.ends_with(',')
        && !suffix.starts_with(')')
        && !suffix.starts_with(',')
        && !suffix.is_empty()
    {
        output.push(' ');
    }
    output.push_str(suffix);
    output
}

fn replace_simple_assignment_value(statement: &str, name: &str, value: &str) -> Result<String> {
    let normalized = normalized_statement(statement);
    let Some((start, _, value_end)) = simple_assignment_span(&normalized, name) else {
        bail!("SIMetrix model parameter {name} is missing");
    };
    let mut output = String::with_capacity(normalized.len());
    output.push_str(&normalized[..start]);
    write!(output, "{name}={value}")?;
    output.push_str(&normalized[value_end..]);
    Ok(output)
}

fn rename_simple_assignment(statement: &str, old_name: &str, new_name: &str) -> Result<String> {
    let normalized = normalized_statement(statement);
    let Some((start, value_start, value_end)) = simple_assignment_span(&normalized, old_name)
    else {
        bail!("SIMetrix model parameter {old_name} is missing");
    };
    let mut output = String::with_capacity(normalized.len() + new_name.len());
    output.push_str(&normalized[..start]);
    write!(output, "{new_name}={}", &normalized[value_start..value_end])?;
    output.push_str(&normalized[value_end..]);
    Ok(output)
}

#[derive(Clone, Copy, Debug)]
struct PspiceMosSeriesResistance {
    rg: f64,
    rb: f64,
}

fn translate_pspice_mos_series_resistance(
    statement: &str,
    models: &std::collections::BTreeMap<String, PspiceMosSeriesResistance>,
) -> Result<Option<Translation>> {
    let normalized = normalized_statement(statement);
    let mut tokens = statement_tokens(&normalized)?;
    if tokens.len() < 6
        || !tokens
            .first()
            .and_then(|name| name.as_bytes().first())
            .is_some_and(|byte| matches!(*byte, b'M' | b'm'))
    {
        return Ok(None);
    }
    let Some(model) = models.get(&tokens[5].to_ascii_uppercase()) else {
        return Ok(None);
    };
    if tokens.iter().skip(6).any(|token| {
        token.split_once('=').is_some_and(|(name, _)| {
            name.eq_ignore_ascii_case("NRG") || name.eq_ignore_ascii_case("NRB")
        })
    }) {
        bail!(
            "PSpice MOS {} combines absolute RG/RB with unsupported NRG/NRB geometry scaling",
            tokens[0]
        );
    }
    for node in &tokens[1..5] {
        validate_node(node)?;
    }
    let stem = sanitized_name(&tokens[0]);
    let mut output = String::new();
    if model.rg > 0.0 {
        let internal = format!("__{stem}_GINT");
        write!(output, "R__{stem}_RG {} {internal} ", tokens[2])?;
        write_number(&mut output, model.rg)?;
        output.push('\n');
        tokens[2] = internal;
    }
    if model.rb > 0.0 {
        let internal = format!("__{stem}_BINT");
        write!(output, "R__{stem}_RB {} {internal} ", tokens[4])?;
        write_number(&mut output, model.rb)?;
        output.push('\n');
        tokens[4] = internal;
    }
    output.push_str(&tokens.join(" "));
    output.push('\n');
    Ok(Some(Translation {
        bytes: output.into_bytes(),
        kind: TranslationKind::PspiceMosSeriesResistance,
    }))
}

fn rewrite_simetrix_model_compat(statement: &str) -> Result<Option<String>> {
    let Some((_name, kind)) = model_header(statement) else {
        return Ok(None);
    };
    let mut output = normalized_statement(statement);
    let temperature_model = matches!(kind.as_str(), "D" | "R" | "RES");
    let mut changed = if temperature_model && simple_assignment_value(&output, "T_ABS").is_some() {
        output = remove_simple_assignment(&output, "T_ABS");
        true
    } else {
        false
    };
    if matches!(kind.as_str(), "NMOS" | "PMOS") {
        if simple_assignment_value(&output, "LEVEL").is_none()
            && simple_assignment_value(&output, "VLEVEL").is_some()
        {
            output = rename_simple_assignment(&output, "VLEVEL", "LEVEL")?;
            changed = true;
        }
        if simple_assignment_value(&output, "VTO").is_none()
            && simple_assignment_value(&output, "TO").is_some()
        {
            output = rename_simple_assignment(&output, "TO", "VTO")?;
            changed = true;
        }
        for parameter in ["RG", "RB"] {
            if simple_assignment_value(&output, parameter).is_some() {
                output = remove_simple_assignment(&output, parameter);
                changed = true;
            }
        }
    }
    if matches!(kind.as_str(), "NMOS" | "PMOS")
        && simple_assignment_value(&output, "LEVEL").as_deref() == Some("7")
    {
        let discrete = ["CGDMAX", "CGDMIN", "XG1CGD", "XG2CGD", "VTCGD"]
            .iter()
            .any(|parameter| simple_assignment_value(&output, parameter).is_some());
        let version_is_bsim3 = simple_assignment_value(&output, "VERSION")
            .is_some_and(|value| value.starts_with("3."));
        let bsim3_controls = ["MOBMOD", "CAPMOD", "PARAMCHK", "NQSMOD"]
            .iter()
            .any(|parameter| simple_assignment_value(&output, parameter).is_some());
        if !discrete && version_is_bsim3 && bsim3_controls {
            output = replace_simple_assignment_value(&output, "LEVEL", "8")?;
            changed = true;
        }
    }
    Ok(changed.then_some(output))
}

fn apply_simetrix_temperature(
    statement: &str,
    diode_temperatures: &std::collections::BTreeMap<String, f64>,
    resistor_temperatures: &std::collections::BTreeMap<String, f64>,
) -> Result<String> {
    let normalized = normalized_statement(statement);
    let tokens = statement_tokens(&normalized)?;
    if tokens.len() < 4 {
        return Ok(statement.to_owned());
    }
    let Some(kind) = tokens[0].as_bytes().first().map(u8::to_ascii_uppercase) else {
        return Ok(statement.to_owned());
    };
    let (temperatures, label) = match kind {
        b'D' => (diode_temperatures, "diode"),
        b'R' => (resistor_temperatures, "resistor"),
        _ => return Ok(statement.to_owned()),
    };
    let Some(temperature) = temperatures.get(&tokens[3].to_ascii_uppercase()) else {
        return Ok(statement.to_owned());
    };
    if tokens.iter().skip(4).any(|token| {
        token
            .split_once('=')
            .is_some_and(|(name, _)| name.eq_ignore_ascii_case("TEMP"))
    }) {
        bail!(
            "{label} instance {} already has TEMP with model-level T_ABS",
            tokens[0]
        );
    }
    let mut output = normalized;
    output.push_str(" TEMP=");
    write_number(&mut output, *temperature)?;
    Ok(output)
}

fn rewrite_pspice_positional_area(statement: &str) -> Result<String> {
    let normalized = normalized_statement(statement);
    let mut tokens = statement_tokens(&normalized)?;
    let Some(kind) = tokens
        .first()
        .and_then(|name| name.as_bytes().first())
        .copied()
        .map(|byte| byte.to_ascii_uppercase())
    else {
        return Ok(statement.to_owned());
    };
    let area_index = match kind {
        b'D' if tokens.len() >= 5 && crate::parse_numeric(&tokens[4]).is_ok() => Some(4),
        b'Q' if tokens.len() >= 6
            && tokens
                .last()
                .is_some_and(|token| crate::parse_numeric(token).is_ok()) =>
        {
            Some(tokens.len().saturating_sub(1))
        }
        _ => None,
    };
    let Some(area_index) = area_index else {
        return Ok(statement.to_owned());
    };
    if tokens[area_index].contains('=') {
        return Ok(statement.to_owned());
    }
    tokens[area_index] = format!("AREA={}", tokens[area_index]);
    Ok(tokens.join(" "))
}

fn sanitize_native_passive(statement: &str) -> Result<String> {
    let normalized = normalized_statement(statement);
    let tokens = statement_tokens(&normalized)?;
    let Some(name) = tokens.first() else {
        return Ok(statement.to_owned());
    };
    let kind = name.as_bytes().first().map(u8::to_ascii_uppercase);
    if !matches!(kind, Some(b'R' | b'C' | b'L')) {
        return Ok(statement.to_owned());
    }

    let mut changed = false;
    let mut filtered = Vec::with_capacity(tokens.len().saturating_add(1));
    let mut index = 0usize;
    while index < tokens.len() {
        let token = &tokens[index];
        let key = token.split_once('=').map_or(token.as_str(), |(key, _)| key);
        if index >= 3
            && token.contains('=')
            && (key.eq_ignore_ascii_case("BRANCH") || key.eq_ignore_ascii_case("ICMODE"))
        {
            bail!(
                "unbound source initialization field {key} on {name}; prepare the source-specific initialization policy before native translation"
            );
        }

        if kind == Some(b'R') && key.eq_ignore_ascii_case("TC") {
            let mut values = Vec::new();
            if let Some((_, value)) = token.split_once('=') {
                values.extend(
                    value
                        .split(',')
                        .filter(|value| !value.is_empty())
                        .map(str::to_owned),
                );
                index += 1;
            } else {
                let first = tokens
                    .get(index.saturating_add(1))
                    .context("PSpice resistor TC has no first coefficient")?;
                values.extend(
                    first
                        .split(',')
                        .filter(|value| !value.is_empty())
                        .map(str::to_owned),
                );
                index += 2;
            }
            if values.len() == 1
                && let Some(second) = tokens.get(index)
                && !second.contains('=')
                && (crate::parse_numeric(second).is_ok()
                    || second.starts_with('{') && second.ends_with('}'))
            {
                values.push(second.clone());
                index += 1;
            }
            if values.is_empty() || values.len() > 2 {
                bail!("unsupported PSpice resistor TC coefficients in {statement}");
            }
            filtered.push(format!("TC1={}", values[0]));
            if let Some(second) = values.get(1) {
                filtered.push(format!("TC2={second}"));
            }
            changed = true;
            continue;
        }

        filtered.push(token.clone());
        index += 1;
    }
    if changed {
        Ok(filtered.join(" "))
    } else {
        Ok(statement.to_owned())
    }
}

fn emit_deck_statement(
    statement: &str,
    models: &ScopeAnalysis,
    output: &mut String,
    primitives: &mut Vec<TranslationKind>,
    stripped_directives: &mut usize,
) -> Result<()> {
    let trimmed = statement.trim();
    if trimmed.is_empty() || trimmed.starts_with('*') {
        if magic_comment(statement) {
            *stripped_directives = stripped_directives.saturating_add(1);
        } else {
            writeln!(output, "{statement}")?;
        }
        return Ok(());
    }
    let banner_upper = trimmed.to_ascii_uppercase();
    if banner_upper.ends_with(" SPICE MACRO-MODEL") && trimmed.split_whitespace().count() == 3 {
        writeln!(output, "* {trimmed}")?;
        return Ok(());
    }
    if let Some(rewritten) = rewrite_simetrix_nodeset(statement)? {
        writeln!(output, "{rewritten}")?;
        return Ok(());
    }
    if strip_directive(statement) {
        *stripped_directives = stripped_directives.saturating_add(1);
        return Ok(());
    }
    if unsupported_directive(statement) {
        bail!("unsupported SIMPLIS directive remains in expanded deck: {trimmed}");
    }
    let effective = statement_with_init_override(statement, &models.init)?;
    let statement = effective.as_str();
    if let Some((name, model)) = raw_level17_model(statement)? {
        output.push_str(&emit_level17_core_model(&name, &model)?);
        return Ok(());
    }
    if let Some(rewritten) = rewrite_native_bridge_model(statement, &models.native_bridges)? {
        output.push_str(&rewritten);
        output.push('\n');
        return Ok(());
    }
    if let Some(rewritten) = rewrite_simetrix_model_compat(statement)? {
        output.push_str(&rewritten);
        output.push('\n');
        return Ok(());
    }
    if raw_pwl_model(statement)?.is_some()
        || raw_reactive_model(statement)?.is_some()
        || raw_comp_model(statement)?.is_some()
        || raw_adc_schmitt_model(statement)?.is_some()
        || raw_srff_model(statement)?.is_some()
        || raw_digital_model(statement)?.is_some()
        || raw_numbered_logic_model(statement)?.is_some()
        || raw_vcsw_model(statement)?.is_some()
        || raw_vcq_model(statement)?.is_some()
    {
        return Ok(());
    }
    if let Some(rewritten) = rewrite_native_bridge_instance(statement, &models.native_bridges)? {
        output.push_str(&rewritten);
        output.push('\n');
        return Ok(());
    }
    if let Some(translation) = translate_deck_primitive(statement, models)? {
        primitives.push(translation.kind);
        output.push_str(std::str::from_utf8(&translation.bytes)?);
        return Ok(());
    }
    let temperature_adjusted = apply_simetrix_temperature(
        statement,
        &models.simetrix_diode_temperatures,
        &models.simetrix_resistor_temperatures,
    )?;
    let behavior_adjusted = lower_simetrix_behavioral_statement(&temperature_adjusted)?;
    let area_adjusted = rewrite_pspice_positional_area(&behavior_adjusted)?;
    let native = sanitize_native_passive(&area_adjusted)?;
    output.push_str(&native);
    output.push('\n');
    Ok(())
}

fn numeric_tokens(text: &str) -> impl Iterator<Item = &str> {
    text.split(|character: char| !character.is_ascii_digit())
        .filter(|token| !token.is_empty())
}

fn dead_numeric_subcircuit_calls(block: &Subcircuit) -> Result<std::collections::BTreeSet<String>> {
    let header_tokens = statement_tokens(&normalized_statement(&block.header))?;
    let ports = header_tokens
        .iter()
        .skip(2)
        .filter(|token| token.bytes().all(|byte| byte.is_ascii_digit()))
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    let statements = block
        .body
        .iter()
        .filter_map(|item| match item {
            Item::Statement(statement) => Some(statement.as_str()),
            Item::Subcircuit(_) => None,
        })
        .collect::<Vec<_>>();
    let joined = statements.join("\n");
    let numeric_counts = numeric_tokens(&joined).fold(
        std::collections::BTreeMap::<String, usize>::new(),
        |mut counts, token| {
            *counts.entry(token.to_owned()).or_default() += 1;
            counts
        },
    );
    let mut dead = std::collections::BTreeSet::new();
    for statement in statements {
        let Ok(tokens) = statement_tokens(&normalized_statement(statement)) else {
            continue;
        };
        let Some(instance) = tokens.first() else {
            continue;
        };
        let Some((target_index, _target)) = tokens
            .iter()
            .enumerate()
            .skip(2)
            .find(|(_, token)| generated_simplis_identifier(&token.to_ascii_uppercase()))
        else {
            continue;
        };
        if !instance
            .as_bytes()
            .first()
            .is_some_and(|byte| matches!(*byte, 88 | 120))
            || target_index < 2
        {
            continue;
        }
        let nodes = &tokens[1..target_index];
        if nodes.is_empty()
            || nodes
                .iter()
                .any(|node| !node.bytes().all(|byte| byte.is_ascii_digit()))
            || nodes.iter().any(|node| ports.contains(node))
            || nodes
                .iter()
                .any(|node| numeric_counts.get(node).copied().unwrap_or_default() != 1)
        {
            continue;
        }
        let instance_mentions = joined.matches(instance).count();
        if instance_mentions == 1 {
            dead.insert(statement.to_owned());
        }
    }
    Ok(dead)
}

fn emit_deck_subcircuit_children(
    block: &Subcircuit,
    output: &mut String,
    primitives: &mut Vec<TranslationKind>,
    stripped_directives: &mut usize,
    flattened_subcircuits: &mut usize,
) -> Result<()> {
    for item in &block.body {
        if let Item::Subcircuit(child) = item {
            emit_deck_subcircuit(
                child,
                output,
                primitives,
                stripped_directives,
                flattened_subcircuits,
            )
            .map_err(|error| anyhow::anyhow!("translating subcircuit {}: {error:#}", child.name))?;
        }
    }
    Ok(())
}

fn repair_unambiguous_pspice_one_model_typo(items: &[Item]) -> Result<bool> {
    let mut one_model_cards = 0usize;
    let mut q1_model_cards = 0usize;
    let mut one_model_references = 0usize;
    let mut q1_model_references = 0usize;
    for item in items {
        let Item::Statement(statement) = item else {
            continue;
        };
        if let Some((name, _kind)) = model_header(statement) {
            if name == "1MODEL" {
                one_model_cards += 1;
            } else if name == "Q1MODEL" {
                q1_model_cards += 1;
            }
        }
        let tokens = statement_tokens(&normalized_statement(statement))?;
        if !tokens
            .first()
            .and_then(|name| name.as_bytes().first())
            .is_some_and(|byte| matches!(byte.to_ascii_uppercase(), b'Q'))
        {
            continue;
        }
        one_model_references += tokens
            .iter()
            .skip(1)
            .filter(|token| token.eq_ignore_ascii_case("1model"))
            .count();
        q1_model_references += tokens
            .iter()
            .skip(1)
            .filter(|token| token.eq_ignore_ascii_case("Q1model"))
            .count();
    }
    Ok(one_model_cards == 1
        && q1_model_cards == 0
        && one_model_references == 0
        && q1_model_references > 0)
}

fn rewrite_unambiguous_pspice_one_model_typo(statement: &str, enabled: bool) -> Result<String> {
    if !enabled || model_header(statement).is_none_or(|(name, _kind)| name != "1MODEL") {
        return Ok(statement.to_owned());
    }
    let mut tokens = statement_tokens(&normalized_statement(statement))?;
    if tokens.len() < 3 {
        bail!("malformed PSpice model card selected for 1model/Q1model repair");
    }
    "Q1model".clone_into(&mut tokens[1]);
    Ok(tokens.join(" "))
}

fn repaired_simetrix_subcircuit_header(block: &Subcircuit) -> Result<String> {
    let header = normalized_statement(&block.header);
    let mut tokens = statement_tokens(&header)?;
    let v2_index = tokens.iter().position(|token| {
        token
            .get(..3)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("V2="))
    });
    let has_internal_v2 = tokens.iter().any(|token| {
        token
            .get(..4)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("_V2="))
    });
    let body_uses_internal_v2 = block.body.iter().any(|item| {
        matches!(item, Item::Statement(statement) if statement.to_ascii_uppercase().contains("{_V2}"))
    });
    let body_uses_public_v2 = block.body.iter().any(|item| {
        matches!(item, Item::Statement(statement) if statement.to_ascii_uppercase().contains("{V2}"))
    });
    if let Some(index) = v2_index
        && !has_internal_v2
        && body_uses_internal_v2
        && !body_uses_public_v2
    {
        tokens[index] = format!("_{}", tokens[index]);
        return Ok(tokens.join(" "));
    }
    Ok(header)
}

fn emit_deck_subcircuit(
    block: &Subcircuit,
    output: &mut String,
    primitives: &mut Vec<TranslationKind>,
    stripped_directives: &mut usize,
    flattened_subcircuits: &mut usize,
) -> Result<()> {
    emit_deck_subcircuit_children(
        block,
        output,
        primitives,
        stripped_directives,
        flattened_subcircuits,
    )?;
    let dead_calls = dead_numeric_subcircuit_calls(block)?;
    let repair_one_model_typo = repair_unambiguous_pspice_one_model_typo(&block.body)?;
    let models = ScopeAnalysis::analyze(&block.body, Some(&block.header))?;
    *flattened_subcircuits = flattened_subcircuits.saturating_add(1);
    output.push_str(&repaired_simetrix_subcircuit_header(block)?);
    output.push('\n');
    let mut embedded_file = false;
    for item in &block.body {
        if let Item::Statement(statement) = item {
            let file_directive = file_boundary(statement);
            if file_directive == Some(true) {
                embedded_file = true;
                writeln!(
                    output,
                    "* ngspice-rs-preprocess: omitted embedded SIMetrix logic file"
                )?;
                *stripped_directives = stripped_directives.saturating_add(1);
                continue;
            }
            if embedded_file {
                if file_directive == Some(false) {
                    embedded_file = false;
                }
                *stripped_directives = stripped_directives.saturating_add(1);
                continue;
            }
            if dead_calls.contains(statement) {
                writeln!(
                    output,
                    "* ngspice-rs-preprocess: pruned disconnected instance {statement}"
                )?;
                continue;
            }
            let repaired =
                rewrite_unambiguous_pspice_one_model_typo(statement, repair_one_model_typo)?;
            emit_deck_statement(&repaired, &models, output, primitives, stripped_directives)
                .map_err(|error| {
                    anyhow::anyhow!(
                        "translating statement in subcircuit {}: {}: {error:#}",
                        block.name,
                        statement.trim()
                    )
                })?;
        }
    }
    if embedded_file {
        bail!(
            "unterminated embedded SIMetrix .file block in {}",
            block.name
        );
    }
    writeln!(output, ".ends {}", block.name)?;
    output.push('\n');
    Ok(())
}

fn reject_experimental_models(items: &[Item]) -> Result<()> {
    for statement in electrical_statements(items) {
        if let Some((name, kind)) = model_header(statement)
            && kind == "D_LOGIC_BLOCK"
        {
            bail!("SIMetrix D_LOGIC_BLOCK model {name} requires explicit ALB lowering");
        }
    }
    for item in items {
        if let Item::Subcircuit(block) = item {
            reject_experimental_models(&block.body)
                .map_err(|error| anyhow::anyhow!("subcircuit {}: {error:#}", block.name))?;
        }
    }
    Ok(())
}

/// Translate an already-preprocessed SIMPLIS simulation deck to a native-ngspice deck.
///
/// Root electrical circuitry is preserved. Analysis/UI directives are stripped so callers can
/// attach an ngspice analysis.
/// Unsupported primitives and protected bodies fail closed.
///
/// # Errors
///
/// Returns an error for malformed nesting, unsupported residual SIMPLIS syntax,
/// protected content, or a primitive outside the implemented exact lowering set.
pub fn translate_simplis_deck(input: &[u8]) -> Result<WholeModelTranslation> {
    let text =
        std::str::from_utf8(input).context("SIMPLIS deck translation requires UTF-8/ASCII")?;
    let mut items = parse_items(text)?;
    reject_experimental_models(&items)?;
    distribute_root_init_overrides(&mut items)?;
    let root_models = ScopeAnalysis::analyze(&items, None)?;
    let mut output = String::from(
        "* ngspice-rs-preprocess: translated expanded SIMPLIS deck to native ngspice\n",
    );
    let mut primitives = Vec::new();
    let mut stripped_directives = 0usize;
    let mut flattened_subcircuits = 0usize;
    for item in &items {
        match item {
            Item::Subcircuit(block) => emit_deck_subcircuit(
                block,
                &mut output,
                &mut primitives,
                &mut stripped_directives,
                &mut flattened_subcircuits,
            )
            .map_err(|error| anyhow::anyhow!("translating subcircuit {}: {error:#}", block.name))?,
            Item::Statement(statement) => emit_deck_statement(
                statement,
                &root_models,
                &mut output,
                &mut primitives,
                &mut stripped_directives,
            )
            .map_err(|error| {
                anyhow::anyhow!(
                    "translating root statement: {}: {error:#}",
                    statement.trim()
                )
            })?,
        }
    }
    let required_code_models = if primitives
        .iter()
        .any(|kind| kind.required_runtime().is_some())
    {
        // Load the library before parsing this deck, not from a .control
        // command after unresolved A-device models have already been read.
        output.insert_str(
            0,
            "* Requires ngspice_preprocess_logic.cm loaded before parsing\n",
        );
        vec![CodeModelRuntime::Logic]
    } else {
        Vec::new()
    };
    output.push_str(".end\n");
    reject_residual_simplis(&output)?;
    Ok(WholeModelTranslation {
        bytes: output.into_bytes(),
        primitives,
        required_code_models,
        stripped_directives,
        flattened_subcircuits,
    })
}

/// Translate one independently preprocessed SIMPLIS companion library to native ngspice.
///
/// Existing `.include` dependency edges are preserved, translated subcircuits remain
/// include-safe, and no top-level `.end` is emitted.
///
/// # Errors
///
/// Returns the same semantic errors as [`translate_simplis_deck`].
pub fn translate_simplis_expanded_library(input: &[u8]) -> Result<WholeModelTranslation> {
    const END: &[u8] = b".end\n";
    let mut translated = translate_simplis_deck(input)?;
    if !translated.bytes.ends_with(END) {
        bail!("translated SIMPLIS library is missing synthetic .end terminator");
    }
    translated
        .bytes
        .truncate(translated.bytes.len() - END.len());
    Ok(translated)
}

/// Translate a preprocessed SIMPLIS deck and package generated subcircuits in a companion ngspice include.
///
/// The root circuit remains in `deck_bytes`; translated/generated `.subckt` definitions are emitted in
/// `library_bytes`, and the root deck contains one `.include` at the position of the first subcircuit.
///
/// # Errors
///
/// Returns the same semantic errors as [`translate_simplis_deck`], plus an error for an invalid include
/// path or a translated deck with no generated subcircuits.
pub fn translate_simplis_deck_split(
    input: &[u8],
    include_path: &str,
) -> Result<SplitDeckTranslation> {
    let translated = translate_simplis_deck(input)?;
    let (deck_bytes, library_bytes) = split_native_deck(&translated.bytes, include_path)?;
    Ok(SplitDeckTranslation {
        deck_bytes,
        library_bytes,
        primitives: translated.primitives,
        required_code_models: translated.required_code_models,
        stripped_directives: translated.stripped_directives,
        flattened_subcircuits: translated.flattened_subcircuits,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expanded_library_preserves_dependency_include_without_end() -> Result<()> {
        let source = b".include \"dep.generated.lib\"\n.subckt T 1 2\nR1 1 2 1k\n.ends T\n";
        let translated = translate_simplis_expanded_library(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains(".include \"dep.generated.lib\""));
        assert!(text.contains(".subckt T 1 2"));
        assert!(
            !text
                .lines()
                .any(|line| line.trim().eq_ignore_ascii_case(".end"))
        );
        Ok(())
    }

    #[test]
    fn split_deck_moves_subcircuits_to_companion_include() -> Result<()> {
        let source =
            b"X1 1 0 PASS$1\nV1 1 0 1\n.subckt PASS$1 1 2\nR1 1 2 1k\n.ends PASS$1\n.end\n";
        let translated = translate_simplis_deck_split(source, "generated.lib")?;
        let deck = std::str::from_utf8(&translated.deck_bytes)?;
        let library = std::str::from_utf8(&translated.library_bytes)?;
        assert!(deck.contains("X1 1 0 PASS$1"));
        assert!(deck.contains(".include \"generated.lib\""));
        assert!(!deck.to_ascii_lowercase().contains(".subckt"));
        assert!(library.contains(".subckt PASS$1 1 2"));
        assert!(library.contains("R1 1 2 1k"));
        assert!(library.contains(".ends PASS$1"));
        assert!(!library.to_ascii_lowercase().contains(".end\n"));
        Ok(())
    }

    #[test]
    fn raw_saw_accepts_omitted_idle_in_pop() -> Result<()> {
        let source = br"VSAW a 0 SAW V1=0 V2=1 FREQ=100k DELAY=0 OFF_UNTIL_DELAY=NO
R1 a 0 1k
.end
";
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains("B__VSAW a 0 V='"));
        assert!(!text.contains("IDLE_IN_POP"));
        Ok(())
    }

    #[test]
    fn normalizes_simetrix_nodeset_shorthand() -> Result<()> {
        let source = b".nodeset R3_N 1 OUT {VBIAS}\n.nodeset FLAG {IF(MODE==1,2,3)}\n.nodeset V(KEEP)=2\n.nodeset V(SPACED) = 3 all=0\n.nodeset V(ODD)==4\nR1 R3_N 0 1k\n.end\n";
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains(".nodeset V(R3_N)=1 V(OUT)={VBIAS}"));
        assert!(text.contains(".nodeset V(FLAG)={IF(MODE==1,2,3)}"));
        assert!(text.contains(".nodeset V(KEEP)=2"));
        assert!(text.contains(".nodeset V(SPACED) = 3 all=0"));
        assert!(text.contains(".nodeset V(ODD)==4"));
        Ok(())
    }

    #[test]
    fn rejects_malformed_simetrix_nodeset_forms() {
        for nodeset in [
            ".nodeset R3_N",
            ".nodeset R3_N 1 OUT",
            ".nodeset R3_N =1",
            ".nodeset R3_N 1 OUT =2",
            ".nodeset R3_N 1 V(OUT)=2 3",
            ".nodeset V(OUT) 1",
        ] {
            let source = format!("{nodeset}\nR1 R3_N 0 1k\n.end\n");
            assert!(
                translate_simplis_deck(source.as_bytes()).is_err(),
                "accepted malformed nodeset: {nodeset}"
            );
        }
    }

    #[test]
    fn preserves_native_positional_sine_waveform() -> Result<()> {
        let source = b"I1 out 0 SIN 0 0.375 10000 -0.000115 0\n.end\n";
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains("I1 out 0 SIN 0 0.375 10000 -0.000115 0"));
        Ok(())
    }

    #[test]
    fn translates_raw_sine_with_phase_delay_and_off_until_delay() -> Result<()> {
        let source =
            br"VAC a 0 SIN VOFFSET=1 APEAK=2 FREQ=50 PDELAY=90 OFF_UNTIL_DELAY=YES DAMP_COEF=0
R1 a 0 1k
.end
";
        let translated = translate_simplis_deck(source)?;
        assert_eq!(translated.primitives, vec![TranslationKind::SineSource]);
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains("B__VAC a 0 V='(time<"));
        assert!(text.contains("exp(-("));
        assert!(text.contains(")*sin("));
        assert!(!text.contains(" OFF_UNTIL_DELAY="));
        Ok(())
    }

    #[test]
    fn translates_phase_shifted_raw_saw_and_pulse() -> Result<()> {
        let source = br"VSAW a 0 SAW V1=0 V2=1 FREQ=200k DELAY=2.5u OFF_UNTIL_DELAY=NO IDLE_IN_POP=NO
VPUL b 0 PUL V1=0 V2=5 FREQ=200k T_RISE=5n T_FALL=5n PWIDTH=45n DELAY=2.5u OFF_UNTIL_DELAY=NO IDLE_IN_POP=NO
R1 a 0 1k
R2 b 0 1k
.end
";
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains("B__VSAW a 0 V='"));
        assert!(text.contains("floor("));
        assert!(text.contains("B__VPUL b 0 V=pwl("));
        assert_eq!(translated.primitives.len(), 2);
        Ok(())
    }

    #[test]
    fn preserves_native_positional_pwl_source() -> Result<()> {
        let source = br".subckt T 1 2 params: _T_RISE=1n _DELAY=2n _V1=0 _V2=5
V1 1 2 PWL 0 {_V1} {_DELAY} {_V1} {_DELAY+_T_RISE} {_V2}
.ends T
.end
";
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains("V1 1 2 PWL 0 {_V1} {_DELAY} {_V1} {_DELAY+_T_RISE} {_V2}"));
        assert!(
            !translated
                .primitives
                .contains(&TranslationKind::PiecewiseLinearSource)
        );
        Ok(())
    }

    #[test]
    fn translates_raw_time_pwl_with_duplicate_time_for_step() -> Result<()> {
        let source = br"V1 a 0 PWL NSEG=2 X0=0 Y0=0 X1=1u Y1=0 X2=1u Y2=5
R1 a 0 1k
.end
";
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains("B__V1 a 0 V='"));
        assert!(text.contains("time<"));
        assert_eq!(
            translated.primitives,
            vec![TranslationKind::PiecewiseLinearSource]
        );
        Ok(())
    }

    #[test]
    fn translates_mutual_inductance_to_k() -> Result<()> {
        let source = br".subckt T 1 2 3 4
L1 1 2 100u
L2 3 4 100u
M-L1-L2 85u
.ends T
X1 a 0 b 0 T
.end
";
        let translated = translate_simplis_deck(source)?;
        assert_eq!(
            translated.primitives,
            vec![TranslationKind::MutualInductance]
        );
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains("K__M_L1_L2 L1 L2 8.49999999999999978e-1"));
        Ok(())
    }

    #[test]
    fn translates_lossless_two_winding_transformer() -> Result<()> {
        let source = br"V1 p 0 1
!T1 N_WIND=2 p 0 N1=1 s 0 N2=2
R1 s 0 100
.end
";
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains("V__T1_W2_SENSE s __T1_W2_MID 0"));
        assert!(text.contains("E__T1_W2 __T1_W2_MID 0 p 0"));
        assert!(text.contains("F__T1_W2_PRI 0 p V__T1_W2_SENSE"));
        assert_eq!(
            translated.primitives,
            vec![TranslationKind::IdealTransformer]
        );
        Ok(())
    }

    #[test]
    fn translates_raw_digital_buffer_and_or() -> Result<()> {
        let source = br"V1 a 0 5
V2 b 0 5
A_BUF %gnd_ref 0 [ q qb ] [ a ] MODEL=MB IC=0
.MODEL MB DIGITAL1 D_BUFFER RIN=10Meg ROUT=10 TH=2.5 HYSTWD=1 VOL=0 VOH=5 OUT_DELAY=0
A_AND %gnd_ref 0 [ y yb ] [ a b ] MODEL=MA IC=0
.MODEL MA DIGITAL1 D_AND RIN=10Meg ROUT=10 TH=2.5 HYSTWD=1 VOL=0 VOH=5 OUT_DELAY=0 NUM_INVERTED_INPUTS=0
A_OR %gnd_ref 0 [ z zb ] [ a b ] MODEL=MO IC=0
.MODEL MO DIGITAL1 D_OR RIN=10Meg ROUT=10 TH=2.5 HYSTWD=1 VOL=0 VOH=5 OUT_DELAY=0 NUM_INVERTED_INPUTS=0
R1 q 0 1k
R2 y 0 1k
R3 z 0 1k
.end
";
        let translated = translate_simplis_deck(source)?;
        assert_eq!(
            translated.primitives,
            vec![
                TranslationKind::Buffer,
                TranslationKind::AndGate,
                TranslationKind::OrGate
            ]
        );
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(!text.contains("DIGITAL1"));
        assert!(!text.contains("%gnd_ref"));
        Ok(())
    }

    #[test]
    fn translates_double_percent_digital_ground_reference() -> Result<()> {
        let source = br"A_COMP %%gnd_ref 100 [ 201 202 ] [ 103 ] MODEL=MB IC=0
.MODEL MB DIGITAL1 D_BUFFER RIN=10Meg ROUT=10 TH=0 HYSTWD=0.002 VOL=0 VOH=5 OUT_DELAY=2p
.end
";
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert_eq!(translated.primitives, vec![TranslationKind::Buffer]);
        assert!(!text.contains("%%gnd_ref"));
        Ok(())
    }

    #[test]
    fn translates_reset_dominant_raw_srlatch() -> Result<()> {
        let source = br"A_SR %gnd_ref 0 [ q qb ] [ s r ] MODEL=MS IC=0
.MODEL MS DIGITAL1 D_SRLATCH RIN=10Meg ROUT=10 TH=2.5 HYSTWD=1 VOL=0 VOH=5 OUT_DELAY=2p SET_RESET_LEVEL=1 DOM=R
.end
";
        let translated = translate_simplis_deck(source)?;
        assert_eq!(translated.primitives, vec![TranslationKind::SrLatch]);
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains("A__A_SR_EVENT"));
        assert!(text.contains("preprocess_logic_event(operation=4 delay="));
        assert!(text.contains("e-12 inertial=true"));
        assert!(!text.contains("DIGITAL1"));
        Ok(())
    }

    #[test]
    fn digital1_rejects_all_inverted_inputs() {
        let source = b"A1 %gnd_ref 0 [ q qb ] [ a b c ] MODEL=m IC=0\n.MODEL m DIGITAL1 D_AND RIN=10Meg ROUT=10 VOL=0.5 VOH=4.5 HYSTWD=1 TH=2.5 OUT_DELAY=10n NUM_INVERTED_INPUTS=3\n.end\n";
        assert!(translate_simplis_deck(source).is_err());
    }

    #[test]
    fn raw_srlatch_invalid_dominance_remains_fail_closed() {
        let source = br"A_SR %gnd_ref 0 [ q qb ] [ s r ] MODEL=MS IC=0
.MODEL MS DIGITAL1 D_SRLATCH RIN=10Meg ROUT=10 TH=2.5 HYSTWD=1 VOL=0 VOH=5 OUT_DELAY=2p SET_RESET_LEVEL=1 DOM=BAD
.end
";
        let error = translate_simplis_deck(source)
            .err()
            .map(|value| value.to_string());
        assert!(error.is_some_and(|value| value.contains("DOM=BAD")));
    }

    #[test]
    fn translates_level_two_icqpos_from_piecewise_current_branch() -> Result<()> {
        let source = br"!RGAIN gate 0 MG IC=1
.MODEL MG VPWLR NSEG=2 X0=-1 Y0=-1 X1=0 Y1=0 X2=1 Y2=1
Q1 drain 0 !RGAIN MQ IC=OPEN
.MODEL MQ ICQPOS VSAT=0 RSAT=0.1 ROFF=10Meg GAIN=2 TH=0 HYSTWD=1m LOGIC=POS LEVEL=2 IFLOW=BIDIR
.end
";
        let translated = translate_simplis_deck(source)?;
        assert_eq!(
            translated.primitives,
            vec![
                TranslationKind::Vpwlr,
                TranslationKind::TransistorSwitchLevel2
            ]
        );
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains("B__Q1_CTRL"));
        assert!(text.contains("i(BRGAIN)"));
        assert!(text.contains("max("));
        assert!(text.contains("min("));
        assert!(!text.contains(" ICQPOS "));
        Ok(())
    }

    #[test]
    fn translates_level_two_icqneg_with_negative_logic() -> Result<()> {
        let source = br"!RGAIN gate 0 MG IC=1
.MODEL MG VPWLR NSEG=2 X0=-1 Y0=-1 X1=0 Y1=0 X2=1 Y2=1
Q1 0 drain !RGAIN MQ IC=CLOSE
.MODEL MQ ICQNEG VSAT=-0.2 RSAT=0.1 ROFF=10Meg GAIN=3 TH=0 HYSTWD=2m LOGIC=NEG LEVEL=2 IFLOW=UNIDIR
.end
";
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert_eq!(
            translated.primitives.last(),
            Some(&TranslationKind::TransistorSwitchLevel2)
        );
        assert!(text.contains("B__Q1_CTRL"));
        assert!(text.contains("min("));
        assert!(text.contains("max("));
        assert!(!text.contains(" ICQNEG "));
        Ok(())
    }

    #[test]
    fn translates_level_one_vcqpos_with_vsat() -> Result<()> {
        let source = br"VCTRL ctrl 0 3
Q1 a 0 ctrl 0 MQ IC=CLOSE
.MODEL MQ VCQPOS VSAT=0.1 RSAT=0.1 ROFF=10Meg GAIN=7 TH=2.5 HYSTWD=0.1 LOGIC=POS LEVEL=1 IFLOW=UNIDIR
R1 a 0 1k
.end
";
        let translated = translate_simplis_deck(source)?;
        assert_eq!(
            translated.primitives,
            vec![TranslationKind::TransistorSwitchLevel1]
        );
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains("A__Q1_EVENT"));
        assert!(text.contains("B__Q1_BRANCH a 0 I='"));
        assert!(text.contains("-1.00000000000000006e-1"));
        assert!(!text.contains(" VCQPOS "));
        Ok(())
    }

    #[test]
    fn raw_switch_rejects_vendor_invalid_models_and_initial_conditions() -> Result<()> {
        let model = ".MODEL MQ VCQPOS VSAT=0 RSAT=10 ROFF=1Meg GAIN=0.002 TH=2.5 HYSTWD=1 LOGIC=POS LEVEL=2";
        for (from, to) in [
            ("HYSTWD=1", "HYSTWD=0"),
            ("ROFF=1Meg", "ROFF=10"),
            ("VSAT=0", "VSAT=-0.1"),
            ("GAIN=0.002", "GAIN=0"),
            ("LEVEL=2", "LEVEL=3"),
            ("LEVEL=2", "LEVEL=2 IFLOW=OTHER"),
            ("VSAT=0", "VSAT=0.1 IFLOW=BIDIR"),
            ("LEVEL=2", "LEVEL=2 UNSUPPORTED=1"),
        ] {
            let source = format!(
                "Vctrl c 0 5\nQ1 a 0 c 0 MQ IC=CLOSE\n{}\n.end\n",
                model.replace(from, to)
            );
            assert!(
                translate_simplis_deck(source.as_bytes()).is_err(),
                "{source}"
            );
        }
        for hint in ["", " IC=CLOSED", " IC=OTHER"] {
            let source = format!("Vctrl c 0 5\nQ1 a 0 c 0 MQ{hint}\n{model}\n.end\n");
            assert!(
                translate_simplis_deck(source.as_bytes()).is_err(),
                "{source}"
            );
        }
        for hint in ["", " IC=CLOSED"] {
            let source = format!(
                "Vctrl c 0 5\nS1 a 0 c 0 MS{hint}\n.MODEL MS VCSW RON=10 ROFF=1Meg TH=2.5 HYSTWD=1 LOGIC=POS\n.end\n"
            );
            assert!(
                translate_simplis_deck(source.as_bytes()).is_err(),
                "{source}"
            );
        }
        let default = format!("Vctrl c 0 5\nQ1 a 0 c 0 MQ IC=CLOSE\n{model}\n.end\n");
        let explicit = default.replace("LEVEL=2", "LEVEL=2 IFLOW=UNIDIR");
        let bidir = default.replace("LEVEL=2", "LEVEL=2 IFLOW=BIDIR");
        let default = translate_simplis_deck(default.as_bytes())?;
        assert_eq!(
            default.bytes,
            translate_simplis_deck(explicit.as_bytes())?.bytes
        );
        assert_ne!(
            default.bytes,
            translate_simplis_deck(bidir.as_bytes())?.bytes
        );
        Ok(())
    }

    #[test]
    fn translates_device_voltage_controlled_source() -> Result<()> {
        let source = br".subckt T a b c d
RCTRL a b 1k
E1 c d RCTRL 2
.ends T
.end
";
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains("B__E1 c d V='2.00000000000000000e0*v(a,b)'"));
        assert!(!text.lines().any(|line| line.starts_with("E1 ")));
        Ok(())
    }

    #[test]
    fn normalizes_pspice_resistor_temperature_coefficients() -> Result<()> {
        assert_eq!(
            sanitize_native_passive("R61 0 61 100 TC 3.11M 4.51U")?,
            "R61 0 61 100 TC1=3.11M TC2=4.51U"
        );
        assert_eq!(
            sanitize_native_passive("R98 0 98 1 TC=-2.8M,2.63U")?,
            "R98 0 98 1 TC1=-2.8M TC2=2.63U"
        );
        assert_eq!(
            sanitize_native_passive("RIS 33 4 1 TC=-0.00379747, -3.55271e-20")?,
            "RIS 33 4 1 TC1=-0.00379747 TC2=-3.55271e-20"
        );
        assert_eq!(
            sanitize_native_passive("R27 0 55 10 TC=3.453E-3 7.93E-5")?,
            "R27 0 55 10 TC1=3.453E-3 TC2=7.93E-5"
        );
        assert_eq!(
            sanitize_native_passive("R31 31 0 1 TC=2.8m")?,
            "R31 31 0 1 TC1=2.8m"
        );
        Ok(())
    }

    #[test]
    fn lowers_pspice_boolean_operators_in_runtime_expressions() -> Result<()> {
        let lowered = lower_simetrix_behavioral_statement(
            "G8 VS 0 value={((v(Range)<.05) ? (((v(Vcm)<65.1) & (v(Vcm)>-10.1)) ? 1 : 2) : (~v(A) | v(B)))}",
        )?;
        assert!(lowered.contains("&&"));
        assert!(lowered.contains("||"));
        assert!(lowered.contains("!v(A)"));
        assert!(!lowered.contains(" & "));
        assert!(!lowered.contains(" | "));
        assert!(!lowered.contains("~v(A)"));
        assert_eq!(
            lower_simetrix_behavioral_statement("B1 x 0 V='literal & | ~ TEMP'")?,
            "B1 x 0 V='literal & | ~ TEMP'"
        );
        Ok(())
    }

    #[test]
    fn lowers_simetrix_runtime_if_iff_beyond_behavioral_sources() -> Result<()> {
        let passive = lower_simetrix_behavioral_statement("Lg gate g1 {Lg*if(dgfs==99,0,1)}")?;
        assert!(passive.contains(" ? "));
        assert!(!passive.to_ascii_uppercase().contains("IF("));

        let parameter = lower_simetrix_behavioral_statement(".param sign {IFF(K<0,-1,1)}")?;
        assert!(parameter.contains(" ? "));
        assert!(!parameter.to_ascii_uppercase().contains("IFF("));

        let function = lower_simetrix_behavioral_statement(".FUNC QCdg(x,z) {if(x>z,x,z)}")?;
        assert!(function.contains(" ? "));
        assert!(!function.to_ascii_uppercase().contains("IF("));

        let model = ".model M NMOS LIMIT(VDS)=(*,500)";
        assert_eq!(lower_simetrix_behavioral_statement(model)?, model);
        Ok(())
    }

    #[test]
    fn lowers_simetrix_global_temp_to_ngspice_temper() -> Result<()> {
        let lowered = lower_simetrix_behavioral_statement("B1 out 0 V={TEMP+IF(TEMP>25,1,0)}")?;
        assert_eq!(
            lower_simetrix_behavioral_statement("D1 a b DM TEMP=25")?,
            "D1 a b DM TEMP=25"
        );
        assert!(lowered.contains("temper"));
        assert!(!lowered.to_ascii_uppercase().contains("TEMP+"));
        assert!(!lowered.to_ascii_uppercase().contains("IF("));
        Ok(())
    }

    #[test]
    fn rejects_unterminated_behavioral_argument_brackets() {
        let error = behavioral_function_arguments("a,[1,2")
            .err()
            .map(|value| value.to_string());
        assert!(
            error.is_some_and(|value| value.contains("unterminated behavioral function argument"))
        );
    }

    #[test]
    fn lowers_nested_simetrix_table_inside_if() -> Result<()> {
        let lowered = lower_simetrix_behavioral_statement(
            "B1 OUTP OUTN I=if(v(n2)<.5,0.001*tanh(10*uramp(v(n3)-0.7))*TABLE[0,0,0.8,0,0.81,0.45,1,1.85,8,3.06](V(n1)),-.002)",
        )?;
        let upper = lowered.to_ascii_uppercase();
        assert!(!upper.contains("TABLE["));
        assert!(!upper.contains("IF("));
        assert!(lowered.contains("pwl(max("));
        assert!(lowered.contains("V(n1)"));
        assert!(lowered.contains("tanh(10*uramp"));
        Ok(())
    }

    #[test]
    fn lowers_simetrix_table_expression_with_endpoint_hold() -> Result<()> {
        let lowered = lower_simetrix_behavioral_statement(
            "B1 out 0 V= TABLE[-1Gig,-2, 0,0, 1,10, 2,20](V(in)*2)",
        )?;
        assert!(!lowered.to_ascii_uppercase().contains("TABLE["));
        assert_eq!(lowered.matches("V(in)*2").count(), 1);
        assert!(lowered.contains("pwl(max("));
        assert!(lowered.contains("min("));
        assert!(lowered.contains("1.00000000000000000e9"));
        assert!(lowered.contains("2.00000000000000000e1"));

        let result = lower_simetrix_behavioral_statement("B1 out 0 V=TABLE[0,0, 0,1](V(in))");
        let Err(error) = result else {
            anyhow::bail!("duplicate SIMetrix TABLE x values must be rejected");
        };
        assert!(error.to_string().contains("strictly increasing"));
        Ok(())
    }

    #[test]
    fn lowers_simetrix_if_and_limit_behavioral_functions() -> Result<()> {
        let lowered = lower_simetrix_behavioral_statement(
            "B1 out 0 V={IF(V(in)>0.5, LIMIT(V(in),0.8,0.2), IF(V(in)<0, -1, 0))}",
        )?;
        assert!(!lowered.to_ascii_uppercase().contains("IF("));
        assert!(!lowered.to_ascii_uppercase().contains("LIMIT("));
        assert!(lowered.contains(" ? "));
        assert!(lowered.contains("V(in)"));
        Ok(())
    }

    #[test]
    fn repairs_unambiguous_simetrix_source_v2_header_typo() -> Result<()> {
        let source = br".subckt SQU_SOURCE 1 2 params: _V1=0 V2=1 _FREQ=100k
V1 1 2 PWL 0 {_V1} 1u {_V2}
.ends SQU_SOURCE
X1 a b SQU_SOURCE
.end
";
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains(".subckt SQU_SOURCE 1 2 params: _V1=0 _V2=1 _FREQ=100k"));
        assert!(!text.contains(" params: _V1=0 V2=1"));
        Ok(())
    }

    #[test]
    fn repairs_unambiguous_pspice_one_model_typo() -> Result<()> {
        let source = br".subckt T c b e
Q1 c b x Q1model
Q2 c x e Q2model
.model 1model NPN BF=100
.model Q2model NPN BF=50
.ends T
X1 c b e T
.end
";
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains(".model Q1model NPN BF=100"));
        assert!(!text.contains(".model 1model"));
        Ok(())
    }

    #[test]
    fn model_compat_handles_comma_assignments_and_legacy_mos_typos() -> Result<()> {
        let rewritten = rewrite_simetrix_model_compat(
            ".model pox pmos(vlevel=3,to=-0.75,kp=195u,rd=.5,rs=.5,rg=1,rb=1,cgso=66p)",
        )?
        .context("expected MOS compatibility rewrite")?;
        let upper = rewritten.to_ascii_uppercase();
        assert!(upper.contains("LEVEL=3"));
        assert!(upper.contains("VTO=-0.75"));
        assert!(!upper.contains("VLEVEL="));
        assert!(!upper.contains(" RG="));
        assert!(!upper.contains(",RG="));
        assert!(!upper.contains(" RB="));
        assert!(!upper.contains(",RB="));
        assert!(upper.contains("CGSO=66P"));
        Ok(())
    }

    #[test]
    fn lowers_pspice_mos_gate_and_bulk_series_resistance() -> Result<()> {
        let source = br".subckt T d g s b
M1 d g s b MOD L=2u W=3u
.model MOD NMOS(LEVEL=3 VTO=0.75 KP=200u RD=1 RS=.5 RG=2 RB=3)
.ends T
.end
";
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains("R__M1_RG g __M1_GINT 2.00000000000000000e0"));
        assert!(text.contains("R__M1_RB b __M1_BINT 3.00000000000000000e0"));
        assert!(text.contains("M1 d __M1_GINT s __M1_BINT MOD L=2u W=3u"));
        let model_line = text
            .lines()
            .find(|line| line.to_ascii_uppercase().starts_with(".MODEL MOD "))
            .context("missing rewritten MOS model")?;
        assert!(!model_line.to_ascii_uppercase().contains("RG="));
        assert!(!model_line.to_ascii_uppercase().contains("RB="));
        Ok(())
    }

    #[test]
    fn rewrites_pspice_positional_diode_and_bjt_area() -> Result<()> {
        assert_eq!(
            rewrite_pspice_positional_area("D60 60 0 DN1 1000")?,
            "D60 60 0 DN1 AREA=1000"
        );
        assert_eq!(
            rewrite_pspice_positional_area("Q2 n1 n2 n3 BJT 1.35")?,
            "Q2 n1 n2 n3 BJT AREA=1.35"
        );
        assert_eq!(
            rewrite_pspice_positional_area("Q3 n1 n2 n3 bulk BJT 1.45")?,
            "Q3 n1 n2 n3 bulk BJT AREA=1.45"
        );
        assert_eq!(
            rewrite_pspice_positional_area("D1 1 0 DM AREA=7")?,
            "D1 1 0 DM AREA=7"
        );
        Ok(())
    }

    #[test]
    fn preserves_native_behavioral_value_source() -> Result<()> {
        let source = b"EP 17 0 VALUE {V(7)-1.8}
.end
";
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains("EP 17 0 VALUE {V(7)-1.8}"));
        Ok(())
    }

    #[test]
    fn preserves_native_table_voltage_controlled_source() -> Result<()> {
        let source = br"Err ref 5 TABLE {V(401)}=(0,0)(0.3,0.004)(0.8,0.004)(1.5,1.236)
.end
";
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains("Err ref 5 TABLE {V(401)}=(0,0)(0.3,0.004)(0.8,0.004)(1.5,1.236)"));
        Ok(())
    }

    #[test]
    fn preserves_native_parenthesized_output_voltage_controlled_source() -> Result<()> {
        let source = b"G1 (9 98) (4 6) 4.5e-3
.end
";
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains("G1 (9 98) (4 6) 4.5e-3"));
        Ok(())
    }

    #[test]
    fn translates_parenthesized_node_voltage_controlled_source() -> Result<()> {
        let source = br"E1 9 46 (11,5) 375E6
G1 98 25 (12,15) 52.360E-6
.end
";
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains("B__E1 9 46 V='3.75000000000000000e8*v(11,5)'"));
        assert!(text.contains("B__G1 98 25 I='"));
        assert!(text.contains("*v(12,15)'"));
        Ok(())
    }

    #[test]
    fn translates_runtime_parameter_current_controlled_gain() -> Result<()> {
        let source = br".subckt T a k ctr=2
F1 k n F1$TP_CCCS {ctr}
V$F1$TP_CCCS s k 0
R1 a s 1k
.ends T
.end
";
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains("I='{ctr}*i(V$F1$TP_CCCS)'"));
        Ok(())
    }

    #[test]
    fn translates_pspice_current_controlled_value_source() -> Result<()> {
        let source = br"Vsh sense 0 0
Fsh VDD GND VALUE={I(Vsh)*(V(ctl)-1)-100n}
.end
";
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains("B__Fsh VDD GND I='I(Vsh)*(V(ctl)-1)-100n'"));
        Ok(())
    }

    #[test]
    fn electrical_branch_scan_skips_embedded_file_language() -> Result<()> {
        let items = parse_items(
            "R1 a 0 1k
.file logic
CLOCK=in[0],
LEVEL (WIDTH=1) Latch ;
.endf
V1 b 0 1
",
        )?;
        let branches = branch_voltage_nodes(&items)?;
        assert!(branches.contains_key("R1"));
        assert!(branches.contains_key("V1"));
        assert!(!branches.contains_key("CLOCK=IN[0],"));
        assert!(!branches.contains_key("LEVEL"));
        Ok(())
    }

    #[test]
    fn production_rejects_every_alb_declaration_without_compiling_body() -> Result<()> {
        for source in [
            ".model DL d_logic_block def=\"out[0]=in[0];\"\n.end\n",
            ".subckt inner a b\n.model DL d_logic_block FILE=missing\n.ends inner\n.end\n",
            ".subckt inner a b\n.file program\ninvalid { payload\n.endf\n.model DL d_logic_block FILE=program\n.ends inner\n.end\n",
            ".model DL D_LOGIC_BLOCK DEF='unmatched quote\n.end\n",
        ] {
            let error = translate_simplis_deck(source.as_bytes())
                .err()
                .context("production unexpectedly accepted an experimental ALB model")?;
            assert!(
                format!("{error:#}").contains("requires explicit ALB lowering"),
                "{error:#}"
            );
        }
        Ok(())
    }

    #[test]
    fn embedded_file_language_reaches_residual_model_guard_not_device_parser() {
        let source = br".subckt T a b
.file logic
HOLD=USER[3],
CLOCK=in[0],
.endf
.model DL d_logic_block file=logic
A1 a b DL
.ends T
.end
";
        let error = translate_simplis_deck(source)
            .err()
            .map(|value| value.to_string());
        assert!(error.is_some_and(|value| { value.contains("requires explicit ALB lowering") }));
    }

    #[test]
    fn translates_simplis_current_controlled_sources() -> Result<()> {
        let source = br".subckt T a b c d
R1 a b 1k
!RX c d MX IC=1
.MODEL MX VPWLR NSEG=1 X0=0 Y0=0 X1=1 Y1=1
F1 c 0 R1 2
H1 d 0 !RX 3
.ends T
.end
";
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains("B__F1 c 0 I='2.00000000000000000e0*i(R1)'"));
        assert!(text.contains("B__H1 d 0 V='3.00000000000000000e0*i(BRX)'"));
        assert!(!text.lines().any(|line| line.starts_with("F1 ")));
        assert!(!text.lines().any(|line| line.starts_with("H1 ")));
        Ok(())
    }

    #[test]
    fn pspice_poly_single_coefficient_is_constant_term() -> Result<()> {
        let source = br"V1 a 0 0
F1 out 0 POLY(1) V1 2
.end
";
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains("B__F1 out 0 I='2.00000000000000000e0'"));
        assert!(!text.contains("*i(V1)"));
        Ok(())
    }

    #[test]
    fn translates_legacy_whitespace_pspice_voltage_poly_sources() -> Result<()> {
        let source = br"ECC 16 0 POLY 1 4 0 -2.7 1.0
EX1 19 0 POLY 2 15 0 3 0 0 -0.84731 1.8436
G1 0 20 POLY 1 7 0 0 2m
.end
";
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains("B__ECC 16 0 V='"));
        assert!(text.contains("*v(4,0)'"));
        assert!(text.contains("B__EX1 19 0 V='"));
        assert!(text.contains("*v(15,0)"));
        assert!(text.contains("*v(3,0)"));
        assert!(text.contains("B__G1 0 20 I='"));
        Ok(())
    }

    #[test]
    fn translates_legacy_whitespace_pspice_current_poly_sources() -> Result<()> {
        let source = br"VMI sense 0 0
FCC 0 132 POLY 1 VMI -1.3052e-4 0.13
.end
";
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains("B__FCC 0 132 I='"));
        assert!(text.contains("*i(VMI)'"));
        Ok(())
    }

    #[test]
    fn translates_pspice_current_poly_linear_sources() -> Result<()> {
        let source = br"V1 a 0 0
V2 b 0 0
F1 out 0 POLY(2) V1 V2 0 -1 0.5
.end
";
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains(
            "B__F1 out 0 I='0.00000000000000000e0+(-1.00000000000000000e0)*i(V1)+(5.00000000000000000e-1)*i(V2)'"
        ));
        Ok(())
    }

    #[test]
    fn current_controlled_source_names_encode_punctuation_without_collisions() -> Result<()> {
        let source = br"V1 a 0 0
Fsy+ out 0 POLY(1) V1 0 1
Fsy- 0 out POLY(1) V1 0 -1
Fsy_2B aux 0 POLY(1) V1 0 2
.end
";
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains("B__Fsy_2B out 0"));
        assert!(text.contains("B__Fsy_2D 0 out"));
        assert!(text.contains("B__Fsy_5F2B aux 0"));
        Ok(())
    }

    #[test]
    fn translates_pspice_current_poly_quadratic_terms_in_spice_order() -> Result<()> {
        let source = br"V1 a 0 0
V2 b 0 0
H1 out 0 POLY(2) V1 V2 (0, 11.317k, 0, 0, -28.29Meg)
.end
";
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains("+(0.00000000000000000e0)*i(V1)*i(V1)"));
        assert!(text.contains("+(-2.82900000000000000e7)*i(V1)*i(V2)"));
        assert!(!text.contains("i(V2)*i(V2)"));
        Ok(())
    }

    #[test]
    fn translates_pspice_poly_parenthesized_coefficients_and_branch_aliases() -> Result<()> {
        let source = br"V$SENSE 0 a 0
F1 out 0 POLY(1) SENSE (0,2)
.end
";
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(
            text.contains(
                "B__F1 out 0 I='0.00000000000000000e0+(2.00000000000000000e0)*i(V$SENSE)'"
            )
        );
        Ok(())
    }

    #[test]
    fn translates_simple_srff_hold_on_both() -> Result<()> {
        let source = br"!D1 q qn 0 s r MS IC=1
.MODEL MS SRFF TH=2.5 HYSTWD=0.1 VOL=0 VOH=5 RIN=1Meg ROUT=10 LOGIC=POS
.end
";
        let translated = translate_simplis_deck(source)?;
        assert_eq!(translated.primitives, vec![TranslationKind::SrFlipFlop]);
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains("A___D1_EVENT") || text.contains("A__D1_EVENT"));
        assert!(text.contains("preprocess_logic_event(operation=6"));
        assert!(!text.contains(" SRFF "));
        Ok(())
    }

    #[test]
    fn hierarchical_init_routes_live_state_and_marks_stale_snapshot_entries() -> Result<()> {
        let source = br".INIT XTOP.S1=CLOSE
.INIT V(XTOP.C1)=1.25
.INIT I(XTOP.L1)=2m
.INIT XTOP.XBUF.A_BUF:IC=1
.INIT XGONE.S1=OPEN
XTOP a b ctrl TOP
.subckt TOP a b ctrl
S1 a b ctrl 0 MS IC=OPEN
C1 a b 1u
L1 a b 1u
XBUF ctrl q qb BUF
.MODEL MS VCSW RON=1 ROFF=1Meg TH=2 HYSTWD=0.2 LOGIC=POS
.subckt BUF inp q qb
A_BUF %gnd_ref 0 [ q qb ] [ inp ] MODEL=MB IC=0
.MODEL MB DIGITAL1 D_BUFFER RIN=10Meg ROUT=10 TH=2.5 HYSTWD=1 VOL=0 VOH=5 OUT_DELAY=0
.ends BUF
.ends TOP
.end
";
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains("B__S1_BRANCH a b"));
        assert!(text.contains("rx_ic=[ 1 ]"));
        assert!(text.contains("C1 a b 1u IC=1.25"));
        assert!(text.contains("L1 a b 1u IC=2m"));
        assert!(text.contains("B__A_BUF_INIT __A_BUF_DIFF 0 V='(time<=0 ? "));
        assert!(text.contains(
            "* ngspice-rs-preprocess: ignored stale SIMPLIS .INIT snapshot entry XGONE.S1=OPEN"
        ));
        assert!(
            !text
                .lines()
                .any(|line| line.trim_start().starts_with(".INIT"))
        );
        Ok(())
    }

    #[test]
    fn hierarchical_init_refuses_shared_subcircuit_mutation() {
        let source = br".INIT X1.S1=CLOSE
X1 a b ctrl CHILD
X2 c d ctrl CHILD
.subckt CHILD p n ctrl
S1 p n ctrl 0 MS IC=OPEN
.MODEL MS VCSW RON=1 ROFF=1Meg TH=2 HYSTWD=0.2 LOGIC=POS
.ends CHILD
.end
";
        let error = translate_simplis_deck(source)
            .err()
            .map(|value| value.to_string());
        assert!(error.is_some_and(|value| value.contains("enters shared subcircuit CHILD")));
    }

    #[test]
    fn init_override_replaces_device_initial_state() -> Result<()> {
        let source = br"S1 a 0 ctrl 0 MS IC=OPEN
.MODEL MS VCSW RON=1 ROFF=1Meg TH=2 HYSTWD=0.2 LOGIC=POS
.INIT S1=CLOSE
.end
";
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains("B__S1_BRANCH a 0"));
        assert!(text.contains("rx_ic=[ 1 ]"));
        assert!(!text.contains(".INIT"));
        Ok(())
    }

    #[test]
    fn prunes_fully_disconnected_numeric_subcircuit_island() -> Result<()> {
        let source = br".subckt T p n
XUSED p n CHILD$lib_0123456789abcdef01234567
XDEAD 43 44 CHILD$lib_0123456789abcdef01234567
.subckt CHILD$lib_0123456789abcdef01234567 a b
V1 a b 1
.ends CHILD$lib_0123456789abcdef01234567
.ends T
.end
";
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains("XUSED p n CHILD$lib_0123456789abcdef01234567"));
        assert!(text.contains(
            "pruned disconnected instance XDEAD 43 44 CHILD$lib_0123456789abcdef01234567"
        ));
        assert!(!text.lines().any(|line| line.starts_with("XDEAD ")));
        Ok(())
    }

    #[test]
    fn keeps_numeric_subcircuit_instance_when_a_node_is_referenced() -> Result<()> {
        let source = br".subckt T p n
XKEEP 43 44 CHILD$lib_0123456789abcdef01234567
R1 43 p 1k
.subckt CHILD$lib_0123456789abcdef01234567 a b
V1 a b 1
.ends CHILD$lib_0123456789abcdef01234567
.ends T
.end
";
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains("XKEEP 43 44 CHILD$lib_0123456789abcdef01234567"));
        Ok(())
    }

    #[test]
    fn translates_numbered_raw_logic_bang_devices() -> Result<()> {
        let source = br"V1 a 0 5
V2 b 0 5
!DAND out 0 a b MAND IC=0
.MODEL MAND AND2 RIN=10Meg ROUT=1024 VOL=0 VOH=1 HYSTWD=10m DELAY=0 TH=2.5 LOGIC=POS
R1 out 0 1k
.end
";
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert_eq!(translated.primitives, vec![TranslationKind::AndGate]);
        assert_eq!(
            translated.required_code_models,
            vec![CodeModelRuntime::Logic]
        );
        assert!(text.contains("A___DAND_EVENT %vd [ a 0 b 0 ]"));
        assert!(text.contains("preprocess_logic_event(operation=0"));
        assert!(!text.contains("!DAND"));
        assert!(!text.contains(".MODEL MAND AND2"));
        Ok(())
    }

    #[test]
    fn translates_numbered_nor_logic() -> Result<()> {
        let source = br"!DNOR out 0 a b c MNOR IC=0
.MODEL MNOR NOR3 RIN=1Meg ROUT=10 VOL=0 VOH=5 HYSTWD=0 DELAY=1n TH=2.5 LOGIC=POS
.end
";
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert_eq!(translated.primitives, vec![TranslationKind::OrGate]);
        assert!(text.contains("A___DNOR_EVENT"));
        assert!(text.contains("preprocess_logic_event(operation=3"));
        assert!(text.contains("inertial=false"));
        assert!(!text.lines().any(|line| line.trim_start().starts_with('!')));
        Ok(())
    }

    #[test]
    fn translates_raw_pwlc_charge_capacitor() -> Result<()> {
        let source = br"!CNON a 0 MC
.MODEL MC PWLC NSEG=2 X0=0 Y0=0 X1=1 Y1=10n X2=5 Y2=30n
.end
";
        let translated = translate_simplis_deck(source)?;
        assert_eq!(
            translated.primitives,
            vec![TranslationKind::ChargeCapacitor]
        );
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains("B__CNON_PWLC_V a 0 V='pwl(v(__CNON_PWLC_Q)"));
        assert!(text.contains("B__CNON_PWLC_I __CNON_PWLC_Q 0 I='-i(B__CNON_PWLC_V)'"));
        assert!(text.contains("C__CNON_PWLC_Q __CNON_PWLC_Q 0 "));
        assert!(!text.contains(" Q='"));
        assert!(!text.contains(" PWLC "));
        assert!(!text.contains("!CNON"));
        Ok(())
    }

    #[test]
    fn raw_pwlc_nonzero_initial_voltage_sets_normalized_charge() -> Result<()> {
        let source = br"!CNON a 0 MC IC=1
.MODEL MC PWLC NSEG=2 X0=-1 Y0=2p X1=0 Y1=4p X2=2 Y2=12p
.end
";
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        let state = text
            .lines()
            .find(|line| line.starts_with("C__CNON_PWLC_Q "))
            .context("missing explicit charge state")?;
        let value = state
            .split("IC=")
            .nth(1)
            .context("missing state initial charge")?
            .parse::<f64>()?;
        assert!((value - 0.6).abs() < 1e-15);
        Ok(())
    }

    #[test]
    fn rewrites_parameter_free_native_mixed_signal_bridge_connectors() -> Result<()> {
        let source = br".subckt T ain aout
A1 ain d AD
A2 d internal DA
B1 aout 0 V=v(internal)
.model AD adc_bridge
.model DA dac_bridge
.ends T
.end
";
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains("A1 [ain] [d] AD"));
        assert!(text.contains("A2 [d] [internal] DA"));
        assert!(text.contains(".model AD adc_bridge(in_low=1.00000000000000006e-1 in_high=9.00000000000000022e-1 rise_delay=1.00000000000000006e-9 fall_delay=1.00000000000000006e-9)"));
        assert!(text.contains(".model DA dac_bridge(out_low="));
        assert!(text.contains(" out_high=5."));
        Ok(())
    }

    #[test]
    fn rewrites_compatible_simetrix_adc_bridge_parameters() -> Result<()> {
        let source = br".subckt T ain dout params: lo=.3 hi=3.5 edge=4n
A1 ain dout AD
.model AD adc_bridge in_low={lo} in_high={hi} rise_delay=edge fall_delay={edge}
.ends T
.end
";
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains("A1 [ain] [dout] AD"));
        assert!(text.contains(
            ".model AD adc_bridge(in_low={lo} in_high={hi} rise_delay={edge} fall_delay={edge})"
        ));
        Ok(())
    }

    #[test]
    fn rich_simetrix_adc_bridge_load_semantics_fail_closed() {
        let source = br".subckt T ain dout
A1 ain dout AD
.model AD adc_bridge in_low=.3 in_high=3.5 out_low=0 out_high=5 clamp_res=10
.ends T
.end
";
        let error = translate_simplis_deck(source)
            .err()
            .map(|value| value.to_string());
        assert!(error.is_some_and(|value| {
            value.contains("requires explicit analog-load/clamp compatibility lowering")
        }));
    }

    #[test]
    fn rewrites_unloaded_rich_simetrix_dac_bridge() -> Result<()> {
        let source = br".subckt T d out params: rise=10n
A1 d control DA
S1 out 0 control 0 SW
.model DA dac_bridge out_high=5 t_rise=rise t_fall={rise} g_pullup=.02 g_pulldown=.03 knee_low=2 knee_high=3 v_smooth=.5
.model SW SW(Ron=1 Roff=1Meg Vt=2.5 Vh=.1)
.ends T
.end
";
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains("A1 [d] [control] DA"));
        assert!(text.contains("out_high=5"));
        assert!(text.contains("t_rise={rise}"));
        assert!(!text.contains("g_pullup"));
        assert!(!text.contains("knee_low"));
        Ok(())
    }

    #[test]
    fn loaded_simetrix_dac_bridge_fails_closed() {
        let source = br".subckt T d out
A1 d internal DA
R1 internal out 1k
.model DA dac_bridge out_high=5 g_pullup=.02
.ends T
.end
";
        let error = translate_simplis_deck(source)
            .err()
            .map(|value| value.to_string());
        assert!(
            error.is_some_and(|value| value.contains("drives loaded or externally visible node"))
        );
    }

    #[test]
    fn joins_unambiguous_bare_model_parameter_continuation() {
        let input = ".MODEL ZX5T849G NPN\nIS=1.5E-12 NF=1 BF=210\n+IKF=8 VAF=100\nR1 1 0 1k\n";
        let statements = logical_statements(input);
        assert_eq!(statements.len(), 2);
        assert_eq!(
            statements[0],
            ".MODEL ZX5T849G NPN\n+IS=1.5E-12 NF=1 BF=210\n+IKF=8 VAF=100"
        );
        assert_eq!(statements[1], "R1 1 0 1k");
    }

    #[test]
    fn translates_simetrix_adc_schmitt_with_runtime_thresholds_and_delays() -> Result<()> {
        let source = br".subckt T in out thresh=2 hystwd=1 delay=3n
A1 in out SCH
.model SCH adc_schmitt in_low={thresh-hystwd/2} in_high={thresh+hystwd/2} rise_delay={delay} fall_delay={delay}
.ends T
.end
";
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains("A__A1_SCHMITT_HYST"));
        assert!(text.contains("A__A1_SCHMITT_ADC [__A1_SCHMITT_STATE] [__A1_SCHMITT_DIGITAL]"));
        assert!(text.contains("d_buffer(rise_delay={delay} fall_delay={delay})"));
        assert!(text.contains("thresh-hystwd/2"));
        assert!(text.contains("thresh+hystwd/2"));
        assert!(!text.to_ascii_uppercase().contains(" ADC_SCHMITT"));
        Ok(())
    }

    #[test]
    fn translates_simetrix_adc_schmitt_output_family() -> Result<()> {
        let source = br#".subckt T inp out
A1 inp out SCH
.model SCH adc_schmitt in_low=1 in_high=2 out_family="HC"
.ends T
X1 a b T
.end
"#;
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains("d_buffer(rise_delay="));
        assert!(text.contains(" family=HC)"));
        Ok(())
    }

    #[test]
    fn translates_simetrix_timed_vswitch() -> Result<()> {
        let source = br".subckt T p n cp cn params: RON=10 ROFF=1Meg TH=2.5 H=0.5 TON=2n TOFF=3n TDON=1n TDOFF=2n IC=1
S1 p n cp cn SW IC={IC}
.model SW VSWITCH VT={TH} VH={H} TON={TON} TOFF={TOFF} TD_ON={TDON} TD_OFF={TDOFF} RON={RON} ROFF={ROFF} GIN=0
.ends T
.end
";
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert_eq!(
            translated.required_code_models,
            vec![CodeModelRuntime::Logic]
        );
        assert_eq!(
            translated.primitives,
            vec![TranslationKind::TimedVoltageControlledSwitch]
        );
        assert!(text.contains("A__S1_TIMED %vd(cp cn) %gd(p n) __S1_TIMED_M"));
        assert!(text.contains("preprocess_timed_switch(ron={RON} roff={ROFF} vt={TH} vh={H} ton={TON} toff={TOFF} td_on={TDON} td_off={TDOFF}"));
        assert!(text.contains("ic={IC}"));
        assert!(text.contains("G__S1_TIMED_GIN cp cn cp cn 0.00000000000000000e0"));
        assert!(!text.contains("delay(delay="));
        assert!(!text.to_ascii_uppercase().contains(" VSWITCH"));
        Ok(())
    }

    #[test]
    fn timed_switch_preserves_nested_runtime_expressions_and_tolerance() -> Result<()> {
        let source = br".subckt T p n cp cn params: tol=1p R=10
S1 p n cp cn SW IC=1
.model SW VSWITCH(RON={max(R,1)}, ROFF=1Meg, VT=2.5, TIMETOL={max(tol,2p)}, TD=4n, TD_OFF=3n)
.ends T
.end
";
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains("ron={max(R,1)}"));
        assert!(text.contains("timetol={max(tol,2p)}"));
        assert!(text.contains("td_on=4.00000000000000025e-9"));
        assert!(
            text.contains("td_off=3.00000000000000039e-9")
                || text.contains("td_off=3.00000000000000000e-9")
        );
        Ok(())
    }

    #[test]
    fn timed_switch_rejects_negative_tolerance_and_unsupported_parameters() {
        for extra in [
            "TIMETOL=-1n",
            "TIMETOL=0",
            "TIMETOL=0.5f",
            "TON=0",
            "UNKNOWN=1",
        ] {
            let source = format!("S1 a 0 c 0 M IC=0\n.MODEL M VSWITCH VT=2.5 {extra}\n.end\n");
            assert!(translate_simplis_deck(source.as_bytes()).is_err());
        }
    }

    #[test]
    fn translates_pspice_vswitch_to_limited_log_aswitch() -> Result<()> {
        let source = br"S1 out 0 ctrl 0 MS
.MODEL MS VSWITCH(VON=0.205 VOFF=0.195 RON=0.001 ROFF=1Meg)
S2 out2 0 ctrl2 0 INV
.MODEL INV VSWITCH(VON=4.9 VOFF=5 RON=0.01 ROFF=3.5E11)
S3 out3 0 (ctrl3,0) MS
S4 out4 0 (ctrl4 0) MS
S5 out5 0 ctrl5 0 INV_R
.MODEL INV_R VSWITCH(VON=2 VOFF=0.5 RON=1e9 ROFF=0.01)
.end
";
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains("A__S1 %vd(ctrl 0) %gd(out 0) __S1_ASW"));
        assert!(text.contains("cntl_off=1.95000000000000007e-1"));
        assert!(text.contains("cntl_on=2.04999999999999988e-1"));
        assert!(text.contains("log=TRUE limit=TRUE"));
        assert!(text.contains("A__S2 %vd(ctrl2 0) %gd(out2 0) __S2_ASW"));
        assert!(text.contains("A__S3 %vd(ctrl3 0) %gd(out3 0) __S3_ASW"));
        assert!(text.contains("A__S4 %vd(ctrl4 0) %gd(out4 0) __S4_ASW"));
        assert!(text.contains("cntl_off=5.00000000000000000e0"));
        assert!(text.contains("cntl_on=4.90000000000000036e0"));
        assert!(text.contains("A__S5 %vd(ctrl5 0) %gd(out5 0) __S5_ASW"));
        assert!(text.contains("r_off=1.00000000000000002e-2"));
        assert!(text.contains("r_on=1.00000000000000000e9"));
        assert!(!text.to_ascii_uppercase().contains(" VSWITCH"));
        Ok(())
    }

    #[test]
    fn translates_runtime_parameter_pspice_vswitch() -> Result<()> {
        let source = br".subckt T p n c r ron=1m roff=1Meg von=1 voff=0
S1 p n c r SW
.model SW VSWITCH RON={ron} ROFF={roff} VON={von} VOFF={voff}
.ends T
.end
";
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains("cntl_off={voff}"));
        assert!(text.contains("cntl_on={von}"));
        assert!(text.contains("r_off={roff}"));
        assert!(text.contains("r_on={ron}"));
        Ok(())
    }

    #[test]
    fn translates_raw_vcsw() -> Result<()> {
        let source = br"VCTRL ctrl 0 3
S1 a 0 ctrl 0 MS IC=OPEN
.MODEL MS VCSW RON=1 ROFF=1Meg TH=2 HYSTWD=0.2 LOGIC=POS
R1 a 0 1k
.end
";
        let translated = translate_simplis_deck(source)?;
        assert_eq!(
            translated.primitives,
            vec![TranslationKind::HystereticSwitch]
        );
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains("preprocess_logic_event(operation=0"));
        assert!(text.contains("rx_ic=[ 0 ]"));
        assert!(!text.contains(" VCSW "));
        Ok(())
    }

    #[test]
    fn switch_voltage_control_resolves_named_branch_and_node_pairs() -> Result<()> {
        let source = br"Vctrl c r 5
S1 p n Vctrl MS IC=OPEN
S2 p n (c,r) MS IC=CLOSE
Q1 p n Vctrl MQ IC=OPEN
Q2 p n (c,r) MQ IC=CLOSE
.MODEL MS VCSW RON=1 ROFF=1Meg TH=2.5 HYSTWD=1 LOGIC=POS
.MODEL MQ VCQPOS VSAT=.1 RSAT=1 ROFF=1Meg GAIN=1 TH=2.5 HYSTWD=1 LOGIC=POS LEVEL=2
.end
";
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        for stem in ["S1", "S2", "Q1", "Q2"] {
            assert!(text.contains(&format!("B__{stem}_CTRL __{stem}_CTRL 0 V='v(c,r)'")));
        }
        assert_eq!(translated.primitives.len(), 4);
        Ok(())
    }

    #[test]
    fn switch_unknown_named_voltage_branch_fails() {
        let source = br"S1 p n missing MS IC=OPEN
.MODEL MS VCSW RON=1 ROFF=1Meg TH=2.5 HYSTWD=1 LOGIC=POS
.end
";
        assert!(translate_simplis_deck(source).is_err());
    }

    #[test]
    fn preserves_near_ideal_raw_vcsw_ron() -> Result<()> {
        let source = br"VCTRL ctrl 0 3
S1 a 0 ctrl 0 MS IC=OPEN
.MODEL MS VCSW RON=1n ROFF=1Meg TH=2 HYSTWD=0.2 LOGIC=POS
R1 a 0 1k
.end
";
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains("1.00000000000000006e-9"));
        assert!(text.contains("B__S1_BRANCH"));
        Ok(())
    }

    #[test]
    fn translates_raw_comp_initial_state() -> Result<()> {
        let source = br"V1 in 0 1
V2 ref 0 0
!D1 out 0 in ref MC IC=1
.model MC COMP RIN=10Meg ROUT=50 VOL=0 VOH=5 HYSTWD=0.1 DELAY=0
R1 out 0 1k
.end
";
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains("B__D1_STATE_INPUT"));
        assert!(text.contains("A__D1_HYST"));
        assert!(text.contains("R__D1_OUT"));
        assert!(!text.contains(".model MC COMP"));
        assert_eq!(translated.primitives, vec![TranslationKind::Comparator]);
        Ok(())
    }

    #[test]
    fn translates_expanded_ipwlr_deck_without_dropping_root_circuit() -> Result<()> {
        let source = br"R1 10 0 10k
V1 10 0 1
X1 10 0 LIMIT$1
.subckt LIMIT$1 p n
!R1 p n M IC=2
.model M IPWLR NSEG=2 X0=-1 Y0=-10 X1=0 Y1=0 X2=1 Y2=10
.ends LIMIT$1
.tran 1m 0
.end
";
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains("R1 10 0 10k"));
        assert!(text.contains("V__R1_IPWLR_SENSE p __R1_IPWLR_MID 0"));
        assert!(!text.to_ascii_uppercase().contains(".MODEL M IPWLR"));
        assert!(!text.lines().any(|line| line.trim_start().starts_with('!')));
        assert!(!text.contains(".tran"));
        assert_eq!(translated.primitives, vec![TranslationKind::Ipwlr]);
        Ok(())
    }

    #[test]
    fn accepts_generated_simplis_specialization_identifier() {
        assert!(residual_simplis_reason("X1 1 0 SIMPLIS_HELPER$12").is_none());
        assert!(
            residual_simplis_reason("X1 1 0 SIMPLIS_HELPER$sp_0123456789abcdef01234567").is_none()
        );
        assert!(
            residual_simplis_reason("X1 1 0 SIMPLIS_HELPER$lib_0123456789abcdef01234567").is_none()
        );
        assert!(residual_simplis_reason("X1 1 0 SIMPLIS_HELPER$sp_notahash").is_some());
        assert!(residual_simplis_reason("X1 1 0 SIMPLIS_HELPER").is_some());
    }

    #[test]
    fn rejects_unlowered_standard_library_macro() {
        let source = br".subckt TOP a b c d e
XU a b c d e SIMPLIS_DIGI1_OR2_Y params: IC=0
.ends TOP
";
        let error = match translate_simplis_deck(source) {
            Ok(_) => String::new(),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("unlowered SIMPLIS helper/macro"));
    }
    #[test]
    fn translates_inline_legacy_pwl_resistor() -> Result<()> {
        let source = br"!R1 a 0 NSEG=2 X0=0 Y0=0 X1=1 Y1=1m X2=2 Y2=1 IC=1
.end
";
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert_eq!(translated.primitives, vec![TranslationKind::Vpwlr]);
        assert!(text.contains("BR1 a 0 I=pwl(v(a,0)"));
        assert!(!text.contains("NSEG="));
        Ok(())
    }

    #[test]
    fn translates_legacy_inline_comparator_and_totem_with_snapshot_state() -> Result<()> {
        let source = br"X$U1 out 0 inp inn IC=0 RIN=10Meg ROUT=1 HYSTWD=100m VOL=0 VOH=10 DELAY=0 pinnames: OUT RTN INP INN ngspice_preprocess_init: !DCOMP=1
X$U2 top out bot inp 0 pinnames: TOP VOUT BOT IN RTN ngspice_preprocess_init: SW_BOT=OPEN SW_TOP=CLOSE
.end
";
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(translated.primitives.contains(&TranslationKind::Comparator));
        assert!(
            translated
                .primitives
                .contains(&TranslationKind::VoltageControlledSwitch)
        );
        assert!(text.contains("B__X_U1_STATE_INPUT"));
        assert!(text.contains("time<=0 ? 1.05000000000000004e0"));
        assert!(text.contains("S__X_U2_TOP __X_U2_TOP_MID out inp 0 __X_U2_TOP_SW ON"));
        assert!(text.contains("S__X_U2_BOT out __X_U2_BOT_MID 0 inp __X_U2_BOT_SW OFF"));
        assert!(!text.contains("ngspice_preprocess_init:"));
        assert!(!text.contains("pinnames:"));
        Ok(())
    }

    #[test]
    fn comments_bare_vendor_spice_macro_model_banner() -> Result<()> {
        let source = b"SYNTHETIC SPICE Macro-model\nR1 a 0 1k\n.end\n";
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains("* SYNTHETIC SPICE Macro-model"));
        assert!(
            !text
                .lines()
                .any(|line| line == "SYNTHETIC SPICE Macro-model")
        );
        Ok(())
    }

    #[test]
    fn rejects_unbound_source_initialization_but_keeps_native_ic() -> Result<()> {
        for source in [
            br"C1 a 0 10n IC=2 BRANCH={IF(ANALYSIS=2,1,0)} ICMODE=0
.end
"
            .as_slice(),
            b"L1 b 0 1u IC=3 BRANCH=0
.end
"
            .as_slice(),
        ] {
            let error = translate_simplis_deck(source)
                .err()
                .context("source initialization silently lost")?;
            assert!(format!("{error:#}").contains("unbound source initialization"));
        }
        let translated = translate_simplis_deck(
            b"C1 a 0 10n IC=2
L1 b 0 1u IC=3
.end
",
        )?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains("C1 a 0 10n IC=2"));
        assert!(text.contains("L1 b 0 1u IC=3"));
        Ok(())
    }

    #[test]
    fn translates_simetrix_level17_discrete_mosfet() -> Result<()> {
        let source = br".subckt SYNTHETIC_LEVEL17 D G S
.model mosfet NMOS( LEVEL=17 VTO=3.5 RS=0.125 KP=3 RD=1.75 TC1RD=0.015625 RG=10 IS=1e-35 LIMIT(VDS)=(*, 400)
+ CGDMAX=2.00E-09 CGDMIN=2.00E-11 XG2CGD=0.75 XG1CGD=0.125 CBD=8.00E-11 VTCGD=0)
M1 D G S S mosfet
.ends SYNTHETIC_LEVEL17
.end
";
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert_eq!(
            translated.primitives,
            vec![TranslationKind::SimetrixMosfetLevel17]
        );
        assert!(text.contains(".model __L17_MOSFET NMOS(level=1"));
        assert!(text.contains("R__M1_RD D __M1_DINT"));
        assert!(text.contains("tc1=1.56250000000000000e-2"));
        assert!(text.contains("R__M1_RG G __M1_GINT"));
        assert!(text.contains("M1 __M1_DINT __M1_GINT S S __L17_mosfet L=1 W=1"));
        assert!(text.contains("C__M1_CGD __M1_GINT __M1_DINT Q='"));
        assert!(text.contains("omitted SIMetrix LEVEL=17 LIMIT(VDS)=(*, 400) SOA annotation"));
        assert!(!text.contains(".model mosfet NMOS( LEVEL=17"));
        assert!(!text.contains("CGDMAX="));
        Ok(())
    }

    #[test]
    fn resolves_simetrix_v_dollar_current_sense_alias() -> Result<()> {
        let source = br"H1 out 0 H1$TP_CCVS 2
V$H1$TP_CCVS 0 sense 0
R1 sense 0 1k
.end
";
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains("B__H1 out 0 V='2.00000000000000000e0*i(V$H1$TP_CCVS)'"));
        Ok(())
    }
    #[test]
    fn preserves_uncoupled_negative_fitted_inductor() -> Result<()> {
        let source = br"Lsource a b -1p
Rfit b 0 -10m
.end
";
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains("Lsource a b -1p"));
        assert!(text.contains("Rfit b 0 -10m"));
        Ok(())
    }

    #[test]
    fn inductor_lookup_ignores_embedded_logic_language_lines() -> Result<()> {
        let items = parse_items("Latch = Data ;\nLEVEL (WIDTH=1) Latch ;\n")?;
        assert!(raw_inductor_values(&items)?.is_empty());
        Ok(())
    }

    #[test]
    fn native_inductor_runtime_values_do_not_pollute_mutual_lookup() -> Result<()> {
        let source = br".subckt T 1 2 3 4
L1 1 2 {IND}
L2 3 4 L=0.12N
.ends T
.end
";
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains("L1 1 2 {IND}"));
        assert!(text.contains("L2 3 4 L=0.12N"));
        Ok(())
    }

    #[test]
    fn mutual_inductance_rejects_nonpositive_self_inductance() {
        let source = br"L1 a 0 -1u
L2 b 0 1u
M-L1-L2 500n
.end
";
        let error = translate_simplis_deck(source)
            .err()
            .map(|error| error.to_string());
        assert!(
            error.is_some_and(|message| message.contains("requires positive self-inductances"))
        );
    }
    #[test]
    fn rewrites_pspice_bsim3_level7_to_ngspice_level8() -> Result<()> {
        let source = br".subckt T d g s
.MODEL Bsim3 NMOS(LEVEL= 7 VERSION= 3.1 MOBMOD=3 CAPMOD=2 PARAMCHK=1 NQSMOD=0 TOX=300e-10)
M1 d g s s Bsim3 W=2 L=0.5u
.ends T
.end
";
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains("LEVEL=8"));
        assert!(text.contains("VERSION= 3.1 MOBMOD=3 CAPMOD=2"));
        assert!(!text.contains("LEVEL=7"));
        assert!(!text.contains("LEVEL= 7"));
        Ok(())
    }

    #[test]
    fn level7_with_discrete_cgd_signature_is_not_rewritten_as_bsim3() -> Result<()> {
        let source = ".MODEL OLD NMOS (LEVEL=7 VTO=3 KP=2 CGDMAX=1n CGDMIN=10p XG1CGD=0.1 XG2CGD=0.5 VTCGD=0)";
        assert!(rewrite_simetrix_model_compat(source)?.is_none());
        Ok(())
    }

    #[test]
    fn moves_simetrix_diode_t_abs_to_instance_temp() -> Result<()> {
        let source = br".subckt T a b
D1 a b DM
.MODEL DM D (IS=1e-9 N=1 T_ABS=25)
.ends T
.end
";
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains("D1 a b DM TEMP=2.50000000000000000e1"));
        assert!(text.contains(".MODEL DM D (IS=1e-9 N=1)"));
        assert!(!text.to_ascii_uppercase().contains("T_ABS="));
        Ok(())
    }

    #[test]
    fn moves_simetrix_resistor_t_abs_to_instance_temp() -> Result<()> {
        let source = br".subckt T a b
Rideal a b Rideal 1k
Rnoise a b Rnoise 2k
.MODEL Rideal RES(T_ABS=-273)
.MODEL Rnoise RES T_ABS=27
.ends T
.end
";
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains("Rideal a b Rideal 1k TEMP=-2.73000000000000000e2"));
        assert!(text.contains("Rnoise a b Rnoise 2k TEMP=2.70000000000000000e1"));
        assert!(text.contains(".MODEL Rideal RES()"));
        assert!(text.contains(".MODEL Rnoise RES"));
        assert!(!text.to_ascii_uppercase().contains("T_ABS="));
        Ok(())
    }

    #[test]
    fn preserves_simetrix_sub_absolute_resistor_temperature() -> Result<()> {
        let source = br"R1 a 0 Rcold 1k
.MODEL Rcold RES(T_ABS=-373)
.end
";
        let translated = translate_simplis_deck(source)?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains("R1 a 0 Rcold 1k TEMP=-3.73000000000000000e2"));
        assert!(!text.to_ascii_uppercase().contains("T_ABS="));
        Ok(())
    }
}
