//! `PSpice` timing model parameters and corner selection.
//!
//! The corner triplets are resolved per timing path; neither a missing
//! parameter nor an explicitly specified zero is an arbitrary tiny delay.

use crate::{
    model_card::ModelCard,
    param_expression::{SpiceExpression, SpiceExpressionContext},
};
use anyhow::{Context, Result, bail, ensure};
use model_preprocess_command::WorkBudget;
use std::collections::BTreeMap;

/// Timing model family required by a source primitive.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Family {
    Gate,
    Tristate,
    Edge,
    Latch,
    Delay,
}

impl Family {
    fn parse(name: &str) -> Result<Self> {
        match name.to_ascii_lowercase().as_str() {
            "ugate" => Ok(Self::Gate),
            "utgate" => Ok(Self::Tristate),
            "ueff" => Ok(Self::Edge),
            "ugff" => Ok(Self::Latch),
            "udly" => Ok(Self::Delay),
            _ => bail!("not a PSpice digital timing model: {name}"),
        }
    }

    const fn paths(self) -> &'static [&'static str] {
        match self {
            Self::Gate => &["tplh", "tphl"],
            Self::Tristate => &["tplh", "tphl", "tplz", "tphz", "tpzl", "tpzh"],
            Self::Edge => &["tpclkqlh", "tpclkqhl", "tppcqlh", "tppcqhl"],
            Self::Latch => &["tpdqlh", "tpdqhl", "tpgqlh", "tpgqhl", "tppcqlh", "tppcqhl"],
            Self::Delay => &["dly"],
        }
    }

    const fn constraints(self) -> &'static [&'static str] {
        match self {
            Self::Edge => &[
                "thdclk",
                "tsudclk",
                "tsupcclkh",
                "twpcl",
                "twclkl",
                "twclkh",
                "tsuceclk",
                "thceclk",
            ],
            Self::Latch => &["thdg", "tsudg", "tsupcgh", "twpcl", "twgh"],
            _ => &[],
        }
    }
}

/// Simulation corner; worst case retains a two-sided event window.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Corner {
    Minimum,
    #[default]
    Typical,
    Maximum,
    WorstCase,
}

impl Corner {
    /// Resolve an instance override (zero means inherit circuit policy).
    ///
    /// # Errors
    /// An invalid corner selector.
    pub fn from_override(value: u8) -> Result<Option<Self>> {
        match value {
            0 => Ok(None),
            1 => Ok(Some(Self::Minimum)),
            2 => Ok(Some(Self::Typical)),
            3 => Ok(Some(Self::Maximum)),
            4 => Ok(Some(Self::WorstCase)),
            _ => bail!("invalid PSpice timing corner {value}"),
        }
    }
}

/// Circuit-wide propagation scaling policy.
#[derive(Clone, Copy, Debug)]
pub struct Scale {
    pub minimum: f64,
    pub maximum: f64,
}

impl Default for Scale {
    fn default() -> Self {
        Self {
            minimum: 0.4,
            maximum: 1.6,
        }
    }
}

impl Scale {
    fn validate(self) -> Result<()> {
        ensure!(
            (0.0..=1.0).contains(&self.minimum) && self.maximum.is_finite() && self.maximum > 1.0,
            "invalid PSpice propagation scaling"
        );
        Ok(())
    }
}

/// Circuit policy inherited by a source occurrence unless overridden.
#[derive(Clone, Copy, Debug)]
pub struct Policy {
    pub corner: Corner,
    pub io_level: u8,
    pub scale: Scale,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            corner: Corner::Typical,
            io_level: 1,
            scale: Scale::default(),
        }
    }
}

impl Policy {
    pub(super) fn validate(self) -> Result<()> {
        self.scale.validate()?;
        ensure!(
            (1..=4).contains(&self.io_level),
            "invalid PSpice circuit I/O level"
        );
        Ok(())
    }
}

/// Unevaluated source triplet; each expression is resolved in its owning
/// model occurrence rather than in the global parameter context.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SourceTriplet {
    pub minimum: Option<SpiceExpression>,
    pub typical: Option<SpiceExpression>,
    pub maximum: Option<SpiceExpression>,
}

impl SourceTriplet {
    fn evaluate(
        &self,
        context: &mut impl SpiceExpressionContext,
        budget: &WorkBudget,
    ) -> Result<Triplet> {
        let mut value = |expression: &Option<SpiceExpression>| {
            expression
                .as_ref()
                .map(|expr| expr.evaluate_with_budget(context, budget))
                .transpose()
        };
        Ok(Triplet {
            minimum: value(&self.minimum)?,
            typical: value(&self.typical)?,
            maximum: value(&self.maximum)?,
        })
    }
}

/// Explicitly supplied corners, before applying source defaults.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Triplet {
    pub minimum: Option<f64>,
    pub typical: Option<f64>,
    pub maximum: Option<f64>,
}

/// Fully resolved numerical corners in seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Resolved {
    pub minimum: f64,
    pub typical: f64,
    pub maximum: f64,
}

/// Event or constraint window in seconds. A single-corner selection is a
/// degenerate interval; a worst-case selection retains both endpoints.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Window {
    pub earliest: f64,
    pub latest: f64,
}

