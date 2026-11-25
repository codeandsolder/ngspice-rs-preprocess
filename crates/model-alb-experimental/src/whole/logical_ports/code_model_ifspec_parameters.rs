//! Validate original XSPICE parameter tables and independently declared
//! relations against reachable native .model definitions.
use super::{CodeModelContract, Ifs, ifs_vector_shape, ifs_yes_no, table_field};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug)]
pub(super) struct IfsParameter {
    name: String,
    datatype: String,
    vector: bool,
    bounds: (usize, Option<usize>),
    nullable: bool,
    has_default: bool,
    limits: Option<(Option<f64>, Option<f64>)>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LookupTableShape {
    pub parameter: String,
    pub input_port: String,
    pub output_port: String,
    pub allowed_symbols: String,
}

fn numeric_literal(text: &str) -> Result<f64> {
    let text = text.trim();
    let mut boundary = None;
    for (i, _) in text.char_indices().skip(1) {
        if text[..i].parse::<f64>().is_ok() {
            boundary = Some(i);
        }
    }
    if text.parse::<f64>().is_ok() {
        boundary = Some(text.len());
    }
    let split = boundary.context("invalid numeric literal")?;
    let value: f64 = text[..split].parse()?;
    ensure!(value.is_finite(), "non-finite SPICE numeric literal");
    let suffix = text[split..].to_ascii_lowercase();
    let scale = if suffix.is_empty() {
        1.0
    } else if suffix.starts_with("meg") {
        1e6
    } else if suffix.starts_with("mil") {
        25.4e-6
    } else {
        match suffix.chars().next() {
            Some('t') => 1e12,
            Some('g') => 1e9,
            Some('k') => 1e3,
            Some('m') => 1e-3,
            Some('u') => 1e-6,
            Some('n') => 1e-9,
            Some('p') => 1e-12,
            Some('f') => 1e-15,
            Some('a') => 1e-18,
            _ => bail!("unknown SPICE numeric suffix {suffix}"),
        }
    };
    let scaled = value * scale;
    ensure!(scaled.is_finite(), "SPICE numeric literal overflows");
    Ok(scaled)
}

fn limits(text: &str) -> Result<Option<(Option<f64>, Option<f64>)>> {
    if text == "-" {
        return Ok(None);
    }
    let inner = text
        .strip_prefix('[')
        .and_then(|v| v.strip_suffix(']'))
        .context("malformed original IFS numeric limits")?;
    let pieces = inner.split_whitespace().collect::<Vec<_>>();
    ensure!(pieces.len() == 2, "IFS parameter limits need two bounds");
    let lower = (pieces[0] != "-")
        .then(|| numeric_literal(pieces[0]))
        .transpose()?;
    let upper = (pieces[1] != "-")
        .then(|| numeric_literal(pieces[1]))
        .transpose()?;
    ensure!(
        lower.zip(upper).is_none_or(|(a, b)| a <= b),
        "IFS numeric limits reversed"
    );
    Ok(Some((lower, upper)))
}

pub(super) fn append_parameter_table(
    fields: &BTreeMap<String, Vec<String>>,
    out: &mut Vec<IfsParameter>,
) -> Result<()> {
    if fields.is_empty() {
        return Ok(());
    }
    let names = fields
        .get("parameter_name")
        .context("IFS PARAMETER_TABLE lacks Parameter_Name")?;
    ensure!(!names.is_empty(), "empty IFS PARAMETER_TABLE");
    let datatypes = table_field(fields, "data_type", names.len())?;
    let defaults = table_field(fields, "default_value", names.len())?;
    let limit_fields = table_field(fields, "limits", names.len())?;
    let vectors = table_field(fields, "vector", names.len())?;
    let widths = table_field(fields, "vector_bounds", names.len())?;
    let nullable = table_field(fields, "null_allowed", names.len())?;
    for (i, name) in names.iter().enumerate() {
        ensure!(
            !out.iter()
                .any(|p: &IfsParameter| p.name.eq_ignore_ascii_case(name)),
            "duplicate original IFS parameter {name}"
        );
        let datatype = datatypes[i].to_ascii_lowercase();
        ensure!(
            matches!(datatype.as_str(), "real" | "int" | "boolean" | "string"),
            "IFS parameter {name} has unsupported datatype {datatype}"
        );
        let subject = format!("parameter {name}");
        let (vector, bounds) = ifs_vector_shape(&vectors[i], &widths[i], &subject)?;
        let nullable = ifs_yes_no(&nullable[i], &subject, "Null_Allowed")?;
        out.push(IfsParameter {
            name: name.clone(),
            datatype,
            vector,
            bounds,
            nullable,
            has_default: defaults[i] != "-",
            limits: limits(&limit_fields[i])?,
        });
    }
    Ok(())
}

// Literal SPICE model parser: quoted strings, bracketed vectors and
// both equals and whitespace parameter assignment forms.
struct Cursor<'a> {
    rest: &'a str,
}

