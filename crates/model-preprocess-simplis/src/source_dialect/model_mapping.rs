//! Source model selection, independent of electrical kernel implementation.
//!
//! A mapping key is the declared model type and level (zero for omitted
//! level), not a model instance name. The file language controls lexical
//! rules, while compatibility presets and explicit maps are simulation
//! configuration. Keep those two namespaces separate.

use std::{collections::BTreeMap, fmt::Write as _};

use anyhow::{Context, Result, bail, ensure};
use model_normalize_ngspice::model_card::{
    ModelCard, ModelParameter, parse_model_card, parse_parameter_fields, parse_spice_constant,
};
use model_preprocess_command::{Resource, WorkBudget};
use serde::{Deserialize, Serialize};

use super::{DialectContext, OpaqueSource, ScopedStatement};

/// Simulator implementation identity. Different charge, temperature and
/// geometry laws must not share an identity solely because a native
/// simulator uses the same level number.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Device {
    Diode1,
    Diode3,
    Resistor,
    HspiceResistor,
    Mos1,
    Mos2,
    Mos3,
    Mos17,
    Bsim3Pspice,
    Bsim3Berkeley,
    Bsim3HspiceJunction,
    Bsim3HspiceBerkeleyJunction,
    Bsim4,
    GummelPoon,
    Vbic,
    VbicThermal,
    Hicum211,
    HisimHv1,
    HisimHv2,
    Psp103N,
    Psp103P,
}

impl Device {
    const fn letter(self) -> char {
        match self {
            Self::Diode1 | Self::Diode3 => 'D',
            Self::Resistor | Self::HspiceResistor => 'R',
            Self::GummelPoon | Self::Vbic | Self::VbicThermal | Self::Hicum211 => 'Q',
            _ => 'M',
        }
    }

    /// Known internal implementation identifiers from the published
    /// mapping examples. Do not interpret unknown target strings as a
    /// model level or silently fall back to another device.
    fn named(name: &str) -> Result<Self> {
        match name.to_ascii_uppercase().as_str() {
            "DIODE1" => Ok(Self::Diode1),
            "DIODE3" => Ok(Self::Diode3),
            "HSPICERES" => Ok(Self::HspiceResistor),
            "MOS1" => Ok(Self::Mos1),
            "MOS2" => Ok(Self::Mos2),
            "MOS3" => Ok(Self::Mos3),
            "VBIC" => Ok(Self::Vbic),
            "PSP103_N" => Ok(Self::Psp103N),
            "PSP103_P" => Ok(Self::Psp103P),
            _ => bail!("unknown or unsupported source mapping target {name}"),
        }
    }

    /// Canonical source level; this is deliberately *not* an assertion
    /// that the target simulator implements this source variant.
    #[must_use = "the returned level identifies the source-model variant"]
    pub const fn source_level(self) -> u16 {
        match self {
            Self::Diode1 | Self::Resistor | Self::Mos1 | Self::GummelPoon => 1,
            Self::HspiceResistor | Self::Mos2 => 2,
            Self::Diode3 | Self::Mos3 => 3,
            Self::Vbic => 4,
            Self::Bsim3Pspice => 7,
            Self::Bsim3Berkeley | Self::Hicum211 => 8,
            Self::Bsim4 => 14,
            Self::Mos17 => 17,
            Self::Bsim3HspiceJunction => 49,
            Self::Bsim3HspiceBerkeleyJunction => 53,
            Self::HisimHv1 => 62,
            Self::HisimHv2 => 73,
            Self::VbicThermal => 1004,
            // A PSP alias carries the implementation identity itself;
            // no invented canonical source level is assigned.
            Self::Psp103N | Self::Psp103P => 0,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ModelKey {
    pub model_type: String,
    pub level: u16,
}

impl ModelKey {
    /// Construct a case-insensitive model key.
    ///
    /// # Errors
    /// Invalid source model type.
    pub fn new(model_type: &str, level: u16) -> Result<Self> {
        ensure!(
            !model_type.is_empty()
                && model_type
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'$')),
            "invalid mapping model type {model_type}"
        );
        Ok(Self {
            model_type: model_type.to_ascii_uppercase(),
            level,
        })
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SelectionOrigin {
    Builtin,
    DeviceConfiguration,
    HspicePreset,
    StandaloneHspice,
    NetlistMap,
    LegacyMos17Signature,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Mapping {
    pub key: ModelKey,
    pub device: Device,
    pub letter: char,
    pub origin: SelectionOrigin,
    /// Source resource or explicit configuration identity for diagnostics.
    pub source: Option<String>,
}

/// All original source model-card fields, effective selection and location.
/// Resolution does not silently discard the requested version or polarity.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ModelSelection {
    pub source: String,
    pub scope: Vec<String>,
    pub context: DialectContext,
    pub original: String,
    pub card: ModelCard,
    /// Inclusive physical end of the source declaration.
    pub last_line: usize,
    pub declared_level: Option<u16>,
    pub declared_version: Option<String>,
    pub effective_version: Option<String>,
    pub mapping: Mapping,
}

#[derive(Clone, Debug, Default)]
pub struct DeviceConfiguration {
    entries: Vec<MapDeclaration>,
}

#[derive(Clone, Debug)]
struct MapDeclaration {
    key: ModelKey,
    device: Device,
    letter: Option<char>,
    source: String,
}

fn one<'a>(fields: &'a [ModelParameter], name: &str) -> Result<Option<&'a str>> {
    let mut matching = fields.iter().filter(|f| f.name.eq_ignore_ascii_case(name));
    let Some(first) = matching.next() else {
        return Ok(None);
    };
    ensure!(matching.next().is_none(), "duplicate {name} assignment");
    first
        .expression
        .as_deref()
        .map(Some)
        .with_context(|| format!("{name} requires a value"))
}

fn required<'a>(fields: &'a [ModelParameter], name: &str) -> Result<&'a str> {
    one(fields, name)?.with_context(|| format!("missing {name} assignment"))
}

