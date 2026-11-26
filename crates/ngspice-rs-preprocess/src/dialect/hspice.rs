//! Standalone HSPICE resource admission and native artifact preparation.
//! HSPICE compatibility is local to source resources; no simulator global
//! compatibility flag is set by this producer.
use super::{SourceExpansion, analysis_plan};
use crate::{
    AdmittedSource, Cancellation, NativeProjection, Options, PreparedBundle, Request, Resource,
    SourceSession, WorkBudget, native_bundle,
};
use anyhow::{Context, Result, bail, ensure};
use model_normalize_ngspice::{
    linked_source::RootKind, model_card::parse_parameter_fields, statement_token_spans,
};
use model_preprocess_simplis::{
    lower_spice_expression,
    source_dialect::{DialectContext, SourceKind, SourceLanguage},
};
use std::fmt::Write as _;

mod analysis;
mod bsim4_catalog;
mod hierarchy;
mod models;
mod mos;

pub(super) const HSPICE: DialectContext = DialectContext {
    language: SourceLanguage::Hspice,
    separator: None,
};

#[derive(Default)]
struct DeckOptions {
    nominal_temperature: bool,
    temperature: bool,
    depth: usize,
    parhier: Option<hierarchy::Precedence>,
    xmult_in_exp: Option<bool>,
    mos: mos::Options,
}

fn normalize_options(text: &str, state: &mut DeckOptions, budget: &WorkBudget) -> Result<String> {
    let tail = text
        .split_once(char::is_whitespace)
        .map_or("", |(_, tail)| tail);
    let mut native = String::from(".options");
    for field in parse_parameter_fields(tail, &[])? {
        let key = field.name.to_ascii_uppercase();
        match key.as_str() {
            "PARHIER" => {
                let value = field
                    .expression
                    .as_deref()
                    .context("PARHIER requires a value")?;
                ensure!(
                    value.eq_ignore_ascii_case("LOCAL") || value.eq_ignore_ascii_case("GLOBAL"),
                    "invalid HSPICE PARHIER value {value}"
                );
                let precedence = if value.eq_ignore_ascii_case("LOCAL") {
                    hierarchy::Precedence::Local
                } else {
                    hierarchy::Precedence::Global
                };
                ensure!(
                    state.parhier.is_none_or(|old| old == precedence),
                    "conflicting HSPICE PARHIER options"
                );
                state.parhier = Some(precedence);
                continue;
            }
            "XMULT_IN_EXP" => {
                let value = field
                    .expression
                    .as_deref()
                    .context("XMULT_IN_EXP requires YES or NO")?;
                let enabled = if value.eq_ignore_ascii_case("YES") {
                    true
                } else if value.eq_ignore_ascii_case("NO") {
                    false
                } else {
                    bail!("invalid HSPICE XMULT_IN_EXP value {value}");
                };
                ensure!(
                    state.xmult_in_exp.is_none_or(|old| old == enabled),
                    "conflicting HSPICE XMULT_IN_EXP options"
                );
                state.xmult_in_exp = Some(enabled);
                continue;
            }
            "DCAP" => continue,
            "WNFLAG" | "WL" | "DEFL" | "DEFW" | "DEFAD" | "DEFAS" | "DEFNRD" | "DEFNRS"
            | "DEFPD" | "DEFPS" => {
                state.mos.set(
                    &key,
                    field
                        .expression
                        .as_deref()
                        .or_else(|| (key == "WL").then_some("1"))
                        .context("missing MOS option value")?,
                    budget,
                )?;
                continue;
            }
            "TNOM" => state.nominal_temperature = true,
            "TEMP" => state.temperature = true,
            "ABSTOL" | "VNTOL" | "RELTOL" | "GMIN" | "ITL1" | "ITL2" | "ITL4" | "CHGTOL" => (),
            _ => bail!("unqualified HSPICE option {}", field.name),
        }
        write!(native, " {}", field.name)?;
        if let Some(value) = field.expression {
            write!(native, "={value}")?;
        } else {
            bail!("HSPICE numeric option {} requires a value", field.name);
        }
    }
    if native == ".options" {
        Ok(format!("* consumed {text}"))
    } else {
        Ok(native)
    }
}

