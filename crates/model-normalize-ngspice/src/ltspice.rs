//! Source-specific `LTspice` normalization, before native execution.
//!
//! Preserve physical source mapping; generated helper definitions inherit the
//! line of their first use. Native decks do not receive these dialect rewrites.

use crate::{
    MappedNormalization, NormalizationPass, expression_start, fresh_identifier,
    lexical::{self, Edit},
    source_identifiers,
};
use anyhow::{Context, Result, ensure};
use std::collections::BTreeMap;

const HELPERS: &[(&[u8], &str)] = &[
    (b"if", "(c,a,b) {ternary_fcn(c>0.5,a,b)}"),
    (b"limit", "(x,a,b) {min(max(x,min(a,b)),max(a,b))}"),
    (b"pwr", "(x,y) {pow(abs(x),y)}"),
    (b"pwrs", "(x,y) {sgn(x)*pow(abs(x),y)}"),
    (b"inv", "(x) {ternary_fcn(x>0.5,0,1)}"),
    (b"square", "(x) {x*x}"),
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
                b"pow",
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
    for model in crate::model_card::parse_model_cards(&statement.text)? {
        if !model.family.eq_ignore_ascii_case("d") {
            continue;
        }
        for field in &model.parameters {
            ensure!(
                ![
                    "ron",
                    "roff",
                    "rrev",
                    "vfwd",
                    "vrev",
                    "epsilon",
                    "revepsilon",
                    "ilimit",
                    "revilimit"
                ]
                .iter()
                .any(|name| field.name.eq_ignore_ascii_case(name)),
                "line {}: LTspice idealized diode parameter {} requires typed model lowering",
                statement.line,
                field.name
            );
        }
    }
    Ok(())
}

fn validate_expression(statement: &lexical::Statement, start: usize) -> Result<()> {
    for name in [
        b"table".as_slice(),
        b"uplim",
        b"dnlim",
        b"idt",
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

struct RewritePlan {
    edits: Vec<Edit>,
    helpers: BTreeMap<usize, (Vec<u8>, usize)>,
    flags_changed: bool,
    annotation_changed: bool,
}

fn rewrite_intrinsics(
    statement: &lexical::Statement,
    start: usize,
    title_lines: usize,
    used: &mut std::collections::BTreeSet<Vec<u8>>,
    helpers: &mut BTreeMap<usize, (Vec<u8>, usize)>,
    edits: &mut Vec<Edit>,
) -> Result<()> {
    let text = &statement.text;
    for (index, (name, _)) in HELPERS.iter().enumerate() {
        let spans = lexical::function_names(text, name, start);
        if spans.is_empty() {
            continue;
        }
        let (identifier, _) = if let Some(helper) = helpers.get(&index) {
            helper
        } else {
            let mut base = b"__lt_intrinsic_".to_vec();
            base.extend_from_slice(name);
            let identifier =
                fresh_identifier(used, &base).context("LTspice intrinsic identifier overflow")?;
            helpers
                .entry(index)
                .or_insert((identifier, statement.line + title_lines))
        };
        for span in spans {
            lexical::replace_span(statement, span, identifier, edits);
        }
    }
    Ok(())
}

fn plan_rewrites(bytes: &[u8], body: &[u8], title_lines: usize) -> Result<RewritePlan> {
    let mut used = source_identifiers(bytes);
    let mut edits = Vec::new();
    let mut helpers = BTreeMap::<usize, (Vec<u8>, usize)>::new();
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
    let bytes = &input.model.bytes;
    let title = if kind == crate::linked_source::RootKind::Deck {
        bytes
            .split_inclusive(|byte| *byte == b'\n')
            .next()
            .context("missing LTspice title")?
            .len()
    } else {
        0
    };
    let body = &bytes[title..];
    let title_lines = usize::from(title != 0);
    let plan = plan_rewrites(bytes, body, title_lines)?;
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
    for (index, (identifier, origin)) in &helpers {
        output.extend_from_slice(b".func ");
        output.extend_from_slice(identifier);
        output.extend_from_slice(HELPERS[*index].1.as_bytes());
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
    if !helpers.is_empty() {
        input
            .model
            .passes
            .push(NormalizationPass::LtspiceIntrinsics);
    }
    Ok(input)
}
