//! Source-specific `LTspice` normalization, before native execution.
//!
//! Preserve physical source mapping; generated helper definitions inherit the
//! line of their first use. Native decks do not receive these dialect rewrites.

mod constants;
mod functions;
mod operators;
mod sources;
mod table;

use crate::{
    MappedNormalization, NormalizationPass, expression_start, fresh_identifier,
    lexical::{self, Edit},
    source_identifiers,
};
use anyhow::{Context, Result, ensure};
use std::collections::{BTreeMap, BTreeSet};

const HELPERS: &[(&[u8], &str)] = &[
    (b"if", "(c,a,b) {ternary_fcn(c>0.5,a,b)}"),
    (b"limit", "(x,a,b) {min(max(x,min(a,b)),max(a,b))}"),
    (
        b"pwr",
        "(x,y) {ternary_fcn(x==0,ternary_fcn(y==0,1,0),pow(abs(x),y))}",
    ),
    (
        b"pwrs",
        "(x,y) {ternary_fcn(x==0,ternary_fcn(y==0,1,0),ternary_fcn(x<0,-1,1)*pow(abs(x),y))}",
    ),
    (b"inv", "(x) {ternary_fcn(x>0.5,0,1)}"),
    (b"square", "(x) {x*x}"),
    (b"sqrt", "(x) {sqrt(max(x,0))}"),
    (
        b"round",
        "(x) {ternary_fcn(x<0,-floor(-x+0.5),floor(x+0.5))}",
    ),
    (b"int", "(x) {ternary_fcn(x<0,ceil(x),floor(x))}"),
    (b"u", "(x) {ternary_fcn(x>0,1,0)}"),
    (
        b"mod",
        "(x,y) {x-y*ternary_fcn(x/y>=0,floor(x/y),ceil(x/y))}",
    ),
    (b"buf", "(x) {ternary_fcn(x>0.5,1,0)}"),
    (b"xor", "(x,y) {ternary_fcn((x>0.5)!=(y>0.5),1,0)}"),
    (
        b"pow",
        "(x,y) {ternary_fcn(x==0,ternary_fcn(y==0,1,0),ternary_fcn(x<0,ternary_fcn(y==floor(y),pow(-1,y)*pow(abs(x),y),0),pow(x,y)))}",
    ),
    (
        b"uplim",
        "(x,y,z) {ternary_fcn(z==0,min(x,y),ternary_fcn(x<=y-z,x,y-z*exp((y-z-x)/z)))}",
    ),
    (
        b"dnlim",
        "(x,y,z) {ternary_fcn(z==0,max(x,y),ternary_fcn(x>=y+z,x,y+z*exp((x-y-z)/z)))}",
    ),
];

fn resistor_flags(statement: &lexical::Statement, edits: &mut Vec<Edit>) -> Result<()> {
    let text = &statement.text;
    let fields = lexical::tokens(text)?;
    let mut flag = None;
    let mut noisy = false;
    for (index, span) in fields.iter().enumerate().skip(4) {
        if text[fields[index - 1].clone()] == *b"=" {
            continue;
        }
        let field = &text[span.clone()];
        if field.eq_ignore_ascii_case(b"noisy") {
            noisy = true;
        }
        if field.eq_ignore_ascii_case(b"noiseless") {
            ensure!(
                fields
                    .get(index + 1)
                    .is_none_or(|next| text[next.clone()] != *b"="),
                "line {}: LTspice noiseless is a flag, not an assignment",
                statement.line
            );
            ensure!(
                flag.replace(span.clone()).is_none(),
                "line {}: duplicate LTspice noiseless flag",
                statement.line
            );
        }
    }
    if let Some(span) = flag {
        ensure!(
            !noisy,
            "line {}: conflicting noisy and noiseless resistor settings",
            statement.line
        );
        lexical::replace_span(statement, span, b"noisy=0", edits);
    }
    Ok(())
}

