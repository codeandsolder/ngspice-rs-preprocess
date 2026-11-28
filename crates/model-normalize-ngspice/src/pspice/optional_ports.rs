//! Optional source subcircuit ports, independent of numerical parameters.
//!
//! The `PSpice` A/D Reference Guide 10.5, pp. 84–86, specifies that
//! callers may supply a prefix of optional nodes; remaining ports use
//! their declaration defaults. Defaults are not scalar parameter values.

use super::nodes::{NodeKind, NodeReference};
use crate::{
    lexical,
    model_card::{parse_assignments, positional_prefix},
};
use anyhow::{Context, Result, ensure};
use model_preprocess_command::{Resource, WorkBudget};
use serde::Serialize;
use std::collections::BTreeSet;

/// One optional formal port and its node-valued default.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct OptionalPort {
    /// Formal name in the subcircuit.
    pub name: String,
    /// Source default, not an already elaborated node.
    pub default: NodeReference,
}

/// The electrical signature of a source subcircuit with optional ports.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct OptionalPorts {
    /// Declared subcircuit name.
    pub name: String,
    /// Mandatory formal ports in declaration order.
    pub required: Vec<String>,
    /// Optional formal ports in declaration order.
    pub optional: Vec<OptionalPort>,
}

/// A binding retains whether a node was supplied by the actual caller
/// or taken from the subcircuit's own default declaration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PortArgument<'a> {
    /// Resolve in the actual caller's node context.
    Actual(&'a NodeReference),
    /// Resolve according to source default-node semantics, not as a caller
    /// override or a numeric parameter expression.
    Default(&'a NodeReference),
}

/// One formal-to-node binding, before hierarchy elaboration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PortBinding<'a> {
    /// Formal port in definition order.
    pub formal: &'a str,
    /// Supplied or default node reference.
    pub argument: PortArgument<'a>,
}

fn formal(name: &str, seen: &mut BTreeSet<String>) -> Result<String> {
    let node = NodeReference::parse(name)?;
    ensure!(
        node.kind == NodeKind::Named,
        "PSpice subcircuit port must be a named formal, not a reserved node {name}"
    );
    ensure!(
        seen.insert(name.to_ascii_lowercase()),
        "duplicate PSpice subcircuit port {name}"
    );
    Ok(node.name)
}

