//! Source switch classification and delayed threshold declaration binding.
//!
//! The `PSpice` Reference Guide 10.5, pp. 295–299 and 311–316, distinguishes
//! continuous log-resistance interpolation from short-transition switches.
//! TD shifts the transition; it is not a finite transition duration.

use super::{
    digital_source::{Capture, Located, Location},
    scope::charge_card,
    temperature,
};
use crate::model_card::{ModelCard, ModelParameter, parse_spice_constant, positional_prefix};
use anyhow::{Context, Result, ensure};
use model_preprocess_command::{Resource, WorkBudget};
use serde::Serialize;

#[derive(Clone, Copy)]
pub(super) enum SwitchLaw {
    ThresholdVoltage,
    ThresholdCurrent,
    ContinuousVoltage,
    ContinuousCurrent,
}

impl SwitchLaw {
    fn classify<'a>(
        family: &str,
        fields: impl Iterator<Item = &'a ModelParameter> + Clone,
    ) -> Result<Option<Self>> {
        let voltage = family.eq_ignore_ascii_case("vswitch");
        if !voltage && !family.eq_ignore_ascii_case("iswitch") {
            return Ok(None);
        }
        let (threshold, hysteresis, on, off) = if voltage {
            ("vt", "vh", "von", "voff")
        } else {
            ("it", "ih", "ion", "ioff")
        };
        let has = |names: &[&str]| {
            fields.clone().any(|field| {
                names
                    .iter()
                    .any(|name| field.name.eq_ignore_ascii_case(name))
            })
        };
        let discrete = has(&[threshold, hysteresis, "td"]);
        let continuous = has(&[on, off]);
        ensure!(
            !(discrete && continuous),
            "PSpice switch cannot mix threshold and continuous laws"
        );
        let allowed: &[&str] = if discrete {
            &[threshold, hysteresis, "ron", "roff", "td"]
        } else {
            &[on, off, "ron", "roff"]
        };
        for field in fields {
            ensure!(
                allowed
                    .iter()
                    .any(|name| field.name.eq_ignore_ascii_case(name))
                    && field.expression.is_some(),
                "unsupported PSpice switch parameter {}",
                field.name
            );
        }
        Ok(Some(match (voltage, discrete) {
            (true, true) => Self::ThresholdVoltage,
            (false, true) => Self::ThresholdCurrent,
            (true, false) => Self::ContinuousVoltage,
            (false, false) => Self::ContinuousCurrent,
        }))
    }

    pub(super) fn from_static_model(card: &ModelCard) -> Result<Option<Self>> {
        let law = Self::classify(&card.family, card.parameters.iter())?;
        if law.is_some() {
            ensure!(
                card.parameters
                    .iter()
                    .rev()
                    .find(|field| field.name.eq_ignore_ascii_case("td"))
                    .is_none_or(|field| field.expression.as_deref().is_some_and(|value| {
                        matches!(
                            parse_spice_constant(temperature::expression(value)),
                            Ok(0.0)
                        )
                    })),
                "PSpice nonzero threshold-switch delay requires a timed switch kernel"
            );
        }
        Ok(law)
    }
}

/// A differential voltage control or signed controlling branch current.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ControlConnection {
    /// Voltage positive minus negative, with the source GMIN control shunt.
    Voltage {
        /// Positive control node.
        positive: String,
        /// Negative control node.
        negative: String,
    },
    /// Signed current through the named voltage-source branch.
    Current {
        /// Controlling branch name.
        branch: String,
    },
}

/// A short-transition switch occurrence and all its lexical model layers.
/// No pending event cancellation policy is assumed from the source spelling.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DelayedSwitchDeclaration {
    /// Original source device identifier.
    pub name: String,
    /// Instance declaration address.
    pub location: Location,
    /// Conducting terminal pair, positive then negative.
    pub terminals: [String; 2],
    /// Control connection, preserving orientation.
    pub control: ControlConnection,
    /// Base-to-derived model declarations with individual lexical scopes.
    pub models: Vec<Located<ModelCard>>,
}

/// Resolved physical parameters for the instantaneous delayed law.
/// The native event kernel owns accepted state, deadlines and cancellation.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct BoundThreshold {
    /// On resistance in ohms.
    pub ron: f64,
    /// Off resistance in ohms.
    pub roff: f64,
    /// Signed threshold in volts or amperes according to control connection.
    pub threshold: f64,
    /// Hysteresis in the same control units.
    pub hysteresis: f64,
    /// Transition delay in seconds (not slew time).
    pub delay: f64,
}

