//! Discover actual compiled-ALB pin bundles through nested SPICE X instances.
//! Source annotations need only identify the analog and digital state nets,
//! their family, direction and supply. No physical triples are hand-mapped.

use super::{
    AnalogBoundaryPin, BridgeCatalog, DigitalPinDirection, LogicFamilyAssignment,
    NativeDigitalBundle, NativePinWiring, OutputResistance, PlannedAnalogBridge, Result, bail,
    plan_analog_bridges,
};
use anyhow::{Context, ensure};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug)]
struct Instance {
    name: String,
    fields: Vec<String>,
}
#[derive(Clone, Debug)]
struct Subckt {
    path: Vec<String>,
    ports: Vec<String>,
    instances: Vec<Instance>,
    statements: Vec<String>,
}
#[derive(Default)]
struct Circuit {
    definitions: BTreeMap<Vec<String>, Subckt>,
    root: Vec<Instance>,
    root_statements: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DiscoveredAlbPin {
    pub id: String,
    pub direction: DigitalPinDirection,
    /// None means that at least one part of the bundle is private to
    /// a subcircuit and cannot be bridged at root without rewriting it.
    pub bundle: Option<NativeDigitalBundle>,
    pub output_res_ohm: Option<OutputResistance>,
}

/// A public analog net connected to a digital STATE node. Physical pin
/// occurrences and sink/source metadata are discovered from the source.
#[derive(Clone, Debug)]
pub struct AnalogDigitalConnection {
    pub analog_net: String,
    pub digital_net: String,
    pub direction: DigitalPinDirection,
    pub family: LogicFamilyAssignment,
    pub supply_v: f64,
}

#[derive(Clone)]
enum Node {
    Root(String),
    Local,
}

fn key(s: &str) -> String {
    s.to_ascii_uppercase()
}
fn node(s: &str) -> Result<String> {
    crate::validate_node(s)?;
    Ok(key(s))
}

fn statements(input: &str) -> Result<Vec<String>> {
    let mut result = Vec::<String>::new();
    let mut control = false;
    for (num, line) in input.lines().enumerate() {
        let mut quote = None;
        let cutoff = line.char_indices().find_map(|(at, ch)| {
            if ch == char::from(39) || ch == char::from(34) {
                if quote == Some(ch) {
                    quote = None;
                } else if quote.is_none() {
                    quote = Some(ch);
                }
            }
            (ch == ';' && quote.is_none()).then_some(at)
        });
        let s = line[..cutoff.unwrap_or(line.len())].trim();
        if s.is_empty() || s.starts_with(['*', ';']) {
            continue;
        }
        if s.eq_ignore_ascii_case(".control") {
            control = true;
            continue;
        }
        if s.eq_ignore_ascii_case(".endc") {
            ensure!(control, "orphan .endc at line {}", num + 1);
            control = false;
            continue;
        }
        if control {
            continue;
        }
        if let Some(continued) = s.strip_prefix('+') {
            let previous = result
                .last_mut()
                .with_context(|| format!("orphan continuation at line {}", num + 1))?;
            previous.push(' ');
            previous.push_str(continued.trim());
        } else {
            result.push(s.to_owned());
        }
    }
    ensure!(!control, "unterminated .control block");
    Ok(result)
}

fn header_ports(fields: &[&str]) -> Result<Vec<String>> {
    ensure!(fields.len() >= 2, "nameless .subckt");
    let mut ports = Vec::new();
    for s in fields.iter().skip(2) {
        if s.eq_ignore_ascii_case("params:") || s.eq_ignore_ascii_case("params") || s.contains('=')
        {
            break;
        }
        ports.push(node(s)?);
    }
    ensure!(
        ports.iter().collect::<BTreeSet<_>>().len() == ports.len(),
        "duplicate .subckt formal port"
    );
    Ok(ports)
}

fn parse_sources(deck: &str, definitions: &[&str]) -> Result<Circuit> {
    let mut circuit = Circuit::default();
    for (part, source) in std::iter::once(deck)
        .chain(definitions.iter().copied())
        .enumerate()
    {
        let mut scope = Vec::<String>::new();
        for statement in statements(source)? {
            let fields = statement.split_ascii_whitespace().collect::<Vec<_>>();
            let first = fields[0];
            if first.eq_ignore_ascii_case(".subckt") {
                ensure!(fields.len() >= 2, ".subckt without model name");
                let mut path = scope.clone();
                path.push(key(fields[1]));
                let def = Subckt {
                    path: path.clone(),
                    ports: header_ports(&fields)?,
                    instances: Vec::new(),
                    statements: Vec::new(),
                };
                ensure!(
                    circuit.definitions.insert(path.clone(), def).is_none(),
                    "duplicate subcircuit definition {}",
                    path.join(".")
                );
                scope = path;
            } else if first.eq_ignore_ascii_case(".ends") {
                let closing = scope.pop().context("unmatched .ends")?;
                if let Some(name) = fields.get(1) {
                    ensure!(key(name) == closing, ".ends {name} mismatches {closing}");
                }
            } else if first.starts_with(['X', 'x']) && first.len() > 1 {
                let entry = Instance {
                    name: key(first),
                    fields: fields[1..].iter().map(|s| (*s).to_owned()).collect(),
                };
                if scope.is_empty() {
                    ensure!(part == 0, "definitions file contains root-level X instance");
                    circuit.root.push(entry);
                } else {
                    circuit
                        .definitions
                        .get_mut(&scope)
                        .context("missing subcircuit scope")?
                        .instances
                        .push(entry);
                }
            } else if !scope.is_empty() {
                circuit
                    .definitions
                    .get_mut(&scope)
                    .context("missing subcircuit scope")?
                    .statements
                    .push(statement);
            } else if part == 0 {
                circuit.root_statements.push(statement);
            }
        }
        ensure!(scope.is_empty(), "unterminated .subckt in source {part}");
    }
    Ok(circuit)
}

fn lookup<'a>(circuit: &'a Circuit, scope: &[String], name: &str) -> Option<&'a Subckt> {
    let mut context = scope.to_vec();
    loop {
        let mut target = context.clone();
        target.push(key(name));
        if let Some(def) = circuit.definitions.get(&target) {
            return Some(def);
        }
        if context.pop().is_none() {
            break;
        }
    }
    None
}

fn resolve<'a>(
    circuit: &'a Circuit,
    scope: &[String],
    instance: &Instance,
) -> Result<(&'a Subckt, Vec<String>)> {
    let mut choices = Vec::new();
    for (i, word) in instance.fields.iter().enumerate() {
        let Some(def) = lookup(circuit, scope, word) else {
            continue;
        };
        if i != def.ports.len() {
            continue;
        }
        if instance.fields.get(i + 1).is_none_or(|s| {
            s.eq_ignore_ascii_case("params:") || s.eq_ignore_ascii_case("params") || s.contains('=')
        }) {
            choices.push((
                def,
                instance.fields[..i]
                    .iter()
                    .map(|s| key(s))
                    .collect::<Vec<_>>(),
            ));
        }
    }
    match choices.as_slice() {
        [(def, pins)] => Ok((def, pins.clone())),
        [] => bail!(
            "cannot resolve X{} in scope {scope:?}; missing or invalid subckt/port layout",
            instance.name
        ),
        _ => bail!("ambiguous X{} subcircuit resolution", instance.name),
    }
}

