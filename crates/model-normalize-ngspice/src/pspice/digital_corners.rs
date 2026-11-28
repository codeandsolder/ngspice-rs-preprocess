//! Source timing-corner completion and selection, independent of event execution.
//!
//! Reference: `PSpice` A/D Reference Guide 9.0, pp. 252–253. Propagation
//! delays and timing checks have different omitted-corner completion rules.

use super::{
    digital_source::{DigitalDeclaration, Location},
    digital_timing::{TimingCorner, TimingPath},
};
use crate::model_card::ModelParameter;
use anyhow::{Context, Result, ensure};
use model_preprocess_command::{Resource, WorkBudget};
use serde::Serialize;
use std::collections::BTreeMap;

/// Resolved delay selection. Worst-case mode is not one nominal delay.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DelaySelection {
    /// Minimum corner.
    Minimum,
    /// Typical corner.
    Typical,
    /// Maximum corner.
    Maximum,
    /// Earliest/latest window, retaining both endpoints.
    WorstCase,
}

impl DelaySelection {
    /// Parse the circuit-wide numeric selector.
    ///
    /// # Errors
    /// Invalid or nonintegral selector.
    pub fn from_global(global: Option<f64>) -> Result<Self> {
        ensure!(
            global != Some(0.),
            "invalid global PSpice delay selector zero"
        );
        Self::Typical.override_with(global)
    }

    /// An omitted or zero instance selector delegates to the already
    /// validated circuit-wide value.
    ///
    /// # Errors
    /// Invalid or nonintegral selector.
    pub fn override_with(self, instance: Option<f64>) -> Result<Self> {
        Ok(match instance {
            None | Some(0.) => self,
            Some(1.) => Self::Minimum,
            Some(2.) => Self::Typical,
            Some(3.) => Self::Maximum,
            Some(4.) => Self::WorstCase,
            Some(value) => anyhow::bail!("invalid PSpice delay selector {value}"),
        })
    }
}

/// Source propagation extrapolation factors. These do not apply to checks.
#[derive(Clone, Copy, Debug)]
pub struct TimingScales {
    minimum_from_typical: f64,
    maximum_from_typical: f64,
}

impl Default for TimingScales {
    fn default() -> Self {
        Self {
            minimum_from_typical: 0.4,
            maximum_from_typical: 1.6,
        }
    }
}

impl TimingScales {
    /// Construct validated DIGMNTYSCALE and DIGTYMXSCALE values.
    ///
    /// # Errors
    /// Minimum factor outside [0,1] or maximum factor not finite and >1.
    pub fn new(minimum_from_typical: f64, maximum_from_typical: f64) -> Result<Self> {
        ensure!(
            (0. ..=1.).contains(&minimum_from_typical),
            "invalid PSpice DIGMNTYSCALE"
        );
        ensure!(
            maximum_from_typical.is_finite() && maximum_from_typical > 1.,
            "invalid PSpice DIGTYMXSCALE"
        );
        Ok(Self {
            minimum_from_typical,
            maximum_from_typical,
        })
    }
}

/// A complete numerical corner set in seconds.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct CornerValues {
    /// Minimum.
    pub minimum: f64,
    /// Typical.
    pub typical: f64,
    /// Maximum.
    pub maximum: f64,
}

/// Selected numeric timing value or a retained worst-case interval.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SelectedTiming {
    /// Exact selected corner.
    Point {
        /// Seconds.
        value: f64,
    },
    /// Earliest/latest endpoints of the source timing interval.
    Window {
        /// Lower bound in seconds.
        minimum: f64,
        /// Upper bound in seconds.
        maximum: f64,
    },
}