impl<'a> Cursor<'a> {
    fn spaces(&mut self) {
        self.rest = self
            .rest
            .trim_start_matches(|c: char| c.is_ascii_whitespace() || c == ',');
    }

    fn word(&mut self) -> Result<&'a str> {
        self.spaces();
        let len = self
            .rest
            .find(|c: char| c.is_ascii_whitespace() || matches!(c, '=' | ',' | ';'))
            .unwrap_or(self.rest.len());
        ensure!(len > 0, "expected SPICE model parameter or value");
        let (word, remaining) = self.rest.split_at(len);
        self.rest = remaining;
        Ok(word)
    }

    fn value(&mut self) -> Result<String> {
        self.spaces();
        let first = self
            .rest
            .chars()
            .next()
            .context("missing SPICE model parameter value")?;
        if first == '[' || matches!(first, '"' | '\'') {
            let closing = if first == '[' { ']' } else { first };
            let mut escaped = false;
            for (i, c) in self.rest.char_indices().skip(1) {
                if escaped {
                    escaped = false;
                } else if c == '\\' && closing != ']' {
                    escaped = true;
                } else if c == closing {
                    let len = i + c.len_utf8();
                    let value = self.rest[..len].to_owned();
                    self.rest = &self.rest[len..];
                    ensure!(
                        self.rest.is_empty()
                            || self.rest.starts_with(
                                |c: char| c.is_ascii_whitespace() || matches!(c, ',' | ';')
                            ),
                        "junk after quoted/vector SPICE model parameter"
                    );
                    return Ok(value);
                }
            }
            bail!("unterminated SPICE model parameter vector/string");
        }
        ensure!(
            !matches!(first, '{' | '(' | ')'),
            "unresolved SPICE expression is not statically verifiable"
        );
        Ok(self.word()?.to_owned())
    }
}

fn model_values(statement: &str) -> Result<BTreeMap<String, String>> {
    let mut cursor = Cursor {
        rest: statement.trim(),
    };
    ensure!(
        cursor.word()?.eq_ignore_ascii_case(".model"),
        "expected original native .model declaration"
    );
    let _name = cursor.word()?;
    cursor.spaces();
    let kind_len = cursor
        .rest
        .find(|ch: char| ch.is_ascii_whitespace() || ch == '(')
        .unwrap_or(cursor.rest.len());
    ensure!(kind_len > 0, "missing original code-model kind");
    cursor.rest = &cursor.rest[kind_len..];
    cursor.spaces();
    if cursor.rest.starts_with('(') {
        cursor.rest = cursor.rest[1..].trim();
        cursor.rest = cursor
            .rest
            .strip_suffix(')')
            .context("unterminated SPICE model parameter list")?
            .trim();
    }
    let mut parameters = BTreeMap::new();
    while !cursor.rest.is_empty() {
        cursor.spaces();
        if cursor.rest.is_empty() || cursor.rest.starts_with(';') {
            break;
        }
        let name = cursor.word()?.to_ascii_lowercase();
        ensure!(
            name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
            "malformed SPICE parameter name {name}"
        );
        cursor.spaces();
        if cursor.rest.starts_with('=') {
            cursor.rest = &cursor.rest[1..];
        }
        let value = cursor.value()?;
        ensure!(
            parameters.insert(name.clone(), value).is_none(),
            "duplicate original SPICE model parameter {name}"
        );
    }
    Ok(parameters)
}

