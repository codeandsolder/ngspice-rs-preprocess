//! Source-level UIO declarations. Conversion subcircuits remain references;
//! their electrical behavior must be evaluated by the ordinary circuit engine.

use crate::{
    model_card::ModelCard,
    param_expression::{SpiceExpression, SpiceExpressionContext},
};
use anyhow::{Context, Result, bail, ensure};
use model_preprocess_command::WorkBudget;

/// A single interface level, including independent output transition offsets.
#[derive(Clone, Debug)]
pub struct Interface {
    pub analog_to_digital: String,
    pub digital_to_analog: String,
    pub switching_low_to_high: Option<SpiceExpression>,
    pub switching_high_to_low: Option<SpiceExpression>,
}

impl Default for Interface {
    fn default() -> Self {
        Self {
            analog_to_digital: "AtoDDefault".into(),
            digital_to_analog: "DtoADefault".into(),
            switching_low_to_high: None,
            switching_high_to_low: None,
        }
    }
}

/// Electrical load and drive specification before occurrence binding.
#[derive(Clone, Debug, Default)]
pub struct Model {
    pub interfaces: [Interface; 4],
    pub power_subcircuit: Option<String>,
    pub drive_high: Option<SpiceExpression>,
    pub drive_low: Option<SpiceExpression>,
    pub drive_z: Option<SpiceExpression>,
    pub input_capacitance: Option<SpiceExpression>,
    pub input_resistance: Option<SpiceExpression>,
    pub output_capacitance: Option<SpiceExpression>,
    pub pulse_rejection: Option<SpiceExpression>,
    pub minimum_storage: Option<SpiceExpression>,
}

fn reference(raw: &str) -> Result<String> {
    let value = raw
        .strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
        .unwrap_or(raw);
    ensure!(
        !value.is_empty() && !value.chars().any(|c| c.is_whitespace() || c == '"'),
        "invalid PSpice UIO subcircuit reference {raw}"
    );
    Ok(value.into())
}

impl Model {
    /// Parse a UIO model, preserving references and deferred scalar expressions.
    ///
    /// # Errors
    /// Invalid model family, field, expression, or subcircuit reference.
    pub fn parse(card: &ModelCard, budget: &WorkBudget) -> Result<Self> {
        ensure!(
            card.family.eq_ignore_ascii_case("uio"),
            "expected PSpice UIO model"
        );
        let mut model = Self::default();
        for param in &card.parameters {
            let name = param.name.to_ascii_lowercase();
            let raw = param
                .expression
                .as_deref()
                .context("missing PSpice UIO value")?;
            if name == "digpower" {
                model.power_subcircuit = Some(reference(raw)?);
                continue;
            }
            if let Some((prefix, level)) = name.split_at_checked(name.len().saturating_sub(1))
                && let Ok(level) = level.parse::<usize>()
                && (1..=4).contains(&level)
            {
                let interface = &mut model.interfaces[level - 1];
                match prefix {
                    "atod" => interface.analog_to_digital = reference(raw)?,
                    "dtoa" => interface.digital_to_analog = reference(raw)?,
                    "tswlh" => {
                        interface.switching_low_to_high =
                            Some(SpiceExpression::parse_with_budget(raw, budget)?);
                    }
                    "tswhl" => {
                        interface.switching_high_to_low =
                            Some(SpiceExpression::parse_with_budget(raw, budget)?);
                    }
                    _ => bail!("unsupported PSpice UIO field {name}"),
                }
                continue;
            }
            let target = match name.as_str() {
                "drvh" => &mut model.drive_high,
                "drvl" => &mut model.drive_low,
                "drvz" => &mut model.drive_z,
                "inld" => &mut model.input_capacitance,
                "inr" => &mut model.input_resistance,
                "outld" => &mut model.output_capacitance,
                "tpwrt" => &mut model.pulse_rejection,
                "tstoremn" => &mut model.minimum_storage,
                _ => bail!("unsupported PSpice UIO field {name}"),
            };
            *target = Some(SpiceExpression::parse_with_budget(raw, budget)?);
        }
        Ok(model)
    }

    /// Bind electrical characteristics in the declaring model's occurrence.
    /// The missing pulse-rejection threshold remains distinct from explicit zero:
    /// the device's effective propagation delay supplies that default.
    ///
    /// # Errors
    /// Invalid level, expression, or nonphysical electrical value.
    pub fn bind(
        &self,
        level: u8,
        context: &mut impl SpiceExpressionContext,
        budget: &WorkBudget,
    ) -> Result<BoundModel<'_>> {
        ensure!((1..=4).contains(&level), "invalid PSpice UIO level");
        let interface = &self.interfaces[usize::from(level - 1)];
        let mut evaluate = |expr: &Option<SpiceExpression>, default: f64| {
            let value = expr
                .as_ref()
                .map_or(Ok(default), |e| e.evaluate_with_budget(context, budget))?;
            ensure!(
                value.is_finite() && value >= 0.0,
                "invalid negative or nonfinite PSpice UIO value"
            );
            Ok(value)
        };
        let bound = BoundModel {
            analog_to_digital: &interface.analog_to_digital,
            digital_to_analog: &interface.digital_to_analog,
            power_subcircuit: self.power_subcircuit.as_deref().unwrap_or("DIGIFPWR"),
            drive_high: evaluate(&self.drive_high, 50.0)?,
            drive_low: evaluate(&self.drive_low, 50.0)?,
            drive_z: evaluate(&self.drive_z, 250e3)?,
            input_capacitance: evaluate(&self.input_capacitance, 0.0)?,
            input_resistance: evaluate(&self.input_resistance, 30e3)?,
            output_capacitance: evaluate(&self.output_capacitance, 0.0)?,
            pulse_rejection: self
                .pulse_rejection
                .as_ref()
                .map(|_| evaluate(&self.pulse_rejection, 0.0))
                .transpose()?,
            minimum_storage: evaluate(&self.minimum_storage, 1e-3)?,
            switching_low_to_high: evaluate(&interface.switching_low_to_high, 0.0)?,
            switching_high_to_low: evaluate(&interface.switching_high_to_low, 0.0)?,
        };
        Ok(bound)
    }
}

/// Bound electrical characteristics for one selected interface level.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BoundModel<'a> {
    pub analog_to_digital: &'a str,
    pub digital_to_analog: &'a str,
    pub power_subcircuit: &'a str,
    pub drive_high: f64,
    pub drive_low: f64,
    pub drive_z: f64,
    pub input_capacitance: f64,
    pub input_resistance: f64,
    pub output_capacitance: f64,
    pub pulse_rejection: Option<f64>,
    pub minimum_storage: f64,
    pub switching_low_to_high: f64,
    pub switching_high_to_low: f64,
}

impl BoundModel<'_> {
    /// Compensate only transitions to analog interface nodes, so the analog
    /// waveform crosses the receiver threshold at the intended event time.
    #[must_use]
    pub fn interface_delay(self, propagation: f64, rising: bool, mixed: bool) -> f64 {
        if !mixed {
            return propagation;
        }
        let switching = if rising {
            self.switching_low_to_high
        } else {
            self.switching_high_to_low
        };
        (propagation - switching).max(0.0)
    }
}
