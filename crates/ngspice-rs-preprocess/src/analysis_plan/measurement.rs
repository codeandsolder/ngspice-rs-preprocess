//! Source post-processing declarations. Expressions and earlier result
//! references bind to waveform data, not the circuit scalar evaluator.

use super::AnalysisKind;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

/// Source post-processing operation, distinct from circuit parameter evaluation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeasurementOperation {
    /// Value at an axis position or threshold event.
    Find,
    /// Local waveform derivative.
    Derivative,
    /// Coordinate of a threshold event.
    When,
    /// Explicit axis position without an ordinate expression.
    At,
    /// Scalar expression, potentially depending on preceding measurements.
    Parameter,
    /// Interval average.
    Average,
    /// Interval maximum.
    Maximum,
    /// Interval minimum.
    Minimum,
    /// Peak-to-peak.
    PeakToPeak,
    /// Root mean square.
    RootMeanSquare,
    /// Interval integral.
    Integral,
    /// Distance between trigger and target events.
    TriggerTarget,
}

impl MeasurementOperation {
    fn parse(name: &str) -> Option<Self> {
        Some(match name.to_ascii_uppercase().as_str() {
            "FIND" => Self::Find,
            "DERIV" => Self::Derivative,
            "WHEN" => Self::When,
            "AT" => Self::At,
            "PARAM" => Self::Parameter,
            "AVG" => Self::Average,
            "MAX" => Self::Maximum,
            "MIN" => Self::Minimum,
            "PP" => Self::PeakToPeak,
            "RMS" => Self::RootMeanSquare,
            "INTEG" => Self::Integral,
            "TRIG" | "TARG" => Self::TriggerTarget,
            _ => return None,
        })
    }
}

/// A source measurement prior to waveform and result-name binding.
/// Keep the remaining source grammar verbatim: complex AC expressions,
/// event predicates and previous measurement names are not numparam.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MeasurementIntent {
    /// Explicit selector or current analysis when omitted.
    pub analysis: Option<AnalysisKind>,
    /// Original candidate-local result identity.
    pub name: String,
    /// Requested operation.
    pub operation: MeasurementOperation,
    /// Operation operands and qualifiers in the original source syntax.
    pub expression: String,
}

impl MeasurementIntent {
    pub(super) fn parse_ltspice(statement: &str) -> Result<Self> {
        let spans = model_normalize_ngspice::statement_token_spans(statement)?;
        let (_, operands) = spans
            .split_first()
            .context("missing measurement declaration")?;
        let first = operands
            .first()
            .context("missing measurement result name")?;
        let analysis = match statement[first.clone()].to_ascii_uppercase().as_str() {
            "OP" => Some(AnalysisKind::OperatingPoint),
            "DC" => Some(AnalysisKind::DcSweep),
            "AC" => Some(AnalysisKind::Ac),
            "TRAN" => Some(AnalysisKind::Transient),
            "NOISE" => Some(AnalysisKind::Noise),
            "TF" => Some(AnalysisKind::TransferFunction),
            _ => None,
        };
        let fields = if analysis.is_some() {
            &operands[1..]
        } else {
            operands
        };
        let (name, rest) = fields
            .split_first()
            .context("missing measurement result name")?;
        let name = &statement[name.clone()];
        ensure!(
            !name.is_empty()
                && !name
                    .chars()
                    .any(|c| c.is_whitespace() || matches!(c, '(' | ')' | '{' | '}' | '=' | ',')),
            "invalid measurement result name {name:?}"
        );
        let (kind, expression) = if let Some(operation) = rest.first() {
            let operation_name = &statement[operation.clone()];
            let kind = MeasurementOperation::parse(operation_name).with_context(|| {
                format!("unqualified LTspice measurement operation {operation_name}")
            })?;
            let operands = statement[operation.end..].trim();
            ensure!(!operands.is_empty(), "missing measurement expression");
            let expression = if matches!(
                kind,
                MeasurementOperation::TriggerTarget | MeasurementOperation::At
            ) {
                // In a range with no reduction, TRIG and TARG are optional
                // endpoints. Keep which endpoint was actually written.
                statement[operation.start..].trim()
            } else {
                operands
            };
            (kind, expression)
        } else {
            // With neither endpoint supplied, the result is the full
            // abscissa interval. A measurement is still a valid declaration.
            (MeasurementOperation::TriggerTarget, "")
        };
        Ok(Self {
            analysis,
            name: name.to_owned(),
            operation: kind,
            expression: expression.to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measurement_source_expressions_and_result_dependencies_are_preserved() -> Result<()> {
        let result = MeasurementIntent::parse_ltspice(
            ".meas AC fc WHEN mag(V(out)) = tmp / sqrt(2) FALL=LAST",
        )?;
        assert_eq!(result.analysis, Some(AnalysisKind::Ac));
        assert_eq!(result.operation, MeasurementOperation::When);
        assert_eq!(result.expression, "mag(V(out)) = tmp / sqrt(2) FALL=LAST");
        let delta = MeasurementIntent::parse_ltspice(
            ".measure delay TRIG V(a)=0.5 RISE=1 TARG V(b)=0.5 RISE=2",
        )?;
        assert_eq!(delta.analysis, None);
        assert_eq!(delta.operation, MeasurementOperation::TriggerTarget);
        assert_eq!(
            delta.expression,
            "TRIG V(a)=0.5 RISE=1 TARG V(b)=0.5 RISE=2"
        );
        assert_eq!(delta.name, "delay");
        let scalar = MeasurementIntent::parse_ltspice(".meas tran scaled PARAM delay*2")?;
        assert_eq!(scalar.operation, MeasurementOperation::Parameter);
        assert_eq!(scalar.expression, "delay*2");
        assert!(MeasurementIntent::parse_ltspice(".meas tran").is_err());
        assert!(MeasurementIntent::parse_ltspice(".meas tran p AVG").is_err());
        let target = MeasurementIntent::parse_ltspice(".meas tran arrival TARG V(out)=0.5")?;
        assert_eq!(target.operation, MeasurementOperation::TriggerTarget);
        assert_eq!(target.expression, "TARG V(out)=0.5");
        let duration = MeasurementIntent::parse_ltspice(".meas tran duration")?;
        assert_eq!(duration.operation, MeasurementOperation::TriggerTarget);
        assert!(duration.expression.is_empty());
        let position = MeasurementIntent::parse_ltspice(".meas tran position AT=1m")?;
        assert_eq!(position.operation, MeasurementOperation::At);
        assert_eq!(position.expression, "AT=1m");
        Ok(())
    }
}