fn alb_signature(ports: &[String]) -> Option<(usize, usize)> {
    let mut at = 0;
    let mut counts = [0usize; 2];
    for (i, prefix) in ["SR_IN", "SR_OUT"].iter().enumerate() {
        while ports
            .get(at)
            .is_some_and(|s| s == &format!("{prefix}{}", counts[i]))
        {
            let n = counts[i];
            if ports.get(at + 1)? != &format!("{prefix}{n}_SINK")
                || ports.get(at + 2)? != &format!("{prefix}{n}_SOURCE")
            {
                return None;
            }
            at += 3;
            counts[i] += 1;
        }
    }
    (at == ports.len() && counts[1] > 0).then_some(counts.into())
}

/// Check every fan-in against the compiler's actual digital-net namespace.
/// Only already-emitted stages and output drivers can be read; an undeclared
/// node, metadata terminal, malformed vector, or self-reference is not a
/// trusted compiled ALB source even if the destination ABI is correct.
fn validate_compiled_lut_sources(
    fields: &[&str],
    inputs: usize,
    earlier_stages: &BTreeSet<usize>,
    earlier_outputs: &BTreeSet<usize>,
) -> Result<bool> {
    let end = fields
        .len()
        .checked_sub(5)
        .context("missing compiled ALB LUT physical terminals")?;
    ensure!(end >= 2, "missing compiled ALB LUT source vector");
    let vector = &fields[1..end];
    ensure!(
        vector.len() <= 16,
        "compiled ALB LUT exceeds native 16-source limit"
    );
    let mut uses_tie = false;
    for (index, token) in vector.iter().enumerate() {
        let token = if index == 0 {
            token
                .strip_prefix('[')
                .context("compiled ALB LUT source vector lacks opening bracket")?
        } else {
            token
        };
        let token = if index + 1 == vector.len() {
            token
                .strip_suffix(']')
                .context("compiled ALB LUT source vector lacks closing bracket")?
        } else {
            token
        };
        ensure!(
            !token.is_empty() && !token.contains(['[', ']']),
            "invalid compiled ALB LUT source token {token:?}"
        );
        let source = key(token);
        if source == "SR_TIE0" {
            ensure!(
                vector.len() == 1,
                "constant-zero ALB input must be the only LUT source"
            );
            uses_tie = true;
            continue;
        }
        let valid = source
            .strip_prefix("SR_IN")
            .and_then(|digits| digits.parse::<usize>().ok())
            .is_some_and(|pin| pin < inputs && source == format!("SR_IN{pin}"))
            || source
                .strip_prefix("SR_STAGE")
                .and_then(|digits| digits.parse::<usize>().ok())
                .is_some_and(|stage| {
                    earlier_stages.contains(&stage) && source == format!("SR_STAGE{stage}")
                })
            || source
                .strip_prefix("SR_OUT")
                .and_then(|digits| digits.parse::<usize>().ok())
                .is_some_and(|pin| {
                    earlier_outputs.contains(&pin) && source == format!("SR_OUT{pin}")
                });
        ensure!(
            valid,
            "undeclared or non-emitted compiled ALB LUT source {source}"
        );
    }
    Ok(uses_tie)
}

/// Validate the emitter's stage/output LUT port order and global stage index.
fn validate_compiled_alb_lut_device(
    device: &str,
    name: &str,
    fields: &[&str],
    outputs: usize,
    bit_indices: &mut BTreeSet<usize>,
    output_pins: &mut BTreeSet<usize>,
) -> Result<()> {
    let kind_and_index = name
        .strip_prefix("ASR_S")
        .map(|digits| ('S', digits))
        .or_else(|| name.strip_prefix("ASR_O").map(|digits| ('O', digits)));
    let (kind, digits) = kind_and_index
        .with_context(|| format!("unexpected code-model device {device} in compiled ALB"))?;
    let index = digits
        .parse::<usize>()
        .with_context(|| format!("invalid ALB stage/output index on {device}"))?;
    ensure!(
        name == format!("ASR_{kind}{index}") && bit_indices.insert(index) && fields.len() >= 7,
        "invalid or duplicate compiled ALB LUT device {device}"
    );
    let at = fields.len() - 5;
    let expected_state = if kind == 'S' {
        format!("SR_STAGE{index}")
    } else {
        let output = key(fields[at])
            .strip_prefix("SR_OUT")
            .and_then(|digits| digits.parse::<usize>().ok())
            .with_context(|| format!("invalid compiled ALB output target on {device}"))?;
        ensure!(
            output < outputs && output_pins.insert(output),
            "invalid or duplicate ALB output target on {device}"
        );
        format!("SR_OUT{output}")
    };
    ensure!(
        fields[1].starts_with('[')
            && fields[at - 1].ends_with(']')
            && fields[at].eq_ignore_ascii_case(&expected_state)
            && fields[at + 1].eq_ignore_ascii_case(&format!("{expected_state}_SINK"))
            && fields[at + 2].eq_ignore_ascii_case(&format!("{expected_state}_SOURCE"))
            && fields[at + 3] == "0"
            && fields[at + 4].eq_ignore_ascii_case(&format!("MSR_{kind}{index}")),
        "invalid compiled ALB LUT physical/clock/model ABI for {device}"
    );
    Ok(())
}

/// A matching port signature alone does not establish that a subcircuit was
/// emitted by the native ALB compiler. Check the complete emitted device ABI
/// before treating its internal A devices as trusted implementations.
fn validate_compiled_alb_device_abi(
    def: &Subckt,
    inputs: usize,
    outputs: usize,
) -> Result<Vec<OutputResistance>> {
    ensure!(
        def.instances.is_empty(),
        "compiled ALB {} contains non-emitted X instances",
        def.path.join(".")
    );
    let mut models = BTreeMap::<String, String>::new();
    for statement in &def.statements {
        let fields = statement.split_ascii_whitespace().collect::<Vec<_>>();
        let first = fields.first().copied().unwrap_or_default();
        if first.eq_ignore_ascii_case(".model") {
            ensure!(
                fields.len() >= 3,
                "malformed native model declaration in ALB {}",
                def.path.join(".")
            );
            let kind = fields[2].split('(').next().unwrap_or_default();
            ensure!(
                !kind.is_empty() && models.insert(key(fields[1]), key(kind)).is_none(),
                "duplicate or malformed native model {} in ALB {}",
                fields[1],
                def.path.join(".")
            );
        } else {
            ensure!(
                first.starts_with(['A', 'a']) && first.len() > 1,
                "non-emitted statement {first} in compiled ALB {}",
                def.path.join(".")
            );
        }
    }
    validate_compiled_alb_devices(def, inputs, outputs, &models)
}

fn validate_input_load(
    name: &str,
    fields: &[&str],
    index: usize,
    inputs: usize,
    input_loads: &mut BTreeSet<usize>,
) -> Result<()> {
    let expected = [
        format!("SR_IN{index}"),
        format!("SR_IN{index}_SINK"),
        format!("SR_IN{index}_SOURCE"),
    ];
    ensure!(
        name == format!("ASR_LOAD{index}")
            && index < inputs
            && fields.len() == 5
            && fields[1..4]
                .iter()
                .zip(&expected)
                .all(|(found, expected)| found.eq_ignore_ascii_case(expected))
            && fields[4].eq_ignore_ascii_case("MSR_INPUT_LOAD")
            && input_loads.insert(index),
        "invalid compiled ALB input load {name}"
    );
    Ok(())
}

