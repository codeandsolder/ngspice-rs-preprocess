//! Lower source-dialect built-ins before native device translation.
//!
//! Static parameters are elaborated at the source deck's nominal analysis
//! temperature. Native behavioral expressions use ngspice's live temperature.

use super::{
    Item, behavioral_function_arguments, directive_name, lower_simetrix_behavioral_expression,
    matching_behavioral_parenthesis, model_header, parse_items, render_native_subcircuit,
};
use crate::{normalized_statement, parse_numeric, statement_tokens};
use anyhow::{Context, Result, bail, ensure};
use model_preprocess_simplis::{
    LinkedPreprocessedNetlist, active_statement_prefix, parameter_assignments, simetrix_constant,
    validate_simetrix_parameter_name,
};
use std::{collections::BTreeMap, fmt::Write as _};

const LNCOSH_NAME: &str = "sr_simetrix_lncosh";
const TEMP_NAME: &str = "sr_simetrix_analysis_temp";

#[derive(Clone, Copy)]
enum ExpressionContext {
    Static,
    Behavioral,
}

struct Builtins {
    lncosh: bool,
    static_temp: bool,
    lncosh_name: String,
    temp_name: String,
}

fn unused_identifier(base: &str, source: &str) -> String {
    // The complete source is case folded once. Avoid *all* existing source
    // identifiers, not only declarations known to this particular pass.
    let mut name = base.to_owned();
    let mut suffix = 0usize;
    while source.contains(&name) {
        suffix += 1;
        name = format!("{base}_{suffix}");
    }
    name
}

impl Builtins {
    fn new(linked: &LinkedPreprocessedNetlist) -> Self {
        let mut source = String::from_utf8_lossy(&linked.root_bytes).to_ascii_lowercase();
        for library in &linked.libraries {
            source.push('\n');
            source.push_str(&String::from_utf8_lossy(&library.bytes).to_ascii_lowercase());
        }
        Self {
            lncosh: false,
            static_temp: false,
            lncosh_name: unused_identifier(LNCOSH_NAME, &source),
            temp_name: unused_identifier(TEMP_NAME, &source),
        }
    }
}

const fn symbol_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$' | b'.')
}

const fn symbol_start(input: &[u8], index: usize) -> bool {
    (input[index].is_ascii_alphabetic() || input[index] == b'_')
        && (index == 0 || !symbol_byte(input[index - 1]))
}

impl Builtins {
    fn expression(&mut self, input: &str, context: ExpressionContext) -> Result<String> {
        let mut output = String::with_capacity(input.len());
        let bytes = input.as_bytes();
        let mut cursor = 0;
        while cursor < bytes.len() {
            if bytes[cursor] == b'"' {
                let end = input[cursor + 1..]
                    .find('"')
                    .map(|index| cursor + index + 2)
                    .context("unterminated quoted expression string")?;
                output.push_str(&input[cursor..end]);
                cursor = end;
            } else if symbol_start(bytes, cursor) {
                let (rewritten, end) = self.symbol(input, cursor, context)?;
                output.push_str(&rewritten);
                cursor = end;
            } else {
                let character = input[cursor..]
                    .chars()
                    .next()
                    .context("expression boundary")?;
                output.push(character);
                cursor += character.len_utf8();
            }
        }
        Ok(output)
    }