fn reject_shadowed_helpers(statement: &lexical::Statement) -> Result<()> {
    let fields = lexical::tokens(&statement.text)?;
    let Some(name) = fields.get(1) else {
        return Ok(());
    };
    let name = &statement.text[name.clone()];
    let name = name.split(|byte| *byte == b'(').next().unwrap_or_default();
    ensure!(
        !HELPERS
            .iter()
            .any(|(helper, _)| name.eq_ignore_ascii_case(helper))
            && ![
                b"abs".as_slice(),
                b"table",
                b"floor",
                b"ceil",
                b"exp",
                b"min",
                b"max",
                b"sgn",
                b"ternary_fcn"
            ]
            .iter()
            .any(|native| name.eq_ignore_ascii_case(native)),
        "line {}: LTspice function definition shadows a dialect intrinsic",
        statement.line
    );
    Ok(())
}

/// Reject known model forms whose native counterpart has different physics.
fn validate_model(statement: &lexical::Statement) -> Result<()> {
    use crate::model_card::parse_spice_constant;
    for model in crate::model_card::parse_model_cards(&statement.text)? {
        let family = model.family.to_ascii_lowercase();
        for field in &model.parameters {
            let name = field.name.to_ascii_lowercase();
            match family.as_str() {
                "d" => ensure!(
                    !matches!(
                        name.as_str(),
                        "ron"
                            | "roff"
                            | "rrev"
                            | "vfwd"
                            | "vrev"
                            | "epsilon"
                            | "revepsilon"
                            | "ilimit"
                            | "revilimit"
                    ),
                    "line {}: LTspice idealized diode parameter {} requires typed model lowering",
                    statement.line,
                    field.name
                ),
                "sw" => {
                    let value = field
                        .expression
                        .as_deref()
                        .and_then(|value| parse_spice_constant(value).ok());
                    ensure!(
                        match name.as_str() {
                            "vh" => value.is_some_and(|value| value >= 0.),
                            "level" =>
                                value.is_some_and(|value| value.to_bits() == 1_f64.to_bits()),
                            "lser" | "vser" | "ilimit" => false,
                            _ => true,
                        },
                        "line {}: LTspice switch parameter {} requires typed switch lowering",
                        statement.line,
                        field.name
                    );
                }
                "c" | "cap" | "l" | "ind" => ensure!(
                    !matches!(
                        name.as_str(),
                        "rser" | "rpar" | "cpar" | "lser" | "rlshunt" | "ic"
                    ),
                    "line {}: LTspice passive model parameter {} requires typed lowering",
                    statement.line,
                    field.name
                ),
                _ => {}
            }
        }
    }
    Ok(())
}

