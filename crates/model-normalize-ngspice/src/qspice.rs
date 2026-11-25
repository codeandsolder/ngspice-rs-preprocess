//! Source-scoped QSPICE plaintext normalization. No compiled device execution.
mod numeric;
use crate::{
    lexical::{Edit, Statement, apply_edits, replace_span, statements, tokens},
    linked_source::{LinkedSource, RootKind, SourceOrigin},
    model_card::{parse_model_card, parse_spice_constant},
};
use anyhow::{Context, Result, ensure};
use std::{
    borrow::Cow,
    fmt,
    ops::Range,
    path::{Path, PathBuf},
};

/// A dialect construct that cannot currently be represented faithfully.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Feature {
    /// Schematic, symbol, raw output or compiled resource.
    NonTextResource,
    /// Invalid byte sequence for the selected source encoding.
    SourceEncoding,
    /// Numeric spelling not representable in the native grammar.
    NumericLiteral,
    /// Digital, specialized or compiled device class.
    SpecialDevice,
    /// Simulator-specific command, search path or execution directive.
    Directive,
    /// Implicit or explicit parasitics requiring a physical device contract.
    PassiveParasitics,
    /// Analysis scheduling or output-time semantics.
    TransientTiming,
    /// Nonlinear model laws or defaults without a qualified native binding.
    ModelLaw,
}

impl fmt::Display for Feature {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::NonTextResource => "non-netlist resource",
            Self::SourceEncoding => "source encoding",
            Self::NumericLiteral => "numeric literal",
            Self::SpecialDevice => "specialized device class",
            Self::Directive => "simulator-specific directive",
            Self::PassiveParasitics => "passive parasitics",
            Self::TransientTiming => "transient timing",
            Self::ModelLaw => "model law",
        })
    }
}

/// Structured diagnostic retaining the original admitted source location.
#[derive(Debug)]
pub struct Unsupported {
    /// Original source path.
    pub path: PathBuf,
    /// Original physical line.
    pub line: usize,
    /// Unsupported physical or syntactic feature.
    pub feature: Feature,
    /// Explanation of the unsupported semantics.
    pub detail: String,
}

impl fmt::Display for Unsupported {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}:{}: unsupported QSPICE {}: {}",
            self.path.display(),
            self.line,
            self.feature,
            self.detail
        )
    }
}
impl std::error::Error for Unsupported {}

fn unsupported(path: &Path, line: usize, feature: Feature, detail: &str) -> anyhow::Error {
    Unsupported {
        path: path.to_owned(),
        line,
        feature,
        detail: detail.to_owned(),
    }
    .into()
}

pub(crate) fn validate_resource(path: &Path, input: &[u8]) -> Result<()> {
    if input.contains(&0)
        || input.starts_with(b"\xff\xd8\xff")
        || input.starts_with("ÿØÿÛ".as_bytes())
        || path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                ["qsch", "qsym", "qraw", "dll", "exe"]
                    .iter()
                    .any(|candidate| extension.eq_ignore_ascii_case(candidate))
            })
    {
        return Err(unsupported(
            path,
            1,
            Feature::NonTextResource,
            "supply a plaintext circuit or model library",
        ));
    }
    Ok(())
}

// Windows-1252's 0x80..0x9f extension; undefined code points are
// diagnosed instead of inserting replacement characters into names.
const EXTENDED: [Option<char>; 32] = [
    Some('€'),
    None,
    Some('‚'),
    Some('ƒ'),
    Some('„'),
    Some('…'),
    Some('†'),
    Some('‡'),
    Some('ˆ'),
    Some('‰'),
    Some('Š'),
    Some('‹'),
    Some('Œ'),
    None,
    Some('Ž'),
    None,
    None,
    Some('‘'),
    Some('’'),
    Some('“'),
    Some('”'),
    Some('•'),
    Some('–'),
    Some('—'),
    Some('˜'),
    Some('™'),
    Some('š'),
    Some('›'),
    Some('œ'),
    None,
    Some('ž'),
    Some('Ÿ'),
];

// QSPICE documents Windows-1252 netlists. UTF-8 is accepted only with an
// explicit BOM, avoiding heuristic reinterpretation of valid legacy bytes.
// https://forum.qorvo.com/t/can-text-comment-support-chinese-utf8/16722/3
pub(crate) fn decode_resource<'a>(path: &Path, input: &'a [u8]) -> Result<Cow<'a, [u8]>> {
    validate_resource(path, input)?;
    if let Some(utf8) = input.strip_prefix(b"\xef\xbb\xbf") {
        if let Err(error) = std::str::from_utf8(utf8) {
            let line = utf8[..error.valid_up_to()]
                .split(|byte| *byte == b'\n')
                .count();
            return Err(unsupported(
                path,
                line,
                Feature::SourceEncoding,
                "invalid explicitly marked UTF-8",
            ));
        }
        return Ok(Cow::Borrowed(utf8));
    }
    if input.is_ascii() {
        return Ok(Cow::Borrowed(input));
    }
    let mut decoded = String::with_capacity(input.len());
    let mut line = 1;
    for &byte in input {
        let character = if (0x80..=0x9f).contains(&byte) {
            EXTENDED[usize::from(byte - 0x80)].ok_or_else(|| {
                unsupported(
                    path,
                    line,
                    Feature::SourceEncoding,
                    "undefined Windows-1252 byte",
                )
            })?
        } else {
            char::from(byte)
        };
        decoded.push(character);
        if byte == b'\n' {
            line += 1;
        }
    }
    Ok(Cow::Owned(decoded.into_bytes()))
}

