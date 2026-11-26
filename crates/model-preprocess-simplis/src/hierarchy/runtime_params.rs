//! `SIMetrix` runtime instance parameter binding.
//!
//! Actuals are evaluated in source order in the caller, with earlier actuals
//! visible; callee defaults and local declarations are not visible until
//! entering the body. Native ngspice instead sorts the complete formal list
//! and searches dynamically through enclosing subcircuit parameter scopes.
//! Use caller-local argument temporaries and hygienic callee-local names to
//! preserve the source binding, without evaluating electrical expressions.

use super::{active_statement_prefix, logical_statements, lower_hex};
use crate::{find_ascii_case_insensitive, parameter_assignments, validate_simetrix_parameter_name};
use anyhow::{Context, Result, bail, ensure};
use model_preprocess_command::{Resource, WorkBudget};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::sync::Arc;

type Names = BTreeMap<String, String>;

#[derive(Clone, Debug, Default)]
struct Symbols {
    parameters: BTreeSet<String>,
    models: BTreeSet<String>,
}

impl Symbols {
    fn collect(statements: &[String], budget: &WorkBudget) -> Result<Self> {
        let mut symbols = Self::default();
        for statement in statements {
            budget.charge(Resource::Statements, 1)?;
            budget.charge(Resource::ScannedBytes, statement.len())?;
            if let Some(tail) = param_tail(statement) {
                for (name, _) in parameter_assignments(tail)? {
                    budget.charge(Resource::GeneratedNodes, 1)?;
                    budget.charge(Resource::EmittedBytes, name.len())?;
                    symbols.parameters.insert(name.to_ascii_uppercase());
                }
            }
            if let Some(name) = model_name(statement) {
                budget.charge(Resource::GeneratedNodes, 1)?;
                budget.charge(Resource::EmittedBytes, name.len())?;
                symbols.models.insert(name.to_ascii_uppercase());
            }
        }
        Ok(symbols)
    }
}

#[derive(Clone, Debug, Default)]
pub(super) struct RuntimeScope {
    identity: String,
    budget: WorkBudget,
    names: Names,
    local_models: BTreeSet<String>,
    root_symbols: Arc<Symbols>,
    source_symbols: Symbols,
}

fn generated_name(prefix: &str, identity: &str, name: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(identity.as_bytes());
    digest.update([0]);
    digest.update(name.to_ascii_uppercase().as_bytes());
    format!("{prefix}_{}", &lower_hex(&digest.finalize())[..24])
}

const fn identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$' | b'.' | b'#' | b'[' | b']')
}

fn number_end(bytes: &[u8], start: usize) -> usize {
    let mut end = start;
    while let Some(&byte) = bytes.get(end) {
        if byte.is_ascii_alphanumeric()
            || byte == b'.'
            || (matches!(byte, b'+' | b'-') && end > start && matches!(bytes[end - 1], b'e' | b'E'))
        {
            end += 1;
        } else {
            break;
        }
    }
    end
}

fn closing_parenthesis(bytes: &[u8], open: usize) -> Result<usize> {
    let mut depth = 0usize;
    for (index, byte) in bytes.iter().enumerate().skip(open) {
        match byte {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(index);
                }
            }
            _ => {}
        }
    }
    bail!("unclosed parenthesis in runtime expression")
}

/// Rename parameter symbols, not numeric suffixes, function names, string
/// literals, or node/current probe arguments. Expressions may contain braces
/// or native behavioural expression syntax; no numeric evaluation is done.
fn rewrite_expression(input: &str, names: &Names, budget: &WorkBudget) -> Result<String> {
    rewrite_symbols(input, names, false, budget)
}

/// An occurrence-specific name and source arity of a user function.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FunctionAlias {
    /// Native declaration identifier.
    pub name: String,
    /// Number of formal parameters.
    pub arity: usize,
}

/// An expression with its resolved user-function dependency edges.
#[derive(Debug)]
pub struct BoundExpression {
    /// Rewritten expression, preserving non-symbol source text.
    pub text: String,
    /// Case-folded source identifiers of referenced user functions.
    pub calls: BTreeSet<String>,
    /// Calls not matched by the supplied function environment (including builtins).
    pub unbound_calls: BTreeSet<String>,
}