fn lower_source(text: &str) -> Result<String> {
    let ranges = statement_token_spans(text)?;
    let Some(first) = ranges.first() else {
        return Ok(text.to_owned());
    };
    let kind = text[first.clone()]
        .as_bytes()
        .first()
        .map(u8::to_ascii_uppercase);
    let Some(mode) = ranges.get(3) else {
        return Ok(text.to_owned());
    };
    let mode_text = &text[mode.clone()];
    let linear = match kind {
        Some(b'E') => "VCVS",
        Some(b'G') => "VCCS",
        Some(b'F') => "CCCS",
        Some(b'H') => "CCVS",
        _ => return Ok(text.to_owned()),
    };
    if mode_text.eq_ignore_ascii_case(linear) {
        let mut lowered = text.to_owned();
        lowered.replace_range(mode.clone(), "");
        return Ok(lowered);
    }
    let behavioral = match kind {
        Some(b'E') => "VOL",
        Some(b'G') => "CUR",
        _ => return Ok(text.to_owned()),
    };
    if mode_text.eq_ignore_ascii_case(behavioral) {
        ensure!(
            ranges
                .get(4)
                .is_some_and(|range| &text[range.clone()] == "="),
            "HSPICE {behavioral} requires an expression assignment"
        );
        let mut lowered = text.to_owned();
        lowered.replace_range(mode.clone(), "VALUE");
        return Ok(lowered);
    }
    Ok(text.to_owned())
}

/// Resolve the structural prefix of a dependent source. POLY dimensions
/// and electrical control names are not parameter expressions.
fn controlled_value_start(
    text: &str,
    ranges: &[std::ops::Range<usize>],
    kind: u8,
) -> Result<usize> {
    let token = |index: usize| ranges.get(index).map_or("", |range| &text[range.clone()]);
    let mode = token(3).to_ascii_uppercase();
    if mode.starts_with("POLY(") || mode == "POLY" {
        let (dimension, first) = if mode == "POLY" {
            (token(4).trim_matches(['(', ')']), 5_usize)
        } else {
            (
                mode.strip_prefix("POLY(")
                    .and_then(|tail| tail.strip_suffix(')'))
                    .context("invalid HSPICE POLY dimension")?,
                4_usize,
            )
        };
        let dimension = dimension
            .parse::<usize>()
            .context("HSPICE POLY requires a constant positive dimension")?;
        ensure!(dimension > 0, "HSPICE POLY requires a positive dimension");
        let controls = dimension
            .checked_mul(if matches!(kind, b'E' | b'G') { 2 } else { 1 })
            .and_then(|count| first.checked_add(count))
            .context("HSPICE POLY control count overflow")?;
        return Ok(ranges
            .get(controls)
            .context("HSPICE POLY lacks controls or coefficients")?
            .start);
    }
    if mode == "VALUE" || mode == "VOL" || mode == "CUR" {
        return Ok(ranges
            .get(4)
            .context("missing HSPICE controlled expression")?
            .start);
    }
    ensure!(
        ![
            "TABLE", "LAPLACE", "FREQ", "VCR", "VCCAP", "CCCR", "CCCAP", "DELAY"
        ]
        .iter()
        .any(|name| mode == *name
            || mode.starts_with(&format!("{name}("))
            || mode.starts_with(&format!("{name}{{"))),
        "unqualified HSPICE controlled transfer {mode}"
    );
    let index = if matches!(kind, b'E' | b'G') { 5 } else { 4 };
    Ok(ranges
        .get(index)
        .context("missing HSPICE controlled source coefficient")?
        .start)
}