fn unsigned_level(text: &str) -> Result<u16> {
    let value = parse_spice_constant(text)?;
    ensure!(
        value >= 0. && value <= f64::from(u16::MAX) && value.fract() == 0.,
        "model level must be a nonnegative integer: {text}"
    );
    // A structural level is parsed once, never on a physical trial path.
    Ok(format!("{value:.0}").parse()?)
}

impl MapDeclaration {
    fn parse(text: &str, source: &str) -> Result<Self> {
        let fields = parse_parameter_fields(text, &[])?;
        for field in &fields {
            ensure!(
                ["MODELNAME", "LEVEL", "DEVICE", "LETTER", "REPORT"]
                    .iter()
                    .any(|name| field.name.eq_ignore_ascii_case(name)),
                "unknown mapping field {}",
                field.name
            );
        }
        let key = ModelKey::new(
            required(&fields, "MODELNAME")?,
            unsigned_level(required(&fields, "LEVEL")?)?,
        )?;
        let device = Device::named(required(&fields, "DEVICE")?)?;
        let letter = one(&fields, "LETTER")?
            .map(|value| {
                let mut chars = value.chars();
                let letter = chars
                    .next()
                    .context("empty access letter")?
                    .to_ascii_uppercase();
                ensure!(
                    letter.is_ascii_alphabetic() && chars.next().is_none(),
                    "invalid access letter {value}"
                );
                Ok::<_, anyhow::Error>(letter)
            })
            .transpose()?;
        if let Some(value) = one(&fields, "REPORT")? {
            ensure!(
                value.eq_ignore_ascii_case("ON") || value.eq_ignore_ascii_case("OFF"),
                "invalid REPORT setting {value}"
            );
        }
        Ok(Self {
            key,
            device,
            letter,
            source: source.into(),
        })
    }
}

impl DeviceConfiguration {
    /// Parse explicit DCF input. It is a request-owned immutable value,
    /// never loaded from a mutable simulator installation directory.
    ///
    /// # Errors
    /// Invalid or unsupported declaration, target, field or level.
    pub fn parse(source: &str, text: &str, budget: &WorkBudget) -> Result<Self> {
        budget.charge(Resource::ScannedBytes, text.len())?;
        let mut entries = Vec::new();
        for (index, line) in text.lines().enumerate() {
            budget.charge(Resource::Statements, 1)?;
            let line = line.trim();
            if line.is_empty() || line.starts_with(['*', ';', '#']) {
                continue;
            }
            budget.charge(Resource::GeneratedNodes, 1)?;
            entries.push(
                MapDeclaration::parse(line, source)
                    .with_context(|| format!("device configuration {source}:{}", index + 1))?,
            );
        }
        Ok(Self { entries })
    }
}

#[derive(Clone, Debug, Default)]
pub struct MappingTable {
    entries: BTreeMap<ModelKey, Mapping>,
    hspice_models: bool,
}

fn builtin(key: &ModelKey) -> Option<Device> {
    let level = key.level;
    match (key.model_type.as_str(), level) {
        ("D", 0 | 1) => Some(Device::Diode1),
        ("D", 3) => Some(Device::Diode3),
        ("R" | "RES", 0 | 1) => Some(Device::Resistor),
        ("R" | "RES", 2) => Some(Device::HspiceResistor),
        ("NMOS" | "PMOS", 0 | 1) => Some(Device::Mos1),
        ("NMOS" | "PMOS", 2) => Some(Device::Mos2),
        ("NMOS" | "PMOS", 3) => Some(Device::Mos3),
        ("NMOS" | "PMOS", 7) => Some(Device::Bsim3Pspice),
        ("NMOS" | "PMOS", 8) => Some(Device::Bsim3Berkeley),
        ("NMOS" | "PMOS", 14) => Some(Device::Bsim4),
        ("NMOS" | "PMOS", 17) => Some(Device::Mos17),
        ("NMOS" | "PMOS", 49) => Some(Device::Bsim3HspiceJunction),
        ("NMOS" | "PMOS", 53) => Some(Device::Bsim3HspiceBerkeleyJunction),
        ("NMOS" | "PMOS", 62) => Some(Device::HisimHv1),
        ("NMOS" | "PMOS", 73) => Some(Device::HisimHv2),
        ("NPN" | "PNP" | "LPNP", 0 | 1) => Some(Device::GummelPoon),
        ("NPN" | "PNP", 4) => Some(Device::Vbic),
        ("NPN" | "PNP", 8) | ("HICUM_211", 0) => Some(Device::Hicum211),
        ("NPN" | "PNP", 1004) => Some(Device::VbicThermal),
        _ => None,
    }
}