fn validate_stateful_model(def: &Subckt) -> Result<OutputResistance> {
    let model = def
        .statements
        .iter()
        .find(|statement| {
            let mut words = statement.split_ascii_whitespace();
            words
                .next()
                .is_some_and(|word| word.eq_ignore_ascii_case(".model"))
                && words
                    .next()
                    .is_some_and(|word| word.eq_ignore_ascii_case("Msr_stateful"))
        })
        .context("missing stateful ALB model declaration")?;
    ensure!(
        model_numeric_parameter(model, "INPUT_LOAD")? == 0.0,
        "compiled stateful ALB must not duplicate its physical input loads"
    );
    for name in [
        "REG_DELAY",
        "PORT_DELAY",
        "SETUP_TIME",
        "HOLD_TIME",
        "MIN_CLOCK",
        "USER_SCALE",
    ] {
        model_numeric_parameter(model, name)?;
    }
    compiled_output_resistance(model)
}

/// Validate every emitted A-device and its source/physical model mapping.
fn validate_compiled_alb_devices(
    def: &Subckt,
    inputs: usize,
    outputs: usize,
    models: &BTreeMap<String, String>,
) -> Result<Vec<OutputResistance>> {
    let mut stateful_present = false;
    let mut input_loads = BTreeSet::new();
    let mut bit_indices = BTreeSet::new();
    let mut stage_indices = BTreeSet::new();
    let mut output_pins = BTreeSet::new();
    let mut tie_present = false;
    let mut tie_used = false;
    for statement in &def.statements {
        let fields = statement.split_ascii_whitespace().collect::<Vec<_>>();
        let Some(device) = fields.first() else {
            continue;
        };
        if !device.starts_with(['A', 'a']) || device.len() < 2 {
            continue;
        }
        let name = key(device);
        let expected_kind = if let Some(index) = name
            .strip_prefix("ASR_LOAD")
            .and_then(|digits| digits.parse::<usize>().ok())
        {
            validate_input_load(&name, &fields, index, inputs, &mut input_loads)?;
            "SR_BUNDLE_LOAD"
        } else if name == "ASR_STATEFUL" {
            let expected = super::xspice::stateful_device_statement(inputs, outputs);
            ensure!(
                !stateful_present
                    && bit_indices.is_empty()
                    && !tie_present
                    && fields
                        .iter()
                        .map(|word| key(word))
                        .eq(expected.split_ascii_whitespace().map(key)),
                "invalid compiled stateful ALB bundle ABI"
            );
            stateful_present = true;
            "SR_ALB_STATEFUL"
        } else if name == "ASR_TIE0" {
            ensure!(
                fields.len() == 3
                    && fields[1].eq_ignore_ascii_case("SR_TIE0")
                    && fields[2].eq_ignore_ascii_case("MSR_TIE0")
                    && !tie_present,
                "invalid compiled ALB constant-zero driver"
            );
            tie_present = true;
            "D_PULLDOWN"
        } else {
            // Evaluate sources before registering the new device so a LUT
            // cannot read its own output or a forward stage.
            tie_used |=
                validate_compiled_lut_sources(&fields, inputs, &stage_indices, &output_pins)?;
            validate_compiled_alb_lut_device(
                device,
                &name,
                &fields,
                outputs,
                &mut bit_indices,
                &mut output_pins,
            )?;
            if let Some(index) = name
                .strip_prefix("ASR_S")
                .and_then(|digits| digits.parse::<usize>().ok())
            {
                stage_indices.insert(index);
            }
            "SR_ALB_LUT_BIT"
        };
        let model = key(fields
            .last()
            .context("compiled ALB A device has no model")?);
        ensure!(
            models.get(&model).is_some_and(|kind| kind == expected_kind),
            "compiled ALB device {device} has missing or incorrect model {model} (expected {expected_kind})"
        );
    }
    if stateful_present {
        ensure!(
            bit_indices.is_empty() && !tie_present && input_loads.len() == inputs,
            "compiled stateful ALB contains conflicting physical drivers or missing input loads"
        );
        let out_res = validate_stateful_model(def)?;
        return Ok(vec![out_res; outputs]);
    }
    ensure!(
        !tie_used || tie_present,
        "compiled ALB LUT reads a missing constant-zero driver"
    );
    ensure!(
        input_loads.len() == inputs && output_pins.len() == outputs,
        "compiled ALB {} has incomplete native physical drivers",
        def.path.join(".")
    );
    (0..outputs)
        .map(|pin| output_resistance(def, pin))
        .collect()
}

fn output_resistance(def: &Subckt, pin: usize) -> Result<OutputResistance> {
    let formal = format!("SR_OUT{pin}");
    let sink = format!("{formal}_SINK");
    let source = format!("{formal}_SOURCE");
    // The native emitter uses "Asr_oN [inputs] state sink source 0 model".
    // Only the three output slots establish ownership. Another output LUT
    // may legally read this output through its bracketed source vector.
    let drivers = def
        .statements
        .iter()
        .filter(|s| {
            let f = s.split_ascii_whitespace().collect::<Vec<_>>();
            f.first().is_some_and(|x| {
                x.get(..5)
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case("ASR_O"))
            }) && f
                .len()
                .checked_sub(5)
                .and_then(|start| f.get(start..start + 3))
                .is_some_and(|ports| {
                    ports.iter().any(|word| {
                        [&formal, &sink, &source]
                            .iter()
                            .any(|name| word.eq_ignore_ascii_case(name))
                    })
                })
        })
        .collect::<Vec<_>>();
    ensure!(
        drivers.len() == 1,
        "ALB {} has {} physical drivers for {formal}",
        def.path.join("."),
        drivers.len()
    );
    let driver = drivers[0].split_ascii_whitespace().collect::<Vec<_>>();
    ensure!(
        driver.len() >= 7,
        "malformed ALB physical output driver for {formal}"
    );
    let state = driver.len() - 5;
    ensure!(
        driver[1].starts_with('[')
            && driver[state - 1].ends_with(']')
            && driver[state].eq_ignore_ascii_case(&formal)
            && driver[state + 1].eq_ignore_ascii_case(&sink)
            && driver[state + 2].eq_ignore_ascii_case(&source)
            && driver[state + 3] == "0",
        "invalid ALB output driver state/sink/source/wake ABI for {formal}"
    );
    // The emitter numbers each LUT in its global stage order. An output
    // pin may therefore be driven by Asr_o2 / Msr_o2 even when it is SR_OUT0;
    // the device and model suffixes, not the output pin number, must match.
    let suffix = driver[0]
        .get(5..)
        .and_then(|s| s.parse::<usize>().ok())
        .with_context(|| format!("invalid emitted ALB output device {}", driver[0]))?;
    ensure!(
        driver[0].eq_ignore_ascii_case(&format!("Asr_o{suffix}")),
        "invalid emitted ALB output device name {}",
        driver[0]
    );
    let model = driver[state + 4];
    ensure!(
        model.eq_ignore_ascii_case(&format!("Msr_o{suffix}")),
        "ALB output driver {} references unrelated model {model}",
        driver[0]
    );
    let matching = def
        .statements
        .iter()
        .filter(|s| {
            let f = s.split_ascii_whitespace().collect::<Vec<_>>();
            f.len() >= 2 && f[0].eq_ignore_ascii_case(".model") && f[1].eq_ignore_ascii_case(model)
        })
        .collect::<Vec<_>>();
    ensure!(
        matching.len() == 1,
        "expected one unique ALB output model {model}, found {}",
        matching.len()
    );
    let fields = matching[0].split_ascii_whitespace().collect::<Vec<_>>();
    ensure!(
        fields.get(2).is_some_and(|kind| {
            kind.split_once('(')
                .map_or(*kind, |(name, _)| name)
                .eq_ignore_ascii_case("SR_ALB_LUT_BIT")
        }),
        "invalid ALB output model type for {model}"
    );
    compiled_output_resistance(matching[0])
}

