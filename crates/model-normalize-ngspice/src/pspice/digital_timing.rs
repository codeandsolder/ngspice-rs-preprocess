//! Source timing declarations for PSpice digital primitives.
//!
//! The reference guide (10.5, digital devices, pp. 345–365) distinguishes
//! directional propagation paths, timing checks and min/typ/max corners.
//! Keep these distinctions before resolving the model in an instance scope.

use crate::model_card::{ModelCard, ModelParameter};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

/// Timing model class, independent of the primitive's I/O model.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimingFamily {
    /// UGATE: standard gate.
    Gate,
    /// UTGATE: tristate gate.
    Tristate,
    /// UEFF: edge-triggered flip-flop.
    EdgeFlipFlop,
    /// UGFF: gated latch.
    GatedLatch,
    /// UDLY: transport delay line.
    DelayLine,
}

/// One declared timing corner, not a simulator-selected estimate.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimingCorner {
    /// Minimum.
    Minimum,
    /// Typical.
    Typical,
    /// Maximum.
    Maximum,
}

/// Directed propagation path or a timing check in the source model.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimingPath {
    /// Input change to low-to-high output.
    Rise,
    /// Input change to high-to-low output.
    Fall,
    /// Low output to high impedance.
    LowToZ,
    /// High output to high impedance.
    HighToZ,
    /// High impedance to high output.
    ZToHigh,
    /// High impedance to low output.
    ZToLow,
    /// Clock edge to rising output.
    ClockRise,
    /// Clock edge to falling output.
    ClockFall,
    /// Asynchronous preset/clear to rising output.
    PresetClearRise,
    /// Asynchronous preset/clear to falling output.
    PresetClearFall,
    /// Data setup before a clock edge.
    DataClockSetup,
    /// Data hold after a clock edge.
    DataClockHold,
    /// Inactive preset/clear setup before a clock edge.
    PresetClearClockSetup,
    /// Clock enable setup before a clock edge.
    EnableClockSetup,
    /// Clock enable hold after a clock edge.
    EnableClockHold,
    /// Minimum active-low preset/clear pulse width.
    PresetClearLowWidth,
    /// Minimum low clock pulse width.
    ClockLowWidth,
    /// Minimum high clock pulse width.
    ClockHighWidth,
    /// Data change to rising latch output.
    DataRise,
    /// Data change to falling latch output.
    DataFall,
    /// Gate change to rising latch output.
    GateRise,
    /// Gate change to falling latch output.
    GateFall,
    /// Data setup before the gate edge.
    DataGateSetup,
    /// Data hold after the gate edge.
    DataGateHold,
    /// Inactive preset/clear setup before the gate edge.
    PresetClearGateSetup,
    /// Minimum high gate pulse width.
    GateHighWidth,
    /// Transport delay for both transitions.
    Transport,
}

impl TimingFamily {
    fn parse(name: &str) -> Option<Self> {
        Some(match name.to_ascii_uppercase().as_str() {
            "UGATE" => Self::Gate,
            "UTGATE" => Self::Tristate,
            "UEFF" => Self::EdgeFlipFlop,
            "UGFF" => Self::GatedLatch,
            "UDLY" => Self::DelayLine,
            _ => return None,
        })
    }

    fn path(self, name: &str) -> Option<TimingPath> {
        use TimingPath as P;
        Some(match (self, name) {
            (Self::Gate | Self::Tristate, "TPLH") => P::Rise,
            (Self::Gate | Self::Tristate, "TPHL") => P::Fall,
            (Self::Tristate, "TPLZ") => P::LowToZ,
            (Self::Tristate, "TPHZ") => P::HighToZ,
            (Self::Tristate, "TPZH") => P::ZToHigh,
            (Self::Tristate, "TPZL") => P::ZToLow,
            (Self::EdgeFlipFlop, "TPCLKQLH") => P::ClockRise,
            (Self::EdgeFlipFlop, "TPCLKQHL") => P::ClockFall,
            (Self::EdgeFlipFlop | Self::GatedLatch, "TPPCQLH") => P::PresetClearRise,
            (Self::EdgeFlipFlop | Self::GatedLatch, "TPPCQHL") => P::PresetClearFall,
            (Self::EdgeFlipFlop, "TSUDCLK") => P::DataClockSetup,
            (Self::EdgeFlipFlop, "THDCLK") => P::DataClockHold,
            (Self::EdgeFlipFlop, "TSUPCCLKH") => P::PresetClearClockSetup,
            (Self::EdgeFlipFlop, "TSUCECLK") => P::EnableClockSetup,
            (Self::EdgeFlipFlop, "THCECLK") => P::EnableClockHold,
            (Self::EdgeFlipFlop | Self::GatedLatch, "TWPCL") => P::PresetClearLowWidth,
            (Self::EdgeFlipFlop, "TWCLKL") => P::ClockLowWidth,
            (Self::EdgeFlipFlop, "TWCLKH") => P::ClockHighWidth,
            (Self::GatedLatch, "TPDQLH") => P::DataRise,
            (Self::GatedLatch, "TPDQHL") => P::DataFall,
            (Self::GatedLatch, "TPGQLH") => P::GateRise,
            (Self::GatedLatch, "TPGQHL") => P::GateFall,
            (Self::GatedLatch, "TSUDG") => P::DataGateSetup,
            (Self::GatedLatch, "THDG") => P::DataGateHold,
            (Self::GatedLatch, "TSUPCGH") => P::PresetClearGateSetup,
            (Self::GatedLatch, "TWGH") => P::GateHighWidth,
            (Self::DelayLine, "DLY") => P::Transport,
            _ => return None,
        })
    }
}