fn known_family(name: &str) -> bool {
    matches!(
        name,
        "D" | "R" | "RES" | "NMOS" | "PMOS" | "NPN" | "PNP" | "LPNP" | "HICUM_211"
    )
}

fn hspice(key: &ModelKey) -> Option<Device> {
    match (key.model_type.as_str(), key.level) {
        ("D", 0 | 1) => Some(Device::Diode3),
        ("R" | "RES", 0) => Some(Device::HspiceResistor),
        ("NMOS" | "PMOS", 54) => Some(Device::Bsim4),
        _ => None,
    }
}

fn bool_option(value: Option<&str>, name: &str) -> Result<bool> {
    match value {
        None | Some("1") => Ok(true),
        Some("0") => Ok(false),
        Some(text) if text.eq_ignore_ascii_case("TRUE") || text.eq_ignore_ascii_case("ON") => {
            Ok(true)
        }
        Some(text) if text.eq_ignore_ascii_case("FALSE") || text.eq_ignore_ascii_case("OFF") => {
            Ok(false)
        }
        Some(text) => bail!("invalid {name} selector {text}"),
    }
}

/// A global directive in the admitted graph; .FILE and .CONTROL bodies
/// are separate languages and must not contribute electrical mappings.
fn directives(statements: &[ScopedStatement]) -> impl Iterator<Item = (&ScopedStatement, bool)> {
    let mut opaque = OpaqueSource::default();
    let mut scope_depth = 0usize;
    let mut conditional_depth = 0usize;
    statements.iter().filter_map(move |line| {
        if opaque.consume(&line.text) {
            return None;
        }
        let first = line
            .text
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        match first.as_str() {
            ".SUBCKT" => scope_depth = scope_depth.saturating_add(1),
            ".ENDS" => scope_depth = scope_depth.saturating_sub(1),
            ".IF" | ".WHILE" | ".FOR" => conditional_depth = conditional_depth.saturating_add(1),
            ".ENDIF" | ".ENDWHILE" | ".ENDFOR" => {
                conditional_depth = conditional_depth.saturating_sub(1);
            }
            _ => (),
        }
        Some((line, scope_depth != 0 || conditional_depth != 0))
    })
}

fn charge_mapping_scan(statements: &[ScopedStatement], budget: &WorkBudget) -> Result<()> {
    budget.charge(Resource::Statements, statements.len())?;
    for statement in statements {
        budget.charge(Resource::ScannedBytes, statement.text.len())?;
    }
    Ok(())
}

fn require_global(line: &ScopedStatement, nested: bool) -> Result<()> {
    ensure!(
        !nested,
        "simulation-wide model selection in a local or conditional scope at {}:{}",
        line.source,
        line.first_line
    );
    Ok(())
}

impl MappingTable {
    /// Build the simulation-local table from the caller's DCF snapshot,
    /// global compatibility options and all active deck .MAP declarations.
    ///
    /// Explicit deck declarations override defaults. Conflicting duplicate
    /// declarations at one precedence are rejected, rather than inferring
    /// unspecified order-dependent model physics.
    ///
    /// # Errors
    /// Invalid mapping, ambiguous configuration or unsupported target.
    pub fn resolve(
        configuration: &DeviceConfiguration,
        statements: &[ScopedStatement],
        budget: &WorkBudget,
    ) -> Result<Self> {
        charge_mapping_scan(statements, budget)?;
        let mut table = Self::default();
        let mut hspice_models = None;
        for (line, nested) in directives(statements) {
            budget.check()?;
            let text = line.text.trim_start();
            let (first, tail) = text.split_at(text.find(char::is_whitespace).unwrap_or(text.len()));
            if [".OPTIONS", ".OPTION", ".OPT"]
                .iter()
                .any(|name| first.eq_ignore_ascii_case(name))
            {
                let fields = parse_parameter_fields(tail, &[])?;
                for field in fields {
                    if field.name.eq_ignore_ascii_case("HSPICEMODELS") {
                        require_global(line, nested)?;
                        let enabled = bool_option(field.expression.as_deref(), &field.name)?;
                        ensure!(
                            hspice_models.is_none_or(|previous| previous == enabled),
                            "conflicting HSPICEMODELS selections"
                        );
                        hspice_models = Some(enabled);
                    } else if field.name.eq_ignore_ascii_case("HSPICECOMPATIBILITY") {
                        require_global(line, nested)?;
                        ensure!(
                            !bool_option(field.expression.as_deref(), &field.name)?,
                            "HSPICECOMPATIBILITY also selects TEMP, TNOM and MCHSPICE; use HSPICEMODELS for model mapping alone"
                        );
                    }
                }
            }
        }
        table.hspice_models = hspice_models.unwrap_or_default();
        for entry in &configuration.entries {
            budget.charge(Resource::GeneratedNodes, 1)?;
            table.insert(entry, SelectionOrigin::DeviceConfiguration)?;
        }
        charge_mapping_scan(statements, budget)?;
        for (line, nested) in directives(statements) {
            budget.check()?;
            let text = line.text.trim_start();
            let (first, tail) = text.split_at(text.find(char::is_whitespace).unwrap_or(text.len()));
            if first.eq_ignore_ascii_case(".MAP") {
                require_global(line, nested)?;
                budget.charge(Resource::GeneratedNodes, 1)?;
                let entry = MapDeclaration::parse(tail, &line.source)?;
                table.insert(&entry, SelectionOrigin::NetlistMap)?;
            }
        }
        Ok(table)
    }

