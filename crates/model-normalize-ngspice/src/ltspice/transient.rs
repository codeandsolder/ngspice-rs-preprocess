//! `LTspice` transient intent and occurrence-bound timing. The output window,
//! integration cap and initialization/termination modes are independent.

use anyhow::{Context, Result, ensure};
use model_preprocess_command::{Resource, WorkBudget};
use serde::{Deserialize, Serialize};

/// Independent source transient modifier.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransientModifier {
    /// Skip the operating-point solve.
    Uic,
    /// Source-specific initialization and source startup ramp.
    Startup,
    /// Enable steady-state detection and early termination.
    Steady,
    /// Preserve data prior to steady-state detection.
    Nodiscard,
    /// Request the step response instead of the ordinary transient.
    StepResponse,
    /// Request convergence diagnostics.
    ConvergenceReport,
}

impl TransientModifier {
    fn parse(field: &str) -> Option<Self> {
        Some(match field.to_ascii_lowercase().as_str() {
            "uic" => Self::Uic,
            "startup" => Self::Startup,
            "steady" => Self::Steady,
            "nodiscard" => Self::Nodiscard,
            "step" => Self::StepResponse,
            "convreport" => Self::ConvergenceReport,
            _ => return None,
        })
    }
}

/// Source transient modifiers, retaining independent combinations.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TransientModes(std::collections::BTreeSet<TransientModifier>);

impl TransientModes {
    /// Whether a particular original source modifier was requested.
    #[must_use = "each modifier controls its own source analysis policy"]
    pub fn contains(&self, modifier: TransientModifier) -> bool {
        self.0.contains(&modifier)
    }

    /// The ordinary native transient card cannot express the other modes.
    #[must_use = "non-native modes require a separately qualified execution policy"]
    pub fn native_compatible(&self) -> bool {
        self.0
            .iter()
            .all(|modifier| *modifier == TransientModifier::Uic)
    }
}

/// Timing declarations in the source scalar language, before parameter binding.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TransientIntent {
    /// Plotting increment / initial integration step hint, if specified.
    pub interval: Option<String>,
    /// Required final time.
    pub stop: String,
    /// Start of the saved output window.
    pub output_start: Option<String>,
    /// Explicit integration step cap.
    pub max_step: Option<String>,
    /// Independent initialization, termination and output modifiers.
    pub modes: TransientModes,
}

/// Numeric timing and requested policy, independent of source spelling.
#[derive(Clone, Debug, PartialEq)]
pub struct BoundTransient {
    /// Plotting increment / initial step hint.
    pub interval: f64,
    /// Final time in seconds.
    pub stop: f64,
    /// First saved output time; integration still begins at zero.
    pub output_start: f64,
    /// Maximum accepted integration step.
    pub max_step: f64,
    /// Original source execution policies.
    pub modes: TransientModes,
}

impl BoundTransient {
    /// Native-equivalent timing operands, excluding the UIC flag.
    ///
    /// # Errors
    /// Source-specific execution modes that need explicit consumer support.
    pub fn native_timing(&self) -> Result<[f64; 4]> {
        ensure!(
            self.modes.native_compatible(),
            "LTspice transient modes require typed analysis binding"
        );
        Ok([self.interval, self.stop, self.output_start, self.max_step])
    }
}

impl TransientIntent {
    /// Parse ordered timing fields followed by source modifiers. State-file
    /// operations require admitted resource identities and are not ordinary
    /// timing modifiers.
    ///
    /// # Errors
    /// Invalid arity, repeated modifiers, or unsupported state-file operations.
    pub fn parse(fields: &[&str]) -> Result<Self> {
        let mut modes = TransientModes::default();
        let mut timing = Vec::new();
        let mut flags_started = false;
        for &field in fields {
            if let Some(modifier) = TransientModifier::parse(field) {
                ensure!(
                    modes.0.insert(modifier),
                    "duplicate LTspice transient modifier {field}"
                );
                flags_started = true;
            } else {
                let keyword = field.split_once('=').map_or(field, |(key, _)| key);
                ensure!(
                    !["loadstate", "savestate", "savestatetime"]
                        .iter()
                        .any(|name| keyword.eq_ignore_ascii_case(name)),
                    "LTspice transient state-file operation requires admitted state binding"
                );
                ensure!(
                    !flags_started,
                    "unexpected transient operand after modifier: {field}"
                );
                ensure!(
                    timing.len() < 4,
                    "unsupported LTspice transient timing arity"
                );
                timing.push(field);
            }
        }
        let (interval, stop, output_start, max_step) = match timing.as_slice() {
            [stop] => (None, *stop, None, None),
            [interval, stop, rest @ ..] => (
                Some(*interval),
                *stop,
                rest.first().copied(),
                rest.get(1).copied(),
            ),
            _ => anyhow::bail!("missing LTspice transient duration"),
        };
        Ok(Self {
            interval: interval.map(str::to_owned),
            stop: stop.to_owned(),
            output_start: output_start.map(str::to_owned),
            max_step: max_step.map(str::to_owned),
            modes,
        })
    }

    /// Resolve timing fields against the actual candidate, then apply the
    /// source-specific zero/omitted defaults. The default integration cap is
    /// Tstop/1024, independent of the output interval or saved output window.
    ///
    /// # Errors
    /// Missing scalar bindings or invalid/nonrepresentable timing.
    pub fn bind(&self, mut resolve: impl FnMut(&str) -> Result<f64>) -> Result<BoundTransient> {
        let interval = self.interval.as_deref().map_or(Ok(0.), &mut resolve)?;
        let stop = resolve(&self.stop)?;
        let output_start = self.output_start.as_deref().map_or(Ok(0.), &mut resolve)?;
        let max_step = self.max_step.as_deref().map_or(Ok(0.), &mut resolve)?;
        self.bind_values(interval, stop, output_start, max_step)
    }