impl DelayedSwitchDeclaration {
    /// Resolve effective parameters in the scopes of their declarations.
    ///
    /// The reference guide specifies strict comparisons at threshold plus
    /// or minus hysteresis. Equality retains the existing state. This API
    /// does not decide when a scheduled transition is cancelled.
    ///
    /// # Errors
    /// Missing model, invalid/nonfinite parameters or exhausted budget.
    pub fn bind(
        &self,
        mut resolve: impl FnMut(&Location, &ModelParameter) -> Result<f64>,
        budget: &WorkBudget,
    ) -> Result<BoundThreshold> {
        ensure!(
            !self.models.is_empty(),
            "missing effective PSpice switch model"
        );
        let fields = self.models.iter().rev().flat_map(|model| {
            model
                .declaration
                .parameters
                .iter()
                .rev()
                .map(|field| (&model.location, field))
        });
        let mut number = |name: &str, default| {
            for (location, field) in fields.clone() {
                budget.charge(Resource::EvaluationSteps, 1)?;
                if field.name.eq_ignore_ascii_case(name) {
                    let value = resolve(location, field).with_context(|| {
                        format!("PSpice switch field {} at {location:?}", field.name)
                    })?;
                    ensure!(
                        value.is_finite(),
                        "nonfinite PSpice switch field {}",
                        field.name
                    );
                    return Ok(value);
                }
            }
            Ok(default)
        };
        let (threshold, hysteresis) = match self.control {
            ControlConnection::Voltage { .. } => ("vt", "vh"),
            ControlConnection::Current { .. } => ("it", "ih"),
        };
        let bound = BoundThreshold {
            ron: number("ron", 1.)?,
            roff: number("roff", 1e12)?,
            threshold: number(threshold, 0.)?,
            hysteresis: number(hysteresis, 0.)?,
            delay: number("td", 0.)?,
        };
        ensure!(
            bound.ron > 0. && bound.roff > 0.,
            "nonpositive PSpice switch resistance"
        );
        ensure!(bound.delay >= 0., "negative PSpice switch delay");
        Ok(bound)
    }
}

impl Capture<'_> {
    pub(super) fn delayed_switch(
        &self,
        text: &str,
        line: usize,
    ) -> Result<Option<DelayedSwitchDeclaration>> {
        let (positions, tail) = positional_prefix(text);
        let fields = positions
            .iter()
            .map(|(field, _)| *field)
            .collect::<Vec<_>>();
        let name = fields.first().context("missing PSpice switch name")?;
        let voltage_instance = name.starts_with(['s', 'S']);
        let index = if voltage_instance { 5 } else { 4 };
        let Some(model_name) = fields.get(index) else {
            return Ok(None);
        };
        let scope = self.scopes.scope(line)?;
        let Some(model_line) = self
            .scopes
            .find_model(scope, &model_name.to_ascii_lowercase())?
        else {
            return Ok(None);
        };
        if !self.scopes.is_pspice_model(model_line) {
            return Ok(None);
        }
        let chain = self.scopes.model_chain(model_line)?;
        let root = chain.first().context("empty PSpice switch model chain")?;
        let family = root.card.family.as_str();
        if !family.eq_ignore_ascii_case("vswitch") && !family.eq_ignore_ascii_case("iswitch") {
            return Ok(None);
        }
        ensure!(
            chain
                .iter()
                .all(|model| model.card.family.eq_ignore_ascii_case(family)),
            "PSpice switch inheritance changes model family"
        );
        let fields_iter = chain.iter().flat_map(|model| model.card.parameters.iter());
        let Some(law) = SwitchLaw::classify(family, fields_iter.clone())? else {
            return Ok(None);
        };
        let delayed = fields_iter
            .rev()
            .find(|field| field.name.eq_ignore_ascii_case("td"))
            .is_some_and(|field| {
                field.expression.as_deref().is_none_or(|value| {
                    !matches!(
                        parse_spice_constant(temperature::expression(value)),
                        Ok(0.0)
                    )
                })
            });
        if !delayed {
            return Ok(None);
        }
        let control = match (law, voltage_instance, fields.as_slice()) {
            (SwitchLaw::ThresholdVoltage, true, [_, _, _, positive, negative, _]) => {
                ControlConnection::Voltage {
                    positive: (*positive).to_owned(),
                    negative: (*negative).to_owned(),
                }
            }
            (SwitchLaw::ThresholdCurrent, false, [_, _, _, branch, _]) => {
                ControlConnection::Current {
                    branch: (*branch).to_owned(),
                }
            }
            _ => anyhow::bail!("invalid PSpice delayed threshold switch instance"),
        };
        ensure!(
            tail.trim().is_empty(),
            "unsupported delayed switch instance assignment"
        );
        self.scopes.budget().charge(Resource::GeneratedNodes, 1)?;
        self.scopes
            .budget()
            .charge(Resource::EmittedBytes, text.len())?;
        let models = chain
            .into_iter()
            .map(|model| {
                charge_card(&model.card, self.scopes.budget())?;
                Ok(Located {
                    location: self.location(model.card.source_line, model.scope)?,
                    declaration: model.card.clone(),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Some(DelayedSwitchDeclaration {
            name: (*name).to_owned(),
            location: self.location(line, scope)?,
            terminals: [fields[1].to_owned(), fields[2].to_owned()],
            control,
            models,
        }))
    }
}
