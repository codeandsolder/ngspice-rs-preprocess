//! Explicit `PSpice` source lowering.
//!
//! Ordinary syntax needs no global `ngbehavior=ps` mode; explicit physical
//! model families require the native engine.
//!
//! Reference: ngspice-47 frontend inpcompat.c, `pspice_compat`. Unsupported
//! physical model families and stochastic declarations fail explicitly.

use crate::{
    MappedNormalization, NativeNormalization, NormalizationPass, RenamedDevice, SourceDialects,
    lexical::{self, Edit},
    linked_source::{RootKind, SourceDialect},
    model_card::{ModelCard, parse_parameter_fields, parse_spice_constant},
};
use anyhow::{Context, Result, bail, ensure};
use model_preprocess_command::{Resource, WorkBudget};
use std::{collections::BTreeMap, fmt::Write as _};

pub mod digital_corners;
pub mod digital_instance;
pub mod digital_io;
pub mod digital_options;
pub mod digital_source;
pub mod digital_timing;
mod initialization;
mod instances;
pub use initialization::capture_initialization;
pub mod nodes;
pub mod optional_ports;
mod options;
mod passive;
mod scope;
pub mod switch_source;
mod temperature;
use scope::Scopes;

const FUNCTIONS: &[(&str, &str)] = &[
    (
        "limit",
        "(x,a,b) {ternary_fcn(a>b,max(min(x,a),b),max(min(x,b),a))}",
    ),
    ("pwr", "(x,a) {pow(x,a)}"),
    ("pwrs", "(x,a) {sgn(x)*pow(x,a)}"),
    ("stp", "(x) {u(x)}"),
    ("if", "(a,b,c) {ternary_fcn(a,b,c)}"),
    ("int", "(x) {sgn(x)*floor(abs(x))}"),
];

const fn identifier(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$' | b'.' | b'#')
}

struct Functions {
    names: BTreeMap<String, String>,
    used: BTreeMap<String, usize>,
    line: usize,
    scopes: Scopes,
    scope: usize,
    ground: String,
    identifiers: crate::IdentifierAllocator,
    passives: passive::PassiveModels,
    temperature: Option<usize>,
    uses_gmin: Option<usize>,
    temperature_name: String,
    thermal_voltage_name: String,
    gmin_name: String,
    options: options::GlobalOptions,
    renamed: Vec<RenamedDevice>,
}

impl Functions {
    fn new(input: &[u8], scopes: Scopes, options: options::GlobalOptions) -> Result<Self> {
        let mut identifiers = crate::IdentifierAllocator::new(input, scopes.budget())?;
        let mut names = BTreeMap::new();
        for (name, _) in FUNCTIONS {
            let base = format!("__ps_{name}");
            let chosen = identifiers.fresh(base.as_bytes())?;
            names.insert((*name).to_owned(), String::from_utf8(chosen)?);
        }
        let ground = String::from_utf8(identifiers.fresh(b"__ps_gnd")?)?;
        let passives = passive::PassiveModels::collect(&scopes, &mut identifiers)?;
        let temperature_name = String::from_utf8(identifiers.fresh(b"__ps_temperature")?)?;
        let thermal_voltage_name = String::from_utf8(identifiers.fresh(b"__ps_thermal_voltage")?)?;
        let gmin_name = String::from_utf8(identifiers.fresh(b"__ps_gmin")?)?;
        Ok(Self {
            names,
            used: BTreeMap::new(),
            line: 0,
            scopes,
            scope: 0,
            ground,
            identifiers,
            passives,
            temperature: None,
            uses_gmin: None,
            temperature_name,
            thermal_voltage_name,
            gmin_name,
            options,
            renamed: Vec::new(),
        })
    }

    fn allocate(&mut self, base: &str) -> Result<String> {
        Ok(String::from_utf8(self.identifiers.fresh(base.as_bytes())?)?)
    }