fn compiled_output_resistance(statement: &str) -> Result<OutputResistance> {
    Ok(OutputResistance {
        positive: model_numeric_parameter(statement, "OUT_RES_POS")?,
        negative: model_numeric_parameter(statement, "OUT_RES_NEG")?,
    })
}

/// Read a numeric code-model parameter by its complete identifier, outside
/// quoted lookup tables. Substring matches and duplicate assignments would
/// silently change the conductance of a synthesized physical boundary.
fn model_numeric_parameter(statement: &str, name: &str) -> Result<f64> {
    let bytes = statement.as_bytes();
    let mut offset = 0;
    let mut found = None;
    while offset < bytes.len() {
        if matches!(bytes[offset], b'"' | b'\'') {
            let quote = bytes[offset];
            offset += 1;
            while offset < bytes.len() && bytes[offset] != quote {
                offset += 1;
            }
            ensure!(
                offset < bytes.len(),
                "unterminated compiled ALB model quote"
            );
            offset += 1;
            continue;
        }
        let boundary = offset == 0
            || bytes[offset - 1].is_ascii_whitespace()
            || matches!(bytes[offset - 1], b'(' | b',');
        if !boundary || !(bytes[offset].is_ascii_alphabetic() || bytes[offset] == b'_') {
            offset += 1;
            continue;
        }
        let start = offset;
        while bytes
            .get(offset)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        {
            offset += 1;
        }
        let identifier = &statement[start..offset];
        let mut cursor = offset;
        while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
            cursor += 1;
        }
        if bytes.get(cursor) != Some(&b'=') || !identifier.eq_ignore_ascii_case(name) {
            continue;
        }
        cursor += 1;
        while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
            cursor += 1;
        }
        let value_start = cursor;
        while bytes
            .get(cursor)
            .is_some_and(|byte| !byte.is_ascii_whitespace() && !matches!(*byte, b')' | b','))
        {
            cursor += 1;
        }
        ensure!(cursor > value_start, "empty compiled ALB {name} parameter");
        ensure!(found.is_none(), "duplicate compiled ALB {name} parameter");
        found = Some(
            crate::parse_numeric(&statement[value_start..cursor])
                .with_context(|| format!("invalid compiled ALB {name} parameter"))?,
        );
    }
    let value = found.with_context(|| format!("compiled output model missing {name}"))?;
    ensure!(value.is_finite(), "invalid compiled ALB {name} {value}");
    Ok(value)
}

struct Walker<'a> {
    circuit: &'a Circuit,
    pins: Vec<DiscoveredAlbPin>,
    instance_paths: BTreeSet<String>,
}

impl Walker<'_> {
    fn visit(
        &mut self,
        scope: &[String],
        items: &[Instance],
        inherited: &BTreeMap<String, Node>,
        path: &str,
        recursion: &mut Vec<Vec<String>>,
    ) -> Result<()> {
        for instance in items {
            let (def, actual) = resolve(self.circuit, scope, instance)?;
            ensure!(
                !recursion.contains(&def.path),
                "recursive subcircuit model {} at {path}",
                def.path.join(".")
            );
            let instance_path = if path.is_empty() {
                instance.name.clone()
            } else {
                format!("{path}.{}", instance.name)
            };
            ensure!(
                self.instance_paths.insert(instance_path.clone()),
                "duplicate instance name {instance_path}"
            );
            let mut mapping = BTreeMap::new();
            for (formal, actual) in def.ports.iter().zip(&actual) {
                mapping.insert(
                    formal.clone(),
                    inherited.get(actual).cloned().unwrap_or(Node::Local),
                );
            }
            if let Some((inputs, outputs)) = alb_signature(&def.ports) {
                let output_resistances = validate_compiled_alb_device_abi(def, inputs, outputs)?;
                for (pin, resistance) in std::iter::repeat_n(None, inputs)
                    .chain(output_resistances.into_iter().map(Some))
                    .enumerate()
                {
                    let (index, direction, prefix, idprefix) = if pin < inputs {
                        (pin, DigitalPinDirection::Input, "SR_IN", "in")
                    } else {
                        (pin - inputs, DigitalPinDirection::Output, "SR_OUT", "out")
                    };
                    let nodes = [
                        format!("{prefix}{index}"),
                        format!("{prefix}{index}_SINK"),
                        format!("{prefix}{index}_SOURCE"),
                    ]
                    .map(|name| mapping.get(&name).cloned().unwrap_or(Node::Local));
                    let bundle = match nodes {
                        [
                            Node::Root(state),
                            Node::Root(sink_meta),
                            Node::Root(source_meta),
                        ] => Some(NativeDigitalBundle {
                            state,
                            sink_meta,
                            source_meta,
                        }),
                        _ => None,
                    };
                    self.pins.push(DiscoveredAlbPin {
                        id: format!("{instance_path}.{idprefix}{index}"),
                        direction,
                        bundle,
                        output_res_ohm: resistance,
                    });
                }
            } else {
                recursion.push(def.path.clone());
                self.visit(
                    &def.path,
                    &def.instances,
                    &mapping,
                    &instance_path,
                    recursion,
                )?;
                recursion.pop();
            }
        }
        Ok(())
    }
}

/// Traverse an expanded SPICE deck and its supplied subcircuit definitions.
///
/// Source/library references are not silently invented; every X instance
/// must resolve to an available declaration. Local, non-exported nodes are
/// reported but cannot be bridged from the parent circuit.
///
/// # Errors
/// Malformed hierarchy, duplicate instances, unresolved or recursive
/// subcircuits, or invalid compiled-ALB port models.
pub fn discover_alb_pins(deck: &str, definitions: &[&str]) -> Result<Vec<DiscoveredAlbPin>> {
    let circuit = parse_sources(deck, definitions)?;
    let mut walker = Walker {
        circuit: &circuit,
        pins: Vec::new(),
        instance_paths: BTreeSet::new(),
    };
    let mut root = BTreeMap::new();
    for instance in &circuit.root {
        for item in &instance.fields {
            if let Ok(name) = node(item) {
                root.insert(name.clone(), Node::Root(name));
            }
        }
    }
    walker.visit(&[], &circuit.root, &root, "", &mut Vec::new())?;
    Ok(walker.pins)
}