impl Resolved {
    /// Select a corner without collapsing worst-case timing.
    #[must_use]
    pub const fn select(self, corner: Corner) -> Window {
        let value = match corner {
            Corner::Minimum => self.minimum,
            Corner::Typical => self.typical,
            Corner::Maximum => self.maximum,
            Corner::WorstCase => {
                return Window {
                    earliest: self.minimum,
                    latest: self.maximum,
                };
            }
        };
        Window {
            earliest: value,
            latest: value,
        }
    }

    fn validated(self) -> Result<Self> {
        ensure!(
            [self.minimum, self.typical, self.maximum]
                .iter()
                .all(|v| v.is_finite()),
            "nonfinite resolved PSpice timing"
        );
        ensure!(
            self.minimum <= self.typical && self.typical <= self.maximum,
            "unordered PSpice timing corners"
        );
        Ok(self)
    }
}

impl Triplet {
    /// Resolve propagation delays with the documented scaling defaults.
    ///
    /// # Errors
    /// Invalid scale, negative or inconsistent delay or overflow.
    pub fn propagation(self, scale: Scale) -> Result<Resolved> {
        scale.validate()?;
        ensure!(
            [self.minimum, self.typical, self.maximum]
                .into_iter()
                .flatten()
                .all(|v| v.is_finite() && v >= 0.0),
            "invalid PSpice propagation delay"
        );
        let typical = if let Some(value) = self.typical {
            value
        } else {
            match (self.minimum, self.maximum) {
                (Some(min), Some(max)) => min.midpoint(max),
                (Some(min), None) => {
                    ensure!(
                        scale.minimum > 0.0,
                        "cannot infer typical delay from zero minimum scale"
                    );
                    min / scale.minimum
                }
                (None, Some(max)) => max / scale.maximum,
                (None, None) => 0.0,
            }
        };
        Resolved {
            minimum: self.minimum.unwrap_or(typical * scale.minimum),
            typical,
            maximum: self.maximum.unwrap_or(typical * scale.maximum),
        }
        .validated()
    }

    /// Resolve timing checks separately from propagation delays.
    ///
    /// # Errors
    /// Nonfinite or unordered constraint corners.
    pub fn constraint(self) -> Result<Resolved> {
        let minimum = self.minimum.unwrap_or(0.0);
        let maximum = self.maximum.or(self.typical).unwrap_or(minimum);
        Resolved {
            minimum,
            typical: self.typical.unwrap_or_else(|| minimum.midpoint(maximum)),
            maximum,
        }
        .validated()
    }
}

/// A fully parsed timing declaration. Directional propagation paths and
/// timing checks are kept separately, including explicitly missing corners.
#[derive(Debug, PartialEq)]
pub struct Model {
    pub family: Family,
    pub propagation: BTreeMap<&'static str, SourceTriplet>,
    pub constraints: BTreeMap<&'static str, SourceTriplet>,
}

impl Model {
    /// Parse one scoped timing model, rejecting unknown fields rather than
    /// silently deleting timing checks.
    ///
    /// # Errors
    /// Wrong model family, unsupported field, or invalid scalar.
    pub fn parse(card: &ModelCard, budget: &WorkBudget) -> Result<Self> {
        let family = Family::parse(&card.family)?;
        let mut model = Self {
            family,
            propagation: family
                .paths()
                .iter()
                .map(|&name| (name, SourceTriplet::default()))
                .collect(),
            constraints: family
                .constraints()
                .iter()
                .map(|&name| (name, SourceTriplet::default()))
                .collect(),
        };
        for param in &card.parameters {
            let name = param.name.to_ascii_lowercase();
            let split = name
                .len()
                .checked_sub(2)
                .context("invalid PSpice timing field")?;
            let (path, suffix) = name
                .split_at_checked(split)
                .context("invalid PSpice timing field")?;
            let triplet = model
                .propagation
                .get_mut(path)
                .or_else(|| model.constraints.get_mut(path))
                .with_context(|| format!("unsupported PSpice timing field {name}"))?;
            let target = match suffix {
                "mn" => &mut triplet.minimum,
                "ty" => &mut triplet.typical,
                "mx" => &mut triplet.maximum,
                _ => bail!("invalid PSpice timing corner suffix {name}"),
            };
            let value = SpiceExpression::parse_with_budget(
                param
                    .expression
                    .as_deref()
                    .context("missing PSpice timing value")?,
                budget,
            )?;
            *target = Some(value);
        }
        Ok(model)
    }

    /// Evaluate the timing declaration in its own lexical occurrence.
    /// Keep all three corners, so worst-case event propagation and timing
    /// checks can consume the appropriate endpoints independently.
    ///
    /// # Errors
    /// Invalid timing expressions, scale or corners.
    pub fn bind(
        &self,
        scale: Scale,
        context: &mut impl SpiceExpressionContext,
        budget: &WorkBudget,
    ) -> Result<BoundModel> {
        let propagation = self
            .propagation
            .iter()
            .map(|(&path, values)| {
                Ok((path, values.evaluate(context, budget)?.propagation(scale)?))
            })
            .collect::<Result<_>>()?;
        let constraints = self
            .constraints
            .iter()
            .map(|(&path, values)| Ok((path, values.evaluate(context, budget)?.constraint()?)))
            .collect::<Result<_>>()?;
        Ok(BoundModel {
            family: self.family,
            propagation,
            constraints,
        })
    }
}

/// Resolved model timing, with independent path and constraint corners.
#[derive(Debug, PartialEq)]
pub struct BoundModel {
    pub family: Family,
    pub propagation: BTreeMap<&'static str, Resolved>,
    pub constraints: BTreeMap<&'static str, Resolved>,
}
