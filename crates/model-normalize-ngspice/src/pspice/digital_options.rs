//! Circuit-wide digital option values, resolved before instance binding.
//!
//! Reference: PSpice A/D Reference Guide 10.5, pp. 70 and 352.

use super::{
    digital_corners::{DelaySelection, TimingScales},
    digital_io::InterfaceLevel,
};
use anyhow::{Result, ensure};
use serde::Serialize;
use std::collections::BTreeMap;

/// Circuit-wide option understood by the digital occurrence binder.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DigitalOption {
    /// Global delay selection.
    DelaySelection,
    /// Global interface level.
    InterfaceLevel,
    /// Minimum-from-typical propagation factor.
    MinimumScale,
    /// Maximum-from-typical propagation factor.
    MaximumScale,
    /// Initial latch/flip-flop state.
    InitialState,
    /// Digital time resolution frequency.
    Frequency,
    /// Minimum drive resistance.
    DriveMinimum,
    /// Maximum drive resistance.
    DriveMaximum,
    /// Drive resistance ratio required to override another output.
    OverrideRatio,
}

impl DigitalOption {
    /// Recognize a PSpice digital option name, case-insensitively.
    #[must_use = "unrecognized options need a separate source analysis policy"]
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name.to_ascii_uppercase().as_str() {
            "DIGMNTYMX" => Self::DelaySelection,
            "DIGIOLVL" => Self::InterfaceLevel,
            "DIGMNTYSCALE" => Self::MinimumScale,
            "DIGTYMXSCALE" => Self::MaximumScale,
            "DIGINITSTATE" => Self::InitialState,
            "DIGFREQ" => Self::Frequency,
            "DIGDRVF" => Self::DriveMinimum,
            "DIGDRVZ" => Self::DriveMaximum,
            "DIGOVRDRV" => Self::OverrideRatio,
            _ => return None,
        })
    }
}

/// Requested starting state of sequential primitives, subject to input state.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InitialState {
    /// Clear.
    Clear,
    /// Set.
    Set,
    /// Unknown.
    Unknown,
}

/// Resolved circuit-wide digital quantities. Engine topology and event
/// scheduling are separate from this source binding.
#[derive(Clone, Copy, Debug)]
pub struct DigitalOptions {
    /// Global delay selector (instance selectors override this).
    pub delay_selection: DelaySelection,
    /// Global mixed-domain interface level.
    pub interface_level: InterfaceLevel,
    /// Extrapolation factors for missing propagation corners.
    pub timing_scales: TimingScales,
    /// Sequential initialization request.
    pub initial_state: InitialState,
    /// Digital frequency in hertz; minimum digital step is its reciprocal.
    pub frequency: f64,
    /// Minimum drive resistance in ohms.
    pub drive_minimum: f64,
    /// Maximum drive resistance in ohms.
    pub drive_maximum: f64,
    /// Required drive override ratio.
    pub override_ratio: f64,
}

impl DigitalOptions {
    /// Bind effective global values with documented defaults.
    ///
    /// The caller resolves the ordered source option assignments and retains
    /// only the effective value of each option. No event rounding or
    /// electrical-strength quantization is performed at this boundary.
    ///
    /// # Errors
    /// Nonfinite or invalid option values.
    pub fn from_effective(values: &BTreeMap<DigitalOption, f64>) -> Result<Self> {
        ensure!(
            values.values().all(|value| value.is_finite()),
            "nonfinite PSpice digital option"
        );
        use DigitalOption as O;
        let get = |option, default| values.get(&option).copied().unwrap_or(default);
        let options = Self {
            delay_selection: DelaySelection::resolve(
                None,
                values.get(&O::DelaySelection).copied(),
            )?,
            interface_level: InterfaceLevel::resolve(
                None,
                values.get(&O::InterfaceLevel).copied(),
            )?,
            timing_scales: TimingScales::new(get(O::MinimumScale, 0.4), get(O::MaximumScale, 1.6))?,
            initial_state: match get(O::InitialState, 2.) {
                0. => InitialState::Clear,
                1. => InitialState::Set,
                _ => InitialState::Unknown,
            },
            frequency: get(O::Frequency, 1e10),
            drive_minimum: get(O::DriveMinimum, 2.),
            drive_maximum: get(O::DriveMaximum, 20_000.),
            override_ratio: get(O::OverrideRatio, 3.),
        };
        ensure!(
            options.frequency > 0. && options.frequency.recip().is_finite(),
            "invalid PSpice DIGFREQ"
        );
        ensure!(
            options.drive_minimum > 0. && options.drive_maximum >= options.drive_minimum,
            "invalid PSpice drive-resistance range"
        );
        ensure!(options.override_ratio >= 1., "invalid PSpice DIGOVRDRV");
        Ok(options)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_and_overridden_digital_options() -> Result<()> {
        let options = DigitalOptions::from_effective(&BTreeMap::new())?;
        assert_eq!(options.delay_selection, DelaySelection::Typical);
        assert_eq!(options.interface_level, InterfaceLevel::One);
        assert_eq!(options.initial_state, InitialState::Unknown);
        assert_eq!(options.frequency.to_bits(), 1e10_f64.to_bits());
        assert_eq!(options.drive_minimum.to_bits(), 2_f64.to_bits());
        assert_eq!(options.drive_maximum.to_bits(), 20_000_f64.to_bits());
        let values = BTreeMap::from([
            (DigitalOption::DelaySelection, 4.),
            (DigitalOption::InterfaceLevel, 3.),
            (DigitalOption::InitialState, 0.),
            (DigitalOption::Frequency, 1e9),
        ]);
        let options = DigitalOptions::from_effective(&values)?;
        assert_eq!(options.delay_selection, DelaySelection::WorstCase);
        assert_eq!(options.interface_level, InterfaceLevel::Three);
        assert_eq!(options.initial_state, InitialState::Clear);
        assert_eq!(options.frequency.to_bits(), 1e9_f64.to_bits());
        assert!(
            DigitalOptions::from_effective(&BTreeMap::from([(DigitalOption::Frequency, 0.)]))
                .is_err()
        );
        assert!(
            DigitalOptions::from_effective(&BTreeMap::from([(DigitalOption::DriveMaximum, 1.)]))
                .is_err()
        );
        Ok(())
    }
}
