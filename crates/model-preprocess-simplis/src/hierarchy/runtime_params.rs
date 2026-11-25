//! `SIMetrix` runtime instance parameter binding.
//!
//! Actuals are evaluated in source order in the caller, with earlier actuals
//! visible; callee defaults and local declarations are not visible until
//! entering the body. Native ngspice instead sorts the complete formal list
//! and searches dynamically through enclosing subcircuit parameter scopes.
//! Use caller-local argument temporaries and hygienic callee-local names to
//! preserve the source binding, without evaluating electrical expressions.

use super::{active_statement_prefix, logical_statements, lower_hex};
use crate::{find_ascii_case_insensitive, parameter_assignments};
use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

type Names = BTreeMap<String, String>;

#[derive(Clone, Debug, Default)]
pub(super) struct RuntimeScope {
    identity: String,
    names: Names,
}

fn generated_name(prefix: &str, identity: &str, name: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(identity.as_bytes());
    digest.update([0]);
    digest.update(name.to_ascii_uppercase().as_bytes());
    format!("{prefix}_{}", &lower_hex(&digest.finalize())[..24])
}

const fn identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$' | b'.')
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
fn rewrite_expression(input: &str, names: &Names) -> Result<String> {
    if names.is_empty() {
        return Ok(input.to_owned());
    }
    let bytes = input.as_bytes();
    let mut output = String::with_capacity(input.len());
    let mut offset = 0;
    while offset < bytes.len() {
        let byte = bytes[offset];
        if byte.is_ascii_digit()
            || (byte == b'.' && bytes.get(offset + 1).is_some_and(u8::is_ascii_digit))
        {
            let end = number_end(bytes, offset);
            output.push_str(&input[offset..end]);
            offset = end;
            continue;
        }
        if byte == b'"' {
            let end = input[offset + 1..]
                .find('"')
                .map(|index| index + offset + 2)
                .context("unclosed string in runtime expression")?;
            output.push_str(&input[offset..end]);
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
                // V/I probes name electrical objects, not parameters.
                if name.eq_ignore_ascii_case("V") || name.eq_ignore_ascii_case("I") {
                    let close = closing_parenthesis(bytes, next)?;
                    output.push_str(&input[offset..=close]);
                    offset = close + 1;
                    continue;
                }
                output.push_str(name);
            } else if let Some(replacement) = names.get(&name.to_ascii_uppercase()) {
                output.push_str(replacement);
            } else {
                output.push_str(name);
            }
            offset = end;
            continue;
        }
        let character = input[offset..]
            .chars()
            .next()
            .context("expression character")?;
        output.push(character);
        offset += character.len_utf8();
    }
    Ok(output)
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
    pub(super) fn root(identity: &str) -> Self {
        Self {
            identity: identity.to_owned(),
            names: Names::new(),
        }
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
    ) -> Result<(Vec<u8>, Self)> {
        let text = std::str::from_utf8(input)?;
        let lines = text.lines().map(str::to_owned).collect::<Vec<_>>();
        let statements = logical_statements(&lines);
        let (header, body) = statements
            .split_first()
            .context("empty runtime subcircuit")?;
        let (prefix, defaults) = header_parameters(header)?;
        let actuals = parameter_assignments(actual_text)?;
        let mut scope = Self::root(identity);
        let mut formal_names = BTreeSet::new();
        for (name, _) in &defaults {
            if !formal_names.insert(name.to_ascii_uppercase()) {
                bail!("duplicate subcircuit formal {name}");
            }
        }
        let mut implicit = BTreeSet::new();
        for (name, _) in &actuals {
            let key = name.to_ascii_uppercase();
            if formal_names.insert(key.clone()) {
                implicit.insert(key);
            }
        }
        for key in &formal_names {
            scope
                .names
                .insert(key.clone(), generated_name("srp", identity, key));
        }
        for statement in body {
            if let Some(tail) = param_tail(statement) {
                for (name, _) in parameter_assignments(tail)? {
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
        let mut output = prefix.to_owned();
        if !formal_names.is_empty() {
            output.push_str(" params:");
            for (name, value) in defaults {
                let target = scope.lookup(name)?;
                let value = rewrite_expression(value, &scope.names)?;
                write!(output, " {target}={value}")?;
            }
            for key in implicit {
                let target = scope.lookup(&key)?;
                let required = generated_name("sr_required", identity, &key);
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
        let mut visible = self.names.clone();
        let mut actuals = BTreeMap::new();
        let mut declarations = String::new();
        for (index, (name, expression)) in
            parameter_assignments(actual_text)?.into_iter().enumerate()
        {
            let temporary = generated_name("sra", &self.identity, &format!("{instance}:{index}"));
            let expression = rewrite_expression(expression, &visible)?;
            writeln!(declarations, ".param {temporary}={}", braced(&expression))?;
            visible.insert(name.to_ascii_uppercase(), temporary.clone());
            actuals.insert(callee.lookup(name)?, temporary);
        }
        let mut arguments = String::new();
        for (name, temporary) in actuals {
            write!(arguments, " {name}={{{temporary}}}")?;
        }
        Ok((declarations, arguments))
    }

    pub(super) fn rewrite_statement(&self, statement: &str) -> Result<String> {
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
                    rewrite_expression(value, &self.names)?
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
                rewrite_expression(&trimmed[close + 1..], &names)?
            ));
        }
        rewrite_device_expressions(active, &self.names)
    }
}

/// Rewrite explicit expression regions of native device statements. The
/// structural device/model/node fields are left intact. Unbraced behavioural
/// RHS expressions are supported in addition to ordinary brace expressions.
fn rewrite_device_expressions(statement: &str, names: &Names) -> Result<String> {
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
                rewrite_expression(&trimmed[eq + 1..], names)?
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
        output.push_str(&rewrite_expression(&statement[start + 1..end], names)?);
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
        let parent = RuntimeScope::root("caller");
        let (body, callee) = RuntimeScope::prepare(
            b".subckt T a b params: r=5k q={2*r}\nR1 a b {q}\n.ends\n",
            "callee",
            "q={2*r} r=2k q={r+1}",
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
    fn electrical_names_and_function_arguments_are_not_parameter_symbols() -> Result<()> {
        let (_, scope) = RuntimeScope::prepare(
            b".subckt T p n\nB1 n 0 V=gain*V(p)+p+1e-9\n.ends\n",
            "scope",
            "gain=3 p=9 e=2",
        )?;
        let gain = scope.lookup("gain")?;
        let p = scope.lookup("p")?;
        assert_eq!(
            scope.rewrite_statement("B1 n 0 V=gain*V(p)+p+1e-9")?,
            format!("B1 n 0 V={gain}*V(p)+{p}+1e-9")
        );
        assert_eq!(
            scope.rewrite_statement(".func f(p) {p*gain}")?,
            format!(".func f(p) {{p*{gain}}}")
        );
        Ok(())
    }
}