/// Match each public digital-state node to every physical ALB input/output.
///
/// Each instance at that node. The existing family planner groups analog inputs;
/// actual sink/source metadata is recovered from the SPICE hierarchy.
///
/// # Errors
/// Unmatched state nodes, overlapping annotations or incompatible groups.
pub fn derive_family_wiring(
    pins: &[DiscoveredAlbPin],
    connections: &[AnalogDigitalConnection],
    catalog: &BridgeCatalog,
) -> Result<(Vec<PlannedAnalogBridge>, Vec<NativePinWiring>)> {
    let all_digital_nodes = pins
        .iter()
        .filter_map(|pin| pin.bundle.as_ref())
        .flat_map(|bundle| {
            [
                bundle.state.as_str(),
                bundle.sink_meta.as_str(),
                bundle.source_meta.as_str(),
            ]
        })
        .collect::<BTreeSet<_>>();
    let mut selected = BTreeSet::new();
    let mut physical = Vec::new();
    let mut wires = Vec::new();
    for entry in connections {
        let state = node(&entry.digital_net)?;
        let analog = node(&entry.analog_net)?;
        ensure!(
            !all_digital_nodes.contains(analog.as_str()),
            "analog boundary {analog} collides with a physical digital bundle node"
        );
        let family = entry.family.for_pin(entry.direction)?;
        let found = pins
            .iter()
            .filter(|p| {
                p.direction == entry.direction
                    && p.bundle.as_ref().is_some_and(|b| b.state == state)
            })
            .collect::<Vec<_>>();
        ensure!(
            !found.is_empty(),
            "no root-visible ALB {:?} physical pin on digital state {state}",
            entry.direction
        );
        for p in found {
            ensure!(
                selected.insert(p.id.clone()),
                "physical pin {} mapped to multiple mixed-signal boundaries",
                p.id
            );
            physical.push(AnalogBoundaryPin {
                id: p.id.clone(),
                analog_net: entry.analog_net.clone(),
                direction: p.direction,
                family: family.clone(),
                supply_v: entry.supply_v,
            });
            wires.push(NativePinWiring {
                id: p.id.clone(),
                bundle: p
                    .bundle
                    .clone()
                    .context("internal ALB bundle cannot connect at root")?,
                out_res_ohm: p.output_res_ohm,
            });
        }
    }
    Ok((plan_analog_bridges(&physical, catalog)?, wires))
}

/// One electrical boundary owned by a lexical subcircuit definition.
/// Empty scope selects the root deck. Non-root connections name strictly
/// private nets, not formals exported to parent circuits.
#[derive(Clone, Debug)]
pub struct ScopedAnalogDigitalConnection {
    pub scope: Vec<String>,
    pub boundary: AnalogDigitalConnection,
}

/// A preexisting XSPICE device can occupy any part of the native bundle.
fn reject_a_devices_on_nodes(
    statements: &[String],
    bundle_nodes: &BTreeSet<String>,
    scope: &[String],
) -> Result<()> {
    for statement in statements {
        let fields = statement.split_ascii_whitespace().collect::<Vec<_>>();
        let Some(device) = fields.first() else {
            continue;
        };
        if !device.starts_with(['A', 'a']) || device.len() < 2 {
            continue;
        }
        ensure!(
            fields.len() >= 3,
            "malformed XSPICE device {device} in {scope:?}"
        );
        // The final word is the code-model identifier, not an electrical
        // terminal. It can legitimately share a name with an ALB net.
        for field in &fields[1..fields.len() - 1] {
            // XSPICE permits vector ports and typed %gd(net 0) terminals.
            for name in field.split(['[', ']', '(', ')', ',']) {
                let token = key(name);
                if bundle_nodes.contains(&token) {
                    bail!(
                        "unverified pre-existing A device {device} on physical ALB bundle node {token} in {scope:?}"
                    );
                }
            }
        }
    }
    Ok(())
}

/// Follow only the X instance branches electrically connected to a selected
/// physical node. Each descent translates the parent's net into child formals.
/// A compiled ALB has verified native drivers and must not be treated as an
/// independently inserted code-model device.
fn reject_nested_a_devices(
    circuit: &Circuit,
    scope: &[String],
    instances: &[Instance],
    bundle_nodes: &BTreeSet<String>,
    recursion: &mut Vec<Vec<String>>,
) -> Result<()> {
    for instance in instances {
        let (def, actual) = resolve(circuit, scope, instance)?;
        if alb_signature(&def.ports).is_some() {
            continue;
        }
        let child_nodes = def
            .ports
            .iter()
            .zip(&actual)
            .filter(|(_, net)| bundle_nodes.contains(net.as_str()))
            .map(|(formal, _)| formal.clone())
            .collect::<BTreeSet<_>>();
        if child_nodes.is_empty() {
            continue;
        }
        ensure!(
            !recursion.contains(&def.path),
            "recursive X instance {} while checking ALB physical boundary",
            instance.name
        );
        reject_a_devices_on_nodes(&def.statements, &child_nodes, &def.path)
            .with_context(|| format!("through {} in {scope:?}", instance.name))?;
        recursion.push(def.path.clone());
        let result =
            reject_nested_a_devices(circuit, &def.path, &def.instances, &child_nodes, recursion);
        recursion.pop();
        result?;
    }
    Ok(())
}

fn reject_existing_digital_devices(
    circuit: &Circuit,
    scope: &[String],
    wires: &[NativePinWiring],
) -> Result<()> {
    let bundle_nodes = wires
        .iter()
        .flat_map(|wire| {
            [
                wire.bundle.state.as_str(),
                wire.bundle.sink_meta.as_str(),
                wire.bundle.source_meta.as_str(),
            ]
        })
        .map(key)
        .collect::<BTreeSet<_>>();
    let (statements, instances) = if scope.is_empty() {
        (&circuit.root_statements, &circuit.root)
    } else {
        let owner = circuit
            .definitions
            .get(scope)
            .with_context(|| format!("unknown physical boundary scope {scope:?}"))?;
        (&owner.statements, &owner.instances)
    };
    reject_a_devices_on_nodes(statements, &bundle_nodes, scope)?;
    reject_nested_a_devices(
        circuit,
        scope,
        instances,
        &bundle_nodes,
        &mut vec![scope.to_vec()],
    )
}
fn pins_in_scope(circuit: &Circuit, scope: &[String]) -> Result<Vec<DiscoveredAlbPin>> {
    let mut walk = Walker {
        circuit,
        pins: Vec::new(),
        instance_paths: BTreeSet::new(),
    };
    let instances = if scope.is_empty() {
        &circuit.root
    } else {
        &circuit
            .definitions
            .get(scope)
            .with_context(|| format!("unknown lexical subcircuit {scope:?}"))?
            .instances
    };
    let mut available = BTreeMap::new();
    for instance in instances {
        let (_, actual) = resolve(circuit, scope, instance)?;
        for name in actual {
            available.insert(name.clone(), Node::Root(name));
        }
    }
    walk.visit(scope, instances, &available, "", &mut Vec::new())?;
    Ok(walk.pins)
}

