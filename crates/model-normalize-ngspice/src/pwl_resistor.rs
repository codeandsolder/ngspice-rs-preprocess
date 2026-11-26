//! Prepared piecewise-linear resistor laws and advisory startup segments.

use anyhow::{Context, Result, ensure};
use model_preprocess_command::{Resource, WorkBudget};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Independent electrical coordinate of a resistor law.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Axis {
    Voltage,
    Current,
}

/// Signed voltage (positive minus negative terminal) and current (positive
/// into the positive terminal). Coordinates retain their physical meanings
/// regardless of the independent axis.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Knot {
    pub voltage: f64,
    pub current: f64,
}

impl Axis {
    const fn coordinates(self, knot: Knot) -> (f64, f64) {
        match self {
            Self::Voltage => (knot.voltage, knot.current),
            Self::Current => (knot.current, knot.voltage),
        }
    }
}

/// Electrical identities emitted for the ordinary native representation.
/// The current sensor of a current-defined law is also its observable branch.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum NativeBinding {
    CurrentLaw {
        source: String,
    },
    VoltageLaw {
        sensor: String,
        source: String,
        intermediate: String,
    },
}

/// Optional zero-voltage sensor inserted for another device's current probe.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SeriesSensor {
    pub source: String,
    pub intermediate: String,
}

/// A resistor declaration in the emitted lexical definition, before circuit
/// elaboration. Bind separately for every occurrence of that definition.
///
/// The startup segment is advisory and one-based. It selects an initial
/// linearized law, not a terminal constraint. The consumer must subsequently
/// select segments from the solved independent coordinate.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PreparedPwlResistor {
    pub artifact: String,
    pub scope: Vec<String>,
    /// Original linked source device token, including any leading exclamation.
    pub device: String,
    pub positive: String,
    pub negative: String,
    pub axis: Axis,
    pub knots: Vec<Knot>,
    pub initial_segment: Option<usize>,
    pub binding: NativeBinding,
    pub series_sensor: Option<SeriesSensor>,
}

/// Validate a finite curve with a strictly increasing independent coordinate
/// and positive differential resistance on both extrapolated end segments.
///
/// # Errors
/// Fewer than two knots, non-finite values, invalid order or end slopes.
pub fn validate_curve(axis: Axis, knots: impl IntoIterator<Item = Knot>) -> Result<()> {
    let mut knots = knots.into_iter();
    let first = knots
        .next()
        .context("piecewise-linear characteristic requires at least two points")?;
    let mut previous = axis.coordinates(first);
    ensure!(
        previous.0.is_finite() && previous.1.is_finite(),
        "piecewise-linear characteristic contains a non-finite coordinate"
    );
    let mut first_segment = true;
    let mut increasing = false;
    for knot in knots {
        let next = axis.coordinates(knot);
        ensure!(
            next.0.is_finite() && next.1.is_finite(),
            "piecewise-linear characteristic contains a non-finite coordinate"
        );
        ensure!(
            previous.0 < next.0,
            "piecewise-linear resistor independent coordinates must be strictly increasing"
        );
        increasing = previous.1 < next.1;
        if first_segment {
            ensure!(
                increasing,
                "piecewise-linear resistor end segments require positive differential resistance"
            );
            first_segment = false;
        }
        previous = next;
    }
    ensure!(
        !first_segment,
        "piecewise-linear characteristic requires at least two points"
    );
    ensure!(
        increasing,
        "piecewise-linear resistor end segments require positive differential resistance"
    );
    Ok(())
}

impl PreparedPwlResistor {
    /// Check the curve, one-based segment selector and native binding shape.
    ///
    /// # Errors
    /// Invalid numerical data, identity or axis/binding combination.
    pub fn validate(&self) -> Result<()> {
        validate_curve(self.axis, self.knots.iter().copied())?;
        ensure!(
            self.initial_segment
                .is_none_or(|segment| (1..self.knots.len()).contains(&segment)),
            "PWL resistor {} IC must select a defined segment",
            self.device
        );
        ensure!(
            matches!(
                (self.axis, &self.binding),
                (Axis::Voltage, NativeBinding::CurrentLaw { .. })
                    | (Axis::Current, NativeBinding::VoltageLaw { .. })
            ),
            "PWL resistor {} has an inconsistent native binding",
            self.device
        );
        ensure!(
            !self.artifact.is_empty()
                && !self.device.is_empty()
                && !self.positive.is_empty()
                && !self.negative.is_empty(),
            "empty PWL resistor identity"
        );
        Ok(())
    }

    fn expected_cards(&self) -> Vec<(&str, &str, &str, CardKind)> {
        let mut cards = Vec::new();
        let positive = if let Some(sensor) = &self.series_sensor {
            cards.push((
                sensor.source.as_str(),
                self.positive.as_str(),
                sensor.intermediate.as_str(),
                CardKind::Sensor,
            ));
            &sensor.intermediate
        } else {
            &self.positive
        };
        match &self.binding {
            NativeBinding::CurrentLaw { source } => {
                cards.push((source, positive, &self.negative, CardKind::Current));
            }
            NativeBinding::VoltageLaw {
                sensor,
                source,
                intermediate,
            } => {
                cards.push((sensor, positive, intermediate, CardKind::Sensor));
                cards.push((source, intermediate, &self.negative, CardKind::Voltage));
            }
        }
        cards
    }
}