    fn temperature_parameters(&self) -> String {
        format!(
            ".param {}='temper'\n.param {}='(temper+273.15)*8.6173303e-5'",
            self.temperature_name, self.thermal_voltage_name
        )
    }

    fn definitions(&self) -> Result<(String, Vec<usize>)> {
        let mut result = String::new();
        let mut origins = Vec::new();
        for (name, body) in FUNCTIONS {
            if let Some(&line) = self.used.get(*name) {
                let generated = self.names.get(*name).context("missing PSpice helper")?;
                writeln!(result, ".func {generated}{body}")?;
                origins.push(line);
            }
        }
        if let Some(line) = self.temperature {
            result.push_str(&self.temperature_parameters());
            result.push('\n');
            origins.extend([line, line]);
        }
        if let Some(line) = self.uses_gmin {
            writeln!(result, ".param {}={:e}", self.gmin_name, self.options.gmin)?;
            origins.push(line);
        }
        Ok((result, origins))
    }

    // Keep electrical probe operands opaque while rewriting actual function
    // calls and detecting the predefined scalar parameters.
    fn expression_identifier(
        &mut self,
        input: &str,
        start: usize,
        table: bool,
        output: &mut Vec<u8>,
    ) -> Result<usize> {
        let bytes = input.as_bytes();
        let mut end = start + 1;
        while bytes.get(end).is_some_and(|&byte| identifier(byte)) {
            end += 1;
        }
        let name = &input[start..end];
        let mut after = end;
        while bytes.get(after).is_some_and(u8::is_ascii_whitespace) {
            after += 1;
        }
        if bytes.get(after) == Some(&b'(') {
            if name.eq_ignore_ascii_case("v") || name.eq_ignore_ascii_case("i") {
                let close = bytes[after + 1..]
                    .iter()
                    .position(|&byte| byte == b')')
                    .map(|index| after + index + 2)
                    .context("unterminated PSpice probe")?;
                output.extend_from_slice(&bytes[start..close]);
                return Ok(close);
            }
            let lower = name.to_ascii_lowercase();
            if !self.scopes.shadows(self.scope, &lower)? {
                if let Some(generated) = self.names.get(&lower) {
                    self.scopes
                        .budget()
                        .charge(Resource::EmittedBytes, generated.len())?;
                    output.extend_from_slice(generated.as_bytes());
                    self.used.entry(lower).or_insert(self.line);
                    return Ok(end);
                }
                if lower == "table" {
                    ensure!(
                        table,
                        "PSpice TABLE() requires an E/G VALUE or G CUR expression"
                    );
                    output.extend_from_slice(b"pwl");
                    return Ok(end);
                }
            }
        } else if bytes.get(after) != Some(&b'=') || bytes.get(after + 1) == Some(&b'=') {
            // Generated names keep PSpice's implicit scalar parameters from
            // replacing same-spelled native parameters or instance setters.
            let generated = if name.eq_ignore_ascii_case("temp") {
                self.temperature.get_or_insert(self.line);
                Some(&self.temperature_name)
            } else if name.eq_ignore_ascii_case("vt") {
                self.temperature.get_or_insert(self.line);
                Some(&self.thermal_voltage_name)
            } else if name.eq_ignore_ascii_case("gmin") {
                self.uses_gmin.get_or_insert(self.line);
                Some(&self.gmin_name)
            } else {
                None
            };
            if let Some(generated) = generated {
                self.scopes
                    .budget()
                    .charge(Resource::EmittedBytes, generated.len())?;
                output.extend_from_slice(generated.as_bytes());
                return Ok(end);
            }
        }
        output.extend_from_slice(name.as_bytes());
        Ok(end)
    }