/// Identify electrical names before lowering values. In particular, a node
/// named 1x is legal and is not a one-mega engineering literal.
fn map_values(
    text: &str,
    limit: usize,
    mut map: impl FnMut(&str, usize) -> Result<String>,
) -> Result<String> {
    let ranges = statement_token_spans(text)?;
    let command = analysis_plan::command(text);
    let start = |index: usize| ranges.get(index).map_or(text.len(), |range| range.start);
    let assignment_start = || {
        ranges
            .iter()
            .position(|range| &text[range.clone()] == "=")
            .map_or(text.len(), |index| start(index.saturating_sub(1)))
    };
    let offset = match command.as_str() {
        ".PARAM" | ".TEMP" | ".OPTIONS" | ".OPTION" | ".OPT" | ".IC" | ".NODESET" | ".TRAN" => {
            start(1)
        }
        ".FUNC" => ranges.get(1).context("missing function declaration")?.end,
        ".AC" => start(2),
        ".DC" => {
            // A second sweep has its own structural source identifier.
            let mut output = String::new();
            let mut previous = 0;
            for index in [2, 6] {
                if index >= ranges.len() {
                    break;
                }
                let begin = start(index);
                let end = start(index + 3);
                output.push_str(&text[previous..begin]);
                output.push_str(&map(&text[begin..end], limit.saturating_sub(output.len()))?);
                previous = end;
            }
            output.push_str(&text[previous..]);
            ensure!(
                output.len() <= limit,
                "source expression byte budget exceeded"
            );
            return Ok(output);
        }
        ".SUBCKT" => assignment_start(),
        _ if command.starts_with('.') => return Ok(text.to_owned()),
        _ => match command.as_bytes().first() {
            Some(b'X') => assignment_start(),
            Some(b'M') => start(6),
            Some(b'R' | b'C' | b'L' | b'V' | b'I' | b'B') => start(3),
            Some(kind @ (b'E'..=b'H')) => controlled_value_start(text, &ranges, *kind)?,
            Some(b'D') => start(4),
            _ => text.len(),
        },
    };
    let mut output = text[..offset].to_owned();
    output.push_str(&map(&text[offset..], limit.saturating_sub(offset))?);
    ensure!(
        output.len() <= limit,
        "source expression byte budget exceeded"
    );
    Ok(output)
}

fn lower_card(
    text: &str,
    context: DialectContext,
    state: &mut DeckOptions,
    limit: usize,
    budget: &WorkBudget,
) -> Result<String> {
    let text = text.trim();
    if text.is_empty() || text.starts_with('*') {
        return Ok(text.to_owned());
    }
    let command = analysis_plan::command(text);
    let hspice = context.language == SourceLanguage::Hspice;
    let fields = statement_token_spans(text)?;
    if command == ".SUBCKT" {
        state.depth += 1;
        budget.check_depth(state.depth)?;
    } else if command == ".ENDS" {
        state.depth = state.depth.checked_sub(1).context("unmatched .ENDS")?;
    }
    if state.depth > 0
        && (matches!(command.as_str(), ".FUNC" | ".OPTION" | ".OPTIONS" | ".OPT")
            || matches!(command.as_str(), ".OP" | ".DC" | ".AC" | ".TRAN" | ".TEMP"))
    {
        bail!("HSPICE scoped parameters/options require hierarchy lowering");
    }
    if !command.starts_with('.') {
        let kind = command.as_bytes()[0];
        ensure!(
            matches!(
                kind,
                b'R' | b'C'
                    | b'L'
                    | b'V'
                    | b'I'
                    | b'E'
                    | b'G'
                    | b'F'
                    | b'H'
                    | b'B'
                    | b'X'
                    | b'D'
                    | b'M'
            ),
            "HSPICE device {command} requires qualified model/device lowering"
        );
        if kind == b'D' {
            for field in fields.iter().skip(4) {
                let token = &text[field.clone()];
                ensure!(
                    !["W", "L", "LM", "LP", "WM", "WP", "THERMAL"]
                        .iter()
                        .any(|name| token.eq_ignore_ascii_case(name)),
                    "geometric or thermal HSPICE diode instance is not qualified"
                );
            }
        }
    }
    let mut source = match command.as_str() {
        ".OPTION" | ".OPTIONS" | ".OPT" if hspice => normalize_options(text, state, budget)?,
        ".TEMP" => {
            ensure!(
                fields.len() == 2,
                "HSPICE multiple-temperature sweep requires analysis lowering"
            );
            state.temperature = true;
            text.to_owned()
        }
        ".PARAM" | ".FUNC" | ".SUBCKT" | ".ENDS" | ".GLOBAL" | ".END" | ".OP" | ".DC" | ".AC"
        | ".TRAN" | ".IC" | ".NODESET" => text.to_owned(),
        ".MODEL" => bail!("model selection requires a qualified native mapping"),
        _ if command.starts_with('.') => bail!("unqualified HSPICE directive {command}"),
        _ => lower_source(text)?,
    };
    source = map_values(&source, limit, |value, remaining| {
        lower_spice_expression(value, context, remaining, budget)
    })?;
    if matches!(command.as_str(), ".OP" | ".DC" | ".AC" | ".TRAN") {
        source = analysis::lower(&source)?;
    }
    Ok(source)
}

