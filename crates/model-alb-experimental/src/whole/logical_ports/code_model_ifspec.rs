//! Verify a real original XSPICE IFS port signature against an A instance
//! inside the verified native external X library, including vector groups.
use super::OpaqueXInterface;
#[path = "code_model_ifspec_parameters.rs"]
mod parameters;
use super::opaque_library::load_native_libraries;
use anyhow::{Context, Result, bail, ensure};
pub use parameters::LookupTableShape;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::{
    env, fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CodeModelContract {
    pub native_model: String,
    pub instance: String,
    pub ifspec: PathBuf,
    /// Independent mapping from published IFS port names to ordered node connections.
    #[serde(deserialize_with = "deserialize_named_ports")]
    pub ports: BTreeMap<String, Vec<String>>,
    /// Original code-model vector relations that the IFS itself cannot express.
    /// Every group is a set of original *vector* port names of equal width.
    pub equal_width_ports: Vec<Vec<String>>,
    /// Independently declared exact widths for explicitly supplied vector parameters.
    pub parameter_vector_widths: BTreeMap<String, String>,
    /// Shape restrictions on model-specific string lookup tables.
    pub table_shapes: Vec<LookupTableShape>,
}

// Serde's normal map collector overwrites exactly duplicated JSON keys before
// the binding verifier can examine them. Preserve strict fail-closed behavior
// for both exact duplicates (here) and case-only duplicates (at verification).
fn deserialize_named_ports<'de, D>(
    deserializer: D,
) -> std::result::Result<BTreeMap<String, Vec<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct UniqueNamedPorts;

    impl<'de> serde::de::Visitor<'de> for UniqueNamedPorts {
        type Value = BTreeMap<String, Vec<String>>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("an object mapping original IFS port names to node vectors")
        }

        fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
        where
            A: serde::de::MapAccess<'de>,
        {
            let mut bindings = BTreeMap::new();
            while let Some((name, nodes)) = map.next_entry::<String, Vec<String>>()? {
                if bindings.insert(name.clone(), nodes).is_some() {
                    return Err(serde::de::Error::custom(format!(
                        "duplicate original IFS JSON port binding {name}"
                    )));
                }
            }
            Ok(bindings)
        }
    }

    deserializer.deserialize_map(UniqueNamedPorts)
}

#[derive(Debug)]
struct IfsPort {
    name: String,
    direction: String,
    kind: String,
    allowed_kinds: BTreeSet<String>,
    vector: bool,
    bounds: (usize, Option<usize>),
    nullable: bool,
}

#[derive(Debug)]
struct Ifs {
    model: String,
    ports: Vec<IfsPort>,
    parameters: Vec<parameters::IfsParameter>,
}

fn columns(s: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut bracket = false;
    let mut quote = None;
    let mut escaped = false;
    for ch in s.chars().chain(std::iter::once(' ')) {
        if escaped {
            word.push(ch);
            escaped = false;
            continue;
        }
        if let Some(q) = quote {
            word.push(ch);
            if ch == '\\' {
                escaped = true;
            } else if ch == q {
                quote = None;
            }
            continue;
        }
        match ch {
            '"' | '\'' if word.is_empty() => {
                quote = Some(ch);
                word.push(ch);
            }
            '[' if !bracket => {
                bracket = true;
                word.push(ch);
            }
            ']' if bracket => {
                bracket = false;
                word.push(ch);
            }
            '[' | ']' => bail!("malformed IFS square brackets: {s}"),
            ch if ch.is_whitespace() && !bracket => {
                if !word.is_empty() {
                    out.push(std::mem::take(&mut word));
                }
            }
            _ => word.push(ch),
        }
    }
    ensure!(!bracket, "unterminated IFS square brackets: {s}");
    ensure!(quote.is_none(), "unterminated IFS quoted field: {s}");
    Ok(out)
}

fn table_field<'a>(
    fields: &'a BTreeMap<String, Vec<String>>,
    name: &str,
    width: usize,
) -> Result<&'a [String]> {
    let values = fields
        .get(name)
        .with_context(|| format!("IFS port table missing {name}"))?;
    ensure!(
        values.len() == width,
        "IFS {name} has {} entries, expected {width}",
        values.len()
    );
    Ok(values)
}

// Original published IFS Port_Table and Parameter_Table use the same
// Vector, Vector_Bounds and Null_Allowed grammar. Keep the rules identical:
// a bare dash on a vector means at least one connection with no upper bound,
// but a scalar must not declare any vector bound.
fn ifs_yes_no(value: &str, subject: &str, field: &str) -> Result<bool> {
    if value.eq_ignore_ascii_case("yes") {
        Ok(true)
    } else if value.eq_ignore_ascii_case("no") {
        Ok(false)
    } else {
        bail!("IFS {subject} has invalid {field}: {value}")
    }
}

fn ifs_vector_shape(
    vector_field: &str,
    bounds_field: &str,
    subject: &str,
) -> Result<(bool, (usize, Option<usize>))> {
    let vector = ifs_yes_no(vector_field, subject, "Vector")?;
    if !vector {
        ensure!(
            bounds_field == "-",
            "IFS scalar {subject} declares vector bounds"
        );
        return Ok((false, (1, Some(1))));
    }
    if bounds_field == "-" {
        return Ok((true, (1, None)));
    }
    let bounds = bounds_field
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .with_context(|| format!("IFS {subject} has malformed vector bounds"))?;
    let parts = bounds.split_whitespace().collect::<Vec<_>>();
    ensure!(
        parts.len() == 2,
        "IFS {subject} requires min/max vector bounds"
    );
    let min = parts[0]
        .parse::<usize>()
        .with_context(|| format!("IFS {subject} invalid minimum vector bound"))?;
    let max = (parts[1] != "-")
        .then(|| parts[1].parse::<usize>())
        .transpose()
        .with_context(|| format!("IFS {subject} invalid maximum vector bound"))?;
    ensure!(
        min > 0 && max.is_none_or(|maximum| maximum >= min),
        "IFS {subject} has invalid vector bounds"
    );
    Ok((true, (min, max)))
}