    fn expression(&mut self, input: &str, table: bool) -> Result<String> {
        self.scopes
            .budget()
            .charge(Resource::EvaluationSteps, input.len())?;
        self.scopes
            .budget()
            .charge(Resource::EmittedBytes, input.len())?;
        let bytes = input.as_bytes();
        let mut cursor = 0;
        let mut output = Vec::with_capacity(input.len());
        let mut quote = None;
        let mut braces = 0usize;
        while let Some(&byte) = bytes.get(cursor) {
            if quote.is_some() {
                output.push(byte);
                cursor += 1;
                if byte == b'\\'
                    && let Some(&next) = bytes.get(cursor)
                {
                    output.push(next);
                    cursor += 1;
                } else if quote == Some(byte) {
                    quote = None;
                }
                continue;
            }
            if byte == b'"' {
                quote = Some(byte);
                output.push(byte);
                cursor += 1;
                continue;
            }
            if (byte.is_ascii_alphabetic() || byte == b'_')
                && (cursor == 0 || !identifier(bytes[cursor - 1]))
            {
                cursor = self.expression_identifier(input, cursor, table, &mut output)?;
                continue;
            }
            match byte {
                b'{' => {
                    output.push(if braces == 0 { b'{' } else { b'(' });
                    braces += 1;
                    cursor += 1;
                }
                b'}' => {
                    braces = braces
                        .checked_sub(1)
                        .context("unmatched PSpice expression brace")?;
                    output.push(if braces == 0 { b'}' } else { b')' });
                    cursor += 1;
                }
                b'&' | b'|' => {
                    self.scopes.budget().charge(Resource::EmittedBytes, 1)?;
                    output.extend_from_slice(&[byte, byte]);
                    cursor += 1 + usize::from(bytes.get(cursor + 1) == Some(&byte));
                }
                b'~' => {
                    output.push(b'!');
                    cursor += 1;
                }
                _ => {
                    output.push(byte);
                    cursor += 1;
                }
            }
        }
        ensure!(braces == 0, "unclosed PSpice expression brace");
        Ok(String::from_utf8(output)?)
    }
}

fn parameter(card: &ModelCard, name: &str) -> Option<String> {
    card.parameters
        .iter()
        .rev()
        .find(|p| p.name.eq_ignore_ascii_case(name))
        .and_then(|p| p.expression.clone())
}

fn set_parameter(card: &mut ModelCard, name: &str, expression: &str) -> Result<()> {
    // These are newly emitted fields; their spans describe the generated
    // assignment, not a position in the original source declaration.
    let generated = parse_parameter_fields(&format!("{name}={expression}"), &[])?;
    card.parameters
        .retain(|p| !p.name.eq_ignore_ascii_case(name));
    card.parameters.extend(generated);
    Ok(())
}

fn default_parameter(card: &mut ModelCard, name: &str, expression: &str) -> Result<()> {
    if parameter(card, name).is_none() {
        set_parameter(card, name, expression)?;
    }
    Ok(())
}

fn rename_parameter(card: &mut ModelCard, from: &str, to: &str) {
    for p in &mut card.parameters {
        if p.name.eq_ignore_ascii_case(from) {
            to.clone_into(&mut p.name);
        }
    }
}

use switch_source::SwitchLaw;

