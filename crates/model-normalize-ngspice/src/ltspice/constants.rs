//! Closed scalar expressions use the same lowering and helper definitions as
//! emitted parameter expressions. Never guess a value for a free symbol.

use super::{HELPERS, table};
use crate::{
    lexical,
    param_expression::{SpiceExpression, SpiceExpressionContext, lower_ltspice},
};
use anyhow::{Context, Result, bail, ensure};
use std::collections::BTreeMap;

const PREFIX: &str = "__lt_constant_";

fn lower(raw: &str, runtime_literal: bool) -> Result<SpiceExpression> {
    let lowered = lower_ltspice(raw, runtime_literal)?;
    let bytes = lowered.as_bytes();
    let mut edits = Vec::new();
    for span in lexical::function_calls(bytes, 0) {
        let name = &bytes[span.clone()];
        ensure!(
            !name.starts_with(PREFIX.as_bytes()),
            "reserved constant-evaluation helper name"
        );
        if name == b"table" || HELPERS.iter().any(|(helper, _)| name == *helper) {
            let mut alias = PREFIX.as_bytes().to_vec();
            alias.extend_from_slice(name);
            edits.push((span, alias));
        }
    }
    let output = lexical::apply_edits(bytes, edits);
    let output = std::str::from_utf8(output.as_deref().unwrap_or(bytes))?;
    // Parameter groups remain groups, but have no separate runtime phase
    // in a closed static expression.
    SpiceExpression::parse(&output.replace('{', "(").replace('}', ")"))
}

struct Constants {
    frames: Vec<BTreeMap<String, SpiceExpression>>,
    remaining: usize,
}

impl Constants {
    fn charge(&mut self) -> Result<()> {
        self.remaining = self
            .remaining
            .checked_sub(1)
            .context("LTspice constant expansion budget exceeded")?;
        Ok(())
    }
}

impl SpiceExpressionContext for Constants {
    fn symbol(&mut self, name: &str) -> Result<f64> {
        self.charge()?;
        if let Some(frame) = self.frames.pop() {
            let result = match frame.get(name) {
                Some(argument) => argument.evaluate_with(self),
                None => self.symbol(name),
            };
            self.frames.push(frame);
            return result;
        }
        bail!("waveform parameter {name} needs occurrence-specific binding")
    }

    fn function(&mut self, name: &str, args: &[SpiceExpression]) -> Result<f64> {
        self.charge()?;
        let name = name
            .strip_prefix(PREFIX)
            .context("unsupported closed LTspice function")?;
        let definition = if name == "table" {
            ensure!(
                args.len() >= 3 && args.len() % 2 == 1,
                "invalid table arity"
            );
            table::definition(args.len() / 2, true)?
        } else {
            HELPERS
                .iter()
                .find(|(key, _)| *key == name.as_bytes())
                .context("unknown LTspice constant helper")?
                .1
                .to_owned()
        };
        let (signature, body) = definition
            .split_once(')')
            .context("invalid helper definition")?;
        let signature = signature
            .strip_prefix('(')
            .context("invalid helper signature")?;
        let parameters = signature.split(',').collect::<Vec<_>>();
        ensure!(
            parameters.len() == args.len(),
            "invalid LTspice {name} arity"
        );
        let body = SpiceExpression::parse(body)?;
        self.frames.push(
            parameters
                .into_iter()
                .map(str::to_owned)
                .zip(args.iter().cloned())
                .collect(),
        );
        let result = body.evaluate_with(self);
        self.frames.pop();
        result
    }
}

pub(super) fn evaluate(raw: &str, runtime_literal: bool) -> Result<f64> {
    lower(raw, runtime_literal)?.evaluate_with(&mut Constants {
        frames: Vec::new(),
        remaining: 100_000,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_timing_uses_source_precedence_and_helpers() -> Result<()> {
        for (raw, expected) in [
            ("{2+1}", 3.),
            ("{2**3**2}", 512.),
            ("{-2**2}", 4.),
            ("{3%2}", 1.),
            ("{if(.4,1/0,3)}", 3.),
            ("{table(0.5,0,2,1,4)}", 3.),
            ("{sqrt(-2)}", 0.),
            ("{pow(-2,.5)}", 0.),
            ("{1|1&0}", 0.),
        ] {
            let value = evaluate(raw, false)?;
            ensure!(
                (value - expected).abs() < 1e-12,
                "{raw}: {value} != {expected}"
            );
        }
        ensure!(
            evaluate("{width*2}", false).is_err(),
            "free parameter was guessed"
        );
        Ok(())
    }
}