fn finish_table(fields: &BTreeMap<String, Vec<String>>, ports: &mut Vec<IfsPort>) -> Result<()> {
    if fields.is_empty() {
        return Ok(());
    }
    let names = fields
        .get("port_name")
        .context("IFS port table lacks Port_Name")?;
    let width = names.len();
    ensure!(width > 0, "empty IFS port table");
    let dirs = table_field(fields, "direction", width)?;
    let kinds = table_field(fields, "default_type", width)?;
    let allowed = table_field(fields, "allowed_types", width)?;
    let vectors = table_field(fields, "vector", width)?;
    let limits = table_field(fields, "vector_bounds", width)?;
    let nulls = table_field(fields, "null_allowed", width)?;
    for i in 0..width {
        let name = &names[i];
        let direction = dirs[i].to_ascii_lowercase();
        ensure!(
            matches!(direction.as_str(), "in" | "out" | "inout"),
            "IFS {name} has invalid direction"
        );
        let kind = kinds[i].to_ascii_lowercase();
        let choices = allowed[i]
            .strip_prefix('[')
            .and_then(|s| s.strip_suffix(']'))
            .with_context(|| format!("IFS {name} has malformed Allowed_Types"))?;
        let allowed_kinds = choices
            .split(|ch: char| ch.is_whitespace() || ch == ',')
            .filter(|choice| !choice.is_empty())
            .map(str::to_ascii_lowercase)
            .collect::<BTreeSet<_>>();
        ensure!(
            allowed_kinds.contains(&kind),
            "IFS {name} default {kind} not in allowed types {choices}"
        );
        let (vector, bounds) = ifs_vector_shape(&vectors[i], &limits[i], name)?;
        let nullable = ifs_yes_no(&nulls[i], name, "Null_Allowed")?;
        ensure!(
            !ports
                .iter()
                .any(|p: &IfsPort| p.name.eq_ignore_ascii_case(name)),
            "IFS port {name} declared more than once"
        );
        ports.push(IfsPort {
            name: name.clone(),
            direction,
            kind,
            allowed_kinds,
            vector,
            bounds,
            nullable,
        });
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum IfsSection {
    Other,
    Ports,
    Parameters,
}

fn parse_ifspec(text: &str) -> Result<Ifs> {
    let mut model = None;
    let mut ports = Vec::new();
    let mut parameters = Vec::new();
    let mut table = BTreeMap::new();
    let mut section = IfsSection::Other;
    for line in text.lines() {
        let line = line.trim();
        if line.to_ascii_uppercase().ends_with("_TABLE:") {
            match section {
                IfsSection::Other => {}
                IfsSection::Ports => finish_table(&table, &mut ports)?,
                IfsSection::Parameters => {
                    parameters::append_parameter_table(&table, &mut parameters)?;
                }
            }
            table.clear();
            section = if line.eq_ignore_ascii_case("PORT_TABLE:") {
                IfsSection::Ports
            } else if line.eq_ignore_ascii_case("PARAMETER_TABLE:") {
                IfsSection::Parameters
            } else {
                IfsSection::Other
            };
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        if key == "spice_model_name" {
            ensure!(model.is_none(), "IFS Spice_Model_Name repeated");
            let names = columns(value)?;
            ensure!(names.len() == 1, "IFS Spice_Model_Name needs one value");
            model = names.into_iter().next();
        } else if !matches!(section, IfsSection::Other) {
            ensure!(
                table.insert(key.clone(), columns(value)?).is_none(),
                "IFS repeats table field {key}"
            );
        }
    }
    match section {
        IfsSection::Other => {}
        IfsSection::Ports => finish_table(&table, &mut ports)?,
        IfsSection::Parameters => parameters::append_parameter_table(&table, &mut parameters)?,
    }
    ensure!(!ports.is_empty(), "IFS does not define any ports");
    Ok(Ifs {
        model: model.context("IFS missing Spice_Model_Name")?,
        ports,
        parameters,
    })
}

#[derive(Debug, PartialEq)]
enum Connection {
    Scalar(String),
    Vector(Vec<String>),
}

fn a_fields(statement: &str) -> Result<Vec<Connection>> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut vector: Option<Vec<String>> = None;
    let mut selector_parens = 0_usize;
    for ch in statement.chars().chain(std::iter::once(' ')) {
        if ch == ';' && vector.is_none() && selector_parens == 0 {
            break;
        }
        match ch {
            '(' if word.starts_with('%') || selector_parens > 0 => {
                selector_parens += 1;
                word.push(ch);
            }
            ')' if selector_parens > 0 => {
                selector_parens -= 1;
                word.push(ch);
            }
            '(' | ')' => bail!("malformed A-instance typed selector: {statement}"),
            '[' if vector.is_none() => {
                ensure!(word.is_empty(), "A instance vector starts inside scalar");
                vector = Some(Vec::new());
            }
            ']' if vector.is_some() => {
                if !word.is_empty() {
                    vector
                        .as_mut()
                        .context("missing open vector")?
                        .push(std::mem::take(&mut word));
                }
                let v = vector.take().context("missing A vector")?;
                ensure!(!v.is_empty(), "A instance vector has zero nodes");
                out.push(Connection::Vector(v));
            }
            '[' | ']' => bail!("malformed A-instance bracket structure"),
            ch if ch.is_whitespace() && selector_parens > 0 => word.push(ch),
            ch if ch.is_whitespace() => {
                if !word.is_empty() {
                    if let Some(v) = &mut vector {
                        v.push(std::mem::take(&mut word));
                    } else {
                        out.push(Connection::Scalar(std::mem::take(&mut word)));
                    }
                }
            }
            _ => word.push(ch),
        }
    }
    ensure!(vector.is_none(), "unterminated A-instance vector");
    ensure!(
        selector_parens == 0,
        "unterminated A-instance typed selector"
    );
    Ok(out)
}

/// SPICE selectors such as %vd(VP VN) select a non-default port type.
/// Preserve the full selector (including both differential terminals) as one
/// node in the independently named port binding witness.
fn selected_port_kind<'a>(node: &'a str, original: &'a IfsPort) -> Result<&'a str> {
    let Some(selected) = node.strip_prefix('%') else {
        return Ok(&original.kind);
    };
    let (kind, rest) = selected
        .split_once('(')
        .context("malformed original XSPICE typed port selector")?;
    let terminals = rest
        .strip_suffix(')')
        .context("unclosed original XSPICE typed port selector")?;
    ensure!(
        !kind.is_empty() && !terminals.trim().is_empty(),
        "empty original XSPICE typed selector"
    );
    ensure!(
        !terminals.contains(['(', ')', '[', ']', '%']),
        "nested or malformed original XSPICE typed selector"
    );
    Ok(kind)
}

fn verify_port_types(original: &IfsPort, nodes: &[&str]) -> Result<()> {
    let mut chosen = None;
    for node in nodes {
        let kind = selected_port_kind(node, original)?;
        ensure!(
            original.allowed_kinds.contains(&kind.to_ascii_lowercase()),
            "IFS {} port {} does not permit selected type {kind}",
            original.kind,
            original.name
        );
        ensure!(
            chosen.is_none_or(|other: &str| other.eq_ignore_ascii_case(kind)),
            "IFS {} vector mixes XSPICE port types",
            original.name
        );
        chosen = Some(kind);
    }
    Ok(())
}

fn verify_equal_width_ports(
    ifs: &Ifs,
    equal_width_ports: &[Vec<String>],
    bindings: &BTreeMap<String, &Vec<String>>,
) -> Result<()> {
    // The public IFS only expresses independent vector bounds. Some original
    // code-model implementations impose additional relationships: Georgia
    // Tech d_ram obtains the word width from data_in and indexes data_out
    // with that same width. A different data_out length is memory-unsafe for
    // the original C model even though each vector separately satisfies IFS.
    // These extra constraints must be independently declared in the contract.
    for group in equal_width_ports {
        ensure!(
            group.len() >= 2,
            "IFS equal-width constraint requires at least two named vector ports"
        );
        let mut names = BTreeSet::new();
        let mut width = None;
        for name in group {
            let key = name.to_ascii_lowercase();
            ensure!(
                names.insert(key.clone()),
                "IFS equal-width constraint repeats port {name}"
            );
            let original = ifs
                .ports
                .iter()
                .find(|port| port.name.eq_ignore_ascii_case(name))
                .with_context(|| format!("IFS equal-width constraint names unknown port {name}"))?;
            ensure!(
                original.vector,
                "IFS equal-width constraint names scalar port {name}"
            );
            let count = bindings
                .get(&key)
                .with_context(|| format!("IFS equal-width constraint omits port {name}"))?
                .len();
            ensure!(
                width.is_none_or(|previous| previous == count),
                "IFS equal-width constraint for {group:?} is violated at port {name}: {count} vs {width:?}"
            );
            width = Some(count);
        }
    }
    Ok(())
}

fn verify_bindings(ifs: &Ifs, spec: &CodeModelContract, statement: &str) -> Result<String> {
    // Match by the original IFS port names, never by a duplicated user-defined
    // order. Keep the binding witness independent of native A-instance wiring.
    let mut bindings = BTreeMap::new();
    for (name, nodes) in &spec.ports {
        ensure!(
            bindings.insert(name.to_ascii_lowercase(), nodes).is_none(),
            "original IFS binding {name} appears more than once (case-insensitive)"
        );
    }
    ensure!(
        ifs.ports.len() == bindings.len(),
        "IFS {} defines {} ports, code-model contract supplies {}",
        ifs.model,
        ifs.ports.len(),
        bindings.len()
    );
    let actual = a_fields(statement)?;
    ensure!(
        actual.len() == ifs.ports.len() + 2,
        "original XSPICE A instance {} has {} port groups, original IFS {} requires {}",
        spec.instance,
        actual.len().saturating_sub(2),
        ifs.model,
        ifs.ports.len()
    );
    ensure!(
        matches!(&actual[0], Connection::Scalar(name) if name.eq_ignore_ascii_case(&spec.instance)),
        "wrong XSPICE A instance name"
    );
    for (i, original) in ifs.ports.iter().enumerate() {
        let expected = bindings
            .get(&original.name.to_ascii_lowercase())
            .with_context(|| {
                format!(
                    "original IFS {} port {} lacks a named binding",
                    ifs.model, original.name
                )
            })?;
        let nodes = match &actual[i + 1] {
            Connection::Scalar(s) if s.eq_ignore_ascii_case("null") => {
                ensure!(
                    original.nullable,
                    "IFS {} port {} cannot be NULL",
                    ifs.model,
                    original.name
                );
                ensure!(
                    expected.len() == 1 && expected[0].eq_ignore_ascii_case("null"),
                    "IFS {} port {} unexpected NULL binding",
                    ifs.model,
                    original.name
                );
                continue;
            }
            Connection::Scalar(s) => {
                ensure!(
                    !original.vector,
                    "IFS {} port {} requires vector group",
                    ifs.model,
                    original.name
                );
                vec![s.as_str()]
            }
            Connection::Vector(v) => {
                ensure!(
                    original.vector,
                    "IFS {} port {} requires scalar, got vector",
                    ifs.model,
                    original.name
                );
                v.iter().map(String::as_str).collect()
            }
        };
        verify_port_types(original, &nodes)?;
        ensure!(
            nodes.len() >= original.bounds.0
                && original.bounds.1.is_none_or(|max| nodes.len() <= max),
            "IFS {} port {} vector width {} outside {:?}",
            ifs.model,
            original.name,
            nodes.len(),
            original.bounds
        );
        ensure!(
            nodes.len() == expected.len()
                && nodes
                    .iter()
                    .zip(expected.iter())
                    .all(|(n, want)| n.eq_ignore_ascii_case(want)),
            "IFS {} port {} ({}/{}) miswired: {nodes:?}, expected {expected:?}",
            ifs.model,
            original.name,
            original.direction,
            original.kind
        );
    }
    verify_equal_width_ports(ifs, &spec.equal_width_ports, &bindings)?;
    match actual.last().context("missing A instance model label")? {
        Connection::Scalar(name) => Ok(name.clone()),
        Connection::Vector(_) => bail!("A-instance model label is a vector"),
    }
}

/// Every original XSPICE code-model port agrees on the event data type of
/// any *shared bare node*. A named real event cannot be consumed as a digital
/// event (or an analog electrical node) merely because the independently
/// authored JSON witness was changed to agree with the mistaken connection.
///
/// Explicit percent selectors are connection expressions, not bare event-net
/// names (e.g. %vd(PLUS MINUS) is a two-terminal analog measurement).
fn check_shared_original_event_types(
    ifs: &Ifs,
    spec: &CodeModelContract,
    domains: &mut BTreeMap<(String, String), (String, String)>,
) -> Result<()> {
    for port in &ifs.ports {
        let bindings = spec
            .ports
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(&port.name))
            .with_context(|| format!("missing original IFS port {}", port.name))?;
        // XSPICE's 'd' and 'real' are event node classes; analog port
        // variants share the ordinary SPICE electrical-node class.
        let domain = match port.kind.as_str() {
            "d" | "real" | "int" | "boolean" | "string" => port.kind.as_str(),
            _ => "analog",
        };
        for node in bindings.1 {
            if node.eq_ignore_ascii_case("null") || node.starts_with('%') {
                continue;
            }
            let key = (
                spec.native_model.to_ascii_uppercase(),
                node.to_ascii_uppercase(),
            );
            if let Some((prior_kind, origin)) = domains.get(&key) {
                ensure!(
                    prior_kind == domain,
                    "original XSPICE shared node {node} has conflicting IFS types {prior_kind} ({origin}) versus {domain} ({}/{})",
                    spec.instance,
                    port.name
                );
            } else {
                domains.insert(
                    key,
                    (domain.into(), format!("{}/{}", spec.instance, port.name)),
                );
            }
        }
    }
    Ok(())
}

fn resolve_ifspec(original_path: &Path, filename: &Path) -> Result<PathBuf> {
    if filename.is_absolute() {
        return Ok(filename.to_path_buf());
    }
    let root = env::var_os("XSPICE_IFSPEC_ROOT")
        .map(PathBuf::from)
        .or_else(|| original_path.parent().map(Path::to_path_buf))
        .context("original XSPICE IFS needs source directory or XSPICE_IFSPEC_ROOT")?;
    Ok(root.join(filename))
}

/// Verify an original code-model IFS port contract against the actual A
/// instance and .model declaration within a real reachable native .subckt.
///
/// # Errors
/// Missing source or IFS, malformed tables and bounds, missing or mislabeled
/// instances, wrong model kinds, vector widths, event-port order or bindings.
pub fn verify_external_code_models(
    original_path: &Path,
    source: &str,
    contracts: &[CodeModelContract],
    interfaces: &[OpaqueXInterface],
) -> Result<()> {
    if contracts.is_empty() {
        return Ok(());
    }
    let native_models = interfaces
        .iter()
        .filter_map(|i| i.native_model.as_ref())
        .map(|n| n.to_ascii_uppercase())
        .collect::<BTreeSet<_>>();
    let wanted = contracts
        .iter()
        .map(|c| c.native_model.to_ascii_uppercase())
        .collect::<BTreeSet<_>>();
    for name in &wanted {
        ensure!(
            native_models.contains(name),
            "IFS contract names undeclared native model {name}"
        );
    }
    let native = load_native_libraries(original_path, source, &wanted)?;
    let mut keys = BTreeSet::new();
    let mut shared_domains = BTreeMap::new();
    for spec in contracts {
        let model_name = spec.native_model.to_ascii_uppercase();
        ensure!(
            keys.insert((model_name.clone(), spec.instance.to_ascii_uppercase())),
            "duplicate original code-model IFS binding to {model_name}/{}",
            spec.instance
        );
        let wrapper = native.get(&model_name).with_context(|| {
            format!("IFS native wrapper {model_name} missing from reachable library")
        })?;
        let ifs_path = resolve_ifspec(original_path, &spec.ifspec)?;
        let ifs = parse_ifspec(
            &fs::read_to_string(&ifs_path)
                .with_context(|| format!("reading original XSPICE IFS {}", ifs_path.display()))?,
        )
        .with_context(|| format!("parsing original XSPICE IFS {}", ifs_path.display()))?;
        let mut labels = BTreeMap::new();
        let mut instance = None;
        for stmt in &wrapper.statements {
            let words = stmt.split_ascii_whitespace().collect::<Vec<_>>();
            if words.is_empty() {
                continue;
            }
            if words[0].eq_ignore_ascii_case(".model") {
                ensure!(
                    words.len() >= 3,
                    "malformed .model in {}",
                    wrapper.origin.display()
                );
                let kind = words[2].split('(').next().context("missing .model kind")?;
                ensure!(
                    labels
                        .insert(
                            words[1].to_ascii_uppercase(),
                            (kind.to_ascii_uppercase(), stmt.as_str())
                        )
                        .is_none(),
                    "duplicate native .model label {}",
                    words[1]
                );
            } else if words[0].eq_ignore_ascii_case(&spec.instance) {
                ensure!(
                    instance.replace(stmt.as_str()).is_none(),
                    "duplicate native code-model A instance {}",
                    spec.instance
                );
            }
        }
        let stmt = instance.with_context(|| {
            format!(
                "original code-model A instance {} missing from native {model_name}",
                spec.instance
            )
        })?;
        let label = verify_bindings(&ifs, spec, stmt)?;
        let (kind, model_stmt) = labels.get(&label.to_ascii_uppercase()).with_context(|| {
            format!(
                "original A instance {} uses undefined .model {label}",
                spec.instance
            )
        })?;
        ensure!(
            kind.eq_ignore_ascii_case(&ifs.model),
            "original IFS {} vs native A instance {} wrong code-model kind {kind}",
            ifs.model,
            spec.instance
        );
        parameters::verify_parameters(&ifs, spec, model_stmt)?;
        check_shared_original_event_types(&ifs, spec, &mut shared_domains)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::whole::OpaqueXPinKind::Analog;

    // The directory is pinned at compile time: family and variant stay
    // explicit even when all three original native model orders are checked.
    macro_rules! original_model_variant {
        ($family:literal, $variant:literal) => {
            (
                $variant,
                include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../runtime/xspice/fixtures/family/auto-discovery-original-",
                    $family,
                    "-",
                    $variant,
                    "-x.json"
                )),
                include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../runtime/xspice/fixtures/family/native-original-",
                    $family,
                    "-",
                    $variant,
                    ".lib"
                )),
            )
        };
    }

    const ORIGINAL_D_SOURCE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../runtime/xspice/fixtures/family/georgiatech-d_source-original.ifs"
    ));
    const ORIGINAL_REAL_GAIN: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../runtime/xspice/fixtures/family/original-xtraevt-real_gain.ifs"
    ));
    const ORIGINAL_D_TO_REAL: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../runtime/xspice/fixtures/family/original-xtraevt-d_to_real.ifs"
    ));
    const ORIGINAL_REAL_DELAY: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../runtime/xspice/fixtures/family/original-xtraevt-real_delay.ifs"
    ));
    const ORIGINAL_REAL_TO_V: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../runtime/xspice/fixtures/family/original-xtraevt-real_to_v.ifs"
    ));
    const DLATCH: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../runtime/xspice/fixtures/family/georgiatech-dlatch-original.ifs"
    ));
    const RAM: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../runtime/xspice/fixtures/family/georgiatech-ram-original.ifs"
    ));
    const GENLUT: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../runtime/xspice/fixtures/family/tim-edwards-d_genlut-original.ifs"
    ));
    const ORIGINAL_ADC: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../runtime/xspice/fixtures/family/georgiatech-adc_bridge-original.ifs"
    ));
    const ORIGINAL_DAC: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../runtime/xspice/fixtures/family/georgiatech-dac_bridge-original.ifs"
    ));
    const AND: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../runtime/xspice/fixtures/family/georgiatech-d_and-original.ifs"
    ));
    const XOR: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../runtime/xspice/fixtures/family/georgiatech-d_xor-original.ifs"
    ));

    fn links() -> Vec<OpaqueXInterface> {
        vec![OpaqueXInterface {
            model: "ORIGINAL".into(),
            pins: vec![Analog],
            native_model: Some("NATIVE".into()),
            native_formals: vec!["IN".into()],
        }]
    }

    // Share only the native test environment; independent IFS witnesses,
    // original model semantics and negative mutations remain case-specific.
    struct OriginalEventModelFixture {
        temp: tempfile::TempDir,
        deck: PathBuf,
        contracts: Vec<CodeModelContract>,
        interfaces: Vec<OpaqueXInterface>,
    }

    impl OriginalEventModelFixture {
        const SOURCE: &'static str = ".include \"native.lib\"\n";

        fn new(config: &str, native: &str) -> Result<Self> {
            let config: serde_json::Value = serde_json::from_str(config)?;
            let contracts = serde_json::from_value(config["code_model_contracts"].clone())?;
            let interfaces = serde_json::from_value(config["opaque_x_interfaces"].clone())?;
            let temp = tempfile::tempdir()?;
            let deck = temp.path().join("deck.cir");
            fs::write(&deck, Self::SOURCE)?;
            fs::write(temp.path().join("native.lib"), native)?;
            for (name, contents) in [
                ("georgiatech-d_source-original.ifs", ORIGINAL_D_SOURCE),
                ("original-xtraevt-d_to_real.ifs", ORIGINAL_D_TO_REAL),
                ("original-xtraevt-real_gain.ifs", ORIGINAL_REAL_GAIN),
                ("original-xtraevt-real_to_v.ifs", ORIGINAL_REAL_TO_V),
            ] {
                fs::write(temp.path().join(name), contents)?;
            }
            Ok(Self {
                temp,
                deck,
                contracts,
                interfaces,
            })
        }

        fn verify(&self, contracts: &[CodeModelContract]) -> Result<()> {
            verify_external_code_models(&self.deck, Self::SOURCE, contracts, &self.interfaces)
        }

        fn verify_original(&self) -> Result<()> {
            self.verify(&self.contracts)
        }

        fn replace_native(&self, native: &str) -> Result<()> {
            fs::write(self.deck.with_file_name("native.lib"), native)?;
            Ok(())
        }
    }

    fn ram_contract() -> CodeModelContract {
        CodeModelContract {
            native_model: "NATIVE".into(),
            instance: "Aram".into(),
            ifspec: PathBuf::from("ram.ifs"),
            ports: [
                ("data_in", vec!["D0", "D1"]),
                ("data_out", vec!["Q0", "Q1"]),
                ("address", vec!["A0", "A1"]),
                ("write_en", vec!["WE"]),
                ("select", vec!["S0", "S1"]),
            ]
            .into_iter()
            .map(|(name, nodes)| (name.into(), nodes.into_iter().map(str::to_owned).collect()))
            .collect(),
            equal_width_ports: vec![vec!["data_in".into(), "data_out".into()]],
            parameter_vector_widths: BTreeMap::new(),
            table_shapes: Vec::new(),
        }
    }

    fn check_native(original: &str, contract: &CodeModelContract) -> Result<()> {
        let temp = tempfile::tempdir()?;
        let deck = temp.path().join("deck.cir");
        let source = ".include \"native.lib\"\n";
        fs::write(&deck, source)?;
        fs::write(temp.path().join("native.lib"), original)?;
        fs::write(temp.path().join("ram.ifs"), RAM)?;
        verify_external_code_models(&deck, source, std::slice::from_ref(contract), &links())
    }

    fn ram_library() -> &'static str {
        ".subckt NATIVE IN\nAram [D0 D1] [Q0 Q1] [A0 A1] WE [S0 S1] CM\n.model CM d_ram(select_value=1)\n.ends NATIVE\n"
    }

    #[test]
    fn original_multi_driver_digital_resolver_pinned_truth_table() -> Result<()> {
        const PAIR: &str = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../runtime/xspice/fixtures/family/original-contention-pair-expected.txt"
        ));
        const TRIPLE: &str = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../runtime/xspice/fixtures/family/original-contention-triple-expected.txt"
        ));
        let alphabet = [
            "0s", "1s", "Us", "0r", "1r", "Ur", "0z", "1z", "Uz", "0u", "1u", "Uu",
        ];
        let pair_rows = PAIR
            .lines()
            .filter(|line| !line.starts_with('*') && !line.trim().is_empty())
            .map(|line| line.split_whitespace().collect::<Vec<_>>())
            .collect::<Vec<_>>();
        assert_eq!(pair_rows.len(), 144);
        for (index, row) in pair_rows.iter().enumerate() {
            assert_eq!(row.len(), 3);
            assert_eq!(row[0], alphabet[index / 12]);
            assert_eq!(row[1], alphabet[index % 12]);
            assert!(alphabet.contains(&row[2]));
            assert_eq!(
                row[2],
                pair_rows[(index % 12) * 12 + (index / 12)][2],
                "original 12×12 digital resolver must be commutative"
            );
        }

        let triple_rows = TRIPLE
            .lines()
            .filter(|line| !line.starts_with('*') && !line.trim().is_empty())
            .map(|line| line.split_whitespace().collect::<Vec<_>>())
            .collect::<Vec<_>>();
        assert_eq!(triple_rows.len(), 48);
        // Enumerate all 12³ possible ordered combinations, not only the
        // 48 supplied witnesses. This guarantees that the pinned table is
        // *complete* and that reordering all three original driver slots
        // changes exactly this set of original C model outcomes.
        let mut nonassociative = 0;
        for (a, drive_a) in alphabet.iter().enumerate() {
            for (b, drive_b) in alphabet.iter().enumerate() {
                for (c, drive_c) in alphabet.iter().enumerate() {
                    let ab = pair_rows[a * 12 + b][2];
                    let bc = pair_rows[b * 12 + c][2];
                    let ab_idx = alphabet
                        .iter()
                        .position(|value| *value == ab)
                        .context("original pair resolver returned a missing state")?;
                    let bc_idx = alphabet
                        .iter()
                        .position(|value| *value == bc)
                        .context("original pair resolver returned a missing state")?;
                    let left = pair_rows[ab_idx * 12 + c][2];
                    let right = pair_rows[a * 12 + bc_idx][2];
                    if left != right {
                        let row = &triple_rows[nonassociative];
                        assert_eq!(row.as_slice(), [*drive_a, *drive_b, *drive_c, left, right]);
                        nonassociative += 1;
                    }
                }
            }
        }
        assert_eq!(nonassociative, 48);
        for row in &triple_rows {
            assert_eq!(row.len(), 5);
            assert!(row.iter().all(|code| alphabet.contains(code)));
            assert_ne!(row[3], row[4], "every selected triple is nonassociative");
            let idx = |value: &str| {
                alphabet
                    .iter()
                    .position(|candidate| *candidate == value)
                    .with_context(|| format!("missing original 12-state code {value}"))
            };
            let (a, b, c) = (idx(row[0])?, idx(row[1])?, idx(row[2])?);
            let left = pair_rows[a * 12 + b][2];
            let right = pair_rows[b * 12 + c][2];
            assert_eq!(row[3], pair_rows[idx(left)? * 12 + c][2]);
            assert_eq!(row[4], pair_rows[a * 12 + idx(right)?][2]);
        }

        Ok(())
    }

    fn verify_original_contention_wrapper(variant: &str, config: &str, native: &str) -> Result<()> {
        let fixture = OriginalEventModelFixture::new(config, native)?;
        let contracts = &fixture.contracts;
        let source_count = if variant == "pair" { 2 } else { 3 };
        assert_eq!(contracts.len(), source_count + 6);
        assert_eq!(
            contracts
                .iter()
                .filter(|contract| {
                    contract.ifspec.as_path() == Path::new("georgiatech-d_source-original.ifs")
                })
                .count(),
            source_count,
        );
        fixture.verify_original()?;

        let pos0 = native.find("Asource0 [QEV]").context("missing source A0")?;
        let pos1 = native.find("Asource1 [QEV]").context("missing source A1")?;
        if variant == "triple-reordered" {
            let pos2 = native.find("Asource2 [QEV]").context("missing source A2")?;
            assert!(pos1 < pos2 && pos2 < pos0);
        } else {
            assert!(pos0 < pos1);
            if variant == "triple" {
                let pos2 = native.find("Asource2 [QEV]").context("missing source A2")?;
                assert!(pos1 < pos2);
            }
        }

        let wrong_source = native.replace(
            "Asource0 [QEV] ORIGINAL_SOURCE0",
            "Asource0 QEV ORIGINAL_SOURCE0",
        );
        fixture.replace_native(&wrong_source)?;
        assert!(
            fixture.verify_original().is_err(),
            "original source digital vector cannot be silently used as scalar"
        );
        let mut retyped = contracts.clone();
        fixture
            .replace_native(&native.replace("Ad2real QEV NULL RRAW", "Ad2real RRAW NULL RRAW"))?;
        retyped
            .iter_mut()
            .find(|contract| contract.instance == "Ad2real")
            .context("original digital-to-real contract not found")?
            .ports
            .insert("in".into(), vec!["RRAW".into()]);
        assert!(
            fixture.verify(&retyped).is_err(),
            "changing the witness cannot reinterpret a real node as digital"
        );
        Ok(())
    }

    #[test]
    fn original_multi_driver_native_ifs_source_contracts() -> Result<()> {
        for (variant, config, native) in [
            original_model_variant!("contention", "pair"),
            original_model_variant!("contention", "triple"),
            original_model_variant!("contention", "triple-reordered"),
        ] {
            verify_original_contention_wrapper(variant, config, native)?;
        }
        Ok(())
    }

    #[test]
    fn original_multi_driver_real_event_source_cycles() {
        const ALPHABET: [&str; 12] = [
            "0s", "1s", "Us", "0r", "1r", "Ur", "0z", "1z", "Uz", "0u", "1u", "Uu",
        ];
        for (source, multiple, add) in [
            (
                include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../runtime/xspice/fixtures/family/original-real-sum-driver-0.txt"
                )),
                1,
                0,
            ),
            (
                include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../runtime/xspice/fixtures/family/original-real-sum-driver-1.txt"
                )),
                5,
                4,
            ),
            (
                include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../runtime/xspice/fixtures/family/original-real-sum-driver-2.txt"
                )),
                7,
                3,
            ),
        ] {
            let rows = source
                .lines()
                .filter(|line| !line.starts_with('*') && !line.is_empty())
                .collect::<Vec<_>>();
            assert_eq!(rows.len(), 24);
            for (i, row) in rows.iter().enumerate() {
                let fields = row.split_ascii_whitespace().collect::<Vec<_>>();
                assert_eq!(fields.len(), 2);
                assert_eq!(fields[0], format!("{}n", 12 * i));
                assert_eq!(fields[1], ALPHABET[(multiple * i + add) % 12]);
            }
        }
        // Every C-model real value here is a binary-exact multiple of
        // 1/16. Their original UDN real sums have a stable exact value
        // under all three driver declarations, unlike digital UDN.
        for phase in 0..24 {
            let raw = |source: usize| {
                let index = match source {
                    0 => phase % 12,
                    1 => (5 * phase + 4) % 12,
                    _ => (7 * phase + 3) % 12,
                };
                ALPHABET[index].chars().next()
            };
            let a = match raw(0) {
                Some('0') => -8_i32,
                Some('1') => 24,
                _ => 8,
            };
            let b = match raw(1) {
                Some('0') => -29_i32,
                Some('1') => 19,
                _ => -5,
            };
            let c = match raw(2) {
                Some('0') => 2_i32,
                Some('1') => 30,
                _ => 16,
            };
            assert_eq!((a + b) + c, (b + c) + a);
        }
    }

    fn verify_original_real_udn_wrapper(variant: &str, config: &str, native: &str) -> Result<()> {
        let fixture = OriginalEventModelFixture::new(config, native)?;
        let contracts = &fixture.contracts;
        let three = variant != "pair";
        assert_eq!(contracts.len(), if three { 12 } else { 10 });
        assert_eq!(
            contracts
                .iter()
                .filter(|contract| {
                    contract.ifspec.as_path() == Path::new("original-xtraevt-d_to_real.ifs")
                })
                .count(),
            if three { 4 } else { 3 }
        );
        let a = native
            .find("ArealA Q0 NULL REAL_SUM")
            .context("missing first real driver")?;
        let b = native
            .find("AgainB BPRE REAL_SUM")
            .context("missing real_gain driver")?;
        if three {
            let c = native
                .find("ArealC Q2 NULL REAL_SUM")
                .context("missing third real driver")?;
            if variant == "triple-reordered" {
                assert!(b < c && c < a);
            } else if variant == "cancellation-grouped" {
                assert!(a < c && c < b);
            } else {
                assert!(a < b && b < c);
            }
        } else {
            assert!(a < b);
        }
        fixture.verify_original()?;

        // Even if the external independent witness is altered, a digital
        // event cannot be substituted for the typed original REAL output.
        let mut corrupted_contracts = contracts.clone();
        fixture.replace_native(&native.replace("ArealA Q0 NULL REAL_SUM", "ArealA Q0 NULL Q0"))?;
        corrupted_contracts
            .iter_mut()
            .find(|contract| contract.instance == "ArealA")
            .context("missing original d_to_real output contract")?
            .ports
            .insert("out".into(), vec!["Q0".into()]);
        assert!(
            fixture.verify(&corrupted_contracts).is_err(),
            "original d_source output is digital but d_to_real output is REAL"
        );
        Ok(())
    }

    #[test]
    fn original_multi_driver_real_udn_ifs_and_order_contracts() -> Result<()> {
        for (variant, config, native) in [
            original_model_variant!("real-sum", "pair"),
            original_model_variant!("real-sum", "triple"),
            original_model_variant!("real-sum", "triple-reordered"),
        ] {
            verify_original_real_udn_wrapper(variant, config, native)?;
        }
        Ok(())
    }

    #[test]
    fn original_multi_driver_real_udn_ieee754_cancellation_contracts() -> Result<()> {
        const CONFIG: &str = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../runtime/xspice/fixtures/family/auto-discovery-original-real-sum-triple-x.json"
        ));
        let left = (1e16_f64 + 1.0) - 1e16;
        let grouped = (1e16_f64 - 1e16) + 1.0;
        assert!(left.abs() < f64::EPSILON);
        assert!((grouped - 1.0).abs() < f64::EPSILON);
        assert!((left - grouped).abs() > 0.5);
        for (variant, native) in [
            (
                "cancellation-left",
                include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../runtime/xspice/fixtures/family/native-original-real-sum-cancellation-left.lib"
                )),
            ),
            (
                "cancellation-grouped",
                include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../runtime/xspice/fixtures/family/native-original-real-sum-cancellation-grouped.lib"
                )),
            ),
        ] {
            for model in [
                "ORIGINAL_DA d_to_real(zero=1e16 one=1e16 delay=200p)",
                "ORIGINAL_DC d_to_real(zero=-1e16 one=-1e16 delay=700p)",
                "ORIGINAL_GAIN_B real_gain(in_offset=0.125 gain=0 out_offset=1 delay=240p ic=1)",
            ] {
                assert!(
                    native.contains(model),
                    "original real C contribution changed"
                );
            }
            verify_original_real_udn_wrapper(variant, CONFIG, native)?;
        }
        Ok(())
    }

    #[test]
    fn original_real_udn_nonfinite_arithmetic_and_midpoint_reproduction() {
        assert!((8e307_f64 + 1e308).is_infinite());
        assert!(((8e307_f64 - 8e307) + 1e308).is_finite());
        assert!((8e307_f64 + 1e308 + f64::NEG_INFINITY).is_nan());
        assert!(((8e307_f64 + f64::NEG_INFINITY) + 1e308).is_infinite());
        assert!(((8e307_f64 + f64::NEG_INFINITY) + 1e308).is_sign_negative());

        // Original ngspice47 d_to_real C computes UNKNOWN as the sum
        // of zero and one, then divides by two, overflowing for 1e308.
        let zero = std::hint::black_box(1e308_f64);
        let one = std::hint::black_box(1e308_f64);
        let original_sum = zero + one;
        let original_midpoint = original_sum / 2.0;
        assert!(original_midpoint.is_infinite());
        assert!(zero.is_finite() && one.is_finite());
        let corrected_midpoint = f64::midpoint(zero, one);
        assert!(corrected_midpoint.is_finite());
        assert_eq!(corrected_midpoint, zero);
    }

    fn verify_original_real_nonfinite_driver_order(variant: &str, native: &str) -> Result<()> {
        assert!(native.contains("AgainNonfinite REAL_SUM R_META ORIGINAL_META"));
        assert!(native.contains("AgainPost BPRE RPOST ORIGINAL_GAIN_POST"));
        assert!(native.contains("Areadout BPRE ANALOG_SUM ORIGINAL_R2V"));
        assert!(native.contains("ORIGINAL_DA d_to_real(zero=8e307 one=8e307"));
        assert!(native.contains("ORIGINAL_GAIN_B real_gain(in_offset=1e308 gain=1"));

        let a = native
            .find("ArealA Q0 NULL REAL_SUM")
            .context("missing original real UDN A driver")?;
        let b = native
            .find("AgainB BPRE REAL_SUM")
            .context("missing original real UDN B driver")?;
        if variant.starts_with("nan-") {
            assert!(native.contains("ORIGINAL_GAIN_C real_gain(in_offset=0 gain=-2"));
            let c = native
                .find("AgainC CPRE REAL_SUM")
                .context("missing original -Infinity C driver")?;
            if variant == "nan-middle" {
                assert!(a < c && c < b);
            } else {
                assert!(a < b && b < c);
            }
        } else {
            assert!(native.contains("ORIGINAL_DC d_to_real(zero=-8e307 one=-8e307"));
            let c = native
                .find("ArealC Q2 NULL REAL_SUM")
                .context("missing original REAL finite C driver")?;
            if variant == "overflow-middle" {
                assert!(a < c && c < b);
            } else {
                assert!(a < b && b < c);
            }
        }
        if variant == "midpoint" {
            assert!(native.contains("Amidpoint Q0 NULL R_MIDPOINT ORIGINAL_MIDPOINT"));
            assert!(native.contains("AmidpointGated Q0 EN R_MIDPOINT_GATED ORIGINAL_MIDPOINT"));
            assert!(
                native.contains("ORIGINAL_MIDPOINT d_to_real(zero=1e308 one=1e308 delay=180p)")
            );
        }
        Ok(())
    }

    fn verify_original_real_nonfinite_ifs(variant: &str, config: &str, native: &str) -> Result<()> {
        let fixture = OriginalEventModelFixture::new(config, native)?;
        let contracts = fixture.contracts.clone();
        let interfaces = fixture.interfaces.clone();
        let expected_contracts = if variant == "midpoint" {
            15
        } else if variant.starts_with("nan-") {
            14
        } else {
            13
        };
        assert_eq!(contracts.len(), expected_contracts);
        let root = fixture.temp.path();
        let deck = fixture.deck.clone();
        let source = OriginalEventModelFixture::SOURCE;
        fixture.verify_original()?;
        verify_original_real_nonfinite_driver_order(variant, native)?;
        if variant == "midpoint" {
            let mut retyped_enable = fixture.contracts.clone();
            retyped_enable
                .iter_mut()
                .find(|contract| contract.instance == "AmidpointGated")
                .context("original enabled-midpoint IFS contract missing")?
                .ports
                .insert("enable".into(), vec!["R_META".into()]);
            fs::write(
                root.join("native.lib"),
                native.replace(
                    "AmidpointGated Q0 EN R_MIDPOINT_GATED",
                    "AmidpointGated Q0 R_META R_MIDPOINT_GATED",
                ),
            )?;
            assert!(
                verify_external_code_models(&deck, source, &retyped_enable, &interfaces).is_err(),
                "source IFS rejects REAL value on original digital enable port"
            );
            fs::write(root.join("native.lib"), native)?;
        }

        let wrong_source = native.replace(
            "Asource0 [Q0] ORIGINAL_SOURCE0",
            "Asource0 Q0 ORIGINAL_SOURCE0",
        );
        fs::write(root.join("native.lib"), wrong_source)?;
        assert!(
            verify_external_code_models(&deck, source, &contracts, &interfaces).is_err(),
            "original digital source is a required vector"
        );

        let mut corrupted = contracts;
        fs::write(
            root.join("native.lib"),
            native.replace("AgainNonfinite REAL_SUM R_META", "AgainNonfinite Q0 R_META"),
        )?;
        corrupted
            .iter_mut()
            .find(|contract| contract.instance == "AgainNonfinite")
            .context("missing original special-valued REAL gain contract")?
            .ports
            .insert("in".into(), vec!["Q0".into()]);
        assert!(
            verify_external_code_models(&deck, source, &corrupted, &interfaces).is_err(),
            "forged independent JSON witness cannot retype digital as REAL"
        );
        Ok(())
    }

    #[test]
    fn original_real_udn_nonfinite_source_ifs_and_driver_order() -> Result<()> {
        for (name, config, native) in [
            (
                "overflow-first",
                include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../runtime/xspice/fixtures/family/auto-discovery-original-real-nonfinite-overflow-first-x.json"
                )),
                include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../runtime/xspice/fixtures/family/native-original-real-nonfinite-overflow-first.lib"
                )),
            ),
            (
                "overflow-middle",
                include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../runtime/xspice/fixtures/family/auto-discovery-original-real-nonfinite-overflow-middle-x.json"
                )),
                include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../runtime/xspice/fixtures/family/native-original-real-nonfinite-overflow-middle.lib"
                )),
            ),
            (
                "nan-first",
                include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../runtime/xspice/fixtures/family/auto-discovery-original-real-nonfinite-nan-first-x.json"
                )),
                include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../runtime/xspice/fixtures/family/native-original-real-nonfinite-nan-first.lib"
                )),
            ),
            (
                "nan-middle",
                include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../runtime/xspice/fixtures/family/auto-discovery-original-real-nonfinite-nan-middle-x.json"
                )),
                include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../runtime/xspice/fixtures/family/native-original-real-nonfinite-nan-middle.lib"
                )),
            ),
            (
                "midpoint",
                include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../runtime/xspice/fixtures/family/auto-discovery-original-real-nonfinite-midpoint-x.json"
                )),
                include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../runtime/xspice/fixtures/family/native-original-real-nonfinite-midpoint.lib"
                )),
            ),
        ] {
            verify_original_real_nonfinite_ifs(name, config, native)?;
        }
        Ok(())
    }

    #[test]
    fn original_full_digital_strength_and_serial_real_gain_contracts() -> Result<()> {
        const CONFIG: &str = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../runtime/xspice/fixtures/family/auto-discovery-original-full-strength-cascade-x.json"
        ));
        const NATIVE: &str = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../runtime/xspice/fixtures/family/native-original-full-strength-cascade.lib"
        ));
        const SOURCE: &str = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../runtime/xspice/fixtures/family/original-full-strength-source.txt"
        ));
        let expected = [
            "0s", "1s", "Us", "0r", "1r", "Ur", "0z", "1z", "Uz", "0u", "1u", "Uu",
        ];
        let rows = SOURCE
            .lines()
            .filter(|line| !line.starts_with('*') && !line.trim().is_empty())
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), 36);
        for (index, row) in rows.iter().enumerate() {
            let fields = row.split_whitespace().collect::<Vec<_>>();
            assert_eq!(fields.len(), 2);
            assert_eq!(fields[0], format!("{}n", 12 * index));
            assert_eq!(fields[1], expected[index % 12]);
        }

        let fixture = OriginalEventModelFixture::new(CONFIG, NATIVE)?;
        let contracts = fixture.contracts.clone();
        let interfaces = fixture.interfaces.clone();
        assert_eq!(contracts.len(), 11);
        assert_eq!(
            contracts
                .iter()
                .filter(|contract| contract.ifspec.as_path()
                    == Path::new("original-xtraevt-real_gain.ifs"))
                .count(),
            3
        );
        let root = fixture.temp.path();
        let deck = fixture.deck.clone();
        let source = OriginalEventModelFixture::SOURCE;
        fixture.verify_original()?;

        // Even a source-authored witness changed together with the native
        // C-model statement cannot reinterpret a shared digital event as
        // the serial gain stage's real-valued input.
        let mut wrong_domains = contracts.clone();
        fs::write(
            root.join("native.lib"),
            NATIVE.replace("AgainChain RPOS RCHAIN", "AgainChain EN RCHAIN"),
        )?;
        wrong_domains
            .iter_mut()
            .find(|contract| contract.instance == "AgainChain")
            .context("original gain cascade contract not found")?
            .ports
            .insert("in".into(), vec!["EN".into()]);
        assert!(verify_external_code_models(&deck, source, &wrong_domains, &interfaces).is_err());

        // Original d_source declares a vector output, not a scalar even
        // when the vector contains exactly one original digital node.
        fs::write(
            root.join("native.lib"),
            NATIVE.replace("Astate [QEV] ORIG_STATE", "Astate QEV ORIG_STATE"),
        )?;
        assert!(verify_external_code_models(&deck, source, &contracts, &interfaces).is_err());
        Ok(())
    }

    #[test]
    fn original_d_source_unknown_strength_and_real_gain_fanout_contracts() -> Result<()> {
        const CONFIG: &str = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../runtime/xspice/fixtures/family/auto-discovery-original-real-strength-fanout-x.json"
        ));
        const NATIVE: &str = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../runtime/xspice/fixtures/family/native-original-real-strength-fanout.lib"
        ));
        let fixture = OriginalEventModelFixture::new(CONFIG, NATIVE)?;
        let mut contracts = fixture.contracts.clone();
        let interfaces = fixture.interfaces.clone();
        assert_eq!(contracts.len(), 9);
        let source_ifs = parse_ifspec(ORIGINAL_D_SOURCE)?;
        assert_eq!(source_ifs.model, "d_source");
        assert_eq!(source_ifs.ports.len(), 1);
        assert_eq!(source_ifs.ports[0].kind, "d");
        assert!(source_ifs.ports[0].vector);
        assert!(!source_ifs.ports[0].nullable);
        let gain_ifs = parse_ifspec(ORIGINAL_REAL_GAIN)?;
        assert_eq!(gain_ifs.model, "real_gain");
        assert_eq!(
            gain_ifs
                .ports
                .iter()
                .map(|port| (port.direction.as_str(), port.kind.as_str()))
                .collect::<Vec<_>>(),
            [("in", "real"), ("out", "real")]
        );
        assert_eq!(gain_ifs.parameters.len(), 5);
        let root = fixture.temp.path();
        let deck = fixture.deck.clone();
        let source = OriginalEventModelFixture::SOURCE;
        fixture.verify_original()?;

        let poisoned = NATIVE.replace("AgainPos RRAW RPOS", "AgainPos EN RPOS");
        fs::write(root.join("native.lib"), poisoned)?;
        let gain_pos = contracts
            .iter_mut()
            .find(|c| c.instance == "AgainPos")
            .context("gain original IFS contract missing")?;
        gain_pos.ports.insert("in".into(), vec!["EN".into()]);
        assert!(
            verify_external_code_models(&deck, source, &contracts, &interfaces).is_err(),
            "changed witness cannot retype an original d_source digital node as real_gain input"
        );
        // A source-produced digital vector cannot be mislabeled as a scalar,
        // even if the one-element JSON witness contains the same node.
        contracts = fixture.contracts.clone();
        fs::write(
            root.join("native.lib"),
            NATIVE.replace("Astate [QEV] ORIG_STATE", "Astate QEV ORIG_STATE"),
        )?;
        assert!(
            verify_external_code_models(&deck, source, &contracts, &interfaces).is_err(),
            "original d_source IFS mandates vector output shape"
        );
        Ok(())
    }

    fn verify_clocked_real_invalid_original_models(
        root: &Path,
        deck: &Path,
        source: &str,
        contracts: &[CodeModelContract],
        links: &[OpaqueXInterface],
        native: &str,
    ) -> Result<()> {
        // Rewriting the independent manifest cannot make original source
        // REAL event data a digital clock or digital input a real data port.
        for (physical, contract_port, wrong_node) in [
            ("Ahold RDATA CLK RHELD", "clk", "RDATA"),
            ("Ahold RDATA CLK RHELD", "in", "CLK"),
            ("Ahold RDATA CLK RHELD", "clk", "NULL"),
        ] {
            let mut retyped = contracts.to_vec();
            let target = retyped
                .iter_mut()
                .find(|c| c.instance == "Ahold")
                .context("missing true original sample-and-hold source instance")?;
            target
                .ports
                .insert(contract_port.into(), vec![wrong_node.into()]);
            let replacement = if contract_port == "in" {
                format!("Ahold {wrong_node} CLK RHELD")
            } else {
                format!("Ahold RDATA {wrong_node} RHELD")
            };
            fs::write(
                root.join("native.lib"),
                native.replace(physical, &replacement),
            )?;
            assert!(
                verify_external_code_models(deck, source, &retyped, links).is_err(),
                "original IFS must reject {contract_port}={wrong_node}, regardless of JSON witness"
            );
        }

        for modified in [
            native.replace("AsourceClock [CLK]", "AsourceClock CLK"),
            native.replace("real_delay(delay=610p)", "real_delay(delay=0)"),
            native.replace("real_delay(delay=610p)", "real_delay(delay=610p delay=1n)"),
            native.replace("ORIGINAL_DELAY real_delay(", "ORIGINAL_DELAY real_gain("),
        ] {
            fs::write(root.join("native.lib"), modified)?;
            assert!(
                verify_external_code_models(deck, source, contracts, links).is_err(),
                "changed original C-model source/parameter must fail verified IFS"
            );
        }
        Ok(())
    }

    #[test]
    fn original_clocked_real_delay_source_ifs_and_typed_event_domains() -> Result<()> {
        const CONFIG: &str = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../runtime/xspice/fixtures/family/auto-discovery-original-clocked-real-x.json"
        ));
        const NATIVE: &str = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../runtime/xspice/fixtures/family/native-original-clocked-real.lib"
        ));
        const CLOCK: &str = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../runtime/xspice/fixtures/family/original-clocked-real-clock.txt"
        ));
        const DATA: &str = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../runtime/xspice/fixtures/family/original-clocked-real-data.txt"
        ));
        let config: serde_json::Value = serde_json::from_str(CONFIG)?;
        let contracts: Vec<CodeModelContract> =
            serde_json::from_value(config["code_model_contracts"].clone())?;
        let links: Vec<OpaqueXInterface> =
            serde_json::from_value(config["opaque_x_interfaces"].clone())?;
        assert_eq!(contracts.len(), 10);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].pins.len(), 6);
        assert_eq!(links[0].pins[0], crate::whole::OpaqueXPinKind::Digital);
        assert!(links[0].pins[1..].iter().all(|p| *p == Analog));
        assert!(CLOCK.contains("33n 1z") && CLOCK.contains("46n 1u"));
        assert!(CLOCK.contains("80n 1r") && CLOCK.contains("60n 1s"));
        assert!(DATA.contains("60n 1s"));
        let original_clock = parse_ifspec(ORIGINAL_REAL_DELAY)?;
        assert_eq!(original_clock.model, "real_delay");
        assert_eq!(
            original_clock
                .ports
                .iter()
                .map(|p| (p.name.as_str(), p.kind.as_str(), p.nullable))
                .collect::<Vec<_>>(),
            [
                ("in", "real", false),
                ("clk", "d", false),
                ("out", "real", false)
            ]
        );
        let clock = contracts
            .iter()
            .find(|c| c.instance == "Ahold")
            .context("original clocked REAL sample C model contract missing")?;
        assert_eq!(clock.ports["in"], ["RDATA"]);
        assert_eq!(clock.ports["clk"], ["CLK"]);
        assert_eq!(clock.ports["out"], ["RHELD"]);
        assert!(NATIVE.contains("Ahold RDATA CLK RHELD ORIGINAL_DELAY"));
        assert!(NATIVE.contains("ORIGINAL_DELAY real_delay(delay=610p)"));
        assert!(NATIVE.contains("AloadEn EN EN_SINK EN_SOURCE INPUT_LOAD"));

        let temp = tempfile::tempdir()?;
        let root = temp.path();
        let source = ".include \"native.lib\"\n";
        let deck = root.join("original.cir");
        fs::write(&deck, source)?;
        fs::write(root.join("native.lib"), NATIVE)?;
        for (name, content) in [
            ("georgiatech-d_source-original.ifs", ORIGINAL_D_SOURCE),
            ("original-xtraevt-d_to_real.ifs", ORIGINAL_D_TO_REAL),
            ("original-xtraevt-real_delay.ifs", ORIGINAL_REAL_DELAY),
            ("original-xtraevt-real_gain.ifs", ORIGINAL_REAL_GAIN),
            ("original-xtraevt-real_to_v.ifs", ORIGINAL_REAL_TO_V),
        ] {
            fs::write(root.join(name), content)?;
        }
        verify_external_code_models(&deck, source, &contracts, &links)?;

        verify_clocked_real_invalid_original_models(
            root, &deck, source, &contracts, &links, NATIVE,
        )?;
        Ok(())
    }

    #[test]
    fn original_real_event_ifs_null_and_shared_domains() -> Result<()> {
        const CONFIG: &str = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../runtime/xspice/fixtures/family/auto-discovery-original-real-event-chain-x.json"
        ));
        const NATIVE: &str = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../runtime/xspice/fixtures/family/native-original-real-event-chain.lib"
        ));
        let config: serde_json::Value = serde_json::from_str(CONFIG)?;
        let contracts: Vec<CodeModelContract> =
            serde_json::from_value(config["code_model_contracts"].clone())?;
        let original_ifs = [
            (
                "d_to_real",
                ORIGINAL_D_TO_REAL,
                ["d", "d", "real"].as_slice(),
            ),
            (
                "real_delay",
                ORIGINAL_REAL_DELAY,
                ["real", "d", "real"].as_slice(),
            ),
            ("real_to_v", ORIGINAL_REAL_TO_V, ["real", "v"].as_slice()),
        ];
        let mut domains = BTreeMap::new();
        for contract in &contracts {
            let (name, text, kinds) = original_ifs
                .iter()
                .find(|(name, _, _)| contract.ifspec.to_string_lossy().contains(name))
                .context("source IFS missing from original real event test")?;
            let ifs = parse_ifspec(text)?;
            assert_eq!(&ifs.model, name);
            assert_eq!(
                ifs.ports
                    .iter()
                    .map(|p| p.kind.as_str())
                    .collect::<Vec<_>>(),
                *kinds
            );
            let instance = NATIVE
                .lines()
                .find(|line| line.starts_with(&format!("{} ", contract.instance)))
                .context("original C-model A instance missing")?;
            verify_bindings(&ifs, contract, instance)?;
            check_shared_original_event_types(&ifs, contract, &mut domains)?;
        }
        let d2r = parse_ifspec(ORIGINAL_D_TO_REAL)?;
        let delay = parse_ifspec(ORIGINAL_REAL_DELAY)?;
        let to_voltage = parse_ifspec(ORIGINAL_REAL_TO_V)?;
        assert!(d2r.ports[1].nullable);
        assert!(delay.ports.iter().all(|port| !port.nullable));
        assert_eq!(to_voltage.ports[0].kind, "real");
        assert_eq!(to_voltage.ports[1].kind, "v");

        let mut conflicting = contracts[1].clone();
        conflicting.ports.insert("in".into(), vec!["DATA".into()]);
        verify_bindings(
            &delay,
            &conflicting,
            "Adelay DATA CLOCK RHELD ORIGINAL_DELAY",
        )?;
        assert!(
            check_shared_original_event_types(&delay, &conflicting, &mut domains).is_err(),
            "changed witness cannot make a digital event into a real node"
        );

        let mut invalid_null = contracts[1].clone();
        invalid_null.ports.insert("in".into(), vec!["NULL".into()]);
        assert!(
            verify_bindings(
                &delay,
                &invalid_null,
                "Adelay NULL CLOCK RHELD ORIGINAL_DELAY"
            )
            .is_err(),
            "non-nullable original real event input cannot be NULL"
        );
        assert!(
            verify_bindings(&d2r, &contracts[0], "Ad2r DATA NULL RDATA ORIGINAL_D2R").is_ok(),
            "original d_to_real enables an explicitly nullable digital control"
        );
        Ok(())
    }

    #[test]
    fn original_mixed_adc_and_dac_differential_typed_connections() -> Result<()> {
        let adc = parse_ifspec(ORIGINAL_ADC)?;
        let dac = parse_ifspec(ORIGINAL_DAC)?;
        assert_eq!(adc.model, "adc_bridge");
        assert_eq!(dac.model, "dac_bridge");
        for (ifs, input, output) in [(&adc, "v", "d"), (&dac, "d", "v")] {
            assert_eq!(ifs.ports.len(), 2);
            assert_eq!(ifs.ports[0].direction, "in");
            assert_eq!(ifs.ports[1].direction, "out");
            assert_eq!(ifs.ports[0].kind, input);
            assert_eq!(ifs.ports[1].kind, output);
            assert!(ifs.ports.iter().all(|p| p.vector && p.bounds == (1, None)));
        }
        assert!(adc.ports[0].allowed_kinds.contains("vd"));
        assert!(adc.ports[0].allowed_kinds.contains("id"));
        assert!(!adc.ports[0].allowed_kinds.contains("gd"));
        assert!(!adc.ports[1].allowed_kinds.contains("v"));

        let actual = "Aadc [%vd(APLUS VCM)] [A_BIT] GT_ADC";
        assert_eq!(
            a_fields(actual)?,
            vec![
                Connection::Scalar("Aadc".into()),
                Connection::Vector(vec!["%vd(APLUS VCM)".into()]),
                Connection::Vector(vec!["A_BIT".into()]),
                Connection::Scalar("GT_ADC".into()),
            ]
        );
        let contract = CodeModelContract {
            native_model: "SR_ORIGINAL_MIXED".into(),
            instance: "Aadc".into(),
            ifspec: PathBuf::from("georgiatech-adc_bridge-original.ifs"),
            ports: [
                ("in".into(), vec!["%vd(APLUS VCM)".into()]),
                ("out".into(), vec!["A_BIT".into()]),
            ]
            .into_iter()
            .collect(),
            equal_width_ports: vec![vec!["in".into(), "out".into()]],
            parameter_vector_widths: BTreeMap::new(),
            table_shapes: Vec::new(),
        };
        assert_eq!(verify_bindings(&adc, &contract, actual)?, "GT_ADC");
        for bad in [
            "Aadc [%vd(VCM APLUS)] [A_BIT] GT_ADC",
            "Aadc [%gd(APLUS VCM)] [A_BIT] GT_ADC",
            "Aadc [%vd(APLUS VCM)] [%v(A_BIT)] GT_ADC",
            "Aadc [%vd(APLUS VCM] [A_BIT] GT_ADC",
            "Aadc [%vd(APLUS VCM))] [A_BIT] GT_ADC",
            "Aadc [%vd(APLUS VCM) %v(APLUS)] [A_BIT] GT_ADC",
        ] {
            assert!(verify_bindings(&adc, &contract, bad).is_err(), "{bad}");
        }
        for malformed in [
            "Aadc [%vd(APLUS VCM)] [A_BIT GT_ADC",
            "Aadc [%vd(APLUS VCM) [A_BIT] GT_ADC",
            "Aadc [%vd(APLUS VCM)] [A_BIT] GT_ADC %gd(foo",
        ] {
            assert!(a_fields(malformed).is_err(), "{malformed}");
        }
        let mut bad_contract = contract;
        bad_contract
            .ports
            .insert("in".into(), vec!["%gd(APLUS VCM)".into()]);
        assert!(
            verify_bindings(&adc, &bad_contract, "Aadc [%gd(APLUS VCM)] [A_BIT] GT_ADC",).is_err(),
            "invalid type must reject even if the independent witness matches it"
        );
        Ok(())
    }

    #[test]
    fn original_1991_digital_model_ports_are_genuinely_d_typed() -> Result<()> {
        let ifs = parse_ifspec(DLATCH)?;
        assert_eq!(ifs.model, "d_dlatch");
        assert_eq!(
            ifs.ports
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            ["data", "enable", "set", "reset", "out", "Nout"]
        );
        assert_eq!(
            ifs.ports
                .iter()
                .map(|p| p.direction.as_str())
                .collect::<Vec<_>>(),
            ["in", "in", "in", "in", "out", "out"]
        );
        assert!(ifs.ports.iter().all(|p| p.kind == "d" && !p.vector));
        assert!(!ifs.ports[0].nullable);
        assert!(ifs.ports[2].nullable);
        assert!(ifs.ports[5].nullable);
        Ok(())
    }

    #[test]
    fn original_genlut_and_fanout_logic_ifs_vector_bounds() -> Result<()> {
        let genlut = parse_ifspec(GENLUT)?;
        assert_eq!(genlut.model, "d_genlut");
        assert_eq!(genlut.ports.len(), 2);
        assert_eq!(genlut.ports[0].name, "in");
        assert_eq!(genlut.ports[1].name, "out");
        for port in &genlut.ports {
            assert!(port.vector);
            assert_eq!(port.bounds, (1, None));
            assert_eq!(port.kind, "d");
        }
        assert_eq!(genlut.ports[0].direction, "in");
        assert_eq!(genlut.ports[1].direction, "out");
        for (model, original) in [("d_and", AND), ("d_xor", XOR)] {
            let ifs = parse_ifspec(original)?;
            assert_eq!(ifs.model, model);
            assert_eq!(ifs.ports.len(), 2);
            assert_eq!(ifs.ports[0].bounds, (2, None));
            assert_eq!(ifs.ports[1].bounds, (1, Some(1)));
            assert!(ifs.ports[0].vector);
            assert!(!ifs.ports[1].vector);
        }
        // A bare dash is an unbounded vector bound only for a vector;
        // a scalar cannot claim vector bounds, including [1 -].
        assert!(
            parse_ifspec(&GENLUT.replace(
                "Vector_Bounds:       -                  -",
                "Vector_Bounds:       [0 -]              -"
            ))
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn original_ram_ifs_vector_width_and_directions() -> Result<()> {
        let ifs = parse_ifspec(RAM)?;
        assert_eq!(ifs.model, "d_ram");
        assert_eq!(ifs.ports.len(), 5);
        assert_eq!(
            ifs.ports.iter().map(|p| p.vector).collect::<Vec<_>>(),
            [true, true, true, false, true]
        );
        assert_eq!(ifs.ports[4].bounds, (1, Some(16)));
        assert_eq!(ifs.ports[2].bounds, (1, None));
        assert_eq!(ifs.ports[1].direction, "out");
        assert!(ifs.ports.iter().all(|p| p.kind == "d"));
        Ok(())
    }

    #[test]
    fn actual_original_vector_code_model_contract_and_widths() -> Result<()> {
        check_native(ram_library(), &ram_contract())?;
        for broken in [
            ram_library().replace("[A0 A1]", "A0"),
            ram_library().replace(
                "[S0 S1]",
                "[S0 S1 S2 S3 S4 S5 S6 S7 S8 S9 S10 S11 S12 S13 S14 S15 S16]",
            ),
            ram_library().replace("[D0 D1]", "[D0 D1"),
            ram_library().replace("d_ram", "d_dlatch"),
            ram_library().replace("Aram", "Awrong"),
            ram_library().replace("[Q0 Q1]", "[Q1 Q0]"),
        ] {
            let result = check_native(&broken, &ram_contract());
            assert!(
                result.is_err(),
                "invalid original IFS must be rejected: {broken}"
            );
        }
        let mut misbound = ram_contract();
        misbound
            .ports
            .insert("data_in".into(), vec!["Q0".into(), "Q1".into()]);
        let result = check_native(ram_library(), &misbound);
        assert!(
            result.is_err(),
            "independent original IFS named bindings must win"
        );
        Ok(())
    }

    #[test]
    fn original_ram_vector_relations_and_maximum_select_width() -> Result<()> {
        // Original Georgia Tech C loops over PORT_SIZE(data_in) when writing
        // data_out, so simply satisfying both published [1 -] bounds cannot
        // establish memory-safe wiring.
        let mut mismatch = ram_contract();
        mismatch.ports.insert(
            "data_out".into(),
            vec!["Q0".into(), "Q1".into(), "Q2".into()],
        );
        let malformed = ram_library().replace("[Q0 Q1]", "[Q0 Q1 Q2]");
        assert!(
            check_native(&malformed, &mismatch).is_err(),
            "different original RAM data input/output widths must fail"
        );

        let mut max_select = ram_contract();
        let max_select_nodes = (0..16).map(|n| format!("S{n}")).collect::<Vec<_>>();
        max_select
            .ports
            .insert("select".into(), max_select_nodes.clone());
        let native_select =
            ram_library().replace("[S0 S1]", &format!("[{}]", max_select_nodes.join(" ")));
        check_native(&native_select, &max_select)?;
        let mut excessive = max_select.clone();
        let mut excessive_nodes = max_select_nodes;
        excessive_nodes.push("S16".into());
        excessive
            .ports
            .insert("select".into(), excessive_nodes.clone());
        assert!(
            check_native(
                &ram_library().replace("[S0 S1]", &format!("[{}]", excessive_nodes.join(" "))),
                &excessive
            )
            .is_err()
        );

        // Data bus vectors have unbounded upper widths in the actual IFS.
        let mut wide = ram_contract();
        let d = (0..32).map(|n| format!("D{n}")).collect::<Vec<_>>();
        let q = (0..32).map(|n| format!("Q{n}")).collect::<Vec<_>>();
        wide.ports.insert("data_in".into(), d.clone());
        wide.ports.insert("data_out".into(), q.clone());
        let lib = ram_library()
            .replace("[D0 D1]", &format!("[{}]", d.join(" ")))
            .replace("[Q0 Q1]", &format!("[{}]", q.join(" ")));
        check_native(&lib, &wide)?;

        // Invalid/misdeclared relationships fail independently of native A
        // labels and actual vector groups.
        for group in [
            vec!["data_in".into(), "write_en".into()],
            vec!["data_in".into(), "unknown".into()],
            vec!["data_in".into(), "DATA_IN".into()],
            vec!["data_out".into()],
        ] {
            let mut invalid = ram_contract();
            invalid.equal_width_ports = vec![group];
            assert!(check_native(ram_library(), &invalid).is_err());
        }
        Ok(())
    }

    #[test]
    fn named_contract_is_order_independent_and_complete() -> Result<()> {
        let ifs = parse_ifspec(RAM)?;
        let contract = ram_contract();
        verify_bindings(
            &ifs,
            &contract,
            "Aram [D0 D1] [Q0 Q1] [A0 A1] WE [S0 S1] CM",
        )?;

        // JSON field order is irrelevant; only the original IFS determines
        // the physical A-instance order. Case-insensitive original port names
        // are accepted, but ambiguous duplicates are never accepted.
        let mut varied = contract.clone();
        let original = varied
            .ports
            .remove("data_in")
            .context("data_in must exist")?;
        varied.ports.insert("DATA_IN".into(), original);
        verify_bindings(&ifs, &varied, "Aram [D0 D1] [Q0 Q1] [A0 A1] WE [S0 S1] CM")?;

        let mut duplicated = varied.clone();
        duplicated
            .ports
            .insert("data_in".into(), vec!["D0".into(), "D1".into()]);
        assert!(
            verify_bindings(
                &ifs,
                &duplicated,
                "Aram [D0 D1] [Q0 Q1] [A0 A1] WE [S0 S1] CM"
            )
            .is_err()
        );

        for missing in ["data_in", "data_out", "address", "write_en", "select"] {
            let mut incomplete = contract.clone();
            incomplete.ports.remove(missing);
            assert!(
                verify_bindings(
                    &ifs,
                    &incomplete,
                    "Aram [D0 D1] [Q0 Q1] [A0 A1] WE [S0 S1] CM"
                )
                .is_err()
            );
        }
        let mut unknown = contract.clone();
        unknown
            .ports
            .insert("no_such_port".into(), vec!["D0".into()]);
        assert!(
            verify_bindings(&ifs, &unknown, "Aram [D0 D1] [Q0 Q1] [A0 A1] WE [S0 S1] CM").is_err()
        );

        let serialized = serde_json::to_string(&contract)?;
        let roundtrip: CodeModelContract = serde_json::from_str(&serialized)?;
        let duplicated_json = serialized.replace(
            "\"address\":[\"A0\",\"A1\"]",
            "\"address\":[\"A0\",\"A1\"],\"address\":[\"A1\",\"A0\"]",
        );
        assert_ne!(duplicated_json, serialized);
        assert!(
            serde_json::from_str::<CodeModelContract>(&duplicated_json).is_err(),
            "JSON map duplicate keys must not overwrite the original binding"
        );
        verify_bindings(
            &ifs,
            &roundtrip,
            "Aram [D0 D1] [Q0 Q1] [A0 A1] WE [S0 S1] CM",
        )?;
        assert!(
            serde_json::from_str::<CodeModelContract>(
                r#"{"native_model":"NATIVE","instance":"Aram","ifspec":"ram.ifs","ports":[{"name":"data_in","nodes":["D0","D1"]}]}"#
            ).is_err(),
            "old positional contract JSON must not parse"
        );
        Ok(())
    }

    #[test]
    fn original_port_and_parameter_vector_grammar_share_exact_bounds() -> Result<()> {
        for (vector, bounds, expected) in [
            ("no", "-", (false, (1, Some(1)))),
            ("yes", "-", (true, (1, None))),
            ("YeS", "[1 -]", (true, (1, None))),
            ("YES", "[2 16]", (true, (2, Some(16)))),
            ("yes", "[12 12]", (true, (12, Some(12)))),
        ] {
            assert_eq!(ifs_vector_shape(vector, bounds, "out")?, expected);
            assert_eq!(
                ifs_vector_shape(vector, bounds, "parameter gain")?,
                expected
            );
        }
        for (vector, bounds) in [
            ("invalid", "-"),
            ("no", "[1 -]"),
            ("yes", "[0 -]"),
            ("yes", "[3 2]"),
            ("yes", "[1 0]"),
            ("yes", "[3]"),
            ("yes", "[3 4 5]"),
            ("yes", "[n 4]"),
            ("yes", "[1 n]"),
            ("yes", "[184467440737095516161 -]"),
            ("yes", "1 4"),
            ("yes", "[- -]"),
        ] {
            for subject in ["out", "parameter gain"] {
                assert!(
                    ifs_vector_shape(vector, bounds, subject).is_err(),
                    "invalid IFS {subject} vector={vector} bounds={bounds} accepted"
                );
            }
        }
        for (value, expected) in [("yes", true), ("NO", false), ("YeS", true)] {
            for field in ["Vector", "Null_Allowed"] {
                assert_eq!(ifs_yes_no(value, "out", field)?, expected);
                assert_eq!(ifs_yes_no(value, "parameter input_load", field)?, expected);
            }
        }
        for invalid in ["true", "false", "1", "maybe", "", "-"] {
            assert!(ifs_yes_no(invalid, "out", "Null_Allowed").is_err());
            assert!(ifs_yes_no(invalid, "parameter gain", "Null_Allowed").is_err());
        }
        Ok(())
    }

    #[test]
    fn malformed_ifspec_fails_closed() {
        for broken in [
            RAM.replace(
                "Vector_Bounds:        [1 16]",
                "Vector_Bounds:        [17 16]",
            ),
            RAM.replace("Default_Type:        d", "Default_Type:        v"),
            RAM.replace("Port_Name:", "Unknown_Port_Name:"),
            RAM.replace("Vector:               yes", "Vector:               MAYBE"),
            RAM.replace("Null_Allowed:        no", "Null_Allowed:        MAYBE"),
        ] {
            assert!(
                parse_ifspec(&broken).is_err(),
                "bad IFS must fail: {broken}"
            );
        }
    }

    #[test]
    fn original_scalar_ifs_and_nullable_required() -> Result<()> {
        let spec = CodeModelContract {
            native_model: "NATIVE".into(),
            instance: "Alatch".into(),
            ifspec: PathBuf::from("dlatch.ifs"),
            ports: ["DATA", "EN", "SET", "RESET", "Q", "NQ"]
                .iter()
                .zip(["data", "enable", "set", "reset", "out", "Nout"])
                .map(|(node, name)| (name.to_ascii_lowercase(), vec![(*node).into()]))
                .collect(),
            equal_width_ports: vec![],
            parameter_vector_widths: BTreeMap::new(),
            table_shapes: Vec::new(),
        };
        let ifs = parse_ifspec(DLATCH)?;
        verify_bindings(&ifs, &spec, "Alatch DATA EN SET RESET Q NQ CM")?;
        for bad in [
            "Alatch EN DATA SET RESET Q NQ CM",
            "Alatch DATA EN SET RESET Q NULL CM",
            "Alatch [DATA] EN SET RESET Q NQ CM",
            "Alatch DATA EN [SET] RESET Q NQ CM",
        ] {
            assert!(verify_bindings(&ifs, &spec, bad).is_err());
        }
        Ok(())
    }
}