    fn base(&self, key: &ModelKey) -> Option<Mapping> {
        let (device, origin) = if self.hspice_models
            && let Some(device) = hspice(key)
        {
            (device, SelectionOrigin::HspicePreset)
        } else {
            (builtin(key)?, SelectionOrigin::Builtin)
        };
        Some(Mapping {
            key: key.clone(),
            device,
            letter: device.letter(),
            origin,
            source: None,
        })
    }

    fn insert(&mut self, declaration: &MapDeclaration, origin: SelectionOrigin) -> Result<()> {
        let previous = self
            .entries
            .get(&declaration.key)
            .cloned()
            .or_else(|| self.base(&declaration.key));
        let letter = declaration
            .letter
            .or_else(|| previous.as_ref().map(|entry| entry.letter))
            .context("new model mapping requires LETTER")?;
        if let Some(existing) = &previous {
            ensure!(
                existing.letter == letter,
                "cannot change access letter of an existing model mapping"
            );
            ensure!(
                existing.device.letter() == declaration.device.letter(),
                "incompatible mapped physical model family"
            );
            if existing.origin == origin {
                ensure!(
                    existing.device == declaration.device,
                    "ambiguous duplicate model mapping"
                );
            }
        }
        // New aliases may use the conventional letter or the explicitly
        // documented extension letters. Native rewriting separately checks
        // if the selected access spelling can be represented.
        ensure!(
            letter == declaration.device.letter() || matches!(letter, 'N' | 'P' | 'W' | 'U' | 'Y'),
            "mapping access letter {letter} cannot instantiate the selected device"
        );
        self.entries.insert(
            declaration.key.clone(),
            Mapping {
                key: declaration.key.clone(),
                device: declaration.device,
                letter,
                origin,
                source: Some(declaration.source.clone()),
            },
        );
        Ok(())
    }

    /// Resolve a model type and source LEVEL (zero for omission).
    ///
    /// # Errors
    /// Unknown level of a recognized device family.
    pub fn select(&self, key: &ModelKey) -> Result<Option<Mapping>> {
        let result = self.entries.get(key).cloned().or_else(|| self.base(key));
        ensure!(
            result.is_some() || !known_family(&key.model_type),
            "unsupported source model type {} level {}",
            key.model_type,
            key.level
        );
        Ok(result)
    }

    /// True if unmodeled resistors select the HSPICE implementation.
    #[must_use = "the result selects the default resistor implementation"]
    pub fn hspice_default_resistor(&self) -> bool {
        let key = ModelKey {
            model_type: "R".into(),
            level: 0,
        };
        self.entries
            .get(&key)
            .cloned()
            .or_else(|| self.base(&key))
            .is_some_and(|mapping| mapping.device == Device::HspiceResistor)
    }

    /// Select a qualified standalone HSPICE model, not a `SIMetrix` deck preset.
    ///
    /// # Errors
    /// The family, level or version has no qualified native lowering.
    pub fn select_standalone_hspice(
        line: &ScopedStatement,
        scope: &[String],
        budget: &WorkBudget,
    ) -> Result<ModelSelection> {
        let table = Self {
            hspice_models: true,
            ..Self::default()
        };
        let mut selection = table
            .select_card(line, scope, budget)?
            .context("HSPICE model has no qualified native mapping")?;
        let diode = selection.mapping.device == Device::Diode3
            && selection.mapping.key.model_type == "D"
            && selection.mapping.key.level <= 1
            && selection.declared_version.is_none();
        let bsim4 = selection.mapping.device == Device::Bsim4
            && selection.mapping.key.level == 54
            && matches!(
                selection
                    .declared_version
                    .as_deref()
                    .map(|v| v.trim_matches(['\'', '"'])),
                Some("4.5" | "4.50" | "4.5.0" | "4.64" | "4.6.4" | "4.7" | "4.70" | "4.7.0")
            );
        if let Some(defaults) = selection.hspice_bsim3_defaults() {
            let junction = selection
                .hspice_mos_junction()?
                .context("missing BSIM3 junction selection")?;
            bail!(
                "HSPICE BSIM3 level {} VERSION={} (junction {:?}, default ACM={}, CAPMOD={}) has no qualified native lowering",
                selection.mapping.key.level,
                selection.effective_version.as_deref().unwrap_or("unknown"),
                junction.mode,
                defaults.acm,
                defaults.capmod,
            );
        }
        ensure!(
            diode || bsim4,
            "HSPICE model {} has no qualified native mapping (BSIM4 requires an exact supported VERSION)",
            selection.card.name
        );
        selection.mapping.origin = SelectionOrigin::StandaloneHspice;
        Ok(selection)
    }

    fn family_mappings<'a>(&'a self, family: &'a str) -> impl Iterator<Item = &'a Mapping> {
        self.entries
            .range(
                ModelKey {
                    model_type: family.to_owned(),
                    level: 0,
                }..,
            )
            .take_while(move |(key, _)| key.model_type == family)
            .map(|(_, mapping)| mapping)
    }

