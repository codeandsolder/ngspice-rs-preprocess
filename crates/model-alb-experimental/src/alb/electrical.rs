//! Physical output electrical contract, independent of the logical program.
use super::{AlbDigitalBit, AlbDriveStrength, AlbLogicLevel};
use anyhow::{Result, ensure};
use serde::Deserialize;

/// Signed driver pull-up and pull-down timing resistance, in ohms.
/// These are not necessarily analog bridge conductance reciprocals.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct OutputResistance {
    pub positive: f64,
    pub negative: f64,
}

impl OutputResistance {
    #[must_use]
    pub const fn symmetric(ohms: f64) -> Self {
        Self {
            positive: ohms,
            negative: ohms,
        }
    }

    pub(super) fn validate(self) -> Result<()> {
        ensure!(
            [self.positive, self.negative]
                .into_iter()
                .all(f64::is_finite),
            "invalid output resistance {self:?}"
        );
        Ok(())
    }

    pub(super) const fn for_transition(self, next: AlbDigitalBit) -> f64 {
        match next.level {
            AlbLogicLevel::One => self.positive,
            AlbLogicLevel::Zero => self.negative,
            AlbLogicLevel::Unknown => self.positive.midpoint(self.negative),
        }
    }
}

/// Inclusive signed aggregate current limits and directional output resistance.
#[derive(Clone, Copy, Debug)]
pub struct PhysicalOutputDrive {
    pub resistance: OutputResistance,
    pub min_sink_a: f64,
    pub max_source_a: f64,
}

impl PhysicalOutputDrive {
    pub(super) fn validate(self) -> Result<()> {
        self.resistance.validate()?;
        ensure!(
            self.min_sink_a.is_finite() && self.max_source_a.is_finite(),
            "invalid signed output current limits {self:?}"
        );
        Ok(())
    }

    #[must_use]
    pub const fn unrestricted(resistance: OutputResistance) -> Self {
        Self {
            resistance,
            min_sink_a: -1e99,
            max_source_a: 1e99,
        }
    }

    pub(super) fn resolve(self, bit: AlbDigitalBit, load: PhysicalOutputLoad) -> AlbDigitalBit {
        if bit.strength == AlbDriveStrength::Strong
            && ((bit.level == AlbLogicLevel::Zero && load.sink_a < self.min_sink_a)
                || (bit.level == AlbLogicLevel::One && load.source_a > self.max_source_a))
        {
            AlbDigitalBit {
                level: AlbLogicLevel::Unknown,
                strength: AlbDriveStrength::Strong,
            }
        } else {
            bit
        }
    }
}

/// Static signed electrical load sums of a physical output bundle.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PhysicalOutputLoad {
    pub capacitance_f: f64,
    pub sink_a: f64,
    pub source_a: f64,
}

impl PhysicalOutputLoad {
    #[must_use]
    pub const fn capacitive(capacitance_f: f64) -> Self {
        Self {
            capacitance_f,
            sink_a: 0.0,
            source_a: 0.0,
        }
    }

    #[must_use]
    pub const fn is_finite(self) -> bool {
        self.capacitance_f.is_finite() && self.sink_a.is_finite() && self.source_a.is_finite()
    }

    #[must_use]
    pub const fn same_bits(self, other: Self) -> bool {
        self.capacitance_f.to_bits() == other.capacitance_f.to_bits()
            && self.sink_a.to_bits() == other.sink_a.to_bits()
            && self.source_a.to_bits() == other.source_a.to_bits()
    }
}
