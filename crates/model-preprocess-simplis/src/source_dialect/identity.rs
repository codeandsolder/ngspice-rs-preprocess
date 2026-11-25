//! Device identity resolution after file inclusion but before subcircuit
//! specialization. Electrical names are scoped to their containing circuit.

use super::DialectContext;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fmt::Write as _};

/// Admitted logical source statement and its effective language.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ScopedStatement {
    /// Canonical admitted source identity.
    pub source: String,
    /// Effective file language, not the language of the root deck.
    pub context: DialectContext,
    /// Joined, projected source statement.
    pub text: String,
}

/// Source-level name mapping before subsequent primitive lowering and
/// subcircuit specialization. Scope consists of source definition names.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DeviceIdentity {
    pub source: String,
    pub scope: Vec<String>,
    pub declared: String,
    pub displayed: String,
    pub linked: String,
}

/// Rewritten source with source-level device identities.
#[derive(Clone, Debug)]
pub struct ResolvedDeviceSource {
    pub text: String,
    pub identities: Vec<DeviceIdentity>,
}

#[derive(Default)]
struct Scope {
    path: Vec<String>,
    displayed: BTreeMap<String, String>,
    declared: BTreeMap<String, String>,
    linked: BTreeMap<String, String>,
}

impl Scope {
    fn insert(&mut self, identity: &DeviceIdentity) -> Result<()> {
        let display = identity.displayed.to_ascii_uppercase();
        let declared = identity.declared.to_ascii_uppercase();
        let linked = identity.linked.to_ascii_uppercase();
        // All three namespaces are checked, so two different source names
        // can never become the same simulator name.
        ensure!(
            !self.displayed.contains_key(&display),
            "duplicate device identity {} in {:?}",
            identity.displayed,
            self.path
        );
        ensure!(
            !self.declared.contains_key(&declared),
            "duplicate device declaration {} in {:?}",
            identity.declared,
            self.path
        );
        ensure!(
            !self.linked.contains_key(&linked),
            "colliding linked device name {} in {:?}",
            identity.linked,
            self.path
        );
        self.displayed.insert(display, identity.linked.clone());
        self.declared.insert(declared, identity.linked.clone());
        self.linked.insert(linked, identity.declared.clone());
        Ok(())
    }

    fn resolve<'a>(&'a self, name: &'a str) -> Result<&'a str> {
        let key = name.to_ascii_uppercase();
        let displayed = self.displayed.get(&key);
        let declared = self.declared.get(&key);
        ensure!(
            displayed.is_none() || declared.is_none() || displayed == declared,
            "ambiguous device reference {name} in {:?}",
            self.path
        );
        Ok(displayed.or(declared).map_or(name, String::as_str))
    }
}

fn first_token(text: &str) -> Option<&str> {
    text.split_whitespace().next()
}

fn is_device(text: &str) -> bool {
    let mut fields = text.split_whitespace();
    let Some(name) = fields.next() else {
        return false;
    };
    let name = name.strip_prefix('!').unwrap_or(name);
    name.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
        && fields.next().is_some()
        && !text
            .trim_end()
            .to_ascii_uppercase()
            .ends_with(" SPICE MACRO-MODEL")
}

fn device_name(token: &str, context: DialectContext) -> Result<(String, String)> {
    let prefix = if token.starts_with('!') { "!" } else { "" };
    let raw = token.strip_prefix('!').unwrap_or(token);
    let displayed = context.device_identity(raw)?;
    let native_alias = raw.as_bytes().get(1) == Some(&b'$');
    let requires_encoding = (displayed != raw && context.separator != Some('$'))
        || (native_alias && context.separator != Some('$'));
    let linked = if requires_encoding {
        let mut name = format!("{prefix}{}__src_", &raw[..1]);
        for byte in raw.bytes() {
            write!(name, "{byte:02x}")?;
        }
        name
    } else {
        token.to_owned()
    };
    Ok((displayed.to_owned(), linked))
}

struct Scan {
    scopes: Vec<Scope>,
    assignments: Vec<(usize, Option<usize>)>,
    identities: Vec<DeviceIdentity>,
}

fn collect(statements: &[ScopedStatement]) -> Result<Scan> {
    let mut scopes = vec![Scope::default()];
    let mut stack = vec![0usize];
    let mut assignments = Vec::with_capacity(statements.len());
    let mut identities = Vec::new();
    let mut control = false;
    for line in statements {
        let scope = *stack.last().context("missing root identity scope")?;
        let first = first_token(&line.text).unwrap_or_default();
        let upper = first.to_ascii_uppercase();
        let mut identity = None;
        match upper.as_str() {
            ".CONTROL" => control = true,
            ".ENDC" => control = false,
            ".SUBCKT" if !control => {
                if let Some(name) = line.text.split_whitespace().nth(1) {
                    let mut path = scopes[scope].path.clone();
                    path.push(name.to_owned());
                    stack.push(scopes.len());
                    scopes.push(Scope {
                        path,
                        ..Scope::default()
                    });
                }
            }
            ".ENDS" if !control => {
                ensure!(
                    stack.len() > 1,
                    "unmatched .ends while resolving device identities"
                );
                stack.pop();
            }
            _ if !control && is_device(&line.text) => {
                let (displayed, linked) = device_name(first, line.context)?;
                let mapping = DeviceIdentity {
                    source: line.source.clone(),
                    scope: scopes[scope].path.clone(),
                    declared: first.to_owned(),
                    displayed,
                    linked,
                };
                scopes[scope].insert(&mapping)?;
                identity = Some(identities.len());
                identities.push(mapping);
            }
            _ => {}
        }
        assignments.push((scope, identity));
    }
    ensure!(
        stack.len() == 1,
        "unterminated subcircuit while resolving device identities"
    );
    Ok(Scan {
        scopes,
        assignments,
        identities,
    })
}