    fn positional_fields(&self, family: &str) -> Vec<&'static str> {
        let mut fields = vec!["LEVEL", "VERSION", "VLEVEL"];
        if family == "D"
            || self
                .family_mappings(family)
                .any(|mapping| mapping.device == Device::Diode1)
        {
            fields.extend_from_slice(model_kernel_diode::SOURCE_PARAMETERS);
        }
        fields
    }

    /// Resolve an admitted card before source-family lowering. Other
    /// device classes retain their own dedicated frontend.
    ///
    /// # Errors
    /// Invalid or ambiguous LEVEL/VERSION, unsupported physical variant.
    pub fn select_card(
        &self,
        line: &ScopedStatement,
        scope: &[String],
        budget: &WorkBudget,
    ) -> Result<Option<ModelSelection>> {
        let text = line.text.trim_start();
        let Some((directive, remainder)) = text.split_once(char::is_whitespace) else {
            return Ok(None);
        };
        if !directive.eq_ignore_ascii_case(".MODEL") {
            return Ok(None);
        }
        let mut header = remainder.split_whitespace();
        let Some(_) = header.next() else {
            bail!("missing model declaration");
        };
        let Some(family) = header.next() else {
            bail!("missing model type");
        };
        let family = family
            .split('(')
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        if !known_family(&family) && self.family_mappings(&family).next().is_none() {
            return Ok(None);
        }
        let positional = self.positional_fields(&family);
        let card = parse_model_card(text, line.first_line, &positional)?;
        let level = one(&card.parameters, "LEVEL")?;
        let vlevel = if matches!(family.as_str(), "NMOS" | "PMOS") {
            one(&card.parameters, "VLEVEL")?
        } else {
            None
        };
        ensure!(
            level.is_none() || vlevel.is_none(),
            "ambiguous LEVEL and VLEVEL on {}",
            card.name
        );
        let declared_level = level.or(vlevel).map(unsigned_level).transpose()?;
        let key = ModelKey::new(&card.family, declared_level.unwrap_or(0))?;
        let Some(mut mapping) = self.select(&key)? else {
            return Ok(None);
        };
        let legacy_signature = card.parameters.iter().any(|parameter| {
            ["CGDMAX", "CGDMIN", "XG1CGD", "XG2CGD", "VTCGD"]
                .iter()
                .any(|name| parameter.name.eq_ignore_ascii_case(name))
        });
        if mapping.device == Device::Bsim3Pspice
            && mapping.origin == SelectionOrigin::Builtin
            && legacy_signature
        {
            mapping.device = Device::Mos17;
            mapping.origin = SelectionOrigin::LegacyMos17Signature;
        }
        ensure!(
            !legacy_signature
                || !matches!(mapping.device, Device::Mos1 | Device::Mos2 | Device::Mos3),
            "discrete LEVEL=17 capacitance parameters on incompatible source model {}",
            card.name
        );
        let version = one(&card.parameters, "VERSION")?.map(str::to_owned);
        let effective_version = resolve_version(mapping.device, version.as_deref())?;
        super::identity::charge_scope_copy(scope, budget)?;
        budget.charge(
            Resource::EmittedBytes,
            line.source.len().saturating_add(line.text.len()),
        )?;
        Ok(Some(ModelSelection {
            source: line.source.clone(),
            scope: scope.to_vec(),
            context: line.context,
            original: line.text.clone(),
            card,
            last_line: line.last_line,
            declared_level,
            declared_version: version,
            effective_version,
            mapping,
        }))
    }
}

fn resolve_version(device: Device, version: Option<&str>) -> Result<Option<String>> {
    let version = version.map(|value| value.trim_matches(['\'', '"']));
    let selected = match device {
        Device::Bsim3HspiceJunction | Device::Bsim3HspiceBerkeleyJunction => match version {
            Some("3.24" | "3.2.4") => "3.24",
            Some("3.3" | "3.30" | "3.3.0") => "3.3",
            Some(other) => bail!("unqualified HSPICE BSIM3 source VERSION={other}"),
            None => bail!("HSPICE BSIM3 requires an exact supported VERSION"),
        },
        Device::Bsim3Pspice | Device::Bsim3Berkeley => match version {
            None | Some("3.3" | "3.30" | "3.3.0") => "3.3",
            Some(
                "3.2" | "3.20" | "3.21" | "3.22" | "3.23" | "3.24" | "3.2.0" | "3.2.1" | "3.2.2"
                | "3.2.3" | "3.2.4",
            ) => "3.24",
            Some(other) => bail!("unsupported BSIM3 source VERSION={other}"),
        },
        Device::Bsim4 => match version {
            None | Some("4.80" | "4.8" | "4.8.0") => "4.80",
            Some("4.0" | "4.1" | "4.2" | "4.21" | "4.2.1") => "4.21",
            Some("4.3" | "4.30" | "4.3.0") => "4.3",
            Some("4.4" | "4.40" | "4.4.0") => "4.4",
            Some("4.5" | "4.50" | "4.5.0") => "4.5",
            Some("4.6" | "4.60" | "4.6.0") => "4.60",
            Some("4.61" | "4.6.1") => "4.61",
            Some("4.62" | "4.6.2") => "4.62",
            Some("4.63" | "4.6.3") => "4.63",
            Some("4.64" | "4.6.4") => "4.64",
            Some("4.65" | "4.6.5") => "4.65",
            Some("4.7" | "4.70" | "4.7.0") => "4.70",
            Some(other) => bail!("unsupported BSIM4 source VERSION={other}"),
        },
        Device::Hicum211 => match version {
            None | Some("2.11" | "2.1.1") => "2.11",
            Some(other) => bail!("unsupported HICUM source VERSION={other}"),
        },
        Device::HisimHv1 => match version {
            None | Some("1.21" | "1.2.1") => "1.2.1",
            Some(other) => bail!("unsupported HiSIM-HV1 source VERSION={other}"),
        },
        Device::HisimHv2 => match version {
            Some("2.1" | "2.1.0") => "2.1.0",
            Some("2.2" | "2.2.0") => "2.2.0",
            Some("2.3" | "2.3.3") => "2.3.3",
            Some("2.4" | "2.4.2") => "2.4.2",
            _ => bail!("HiSIM-HV2 source requires an explicit supported VERSION"),
        },
        Device::Psp103N | Device::Psp103P => match version {
            None | Some("1.03" | "1.0.3") => "1.03",
            Some(other) => bail!("unsupported PSP source VERSION={other}"),
        },
        _ => return Ok(version.map(str::to_owned)),
    };
    Ok(Some(selected.into()))
}