impl SwitchLaw {
    fn lower_model(self, card: &mut ModelCard, control_conductance: f64) -> Result<()> {
        match self {
            Self::ThresholdVoltage | Self::ThresholdCurrent => {
                let (native, threshold, hysteresis) = if matches!(self, Self::ThresholdVoltage) {
                    ("SW", "vt", "vh")
                } else {
                    ("CSW", "it", "ih")
                };
                native.clone_into(&mut card.family);
                card.parameters
                    .retain(|field| !field.name.eq_ignore_ascii_case("td"));
                for (name, value) in [
                    (threshold, "0"),
                    (hysteresis, "0"),
                    ("ron", "1"),
                    ("roff", "1e12"),
                ] {
                    default_parameter(card, name, value)?;
                }
            }
            Self::ContinuousVoltage | Self::ContinuousCurrent => {
                let (on, off) = if matches!(self, Self::ContinuousVoltage) {
                    ("von", "voff")
                } else {
                    ("ion", "ioff")
                };
                // PSpice specifies cubic log-resistance interpolation for
                // both voltage and current control. Native aswitch is linear
                // in log(R), so it is not the correct current-control law.
                "pswitch".clone_into(&mut card.family);
                for (from, to) in [
                    (on, "cntl_on"),
                    (off, "cntl_off"),
                    ("ron", "r_on"),
                    ("roff", "r_off"),
                ] {
                    rename_parameter(card, from, to);
                }
                for (name, value) in [
                    ("cntl_on", "1"),
                    ("cntl_off", "0"),
                    ("r_on", "1"),
                    ("r_off", "1e6"),
                    ("log", "true"),
                ] {
                    default_parameter(card, name, value)?;
                }
                set_parameter(
                    card,
                    "r_cntl_in",
                    &format!("{:e}", control_conductance.recip()),
                )?;
            }
        }
        Ok(())
    }

    fn lower_instance(
        self,
        fields: &[&str],
        functions: &mut Functions,
    ) -> Result<Option<SwitchEmission>> {
        let mut prefix = String::new();
        let (model_index, input) = match self {
            Self::ThresholdVoltage | Self::ThresholdCurrent => return Ok(None),
            Self::ContinuousVoltage => {
                ensure!(
                    fields.len() == 6 && fields[0].starts_with(['s', 'S']),
                    "invalid continuous voltage switch instance"
                );
                (5, format!("%gd({} {})", fields[3], fields[4]))
            }
            Self::ContinuousCurrent => {
                ensure!(
                    fields.len() == 5 && fields[0].starts_with(['w', 'W']),
                    "invalid continuous current switch instance"
                );
                // A unity-transresistance CCVS reads the original voltage
                // source's branch current without loading that branch.
                // Its private output drives the cubic switch control port.
                let sensor = functions.allocate(&format!("H__ps_{}", fields[0]))?;
                let control = functions.allocate(&format!("__ps_ctrl_{}", fields[0]))?;
                writeln!(prefix, "{sensor} {control} 0 {} 1", fields[3])?;
                (4, format!("%gd({control} 0)"))
            }
        };
        let name = functions.allocate(&format!("A__ps_{}", fields[0]))?;
        write!(
            prefix,
            "{name} {input} %gd({} {}) {}",
            fields[1], fields[2], fields[model_index]
        )?;
        Ok(Some(SwitchEmission {
            text: prefix,
            linked: name,
        }))
    }
}

struct SwitchEmission {
    text: String,
    linked: String,
}

fn switch_instance(text: &str, line: usize, functions: &mut Functions) -> Result<String> {
    let Some(first) = text.as_bytes().first().map(u8::to_ascii_lowercase) else {
        return Ok(text.to_owned());
    };
    let index = match first {
        b's' => 5,
        b'w' => 4,
        _ => return Ok(text.to_owned()),
    };
    let (positions, _) = crate::model_card::positional_prefix(text);
    let fields: Vec<_> = positions.iter().map(|(word, _)| *word).collect();
    let Some(name) = fields.get(index) else {
        return Ok(text.to_owned());
    };
    let Some(model_line) = functions
        .scopes
        .find_model(functions.scope, &name.to_ascii_lowercase())?
    else {
        return Ok(text.to_owned());
    };
    if !functions.scopes.is_pspice_model(model_line) {
        return Ok(text.to_owned());
    }
    let model = functions.scopes.model(model_line)?;
    let Some(law) = SwitchLaw::from_static_model(&model)? else {
        return Ok(text.to_owned());
    };
    if matches!(law, SwitchLaw::ThresholdVoltage) {
        let [declared, _, _, positive, negative, _, ..] = fields.as_slice() else {
            bail!("invalid threshold voltage switch instance");
        };
        // Native SW has no control-port loading. Add the documented
        // numerical shunt as a noiseless linear conductance; leave the
        // original switch identity and state flags intact.
        let shunt = functions.allocate(&format!("G__ps_{declared}"))?;
        return Ok(format!(
            "{shunt} {positive} {negative} {positive} {negative} {:e}\n{text}",
            functions.options.gmin
        ));
    }
    // Do not drop source instance assignments while changing its designator.
    if matches!(
        law,
        SwitchLaw::ContinuousCurrent | SwitchLaw::ContinuousVoltage
    ) {
        ensure!(
            lexical::tokens(text.as_bytes())?
                .iter()
                .all(|span| &text[span.clone()] != "="),
            "continuous PSpice switch instance assignments are unsupported"
        );
    }
    let Some(rewritten) = law.lower_instance(&fields, functions)? else {
        return Ok(text.to_owned());
    };
    scope::charge_scope(
        functions.scopes.path(functions.scope),
        functions.scopes.budget(),
    )?;
    functions
        .scopes
        .budget()
        .charge(Resource::GeneratedNodes, 1)?;
    functions.renamed.push(RenamedDevice {
        input_line: line,
        scope: functions.scopes.path(functions.scope).to_vec(),
        declared: fields[0].to_owned(),
        linked: rewritten.linked,
    });
    Ok(rewritten.text)
}

