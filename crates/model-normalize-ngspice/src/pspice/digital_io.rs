//! Digital loading, drive and mixed-domain interface declarations.

use super::{
    digital_source::{DigitalDeclaration, Location},
    scope::charge_scope,
};
use crate::model_card::{ModelCard, ModelParameter};
use anyhow::{Context, Result, ensure};
use model_preprocess_command::{Resource, WorkBudget};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// One of the four source-selectable mixed-domain interface levels.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
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
    /// Parse the global numeric interface selector.
    ///
    /// # Errors
    /// Invalid or nonintegral level.
    pub fn from_global(global: Option<f64>) -> Result<Self> {
        ensure!(
            global != Some(0.),
            "invalid global PSpice interface level zero"
        );
        Self::One.override_with(global)
    }

    /// Select the interface level, using the validated global option for
    /// an absent or zero instance selector.
    ///
    /// # Errors
    /// Invalid or nonintegral level.
    pub fn override_with(self, instance: Option<f64>) -> Result<Self> {
        Ok(match instance {
            None | Some(0.) => self,
            Some(1.) => Self::One,
            Some(2.) => Self::Two,
            Some(3.) => Self::Three,
            Some(4.) => Self::Four,
            Some(value) => anyhow::bail!("invalid PSpice interface level {value}"),
        })
    }

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
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
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

/// A pulse rejection threshold can follow the eventual loaded propagation
/// delay, or be an independent explicit threshold in seconds.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PulseRejection {
    /// Resolve from the applicable propagation path at execution binding.
    PropagationDelay,
    /// Explicit duration, including zero.
    Explicit {
        /// Seconds.
        seconds: f64,
    },
}

/// A selected interface reference, not an already elaborated subcircuit.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct InterfaceReference {
    /// Selected subcircuit name.
    pub name: String,
    /// Declaration scope for an explicit name; default names use library lookup.
    pub declaration: Option<Location>,
}

/// Electrical model quantities and selected interface references. No electrical
/// topology, load-dependent delay, drive category or charge-storage dynamics
/// are inferred here.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BoundIo {
    /// High-state resistance in ohms.
    pub drive_high: f64,
    /// Low-state resistance in ohms.
    pub drive_low: f64,
    /// High-impedance leakage resistance in ohms.
    pub drive_z: f64,
    /// Input leakage resistance in ohms.
    pub input_resistance: f64,
    /// Input capacitance in farads.
    pub input_load: f64,
    /// Output capacitance in farads.
    pub output_load: f64,
    /// Minimum charge-storage duration in seconds.
    pub storage_time: f64,
    /// Pulse rejection policy.
    pub pulse_rejection: PulseRejection,
    /// Selected mixed-domain interface level.
    pub level: InterfaceLevel,
    /// Selected analog-to-digital subcircuit.
    pub analog_to_digital: InterfaceReference,
    /// Selected digital-to-analog subcircuit.
    pub digital_to_analog: InterfaceReference,
    /// Power supply subcircuit reference.
    pub power_supply: InterfaceReference,
    /// Low-to-high output threshold compensation in seconds.
    pub switching_rise: f64,
    /// High-to-low output threshold compensation in seconds.
    pub switching_fall: f64,
}

type Effective<'a> = BTreeMap<IoProperty, (&'a Location, &'a ModelParameter)>;

fn numeric(
    fields: &Effective<'_>,
    property: IoProperty,
    default: f64,
    resolve: &mut impl FnMut(&Location, &ModelParameter) -> Result<f64>,
    budget: &WorkBudget,
) -> Result<f64> {
    let Some(&(location, field)) = fields.get(&property) else {
        return Ok(default);
    };
    budget.charge(Resource::EvaluationSteps, 1)?;
    let value = resolve(location, field)
        .with_context(|| format!("PSpice I/O field {} at {location:?}", field.name))?;
    ensure!(
        value.is_finite() && value >= 0.,
        "invalid PSpice I/O field {}",
        field.name
    );
    Ok(value)
}

fn reference(
    fields: &Effective<'_>,
    property: IoProperty,
    default: &str,
    resolve: &mut impl FnMut(&Location, &ModelParameter) -> Result<String>,
    budget: &WorkBudget,
) -> Result<InterfaceReference> {
    if let Some(&(location, field)) = fields.get(&property) {
        budget.charge(Resource::EvaluationSteps, 1)?;
        let name = resolve(location, field)
            .with_context(|| format!("PSpice interface field {} at {location:?}", field.name))?;
        ensure!(!name.trim().is_empty(), "empty PSpice interface name");
        budget.charge(Resource::EmittedBytes, name.len())?;
        charge_scope(&location.scope, budget)?;
        if let Some(origin) = &location.linked {
            budget.charge(Resource::EmittedBytes, origin.first.path.as_os_str().len())?;
        }
        Ok(InterfaceReference {
            name,
            declaration: Some(location.clone()),
        })
    } else {
        budget.charge(Resource::EmittedBytes, default.len())?;
        Ok(InterfaceReference {
            name: default.to_owned(),
            declaration: None,
        })
    }
}

impl DigitalDeclaration {
    /// Resolve an occurrence's effective I/O fields in their declaration scopes.
    ///
    /// Defaults follow the `PSpice` Reference Guide 9.0, pp. 322–323. Interface
    /// references are not instantiated; omitted pulse rejection remains dependent
    /// on the actual propagation path. Unselected interface fields are not
    /// evaluated.
    ///
    /// # Errors
    /// Unresolvable or invalid values, missing model, or exceeded work budget.
    pub(super) fn bind_io(
        &self,
        level: InterfaceLevel,
        mut resolve_number: impl FnMut(&Location, &ModelParameter) -> Result<f64>,
        mut resolve_name: impl FnMut(&Location, &ModelParameter) -> Result<String>,
        budget: &WorkBudget,
    ) -> Result<BoundIo> {
        use IoProperty as P;
        ensure!(!self.io.is_empty(), "missing effective PSpice I/O model");
        let mut fields = Effective::new();
        for layer in &self.io {
            for assignment in &layer.declaration.assignments {
                budget.charge(Resource::EvaluationSteps, 1)?;
                fields.insert(assignment.property, (&layer.location, &assignment.field));
            }
        }
        let mut number =
            |property, default| numeric(&fields, property, default, &mut resolve_number, budget);
        let mut name =
            |property, default| reference(&fields, property, default, &mut resolve_name, budget);
        let pulse_rejection = if fields.contains_key(&P::PulseWidthRejection) {
            PulseRejection::Explicit {
                seconds: number(P::PulseWidthRejection, 0.)?,
            }
        } else {
            PulseRejection::PropagationDelay
        };
        budget.charge(Resource::GeneratedNodes, 1)?;
        Ok(BoundIo {
            drive_high: number(P::DriveHigh, 50.)?,
            drive_low: number(P::DriveLow, 50.)?,
            drive_z: number(P::DriveZ, 250_000.)?,
            input_resistance: number(P::InputResistance, 30_000.)?,
            input_load: number(P::InputLoad, 0.)?,
            output_load: number(P::OutputLoad, 0.)?,
            storage_time: number(P::StorageTime, 0.001)?,
            pulse_rejection,
            level,
            analog_to_digital: name(P::AnalogToDigital(level), "AtoDDefault")?,
            digital_to_analog: name(P::DigitalToAnalog(level), "DtoADefault")?,
            power_supply: name(P::PowerSupply, "DIGIFPWR")?,
            switching_rise: number(P::SwitchingRise(level), 0.)?,
            switching_fall: number(P::SwitchingFall(level), 0.)?,
        })
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