fn insert_scoped_bridges(deck: &str, generated: &BTreeMap<Vec<String>, String>) -> Result<String> {
    let mut output = String::new();
    let mut path = Vec::<String>::new();
    let mut inserted = BTreeSet::new();
    let mut control = false;
    for line in deck.split_inclusive('\n') {
        let visible = line.split(';').next().unwrap_or_default().trim();
        let words = visible.split_ascii_whitespace().collect::<Vec<_>>();
        let first = words.first().copied().unwrap_or_default();
        if first.eq_ignore_ascii_case(".control") {
            control = true;
        } else if first.eq_ignore_ascii_case(".endc") {
            control = false;
        }
        if !control {
            if first.eq_ignore_ascii_case(".subckt") {
                ensure!(
                    words.len() >= 2,
                    "nameless subcircuit during bridge insertion"
                );
                path.push(key(words[1]));
            } else if first.eq_ignore_ascii_case(".ends") {
                ensure!(!path.is_empty(), "orphan .ends during bridge insertion");
                if let Some(native) = generated.get(&path) {
                    ensure!(
                        inserted.insert(path.clone()),
                        "duplicate bridge scope {path:?}"
                    );
                    output.push_str(native);
                }
                let closed = path.pop().context("no lexical subcircuit")?;
                if let Some(name) = words.get(1) {
                    ensure!(key(name) == closed, "mismatched .ends name {name}/{closed}");
                }
            } else if first.eq_ignore_ascii_case(".end") {
                ensure!(path.is_empty(), "root .end inside a subcircuit");
                if let Some(native) = generated.get(&Vec::new()) {
                    ensure!(
                        inserted.insert(Vec::new()),
                        "duplicate root bridge insertion"
                    );
                    output.push_str(native);
                }
            }
        }
        output.push_str(line);
    }
    ensure!(
        path.is_empty(),
        "unterminated subcircuit during scoped insertion"
    );
    ensure!(
        inserted.len() == generated.len(),
        "requested lexical bridge scope was not encountered in source"
    );
    Ok(output)
}

