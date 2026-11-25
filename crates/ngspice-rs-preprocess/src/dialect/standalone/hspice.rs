//! Standalone HSPICE source normalization. Circuit semantics are distinct
//! from `SIMetrix`'s optional HSPICE model-selection preset.
use anyhow::{Context, Result, bail, ensure};
use model_normalize_ngspice::{
    model_card::{ModelParameter, parse_model_card, parse_parameter_fields, parse_spice_constant},
    statement_token_spans,
};
use model_preprocess_simplis::{
    lower_scoped_statements,
    source_dialect::{
        HspiceExpressionKind, ScopedStatement, SourceLanguage, lower_hspice_expression,
    },
};
use std::{collections::BTreeSet, fmt::Write as _, ops::Range};

mod bindings;
mod scopes;

fn tokens(text: &str) -> Result<Vec<&str>> {
    Ok(statement_token_spans(text)?
        .into_iter()
        .map(|span| &text[span])
        .collect())
}

fn command(text: &str) -> String {
    text.split_ascii_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase()
}

fn tail(text: &str) -> &str {
    text.trim_start()
        .split_once(char::is_whitespace)
        .map_or("", |(_, tail)| tail.trim())
}

fn with_text(source: &ScopedStatement, text: String) -> ScopedStatement {
    ScopedStatement {
        text,
        ..source.clone()
    }
}

fn expression_body(expression: &str) -> &str {
    let expression = expression.trim();
    expression
        .strip_prefix('{')
        .and_then(|v| v.strip_suffix('}'))
        .unwrap_or(expression)
}

fn braced(expression: &str) -> String {
    format!("{{{}}}", expression_body(expression))
}

fn scalar(value: &str, limit: usize) -> Result<String> {
    expression(value, HspiceExpressionKind::Static, limit)
}

fn expression(value: &str, kind: HspiceExpressionKind, limit: usize) -> Result<String> {
    // Both kinds of HSPICE quote delimit arithmetic, not strings, in
    // scalar fields. File path quotes have already been consumed by linking.
    if let Some(body) = value.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
        return Ok(braced(&lower_hspice_expression(body, kind, limit)?));
    }
    lower_hspice_expression(value, kind, limit)
}

fn assignment_value(parameter: &ModelParameter, limit: usize) -> Result<String> {
    scalar(
        parameter
            .expression
            .as_deref()
            .with_context(|| format!("{} requires a value", parameter.name))?,
        limit,
    )
}

fn parameters(source: &ScopedStatement, limit: usize) -> Result<Vec<ScopedStatement>> {
    let mut output = Vec::new();
    for parameter in parse_parameter_fields(tail(&source.text), &[])? {
        let value = assignment_value(&parameter, limit)?;
        let value = braced(&value);
        let statement = if parameter.name.contains('(') {
            format!(".func {} {value}", parameter.name)
        } else {
            format!(".param {}={value}", parameter.name)
        };
        output.push(with_text(source, statement));
    }
    ensure!(!output.is_empty(), "empty .PARAM statement");
    Ok(output)
}

#[derive(Default)]
struct DeckOptions {
    nominal: Option<String>,
    ambient: Option<String>,
    local_parameters: bool,
}