fn values_for(ifs: &IfsParameter, raw: &str) -> Result<Vec<String>> {
    let values = if raw.starts_with('[') {
        ensure!(ifs.vector, "IFS scalar parameter {} given vector", ifs.name);
        let inner = raw
            .strip_suffix(']')
            .context("unterminated SPICE vector parameter")?;
        inner[1..]
            .split(|ch: char| ch.is_ascii_whitespace() || ch == ',')
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>()
    } else {
        // Original XSPICE vector params accept singleton/broadcast values.
        vec![raw.to_owned()]
    };
    ensure!(
        values.len() >= ifs.bounds.0 && ifs.bounds.1.is_none_or(|upper| values.len() <= upper),
        "IFS parameter {} has {} elements outside {:?}",
        ifs.name,
        values.len(),
        ifs.bounds
    );
    for value in &values {
        match ifs.datatype.as_str() {
            "real" | "int" => {
                let numeric = numeric_literal(value).with_context(|| {
                    format!(
                        "invalid original IFS parameter {} numeric value {value}",
                        ifs.name
                    )
                })?;
                if ifs.datatype == "int" {
                    ensure!(
                        numeric.fract() == 0.0
                            && numeric >= f64::from(i32::MIN)
                            && numeric <= f64::from(i32::MAX),
                        "IFS integer parameter {} is not a representable original C integer: {value}",
                        ifs.name
                    );
                }
                if let Some((low, high)) = ifs.limits {
                    ensure!(
                        low.is_none_or(|v| numeric >= v) && high.is_none_or(|v| numeric <= v),
                        "original IFS parameter {} = {value} outside original limits {:?}",
                        ifs.name,
                        ifs.limits
                    );
                }
            }
            "boolean" => ensure!(
                matches!(
                    value.to_ascii_lowercase().as_str(),
                    "true" | "false" | "yes" | "no" | "0" | "1"
                ),
                "IFS boolean parameter {} has invalid value {value}",
                ifs.name
            ),
            "string" => ensure!(
                value.len() >= 2
                    && ((value.starts_with('"') && value.ends_with('"'))
                        || (value.starts_with('\'') && value.ends_with('\''))),
                "IFS string parameter {} needs a quoted literal",
                ifs.name
            ),
            other => bail!("unsupported original IFS datatype {other}"),
        }
    }
    Ok(values)
}

pub(super) fn verify_parameters(
    ifs: &Ifs,
    contract: &CodeModelContract,
    model_statement: &str,
) -> Result<()> {
    let provided = model_values(model_statement)?;
    let original = ifs
        .parameters
        .iter()
        .map(|p| (p.name.to_ascii_lowercase(), p))
        .collect::<BTreeMap<_, _>>();
    let mut sizes = BTreeMap::new();
    for (name, raw) in &provided {
        let declaration = original.get(name).with_context(|| {
            format!(
                "original {} IFS does not declare parameter {name}",
                ifs.model
            )
        })?;
        sizes.insert(name.clone(), values_for(declaration, raw)?.len());
    }
    for declared in &ifs.parameters {
        ensure!(
            declared.nullable
                || declared.has_default
                || provided.contains_key(&declared.name.to_ascii_lowercase()),
            "required original IFS parameter {} missing from .model",
            declared.name
        );
    }
    let named_ports = contract
        .ports
        .iter()
        .map(|(name, nodes)| (name.to_ascii_lowercase(), nodes.len()))
        .collect::<BTreeMap<_, _>>();
    let mut mapped = BTreeSet::new();
    for (parameter, port) in &contract.parameter_vector_widths {
        let name = parameter.to_ascii_lowercase();
        ensure!(
            mapped.insert(name.clone()),
            "case-insensitive duplicate vector parameter relation {parameter}"
        );
        let declared = original
            .get(&name)
            .with_context(|| format!("vector relation refers to unknown parameter {parameter}"))?;
        ensure!(
            declared.vector,
            "vector relation refers to scalar IFS parameter {parameter}"
        );
        ensure!(
            ifs.ports
                .iter()
                .any(|original| original.name.eq_ignore_ascii_case(port) && original.vector),
            "vector parameter relation port {port} must be an original vector"
        );
        let port_width = named_ports
            .get(&port.to_ascii_lowercase())
            .with_context(|| format!("vector relation refers to unknown port {port}"))?;
        let param_width = sizes
            .get(&name)
            .with_context(|| format!("vector parameter {parameter} must be explicitly supplied"))?;
        ensure!(
            param_width == port_width,
            "IFS parameter {parameter} width {param_width} differs from port {port} width {port_width}"
        );
    }
    verify_table_shapes(ifs, contract, &original, &provided, &named_ports)?;
    Ok(())
}