pub(super) fn rewrite_symbols(
    input: &str,
    names: &Names,
    preserve_assignments: bool,
    budget: &WorkBudget,
) -> Result<String> {
    if names.is_empty() {
        budget.charge(Resource::EvaluationSteps, input.len())?;
        budget.charge(Resource::EmittedBytes, input.len())?;
        return Ok(input.to_owned());
    }
    Ok(rewrite_symbols_inner(input, names, None, preserve_assignments, budget)?.text)
}

fn checked_arity(
    input: &str,
    name: &str,
    open: usize,
    arity: usize,
    budget: &WorkBudget,
) -> Result<()> {
    let close = crate::source_dialect::group_end(input, open, budget)?;
    let body = &input[open + 1..close];
    budget.charge(Resource::EvaluationSteps, body.len())?;
    let arguments = crate::source_dialect::split_arguments(body, budget)?;
    let count = if body.trim().is_empty() {
        0
    } else {
        ensure!(
            arguments.iter().all(|arg| !arg.is_empty()),
            "empty argument to function {name}"
        );
        arguments.len()
    };
    ensure!(
        count == arity,
        "function {name} expects {arity} arguments, got {count}"
    );
    Ok(())
}

/// Shared expression token walker. Assignment destinations, numeric suffixes
/// and electrical probe arguments remain untouched. Function references use
/// a separate namespace from scalar parameters; their arity is checked
/// without expanding or evaluating the expression.
pub(super) fn rewrite_bound_symbols(
    input: &str,
    names: &Names,
    functions: &BTreeMap<String, FunctionAlias>,
    preserve_assignments: bool,
    budget: &WorkBudget,
) -> Result<BoundExpression> {
    rewrite_symbols_inner(input, names, Some(functions), preserve_assignments, budget)
}

fn rewrite_symbols_inner(
    input: &str,
    names: &Names,
    functions: Option<&BTreeMap<String, FunctionAlias>>,
    preserve_assignments: bool,
    budget: &WorkBudget,
) -> Result<BoundExpression> {
    budget.charge(Resource::EvaluationSteps, input.len())?;
    budget.charge(Resource::EmittedBytes, input.len())?;
    let mut result = BoundExpression {
        text: String::with_capacity(input.len()),
        calls: BTreeSet::new(),
        unbound_calls: BTreeSet::new(),
    };
    let bytes = input.as_bytes();
    let mut offset = 0;
    while offset < bytes.len() {
        let byte = bytes[offset];
        if byte.is_ascii_digit()
            || (byte == b'.' && bytes.get(offset + 1).is_some_and(u8::is_ascii_digit))
        {
            let end = number_end(bytes, offset);
            result.text.push_str(&input[offset..end]);
            offset = end;
            continue;
        }
        if byte == b'"' {
            let end = input[offset + 1..]
                .find('"')
                .map(|index| index + offset + 2)
                .context("unclosed string in runtime expression")?;
            result.text.push_str(&input[offset..end]);
            offset = end;
            continue;
        }
        if byte.is_ascii_alphabetic() || byte == b'_' {
            let mut end = offset + 1;
            while bytes.get(end).copied().is_some_and(identifier_byte) {
                end += 1;
            }
            let name = &input[offset..end];
            let mut next = end;
            while bytes.get(next).is_some_and(u8::is_ascii_whitespace) {
                next += 1;
            }
            if bytes.get(next) == Some(&b'(') {
                if name.eq_ignore_ascii_case("V") || name.eq_ignore_ascii_case("I") {
                    let close = crate::source_dialect::group_end(input, next, budget)?;
                    result.text.push_str(&input[offset..=close]);
                    offset = close + 1;
                    continue;
                }
                if let Some(functions) = functions {
                    let key = name.to_ascii_uppercase();
                    if let Some(function) = functions.get(&key) {
                        checked_arity(input, name, next, function.arity, budget)?;
                        budget.charge(
                            Resource::EmittedBytes,
                            function.name.len().saturating_sub(name.len()),
                        )?;
                        budget.charge(Resource::GeneratedNodes, 1)?;
                        result.text.push_str(&function.name);
                        // Native SPICE call syntax has no intervening whitespace.
                        result.calls.insert(key);
                        offset = next;
                        continue;
                    }
                    budget.charge(Resource::GeneratedNodes, 1)?;
                    result.unbound_calls.insert(key);
                }
                result.text.push_str(name);
            } else if preserve_assignments
                && bytes.get(next) == Some(&b'=')
                && bytes.get(next + 1) != Some(&b'=')
            {
                result.text.push_str(name);
            } else if let Some(replacement) = names.get(&name.to_ascii_uppercase()) {
                budget.charge(
                    Resource::EmittedBytes,
                    replacement.len().saturating_sub(name.len()),
                )?;
                result.text.push_str(replacement);
            } else {
                result.text.push_str(name);
            }
            offset = end;
            continue;
        }
        let character = input[offset..]
            .chars()
            .next()
            .context("expression character")?;
        result.text.push(character);
        offset += character.len_utf8();
    }
    Ok(result)
}

