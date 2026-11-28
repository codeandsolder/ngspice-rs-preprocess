//! Digital loading, drive and mixed-domain interface declarations.

use crate::model_card::{ModelCard, ModelParameter};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

/// One of the four source-selectable mixed-domain interface levels.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterfaceLevel {
    /// Level one.
    One,
    /// Level two.
    Two,
    /// Level three.
    Three,
    /// Level four.
    Four,
}

impl InterfaceLevel {
    const fn parse(level: &str) -> Option<Self> {
        match level.as_bytes() {
            b"1" => Some(Self::One),
            b"2" => Some(Self::Two),
            b"3" => Some(Self::Three),
            b"4" => Some(Self::Four),
            _ => None,
        }
    }
}

/// Physical or interface property; the original expression remains unresolved.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "level", rename_all = "snake_case")]
pub enum IoProperty {
    /// Input capacitance (farads).
    InputLoad,
    /// Output capacitance (farads).
    OutputLoad,
    /// Input leakage resistance (ohms).
    InputResistance,
    /// High-state output resistance (ohms).
    DriveHigh,
    /// Low-state output resistance (ohms).
    DriveLow,
    /// High-impedance leakage resistance (ohms).
    DriveZ,
    /// Pulse rejection threshold (seconds).
    PulseWidthRejection,
    /// Minimum retained charge-storage time (seconds).
    StorageTime,
    /// Power supply subcircuit name.
    PowerSupply,
    /// Analog-to-digital subcircuit name.
    AnalogToDigital(InterfaceLevel),
    /// Digital-to-analog subcircuit name.
    DigitalToAnalog(InterfaceLevel),
    /// Low-to-high threshold crossing compensation (seconds).
    SwitchingRise(InterfaceLevel),
    /// High-to-low threshold crossing compensation (seconds).
    SwitchingFall(InterfaceLevel),
}

impl IoProperty {
    fn parse(name: &str) -> Option<Self> {
        let upper = name.to_ascii_uppercase();
        Some(match upper.as_str() {
            "INLD" => Self::InputLoad,
            "OUTLD" => Self::OutputLoad,
            "INR" => Self::InputResistance,
            "DRVH" => Self::DriveHigh,
            "DRVL" => Self::DriveLow,
            "DRVZ" => Self::DriveZ,
            "TPWRT" => Self::PulseWidthRejection,
            "TSTOREMN" => Self::StorageTime,
            "DIGPOWER" => Self::PowerSupply,
            _ => {
                let (stem, level) = upper.split_at_checked(upper.len().checked_sub(1)?)?;
                let level = InterfaceLevel::parse(level)?;
                match stem {
                    "ATOD" => Self::AnalogToDigital(level),
                    "DTOA" => Self::DigitalToAnalog(level),
                    "TSWLH" => Self::SwitchingRise(level),
                    "TSWHL" => Self::SwitchingFall(level),
                    _ => return None,
                }
            }
        })
    }
}

/// Source I/O model assignment in declaration order.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct IoAssignment {
    /// Physical or interface property.
    pub property: IoProperty,
    /// Original source field including its expression/span.
    pub field: ModelParameter,
}

/// Typed UIO declaration. Do not equate its drive strength with voltage.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct IoModel {
    /// Declared model name.
    pub name: String,
    /// Source line.
    pub source_line: usize,
    /// Original field ordering, including repeated assignments.
    pub assignments: Vec<IoAssignment>,
}

impl IoModel {
    /// Recognize a UIO card without selecting interfaces or electrical defaults.
    ///
    /// # Errors
    /// Invalid or unqualified UIO field, or missing explicit value.
    pub fn from_card(card: &ModelCard) -> Result<Option<Self>> {
        if !card.family.eq_ignore_ascii_case("UIO") {
            return Ok(None);
        }
        let assignments = card
            .parameters
            .iter()
            .map(|field| {
                let property = IoProperty::parse(&field.name)
                    .with_context(|| format!("unsupported PSpice UIO property {}", field.name))?;
                ensure!(
                    field.expression.is_some(),
                    "missing UIO property value for {}",
                    field.name
                );
                Ok(IoAssignment {
                    property,
                    field: field.clone(),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Some(Self {
            name: card.name.clone(),
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
    fn separate_strength_loading_interface_and_compensation() -> Result<()> {
        let card = parse_model_card(
            ".model Io UIO(DRVH=50 DRVL={sink} DRVZ=250k INLD=0 OUTLD=2p INR=30k TPWRT=0 TSTOREMN=1m AtoD4=rx DtoA2=tx TSWHL2=2n TSWLH2=3n DIGPOWER=pwr)",
            4,
            &[],
        )?;
        let model = IoModel::from_card(&card)?.context("missing UIO")?;
        assert_eq!(
            model
                .assignments
                .iter()
                .map(|field| field.property)
                .collect::<Vec<_>>(),
            [
                IoProperty::DriveHigh,
                IoProperty::DriveLow,
                IoProperty::DriveZ,
                IoProperty::InputLoad,
                IoProperty::OutputLoad,
                IoProperty::InputResistance,
                IoProperty::PulseWidthRejection,
                IoProperty::StorageTime,
                IoProperty::AnalogToDigital(InterfaceLevel::Four),
                IoProperty::DigitalToAnalog(InterfaceLevel::Two),
                IoProperty::SwitchingFall(InterfaceLevel::Two),
                IoProperty::SwitchingRise(InterfaceLevel::Two),
                IoProperty::PowerSupply,
            ]
        );
        assert_eq!(
            model.assignments[1].field.expression.as_deref(),
            Some("{sink}")
        );
        assert_eq!(model.assignments[6].field.expression.as_deref(), Some("0"));
        for bad in [
            ".model io UIO(ATOD5=x)",
            ".model io UIO(DRVH)",
            ".model io UIO(µ=1)",
        ] {
            assert!(
                parse_model_card(bad, 1, &[])
                    .and_then(|card| IoModel::from_card(&card))
                    .is_err(),
                "{bad}"
            );
        }
        Ok(())
    }
}