impl DeckOptions {
    fn read(&mut self, source: &ScopedStatement, limit: usize) -> Result<String> {
        let mut output = String::from(".options");
        for parameter in parse_parameter_fields(tail(&source.text), &[])? {
            let name = parameter.name.to_ascii_uppercase();
            let raw = parameter.expression.as_deref();
            match name.as_str() {
                "PARHIER" | "PARHIE" => {
                    self.local_parameters = match raw.map(str::to_ascii_uppercase).as_deref() {
                        Some("LOCAL") => true,
                        Some("GLOBAL") => false,
                        _ => bail!("PARHIER requires LOCAL or GLOBAL"),
                    };
                }
                "TNOM" | "TEMP" => {
                    let value = assignment_value(&parameter, limit)?;
                    if name == "TNOM" {
                        self.nominal = Some(value);
                    } else {
                        self.ambient = Some(value);
                    }
                }
                // These native numerical options have the same units and
                // meaning. Keep all unsupported options explicit.
                "RELTOL" | "ABSTOL" | "VNTOL" | "GMIN" | "ITL1" | "ITL2" | "ITL4" | "MAXORD"
                | "TRTOL" | "METHOD" => {
                    write!(output, " {name}={}", assignment_value(&parameter, limit)?)?;
                }
                // Listing/output selection is not a constitutive change.
                // Keep it visible in the provenance-bearing comment below.
                "POST" | "PROBE" | "LIST" | "NODE" | "OPTS" | "NUMDGT" | "MEASDGT" | "INGOLD" => {}
                _ => bail!("unqualified HSPICE option {name}"),
            }
        }
        if output == ".options" {
            Ok(format!("* {}", source.text.trim_start()))
        } else {
            Ok(output)
        }
    }

    fn temperature(&mut self, source: &ScopedStatement, limit: usize) -> Result<String> {
        let values = tokens(tail(&source.text))?;
        let [temperature] = values.as_slice() else {
            bail!("HSPICE temperature sweeps require explicit analysis binding");
        };
        self.ambient = Some(scalar(temperature, limit)?);
        Ok(format!("* {}", source.text.trim_start()))
    }

    fn defaults(&self, title: &ScopedStatement) -> [ScopedStatement; 2] {
        let nominal = self.nominal.as_deref().unwrap_or("25");
        let ambient = self.ambient.as_deref().unwrap_or(nominal);
        [
            with_text(title, format!(".options tnom={nominal}")),
            with_text(title, format!(".temp {ambient}")),
        ]
    }
}

fn model(source: &ScopedStatement, limit: usize) -> Result<String> {
    let card = parse_model_card(
        source.text.trim_start(),
        source.first_line,
        &["LEVEL", "VERSION"],
    )?;
    let mut output = format!(".model {} {} (", card.name, card.family);
    let mut fields = BTreeSet::new();
    for parameter in card.parameters {
        let name = parameter.name.to_ascii_uppercase();
        ensure!(fields.insert(name.clone()), "duplicate model field {name}");
        // The native source parser shares the card grammar. Physical
        // model selection is a later pass; do not apply SIMetrix defaults.
        if parameter.expression.is_some() {
            write!(
                output,
                " {}={}",
                parameter.name,
                assignment_value(&parameter, limit)?
            )?;
        } else {
            write!(output, " {}", parameter.name)?;
        }
    }
    output.push(')');
    Ok(output)
}

/// Edit only grammar-identified scalar fields. Electrical names and model
/// identifiers may start with digits; they are never interpreted as numbers.
fn rewrite_fields(
    text: &str,
    fields: &[Range<usize>],
    start: usize,
    positional_values: bool,
    models: &BTreeSet<String>,
    limit: usize,
) -> Result<String> {
    let mut edits = Vec::<(Range<usize>, String)>::new();
    let mut index = start;
    while let Some(span) = fields.get(index) {
        if fields
            .get(index + 1)
            .is_some_and(|next| &text[next.clone()] == "=")
        {
            let value = fields
                .get(index + 2)
                .context("missing instance parameter value")?;
            edits.push((value.clone(), scalar(&text[value.clone()], limit)?));
            index += 3;
            continue;
        }
        let value = &text[span.clone()];
        if positional_values && !models.contains(&value.to_ascii_uppercase()) {
            edits.push((span.clone(), scalar(value, limit)?));
        }
        index += 1;
    }
    let mut result = String::new();
    let mut previous = 0;
    for (span, value) in edits {
        result.push_str(&text[previous..span.start]);
        result.push_str(&value);
        previous = span.end;
        ensure!(
            result.len() <= limit,
            "source statement expansion byte budget exceeded"
        );
    }
    result.push_str(&text[previous..]);
    ensure!(
        result.len() <= limit,
        "source statement expansion byte budget exceeded"
    );
    Ok(result)
}