pub(crate) fn reject_special_device(path: &Path, line: usize) -> Result<()> {
    Err(unsupported(
        path,
        line,
        Feature::SpecialDevice,
        "non-ASCII device designator requires a qualified native model",
    ))
}

fn reject(origin: &SourceOrigin, feature: Feature, detail: &str) -> anyhow::Error {
    unsupported(&origin.path, origin.line, feature, detail)
}

fn literal(field: &[u8]) -> Option<f64> {
    let text = std::str::from_utf8(field).ok()?;
    // An expression, parameter name or quantity with an unknown suffix is not
    // proof of a constant. Only use the shared SPICE literal parser here.
    parse_spice_constant(text)
        .ok()
        .filter(|value| value.is_finite())
}

fn assignment(text: &[u8], fields: &[Range<usize>], index: usize) -> Option<Range<usize>> {
    if index + 2 >= fields.len() || text[fields[index + 1].clone()] != *b"=" {
        return None;
    }
    Some(fields[index].start..fields[index + 2].end)
}

fn lower_passive(
    statement: &Statement,
    origin: &SourceOrigin,
    fields: &[Range<usize>],
    edits: &mut Vec<Edit>,
    input: &[u8],
) -> Result<()> {
    let text = &statement.text;
    let designator = text[0].to_ascii_lowercase();
    let mut parallel = false;
    let mut series = false;
    let mut removed = Vec::new();
    let mut index = 4;
    while index < fields.len() {
        let key = &text[fields[index].clone()];
        if let Some(span) = assignment(text, fields, index) {
            let value = &text[fields[index + 2].clone()];
            let is_parallel = key.eq_ignore_ascii_case(b"rpar");
            let is_series = key.eq_ignore_ascii_case(b"rser");
            if key.eq_ignore_ascii_case(b"ic")
                || key.eq_ignore_ascii_case(b"cpar")
                || key.eq_ignore_ascii_case(b"lser")
            {
                return Err(reject(
                    origin,
                    Feature::PassiveParasitics,
                    "initial condition or additional parasitic requires a qualified device contract",
                ));
            }
            if is_parallel || is_series {
                let seen = if is_parallel {
                    &mut parallel
                } else {
                    &mut series
                };
                ensure!(
                    !*seen,
                    "{}:{}: duplicate passive resistance assignment",
                    origin.path.display(),
                    origin.line
                );
                *seen = true;
                // A zero parallel resistance disables QSPICE's implicit shunt;
                // a zero series resistance denotes an ideal series branch.
                if literal(value) != Some(0.0) {
                    return Err(reject(
                        origin,
                        Feature::PassiveParasitics,
                        "nonzero or symbolic Rpar/Rser requires a qualified physical device representation",
                    ));
                }
                removed.extend(statement.source_ranges(span.clone()));
                replace_span(statement, span, b"", edits);
            }
            index += 3;
        } else {
            index += 1;
        }
    }
    if designator == b'l' && !parallel {
        return Err(reject(
            origin,
            Feature::PassiveParasitics,
            "implicit inductor Rpar depends on analysis; specify Rpar=0 for an ideal inductor",
        ));
    }
    for (plus, code) in &statement.continuations {
        if input[code.clone()]
            .iter()
            .enumerate()
            .all(|(offset, byte)| {
                byte.is_ascii_whitespace()
                    || removed
                        .iter()
                        .any(|range| range.contains(&(code.start + offset)))
            })
        {
            edits.push((*plus..*plus + 1, vec![b'*']));
        }
    }
    Ok(())
}

fn lower_tran(
    statement: &Statement,
    origin: &SourceOrigin,
    fields: &[Range<usize>],
    edits: &mut Vec<Edit>,
) -> Result<()> {
    let text = &statement.text;
    let mut args = &fields[1..];
    if args
        .last()
        .is_some_and(|span| text[span.clone()].eq_ignore_ascii_case(b"uic"))
    {
        args = &args[..args.len() - 1];
    }
    let [interval, duration, start, maximum] = args else {
        return Err(reject(
            origin,
            Feature::TransientTiming,
            "explicit step, stop, zero start and positive maxstep required until simulator defaults are qualified",
        ));
    };
    let values = [interval, duration, start, maximum].map(|span| literal(&text[span.clone()]));
    let [
        Some(interval_value),
        Some(duration_value),
        Some(start_value),
        Some(maximum_value),
    ] = values
    else {
        return Err(reject(
            origin,
            Feature::TransientTiming,
            "symbolic time fields need analysis-specific parameter binding",
        ));
    };
    let end = interval_value.max(duration_value);
    if interval_value < 0.0
        || duration_value < 0.0
        || end <= 0.0
        || start_value != 0.0
        || maximum_value <= 0.0
    {
        return Err(reject(
            origin,
            Feature::TransientTiming,
            "time window or timestep is not a qualified native equivalent",
        ));
    }
    // QSPICE ignores the first interval except that it can extend the stop
    // time. Native SPICE requires a positive output interval.
    if interval_value > duration_value {
        replace_span(statement, duration.clone(), &text[interval.clone()], edits);
    }
    let native_interval = end.min(maximum_value);
    replace_span(
        statement,
        interval.clone(),
        native_interval.to_string().as_bytes(),
        edits,
    );
    Ok(())
}