    fn symbol(
        &mut self,
        input: &str,
        start: usize,
        context: ExpressionContext,
    ) -> Result<(String, usize)> {
        let bytes = input.as_bytes();
        let mut end = start + 1;
        while bytes.get(end).copied().is_some_and(symbol_byte) {
            end += 1;
        }
        let name = &input[start..end];
        let mut open = end;
        while bytes.get(open).is_some_and(u8::is_ascii_whitespace) {
            open += 1;
        }
        if bytes.get(open) == Some(&b'(') {
            if name.eq_ignore_ascii_case("V") || name.eq_ignore_ascii_case("I") {
                let close = matching_behavioral_parenthesis(input, open)?;
                return Ok((input[start..=close].to_owned(), close + 1));
            }
            if name.eq_ignore_ascii_case("GAUSS") || name.eq_ignore_ascii_case("LNCOSH") {
                let close = matching_behavioral_parenthesis(input, open)?;
                let args = behavioral_function_arguments(&input[open + 1..close])?;
                ensure!(
                    args.len() == 1 && !args[0].is_empty(),
                    "SIMetrix {name} requires one argument"
                );
                if name.eq_ignore_ascii_case("GAUSS") {
                    // Nominal GAUSS does not evaluate its argument, even if undefined.
                    return Ok(("1".to_owned(), close + 1));
                }
                self.lncosh = true;
                let value = self.expression(args[0], context)?;
                return Ok((format!("{}({value})", self.lncosh_name), close + 1));
            }
            return Ok((name.to_owned(), end));
        }
        if let Some(value) = simetrix_constant(name) {
            return Ok((value.to_owned(), end));
        }
        if name.eq_ignore_ascii_case("TEMP") {
            return Ok((
                match context {
                    ExpressionContext::Static => {
                        self.static_temp = true;
                        &self.temp_name
                    }
                    ExpressionContext::Behavioral => "temper",
                }
                .to_owned(),
                end,
            ));
        }
        Ok((name.to_owned(), end))
    }

    fn assignments(&mut self, text: &str) -> Result<String> {
        let mut output = String::new();
        for (name, value) in parameter_assignments(text)? {
            validate_simetrix_parameter_name(name)?;
            let rewritten = self.expression(value, ExpressionContext::Static)?;
            let rewritten = lower_simetrix_behavioral_expression(&rewritten)?;
            write!(output, " {name}={rewritten}")?;
        }
        Ok(output)
    }

    fn braced_regions(&mut self, statement: &str) -> Result<String> {
        let mut output = String::with_capacity(statement.len());
        let mut offset = 0;
        while let Some(relative) = statement[offset..].find('{') {
            let start = offset + relative;
            let close = super::matching_behavioral_delimiter(
                statement,
                start,
                '{',
                '}',
                "expression brace underflow",
                "unterminated expression brace",
            )?;
            output.push_str(&statement[offset..=start]);
            let expression =
                self.expression(&statement[start + 1..close], ExpressionContext::Static)?;
            output.push_str(&lower_simetrix_behavioral_expression(&expression)?);
            output.push('}');
            offset = close + 1;
        }
        output.push_str(&statement[offset..]);
        Ok(output)
    }

    fn statement(&mut self, statement: &str) -> Result<String> {
        let trimmed = statement.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('*') {
            return Ok(statement.to_owned());
        }
        let normalized = normalized_statement(statement);
        let active = active_statement_prefix(&normalized);
        match directive_name(trimmed).as_deref() {
            Some(".MODEL") => self.braced_regions(&materialize_jfet_defaults(active)?),
            Some(".PARAM") => {
                let tail = active
                    .split_once(char::is_whitespace)
                    .context("empty .PARAM")?
                    .1;
                Ok(format!(".param{}", self.assignments(tail)?))
            }
            Some(".FUNC") => {
                let open = active.find('(').context("missing function arguments")?;
                let close = matching_behavioral_parenthesis(active, open)?;
                let name = active[..open]
                    .split_whitespace()
                    .next_back()
                    .context("missing function name")?;
                if name.eq_ignore_ascii_case("LNCOSH") || name.eq_ignore_ascii_case("GAUSS") {
                    return Ok(format!(
                        "* ignored definition of predefined SIMetrix function {name}"
                    ));
                }
                let body = self.expression(&active[close + 1..], ExpressionContext::Static)?;
                Ok(format!(
                    "{}{}",
                    &active[..=close],
                    lower_simetrix_behavioral_expression(&body)?
                ))
            }
            Some(
                ".GRAPH" | ".PRINT" | ".KEEP" | ".POST_PROCESS" | ".INCLUDE" | ".NODE_MAP"
                | ".LICENSE_FEATURE" | ".REQUIRED_VERSION",
            ) => Ok(statement.to_owned()),
            Some(".MC" | ".MONTE" | ".MONTECARLO" | ".WCASE" | ".WORSTCASE") => {
                bail!("SIMetrix statistical analysis is not implemented");
            }
            Some(".SUBCKT") | None
                if normalized
                    .as_bytes()
                    .first()
                    .is_some_and(|byte| matches!(byte.to_ascii_uppercase(), b'X' | b'.')) =>
            {
                if let Some(index) = active.to_ascii_lowercase().find(" params:") {
                    let start = index + " params:".len();
                    Ok(format!(
                        "{}{}",
                        &active[..start],
                        self.assignments(&active[start..])?
                    ))
                } else {
                    self.braced_regions(active)
                }
            }
            _ => {
                if let Some((prefix, expression)) = behavioral_rhs(active) {
                    let expression = self.expression(expression, ExpressionContext::Behavioral)?;
                    Ok(format!("{prefix}{expression}"))
                } else {
                    self.braced_regions(active)
                }
            }
        }
    }
}