fn device(text: &str, models: &BTreeSet<String>, limit: usize) -> Result<String> {
    let fields = statement_token_spans(text)?;
    let element = fields.first().context("missing device name")?;
    let letter = text.as_bytes()[element.start].to_ascii_uppercase();
    let token = |index: usize| fields.get(index).map(|span| &text[span.clone()]);
    let mut normalized = text.to_owned();

    // HSPICE permits explicit controlled-source type keywords. Native
    // voltage/current-source VALUE syntax preserves the E/G identity.
    if matches!(letter, b'E'..=b'H') {
        let alias = match letter {
            b'E' => "VCVS",
            b'F' => "CCCS",
            b'G' => "VCCS",
            _ => "CCVS",
        };
        if token(3).is_some_and(|value| value.eq_ignore_ascii_case(alias)) {
            normalized.replace_range(fields[3].clone(), "");
            return device(&normalized, models, limit);
        }
        if matches!(letter, b'E' | b'G')
            && token(3).is_some_and(|value| {
                value.eq_ignore_ascii_case("VOL")
                    || value.eq_ignore_ascii_case("CUR")
                    || value.eq_ignore_ascii_case("VALUE")
            })
        {
            ensure!(
                token(4) == Some("="),
                "behavioral controlled source requires '='"
            );
            let value = fields.get(5).context("missing behavioral expression")?;
            let expr = expression(
                &text[value.start..],
                HspiceExpressionKind::Behavioral,
                limit,
            )?;
            return Ok(format!(
                "{} VALUE={}",
                text[..fields[3].start].trim_end(),
                braced(&expr)
            ));
        }
    }

    let (start, positional) = match letter {
        b'R' | b'C' | b'L' | b'V' | b'I' | b'K' => (3, true),
        b'M' => (6, false),
        b'D' | b'F' | b'H' => (4, true),
        b'J' | b'Z' | b'E' | b'G' => (5, true),
        b'Q' => {
            // The optional fourth terminal precedes the model. Resolve
            // by the declared model namespace, not the spelling of a node.
            let positions = [4, 5]
                .into_iter()
                .filter(|&i| {
                    token(i).is_some_and(|name| models.contains(&name.to_ascii_uppercase()))
                })
                .collect::<Vec<_>>();
            let [position] = positions.as_slice() else {
                bail!("cannot uniquely identify BJT model in {text}")
            };
            (position + 1, true)
        }
        b'X' => (1, false),
        _ => bail!("unqualified HSPICE device {text}"),
    };
    ensure!(fields.len() >= start, "incomplete HSPICE device {text}");
    rewrite_fields(text, &fields, start, positional, models, limit)
}

fn analysis(text: &str, name: &str, models: &BTreeSet<String>, limit: usize) -> Result<String> {
    let fields = statement_token_spans(text)?;
    let token = |index: usize| fields.get(index).map(|span| &text[span.clone()]);
    match name {
        ".OP" => ensure!(fields.len() == 1, "unqualified .OP arguments"),
        ".AC" => ensure!(
            fields.len() == 5
                && token(1).is_some_and(|v| ["LIN", "DEC", "OCT"]
                    .iter()
                    .any(|s| v.eq_ignore_ascii_case(s))),
            "unqualified .AC sweep"
        ),
        ".TRAN" => ensure!(
            (3..=6).contains(&fields.len())
                && !fields
                    .iter()
                    .any(|span| text[span.clone()].eq_ignore_ascii_case("SWEEP")),
            "unqualified .TRAN sweep"
        ),
        ".DC" => ensure!(
            fields.len() == 5
                && token(1).is_some_and(|value| value.starts_with(['V', 'v', 'I', 'i'])
                    || value.eq_ignore_ascii_case("TEMP")),
            "unqualified .DC sweep"
        ),
        _ => bail!("unqualified analysis {name}"),
    }
    let first = match name {
        ".DC" | ".AC" => 2,
        _ => 1,
    };
    rewrite_fields(text, &fields, first, true, models, limit)
}