fn validate_expression(statement: &lexical::Statement, start: usize) -> Result<()> {
    for name in [
        b"idt".as_slice(),
        b"idtmod",
        b"ddt",
        b"delay",
        b"absdelay",
        b"white",
        b"random",
        b"rand",
        b"gauss",
        b"flat",
    ] {
        ensure!(
            lexical::function_names(&statement.text, name, start).is_empty(),
            "line {}: LTspice function {} requires dialect-specific lowering",
            statement.line,
            String::from_utf8_lossy(name)
        );
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Helper {
    Fixed(usize),
    Table(usize, bool),
}

impl Helper {
    fn base(self) -> Vec<u8> {
        match self {
            Self::Fixed(index) => {
                let mut base = b"__lt_intrinsic_".to_vec();
                base.extend_from_slice(HELPERS[index].0);
                base
            }
            Self::Table(points, static_phase) => {
                let phase = if static_phase { "param" } else { "runtime" };
                format!("__lt_table_{phase}_{points}").into_bytes()
            }
        }
    }

    fn definition(self) -> Result<String> {
        match self {
            Self::Fixed(index) => Ok(HELPERS[index].1.to_owned()),
            Self::Table(points, static_phase) => table::definition(points, static_phase),
        }
    }
}

type HelperMap = BTreeMap<Helper, (Vec<u8>, usize)>;

struct RewritePlan {
    edits: Vec<Edit>,
    helpers: HelperMap,
    flags_changed: bool,
    annotation_changed: bool,
}

fn rewrite_intrinsics(
    statement: &lexical::Statement,
    start: usize,
    title_lines: usize,
    used: &mut std::collections::BTreeSet<Vec<u8>>,
    helpers: &mut HelperMap,
    edits: &mut Vec<Edit>,
    static_macros: &BTreeSet<Vec<u8>>,
) -> Result<()> {
    let text = &statement.text;
    for (index, (name, _)) in HELPERS.iter().enumerate() {
        let spans = lexical::function_names(text, name, start);
        if spans.is_empty() {
            continue;
        }
        let helper = Helper::Fixed(index);
        let identifier = helper_identifier(helper, statement.line + title_lines, used, helpers)?;
        for span in spans {
            lexical::replace_span(statement, span, identifier, edits);
        }
    }
    let static_macro = text
        .split(u8::is_ascii_whitespace)
        .next()
        .is_some_and(|keyword| keyword.eq_ignore_ascii_case(b".func"))
        && static_macros.contains(&text[functions::name(text)?].to_ascii_uppercase());
    for span in lexical::function_names(text, b"table", start) {
        let points = table::point_count(text, span.clone())?;
        let identifier = helper_identifier(
            Helper::Table(
                points,
                static_macro || operators::static_table(text, start, span.start)?,
            ),
            statement.line + title_lines,
            used,
            helpers,
        )?;
        lexical::replace_span(statement, span, identifier, edits);
    }
    Ok(())
}

fn helper_identifier<'a>(
    helper: Helper,
    origin: usize,
    used: &mut std::collections::BTreeSet<Vec<u8>>,
    helpers: &'a mut HelperMap,
) -> Result<&'a [u8]> {
    use std::collections::btree_map::Entry;
    let (identifier, _) = match helpers.entry(helper) {
        Entry::Occupied(entry) => entry.into_mut(),
        Entry::Vacant(entry) => {
            let identifier = fresh_identifier(used, &helper.base())
                .context("LTspice intrinsic identifier overflow")?;
            entry.insert((identifier, origin))
        }
    };
    Ok(identifier)
}

fn plan_rewrites(
    bytes: &[u8],
    body: &[u8],
    title_lines: usize,
    static_macros: &BTreeSet<Vec<u8>>,
) -> Result<RewritePlan> {
    let mut used = source_identifiers(bytes);
    let mut edits = Vec::new();
    let mut helpers = HelperMap::new();
    let mut flags_changed = false;
    let mut annotation_changed = false;
    let mut control = false;
    for statement in lexical::statements(body)? {
        let text = &statement.text;
        let first = text
            .split(u8::is_ascii_whitespace)
            .next()
            .unwrap_or_default();
        if first.eq_ignore_ascii_case(b".control") {
            control = true;
        } else if first.eq_ignore_ascii_case(b".endc") {
            control = false;
            continue;
        }
        if control {
            continue;
        }
        if first.eq_ignore_ascii_case(b".backanno") {
            ensure!(
                text.eq_ignore_ascii_case(b".backanno"),
                "line {}: unexpected .backanno operands",
                statement.line
            );
            lexical::replace_span(&statement, 0..1, b"*", &mut edits);
            annotation_changed = true;
            continue;
        }
        if first
            .first()
            .is_some_and(|byte| byte.eq_ignore_ascii_case(&b'r'))
        {
            let before = edits.len();
            resistor_flags(&statement, &mut edits)?;
            flags_changed |= before != edits.len();
        }
        if first.eq_ignore_ascii_case(b".func") {
            reject_shadowed_helpers(&statement)?;
        }
        if first.eq_ignore_ascii_case(b".model") {
            validate_model(&statement)?;
        }
        let Some(start) = expression_start(text, first)? else {
            continue;
        };
        validate_expression(&statement, start)?;
        rewrite_intrinsics(
            &statement,
            start,
            title_lines,
            &mut used,
            &mut helpers,
            &mut edits,
            static_macros,
        )?;
    }
    Ok(RewritePlan {
        edits,
        helpers,
        flags_changed,
        annotation_changed,
    })
}