/// Resolve and emit native family bridges into their owning subcircuits.
///
/// Every instance of a definition gets its own local analog/digital bridge.
/// This does not give different electrical parameter sets to individual
/// instances of the *same* definition.
///
/// # Errors
/// Invalid scopes, exported private nets, physical mapping conflicts,
/// incompatible family models or duplicate model/device names.
pub fn materialize_scoped_family_bridges(
    deck: &str,
    compiled: &[&str],
    connections: &[ScopedAnalogDigitalConnection],
    catalog: &BridgeCatalog,
    native: &super::NativeFamilySpecification,
) -> Result<String> {
    ensure!(
        native.pins.is_empty(),
        "automatic scoped wiring rejects handwritten physical pin triples"
    );
    let circuit = parse_sources(deck, compiled)?;
    let mut by_scope = BTreeMap::<Vec<String>, Vec<AnalogDigitalConnection>>::new();
    for entry in connections {
        let path = entry.scope.iter().map(|s| key(s)).collect::<Vec<_>>();
        for item in &path {
            node(item)?;
        }
        if !path.is_empty() {
            let owner = circuit
                .definitions
                .get(&path)
                .with_context(|| format!("unknown private boundary scope {path:?}"))?;
            node(&entry.boundary.analog_net)?;
            let digital = node(&entry.boundary.digital_net)?;
            ensure!(
                !owner.ports.contains(&digital),
                "boundary in {path:?} cannot connect an exported digital formal {digital}"
            );
        }
        by_scope
            .entry(path)
            .or_default()
            .push(entry.boundary.clone());
    }
    let mut emitted = BTreeMap::new();
    for (path, entries) in by_scope {
        let pins = pins_in_scope(&circuit, &path)?;
        let (plans, wires) = derive_family_wiring(&pins, &entries, catalog)
            .with_context(|| format!("resolving scoped family bridges in {path:?}"))?;
        reject_existing_digital_devices(&circuit, &path, &wires)?;
        let spec = super::NativeFamilySpecification {
            models: native.models.clone(),
            pins: wires,
        };
        let text = super::render_native_family_bridges(&plans, &spec)?;
        // Native model/device identifiers belong to their lexical SPICE
        // scope. In particular, a root bridge must not collide with a
        // nested subcircuit's private .model or A device (or a .control
        // command), so use the parsed statements for both scope kinds.
        let existing = if path.is_empty() {
            &circuit.root_statements
        } else {
            &circuit
                .definitions
                .get(&path)
                .context("lost definition scope")?
                .statements
        };
        for line in text.lines() {
            let words = line.split_ascii_whitespace().collect::<Vec<_>>();
            let Some(first) = words.first() else {
                continue;
            };
            let declared = if first.eq_ignore_ascii_case(".model") {
                words.get(1).copied()
            } else if first.starts_with(['A', 'a']) {
                Some(*first)
            } else {
                None
            };
            if let Some(name) = declared {
                ensure!(
                    !existing.iter().any(|line| {
                        let original = line.split_ascii_whitespace().collect::<Vec<_>>();
                        original
                            .first()
                            .is_some_and(|word| word.eq_ignore_ascii_case(name))
                            || (original
                                .first()
                                .is_some_and(|word| word.eq_ignore_ascii_case(".model"))
                                && original
                                    .get(1)
                                    .is_some_and(|word| word.eq_ignore_ascii_case(name)))
                    }),
                    "native family identifier {name} already exists in scope {path:?}"
                );
            }
        }
        emitted.insert(path, text);
    }
    insert_scoped_bridges(deck, &emitted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AnalogBridgeKind, BridgeModel, LogicFamily};

    const ALB: &str = "
.subckt ALB_BUFFER sr_in0 sr_in0_sink sr_in0_source sr_out0 sr_out0_sink sr_out0_source
Asr_load0 sr_in0 sr_in0_sink sr_in0_source Msr_input_load
.model Msr_input_load sr_bundle_load(capacitance=2p sink_current=0 source_current=0)
Asr_o0 [sr_in0] sr_out0 sr_out0_sink sr_out0_source 0 Msr_o0
.model Msr_o0 sr_alb_lut_bit(table_values=\"01\" delay=2n input_load=0 out_res_pos=40e0 out_res_neg=40e0)
.ends ALB_BUFFER
";

    fn catalog() -> Result<BridgeCatalog> {
        let mut c = BridgeCatalog::default();
        for (name, kind) in [
            ("HC_ADC", AnalogBridgeKind::Adc),
            ("HC_DAC", AnalogBridgeKind::Dac),
        ] {
            c.add(BridgeModel {
                name: name.to_owned(),
                kind,
                family: LogicFamily::parse("HC")?,
                nominal_supply_v: 5.0,
            })?;
        }
        Ok(c)
    }
    fn mapping(digital: &str, direction: DigitalPinDirection) -> AnalogDigitalConnection {
        AnalogDigitalConnection {
            analog_net: match direction {
                DigitalPinDirection::Input => "AIN",
                DigitalPinDirection::Output => "AOUT",
            }
            .to_owned(),
            digital_net: digital.to_owned(),
            direction,
            family: LogicFamilyAssignment {
                in_family: Some("HC".to_owned()),
                out_family: Some("HC".to_owned()),
                family: None,
            },
            supply_v: 3.3,
        }
    }

    #[test]
    fn direct_and_nested_public_bundles_and_output_resistance() -> Result<()> {
        let deck = "
Xtop input input_sink input_source output output_sink output_source OUTER
.subckt OUTER ain ain_sink ain_source aout aout_sink aout_source
Xmiddle ain ain_sink ain_source aout aout_sink aout_source INNER gain=1
.subckt INNER din din_sink din_source dout dout_sink dout_source
Xcore din din_sink din_source dout dout_sink dout_source ALB_BUFFER
.ends INNER
.ends OUTER
.end
";
        let pins = discover_alb_pins(deck, &[ALB])?;
        assert_eq!(pins.len(), 2);
        assert_eq!(pins[0].id, "XTOP.XMIDDLE.XCORE.in0");
        assert_eq!(pins[1].id, "XTOP.XMIDDLE.XCORE.out0");
        let input = pins[0].bundle.as_ref().context("missing input bundle")?;
        assert_eq!(
            (&*input.state, &*input.sink_meta, &*input.source_meta),
            ("INPUT", "INPUT_SINK", "INPUT_SOURCE")
        );
        assert_eq!(
            pins[1].output_res_ohm,
            Some(OutputResistance::symmetric(40.0))
        );
        let (plans, wires) = derive_family_wiring(
            &pins,
            &[
                mapping("input", DigitalPinDirection::Input),
                mapping("OUTPUT", DigitalPinDirection::Output),
            ],
            &catalog()?,
        )?;
        assert_eq!(plans.len(), 2);
        assert_eq!(wires.len(), 2);
        assert_eq!(
            wires[1].out_res_ohm,
            Some(OutputResistance::symmetric(40.0))
        );
        Ok(())
    }

    #[test]
    fn shared_input_maps_two_physical_loads_into_one_adc_group() -> Result<()> {
        let deck = "
Xleft d d_sink d_source y1 y1_sink y1_source ALB_BUFFER
Xright d d_sink d_source y2 y2_sink y2_source ALB_BUFFER
.end
";
        let pins = discover_alb_pins(deck, &[ALB])?;
        assert_eq!(pins.len(), 4);
        let (plans, wires) = derive_family_wiring(
            &pins,
            &[mapping("d", DigitalPinDirection::Input)],
            &catalog()?,
        )?;
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].physical_pins, vec!["XLEFT.in0", "XRIGHT.in0"]);
        assert_eq!(wires.len(), 2);
        assert_eq!(wires[0].bundle, wires[1].bundle);
        Ok(())
    }

    #[test]
    fn private_node_never_aliases_public_boundary() -> Result<()> {
        let deck = "
Xwrap exported exp_sink exp_source WRAP
.subckt WRAP out out_sink out_source
Xcore private private_sink private_source out out_sink out_source ALB_BUFFER
.ends WRAP
.end";
        let pins = discover_alb_pins(deck, &[ALB])?;
        assert_eq!(pins.len(), 2);
        assert!(pins[0].bundle.is_none());
        assert!(pins[1].bundle.is_some());
        assert!(
            derive_family_wiring(
                &pins,
                &[mapping("PRIVATE", DigitalPinDirection::Input)],
                &catalog()?
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn malformed_or_ambiguous_hierarchies_fail() {
        for deck in [
            "Xbad in in_sink in_source out out_sink out_source UNRESOLVED\n.end",
            "Xbad in in_sink in_source out out_sink ALB_BUFFER\n.end",
            "Xdup a b c d e f ALB_BUFFER\nXdup x y z p q r ALB_BUFFER\n.end",
            "Xloop a b c RECURSE\n.subckt RECURSE a b c\nXloop a b c RECURSE\n.ends RECURSE\n.end",
            "Xx a b c ALB_BUFFER\n.subckt ALB_BUFFER a b c\n.ends ALB_BUFFER\n.end",
            "Xx a b c d e f ALB_BUFFER\n.subckt ALB_BUFFER a b c\n.ends ALB_BUFFER\n.end",
        ] {
            assert!(discover_alb_pins(deck, &[ALB]).is_err(), "accepted {deck}");
        }
    }

    #[test]
    fn mixed_family_annotations_must_not_overlap_same_physical_port() -> Result<()> {
        let pins = discover_alb_pins(
            "Xone a a_sink a_source y y_sink y_source ALB_BUFFER\n.end",
            &[ALB],
        )?;
        assert!(
            derive_family_wiring(
                &pins,
                &[
                    mapping("a", DigitalPinDirection::Input),
                    mapping("A", DigitalPinDirection::Input),
                ],
                &catalog()?
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn malformed_compiled_output_model_is_rejected() {
        let broken = ALB.replace("out_res_pos=40e0", "out_res_pos=bad");
        assert!(discover_alb_pins("Xone a b c y z q ALB_BUFFER\n.end", &[&broken]).is_err());
    }

    #[test]
    fn compiled_alb_stage_and_complete_device_contract() -> Result<()> {
        let deck = "Xone a b c y z q ALB_BUFFER\n.end";
        let staged = ALB
            .replace(
                "Asr_o0 [sr_in0]",
                "Asr_s0 [sr_in0] sr_stage0 sr_stage0_sink sr_stage0_source 0 Msr_s0\n\
                 .model Msr_s0 sr_alb_lut_bit(table_values=\"01\" delay=2n out_res_pos=60 out_res_neg=60)\n\
                 Asr_o1 [sr_stage0]",
            )
            .replace("Msr_o0", "Msr_o1");
        let pins = discover_alb_pins(deck, &[&staged])?;
        assert_eq!(
            pins[1].output_res_ohm,
            Some(OutputResistance::symmetric(40.0))
        );
        let tied = ALB.replace(
            ".ends ALB_BUFFER",
            "Asr_tie0 sr_tie0 Msr_tie0\n\
             .model Msr_tie0 d_pulldown(load=0)\n\
             .ends ALB_BUFFER",
        );
        assert_eq!(discover_alb_pins(deck, &[&tied])?.len(), 2);
        let input_load = "Asr_load0 sr_in0 sr_in0_sink sr_in0_source Msr_input_load\n";
        for corrupt in [
            ALB.replace(input_load, ""),
            ALB.replace(
                "sr_in0_sink sr_in0_source Msr_input_load",
                "sr_in0_source sr_in0_sink Msr_input_load",
            ),
            ALB.replace(".ends ALB_BUFFER", "Rrogue sr_in0 0 10\n.ends ALB_BUFFER"),
            staged.replace("Msr_s0 sr_alb_lut_bit(", "Msr_s0 d_buffer("),
            tied.replace("Msr_tie0 d_pulldown(", "Msr_tie0 d_pullup("),
        ] {
            assert!(
                discover_alb_pins(deck, &[&corrupt]).is_err(),
                "accepted non-emitted compiled ALB device topology: {corrupt}"
            );
        }
        Ok(())
    }

    #[test]
    fn output_resistance_requires_an_exact_unique_parameter_and_model_type() -> Result<()> {
        let deck = "Xone a b c y z q ALB_BUFFER\n.end";
        for corrupted in [
            ALB.replace("out_res_pos=40e0", "backup_out_res_pos=40e0"),
            ALB.replace("out_res_pos=40e0", "out_res_pos=40e0 out_res_pos=200e0"),
            ALB.replace("sr_alb_lut_bit(", "sr_alb_lut_bit_custom("),
        ] {
            assert!(
                discover_alb_pins(deck, &[&corrupted]).is_err(),
                "accepted malformed compiled ALB physical output model: {corrupted}"
            );
        }
        let spaced = ALB.replace("out_res_pos=40e0", "OUT_RES_POS = 40e0");
        let pins = discover_alb_pins(deck, &[&spaced])?;
        assert_eq!(
            pins[1].output_res_ohm,
            Some(OutputResistance::symmetric(40.0))
        );
        // Output suffix follows global LUT stage ordering, not pin index.
        let staged = ALB.replace("Asr_o0", "Asr_o2").replace("Msr_o0", "Msr_o2");
        let staged_pins = discover_alb_pins(deck, &[&staged])?;
        assert_eq!(
            staged_pins[1].output_res_ohm,
            Some(OutputResistance::symmetric(40.0))
        );
        for source in [
            ".model M sr_alb_lut_bit(table_values=\"out_res=5\" out_res = 40e0)",
            ".model M sr_alb_lut_bit(backup_out_res=10e0 out_res=40e0)",
        ] {
            assert_eq!(model_numeric_parameter(source, "OUT_RES")?, 40.0);
        }
        Ok(())
    }

    #[test]
    fn asymmetric_driver_resistances_survive_physical_discovery() -> Result<()> {
        let asymmetric = ALB.replace("out_res_neg=40e0", "out_res_neg=-80e0");
        let pins = discover_alb_pins("Xone a b c y z q ALB_BUFFER\n.end", &[&asymmetric])?;
        assert_eq!(
            pins[1].output_res_ohm,
            Some(OutputResistance {
                positive: 40.0,
                negative: -80.0
            })
        );
        Ok(())
    }

    #[test]
    fn output_resistance_requires_a_real_output_driver_with_correct_port_order() {
        let deck = "Xone a b c y z q ALB_BUFFER\n.end";
        let correct = "Asr_o0 [sr_in0] sr_out0 sr_out0_sink sr_out0_source 0";
        for invalid in [
            "Asr_o0 [sr_in0] sr_out0_sink sr_out0 sr_out0_source 0",
            "Asr_o0 [sr_in0] sr_out0 sr_out0_source sr_out0_sink 0",
            "Asr_o0 [sr_in0 sr_out0 sr_in0] private private_sink private_source 0",
            "Asr_o0 [sr_in0] sr_out0 sr_out0_sink sr_out0_source bad_wake",
        ] {
            let broken = ALB.replace(correct, invalid);
            assert!(
                discover_alb_pins(deck, &[&broken]).is_err(),
                "accepted compiled ALB with malformed output port ABI: {invalid}"
            );
        }
    }

    #[test]
    fn compiled_output_driver_must_bind_its_unique_emitted_model() {
        let deck = "Xone a b c y z q ALB_BUFFER\n.end";
        for corrupted in [
            // An output device cannot inherit a stage model's resistance.
            ALB.replace("0 Msr_o0", "0 Msr_s0").replace(
                ".ends ALB_BUFFER",
                ".model Msr_s0 sr_alb_lut_bit(out_res_pos=10 out_res_neg=10)\n.ends ALB_BUFFER",
            ),
            // An emitted output device and its model share the same suffix.
            ALB.replace("Asr_o0", "Asr_oBAD")
                .replace("Msr_o0", "Msr_oBAD"),
            ALB.replace("Asr_o0", "Asr_output"),
            ALB.replace("Msr_o0", "Msr_s0"),
            // No model shadowing: a second declaration of the *same name*
            // is ambiguous even if it has a different or unsupported kind.
            ALB.replace(
                ".ends ALB_BUFFER",
                ".model Msr_o0 sr_alb_lut_bit_other(out_res=1)\n.ends ALB_BUFFER",
            ),
            ALB.replace(
                ".ends ALB_BUFFER",
                ".model Msr_o0 sr_alb_lut_bit(out_res_pos=1 out_res_neg=1)\n.ends ALB_BUFFER",
            ),
        ] {
            assert!(
                discover_alb_pins(deck, &[&corrupted]).is_err(),
                "accepted invalid compiled ALB output-model binding: {corrupted}"
            );
        }
    }

    #[test]
    fn output_driver_can_consume_a_different_output_without_owning_its_boundary() -> Result<()> {
        let two_outputs = ALB
            .replace(
                "sr_out0_source\nAsr_load0",
                "sr_out0_source sr_out1 sr_out1_sink sr_out1_source\nAsr_load0",
            )
            .replace(
                ".ends ALB_BUFFER",
                "Asr_o1 [sr_out0] sr_out1 sr_out1_sink sr_out1_source 0 Msr_o1\n\
                 .model Msr_o1 sr_alb_lut_bit(table_values=\"01\" delay=2n out_res_pos=80 out_res_neg=80)\n\
                 .ends ALB_BUFFER",
            );
        let pins = discover_alb_pins(
            "Xone a b c y y_sink y_source z z_sink z_source ALB_BUFFER\n.end",
            &[&two_outputs],
        )?;
        assert_eq!(pins.len(), 3);
        assert_eq!(
            pins[1].output_res_ohm,
            Some(OutputResistance::symmetric(40.0))
        );
        assert_eq!(
            pins[2].output_res_ohm,
            Some(OutputResistance::symmetric(80.0))
        );
        Ok(())
    }

    #[test]
    fn compiled_lut_sources_must_be_emitted_digital_state_nodes() -> Result<()> {
        let deck = "Xone a b c y z q ALB_BUFFER\n.end";
        for vector in [
            "[UNBOUND]",
            "[sr_in1]",
            "[sr_in0_sink]",
            "[sr_in0_source]",
            "[sr_stage0]",
            "[sr_out0]",
            "[sr_tie0]",
            "[]",
            "[sr_in0 sr_in1]",
            "[sr_in0] [sr_in0]",
            "[sr_in0",
            "sr_in0]",
        ] {
            let altered = ALB.replace("Asr_o0 [sr_in0]", &format!("Asr_o0 {vector}"));
            assert!(
                discover_alb_pins(deck, &[&altered]).is_err(),
                "accepted non-emitted LUT source vector {vector}"
            );
        }
        let valid_tie = ALB.replace("Asr_o0 [sr_in0]", "Asr_o0 [sr_tie0]").replace(
            ".ends ALB_BUFFER",
            "Asr_tie0 sr_tie0 Msr_tie0\n.model Msr_tie0 d_pulldown(load=0)\n.ends ALB_BUFFER",
        );
        assert_eq!(discover_alb_pins(deck, &[&valid_tie])?.len(), 2);
        Ok(())
    }

    #[test]
    fn comments_and_zero_resistance_remain_valid_source_semantics() -> Result<()> {
        let ideal = ALB.replace(
            "out_res_pos=40e0 out_res_neg=40e0",
            "out_res_pos=0e0 out_res_neg=0e0",
        );
        let deck = "Xone in in_sink in_source out out_sink out_source ALB_BUFFER ; comment\n.end\n";
        let pins = discover_alb_pins(deck, &[&ideal])?;
        assert_eq!(pins.len(), 2);
        assert_eq!(
            pins[1].output_res_ohm,
            Some(OutputResistance::symmetric(0.0))
        );
        Ok(())
    }
}