fn param_tail(statement: &str) -> Option<&str> {
    let active = active_statement_prefix(statement).trim_start();
    let (directive, tail) = active.split_once(char::is_whitespace)?;
    directive
        .eq_ignore_ascii_case(".param")
        .then_some(tail.trim())
}

fn header_parameters(header: &str) -> Result<(&str, Vec<(&str, &str)>)> {
    if let Some(offset) = find_ascii_case_insensitive(header, " params:") {
        return Ok((
            header[..offset].trim_end(),
            parameter_assignments(header[offset + " params:".len()..].trim())?,
        ));
    }
    // Native-style headers may omit PARAMS:.
    let mut cursor = 0;
    let mut previous_offset = 0;
    for token in header.split_whitespace() {
        let offset = cursor + header[cursor..].find(token).context("header token")?;
        if token.contains('=') {
            let assignment_offset = if token.starts_with('=') {
                previous_offset
            } else {
                offset
            };
            return Ok((
                header[..assignment_offset].trim_end(),
                parameter_assignments(&header[assignment_offset..])?,
            ));
        }
        previous_offset = offset;
        cursor = offset + token.len();
    }
    Ok((header, Vec::new()))
}

fn braced(expression: &str) -> String {
    let expression = expression.trim();
    if expression.starts_with('{') && expression.ends_with('}') {
        expression.to_owned()
    } else {
        format!("{{{expression}}}")
    }
}

impl RuntimeScope {
    pub(super) fn root(identity: &str, budget: &WorkBudget) -> Self {
        Self {
            identity: identity.to_owned(),
            budget: budget.clone(),
            ..Self::default()
        }
    }

    pub(super) fn root_with_symbols(
        identity: &str,
        statements: &[String],
        budget: &WorkBudget,
    ) -> Result<Self> {
        Ok(Self {
            root_symbols: Arc::new(Symbols::collect(statements, budget)?),
            ..Self::root(identity, budget)
        })
    }

    pub(super) fn set_source_scope(
        &mut self,
        parent: &Self,
        source_statements: &[String],
    ) -> Result<()> {
        self.root_symbols = Arc::clone(&parent.root_symbols);
        self.source_symbols = Symbols::collect(source_statements, &self.budget)?;
        Ok(())
    }

    // A bare R/C/L value can be a parameter, but a visible model with the
    // same name takes precedence. Never rewrite an electrical node or a
    // reserved single-letter device-value keyword.
    pub(super) fn rewrite_bare_passive(
        &self,
        statement: &str,
        catalog_model: impl FnOnce(&str) -> Result<bool>,
    ) -> Result<String> {
        self.budget
            .charge(Resource::ScannedBytes, statement.len())?;
        self.budget
            .charge(Resource::EmittedBytes, statement.len())?;
        let Some(value) = bare_passive_value(statement) else {
            return Ok(statement.to_owned());
        };
        let key = value.to_ascii_uppercase();
        let bound = self.names.get(&key).map(String::as_str).or_else(|| {
            (self.root_symbols.parameters.contains(&key)
                || self.source_symbols.parameters.contains(&key))
            .then_some(value)
        });
        let Some(bound) = bound else {
            return Ok(statement.to_owned());
        };
        if self.local_models.contains(&key)
            || self.root_symbols.models.contains(&key)
            || self.source_symbols.models.contains(&key)
            || catalog_model(value)?
        {
            return Ok(statement.to_owned());
        }
        self.budget.charge(
            Resource::EmittedBytes,
            bound.len().saturating_add(2).saturating_sub(value.len()),
        )?;
        let start = value.as_ptr().addr() - statement.as_ptr().addr();
        Ok(format!(
            "{}{{{bound}}}{}",
            &statement[..start],
            &statement[start + value.len()..]
        ))
    }

