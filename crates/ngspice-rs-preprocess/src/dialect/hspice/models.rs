//! Standalone nongeometric HSPICE junction diodes.
//!
//! HSPICE E-2010.12, chapter 3, specifies AREA*M scaling of IBV even
//! for LEVEL=1. Native level 3 provides this scaling without geometry
//! when the instance has only dimensionless AREA/PJ. DCAP=2 is the
//! zero-threshold special case of the native depletion-charge law.
use anyhow::{Context, Result, bail, ensure};
use model_normalize_ngspice::{
    linked_source::RootKind,
    model_card::{ModelCard, ModelParameter, parse_model_card, parse_spice_constant},
    parameter_scope::ModelLibrary,
};
use model_preprocess_simplis::{
    lower_spice_expression,
    source_dialect::{
        DialectContext, ScopedStatement, SourceLanguage,
        model_mapping::{MappingTable, ModelSelection},
    },
};
use std::{collections::BTreeSet, fmt::Write as _};

fn canonical(name: &str) -> Result<&'static str> {
    Ok(match name.to_ascii_uppercase().as_str() {
        "IS" | "JS" => "IS",
        "JSW" | "ISW" | "ISP" => "JSW",
        "N" => "N",
        "RS" => "RS",
        "CJ" | "CJO" | "CJ0" | "CJA" => "CJ",
        "CJP" | "CJSW" => "CJP",
        "PB" | "VJ" | "PHI" | "PHA" => "PB",
        "PHP" => "PHP",
        "M" | "MJ" | "EXA" => "M",
        "MJSW" | "EXP" => "MJSW",
        "FC" => "FC",
        "FCS" => "FCS",
        "TT" => "TT",
        "BV" | "VB" | "VAR" | "VRB" => "BV",
        "IBV" | "IB" => "IBV",
        "NBV" => "NBV",
        "IK" | "IKF" | "JBF" => "IKF",
        "IKR" | "JBR" => "IKR",
        "EG" => "EG",
        "XTI" => "XTI",
        "TNOM" | "TREF" => "TNOM",
        "KF" => "KF",
        "AF" => "AF",
        "LEVEL" => "LEVEL",
        _ => bail!("unqualified HSPICE diode model parameter {name}"),
    })
}

pub fn render(
    card: &ModelCard,
    name: &str,
    mut map: impl FnMut(&str) -> Result<String>,
) -> Result<String> {
    let mut text = format!(".model {name} {} (", card.family);
    for parameter in &card.parameters {
        let value = parameter
            .expression
            .as_deref()
            .context("model field requires a value")?;
        write!(text, " {}={}", parameter.name, map(value)?)?;
    }
    text.push(')');
    Ok(text)
}

pub fn lower(
    statement: &ScopedStatement,
    scope: &[String],
    dcap: u8,
    limit: usize,
) -> Result<(String, ModelSelection)> {
    ensure!(
        statement.context.language == SourceLanguage::Hspice,
        "model lowering for this resource language is not qualified"
    );
    let selection = MappingTable::select_standalone_hspice(statement, scope)?;
    let mut selected = selection.clone();
    let mut seen = BTreeSet::new();
    for parameter in &mut selected.card.parameters {
        parameter.name = canonical(&parameter.name)?.to_owned();
        ensure!(
            seen.insert(parameter.name.clone()),
            "duplicate HSPICE diode model parameter {}",
            parameter.name
        );
        ensure!(
            parameter.expression.is_some(),
            "model field requires a value"
        );
    }
    if dcap == 2 {
        selected
            .card
            .parameters
            .retain(|field| !matches!(field.name.as_str(), "FC" | "FCS"));
        for name in ["FC", "FCS"] {
            selected.card.parameters.push(ModelParameter {
                name: name.to_owned(),
                expression: Some("0".to_owned()),
            });
        }
    }
    let native = selected.lower()?;
    let card = parse_model_card(&native, statement.first_line, &[])?;
    let text = render(&card, &card.name, |value| {
        lower_spice_expression(
            value,
            DialectContext {
                language: SourceLanguage::Hspice,
                separator: None,
            },
            limit,
        )
    })?;
    ensure!(
        text.len() <= limit,
        "HSPICE model expansion byte budget exceeded"
    );
    Ok((text, selection))
}

/// Resolve HSPICE's zero-breakdown sentinel after occurrence binding. A
/// standalone include cannot assume values of parameters supplied by its caller.
pub fn resolve_breakdown(bytes: &[u8], kind: RootKind, limit: usize) -> Result<Vec<u8>> {
    let source = std::str::from_utf8(bytes)?;
    let library = if kind == RootKind::Deck {
        Some(ModelLibrary::parse_source(bytes, kind)?)
    } else {
        None
    };
    let mut output = String::with_capacity(bytes.len());
    for (index, line) in source.lines().enumerate() {
        if (index == 0 && kind == RootKind::Deck)
            || !super::analysis_plan::command(line).eq_ignore_ascii_case(".MODEL")
        {
            writeln!(output, "{line}")?;
            continue;
        }
        let mut card = parse_model_card(line, index + 1, &[])?;
        if let Some(position) = card
            .parameters
            .iter()
            .position(|field| field.name.eq_ignore_ascii_case("BV"))
        {
            let expression = card.parameters[position]
                .expression
                .as_deref()
                .context("BV requires a value")?;
            let value = if let Some(library) = &library {
                library.evaluate_scalar(0, expression, &[])?
            } else {
                parse_spice_constant(expression).context("parameter-dependent BV in an independent HSPICE include requires consuming deck binding")?
            };
            ensure!(
                value.is_finite() && value >= 0.,
                "invalid HSPICE BV {value}"
            );
            if value == 0. {
                card.parameters.remove(position);
            }
            writeln!(
                output,
                "{}",
                render(&card, &card.name, |value| Ok(value.to_owned()))?
            )?;
        } else {
            writeln!(output, "{line}")?;
        }
        ensure!(
            output.len() <= limit,
            "HSPICE breakdown normalization exceeds byte budget"
        );
    }
    ensure!(
        output.len() <= limit,
        "HSPICE breakdown normalization exceeds byte budget"
    );
    Ok(output.into_bytes())
}
