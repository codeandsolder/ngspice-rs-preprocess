//! Source switch classification and short-transition declaration binding.
//!
//! The `PSpice` Reference Guide 16.6, pp. 309–311 and 325–328, distinguishes
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
                !needs_delayed_kernel(card.parameters.iter()),
                "PSpice nonzero threshold-switch delay requires a timed switch kernel"
            );
        }
        Ok(law)
    }
}

/// A symbolic delay must be bound per candidate; it cannot be assumed zero.
fn needs_delayed_kernel<'a>(fields: impl DoubleEndedIterator<Item = &'a ModelParameter>) -> bool {
    fields
        .rev()
        .find(|field| field.name.eq_ignore_ascii_case("td"))
        .is_some_and(|field| {
            field.expression.as_deref().is_none_or(|value| {
                !matches!(
                    parse_spice_constant(temperature::expression(value)),
                    Ok(0.0)
                )
            })
        })
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

/// A short-transition switch declaration, including zero delay, and all its lexical model layers.
/// No pending event cancellation policy is assumed from the source spelling.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ThresholdSwitchDeclaration {
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

/// Resolved physical parameters for the instantaneous transition law.
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

/// State-specific thresholds in the original differential-control units.
/// The comparison operators are part of the source contract.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct ThresholdBoundaries {
    /// An off switch requests turn-on when the control reaches this value.
    pub turn_on_at_or_above: f64,
    /// An on switch requests turn-off only below this value.
    pub turn_off_below: f64,
}

impl BoundThreshold {
    /// Compute the two documented transition boundaries without changing
    /// the order of the source addition/subtraction or clamping hysteresis.
    ///
    /// # Errors
    /// Nonfinite or overflowing threshold coordinates.
    pub fn boundaries(self) -> Result<ThresholdBoundaries> {
        let boundaries = ThresholdBoundaries {
            turn_on_at_or_above: self.threshold + self.hysteresis,
            turn_off_below: self.threshold - self.hysteresis,
        };
        ensure!(
            boundaries.turn_on_at_or_above.is_finite() && boundaries.turn_off_below.is_finite(),
            "nonfinite PSpice transition boundary"
        );
        Ok(boundaries)
    }
}

impl ThresholdSwitchDeclaration {
    /// Whether a nonzero or candidate-dependent propagation delay requires
    /// an explicit timed kernel. Zero-delay declarations still carry the
    /// source-specific transition boundaries.
    #[must_use = "a nonzero or unresolved delay cannot be lowered as a static switch"]
    pub fn requires_delayed_kernel(&self) -> bool {
        needs_delayed_kernel(
            self.models
                .iter()
                .flat_map(|model| model.declaration.parameters.iter()),
        )
    }

    /// Resolve effective parameters in the scopes of their declarations.
    ///
    /// The 16.6 reference specifies inclusive turn-on at threshold plus
    /// hysteresis and strict turn-off below threshold minus hysteresis.
    /// This API does not decide when a scheduled transition is cancelled.
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
        let _ = bound.boundaries()?;
        Ok(bound)
    }
}

impl Capture<'_> {
    pub(super) fn threshold_switch(
        &self,
        text: &str,
        line: usize,
        last_line: usize,
    ) -> Result<Option<ThresholdSwitchDeclaration>> {
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
        let Some(law) = SwitchLaw::classify(family, fields_iter)? else {
            return Ok(None);
        };
        if !matches!(
            law,
            SwitchLaw::ThresholdVoltage | SwitchLaw::ThresholdCurrent
        ) {
            return Ok(None);
        }
        ensure!(
            chain.iter().all(|model| !model.conditional),
            "conditional PSpice threshold switch model requires topology binding"
        );
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
            _ => anyhow::bail!("invalid PSpice short-transition switch instance"),
        };
        ensure!(
            tail.trim().is_empty(),
            "unsupported short-transition switch instance assignment"
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
                    location: self.location(
                        model.card.source_line,
                        model.last_line,
                        model.scope,
                    )?,
                    declaration: model.card.clone(),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Some(ThresholdSwitchDeclaration {
            name: (*name).to_owned(),
            location: self.location(line, last_line, scope)?,
            terminals: [fields[1].to_owned(), fields[2].to_owned()],
            control,
            models,
        }))
    }
}