    pub(super) fn has_bindings(&self) -> bool {
        !self.names.is_empty()
    }

    /// Declare each implicit actual in this call shape, with an undefined
    /// required-argument marker instead of an invented numerical default.
    /// The linker always supplies all of these formals. A different actual
    /// set receives a different definition identity.
    pub(super) fn prepare(
        input: &[u8],
        identity: &str,
        actual_text: &str,
        budget: &WorkBudget,
    ) -> Result<(Vec<u8>, Self)> {
        budget.charge(Resource::ScannedBytes, input.len())?;
        budget.charge(Resource::EvaluationSteps, actual_text.len())?;
        let text = std::str::from_utf8(input)?;
        budget.charge(Resource::EmittedBytes, input.len().saturating_mul(2))?;
        budget.charge(Resource::GeneratedNodes, text.lines().count())?;
        let lines = text.lines().map(str::to_owned).collect::<Vec<_>>();
        let statements = logical_statements(&lines);
        let (header, body) = statements
            .split_first()
            .context("empty runtime subcircuit")?;
        let (prefix, defaults) = header_parameters(header)?;
        let actuals = parameter_assignments(actual_text)?;
        let mut scope = Self::root(identity, budget);
        scope.local_models = body
            .iter()
            .filter_map(|statement| model_name(statement))
            .map(str::to_ascii_uppercase)
            .collect();
        let mut formal_names = BTreeSet::new();
        for (name, _) in &defaults {
            budget.charge(Resource::GeneratedNodes, 1)?;
            budget.charge(Resource::EmittedBytes, name.len())?;
            validate_simetrix_parameter_name(name)?;
            if !formal_names.insert(name.to_ascii_uppercase()) {
                bail!("duplicate subcircuit formal {name}");
            }
        }
        let mut implicit = BTreeSet::new();
        for (name, _) in &actuals {
            budget.charge(Resource::GeneratedNodes, 1)?;
            budget.charge(Resource::EmittedBytes, name.len().saturating_mul(2))?;
            validate_simetrix_parameter_name(name)?;
            let key = name.to_ascii_uppercase();
            if formal_names.insert(key.clone()) {
                implicit.insert(key);
            }
        }
        for key in &formal_names {
            budget.charge(Resource::GeneratedNodes, 1)?;
            budget.charge(Resource::EmittedBytes, key.len().saturating_add(28))?;
            scope
                .names
                .insert(key.clone(), generated_name("srp", identity, key));
        }
        for statement in body {
            budget.charge(Resource::Statements, 1)?;
            budget.charge(Resource::ScannedBytes, statement.len())?;
            if let Some(tail) = param_tail(statement) {
                for (name, _) in parameter_assignments(tail)? {
                    validate_simetrix_parameter_name(name)?;
                    budget.charge(Resource::GeneratedNodes, 1)?;
                    budget.charge(
                        Resource::EmittedBytes,
                        name.len().saturating_mul(2).saturating_add(28),
                    )?;
                    let key = name.to_ascii_uppercase();
                    if scope.names.contains_key(&key) {
                        bail!("subcircuit parameter {name} has already been defined");
                    }
                    scope
                        .names
                        .insert(key.clone(), generated_name("srp", identity, &key));
                }
            }
        }

        if !scope.has_bindings() {
            return Ok((input.to_vec(), scope));
        }
        budget.charge(Resource::EmittedBytes, input.len())?;
        let mut output = prefix.to_owned();
        if !formal_names.is_empty() {
            output.push_str(" params:");
            for (name, value) in defaults {
                let target = scope.lookup(name)?;
                let value = rewrite_expression(value, &scope.names, budget)?;
                budget.charge(
                    Resource::EmittedBytes,
                    target.len().saturating_add(value.len()).saturating_add(2),
                )?;
                write!(output, " {target}={value}")?;
            }
            for key in implicit {
                let target = scope.lookup(&key)?;
                let required = generated_name("sr_required", identity, &key);
                budget.charge(
                    Resource::EmittedBytes,
                    target
                        .len()
                        .saturating_add(required.len())
                        .saturating_add(4),
                )?;
                write!(output, " {target}={{{required}}}")?;
            }
        }
        output.push('\n');
        for statement in body {
            writeln!(output, "{statement}")?;
        }
        Ok((output.into_bytes(), scope))
    }