fn model(line: usize, functions: &mut Functions) -> Result<String> {
    let mut card = functions.scopes.model(line)?;
    if let Some(timing) = digital_timing::TimingModel::from_card(&card)? {
        bail!(
            "PSpice digital timing model {} ({:?}) requires occurrence-bound digital timing",
            timing.name,
            timing.family
        );
    }
    if let Some(io) = digital_io::IoModel::from_card(&card)? {
        bail!(
            "PSpice digital I/O model {} requires occurrence-bound native digital execution",
            io.name
        );
    }
    let family = card.family.to_ascii_lowercase();
    for p in &card.parameters {
        let name = p.name.to_ascii_lowercase();
        ensure!(
            !matches!(name.split('/').next(), Some("dev" | "lot")),
            "PSpice statistical model field {} requires a realization policy",
            p.name
        );
    }
    for (from, to) in [
        ("t_abs", "temp"),
        ("t_rel_global", "dtemp"),
        ("t_measured", "tnom"),
    ] {
        rename_parameter(&mut card, from, to);
    }
    if matches!(family.as_str(), "nmos" | "pmos" | "npn" | "pnp")
        && let Some(level) = parameter(&card, "level")
    {
        let level =
            parse_spice_constant(&level).context("PSpice physical model level must be constant")?;
        // Model levels are discrete identifiers, not measured quantities.
        if matches!(family.as_str(), "nmos" | "pmos") {
            match level {
                5.0 => bail!("PSpice MOS level 5 requires an EKV physical kernel"),
                6.0 | 7.0 => {
                    set_parameter(&mut card, "level", "8")?;
                    set_parameter(&mut card, "version", "3.2.4")?;
                }
                8.0 => {
                    set_parameter(&mut card, "level", "14")?;
                    set_parameter(&mut card, "version", "4.5.0")?;
                }
                _ => {}
            }
        } else if matches!(level, 2.0) {
            bail!("PSpice BJT level 2 requires a MEXTRAM physical kernel");
        }
    }
    if family == "lpnp" {
        "PNP".clone_into(&mut card.family);
        set_parameter(&mut card, "level", "1")?;
        set_parameter(&mut card, "subs", "-1")?;
    }
    if let Some(switch) = SwitchLaw::from_static_model(&card)? {
        switch.lower_model(&mut card, functions.options.gmin)?;
    }
    let multiplier = functions.passives.lower_model(line, &mut card);
    let mut output = String::new();
    if let Some((name, value)) = multiplier {
        let value = functions.expression(temperature::expression(&value), false)?;
        writeln!(output, ".param {name}={{{value}}}")?;
    }
    write!(output, ".model {} {}(", card.name, card.family)?;
    for (index, p) in card.parameters.iter().enumerate() {
        if index != 0 {
            output.push(' ');
        }
        output.push_str(&p.name);
        if let Some(value) = &p.expression {
            output.push('=');
            output.push_str(&functions.expression(value, false)?);
        }
    }
    output.push(')');
    Ok(output)
}