fn check_model(statement: &Statement, origin: &SourceOrigin) -> Result<()> {
    let model = parse_model_card(std::str::from_utf8(&statement.text)?, origin.line, &[])?;
    if model.family.eq_ignore_ascii_case("d")
        && model.parameters.iter().any(|param| {
            ["ron", "roff", "vfwd", "vrev", "ilimit", "revilimit"]
                .iter()
                .any(|key| param.name.eq_ignore_ascii_case(key))
        })
    {
        return Err(reject(
            origin,
            Feature::ModelLaw,
            "idealized diode requires a native piecewise conduction law",
        ));
    }
    if model.family.eq_ignore_ascii_case("sw") {
        for parameter in &model.parameters {
            if parameter.name.eq_ignore_ascii_case("vh")
                && parameter
                    .expression
                    .as_deref()
                    .and_then(|value| literal(value.as_bytes()))
                    .is_none_or(|value| value < 0.0)
            {
                return Err(reject(
                    origin,
                    Feature::ModelLaw,
                    "negative or symbolic switch hysteresis requires a qualified conductance law",
                ));
            }
            if parameter.name.eq_ignore_ascii_case("level")
                && parameter
                    .expression
                    .as_deref()
                    .and_then(|value| literal(value.as_bytes()))
                    != Some(1.0)
            {
                return Err(reject(
                    origin,
                    Feature::ModelLaw,
                    "switch model level requires a qualified conductance law",
                ));
            }
        }
    }
    Ok(())
}

fn lower_statement(
    statement: &Statement,
    origin: &SourceOrigin,
    edits: &mut Vec<Edit>,
    input: &[u8],
) -> Result<()> {
    let text = &statement.text;
    let fields = tokens(text)?;
    let Some(first) = fields.first() else {
        return Ok(());
    };
    let name = text[first.clone()].to_ascii_lowercase();
    match name.as_slice() {
        b".libpath" | b".control" | b".endc" | b".shell" | b".load" | b".hdl" | b".plugin"
        | b".dll" => Err(reject(
            origin,
            Feature::Directive,
            "external execution or source-defined resource search is not native; use explicit caller search directories for text resources",
        )),
        b".model" => check_model(statement, origin),
        b".step" | b".noise" | b".four" => Err(reject(
            origin,
            Feature::Directive,
            "analysis or output directive needs source-specific qualification",
        )),
        b".tran" => lower_tran(statement, origin, &fields, edits),
        [b'l' | b'c', ..] => lower_passive(statement, origin, &fields, edits, input),
        [b'a' | b'u' | b'y' | b'n' | b'o' | b'@', ..] => Err(reject(
            origin,
            Feature::SpecialDevice,
            "device class has no qualified QSPICE-to-native binding",
        )),
        _ => Ok(()),
    }
}

/// Apply the source-specific pass to a linked graph. Rewrites preserve the
/// physical line count, so every subsequent native pass retains provenance.
/// # Errors
/// Unsupported source semantics, malformed statements or invalid source map.
pub fn lower_linked(linked: &LinkedSource, kind: RootKind) -> Result<Option<Vec<u8>>> {
    let input = &linked.bytes;
    let title_len = if kind == RootKind::Deck {
        input
            .split_inclusive(|byte| *byte == b'\n')
            .next()
            .context("missing QSPICE deck title")?
            .len()
    } else {
        0
    };
    let body = &input[title_len..];
    let mut numeric_edits = Vec::new();
    for statement in statements(body)? {
        let origin = linked
            .origin(statement.line + usize::from(title_len != 0))
            .context("missing QSPICE source location")?;
        numeric::rewrite(&statement, origin, &mut numeric_edits)?;
    }
    let lowered = apply_edits(body, numeric_edits);
    let body = lowered.as_deref().unwrap_or(body);
    let mut edits = Vec::new();
    for statement in statements(body)? {
        let line = statement.line + usize::from(title_len != 0);
        let origin = linked
            .origin(line)
            .context("missing QSPICE source location")?;
        lower_statement(&statement, origin, &mut edits, body)
            .with_context(|| format!("QSPICE source {}:{}", origin.path.display(), origin.line))?;
    }
    let final_body = apply_edits(body, edits).or(lowered);
    Ok(final_body.map(|body| {
        let mut bytes = input[..title_len].to_vec();
        bytes.extend_from_slice(&body);
        bytes
    }))
}
