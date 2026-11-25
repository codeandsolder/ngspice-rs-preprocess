//! Standalone HSPICE resource admission and native artifact preparation.
//! HSPICE compatibility is local to source resources; no simulator global
//! compatibility flag is set by this producer.
use super::{SourceExpansion, analysis_plan};
use crate::{
    AdmittedSource, InputFormat, Options, PreparedBundle, Request, SourceSession, native_bundle,
};
use anyhow::{Context, Result, bail, ensure};
use model_normalize_ngspice::{
    linked_source::RootKind, model_card::parse_parameter_fields, statement_token_spans,
};
use model_preprocess_simplis::{
    lower_spice_statement,
    source_dialect::{DialectContext, SourceKind, SourceLanguage},
};
use std::fmt::Write as _;

const HSPICE: DialectContext = DialectContext {
    language: SourceLanguage::Hspice,
    separator: None,
};

#[derive(Default)]
struct DeckOptions {
    nominal_temperature: bool,
    temperature: bool,
    depth: usize,
}

fn normalize_options(text: &str, state: &mut DeckOptions) -> Result<String> {
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

fn lower_card(
    text: &str,
    context: DialectContext,
    state: &mut DeckOptions,
    limit: usize,
) -> Result<String> {
    let text = text.trim();
    if text.is_empty() || text.starts_with('*') {
        return Ok(text.to_owned());
    }
    let command = analysis_plan::command(text);
    let hspice = context.language == SourceLanguage::Hspice;
    let fields = statement_token_spans(text)?;
    let assignment = fields.iter().any(|field| &text[field.clone()] == "=");
    // Until the source hierarchy resolver can represent both HSPICE precedence
    // modes and nested multiplicity, never emit a native interpretation of
    // a parameter declaration or actual in a subcircuit.
    if command == ".SUBCKT" {
        ensure!(
            !assignment,
            "HSPICE subcircuit formal parameters require hierarchy lowering"
        );
        state.depth += 1;
    } else if command == ".ENDS" {
        state.depth = state.depth.checked_sub(1).context("unmatched .ENDS")?;
    }
    if state.depth > 0
        && matches!(
            command.as_str(),
            ".PARAM" | ".FUNC" | ".OPTION" | ".OPTIONS" | ".OPT"
        )
    {
        bail!("HSPICE scoped parameters/options require hierarchy lowering");
    }
    if !command.starts_with('.') {
        let kind = command.as_bytes()[0];
        ensure!(
            matches!(
                kind,
                b'R' | b'C' | b'L' | b'V' | b'I' | b'E' | b'G' | b'F' | b'H' | b'B' | b'X'
            ),
            "HSPICE device {command} requires qualified model/device lowering"
        );
        if kind == b'X' {
            ensure!(
                !assignment,
                "HSPICE subcircuit actuals/multipliers require hierarchy lowering"
            );
        }
    }
    let mut source = match command.as_str() {
        ".OPTION" | ".OPTIONS" | ".OPT" if hspice => normalize_options(text, state)?,
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
    source = lower_spice_statement(&source, context, limit)?;
    Ok(source)
}

pub(super) fn prepare(
    root: &AdmittedSource,
    request: &Request,
    options: &Options,
    session: &mut SourceSession,
) -> Result<PreparedBundle> {
    let deck = request.format == InputFormat::HspiceDeck;
    let kind = if deck {
        RootKind::Deck
    } else {
        RootKind::Include
    };
    let mut expansion = SourceExpansion::new(session, request, options);
    expansion.expand(
        root,
        HSPICE,
        if deck {
            SourceKind::Deck
        } else {
            SourceKind::Include
        },
        None,
    )?;
    let mut bytes = Vec::new();
    let mut origins = Vec::new();
    let mut state = DeckOptions::default();
    for (index, (statement, origin)) in expansion
        .statements
        .iter()
        .zip(expansion.origins)
        .enumerate()
    {
        let text = if deck && index == 0 {
            // Native ngspice may expand resource directives before discarding
            // the title. Protect the title without changing its source span.
            if statement.text.trim_start().starts_with('.') {
                format!("* {}", statement.text)
            } else {
                statement.text.clone()
            }
        } else {
            lower_card(
                &statement.text,
                statement.context,
                &mut state,
                options.output_bytes.saturating_sub(bytes.len()),
            )
            .with_context(|| {
                format!(
                    "{}:{}-{}",
                    statement.source, statement.first_line, statement.last_line
                )
            })?
        };
        ensure!(
            text.len().saturating_add(1) <= options.output_bytes.saturating_sub(bytes.len()),
            "standalone SPICE output byte budget exceeded"
        );
        bytes.extend_from_slice(text.as_bytes());
        bytes.push(b'\n');
        origins.push(origin);
    }
    ensure!(state.depth == 0, "unclosed HSPICE subcircuit");
    if deck {
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
        bytes.splice(insertion..insertion, default_text.bytes());
        origins.splice(1..1, std::iter::repeat_n(synthetic, defaults.len()));
    }
    native_bundle(root.path.clone(), request.format, kind, &bytes, &origins)
}
