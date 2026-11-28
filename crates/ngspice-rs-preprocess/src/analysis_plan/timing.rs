//! Candidate-bound segmented transient timing. Output sampling and integration
//! ceilings are deliberately separate from source-language parsing.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

/// An absolute stop and output interval expressed in the candidate's scalar scope.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SymbolicTimingSegment {
    /// Output interval.
    pub step: String,
    /// Absolute segment end.
    pub until: String,
}

/// A timing declaration prior to candidate binding.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TimingLayout {
    /// Explicit segment pairs and optional output gate.
    Segments {
        /// Ordered output segments.
        output: Vec<SymbolicTimingSegment>,
        /// First saved output time; omitted means zero.
        start: Option<String>,
    },
    /// Four HSPICE positional operands with no named START. The source rule
    /// distinguishes a second segment from legacy START/DELMAX at binding.
    PairOrLegacy {
        /// First interval/stop pair.
        first: SymbolicTimingSegment,
        /// Either second interval/stop or legacy START/DELMAX.
        second: SymbolicTimingSegment,
    },
}

/// Deck-wide explicit integration ceiling and its declaration provenance.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SymbolicCeiling {
    /// Ceiling expression.
    pub value: String,
    /// Original option location.
    pub origin: super::DirectiveOrigin,
}

/// Symbolic timing and bias-selection policy.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ScheduleIntent {
    /// Output layout.
    pub layout: TimingLayout,
    /// Deck-wide explicit DELMAX, if supplied. Legacy positional DELMAX
    /// takes precedence. Omission retains the automatic source policy.
    pub delmax: Option<SymbolicCeiling>,
    /// Skip the transient operating point.
    pub uic: bool,
}

/// Numeric absolute time boundary and associated interval in seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BoundTimingSegment {
    /// Absolute end.
    pub until: f64,
    /// Output interval or step ceiling.
    pub step: f64,
}

/// Integration ceiling after resolving explicit values.
#[derive(Clone, Debug, PartialEq)]
pub enum IntegrationCeiling {
    /// Source-selected automatic policy, not an arbitrary native default.
    Automatic,
    /// Independent, explicit integration ceiling segments.
    Explicit(Vec<BoundTimingSegment>),
}

/// Timing ready for execution-policy selection.
#[derive(Clone, Debug, PartialEq)]
pub struct BoundSchedule {
    /// First saved output time.
    pub start: f64,
    /// Ordered output schedule.
    pub output: Vec<BoundTimingSegment>,
    /// Independent integration ceiling.
    pub ceiling: IntegrationCeiling,
    /// Skip the bias solve.
    pub uic: bool,
}

impl ScheduleIntent {
    /// Validate every scalar and resolve the schedule if all fields are
    /// closed. No placeholder value is substituted for candidate parameters
    /// or user functions.
    ///
    /// # Errors
    /// Malformed scalar syntax, invalid closed timing, or work exhaustion.
    pub fn try_bind_closed(
        &self,
        budget: &model_preprocess_command::WorkBudget,
    ) -> Result<Option<BoundSchedule>> {
        use model_normalize_ngspice::param_expression::SpiceExpression;
        let mut values = std::collections::BTreeMap::new();
        let mut record = |raw: &str| -> Result<()> {
            let expression = SpiceExpression::parse_with_budget(raw, budget)?;
            let value = expression.known_value(budget)?;
            budget.charge(model_preprocess_command::Resource::GeneratedNodes, 1)?;
            budget.charge(model_preprocess_command::Resource::EmittedBytes, raw.len())?;
            values.insert(raw.to_owned(), value);
            Ok(())
        };
        let mut segment = |value: &SymbolicTimingSegment| -> Result<()> {
            record(&value.step)?;
            record(&value.until)
        };
        match &self.layout {
            TimingLayout::Segments { output, start } => {
                for value in output {
                    segment(value)?;
                }
                if let Some(start) = start {
                    record(start)?;
                }
            }
            TimingLayout::PairOrLegacy { first, second } => {
                segment(first)?;
                segment(second)?;
            }
        }
        if let Some(cap) = &self.delmax {
            record(&cap.value)?;
        }
        if values.values().any(Option::is_none) {
            return Ok(None);
        }
        self.bind(|raw| {
            values
                .get(raw)
                .copied()
                .flatten()
                .ok_or_else(|| anyhow::anyhow!("missing validated timing field {raw}"))
        })
        .map(Some)
    }