/// Selected models and rewritten source, prior to hierarchy specialization.
#[derive(Clone, Debug)]
pub struct MappedSource {
    pub statements: Vec<ScopedStatement>,
    pub selections: Vec<ModelSelection>,
}

/// The source junction/series-resistance selector, not a native ACM number.
///
/// Berkeley variants share a junction law but retain distinct extrinsic
/// resistance geometry. Do not equate these with ngspice ACM=10–13.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HspiceMosJunctionMode {
    HspiceExplicit,
    HspiceWidthNormalized,
    HspiceIsolated,
    HspiceStacked,
    BerkeleyExplicit,
    BerkeleyWidthNormalized,
    BerkeleyIsolated,
    BerkeleyStacked,
}

/// The selected structural junction mode before occurrence preparation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HspiceMosJunctionSelection {
    pub mode: HspiceMosJunctionMode,
    /// Automatic area and perimeter calculation applies only to BSIM3 ACM12.
    pub automatic_bsim3_geometry: bool,
}

/// Source defaults for BSIM3 levels 49 and 53. These differ from the native
/// defaults even when the channel release has the same version number.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HspiceBsim3Defaults {
    pub acm: u8,
    pub capmod: u8,
}

impl HspiceBsim3Defaults {
    /// Both source levels use zero default bottom saturation-current density.
    pub const JUNCTION_CURRENT_DENSITY: f64 = 0.;
    /// Source defaults differ from Berkeley for both capacitance densities.
    pub const BOTTOM_CAPACITANCE_DENSITY: f64 = 5.79e-4;
    pub const SIDEWALL_CAPACITANCE_DENSITY: f64 = 0.;
    pub const SIDEWALL_CURRENT_DENSITY: f64 = 0.;
}

impl ModelSelection {
    /// Defaults of the source model, before explicit or occurrence-dependent
    /// parameters are applied. This is not a native lowering qualification.
    #[must_use = "source defaults must be applied before explicit parameters"]
    pub const fn hspice_bsim3_defaults(&self) -> Option<HspiceBsim3Defaults> {
        // HSPICE MOSFET Models Manual X-2005.09, pp. 403, 414–415.
        match self.mapping.device {
            Device::Bsim3HspiceJunction => Some(HspiceBsim3Defaults { acm: 0, capmod: 0 }),
            Device::Bsim3HspiceBerkeleyJunction => Some(HspiceBsim3Defaults { acm: 10, capmod: 3 }),
            _ => None,
        }
    }

    /// Select the source-specific ACM and CALCACM interpretation.
    ///
    /// The caller must be processing an HSPICE model. Literal structural
    /// selectors must be known before choosing the electrical implementation.
    ///
    /// # Errors
    /// Duplicate, nonliteral or unsupported structural selectors.
    pub fn hspice_mos_junction(&self) -> Result<Option<HspiceMosJunctionSelection>> {
        let bsim3 = self.hspice_bsim3_defaults();
        let default_acm = if let Some(defaults) = bsim3 {
            f64::from(defaults.acm)
        } else if self.mapping.device == Device::Bsim4 && self.mapping.key.level == 54 {
            12.
        } else {
            return Ok(None);
        };
        let acm = one(&self.card.parameters, "ACM")?
            .map(parse_spice_constant)
            .transpose()?
            .unwrap_or(default_acm);
        let mode = match acm {
            0. => HspiceMosJunctionMode::HspiceExplicit,
            1. => HspiceMosJunctionMode::HspiceWidthNormalized,
            2. => HspiceMosJunctionMode::HspiceIsolated,
            3. => HspiceMosJunctionMode::HspiceStacked,
            10. => HspiceMosJunctionMode::BerkeleyExplicit,
            11. => HspiceMosJunctionMode::BerkeleyWidthNormalized,
            12. => HspiceMosJunctionMode::BerkeleyIsolated,
            13. => HspiceMosJunctionMode::BerkeleyStacked,
            _ => bail!("unsupported HSPICE ACM selector {acm}"),
        };
        let calculate = match one(&self.card.parameters, "CALCACM")?
            .map(parse_spice_constant)
            .transpose()?
        {
            Some(0.) => false,
            Some(1.) => true,
            None => bsim3.is_none(),
            Some(value) => bail!("invalid HSPICE CALCACM selector {value}"),
        };
        Ok(Some(HspiceMosJunctionSelection {
            mode,
            automatic_bsim3_geometry: bsim3.is_some()
                && mode == HspiceMosJunctionMode::BerkeleyIsolated
                && calculate,
        }))
    }