/// The ordinary `SIMetrix` JFET has a cubic gate saturation-current
/// temperature factor. Do not change other levels or explicit overrides.
fn materialize_jfet_defaults(statement: &str) -> Result<String> {
    let Some((_, kind)) = model_header(statement) else {
        return Ok(statement.to_owned());
    };
    if !matches!(kind.as_str(), "NJF" | "PJF") {
        return Ok(statement.to_owned());
    }
    let active = active_statement_prefix(statement).trim();
    let card = model_normalize_ngspice::model_card::parse_model_card(active, 1, &["XTI", "LEVEL"])?;
    if card
        .parameters
        .iter()
        .any(|field| field.name.eq_ignore_ascii_case("XTI"))
    {
        return Ok(active.to_owned());
    }
    if let Some(level) = card
        .parameters
        .iter()
        .rev()
        .find(|field| field.name.eq_ignore_ascii_case("LEVEL"))
    {
        let value = level
            .expression
            .as_deref()
            .context("JFET LEVEL requires a value")?;
        let level = model_normalize_ngspice::param_expression::SpiceExpression::parse(value)?
            .evaluate(|name| bail!("unresolved JFET LEVEL symbol {name}"))
            .context("JFET LEVEL must be known to select its source defaults")?;
        if level.to_bits() != 1.0f64.to_bits() {
            return Ok(active.to_owned());
        }
    }
    // The lexer has already established the family and wrapper boundaries.
    let family = active
        .split_whitespace()
        .nth(2)
        .context("missing JFET family")?;
    let offset = family.as_ptr().addr() - active.as_ptr().addr() + kind.len();
    let tail = active[offset..].trim_start();
    if tail.starts_with('(') {
        let body = active
            .strip_suffix(')')
            .context("missing model wrapper terminator")?;
        Ok(format!("{body} XTI=3)"))
    } else {
        Ok(format!("{active} XTI=3"))
    }
}

pub(super) fn behavioral_rhs(statement: &str) -> Option<(&str, &str)> {
    let mut fields = statement.split_whitespace();
    let kind = fields.next()?.as_bytes().first()?.to_ascii_uppercase();
    if !matches!(kind, b'B' | b'E' | b'G' | b'F' | b'H') {
        return None;
    }
    // The first three fields are the instance name and output terminals.
    fields.next()?;
    let last_node = fields.next()?;
    let start = last_node.as_ptr().addr() - statement.as_ptr().addr() + last_node.len();
    let rest = statement[start..].trim_start();
    let (key, value) = rest.split_once('=')?;
    if !["V", "I", "VALUE", "VOL", "CUR"]
        .iter()
        .any(|candidate| key.trim().eq_ignore_ascii_case(candidate))
    {
        return None;
    }
    let offset = value.as_ptr().addr() - statement.as_ptr().addr();
    Some((&statement[..offset], value))
}

fn root_temperature(items: &[Item]) -> Result<f64> {
    let mut temp: Option<f64> = None;
    for item in items {
        let Item::Statement(statement) = item else {
            continue;
        };
        let value = match directive_name(statement).as_deref() {
            Some(".TEMP") => {
                let normalized = normalized_statement(statement);
                Some(
                    active_statement_prefix(&normalized)
                        .split_once(char::is_whitespace)
                        .context("missing .TEMP value")?
                        .1
                        .trim()
                        .to_owned(),
                )
            }
            Some(".OPTIONS" | ".OPTION" | ".OPT") => {
                super::simple_assignment_value(statement, "TEMP")
            }
            _ => None,
        };
        if let Some(value) = value {
            let value = parse_numeric(&value)
                .context("SIMetrix static temperature requires one numeric analysis temperature")?;
            ensure!(
                value.is_finite() && value > -273.15,
                "invalid SIMetrix analysis temperature"
            );
            ensure!(
                temp.is_none_or(|old| old.to_bits() == value.to_bits()),
                "conflicting SIMetrix analysis temperatures"
            );
            temp = Some(value);
        }
    }
    Ok(temp.unwrap_or(27.0))
}