fn commas_to_spaces(text: &str) -> Result<String> {
    let edits = lexical::comma_tokens(text.as_bytes())?
        .into_iter()
        .filter(|span| &text[span.clone()] == ",")
        .map(|span| (span, vec![b' ']))
        .collect();
    Ok(String::from_utf8(
        lexical::apply_edits(text.as_bytes(), edits).unwrap_or_else(|| text.as_bytes().to_vec()),
    )?)
}

fn rewrite(text: &str, line: usize, functions: &mut Functions) -> Result<String> {
    let first = text.split_ascii_whitespace().next().unwrap_or_default();
    let lower = first.to_ascii_lowercase();
    if lower == ".model" {
        return model(line, functions);
    }
    if lower == ".distribution" || lower == ".mc" || lower == ".worstcase" {
        bail!("PSpice statistical directives require a realization policy");
    }
    if lower.starts_with('u') {
        bail!("PSpice digital U-device requires a typed digital primitive");
    }
    let Some(start) = crate::expression_start(text.as_bytes(), first.as_bytes())? else {
        return Ok(text.to_owned());
    };
    let prefix = &text[..start];
    let tail = &text[start..];
    let table = lexical::tokens(tail.as_bytes())?
        .first()
        .is_some_and(|span| {
            let keyword = &tail[span.clone()];
            (lower.starts_with(['e', 'g']) && keyword.eq_ignore_ascii_case("value"))
                || (lower.starts_with('g') && keyword.eq_ignore_ascii_case("cur"))
        });
    let tail = functions.expression(tail, table)?;
    let tail = if lower == ".subckt" || lower.starts_with('x') {
        commas_to_spaces(&tail)?
    } else {
        tail
    };
    Ok(format!("{prefix}{tail}"))
}

struct LoweredBody {
    bytes: Vec<u8>,
    lines: Vec<usize>,
    definitions: (String, Vec<usize>),
    renamed: Vec<RenamedDevice>,
}

fn rewrite_statements(
    statements: &[lexical::Statement],
    functions: &mut Functions,
    origins: &[usize],
    dialects: SourceDialects<'_>,
) -> Result<Vec<String>> {
    let mut rewritten_statements = Vec::with_capacity(statements.len());
    let mut in_control = false;
    for statement in statements {
        functions.line = statement.line;
        functions.scopes.budget().charge(Resource::Statements, 1)?;
        functions
            .scopes
            .budget()
            .charge(Resource::ScannedBytes, statement.text.len())?;
        let text = std::str::from_utf8(&statement.text).context("PSpice source is not UTF-8")?;
        let first = text.split_ascii_whitespace().next().unwrap_or_default();
        if first.eq_ignore_ascii_case(".control") {
            in_control = true;
        }
        if first.eq_ignore_ascii_case(".endc") {
            in_control = false;
            rewritten_statements.push(text.to_owned());
            continue;
        }
        let source = dialects.at_mapped(origins, statement.line)?;
        let rewritten = if !source.is_pspice() {
            if in_control {
                text.to_owned()
            } else {
                functions.scope = functions.scopes.scope(statement.line)?;
                let text = functions.passives.rewrite_instance(
                    text,
                    functions.scope,
                    &functions.scopes,
                    source,
                )?;
                switch_instance(&text, statement.line, functions)?
            }
        } else if in_control {
            nodes::rewrite(text, false, &functions.ground, true, |node| {
                functions.scopes.hierarchical_ground(0, node)
            })?
        } else {
            functions.scope = functions.scopes.scope(statement.line)?;
            let text = instances::rewrite(
                text,
                functions.scopes.local_ground(functions.scope),
                &functions.ground,
            )?;
            let text = functions.passives.rewrite_instance(
                &text,
                functions.scope,
                &functions.scopes,
                source,
            )?;
            let rewritten = rewrite(&text, statement.line, functions).with_context(|| {
                format!(
                    "{}: PSpice source statement",
                    dialects.describe_mapped(origins, statement.line)
                )
            })?;
            if first.eq_ignore_ascii_case(".model") {
                functions
                    .scopes
                    .budget()
                    .charge(Resource::EmittedBytes, rewritten.len())?;
                rewritten_statements.push(rewritten);
                continue;
            }
            let rewritten = nodes::rewrite(
                &rewritten,
                functions.scopes.local_ground(functions.scope),
                &functions.ground,
                false,
                |node| functions.scopes.hierarchical_ground(functions.scope, node),
            )?;
            switch_instance(&rewritten, statement.line, functions)?
        };
        functions
            .scopes
            .budget()
            .charge(Resource::EmittedBytes, rewritten.len())?;
        rewritten_statements.push(rewritten);
    }
    Ok(rewritten_statements)
}