    fn bind_values(
        &self,
        interval: f64,
        stop: f64,
        output_start: f64,
        max_step: f64,
    ) -> Result<BoundTransient> {
        ensure!(
            [interval, stop, output_start, max_step]
                .iter()
                .all(|value| value.is_finite()),
            "nonfinite LTspice transient timing"
        );
        let output_start = output_start.max(0.);
        ensure!(
            stop > output_start && max_step >= 0.,
            "LTspice transient needs positive duration and nonnegative maximum step"
        );
        let max_step = if max_step > 0. {
            max_step
        } else {
            stop / 1024.
        };
        let interval = if interval > 0. { interval } else { max_step };
        ensure!(
            interval > 0. && max_step > 0.,
            "unrepresentable LTspice transient timestep"
        );
        Ok(BoundTransient {
            interval,
            stop,
            output_start,
            max_step,
            modes: self.modes.clone(),
        })
    }

    /// Validate every closed field in the source expression language.
    /// Return a bound timing only when no field needs lexical binding.
    ///
    /// # Errors
    /// Malformed scalar syntax, nonfinite or invalid closed timing, or work
    /// exhaustion. An unresolved name is not replaced with a nominal value.
    pub fn try_bind_closed(&self, budget: &WorkBudget) -> Result<Option<BoundTransient>> {
        let scalar = |raw: &str| {
            budget.charge(Resource::ScannedBytes, raw.len())?;
            let text = raw.trim();
            let grouped = text
                .strip_prefix('{')
                .and_then(|text| text.strip_suffix('}'));
            let expression = grouped.unwrap_or(text);
            crate::model_card::parse_spice_constant(expression).map_or_else(
                |_| super::constants::try_evaluate(expression, grouped.is_none(), budget),
                |value| Ok(Some(value)),
            )
        };
        let interval = self.interval.as_deref().map_or(Ok(Some(0.)), scalar)?;
        let stop = scalar(&self.stop)?;
        let output_start = self.output_start.as_deref().map_or(Ok(Some(0.)), scalar)?;
        let max_step = self.max_step.as_deref().map_or(Ok(Some(0.)), scalar)?;
        ensure!(
            stop.is_none_or(|value| value > 0.),
            "LTspice transient needs positive duration"
        );
        ensure!(
            max_step.is_none_or(|value| value >= 0.),
            "LTspice transient needs nonnegative maximum step"
        );
        if let (Some(stop), Some(start)) = (stop, output_start) {
            ensure!(
                stop > start.max(0.),
                "LTspice transient output window starts after final time"
            );
        }
        let (Some(interval), Some(stop), Some(output_start), Some(max_step)) =
            (interval, stop, output_start, max_step)
        else {
            return Ok(None);
        };
        self.bind_values(interval, stop, output_start, max_step)
            .map(Some)
    }

    /// Bind an entirely closed source declaration using the source's scalar
    /// expression parser, not the native parser's arithmetic conventions.
    ///
    /// # Errors
    /// Symbolic timing requiring occurrence binding, invalid scalar or timing.
    pub fn bind_closed(&self, budget: &WorkBudget) -> Result<BoundTransient> {
        self.try_bind_closed(budget)?
            .context("LTspice transient timing requires occurrence-specific scalar binding")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model_card::parse_spice_constant;

    #[test]
    fn timing_resolves_per_candidate_and_keeps_saved_window_separate() -> Result<()> {
        let parameter_name = "duration";
        let expression = format!("{{{parameter_name}}}");
        let intent = TransientIntent::parse(&["0", &expression, "-1m", "0"])?;
        for duration in [1e-3, 5e-3, 0.2] {
            let bound = intent.bind(|raw| {
                if raw == expression {
                    Ok(duration)
                } else {
                    parse_spice_constant(raw)
                }
            })?;
            assert_eq!(bound.stop.to_bits(), duration.to_bits());
            assert_eq!(bound.output_start.to_bits(), 0_f64.to_bits());
            assert_eq!(bound.max_step.to_bits(), (duration / 1024.).to_bits());
            assert_eq!(bound.interval.to_bits(), bound.max_step.to_bits());
        }
        let explicit = TransientIntent::parse(&["1u", "10m", "2m", "50n", "UIC"])?;
        let bound = explicit.bind_closed(&WorkBudget::default())?;
        assert_eq!(bound.output_start.to_bits(), 0.002_f64.to_bits());
        assert_eq!(bound.interval.to_bits(), 1e-6_f64.to_bits());
        assert!((bound.max_step - 50e-9).abs() < 1e-20);
        assert!(bound.modes.contains(TransientModifier::Uic));
        Ok(())
    }

    #[test]
    fn source_modes_are_never_silently_substituted() -> Result<()> {
        let startup =
            TransientIntent::parse(&["0", "1m", "startup"])?.bind_closed(&WorkBudget::default())?;
        assert!(
            startup.modes.contains(TransientModifier::Startup)
                && !startup.modes.contains(TransientModifier::Uic)
        );
        assert!(startup.native_timing().is_err());
        let steady = TransientIntent::parse(&["1m", "steady", "nodiscard"])?
            .bind_closed(&WorkBudget::default())?;
        assert!(
            steady.modes.contains(TransientModifier::Steady)
                && steady.modes.contains(TransientModifier::Nodiscard)
        );
        assert!(steady.native_timing().is_err());
        assert!(TransientIntent::parse(&["1m", "startup", "startup"]).is_err());
        assert!(TransientIntent::parse(&["1m", "savestate", "=", "state.dat"]).is_err());
        assert!(TransientIntent::parse(&["0", "1m", "startup", "1u"]).is_err());
        Ok(())
    }
}
