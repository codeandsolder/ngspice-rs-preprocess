//! `PSpice` keeps GND local inside subcircuits; native ngspice reserves it.
//! Only electrical operands are changed, not parameter or model identifiers.

use crate::{lexical, model_card::positional_prefix};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::ops::Range;

/// A named node is not necessarily local: explicit global declarations and
/// enclosing formal ports are resolved at occurrence binding.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    /// Ordinary named node, subject to normal hierarchy and port binding.
    Named,
    /// Implicit global reference (zero or a dollar-G-underscore name).
    Global,
    /// Fixed logic zero.
    LogicLow,
    /// Fixed logic one.
    LogicHigh,
    /// Fixed unknown logic value.
    LogicUnknown,
    /// A disconnected terminal; each occurrence is independent, not a net.
    NotConnected,
}

/// Source node reference, with original spelling and explicit special semantics.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NodeReference {
    /// Original source node spelling.
    pub name: String,
    /// Source naming semantics.
    pub kind: NodeKind,
}

impl NodeReference {
    /// Classify a source node without assigning a numerical voltage or row.
    ///
    /// # Errors
    /// Malformed node or an unqualified reserved digital constant.
    pub fn parse(name: &str) -> Result<Self> {
        ensure!(
            !name.is_empty()
                && !name.bytes().any(|byte| byte.is_ascii_whitespace())
                && !name.contains(['=', '(', ')', '{', '}', ',', '"', '\'']),
            "invalid PSpice node {name}"
        );
        let upper = name.to_ascii_uppercase();
        let kind = match upper.as_str() {
            "$D_HI" => NodeKind::LogicHigh,
            "$D_LO" => NodeKind::LogicLow,
            "$D_X" => NodeKind::LogicUnknown,
            "$D_NC" => NodeKind::NotConnected,
            "0" => NodeKind::Global,
            _ if upper.starts_with("$G_") => NodeKind::Global,
            _ if upper.starts_with("$D_") => {
                anyhow::bail!("unsupported reserved PSpice digital node {name}")
            }
            _ => NodeKind::Named,
        };
        Ok(Self {
            name: name.to_owned(),
            kind,
        })
    }
}

fn replace_nodes(
    text: &str,
    generated: &str,
    positions: impl Iterator<Item = Range<usize>>,
    local: bool,
    hierarchical: impl Fn(&str) -> bool,
    edits: &mut Vec<lexical::Edit>,
) {
    for span in positions {
        let node = &text[span.clone()];
        let replacement = if local && node.eq_ignore_ascii_case("gnd") {
            Some(generated.to_owned())
        } else if hierarchical(node) {
            node.rsplit_once('.')
                .map(|(path, _)| format!("{path}.{generated}"))
        } else {
            None
        };
        if let Some(name) = replacement {
            edits.push((span, name.into_bytes()));
        }
    }
}

fn positional_nodes(text: &str) -> Result<Vec<Range<usize>>> {
    let (fields, _) = positional_prefix(text);
    let Some((first, _)) = fields.first() else {
        return Ok(Vec::new());
    };
    let lower = first.to_ascii_lowercase();
    let (skip, count) = match lower.as_str() {
        ".subckt" => (2, fields.len().saturating_sub(2)),
        _ if lower.starts_with('.') => return Ok(Vec::new()),
        _ => {
            let count = match lower.as_bytes()[0] {
                b'x' => fields.len().saturating_sub(2),
                b'r' | b'c' | b'l' | b'd' | b'v' | b'i' | b'b' | b'f' | b'h' | b'w' => 2,
                b'j' | b'z' | b'q' => 3,
                b'm' | b's' | b't' | b'o' => 4,
                b'e' | b'g' => {
                    let control = fields.get(3).map_or("", |(token, _)| *token);
                    if ["value", "table", "freq", "laplace", "vol", "cur"]
                        .iter()
                        .any(|word| control.eq_ignore_ascii_case(word))
                    {
                        2
                    } else if control.eq_ignore_ascii_case("poly") {
                        // POLY(n) has 2n additional node operands after
                        // the dimension; its outputs are handled separately.
                        let dimensions = fields
                            .get(4)
                            .context("missing PSpice POLY dimension")?
                            .0
                            .parse::<usize>()
                            .context("invalid PSpice POLY dimension")?;
                        let controls = dimensions
                            .checked_mul(2)
                            .context("PSpice POLY node count overflow")?;
                        ensure!(
                            fields.len() >= 5usize.saturating_add(controls),
                            "truncated PSpice POLY controls"
                        );
                        return Ok(fields
                            .iter()
                            .skip(1)
                            .take(2)
                            .chain(fields.iter().skip(5).take(controls))
                            .map(|(word, end)| end - word.len()..*end)
                            .collect());
                    } else {
                        4
                    }
                }
                b'k' => 0,
                _ => {
                    ensure!(
                        !fields
                            .iter()
                            .skip(1)
                            .any(|(word, _)| word.eq_ignore_ascii_case("gnd")),
                        "PSpice GND node on unsupported device {first}"
                    );
                    return Ok(Vec::new());
                }
            };
            (1, count)
        }
    };
    Ok(fields
        .iter()
        .skip(skip)
        .take(count)
        .map(|(word, end)| end - word.len()..*end)
        .collect())
}

fn probe_nodes(text: &str) -> Vec<Range<usize>> {
    let bytes = text.as_bytes();
    let mut result = Vec::new();
    for name in lexical::function_names(bytes, b"v", 0) {
        let rest = &text[name.end..];
        let Some(open) = rest.find('(').map(|offset| name.end + offset + 1) else {
            continue;
        };
        let Some(close) = text[open..].find(')').map(|offset| open + offset) else {
            continue;
        };
        let mut offset = open;
        for operand in text[open..close].split(',') {
            let trimmed = operand.trim();
            let start = offset + operand.len() - operand.trim_start().len();
            if !trimmed.is_empty() {
                result.push(start..start + trimmed.len());
            }
            offset += operand.len() + 1;
        }
    }
    result
}

pub(super) fn rewrite(
    text: &str,
    local: bool,
    generated: &str,
    control: bool,
    hierarchical: impl Fn(&str) -> bool,
) -> Result<String> {
    if !text
        .as_bytes()
        .windows(3)
        .any(|w| w.eq_ignore_ascii_case(b"gnd"))
    {
        return Ok(text.to_owned());
    }
    let mut edits = Vec::new();
    if local && !control {
        replace_nodes(
            text,
            generated,
            positional_nodes(text)?.into_iter(),
            local,
            |_| false,
            &mut edits,
        );
    }
    replace_nodes(
        text,
        generated,
        probe_nodes(text).into_iter(),
        local,
        hierarchical,
        &mut edits,
    );
    // Positional syntax and probe syntax are disjoint for supported cards.
    edits.sort_by_key(|(span, _)| span.start);
    ensure!(
        edits
            .windows(2)
            .all(|pair| pair[0].0.end <= pair[1].0.start),
        "overlapping PSpice electrical operand spans"
    );
    Ok(String::from_utf8(
        lexical::apply_edits(text.as_bytes(), edits).unwrap_or_else(|| text.as_bytes().to_vec()),
    )?)
}
