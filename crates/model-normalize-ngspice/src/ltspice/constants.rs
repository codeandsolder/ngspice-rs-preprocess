//! Closed scalar expressions use the same lowering and helper definitions as
//! emitted parameter expressions. Never guess a value for a free symbol.

use super::{HELPERS, table};
use crate::{
    lexical,
    param_expression::{SpiceExpression, SpiceExpressionContext, lower_ltspice},
};
use anyhow::{Context, Result, bail, ensure};
use model_preprocess_command::{Resource, WorkBudget};
use std::collections::BTreeMap;

const PREFIX: &str = "__lt_constant_";

fn lower(raw: &str, runtime_literal: bool, budget: &WorkBudget) -> Result<SpiceExpression> {
    let lowered = lower_ltspice(raw, runtime_literal, budget)?;
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
    let output = lexical::apply_budgeted_edits(bytes, edits, budget)?;
    let output = std::str::from_utf8(output.as_deref().unwrap_or(bytes))?;
    // Parameter groups remain groups, but have no separate runtime phase
    // in a closed static expression.
    SpiceExpression::parse_with_budget(&output.replace('{', "(").replace('}', ")"), budget)
}

struct Constants {
    frames: Vec<BTreeMap<String, SpiceExpression>>,
    budget: WorkBudget,
    unbound: bool,
}

impl Constants {
    fn charge(&mut self) -> Result<()> {
        self.budget.charge(Resource::EvaluationSteps, 1)?;
        Ok(())
    }
}

impl SpiceExpressionContext for Constants {
    fn symbol(&mut self, name: &str) -> Result<f64> {
        self.charge()?;
        if let Some(frame) = self.frames.pop() {
            let result = match frame.get(name) {
                Some(argument) => argument.evaluate_with_budget(self, &self.budget.clone()),
                None => self.symbol(name),
            };
            self.frames.push(frame);
            return result;
        }
        self.unbound = true;
        bail!("waveform parameter {name} needs occurrence-specific binding")
    }

    fn function(&mut self, name: &str, args: &[SpiceExpression]) -> Result<f64> {
        self.charge()?;
        let Some(name) = name.strip_prefix(PREFIX) else {
            self.unbound = true;
            bail!("source-defined LTspice function needs occurrence-specific binding");
        };
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
        let body = SpiceExpression::parse_with_budget(body, &self.budget)?;
        self.frames.push(
            parameters
                .into_iter()
                .map(str::to_owned)
                .zip(args.iter().cloned())
                .collect(),
        );
        let result = body.evaluate_with_budget(self, &self.budget.clone());
        self.frames.pop();
        result
    }
}

pub(super) fn try_evaluate(
    raw: &str,
    runtime_literal: bool,
    budget: &WorkBudget,
) -> Result<Option<f64>> {
    let expression = lower(raw, runtime_literal, budget)?;
    let mut context = Constants {
        frames: Vec::new(),
        budget: budget.clone(),
        unbound: false,
    };
    match expression.evaluate_with_budget(&mut context, budget) {
        Ok(value) => Ok(Some(value)),
        Err(_) if context.unbound => Ok(None),
        Err(error) => Err(error),
    }
}

pub(super) fn evaluate(raw: &str, runtime_literal: bool, budget: &WorkBudget) -> Result<f64> {
    try_evaluate(raw, runtime_literal, budget)?
        .context("LTspice expression requires occurrence-specific binding")
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
            let value = evaluate(raw, false, &WorkBudget::default())?;
            ensure!(
                (value - expected).abs() < 1e-12,
                "{raw}: {value} != {expected}"
            );
        }
        ensure!(
            evaluate("{width*2}", false, &WorkBudget::default()).is_err(),
            "free parameter was guessed"
        );
        Ok(())
    }
}