fn depletion_mode(
    statements: &[model_preprocess_simplis::source_dialect::ScopedStatement],
    budget: &WorkBudget,
) -> Result<u8> {
    let mut mode = None;
    for statement in statements {
        budget.charge(Resource::Statements, 1)?;
        budget.charge(Resource::ScannedBytes, statement.text.len())?;
        if !matches!(
            analysis_plan::command(&statement.text).as_str(),
            ".OPTION" | ".OPTIONS" | ".OPT"
        ) {
            continue;
        }
        let tail = statement
            .text
            .split_once(char::is_whitespace)
            .map_or("", |(_, tail)| tail);
        for field in parse_parameter_fields(tail, &[])? {
            if field.name.eq_ignore_ascii_case("DCAP") {
                let value = match field.expression.as_deref() {
                    Some("1") => 1,
                    Some("2") => 2,
                    _ => bail!("unqualified HSPICE DCAP selection"),
                };
                ensure!(
                    mode.is_none_or(|old| old == value),
                    "conflicting HSPICE DCAP options"
                );
                mode = Some(value);
            }
        }
    }
    Ok(mode.unwrap_or(2))
}

type LoweredExpansion = (
    Vec<u8>,
    Vec<crate::OriginLine>,
    Vec<model_preprocess_simplis::source_dialect::model_mapping::ModelSelection>,
    DeckOptions,
);

fn lower_expansion(
    statements: &[model_preprocess_simplis::source_dialect::ScopedStatement],
    input_origins: Vec<crate::OriginLine>,
    limit: usize,
    budget: &WorkBudget,
) -> Result<LoweredExpansion> {
    let dcap = depletion_mode(
        statements.get(1..).context("missing HSPICE deck title")?,
        budget,
    )?;
    ensure!(
        statements.len() == input_origins.len(),
        "HSPICE statement origin mismatch"
    );
    let mut selections = Vec::new();
    let mut scope = Vec::new();
    let mut bytes = Vec::new();
    let mut origins = Vec::new();
    let mut state = DeckOptions::default();
    for (index, (statement, origin)) in statements.iter().zip(input_origins).enumerate() {
        budget.charge(Resource::Statements, 1)?;
        budget.charge(Resource::ScannedBytes, statement.text.len())?;
        let text = if index == 0 {
            // Native ngspice may expand resource directives before discarding
            // the title. Protect the title without changing its source span.
            if statement.text.trim_start().starts_with('.') {
                format!("* {}", statement.text)
            } else {
                statement.text.clone()
            }
        } else if analysis_plan::command(&statement.text) == ".MODEL" {
            let (text, selection) = models::lower(
                statement,
                &scope,
                dcap,
                limit.saturating_sub(bytes.len()),
                budget,
            )
            .with_context(|| format!("{}:{}", statement.source, statement.first_line))?;
            selections.push(selection);
            text
        } else {
            match analysis_plan::command(&statement.text).as_str() {
                ".SUBCKT" => scope.push(
                    statement
                        .text
                        .split_whitespace()
                        .nth(1)
                        .context("missing subcircuit name")?
                        .to_owned(),
                ),
                ".ENDS" => {
                    scope.pop().context("unmatched .ends")?;
                }
                _ => (),
            }
            lower_card(
                &statement.text,
                statement.context,
                &mut state,
                limit.saturating_sub(bytes.len()),
                budget,
            )
            .with_context(|| {
                format!(
                    "{}:{}-{}",
                    statement.source, statement.first_line, statement.last_line
                )
            })?
        };
        ensure!(
            text.len().saturating_add(1) <= limit.saturating_sub(bytes.len()),
            "standalone SPICE output byte budget exceeded"
        );
        budget.charge(Resource::EmittedBytes, text.len().saturating_add(1))?;
        budget.charge(Resource::GeneratedNodes, 1)?;
        bytes.extend_from_slice(text.as_bytes());
        bytes.push(b'\n');
        origins.push(origin);
    }
    ensure!(state.depth == 0, "unclosed HSPICE subcircuit");
    Ok((bytes, origins, selections, state))
}