/// A source-ordered timing assignment. Expressions and field spans survive
/// until binding; an omitted corner is distinct from an explicit zero.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TimingAssignment {
    /// Timing path or check.
    pub path: TimingPath,
    /// Source-selected corner of this assignment.
    pub corner: TimingCorner,
    /// Original field, expression and source span.
    pub field: ModelParameter,
}

impl TimingAssignment {
    fn parse(family: TimingFamily, field: &ModelParameter) -> Result<Self> {
        let name = field.name.to_ascii_uppercase();
        let (stem, corner) = if let Some(stem) = name.strip_suffix("MN") {
            (stem, TimingCorner::Minimum)
        } else if let Some(stem) = name.strip_suffix("TY") {
            (stem, TimingCorner::Typical)
        } else if let Some(stem) = name.strip_suffix("MX") {
            (stem, TimingCorner::Maximum)
        } else {
            anyhow::bail!("unknown PSpice timing field {}", field.name);
        };
        let path = family.path(stem).with_context(|| {
            format!("unsupported PSpice {family:?} timing field {}", field.name)
        })?;
        ensure!(
            field.expression.is_some(),
            "missing value for PSpice timing field {}",
            field.name
        );
        Ok(Self {
            path,
            corner,
            field: field.clone(),
        })
    }
}

/// Typed declaration only. Corner selection, defaults, model visibility and
/// physical event binding are intentionally separate from lexical parsing.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TimingModel {
    /// Source model identifier.
    pub name: String,
    /// Declared timing model class.
    pub family: TimingFamily,
    /// Original physical line.
    pub source_line: usize,
    /// Explicit assignments in declaration order, including repeated fields.
    pub assignments: Vec<TimingAssignment>,
}

impl TimingModel {
    /// Recognize a digital timing model without evaluating any expressions.
    /// Returns `None` for unrelated model families.
    ///
    /// # Errors
    /// Invalid or unqualified timing parameter, or missing explicit value.
    pub fn from_card(card: &ModelCard) -> Result<Option<Self>> {
        let Some(family) = TimingFamily::parse(&card.family) else {
            return Ok(None);
        };
        let assignments = card
            .parameters
            .iter()
            .map(|field| TimingAssignment::parse(family, field))
            .collect::<Result<Vec<_>>>()?;
        Ok(Some(Self {
            name: card.name.clone(),
            family,
            source_line: card.source_line,
            assignments,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model_card::parse_model_card;

    #[test]
    fn timing_families_preserve_path_corner_and_unevaluated_assignments() -> Result<()> {
        for (family, parameter, path) in [
            ("UGATE", "TPLHTY", TimingPath::Rise),
            ("UTGATE", "TPZLTY", TimingPath::ZToLow),
            ("UEFF", "THCECLKTY", TimingPath::EnableClockHold),
            ("UGFF", "TSUPCGHTY", TimingPath::PresetClearGateSetup),
            ("UDLY", "DLYTY", TimingPath::Transport),
        ] {
            let card = parse_model_card(
                &format!(".model timing {family}({parameter}={{delay}} {parameter}=0)"),
                7,
                &[],
            )?;
            let timing = TimingModel::from_card(&card)?.context("missing timing model")?;
            assert_eq!(timing.source_line, 7);
            assert_eq!(timing.assignments.len(), 2);
            assert_eq!(timing.assignments[0].path, path);
            assert_eq!(timing.assignments[0].corner, TimingCorner::Typical);
            assert_eq!(
                timing.assignments[0].field.expression.as_deref(),
                Some("{delay}")
            );
            assert_eq!(timing.assignments[1].field.expression.as_deref(), Some("0"));
        }
        Ok(())
    }

    #[test]
    fn rejects_wrong_family_and_missing_timing_values() -> Result<()> {
        for statement in [
            ".model m ugate(TPZLTY=1n)",
            ".model m ueff(THDCLKTY)",
            ".model m udly(FOOMX=1n)",
        ] {
            let card = parse_model_card(statement, 1, &[])?;
            assert!(TimingModel::from_card(&card).is_err(), "{statement}");
        }
        let card = parse_model_card(".model m d(is=1p)", 1, &[])?;
        assert!(TimingModel::from_card(&card)?.is_none());
        Ok(())
    }
}