fn verify_table_shapes(
    ifs: &Ifs,
    contract: &CodeModelContract,
    original: &BTreeMap<String, &IfsParameter>,
    provided: &BTreeMap<String, String>,
    named_ports: &BTreeMap<String, usize>,
) -> Result<()> {
    let mut tables = BTreeSet::new();
    for shape in &contract.table_shapes {
        let name = shape.parameter.to_ascii_lowercase();
        ensure!(
            tables.insert(name.clone()),
            "duplicate original IFS table shape for parameter {}",
            shape.parameter
        );
        let decl = original
            .get(&name)
            .with_context(|| format!("table shape refers to undefined IFS parameter {name}"))?;
        ensure!(
            decl.datatype == "string" && !decl.vector,
            "IFS table parameter {name} must be a scalar string"
        );
        let raw = provided
            .get(&name)
            .with_context(|| format!("required IFS shape parameter {name} not provided"))?;
        let literal = raw
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .or_else(|| raw.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
            .context("lookup table must use a quoted literal")?;
        ensure!(
            literal.is_ascii() && !literal.contains('\\'),
            "XSPICE original lookup table must be a non-escaped ASCII literal"
        );
        let inputs = named_ports
            .get(&shape.input_port.to_ascii_lowercase())
            .with_context(|| format!("lookup shape input port {} is unknown", shape.input_port))?;
        let outputs = named_ports
            .get(&shape.output_port.to_ascii_lowercase())
            .with_context(|| {
                format!("lookup shape output port {} is unknown", shape.output_port)
            })?;
        ensure!(
            ifs.ports
                .iter()
                .any(|p| p.name.eq_ignore_ascii_case(&shape.input_port)
                    && p.vector
                    && p.direction == "in"),
            "lookup shape input {} must be an original vector input",
            shape.input_port
        );
        ensure!(
            ifs.ports
                .iter()
                .any(|p| p.name.eq_ignore_ascii_case(&shape.output_port)
                    && p.vector
                    && p.direction == "out"),
            "lookup shape output {} must be an original vector output",
            shape.output_port
        );
        // Original 2016 C model computes (1 << isize) * osize as signed int.
        ensure!(
            *inputs < 31,
            "original lookup index shift exceeds signed-int range"
        );
        let expected = (1_usize << inputs)
            .checked_mul(*outputs)
            .context("original lookup table dimension overflow")?;
        ensure!(
            i32::try_from(expected).is_ok(),
            "original XSPICE LUT uses signed int table length; {expected} exceeds range"
        );
        ensure!(
            literal.len() == expected,
            "original lookup table {name} has {} values, requires {expected} for {inputs} inputs and {outputs} outputs",
            literal.len()
        );
        let alphabet = shape.allowed_symbols.chars().collect::<BTreeSet<_>>();
        ensure!(
            !alphabet.is_empty(),
            "original lookup table alphabet is empty"
        );
        ensure!(
            literal.chars().all(|c| alphabet.contains(&c)),
            "original lookup table {name} contains an undeclared state symbol"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn original_model_parameter_shapes_and_literals() -> Result<()> {
        const CONFIG: &str = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../runtime/xspice/fixtures/family/auto-discovery-original-genlut-fanout-x.json"
        ));
        const IFS: &str = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../runtime/xspice/fixtures/family/tim-edwards-d_genlut-original.ifs"
        ));
        const NATIVE: &str = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../runtime/xspice/fixtures/family/native-original-genlut-fanout.lib"
        ));
        let config: serde_json::Value = serde_json::from_str(CONFIG)?;
        let contracts: Vec<CodeModelContract> =
            serde_json::from_value(config["code_model_contracts"].clone())?;
        let contract = contracts
            .iter()
            .find(|spec| spec.instance == "AGEN")
            .context("original LUT contract missing")?;
        let ifs = super::super::parse_ifspec(IFS)?;
        assert_eq!(ifs.parameters.len(), 5);
        let declaration = NATIVE
            .lines()
            .find(|line| line.starts_with(".model ORIG_GEN "))
            .context("original d_genlut model missing")?;
        verify_parameters(&ifs, contract, declaration)?;

        for (old, new) in [
            ("01100001z01z", "01100001z01"),
            ("01100001z01z", "01100001z01z0"),
            ("01100001z01z", "01100001Z01z"),
            ("rise_delay=[550p 750p 900p]", "rise_delay=[550p 750p]"),
            ("fall_delay=[650p 850p 1100p]", "fall_delay=[650p 850p]"),
            ("input_load=[0 0]", "input_load=[0]"),
            ("input_delay=[150p 250p]", "input_delay=[150p]"),
            ("rise_delay=[550p 750p 900p]", "rise_delay=[0 750p 900p]"),
            (
                "rise_delay=[550p 750p 900p]",
                "rise_delay=[invalid 750p 900p]",
            ),
            (r#"table_values="01100001z01z" "#, ""),
        ] {
            let corrupted = declaration.replace(old, new);
            assert_ne!(declaration, corrupted, "{old}");
            assert!(
                verify_parameters(&ifs, contract, &corrupted).is_err(),
                "original model parameter corruption should reject: {corrupted}"
            );
        }
        for (parameter, port) in [
            ("rise_delay", "undefined"),
            ("rise_delay", "table_values"),
            ("table_values", "out"),
            ("rise_delay", "out"),
        ] {
            let mut invalid = contract.clone();
            if parameter == "rise_delay" && port == "out" {
                invalid
                    .parameter_vector_widths
                    .insert("RISE_DELAY".into(), port.into());
            } else {
                invalid
                    .parameter_vector_widths
                    .insert(parameter.into(), port.into());
            }
            assert!(
                verify_parameters(&ifs, &invalid, declaration).is_err(),
                "invalid parameter relation {parameter} -> {port} must reject"
            );
        }
        for (input, output) in [("missing", "out"), ("out", "in"), ("in", "missing")] {
            let mut invalid = contract.clone();
            invalid.table_shapes[0].input_port = input.into();
            invalid.table_shapes[0].output_port = output.into();
            assert!(verify_parameters(&ifs, &invalid, declaration).is_err());
        }

        // XSPICE permits short parameter vectors to broadcast the last
        // value. Only our independently declared per-port relation requires
        // exact output width for this particular native boundary.
        let mut broadcast = contract.clone();
        broadcast.parameter_vector_widths.remove("rise_delay");
        let short = declaration.replace("rise_delay=[550p 750p 900p]", "rise_delay=[550p]");
        verify_parameters(&ifs, &broadcast, &short)?;
        Ok(())
    }

    #[test]
    fn spice_numeric_engineering_suffixes_and_bound_checks() -> Result<()> {
        for (raw, want) in [
            ("550p", 550e-12),
            ("1e-12", 1e-12),
            ("4.7meg", 4.7e6),
            ("1mil", 25.4e-6),
            ("-15u", -15e-6),
            ("4kOhm", 4e3),
            (".125", 0.125),
        ] {
            assert!((numeric_literal(raw)? - want).abs() <= want.abs().mul_add(1e-12, 1e-20));
        }
        for invalid in ["abc", "1b", "nan", "inf", "1e309", "1e", ""] {
            assert!(numeric_literal(invalid).is_err(), "{invalid}");
        }
        Ok(())
    }

    #[test]
    fn spice_model_literal_forms_preserve_vectors_and_duplicate_rejection() -> Result<()> {
        for model in [
            r#".model REAL d_genlut(table_values="01zx" input_load=[1p 2p] rise_delay=[1n 2n])"#,
            r#".model REAL d_genlut ( table_values = "01zx" input_load = [1p 2p] rise_delay = [1n 2n] )"#,
            r#".model REAL d_genlut table_values "01zx" input_load [1p 2p] rise_delay [1n 2n]"#,
        ] {
            let p = model_values(model)?;
            assert_eq!(p["table_values"], r#""01zx""#);
            assert_eq!(p["rise_delay"], "[1n 2n]");
            assert_eq!(p["input_load"], "[1p 2p]");
        }
        for invalid in [
            r#".model M d_genlut(table_values="01" table_values="10")"#,
            ".model M d_genlut(input_load=[1p 2p)",
            ".model M d_genlut(input_load={PARAM})",
            ".model M d_genlut(input_load=[1p 2p] nonsense)",
        ] {
            assert!(model_values(invalid).is_err(), "{invalid}");
        }
        Ok(())
    }
}