#[derive(Clone, Debug)]
struct ModelTemperature {
    device: u8,
    value: String,
}

type Temperatures = BTreeMap<String, ModelTemperature>;

fn temperature_device(kind: &str) -> Option<u8> {
    match kind {
        "NPN" | "PNP" => Some(b'Q'),
        "D" => Some(b'D'),
        "R" | "RES" => Some(b'R'),
        _ => None,
    }
}

fn collect_model_temperatures(items: &[Item], map: &mut Temperatures) -> Result<()> {
    for item in items {
        let Item::Statement(statement) = item else {
            continue;
        };
        let Some((name, kind)) = model_header(statement) else {
            continue;
        };
        if let Some(device) = temperature_device(&kind)
            && let Some(value) = super::simple_assignment_value(statement, "T_ABS")
        {
            // Native diode and BJT instances cannot run below absolute zero.
            // Resistor models intentionally retain the existing SIMetrix
            // temperature convention, including negative absolute values.
            if let Ok(numeric) = parse_numeric(&value) {
                ensure!(
                    numeric.is_finite() && (device == b'R' || numeric > -273.15),
                    "invalid SIMetrix model temperature {value} for {name}"
                );
            }
            ensure!(
                map.insert(name.clone(), ModelTemperature { device, value })
                    .is_none(),
                "duplicate model temperature for {name}"
            );
        }
    }
    Ok(())
}

fn lower_model_temperature(statement: &str, models: &Temperatures) -> Result<String> {
    if let Some((_, kind)) = model_header(statement)
        && temperature_device(&kind).is_some()
        && super::simple_assignment_value(statement, "T_ABS").is_some()
    {
        return Ok(super::remove_simple_assignment(statement, "T_ABS"));
    }
    let trimmed = statement.trim_start();
    let Some(device) = trimmed.as_bytes().first().map(u8::to_ascii_uppercase) else {
        return Ok(statement.to_owned());
    };
    let indices: &[usize] = match device {
        b'Q' => &[4, 5],
        b'D' => &[3],
        b'R' => &[3, 4],
        _ => return Ok(statement.to_owned()),
    };
    if super::simple_assignment_value(statement, "TEMP").is_some() {
        return Ok(statement.to_owned());
    }
    let normalized = normalized_statement(statement);
    let active = active_statement_prefix(&normalized);
    let tokens = statement_tokens(active)?;
    let model = indices
        .iter()
        .filter_map(|index| tokens.get(*index))
        .filter_map(|token| models.get(&token.to_ascii_uppercase()))
        .find(|model| model.device == device);
    Ok(model.map_or_else(
        || statement.to_owned(),
        |model| format!("{active} TEMP={}", model.value),
    ))
}

fn lower_scope(
    items: &mut [Item],
    inherited: &Temperatures,
    builtins: &mut Builtins,
) -> Result<()> {
    let mut local = Temperatures::new();
    collect_model_temperatures(items, &mut local)?;
    let mut visible = inherited.clone();
    // A local model without T_ABS also shadows an outer model with it.
    for item in items.iter() {
        if let Item::Statement(statement) = item
            && let Some((name, _)) = model_header(statement)
        {
            visible.remove(&name);
        }
    }
    visible.extend(local);
    for item in items {
        match item {
            Item::Statement(statement) => {
                *statement = builtins.statement(&lower_model_temperature(statement, &visible)?)?;
            }
            Item::Subcircuit(block) => {
                block.header = builtins.statement(&block.header)?;
                lower_scope(&mut block.body, &visible, builtins)?;
            }
        }
    }
    Ok(())
}

fn render_items(items: &[Item]) -> Result<Vec<u8>> {
    let mut output = String::new();
    for item in items {
        match item {
            Item::Statement(statement) => writeln!(output, "{statement}")?,
            Item::Subcircuit(block) => render_native_subcircuit(block, &mut output)?,
        }
    }
    Ok(output.into_bytes())
}