impl CornerValues {
    /// Select a corner without collapsing worst-case timing.
    ///
    /// # Errors
    /// Inverted endpoints in worst-case mode.
    pub fn select(self, selection: DelaySelection) -> Result<SelectedTiming> {
        let value = match selection {
            DelaySelection::Minimum => self.minimum,
            DelaySelection::Typical => self.typical,
            DelaySelection::Maximum => self.maximum,
            DelaySelection::WorstCase => {
                ensure!(
                    self.minimum <= self.maximum,
                    "inverted PSpice worst-case timing interval"
                );
                return Ok(SelectedTiming::Window {
                    minimum: self.minimum,
                    maximum: self.maximum,
                });
            }
        };
        Ok(SelectedTiming::Point { value })
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct WrittenCorners {
    minimum: Option<f64>,
    typical: Option<f64>,
    maximum: Option<f64>,
}

impl WrittenCorners {
    const fn set(&mut self, corner: TimingCorner, value: f64) {
        match corner {
            TimingCorner::Minimum => self.minimum = Some(value),
            TimingCorner::Typical => self.typical = Some(value),
            TimingCorner::Maximum => self.maximum = Some(value),
        }
    }

    fn propagation(self, scales: TimingScales) -> Result<CornerValues> {
        let typical = match (self.minimum, self.typical, self.maximum) {
            (_, Some(typical), _) => typical,
            (Some(minimum), None, Some(maximum)) => minimum.midpoint(maximum),
            (Some(minimum), None, None) => {
                ensure!(
                    scales.minimum_from_typical > 0.,
                    "PSpice cannot derive typical delay from minimum with zero DIGMNTYSCALE"
                );
                minimum / scales.minimum_from_typical
            }
            (None, None, Some(maximum)) => maximum / scales.maximum_from_typical,
            (None, None, None) => 0.,
        };
        Ok(CornerValues {
            minimum: self
                .minimum
                .unwrap_or(typical * scales.minimum_from_typical),
            typical,
            maximum: self
                .maximum
                .unwrap_or(typical * scales.maximum_from_typical),
        })
    }

    fn constraint(self) -> CornerValues {
        let minimum = self.minimum.unwrap_or(0.);
        let maximum = self.maximum.or(self.typical).unwrap_or(minimum);
        let typical = self.typical.unwrap_or_else(|| minimum.midpoint(maximum));
        CornerValues {
            minimum,
            typical,
            maximum,
        }
    }

    fn transport(self) -> Result<CornerValues> {
        // The documented TP extrapolation rules do not explicitly cover DLY.
        // Preserve complete declarations and the documented all-absent zero.
        let (minimum, typical, maximum) = match (self.minimum, self.typical, self.maximum) {
            (Some(minimum), Some(typical), Some(maximum)) => (minimum, typical, maximum),
            (None, None, None) => (0., 0., 0.),
            _ => anyhow::bail!(
                "PSpice UDLY partial corner completion requires independent qualification"
            ),
        };
        Ok(CornerValues {
            minimum,
            typical,
            maximum,
        })
    }

    fn complete(self, path: TimingPath, scales: TimingScales) -> Result<CornerValues> {
        let values = if path == TimingPath::Transport {
            self.transport()?
        } else if path.propagation() {
            self.propagation(scales)?
        } else {
            self.constraint()
        };
        let values_array = [values.minimum, values.typical, values.maximum];
        ensure!(
            values_array.iter().all(|value| value.is_finite()),
            "nonfinite PSpice timing corners for {path:?}"
        );
        ensure!(
            !(path.propagation() || path == TimingPath::Transport)
                || values_array.iter().all(|value| *value >= 0.),
            "negative PSpice propagation delay for {path:?}"
        );
        Ok(values)
    }
}

/// Complete per-path timing corners; physical loading and interface timing
/// compensation still belong to the occurrence/event binding stage.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct BoundPath {
    /// Directed path or constraint.
    pub path: TimingPath,
    /// Completed numeric corners.
    pub corners: CornerValues,
}

impl DigitalDeclaration {
    /// Bind effective timing fields in their own lexical declaration context.
    ///
    /// Only the effective assignment for each path/corner is evaluated.
    /// Source expressions remain owned by the caller's occurrence resolver.
    ///
    /// # Errors
    /// Unresolvable expressions, invalid timings, or exhausted work budget.
    pub(super) fn bind_timing(
        &self,
        scales: TimingScales,
        mut resolve: impl FnMut(&Location, &ModelParameter) -> Result<f64>,
        budget: &WorkBudget,
    ) -> Result<Vec<BoundPath>> {
        let Some(family) = self.instance.declaration.primitive.timing_family() else {
            ensure!(
                self.timing.is_empty(),
                "untimed pull has an unexpected timing model"
            );
            return Ok(Vec::new());
        };
        ensure!(!self.timing.is_empty(), "missing effective timing model");
        let mut effective = BTreeMap::new();
        for layer in &self.timing {
            ensure!(
                layer.declaration.family == family,
                "mismatched timing model layer"
            );
            for assignment in &layer.declaration.assignments {
                budget.charge(Resource::EvaluationSteps, 1)?;
                effective.insert(
                    (assignment.path, assignment.corner),
                    (&layer.location, &assignment.field),
                );
            }
        }
        let mut written = BTreeMap::<TimingPath, WrittenCorners>::new();
        for ((path, corner), (location, field)) in effective {
            budget.charge(Resource::EvaluationSteps, 1)?;
            let value = resolve(location, field)
                .with_context(|| format!("PSpice timing field {} at {location:?}", field.name))?;
            ensure!(
                value.is_finite(),
                "nonfinite PSpice timing expression {}",
                field.name
            );
            ensure!(
                !(path.propagation() || path == TimingPath::Transport) || value >= 0.,
                "negative PSpice propagation expression {}",
                field.name
            );
            written.entry(path).or_default().set(corner, value);
        }
        family
            .paths()
            .map(|path| {
                budget.charge(Resource::GeneratedNodes, 1)?;
                let corners = written
                    .remove(&path)
                    .unwrap_or_default()
                    .complete(path, scales)?;
                Ok(BoundPath { path, corners })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn propagation_and_constraint_completion_are_not_interchangeable() -> Result<()> {
        let scales = TimingScales::default();
        for (written, expected) in [
            (
                WrittenCorners {
                    typical: Some(10.),
                    ..WrittenCorners::default()
                },
                [4., 10., 16.],
            ),
            (
                WrittenCorners {
                    minimum: Some(4.),
                    ..WrittenCorners::default()
                },
                [4., 10., 16.],
            ),
            (
                WrittenCorners {
                    maximum: Some(16.),
                    ..WrittenCorners::default()
                },
                [4., 10., 16.],
            ),
            (
                WrittenCorners {
                    minimum: Some(2.),
                    maximum: Some(10.),
                    typical: None,
                },
                [2., 6., 10.],
            ),
            (
                WrittenCorners {
                    minimum: Some(0.),
                    typical: Some(10.),
                    maximum: None,
                },
                [0., 10., 16.],
            ),
            (WrittenCorners::default(), [0., 0., 0.]),
        ] {
            let actual = written.complete(TimingPath::Rise, scales)?;
            assert_eq!(
                [
                    actual.minimum.to_bits(),
                    actual.typical.to_bits(),
                    actual.maximum.to_bits()
                ],
                expected.map(f64::to_bits)
            );
        }
        let checks = WrittenCorners {
            typical: Some(10.),
            ..WrittenCorners::default()
        }
        .complete(TimingPath::DataClockSetup, scales)?;
        assert_eq!(
            [
                checks.minimum.to_bits(),
                checks.typical.to_bits(),
                checks.maximum.to_bits()
            ],
            [0_f64, 10., 10.].map(f64::to_bits)
        );
        let checks = WrittenCorners {
            minimum: Some(2.),
            ..WrittenCorners::default()
        }
        .complete(TimingPath::DataClockHold, scales)?;
        assert_eq!(
            [
                checks.minimum.to_bits(),
                checks.typical.to_bits(),
                checks.maximum.to_bits()
            ],
            [2_f64, 2., 2.].map(f64::to_bits)
        );
        assert!(
            WrittenCorners {
                minimum: Some(1.),
                ..WrittenCorners::default()
            }
            .complete(TimingPath::Rise, TimingScales::new(0., 1.6)?)
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn delay_selection_keeps_worst_case_intervals() -> Result<()> {
        assert_eq!(DelaySelection::from_global(None)?, DelaySelection::Typical);
        assert_eq!(
            DelaySelection::WorstCase.override_with(Some(0.))?,
            DelaySelection::WorstCase
        );
        assert_eq!(
            DelaySelection::WorstCase.override_with(Some(1.))?,
            DelaySelection::Minimum
        );
        assert!(DelaySelection::Typical.override_with(Some(2.5)).is_err());
        let corners = CornerValues {
            minimum: 1.,
            typical: 2.,
            maximum: 3.,
        };
        let SelectedTiming::Window { minimum, maximum } =
            corners.select(DelaySelection::WorstCase)?
        else {
            anyhow::bail!("worst-case interval was collapsed");
        };
        assert_eq!(
            (minimum.to_bits(), maximum.to_bits()),
            (1_f64.to_bits(), 3_f64.to_bits())
        );
        Ok(())
    }
}