fn lower_body(
    body: &[u8],
    origins: &[usize],
    dialects: SourceDialects<'_>,
    budget: &WorkBudget,
) -> Result<LoweredBody> {
    budget.charge(Resource::ScannedBytes, body.len())?;
    let statements = lexical::statements_with(body, |line| {
        Ok(lexical::DollarMode::for_dialect(
            dialects.at_mapped(origins, line)?,
        ))
    })?;
    let scopes = Scopes::collect(&statements, origins, dialects, budget)?;
    let declarations = digital_source::capture(&statements, &scopes, origins, dialects)?;
    if let Some(declaration) = declarations.delayed_switches.first() {
        bail!(
            "PSpice nonzero threshold-switch delay requires a timed switch kernel: {} at {:?}",
            declaration.name,
            declaration.location
        );
    }
    if let Some(declaration) = declarations.digital.first() {
        bail!(
            "PSpice digital primitive {} at {:?} requires occurrence-bound native digital execution",
            declaration.instance.declaration.name,
            declaration.instance.location
        );
    }
    if let Some(signature) = declarations.optional_ports.first() {
        bail!(
            "PSpice OPTIONAL: ports require occurrence binding: {} at {:?}",
            signature.declaration.name,
            signature.location
        );
    }
    let options = options::GlobalOptions::collect(&statements)?;
    let mut functions = Functions::new(body, scopes, options)?;
    let rewritten_statements = rewrite_statements(&statements, &mut functions, origins, dialects)?;
    let mut edits: Vec<Edit> = Vec::new();
    let mut inserted = BTreeMap::<usize, usize>::new();
    for (statement, mut rewritten) in statements.iter().zip(rewritten_statements) {
        if functions.temperature.is_some()
            && statement
                .text
                .split(u8::is_ascii_whitespace)
                .next()
                .is_some_and(|first| first.eq_ignore_ascii_case(b".subckt"))
        {
            rewritten.push('\n');
            rewritten.push_str(&functions.temperature_parameters());
        }
        if rewritten.as_bytes() == statement.text {
            continue;
        }
        let extra = rewritten.bytes().filter(|byte| *byte == b'\n').count();
        if extra != 0 {
            inserted.insert(statement.line, extra);
            let physical = body[statement.start..]
                .split_inclusive(|b| *b == b'\n')
                .next()
                .unwrap_or_default();
            if physical.ends_with(b"\r\n") {
                rewritten = rewritten.replace('\n', "\r\n");
            }
        }
        lexical::replace_span(
            statement,
            0..statement.text.len(),
            rewritten.as_bytes(),
            &mut edits,
        );
        for (plus, _) in &statement.continuations {
            edits.push((*plus..*plus + 1, vec![b'*']));
        }
    }
    // PSpice treats *# directives as comments, not native simulator commands.
    let mut offset = 0;
    for (index, line) in body.split_inclusive(|byte| *byte == b'\n').enumerate() {
        let trimmed = line.trim_ascii_start();
        if trimmed.starts_with(b"*#") && dialects.at_mapped(origins, index + 1)?.is_pspice() {
            let position = offset + line.len() - trimmed.len() + 1;
            edits.push((position..position, vec![b' ']));
        }
        offset += line.len();
    }
    let bytes =
        lexical::apply_budgeted_edits(body, edits, budget)?.unwrap_or_else(|| body.to_vec());
    let lines = (1..=body.split_inclusive(|b| *b == b'\n').count())
        .flat_map(|line| {
            std::iter::repeat_n(line, inserted.get(&line).copied().unwrap_or_default() + 1)
        })
        .collect();
    Ok(LoweredBody {
        bytes,
        lines,
        definitions: functions.definitions()?,
        renamed: functions.renamed,
    })
}