    /// Canonical model card for the existing electrical lowering stage.
    ///
    /// Selection does not certify full numerical equivalence of a device
    /// implementation. Variants for which no corresponding implementation
    /// exists must not reach a different native model accidentally.
    ///
    /// # Errors
    /// Unsupported device family, version or access letter.
    pub fn lower(&self) -> Result<String> {
        use Device::{
            Bsim3Berkeley, Bsim4, Diode1, Diode3, GummelPoon, Mos1, Mos2, Mos3, Mos17, Resistor,
            Vbic,
        };
        let device = self.mapping.device;
        ensure!(
            self.mapping.letter == device.letter(),
            "mapped access letter {} requires instance lowering for {}",
            self.mapping.letter,
            self.card.name
        );
        let original_family = self.card.family.to_ascii_uppercase();
        let (family, level) = match device {
            Diode1 => (model_kernel_diode::MODEL_FAMILY, 1),
            Diode3 => ("D", device.source_level()),
            Resistor => ("R", 0),
            Mos1 | Mos2 | Mos3 | Mos17 | Bsim3Berkeley | Bsim4 => {
                ensure!(
                    matches!(original_family.as_str(), "NMOS" | "PMOS"),
                    "mapped MOS type {} does not specify polarity",
                    self.card.family
                );
                (original_family.as_str(), device.source_level())
            }
            GummelPoon | Vbic => {
                ensure!(
                    matches!(original_family.as_str(), "NPN" | "PNP"),
                    "mapped BJT type {} does not specify polarity",
                    self.card.family
                );
                (original_family.as_str(), device.source_level())
            }
            _ => bail!(
                "source model {} selects {device:?}, which has no qualified native lowering",
                self.card.name
            ),
        };
        self.validate_native_variant()?;
        // Unchanged cards retain their original spelling, including
        // vendor-specific fields interpreted by downstream lowerers.
        let needs_level = self.declared_level.is_some()
            || matches!(device, Diode1 | Diode3)
            || self.mapping.origin != SelectionOrigin::Builtin;
        let native_version = self.effective_version.as_deref();
        let original_version = self.declared_version.as_deref();
        if family.eq_ignore_ascii_case(&self.card.family)
            && !needs_level
            && native_version == original_version
        {
            return Ok(self.original.clone());
        }
        let mut out = format!(".model {} {family} (", self.card.name);
        if needs_level && level != 0 {
            write!(out, "LEVEL={level} ")?;
        }
        if let Some(version) = native_version {
            write!(out, "VERSION={version} ")?;
        }
        for parameter in &self.card.parameters {
            if parameter.name.eq_ignore_ascii_case("LEVEL")
                || parameter.name.eq_ignore_ascii_case("VERSION")
                || matches!(device, Mos1 | Mos2 | Mos3 | Mos17 | Bsim3Berkeley | Bsim4)
                    && parameter.name.eq_ignore_ascii_case("VLEVEL")
            {
                continue;
            }
            out.push_str(&parameter.name);
            if let Some(value) = &parameter.expression {
                write!(out, "={value}")?;
            }
            out.push(' ');
        }
        if device == Diode3 {
            self.append_diode3_defaults(&mut out)?;
        }
        out.push(')');
        Ok(out)
    }

    fn validate_native_variant(&self) -> Result<()> {
        match self.mapping.device {
            Device::Diode1 => self.validate_diode1()?,
            Device::Diode3 => {
                for field in &self.card.parameters {
                    ensure!(
                        !["SHRINK", "XW", "DCAP"]
                            .iter()
                            .any(|name| field.name.eq_ignore_ascii_case(name)),
                        "source diode level 3 parameter {} has no qualified native lowering",
                        field.name
                    );
                }
            }
            Device::Bsim3Berkeley => {
                // Native BSIM3v32 implements 3.2.4 (also accepts 3.24),
                // while the current BSIM3 implements 3.3.
                ensure!(
                    matches!(self.effective_version.as_deref(), Some("3.24" | "3.3")),
                    "BSIM3 source version has no corresponding native implementation"
                );
            }
            Device::Bsim4 => {
                // Native implementations bsim4v5/v6/v7/bsim4 use these
                // exact source releases. Do not rely on the native parser's
                // broad version-prefix fallback to silently change releases.
                ensure!(
                    matches!(
                        self.effective_version.as_deref(),
                        Some("4.5" | "4.64" | "4.70" | "4.80")
                    ),
                    "BSIM4 source version {} has no qualified native lowering",
                    self.effective_version.as_deref().unwrap_or_default()
                );
            }
            _ => (),
        }
        Ok(())
    }