fn add_catalog(
    statements: &mut Vec<model_preprocess_simplis::source_dialect::ScopedStatement>,
    origins: &mut Vec<crate::OriginLine>,
    members: Vec<(std::borrow::Cow<'_, crate::ParsedLibrary>, String)>,
    limit: usize,
    budget: &WorkBudget,
) -> Result<()> {
    let mut linked = Vec::new();
    let mut linked_origins = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut size = statements
        .iter()
        .map(|statement| statement.text.len().saturating_add(1))
        .sum::<usize>();
    for (member, _) in members {
        budget.charge(Resource::SourceVisits, 1)?;
        budget.charge(Resource::EvaluationSteps, 1)?;
        if !seen.insert(member.source_id().to_owned()) {
            continue;
        }
        for statement in member.scoped_statements() {
            budget.charge(Resource::Statements, 1)?;
            budget.charge(Resource::ScannedBytes, statement.text.len())?;
            size = size.saturating_add(statement.text.len()).saturating_add(1);
            ensure!(
                size <= limit,
                "HSPICE catalog expansion byte budget exceeded"
            );
            budget.charge(Resource::GeneratedNodes, 1)?;
            budget.charge(Resource::EmittedBytes, statement.source.len())?;
            linked_origins.push(crate::OriginLine {
                source: std::path::PathBuf::from(&statement.source),
                line: statement.first_line,
                last_line: statement.last_line,
            });
            linked.push(statement.clone_with_budget(budget)?);
        }
    }
    // Catalog support declarations precede root declarations. Explicit root
    // parameter declarations therefore retain last-definition precedence.
    ensure!(
        !statements.is_empty() && !origins.is_empty(),
        "missing HSPICE deck title"
    );
    statements.splice(1..1, linked);
    origins.splice(1..1, linked_origins);
    Ok(())
}

pub(super) fn prepare(
    root: &AdmittedSource,
    request: &Request,
    options: &Options,
    cancellation: &Cancellation,
    session: &mut SourceSession,
) -> Result<PreparedBundle> {
    let mut expansion = SourceExpansion::new(session, request, options);
    let context = expansion.expand(root, HSPICE, SourceKind::Deck, None)?;
    let SourceExpansion {
        mut statements,
        mut origins,
        ..
    } = expansion;
    let members = super::catalog_members(request, options, cancellation, session, context)?;
    let budget = session.budget();
    add_catalog(
        &mut statements,
        &mut origins,
        members,
        options.output_bytes,
        budget,
    )?;
    let (mut bytes, mut origins, selections, state) =
        lower_expansion(&statements, origins, options.output_bytes, budget)?;
    {
        (bytes, origins) = hierarchy::bind(
            &bytes,
            &origins,
            state.parhier.unwrap_or_default(),
            state.xmult_in_exp.unwrap_or_default(),
            &state.mos,
            options.output_bytes,
            budget,
        )?;

        // HSPICE defaults to 25 C for both device nominal and analysis
        // temperatures. Native ngspice defaults to 27 C.
        let mut defaults = Vec::new();
        if !state.nominal_temperature {
            defaults.push(".options TNOM=25\n");
        }
        if !state.temperature {
            defaults.push(".temp 25\n");
        }
        let insertion = bytes
            .iter()
            .position(|byte| *byte == b'\n')
            .context("missing HSPICE deck title")?
            + 1;
        let synthetic = crate::OriginLine {
            source: root.path.clone(),
            line: 0,
            last_line: 0,
        };
        let default_text = defaults.concat();
        ensure!(
            bytes.len().saturating_add(default_text.len()) <= options.output_bytes,
            "standalone SPICE output byte budget exceeded"
        );
        budget.charge(Resource::EmittedBytes, default_text.len())?;
        budget.charge(Resource::GeneratedNodes, defaults.len())?;
        budget.charge(
            Resource::EmittedBytes,
            root.path.as_os_str().len().saturating_mul(defaults.len()),
        )?;
        bytes.splice(insertion..insertion, default_text.bytes());
        origins.splice(1..1, std::iter::repeat_n(synthetic, defaults.len()));
    }
    bytes = models::resolve_breakdown(&bytes, RootKind::Deck, options.output_bytes, budget)?;
    let mut bundle = native_bundle(
        root.path.clone(),
        request.format,
        RootKind::Deck,
        NativeProjection::plain(&bytes, RootKind::Deck, budget)?,
        &origins,
        budget,
    )?;
    bundle.model_selections = selections;
    Ok(bundle)
}