/// Lower a linked `PSpice` artifact and retain physical source provenance.
/// The ordinary native normalizer is applied first for existing shared syntax.
///
/// # Errors
/// Malformed statements, unsupported physical source semantics or invalid provenance.
pub fn normalize_mapped(
    input: &[u8],
    kind: RootKind,
    budget: &WorkBudget,
) -> Result<NativeNormalization> {
    budget.charge(Resource::ScannedBytes, input.len())?;
    let shared = super::normalize_mapped(input, kind, SourceDialect::Pspice, budget)?;
    lower_mapped(
        shared,
        kind,
        SourceDialects::Uniform(SourceDialect::Pspice),
        budget,
    )
}

pub(crate) fn lower_mapped(
    shared: MappedNormalization,
    kind: RootKind,
    dialects: SourceDialects<'_>,
    budget: &WorkBudget,
) -> Result<NativeNormalization> {
    budget.charge(Resource::EmittedBytes, shared.model.bytes.len())?;
    let first = usize::from(kind == RootKind::Deck);
    let title_end = if first != 0 {
        shared
            .model
            .bytes
            .iter()
            .position(|b| *b == b'\n')
            .map_or(shared.model.bytes.len(), |at| at + 1)
    } else {
        0
    };
    let LoweredBody {
        bytes: body,
        lines,
        definitions: (definitions, definition_lines),
        mut renamed,
    } = lower_body(
        &shared.model.bytes[title_end..],
        &shared.input_lines[first..],
        dialects,
        budget,
    )?;
    for device in &mut renamed {
        device.input_line = *shared
            .input_lines
            .get(first + device.input_line - 1)
            .context("missing renamed device origin")?;
    }
    budget.charge(
        Resource::EmittedBytes,
        title_end
            .saturating_add(definitions.len())
            .saturating_add(body.len()),
    )?;
    let mut bytes = shared.model.bytes[..title_end].to_vec();
    let mut input_lines = shared.input_lines[..first].to_vec();
    bytes.extend_from_slice(definitions.as_bytes());
    for line in definition_lines {
        input_lines.push(
            *shared
                .input_lines
                .get(first + line - 1)
                .context("missing PSpice helper origin")?,
        );
    }
    bytes.extend_from_slice(&body);
    for line in lines {
        input_lines.push(
            *shared
                .input_lines
                .get(first + line - 1)
                .context("missing PSpice source origin")?,
        );
    }
    let mut passes = shared.model.passes;
    if bytes != shared.model.bytes {
        passes.push(NormalizationPass::PspiceSource);
    }
    ensure!(
        input_lines.len() == bytes.split_inclusive(|b| *b == b'\n').count(),
        "PSpice source mapping is inconsistent"
    );
    Ok(NativeNormalization {
        mapped: MappedNormalization {
            model: crate::NormalizedModel { bytes, passes },
            waveforms: shared.waveforms,
            input_lines,
        },
        renamed,
    })
}