    fn lookup(&self, name: &str) -> Result<&str> {
        self.names
            .get(&name.to_ascii_uppercase())
            .map(String::as_str)
            .with_context(|| format!("unbound runtime parameter {name}"))
    }

    /// Evaluate the actual list in the caller. The source order matters even
    /// for repeated names and for a forward reference shadowed by a later
    /// actual. Each expression is given a fresh symbolic binding.
    pub(super) fn bind_actuals(
        &self,
        callee: &Self,
        instance: &str,
        actual_text: &str,
    ) -> Result<(String, String)> {
        self.budget
            .charge(Resource::GeneratedNodes, self.names.len())?;
        for (key, value) in &self.names {
            self.budget.charge(
                Resource::EmittedBytes,
                key.len().saturating_add(value.len()),
            )?;
        }
        self.budget
            .charge(Resource::ScannedBytes, actual_text.len())?;
        let mut visible = self.names.clone();
        let mut actuals = BTreeMap::new();
        let mut declarations = String::new();
        for (index, (name, expression)) in
            parameter_assignments(actual_text)?.into_iter().enumerate()
        {
            self.budget.charge(Resource::GeneratedNodes, 2)?;
            self.budget
                .charge(Resource::EmittedBytes, name.len().saturating_add(56))?;
            let temporary = generated_name("sra", &self.identity, &format!("{instance}:{index}"));
            let expression = rewrite_expression(expression, &visible, &self.budget)?;
            self.budget.charge(
                Resource::EmittedBytes,
                temporary
                    .len()
                    .saturating_add(expression.len())
                    .saturating_add(11),
            )?;
            writeln!(declarations, ".param {temporary}={}", braced(&expression))?;
            visible.insert(name.to_ascii_uppercase(), temporary.clone());
            actuals.insert(callee.lookup(name)?, temporary);
        }
        let mut arguments = String::new();
        for (name, temporary) in actuals {
            self.budget.charge(
                Resource::EmittedBytes,
                name.len().saturating_add(temporary.len()).saturating_add(4),
            )?;
            write!(arguments, " {name}={{{temporary}}}")?;
        }
        Ok((declarations, arguments))
    }

    pub(super) fn rewrite_statement(&self, statement: &str) -> Result<String> {
        self.budget
            .charge(Resource::ScannedBytes, statement.len())?;
        self.budget
            .charge(Resource::EmittedBytes, statement.len())?;
        if !self.has_bindings() {
            return Ok(statement.to_owned());
        }
        let active = active_statement_prefix(statement);
        let trimmed = active.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('*') {
            return Ok(statement.to_owned());
        }
        if let Some(tail) = param_tail(active) {
            let mut output = String::from(".param");
            for (name, value) in parameter_assignments(tail)? {
                write!(
                    output,
                    " {}={}",
                    self.lookup(name)?,
                    rewrite_expression(value, &self.names, &self.budget)?
                )?;
            }
            return Ok(output);
        }
        if trimmed
            .get(..5)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(".func"))
        {
            let open = trimmed
                .find('(')
                .context("runtime .FUNC has no argument list")?;
            let close = closing_parenthesis(trimmed.as_bytes(), open)?;
            let mut names = self.names.clone();
            for argument in trimmed[open + 1..close].split(',') {
                names.remove(&argument.trim().to_ascii_uppercase());
            }
            return Ok(format!(
                "{}{}",
                &trimmed[..=close],
                rewrite_expression(&trimmed[close + 1..], &names, &self.budget)?
            ));
        }
        rewrite_device_expressions(active, &self.names, &self.budget)
    }
}

fn model_name(statement: &str) -> Option<&str> {
    let mut fields = active_statement_prefix(statement).split_whitespace();
    fields
        .next()?
        .eq_ignore_ascii_case(".model")
        .then(|| fields.next())
        .flatten()
}

fn bare_passive_value(statement: &str) -> Option<&str> {
    let mut fields = active_statement_prefix(statement).split_whitespace();
    let device = fields.next()?.as_bytes().first()?.to_ascii_uppercase();
    if !matches!(device, b'R' | b'C' | b'L') {
        return None;
    }
    let value = fields.nth(2)?;
    let bytes = value.as_bytes();
    let first = *bytes.first()?;
    if !(first.is_ascii_alphabetic() || first == b'_')
        || !bytes.iter().copied().all(identifier_byte)
        || (bytes.len() == 1 && first.to_ascii_uppercase() == device)
    {
        return None;
    }
    Some(value)
}