/// Apply documented source-specific rewrites to a linked and normalized
/// artifact, retaining the composed physical line map.
///
/// # Errors
/// Malformed statements, conflicting flags, or ambiguous intrinsic definitions.
pub fn normalize(
    mut input: MappedNormalization,
    kind: crate::linked_source::RootKind,
) -> Result<MappedNormalization> {
    let title = if kind == crate::linked_source::RootKind::Deck {
        input
            .model
            .bytes
            .split_inclusive(|byte| *byte == b'\n')
            .next()
            .context("missing LTspice title")?
            .len()
    } else {
        0
    };
    if let Some(lowered) = sources::rewrite(&input.model.bytes[title..])? {
        input.model.bytes.truncate(title);
        input.model.bytes.extend_from_slice(&lowered);
        input
            .model
            .passes
            .push(NormalizationPass::LtspiceSourceWaveforms);
    }
    if let Some(lowered) = operators::rewrite(&input.model.bytes[title..])? {
        input.model.bytes.truncate(title);
        input.model.bytes.extend_from_slice(&lowered);
        input.model.passes.push(NormalizationPass::LtspiceOperators);
    }
    let title_lines = usize::from(title != 0);
    let mut static_macros = BTreeSet::new();
    if let Some(specialization) = functions::specialize(
        &input.model.bytes,
        &input.model.bytes[title..],
        &input.input_lines[title_lines..],
    )? {
        input.model.bytes.truncate(title);
        input.model.bytes.extend_from_slice(&specialization.bytes);
        input.input_lines.truncate(title_lines);
        input.input_lines.extend(specialization.origins);
        static_macros = specialization.static_macros;
        input
            .model
            .passes
            .push(NormalizationPass::LtspiceIntrinsics);
    }
    let bytes = &input.model.bytes;
    let body = &bytes[title..];
    let plan = plan_rewrites(bytes, body, title_lines, &static_macros)?;
    let RewritePlan {
        edits,
        helpers,
        flags_changed,
        annotation_changed,
    } = plan;
    let Some(rewritten) = lexical::apply_edits(body, edits) else {
        return Ok(input);
    };
    let mut output = bytes[..title].to_vec();
    let mut origins = input.input_lines[..title_lines].to_vec();
    for (helper, (identifier, origin)) in &helpers {
        output.extend_from_slice(b".func ");
        output.extend_from_slice(identifier);
        output.extend_from_slice(helper.definition()?.as_bytes());
        output.push(b'\n');
        origins.push(
            *input
                .input_lines
                .get(origin - 1)
                .context("missing LTspice helper origin")?,
        );
    }
    output.extend_from_slice(&rewritten);
    origins.extend_from_slice(&input.input_lines[title_lines..]);
    ensure!(
        origins.len() == output.split_inclusive(|byte| *byte == b'\n').count(),
        "LTspice normalization source mapping is inconsistent"
    );
    input.model.bytes = output;
    input.input_lines = origins;
    if annotation_changed {
        input
            .model
            .passes
            .push(NormalizationPass::LtspiceBackannotation);
    }
    if flags_changed {
        input
            .model
            .passes
            .push(NormalizationPass::LtspiceNoiselessResistor);
    }
    if !helpers.is_empty()
        && !input
            .model
            .passes
            .contains(&NormalizationPass::LtspiceIntrinsics)
    {
        input
            .model
            .passes
            .push(NormalizationPass::LtspiceIntrinsics);
    }
    Ok(input)
}
