//! Discrete, absolute-geometry `SIMetrix` MOS level 17 preparation.
//! Channel and junction terms use the native level-1 MOS equations, with
//! explicit effective geometry; the nonlinear gate-drain term is a
//! two-terminal charge primitive. The two arctangent terms have opposite
//! slopes, as verified by independent simulator measurements.
use super::{
    Translation, TranslationKind, model_header, normalized_statement, statement_tokens,
    validate_node, write_number,
};
use anyhow::{Context, Result, bail, ensure};
use model_normalize_ngspice::model_card::{
    ModelCard, ModelParameter, instance_fields, parse_model_card, parse_spice_constant,
};
use std::{collections::BTreeMap, fmt::Write as _};

#[derive(Clone, Copy, Debug)]
enum Polarity {
    N,
    P,
}

impl Polarity {
    const fn name(self) -> &'static str {
        match self {
            Self::N => "NMOS",
            Self::P => "PMOS",
        }
    }
    const fn sign(self) -> f64 {
        match self {
            Self::N => 1.,
            Self::P => -1.,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct GateDrain {
    min: f64,
    max: f64,
    gradient_min: f64,
    gradient_max: f64,
    threshold: f64,
}

#[derive(Clone, Debug)]
pub(super) struct RawLevel17Mos {
    polarity: Polarity,
    core: Vec<(&'static str, f64)>,
    nominal_temperature: Option<f64>,
    rd: f64,
    rg: f64,
    tc1rd: f64,
    tc2rd: f64,
    cgd: GateDrain,
    soa_limits: Vec<ModelParameter>,
    native_name: String,
    resistance_name: String,
    lexical: bool,
}

#[derive(Clone, Copy)]
struct CoreField {
    name: &'static str,
    default: f64,
    domain: Domain,
}

#[derive(Clone, Copy)]
enum Domain {
    Any,
    Nonnegative,
    Positive,
    Grading,
}

impl Domain {
    fn contains(self, value: f64) -> bool {
        value.is_finite()
            && match self {
                Self::Any => true,
                Self::Nonnegative => value >= 0.,
                Self::Positive => value > 0.,
                Self::Grading => (0. ..1.).contains(&value),
            }
    }
}

const fn field(name: &'static str, default: f64, domain: Domain) -> CoreField {
    CoreField {
        name,
        default,
        domain,
    }
}

// See SIMetrix Simulator Reference, MOS Level 17 model parameters.
// CJ is deliberately retained with zero instance areas: the vendor model
// ignores bottom capacitance density in the absence of absolute CBD/CBS.
const CORE_FIELDS: &[CoreField] = &[
    field("VTO", 0., Domain::Any),
    field("KP", 2e-5, Domain::Nonnegative),
    field("GAMMA", 0., Domain::Nonnegative),
    field("PHI", 0.6, Domain::Positive),
    field("LAMBDA", 0., Domain::Nonnegative),
    field("RS", 0., Domain::Nonnegative),
    field("CBD", 0., Domain::Nonnegative),
    field("CBS", 0., Domain::Nonnegative),
    field("IS", 1e-14, Domain::Nonnegative),
    field("PB", 0.8, Domain::Positive),
    field("CGSO", 0., Domain::Nonnegative),
    field("CGBO", 0., Domain::Nonnegative),
    field("CJ", 0., Domain::Nonnegative),
    field("MJ", 0.5, Domain::Grading),
    field("CJSW", 0., Domain::Nonnegative),
    field("MJSW", 0.5, Domain::Grading),
    field("FC", 0.5, Domain::Grading),
    field("KF", 0., Domain::Nonnegative),
    field("AF", 1., Domain::Nonnegative),
];

fn scalar(
    fields: &mut BTreeMap<String, String>,
    name: &str,
    default: f64,
    domain: Domain,
) -> Result<f64> {
    let value = fields
        .remove(name)
        .map_or(Ok(default), |raw| parse_spice_constant(&raw))?;
    ensure!(domain.contains(value), "invalid level 17 {name}={value}");
    Ok(value)
}

fn model_fields(card: ModelCard) -> Result<BTreeMap<String, String>> {
    let mut fields = BTreeMap::new();
    for parameter in card.parameters {
        let name = parameter.name.to_ascii_uppercase();
        let name = match name.as_str() {
            "VT0" => "VTO".to_owned(),
            "VLEVEL" => "LEVEL".to_owned(),
            _ => name,
        };
        let value = parameter
            .expression
            .with_context(|| format!("level 17 parameter {name} requires a value"))?;
        ensure!(
            fields.insert(name.clone(), value).is_none(),
            "duplicate level 17 model parameter {name}"
        );
    }
    Ok(fields)
}

fn selected_level(card: &ModelCard) -> Result<Option<f64>> {
    let mut levels = card.parameters.iter().filter(|field| {
        field.name.eq_ignore_ascii_case("LEVEL") || field.name.eq_ignore_ascii_case("VLEVEL")
    });
    let level = levels.next();
    ensure!(
        levels.next().is_none(),
        "ambiguous MOS level on {}",
        card.name
    );
    level
        .map(|field| {
            parse_spice_constant(field.expression.as_deref().context("missing MOS level")?)
        })
        .transpose()
}

fn raw_level17_model(statement: &str) -> Result<Option<(String, RawLevel17Mos)>> {
    let Some((name, family)) = model_header(statement) else {
        return Ok(None);
    };
    let polarity = match family.as_str() {
        "NMOS" => Polarity::N,
        "PMOS" => Polarity::P,
        _ => return Ok(None),
    };
    let normalized = normalized_statement(statement);
    let active = model_preprocess_simplis::active_statement_prefix(&normalized).trim();
    let mut card = parse_model_card(active, 1, &["LEVEL", "VLEVEL"])?;
    let legacy = card.parameters.iter().any(|field| {
        ["CGDMAX", "CGDMIN", "XG1CGD", "XG2CGD", "VTCGD"]
            .iter()
            .any(|name| field.name.eq_ignore_ascii_case(name))
    });
    let candidate = selected_level(&card)?;
    if !(matches!(candidate, Some(17.)) || matches!(candidate, Some(7.)) && legacy) {
        return Ok(None);
    }
    let (soa_limits, parameters) = card
        .parameters
        .into_iter()
        .partition(|field| field.name.to_ascii_uppercase().starts_with("LIMIT("));
    card.parameters = parameters;
    let mut fields = model_fields(card)?;
    fields.remove("LEVEL");
    let core = CORE_FIELDS
        .iter()
        .map(|f| Ok((f.name, scalar(&mut fields, f.name, f.default, f.domain)?)))
        .collect::<Result<Vec<_>>>()?;
    let nominal_temperature = fields
        .remove("TNOM")
        .map(|raw| parse_spice_constant(&raw))
        .transpose()?;
    ensure!(
        nominal_temperature.is_none_or(|t| t > -273.15),
        "invalid level 17 TNOM"
    );
    let rd = scalar(&mut fields, "RD", 0., Domain::Nonnegative)?;
    let rg = scalar(&mut fields, "RG", 0., Domain::Nonnegative)?;
    let tc1rd = scalar(&mut fields, "TC1RD", 0., Domain::Any)?;
    let tc2rd = scalar(&mut fields, "TC2RD", 0., Domain::Any)?;
    let cgd = GateDrain {
        min: scalar(&mut fields, "CGDMIN", 0., Domain::Nonnegative)?,
        max: scalar(&mut fields, "CGDMAX", 0., Domain::Nonnegative)?,
        gradient_min: scalar(&mut fields, "XG1CGD", 1., Domain::Any)?,
        gradient_max: scalar(&mut fields, "XG2CGD", 1., Domain::Any)?,
        threshold: scalar(&mut fields, "VTCGD", 0., Domain::Any)?,
    };
    ensure!(
        fields.is_empty(),
        "unsupported level 17 parameters: {:?}",
        fields.keys().collect::<Vec<_>>()
    );

    Ok(Some((
        name,
        RawLevel17Mos {
            polarity,
            core,
            nominal_temperature,
            rd,
            rg,
            tc1rd,
            tc2rd,
            cgd,
            soa_limits,
            native_name: String::new(),
            resistance_name: String::new(),
            lexical: false,
        },
    )))
}

fn write_scalar(output: &mut String, name: &str, value: f64) -> Result<()> {
    write!(output, " {name}=")?;
    write_number(output, value)
}

pub(super) fn emit_level17_core_model(name: &str, model: &RawLevel17Mos) -> Result<String> {
    let mut output = String::new();
    for limit in &model.soa_limits {
        let value = limit
            .expression
            .as_deref()
            .context("SOA limit requires a value")?;
        writeln!(
            output,
            "* SIMetrix LEVEL=17 {}={value} SOA metadata for {name}; not enforced by native simulator",
            limit.name
        )?;
    }
    write!(
        output,
        ".model {} {}(level=1 RD=0",
        model.native_name,
        model.polarity.name()
    )?;
    for &(name, value) in &model.core {
        write_scalar(&mut output, name, value)?;
    }
    if let Some(tnom) = model.nominal_temperature {
        write_scalar(&mut output, "TNOM", tnom)?;
    }
    output.push_str(")\n");
    if model.rd != 0. {
        write!(output, ".model {} R(", model.resistance_name)?;
        write_scalar(&mut output, "TC1", model.tc1rd)?;
        write_scalar(&mut output, "TC2", model.tc2rd)?;
        if let Some(tnom) = model.nominal_temperature {
            write_scalar(&mut output, "TNOM", tnom)?;
        }
        output.push_str(")\n");
    }
    Ok(output)
}

fn charge_term(
    output: &mut String,
    capacitance: f64,
    gradient: f64,
    threshold: f64,
    sign: f64,
    voltage: &str,
    decreasing: bool,
) -> Result<()> {
    write_number(output, capacitance)?;
    if capacitance == 0. {
        return Ok(());
    }
    if gradient == 0. {
        write!(output, "*(0.5*{voltage})")?;
        return Ok(());
    }
    // Integrate C(v)=C0*(1/2 +/- atan(g*(Vt-p*v))/pi), where p
    // is channel polarity. Differentiate the physical terminal charge,
    // not C(v)*v. ln is the natural logarithm in native expressions.
    let u = format!("(({threshold:.17e}-({sign:.17e})*{voltage})*({gradient:.17e}))");
    let coefficient = if decreasing { -sign } else { sign };
    write!(
        output,
        "*(0.5*{voltage}+({coefficient:.17e})*(({u})*atan({u})-0.5*ln(1+({u})^2))/(pi*({gradient:.17e})))"
    )?;
    Ok(())
}

#[derive(Default)]
struct Instance {
    multiplier: Option<f64>,
    temperature: Option<f64>,
    off: bool,
    ic: Option<Vec<f64>>,
}

impl Instance {
    fn parse(tail: &str) -> Result<Self> {
        let mut result = Self::default();
        let mut seen = std::collections::BTreeSet::new();
        for field in instance_fields(tail, 3)? {
            let name = field.name.to_ascii_uppercase();
            ensure!(
                seen.insert(name.clone()),
                "duplicate level 17 instance parameter {name}"
            );
            match name.as_str() {
                "M" | "TEMP" => {
                    let value = parse_spice_constant(
                        &field
                            .expression
                            .context("missing level 17 instance value")?,
                    )?;
                    if name == "M" {
                        ensure!(value > 0., "level 17 M must be positive");
                        result.multiplier = Some(value);
                    } else {
                        ensure!(value > -273.15, "invalid level 17 TEMP");
                        result.temperature = Some(value);
                    }
                }
                "OFF" => {
                    ensure!(field.expression.is_none(), "OFF is a flag");
                    result.off = true;
                }
                "IC" => {
                    let raw = field.expression.context("missing level 17 IC")?;
                    let values = raw
                        .trim()
                        .trim_start_matches('(')
                        .trim_end_matches(')')
                        .split(|c: char| c == ',' || c.is_ascii_whitespace())
                        .filter(|v| !v.is_empty())
                        .map(parse_spice_constant)
                        .collect::<Result<Vec<_>>>()?;
                    ensure!(
                        (1..=3).contains(&values.len()),
                        "level 17 IC requires one to three voltages"
                    );
                    result.ic = Some(values);
                }
                _ => bail!("unsupported level 17 instance parameter {name}"),
            }
        }
        Ok(result)
    }

    fn emit_shared(&self, output: &mut String) -> Result<()> {
        // An explicit unit multiplier avoids geometry-dependent simulator defaults.
        write_scalar(output, "M", self.multiplier.unwrap_or(1.))?;
        if let Some(temp) = self.temperature {
            write_scalar(output, "TEMP", temp)?;
        }
        Ok(())
    }

    fn emit_mos(&self, output: &mut String) -> Result<()> {
        self.emit_shared(output)?;
        if self.off {
            output.push_str(" OFF");
        }
        if let Some(values) = &self.ic {
            output.push_str(" IC=");
            for (i, value) in values.iter().enumerate() {
                if i != 0 {
                    output.push(',');
                }
                write_number(output, *value)?;
            }
        }
        Ok(())
    }
}

fn instance_tail(mut text: &str) -> &str {
    for _ in 0..6 {
        text = text.trim_start();
        text = text.find(char::is_whitespace).map_or("", |at| &text[at..]);
    }
    text.trim_start()
}

pub(super) fn translate_level17_mos(
    statement: &str,
    scope: &Scope<'_>,
) -> Result<Option<Translation>> {
    let normalized = normalized_statement(statement);
    let tokens = statement_tokens(&normalized)?;
    let Some(name) = tokens.first() else {
        return Ok(None);
    };
    if !name.starts_with(['M', 'm']) || tokens.len() < 6 {
        return Ok(None);
    }
    let Some(model) = scope.lookup(&tokens[5].to_ascii_uppercase()) else {
        return Ok(None);
    };
    let instance = Instance::parse(instance_tail(&normalized))?;
    for node in &tokens[1..5] {
        validate_node(node)?;
    }
    let stem = format!(
        "{}I_{}",
        scope.prefix,
        super::escaped_device_stem(&name.to_ascii_uppercase())
    );
    let drain = if model.rd == 0. {
        tokens[1].clone()
    } else {
        format!("{stem}_DINT")
    };
    let gate = if model.rg == 0. {
        tokens[2].clone()
    } else {
        format!("{stem}_GINT")
    };
    let mut output = String::new();
    if model.rd != 0. {
        write!(
            output,
            "R{stem}_RD {} {drain} {} ",
            tokens[1], model.resistance_name
        )?;
        write_number(&mut output, model.rd)?;
        instance.emit_shared(&mut output)?;
        output.push('\n');
    }
    if model.rg != 0. {
        write!(output, "R{stem}_RG {} {gate} ", tokens[2])?;
        write_number(&mut output, model.rg)?;
        instance.emit_shared(&mut output)?;
        output.push('\n');
    }
    // The source model is absolute: unit L/W and perimeters, zero areas.
    // CBD/CBS are absolute, CJSW contributes once to each junction,
    // and CJ does not contribute. Explicit values also defeat DEFA/DEFL/DEFW.
    write!(
        output,
        "{name} {drain} {gate} {} {} {} L=1 W=1 AD=0 AS=0 PD=1 PS=1",
        tokens[3], tokens[4], model.native_name
    )?;
    instance.emit_mos(&mut output)?;
    output.push('\n');
    if model.cgd.min != 0. || model.cgd.max != 0. {
        let voltage = format!("v({gate},{drain})");
        write!(output, "C{stem}_CGD {gate} {drain} Q='(")?;
        let cgd = model.cgd;
        let sign = model.polarity.sign();
        charge_term(
            &mut output,
            cgd.min,
            cgd.gradient_min,
            cgd.threshold,
            sign,
            &voltage,
            true,
        )?;
        output.push('+');
        charge_term(
            &mut output,
            cgd.max,
            cgd.gradient_max,
            cgd.threshold,
            sign,
            &voltage,
            false,
        )?;
        write!(output, ")*")?;
        write_number(&mut output, instance.multiplier.unwrap_or(1.))?;
        output.push_str("'\n");
    }
    Ok(Some(Translation {
        bytes: output.into_bytes(),
        kind: TranslationKind::SimetrixMosfetLevel17,
    }))
}

/// Electrical model lookup in the source lexical scope. Non-level-17
/// declarations are explicit shadow barriers. The generated namespace
/// is selected once across the complete linked input.
pub(super) struct Scope<'a> {
    prefix: &'a str,
    path: String,
    parent: Option<&'a Self>,
    declarations: BTreeMap<String, Option<RawLevel17Mos>>,
}

impl<'a> Scope<'a> {
    pub(super) fn global(prefix: &'a str, scopes: &[Vec<super::Item>]) -> Result<Self> {
        let mut result = Self {
            prefix,
            path: String::new(),
            parent: None,
            declarations: BTreeMap::new(),
        };
        let mut texts = BTreeMap::new();
        for items in scopes {
            for statement in super::electrical_statements(items) {
                let Some((name, _)) = model_header(statement) else {
                    continue;
                };
                let model = result.bind(&name, statement)?;
                if let Some(previous) = result.declarations.get(&name) {
                    // Identical library declarations can occur in multiple
                    // admitted resources. Never silently bind a conflicting
                    // discrete model to the other source's native card.
                    if previous.is_some() || model.is_some() {
                        ensure!(
                            texts.get(&name) == Some(&normalized_statement(statement)),
                            "conflicting global model declaration for {name}"
                        );
                    }
                } else {
                    texts.insert(name.clone(), normalized_statement(statement));
                    result.declarations.insert(name, model);
                }
            }
        }
        Ok(result)
    }

    pub(super) fn child<'b>(&'b self, block: &super::Subcircuit) -> Result<Scope<'b>> {
        let path = format!(
            "{}__{}",
            self.path,
            super::escaped_device_stem(&block.name.to_ascii_uppercase())
        );
        let mut result = Scope {
            prefix: self.prefix,
            path,
            parent: Some(self),
            declarations: BTreeMap::new(),
        };
        for statement in super::electrical_statements(&block.body) {
            let Some((name, _)) = model_header(statement) else {
                continue;
            };
            let model = result.bind(&name, statement)?;
            let selected = model.is_some();
            if let Some(previous) = result.declarations.insert(name.clone(), model) {
                ensure!(
                    previous.is_none() && !selected,
                    "duplicate local model declaration for {name}"
                );
            }
        }
        Ok(result)
    }

    fn bind(&self, name: &str, statement: &str) -> Result<Option<RawLevel17Mos>> {
        let Some((_, mut model)) = raw_level17_model(statement)? else {
            return Ok(None);
        };
        let stem = super::escaped_device_stem(name);
        model.lexical = self.parent.is_some();
        model.native_name = if model.lexical {
            format!("{}L{}__{stem}", self.prefix, self.path)
        } else {
            format!("{}M_{stem}", self.prefix)
        };
        model.resistance_name = format!("{}_RD", model.native_name);
        Ok(Some(model))
    }

    fn lookup(&self, name: &str) -> Option<&RawLevel17Mos> {
        self.declarations.get(name).map_or_else(
            || self.parent.and_then(|parent| parent.lookup(name)),
            Option::as_ref,
        )
    }

    pub(super) fn declared(&self, name: &str) -> Option<&RawLevel17Mos> {
        self.declarations.get(name).and_then(Option::as_ref)
    }

    /// Nested source subcircuits are emitted as flat definitions. Copy
    /// only inherited lexical model definitions actually used here;
    /// global models remain shared through the normal include graph.
    pub(super) fn emit_inherited(&self, items: &[super::Item], output: &mut String) -> Result<()> {
        let mut emitted = std::collections::BTreeSet::new();
        for statement in super::electrical_statements(items) {
            if !statement.trim_start().starts_with(['M', 'm']) {
                continue;
            }
            let tokens = statement_tokens(&normalized_statement(statement))?;
            if !tokens
                .first()
                .is_some_and(|name| name.starts_with(['M', 'm']))
            {
                continue;
            }
            let Some(name) = tokens.get(5).map(|name| name.to_ascii_uppercase()) else {
                continue;
            };
            if self.declarations.contains_key(&name) || !emitted.insert(name.clone()) {
                continue;
            }
            if let Some(model) = self.lookup(&name).filter(|model| model.lexical) {
                output.push_str(&emit_level17_core_model(&name, model)?);
            }
        }
        Ok(())
    }
}