#[derive(Clone, Copy)]
enum CardKind {
    Sensor,
    Current,
    Voltage,
}

impl CardKind {
    fn matches(self, name: &str, tail: &str) -> bool {
        match self {
            Self::Sensor => name.starts_with(['V', 'v']) && tail.trim() == "0",
            Self::Current => name.starts_with(['B', 'b']) && tail.trim_start().starts_with("I="),
            Self::Voltage => name.starts_with(['B', 'b']) && tail.trim_start().starts_with("V="),
        }
    }
}

type Identity = (Vec<String>, String);

fn canonical(scope: &[String], name: &str) -> Identity {
    (
        scope.iter().map(|part| part.to_ascii_lowercase()).collect(),
        name.to_ascii_lowercase(),
    )
}

/// Ensure each prepared law refers to unique, correctly oriented native
/// electrical cards in this artifact; consumers must not bind by global name.
///
/// # Errors
/// Invalid metadata, duplicate declarations, missing or mismatched anchors.
pub fn validate_artifact(
    declarations: &[PreparedPwlResistor],
    filename: &str,
    bytes: &[u8],
    budget: &WorkBudget,
) -> Result<()> {
    budget.charge(Resource::EvaluationSteps, declarations.len())?;
    let mut devices = BTreeSet::new();
    let mut expected = BTreeMap::new();
    for declaration in declarations
        .iter()
        .filter(|value| value.artifact == filename)
    {
        budget.charge(Resource::EvaluationSteps, declaration.knots.len())?;
        budget.charge(Resource::GeneratedNodes, 1)?;
        for part in &declaration.scope {
            budget.charge(Resource::ScannedBytes, part.len())?;
        }
        declaration.validate()?;
        ensure!(
            devices.insert(canonical(&declaration.scope, &declaration.device)),
            "duplicate PWL resistor declaration {}",
            declaration.device
        );
        for (name, positive, negative, kind) in declaration.expected_cards() {
            let key = canonical(&declaration.scope, name);
            ensure!(
                expected
                    .insert(key, (positive, negative, kind, false))
                    .is_none(),
                "duplicate PWL resistor native binding {name}"
            );
        }
    }
    if expected.is_empty() {
        return Ok(());
    }
    let mut scope = Vec::new();
    let mut control = false;
    budget.charge(Resource::ScannedBytes, bytes.len())?;
    for statement in crate::lexical::statements(bytes)? {
        budget.charge(Resource::Statements, 1)?;
        let text = std::str::from_utf8(&statement.text)?;
        let mut fields = text.split_ascii_whitespace();
        let Some(first) = fields.next() else { continue };
        if first.eq_ignore_ascii_case(".control") {
            control = true;
        } else if first.eq_ignore_ascii_case(".endc") {
            control = false;
        } else if control {
            continue;
        } else if first.eq_ignore_ascii_case(".subckt") {
            budget.check_depth(scope.len().saturating_add(1))?;
            scope.push(
                fields
                    .next()
                    .context("unnamed emitted subcircuit")?
                    .to_ascii_lowercase(),
            );
        } else if first.eq_ignore_ascii_case(".ends") {
            ensure!(scope.pop().is_some(), "unmatched emitted .ends");
        } else if let Some((positive, negative, kind, seen)) =
            expected.get_mut(&(scope.clone(), first.to_ascii_lowercase()))
        {
            ensure!(!*seen, "ambiguous PWL resistor native binding {first}");
            ensure!(
                fields
                    .next()
                    .is_some_and(|node| node.eq_ignore_ascii_case(positive))
                    && fields
                        .next()
                        .is_some_and(|node| node.eq_ignore_ascii_case(negative)),
                "incorrect native PWL resistor orientation for {first}"
            );
            ensure!(
                kind.matches(first, &fields.collect::<Vec<_>>().join(" ")),
                "incorrect native PWL resistor law or sensor for {first}"
            );
            *seen = true;
        }
    }
    for ((scope, name), (_, _, _, seen)) in expected {
        ensure!(
            seen,
            "missing PWL resistor native binding {name} in {scope:?} ({filename})"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curve_accepts_negative_interior_slopes_on_either_axis() -> Result<()> {
        let points = [(0., 0.), (1., 2.), (2., 1.), (3., 3.)];
        for axis in [Axis::Voltage, Axis::Current] {
            let knots = points.into_iter().map(|(x, y)| match axis {
                Axis::Voltage => Knot {
                    voltage: x,
                    current: y,
                },
                Axis::Current => Knot {
                    voltage: y,
                    current: x,
                },
            });
            validate_curve(axis, knots)?;
        }
        Ok(())
    }
}