/// Prepare a linked `SIMetrix` deck for a nominal native analysis.
///
/// # Errors
/// Rejects unsupported statistical analysis and invalid expressions or temperatures.
pub fn lower_simetrix_linked_runtime(linked: &mut LinkedPreprocessedNetlist) -> Result<()> {
    let mut root = parse_items(std::str::from_utf8(&linked.root_bytes)?)?;
    let mut libraries = linked
        .libraries
        .iter()
        .map(|library| parse_items(std::str::from_utf8(&library.bytes)?))
        .collect::<Result<Vec<_>>>()?;
    let mut globals = Temperatures::new();
    collect_model_temperatures(&root, &mut globals)?;
    for items in &libraries {
        collect_model_temperatures(items, &mut globals)?;
    }
    let temp = root_temperature(&root)?;
    let mut builtins = Builtins::new(linked);
    lower_scope(&mut root, &globals, &mut builtins)?;
    for items in &mut libraries {
        lower_scope(items, &globals, &mut builtins)?;
    }
    let mut prelude = Vec::new();
    if builtins.static_temp {
        prelude.push(Item::Statement(format!(
            ".param {}={temp}",
            builtins.temp_name
        )));
    }
    // .OPTIONS is stripped by native translation, so materialize its
    // temperature as a native .TEMP card too.
    if !root.iter().any(|item| matches!(item, Item::Statement(statement) if directive_name(statement).as_deref() == Some(".TEMP"))) && temp.to_bits() != 27.0f64.to_bits() {
        prelude.push(Item::Statement(format!(".temp {temp}")));
    }
    if builtins.lncosh {
        prelude.push(Item::Statement(format!(".func {}(x) {{abs(x)<0.01 ? x*x*(0.5-x*x/12+x*x*x*x/45) : abs(x)+ln(1+exp(-2*abs(x)))-ln(2)}}", builtins.lncosh_name)));
    }
    prelude.append(&mut root);
    linked.root_bytes = render_items(&prelude)?;
    for (library, items) in linked.libraries.iter_mut().zip(libraries) {
        library.bytes = render_items(&items)?;
    }
    Ok(())
}

#[cfg(test)]
mod jfet_tests {
    use super::*;

    #[test]
    fn ordinary_jfet_temperature_default_preserves_explicit_models() -> Result<()> {
        for (source, expected) in [
            (".model JX PJF(IS=15p)", ".model JX PJF(IS=15p XTI=3)"),
            (".model JX NJF LEVEL=1", ".model JX NJF LEVEL=1 XTI=3"),
            (".model JX NJF(IS=15p XTI=0)", ".model JX NJF(IS=15p XTI=0)"),
            (".model JX PJF(LEVEL=2)", ".model JX PJF(LEVEL=2)"),
            (".model JX NJF(XTI 0)", ".model JX NJF(XTI 0)"),
            (".model JX NJF VTO=abs(1)", ".model JX NJF VTO=abs(1) XTI=3"),
            (".model DX D(IS=15p)", ".model DX D(IS=15p)"),
            (
                ".model JX NJF(IS=15p) ; XTI=0",
                ".model JX NJF(IS=15p XTI=3)",
            ),
            (".model XTI NJF(IS=15p)", ".model XTI NJF(IS=15p XTI=3)"),
            (
                ".model JX NJF(IS={xti+1}, LEVEL=1)",
                ".model JX NJF(IS={xti+1}, LEVEL=1 XTI=3)",
            ),
            (
                ".model JX NJF(IS=15p, XTI = {2+1})",
                ".model JX NJF(IS=15p, XTI = {2+1})",
            ),
            (".model JX NJF(LEVEL 2)", ".model JX NJF(LEVEL 2)"),
            (".model JX NJF", ".model JX NJF XTI=3"),
        ] {
            assert_eq!(materialize_jfet_defaults(source)?, expected);
        }
        Ok(())
    }

    #[test]
    fn malformed_or_unresolved_jfet_level_is_not_guessed() {
        for source in [
            ".model JX NJF(LEVEL)",
            ".model JX NJF(LEVEL={unknown})",
            ".model JX NJF(XTI=)",
        ] {
            assert!(materialize_jfet_defaults(source).is_err(), "{source}");
        }
    }
}