fn token_spans(text: &str) -> Vec<std::ops::Range<usize>> {
    let mut spans = Vec::new();
    let mut offset = 0;
    while offset < text.len() {
        let rest = &text[offset..];
        let trimmed = rest.trim_start();
        offset += rest.len() - trimmed.len();
        if trimmed.is_empty() {
            break;
        }
        let end = offset + trimmed.find(char::is_whitespace).unwrap_or(trimmed.len());
        spans.push(offset..end);
        offset = end;
    }
    spans
}

/// Rewrite positional control-device references without altering node names.
fn control_references(text: &str, identity: &DeviceIdentity, scope: &Scope) -> Result<String> {
    let tokens = token_spans(text);
    let kind = identity.linked.trim_start_matches('!').as_bytes()[0].to_ascii_uppercase();
    let mut references = Vec::new();
    match kind {
        b'K' => references.extend([1, 2]),
        b'W' => references.push(3),
        b'F' | b'H' => {
            if let Some(span) = tokens.get(3) {
                let control = &text[span.clone()];
                let upper = control.to_ascii_uppercase();
                if let Some(count) = upper
                    .strip_prefix("POLY(")
                    .and_then(|value| value.strip_suffix(')'))
                {
                    let count: usize = count.parse().context("invalid POLY dimension")?;
                    ensure!(
                        count <= tokens.len().saturating_sub(4),
                        "incomplete POLY controls"
                    );
                    references.extend(4..4 + count);
                } else if !upper.starts_with("VALUE") && !upper.starts_with("POLY") {
                    references.push(3);
                }
            }
        }
        _ => {}
    }
    let mut output = text.to_owned();
    for index in references.into_iter().rev() {
        if let Some(span) = tokens.get(index) {
            let target = scope.resolve(&text[span.clone()])?;
            output.replace_range(span.clone(), target);
        }
    }
    Ok(output)
}

fn expression_references(text: &str, scope: &Scope) -> Result<String> {
    let bytes = text.as_bytes();
    let mut output = String::with_capacity(text.len());
    let mut offset = 0;
    while offset < text.len() {
        let current = text[offset..]
            .chars()
            .next()
            .context("identity expression boundary")?;
        let boundary = offset == 0 || !super::symbol_byte(bytes[offset - 1]);
        if current.eq_ignore_ascii_case(&'i') && boundary {
            let mut open = offset + 1;
            while bytes.get(open).is_some_and(u8::is_ascii_whitespace) {
                open += 1;
            }
            if bytes.get(open) == Some(&b'(') {
                let close = super::group_end(text, open)?;
                let reference = text[open + 1..close].trim();
                let target = scope.resolve(reference)?;
                output.push_str(&text[offset..=open]);
                output.push_str(target);
                output.push(')');
                offset = close + 1;
                continue;
            }
        }
        output.push(current);
        offset += current.len_utf8();
    }
    Ok(output)
}

/// Resolve names in the combined source lexical scopes. Included source
/// statements retain their original language until this pass.
///
/// # Errors
/// Duplicate/ambiguous device identities or malformed circuit scopes.
pub fn resolve_device_identities(statements: &[ScopedStatement]) -> Result<ResolvedDeviceSource> {
    let scan = collect(statements)?;
    let mut output = String::new();
    for (line, (scope, identity)) in statements.iter().zip(scan.assignments) {
        let scope = &scan.scopes[scope];
        if let Some(identity) = identity {
            let identity = &scan.identities[identity];
            let text = line.text.trim_start();
            let tail = &text[identity.declared.len()..];
            let rewritten =
                control_references(&format!("{}{tail}", identity.linked), identity, scope)?;
            writeln!(output, "{}", expression_references(&rewritten, scope)?)?;
        } else if first_token(&line.text).is_some_and(|first| {
            first.eq_ignore_ascii_case(".param") || first.eq_ignore_ascii_case(".func")
        }) {
            writeln!(output, "{}", expression_references(&line.text, scope)?)?;
        } else {
            writeln!(output, "{}", line.text)?;
        }
    }
    Ok(ResolvedDeviceSource {
        text: output,
        identities: scan.identities,
    })
}