/// Rewrite explicit expression regions of native device statements. The
/// structural device/model/node fields are left intact. Unbraced behavioural
/// RHS expressions are supported in addition to ordinary brace expressions.
fn rewrite_device_expressions(
    statement: &str,
    names: &Names,
    budget: &WorkBudget,
) -> Result<String> {
    if let Some(tail) = super::capacitor_charge_assignments(statement) {
        let start = tail.as_ptr().addr() - statement.as_ptr().addr();
        let mut output = statement[..start].trim_end().to_owned();
        for (key, value) in parameter_assignments(tail)? {
            write!(
                output,
                " {key}={}",
                rewrite_expression(value, names, budget)?
            )?;
        }
        return Ok(output);
    }
    let trimmed = statement.trim_start();
    let device = trimmed
        .as_bytes()
        .first()
        .copied()
        .map(|b| b.to_ascii_uppercase());
    if matches!(
        device,
        Some(b'B' | b'E' | b'G' | b'F' | b'H' | b'R' | b'C' | b'L')
    ) && let Some(eq) = trimmed.find('=')
        && !trimmed[..eq].contains('{')
    {
        let key = trimmed[..eq]
            .split_whitespace()
            .next_back()
            .unwrap_or_default();
        if ["V", "I", "VALUE", "VOL", "CUR", "R", "C", "L"]
            .iter()
            .any(|k| key.eq_ignore_ascii_case(k))
        {
            return Ok(format!(
                "{}{}",
                &trimmed[..=eq],
                rewrite_expression(&trimmed[eq + 1..], names, budget)?
            ));
        }
    }
    let mut output = String::with_capacity(statement.len());
    let mut offset = 0;
    while let Some(relative) = statement[offset..].find(['{', '\'']) {
        let start = offset + relative;
        output.push_str(&statement[offset..=start]);
        let opener = statement.as_bytes()[start];
        let end = if opener == b'{' {
            crate::find_closing_brace(statement, start)?
        } else {
            statement[start + 1..]
                .find('\'')
                .map(|index| index + start + 1)
                .context("unterminated quoted runtime expression")?
        };
        output.push_str(&rewrite_expression(
            &statement[start + 1..end],
            names,
            budget,
        )?);
        output.push(char::from(statement.as_bytes()[end]));
        offset = end + 1;
    }
    output.push_str(&statement[offset..]);
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actuals_bind_in_source_order_without_callee_defaults() -> Result<()> {
        let parent = RuntimeScope::root("caller", &WorkBudget::default());
        let (body, callee) = RuntimeScope::prepare(
            b".subckt T a b params: r=5k q={2*r}\nR1 a b {q}\n.ends\n",
            "callee",
            "q={2*r} r=2k q={r+1}",
            &WorkBudget::default(),
        )?;
        let body = std::str::from_utf8(&body)?;
        let r = callee.lookup("r")?;
        let q = callee.lookup("q")?;
        assert!(body.contains(&format!("{q}={{2*{r}}}")));
        let (declarations, arguments) =
            parent.bind_actuals(&callee, "X1", "q={2*r} r=2k q={r+1}")?;
        let first = generated_name("sra", "caller", "X1:0");
        let second = generated_name("sra", "caller", "X1:1");
        let last = generated_name("sra", "caller", "X1:2");
        assert!(declarations.contains(&format!("{first}={{2*r}}")));
        assert!(declarations.contains(&format!("{last}={{{second}+1}}")));
        assert!(arguments.contains(&format!("{q}={{{last}}}")));
        assert!(!arguments.contains(&first));
        Ok(())
    }

    #[test]
    fn native_header_defaults_accept_whitespace_around_assignment() -> Result<()> {
        for header in [
            ".subckt T a b R=2k",
            ".subckt T a b R =2k",
            ".subckt T a b R = 2k",
            ".subckt T a b R= 2k",
        ] {
            assert_eq!(
                header_parameters(header)?,
                (".subckt T a b", vec![("R", "2k")])
            );
        }
        Ok(())
    }

    #[test]
    fn bare_passive_values_use_parameters_only_if_no_visible_model() -> Result<()> {
        let root = RuntimeScope::root_with_symbols(
            "root",
            &[
                ".param global_r=2k".into(),
                ".model global_model R(R=3k)".into(),
            ],
            &WorkBudget::default(),
        )?;
        let (_, mut scope) = RuntimeScope::prepare(
            b".subckt T p n params: local_r=2k global_model=7k external_model=8k\n.param local_model=5k\n.model local_model R(R=3k)\n.ends\n",
            "scope",
            "",
            &WorkBudget::default(),
        )?;
        scope.set_source_scope(&root, &[".param source_c=10n".into()])?;
        let local_r = scope.lookup("local_r")?;
        assert_eq!(
            scope.rewrite_bare_passive("R1 p n local_r", |_| Ok(false))?,
            format!("R1 p n {{{local_r}}}")
        );
        assert_eq!(
            scope.rewrite_bare_passive("R1 p n global_r TC=0", |_| Ok(false))?,
            "R1 p n {global_r} TC=0"
        );
        assert_eq!(
            scope.rewrite_bare_passive("C1 p n source_c", |_| Ok(false))?,
            "C1 p n {source_c}"
        );
        for model in ["local_model", "global_model", "external_model"] {
            let statement = format!("R1 p n {model}");
            assert_eq!(
                scope.rewrite_bare_passive(&statement, |name| Ok(name == "external_model"))?,
                statement
            );
        }
        assert_eq!(
            scope.rewrite_bare_passive("R1 local_r n 10k", |_| Ok(false))?,
            "R1 local_r n 10k"
        );
        Ok(())
    }

    #[test]
    fn charge_expression_bindings_preserve_electrical_names() -> Result<()> {
        let (_, scope) = RuntimeScope::prepare(
            b".subckt T p n params: gain=3 p=9 m=2\nC1 p n Q=(gain*V(p,n)+p) M={m}\n.ends\n",
            "charge-scope",
            "",
            &WorkBudget::default(),
        )?;
        let gain = scope.lookup("gain")?;
        let p = scope.lookup("p")?;
        let m = scope.lookup("m")?;
        for (left, right) in [("(", ")"), ("{", "}"), ("'", "'")] {
            let statement = format!("C1 p n Q={left}gain*V(p,n)+p{right} M={{m}}");
            assert_eq!(
                scope.rewrite_statement(&statement)?,
                format!("C1 p n Q={left}{gain}*V(p,n)+{p}{right} M={{{m}}}")
            );
        }
        Ok(())
    }

    #[test]
    fn user_function_bindings_distinguish_calls_parameters_and_probes() -> Result<()> {
        let names = BTreeMap::from([("F".into(), "scalar".into()), ("P".into(), "param".into())]);
        let functions = BTreeMap::from([(
            "F".into(),
            FunctionAlias {
                name: "func".into(),
                arity: 2,
            },
        )]);
        let bound = rewrite_bound_symbols(
            "f (p,f(p,2))+F+v(f)+i(p)+1f",
            &names,
            &functions,
            false,
            &WorkBudget::default(),
        )?;
        assert_eq!(bound.text, "func(param,func(param,2))+scalar+v(f)+i(p)+1f");
        assert_eq!(bound.calls, BTreeSet::from(["F".into()]));
        for invalid in ["f()", "f(1)", "f(1,2,3)", "f(1,)"] {
            assert!(
                rewrite_bound_symbols(invalid, &names, &functions, false, &WorkBudget::default())
                    .is_err()
            );
        }
        Ok(())
    }

    #[test]
    fn electrical_names_and_function_arguments_are_not_parameter_symbols() -> Result<()> {
        let (_, scope) = RuntimeScope::prepare(
            b".subckt T p n\nB1 n 0 V=gain*V(p)+p+1k\n.ends\n",
            "scope",
            "gain=3 p=9 k=2",
            &WorkBudget::default(),
        )?;
        let gain = scope.lookup("gain")?;
        let p = scope.lookup("p")?;
        assert_eq!(
            scope.rewrite_statement("B1 n 0 V=gain*V(p)+p+1k")?,
            format!("B1 n 0 V={gain}*V(p)+{p}+1k")
        );
        assert_eq!(
            scope.rewrite_statement(".func f(p) {p*gain}")?,
            format!(".func f(p) {{p*{gain}}}")
        );
        Ok(())
    }
}