impl OptionalPorts {
    /// Parse only the optional electrical signature of a logical header.
    /// A declaration without `OPTIONAL:` is owned by the ordinary hierarchy
    /// parser. `PARAMS:` and `TEXT:` remain in the original source statement.
    ///
    /// # Errors
    /// Malformed optional pin declaration, missing defaults or repeated formals.
    pub fn parse(text: &str, budget: &WorkBudget) -> Result<Option<Self>> {
        budget.charge(Resource::ScannedBytes, text.len())?;
        let spans = lexical::waveform_tokens(text.as_bytes())?;
        let Some(marker) = spans.iter().find(|span| {
            ["OPTIONAL:", "PARAMS:", "TEXT:"].iter().any(|marker| {
                text[span.start..]
                    .get(..marker.len())
                    .is_some_and(|word| word.eq_ignore_ascii_case(marker))
            })
        }) else {
            return Ok(None);
        };
        if !text[marker.start..]
            .get(..9)
            .is_some_and(|word| word.eq_ignore_ascii_case("OPTIONAL:"))
        {
            return Ok(None);
        }
        let start = marker.start;
        let (prefix, rest) = positional_prefix(&text[..start]);
        let [(command, _), (name, _), required @ ..] = prefix.as_slice() else {
            anyhow::bail!("missing PSpice subcircuit header");
        };
        ensure!(
            command.eq_ignore_ascii_case(".subckt") && rest.trim().is_empty(),
            "invalid PSpice OPTIONAL: header"
        );
        let end = spans
            .iter()
            .find(|span| {
                span.start > start
                    && ["PARAMS:", "TEXT:"].iter().any(|marker| {
                        text[span.start..]
                            .get(..marker.len())
                            .is_some_and(|word| word.eq_ignore_ascii_case(marker))
                    })
            })
            .map_or(text.len(), |span| span.start);
        let assignments = parse_assignments(&text[start + 9..end])?;
        ensure!(
            !assignments.is_empty(),
            "empty PSpice OPTIONAL: declaration"
        );
        budget.charge(
            Resource::GeneratedNodes,
            required.len().saturating_add(assignments.len()),
        )?;
        budget.charge(Resource::EmittedBytes, text.len())?;
        let mut seen = BTreeSet::new();
        let required = required
            .iter()
            .map(|(name, _)| formal(name, &mut seen))
            .collect::<Result<Vec<_>>>()?;
        let optional = assignments
            .into_iter()
            .map(|field| {
                let name = formal(&field.name, &mut seen)?;
                let default = NodeReference::parse(
                    field
                        .expression
                        .as_deref()
                        .context("missing optional node default")?,
                )?;
                Ok(OptionalPort { name, default })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Some(Self {
            name: (*name).to_owned(),
            required,
            optional,
        }))
    }

    /// Bind a prefix of actual ports without losing the source of defaults.
    ///
    /// # Errors
    /// Too few required nodes or too many total actual nodes.
    pub fn bind<'a>(
        &'a self,
        actual: &'a [NodeReference],
    ) -> Result<impl ExactSizeIterator<Item = PortBinding<'a>>> {
        ensure!(
            (self.required.len()..=self.required.len().saturating_add(self.optional.len()))
                .contains(&actual.len()),
            "incorrect PSpice subcircuit port count for {}",
            self.name
        );
        Ok(
            (0..self.required.len() + self.optional.len()).map(move |index| {
                if index < self.required.len() {
                    PortBinding {
                        formal: &self.required[index],
                        argument: PortArgument::Actual(&actual[index]),
                    }
                } else {
                    let port = &self.optional[index - self.required.len()];
                    PortBinding {
                        formal: &port.name,
                        argument: actual
                            .get(index)
                            .map_or(PortArgument::Default(&port.default), PortArgument::Actual),
                    }
                }
            }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optional_ports_bind_supplied_prefix_and_preserve_defaults() -> Result<()> {
        let budget = WorkBudget::default();
        let signature = OptionalPorts::parse(
            ".SUBCKT cell A B Y OPTIONAL:DPWR=$G_DPWR, DGND=$G_DGND OTHER=internal PARAMS: delay=1n TEXT: file=\"a b\"",
            &budget,
        )?.context("missing optional electrical signature")?;
        assert_eq!(signature.required, ["A", "B", "Y"]);
        assert_eq!(signature.optional.len(), 3);
        let nodes = ["input", "$D_HI", "output", "my_supply"]
            .map(NodeReference::parse)
            .into_iter()
            .collect::<Result<Vec<_>>>()?;
        let bound = signature.bind(&nodes)?.collect::<Vec<_>>();
        assert_eq!(bound[3].argument, PortArgument::Actual(&nodes[3]));
        assert_eq!(
            bound[4].argument,
            PortArgument::Default(&signature.optional[1].default)
        );
        assert_eq!(signature.optional[1].default.kind, NodeKind::Global);
        assert_eq!(
            bound[5].argument,
            PortArgument::Default(&signature.optional[2].default)
        );
        assert_eq!(signature.optional[2].default.kind, NodeKind::Named);
        assert!(signature.bind(&nodes[..2]).is_err());
        assert!(OptionalPorts::parse(".subckt ordinary a b params: r=1", &budget)?.is_none());
        for text in [
            ".subckt cell a OPTIONAL: A=$G_DPWR",
            ".subckt cell a OPTIONAL: b",
            ".subckt cell 0 OPTIONAL: b=$G_DGND",
            ".subckt cell a OPTIONAL: b={numeric_expression}",
            ".subckt cell a OPTIONAL: PARAMS: p=1",
        ] {
            assert!(OptionalPorts::parse(text, &budget).is_err(), "{text}");
        }
        Ok(())
    }
}