pub(super) fn lower(
    source: &[ScopedStatement],
    deck: bool,
    limit: usize,
) -> Result<Vec<ScopedStatement>> {
    let skip = usize::from(deck);
    let (title, body) = source.split_at(skip.min(source.len()));
    ensure!(title.len() == skip, "missing deck title");
    let mut output = title.to_vec();
    let mut options = DeckOptions::default();
    let models = body
        .iter()
        .filter(|line| command(&line.text) == ".MODEL")
        .filter_map(|line| line.text.split_whitespace().nth(1))
        .map(str::to_ascii_uppercase)
        .collect::<BTreeSet<_>>();
    let mut scope_depth = 0usize;
    for source in body {
        let result = (|| -> Result<Vec<ScopedStatement>> {
            let name = command(&source.text);
            if name.is_empty() || name.starts_with('*') || name.starts_with('$') {
                return Ok(vec![source.clone()]);
            }
            if name == ".SUBCKT" {
                scope_depth += 1;
            } else if name == ".ENDS" {
                scope_depth = scope_depth.checked_sub(1).context("unmatched .ENDS")?;
            }
            if source.context.language != SourceLanguage::Hspice {
                return lower_scoped_statements(std::slice::from_ref(source), limit);
            }
            let text = source.text.trim_start();
            let text = match name.as_str() {
                ".PARAM" => return parameters(source, limit),
                ".FUNC" => {
                    let fields = statement_token_spans(text)?;
                    let body = fields.get(2).context("missing .FUNC expression")?;
                    format!(
                        "{} {}",
                        text[..body.start].trim_end(),
                        braced(&scalar(&text[body.start..], limit)?)
                    )
                }
                ".MODEL" => model(source, limit)?,
                ".OPTIONS" | ".OPTION" => {
                    ensure!(
                        deck && scope_depth == 0,
                        "subcircuit/library-scoped options require qualification"
                    );
                    options.read(source, limit)?
                }
                ".TEMP" => {
                    ensure!(
                        deck && scope_depth == 0,
                        "subcircuit/library-scoped temperature requires qualification"
                    );
                    options.temperature(source, limit)?
                }
                ".AC" | ".DC" | ".TRAN" | ".OP" => {
                    ensure!(
                        deck && scope_depth == 0,
                        "library-scoped analyses require qualification"
                    );
                    analysis(text, &name, &models, limit)?
                }
                ".SUBCKT" => rewrite_fields(
                    text,
                    &statement_token_spans(text)?,
                    2,
                    false,
                    &models,
                    limit,
                )?,
                ".IC" | ".NODESET" => rewrite_fields(
                    text,
                    &statement_token_spans(text)?,
                    1,
                    false,
                    &models,
                    limit,
                )?,
                ".PRINT" | ".PLOT" | ".PROBE" | ".MEAS" | ".MEASURE" | ".GLOBAL" | ".END"
                | ".ENDS" | ".TITLE" => text.to_owned(),
                _ if name.starts_with('.') => bail!("unqualified HSPICE directive {name}"),
                _ => device(text, &models, limit)?,
            };
            Ok(vec![with_text(source, text)])
        })()
        .with_context(|| format!("{}:{}", source.source, source.first_line))?;
        output.extend(result);
    }
    ensure!(scope_depth == 0, "unterminated source subcircuit");
    finish(output, deck, &options, limit)
}

fn finish(
    output: Vec<ScopedStatement>,
    deck: bool,
    options: &DeckOptions,
    limit: usize,
) -> Result<Vec<ScopedStatement>> {
    let output = scopes::normalize(output, deck, limit)?;
    let mut output = bindings::bind(&output, deck, options.local_parameters, limit)?;
    if deck {
        let title = output.first().context("missing deck title")?;
        let defaults = options.defaults(title);
        output.splice(1..1, defaults);
    }
    ensure!(
        output
            .iter()
            .all(|statement| !statement.text.contains('\n')),
        "unexpected projected statement newline"
    );
    Ok(output)
}

fn validate_multiplier(value: &str) -> Result<()> {
    if let Ok(value) = parse_spice_constant(expression_body(value)) {
        ensure!(value > 0., "HSPICE multiplier must be positive");
    }
    Ok(())
}