    /// Resolve symbolic timing in the actual candidate scope.
    ///
    /// # Errors
    /// Unbound values, invalid gates, intervals or boundaries, or nonfinite
    /// and nonpositive explicit integration ceilings.
    pub fn bind(&self, mut resolve: impl FnMut(&str) -> Result<f64>) -> Result<BoundSchedule> {
        let mut segment = |value: &SymbolicTimingSegment| -> Result<BoundTimingSegment> {
            Ok(BoundTimingSegment {
                step: resolve(&value.step)?,
                until: resolve(&value.until)?,
            })
        };
        let (output, start, legacy_cap) = match &self.layout {
            TimingLayout::Segments { output, start } => (
                output
                    .iter()
                    .map(&mut segment)
                    .collect::<Result<Vec<_>>>()?,
                start.as_deref().map_or(Ok(0.), &mut resolve)?,
                None,
            ),
            TimingLayout::PairOrLegacy { first, second } => {
                let first = segment(first)?;
                let second = segment(second)?;
                if second.step < first.until && second.until < first.until {
                    (vec![first], second.step, Some(second.until))
                } else {
                    (vec![first, second], 0., None)
                }
            }
        };
        ensure!(start.is_finite() && start >= 0., "invalid transient START");
        let mut previous = start;
        for segment in &output {
            ensure!(
                segment.until.is_finite()
                    && segment.step.is_finite()
                    && segment.step > 0.
                    && segment.until > previous
                    && segment.step <= segment.until - previous
                    && previous + segment.step > previous,
                "invalid or non-progressing transient timing segment"
            );
            previous = segment.until;
        }
        ensure!(!output.is_empty(), "empty transient output schedule");
        let cap = match legacy_cap {
            Some(cap) => Some(cap),
            None => self
                .delmax
                .as_ref()
                .map(|cap| resolve(&cap.value))
                .transpose()?,
        };
        let ceiling = if let Some(step) = cap {
            ensure!(step.is_finite() && step > 0., "invalid transient DELMAX");
            IntegrationCeiling::Explicit(vec![BoundTimingSegment {
                until: previous,
                step,
            }])
        } else {
            IntegrationCeiling::Automatic
        };
        Ok(BoundSchedule {
            start,
            output,
            ceiling,
            uic: self.uic,
        })
    }
}

/// Parse already normalized HSPICE timing operands, excluding independent
/// sweep clauses. No expression is evaluated before candidate binding.
pub(crate) fn hspice(args: &[String]) -> Result<ScheduleIntent> {
    let mut operands = Vec::new();
    let mut start = None;
    let mut uic = false;
    let mut rest = args;
    while let Some((field, tail)) = rest.split_first() {
        rest = tail;
        if field.eq_ignore_ascii_case("UIC") {
            ensure!(!uic, "duplicate HSPICE UIC");
            uic = true;
        } else if field.eq_ignore_ascii_case("START") {
            ensure!(start.is_none(), "duplicate HSPICE START");
            let [eq, value, tail @ ..] = rest else {
                anyhow::bail!("missing HSPICE START assignment");
            };
            ensure!(eq == "=", "invalid HSPICE START assignment");
            start = Some(value.clone());
            rest = tail;
        } else {
            ensure!(
                start.is_none() && !uic,
                "unexpected HSPICE timing operand {field}"
            );
            operands.push(field.clone());
        }
    }
    let (pairs, remainder) = operands.as_chunks::<2>();
    ensure!(
        !pairs.is_empty() && remainder.is_empty(),
        "HSPICE transient requires interval/stop pairs"
    );
    let pair = |[step, until]: &[String; 2]| SymbolicTimingSegment {
        step: step.clone(),
        until: until.clone(),
    };
    let layout = if let [first, second] = pairs
        && start.is_none()
    {
        TimingLayout::PairOrLegacy {
            first: pair(first),
            second: pair(second),
        }
    } else {
        TimingLayout::Segments {
            output: pairs.iter().map(pair).collect(),
            start,
        }
    };
    Ok(ScheduleIntent {
        layout,
        delmax: None,
        uic,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use model_normalize_ngspice::model_card::parse_spice_constant;

    fn parse(text: &str) -> Result<ScheduleIntent> {
        hspice(&super::super::arguments(text)?)
    }

    #[test]
    fn legacy_and_second_segment_are_selected_after_binding() -> Result<()> {
        let mut intent = parse(".tran 1n 10n {third} {fourth} UIC")?;
        intent.delmax = Some(SymbolicCeiling {
            value: "0.25n".into(),
            origin: super::super::DirectiveOrigin::Generated {
                artifact: "test.cir".into(),
                first_line: 2,
                last_line: 2,
            },
        });
        for (fourth, expected_count) in [(0.5e-9, 1), (20e-9, 2)] {
            let result = intent.bind(|raw| match raw {
                "{third}" => Ok(2e-9),
                "{fourth}" => Ok(fourth),
                _ => parse_spice_constant(raw),
            })?;
            assert_eq!(result.output.len(), expected_count);
            assert!(result.uic);
            let IntegrationCeiling::Explicit(cap) = result.ceiling else {
                anyhow::bail!("missing explicit cap");
            };
            let expected_cap = if expected_count == 1 { fourth } else { 0.25e-9 };
            assert!((cap[0].step - expected_cap).abs() < 1e-22);
            assert_eq!(
                result.start.to_bits(),
                if expected_count == 1 { 2e-9_f64 } else { 0_f64 }.to_bits()
            );
        }
        Ok(())
    }

    #[test]
    fn named_start_and_automatic_ceiling_are_independent() -> Result<()> {
        let intent = parse(".tran 0.1n 25n 1n 40n START=10n")?;
        let result = intent.bind(parse_spice_constant)?;
        assert!((result.start - 10e-9).abs() < 1e-22);
        assert_eq!(result.output.len(), 2);
        assert_eq!(result.ceiling, IntegrationCeiling::Automatic);
        for invalid in [
            ".tran 1n 10n 2n",
            ".tran 1n 10n START=9.5n",
            ".tran 1n 10n 2n 8n START=0",
            ".tran 0 1n",
            ".tran 1n 10n UIC UIC",
        ] {
            assert!(
                parse(invalid)
                    .and_then(|intent| intent.bind(parse_spice_constant))
                    .is_err(),
                "{invalid}"
            );
        }
        Ok(())
    }
}