    fn validate_diode1(&self) -> Result<()> {
        let mut numeric = Vec::new();
        let mut resolved = true;
        for parameter in &self.card.parameters {
            if parameter.name.eq_ignore_ascii_case("LEVEL") {
                // An explicitly mapped source level is not the kernel's
                // physical selector; the emitted selector is always 1.
                continue;
            }
            ensure!(
                model_kernel_diode::SOURCE_PARAMETERS
                    .iter()
                    .any(|name| name.eq_ignore_ascii_case(&parameter.name)),
                "unsupported SIMetrix level-1 diode parameter {}",
                parameter.name
            );
            let expression = parameter
                .expression
                .as_deref()
                .with_context(|| format!("missing SIMetrix diode parameter {}", parameter.name))?;
            if let Ok(value) = parse_spice_constant(expression) {
                if ["T_ABS", "TNOM", "T_MEASURED"]
                    .iter()
                    .any(|name| parameter.name.eq_ignore_ascii_case(name))
                {
                    ensure!(
                        value > -273.15,
                        "invalid SIMetrix model temperature {value}"
                    );
                }
                numeric.push((parameter.name.as_str(), value));
            } else {
                resolved = false;
            }
        }
        // Validate complete literal declarations here; parameterized models
        // must be compiled only after instance-scoped expression resolution.
        if resolved {
            model_kernel_diode::compile_simetrix(numeric)?;
        }
        Ok(())
    }

    fn append_diode3_defaults(&self, out: &mut String) -> Result<()> {
        // SIMetrix level 3 differs from native defaults: PB=0.8 and PHP=PB.
        // Level 1 retains VJ=1 and is not modified here.
        let potential = self
            .card
            .parameters
            .iter()
            .rev()
            .find(|p| p.name.eq_ignore_ascii_case("VJ") || p.name.eq_ignore_ascii_case("PB"));
        if potential.is_none() {
            out.push_str("VJ=0.8 ");
        }
        if !self
            .card
            .parameters
            .iter()
            .any(|p| p.name.eq_ignore_ascii_case("PHP") || p.name.eq_ignore_ascii_case("VJSW"))
        {
            let value = potential
                .and_then(|p| p.expression.as_deref())
                .unwrap_or("0.8");
            write!(out, "PHP={value} ")?;
        }
        Ok(())
    }
}

impl MappingTable {
    /// Resolve model mapping directives while retaining declaration
    /// provenance. The same table is applied to root and catalog sources. Selection
    /// options remain available to the analysis-plan capture stage.
    ///
    /// # Errors
    /// Invalid model selection, nested scope or unavailable lowering.
    pub fn apply(
        &self,
        statements: &[ScopedStatement],
        budget: &WorkBudget,
    ) -> Result<MappedSource> {
        charge_mapping_scan(statements, budget)?;
        let mut output = Vec::with_capacity(statements.len());
        let mut selections = Vec::new();
        let mut scope = Vec::new();
        let mut opaque = OpaqueSource::default();
        for line in statements {
            let mut line = line.clone_with_budget(budget)?;
            let mut tokens = line.text.split_whitespace();
            let directive = tokens.next().unwrap_or_default().to_ascii_uppercase();
            if !opaque.consume(&line.text) {
                match directive.as_str() {
                    ".SUBCKT" => {
                        budget.check_depth(scope.len().saturating_add(1))?;
                        scope.push(tokens.next().context("missing subcircuit name")?.to_owned());
                    }
                    ".ENDS" => {
                        scope.pop().context("unmatched .ENDS")?;
                    }
                    ".MAP" => line.text = format!("* consumed {}", line.text),
                    ".MODEL" => {
                        if let Some(selection) = self.select_card(&line, &scope, budget)? {
                            line.text = selection.lower().with_context(|| {
                                format!("model {} in {}", selection.card.name, selection.source)
                            })?;
                            budget.charge(Resource::GeneratedNodes, 1)?;
                            selections.push(selection);
                        }
                    }
                    _ => (),
                }
            }
            budget.charge(Resource::EmittedBytes, line.text.len().saturating_add(1))?;
            output.push(line);
        }
        opaque.finish()?;
        ensure!(scope.is_empty(), "unclosed subcircuit in model mapping");
        Ok(MappedSource {
            statements: output,
            selections,
        })
    }
}

/// Catalog members are indexed without simulating them. Global mapping
/// directives must occur in the root's explicitly included source graph.
///
/// # Errors
/// A catalog member contains a simulation-wide selection directive.
pub fn validate_catalog_directives(
    statements: &[ScopedStatement],
    budget: &WorkBudget,
) -> Result<()> {
    charge_mapping_scan(statements, budget)?;
    for (line, _) in directives(statements) {
        budget.check()?;
        let text = line.text.trim_start();
        let (first, tail) = text.split_at(text.find(char::is_whitespace).unwrap_or(text.len()));
        if first.eq_ignore_ascii_case(".MAP") {
            bail!(
                "global .MAP in indexed catalog {}; include the resource in the netlist",
                line.source
            );
        }
        if [".OPTIONS", ".OPTION", ".OPT"]
            .iter()
            .any(|name| first.eq_ignore_ascii_case(name))
        {
            let fields = parse_parameter_fields(tail, &[])?;
            ensure!(
                !fields
                    .iter()
                    .any(|field| field.name.eq_ignore_ascii_case("HSPICEMODELS")
                        || field.name.eq_ignore_ascii_case("HSPICECOMPATIBILITY")),
                "global model-selection option in indexed catalog {}; include the resource in the netlist",
                line.source
            );
        }
    }
    Ok(())
}
