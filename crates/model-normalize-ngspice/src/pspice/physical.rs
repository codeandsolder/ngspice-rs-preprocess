//! Model-declaration selection for `PSpice` devices. The source level
//! and target physical variant are distinct: `PSpice` levels 6 and 7 select
//! Berkeley BSIM3v3.2.4, not the native level-6 or level-7 implementations.
use super::{parameter, set_parameter};
use crate::model_card::{ModelCard, parse_spice_constant};
use anyhow::{Context, Result, bail, ensure};

/// Qualified physical model implementation selected by the `PSpice` frontend.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PhysicalKind {
    Diode,
    Mos1,
    Mos2,
    Mos3,
    Bsim3V324,
    Bsim4V450,
    Resistor,
    Capacitor,
    Inductor,
    NonlinearCapacitor,
    NonlinearInductor,
    ThresholdVoltageSwitch,
    ThresholdCurrentSwitch,
    ContinuousVoltageSwitch,
    ContinuousCurrentSwitch,
}

impl PhysicalKind {
    /// Target version fixed by the source physical-model selector.
    #[must_use = "the version identifies the selected implementation"]
    pub const fn version(self) -> Option<&'static str> {
        match self {
            Self::Bsim3V324 => Some("3.2.4"),
            Self::Bsim4V450 => Some("4.5.0"),
            _ => None,
        }
    }

    /// Whether the physical model uses a nominal temperature coordinate.
    #[must_use = "nominal temperature belongs to the selected physical law"]
    pub const fn has_nominal_temperature(self) -> bool {
        !matches!(
            self,
            Self::ThresholdVoltageSwitch
                | Self::ThresholdCurrentSwitch
                | Self::ContinuousVoltageSwitch
                | Self::ContinuousCurrentSwitch
        )
    }

    fn apply(self, card: &mut ModelCard) -> Result<()> {
        let level = match self {
            Self::Bsim3V324 => "8",
            Self::Bsim4V450 => "14",
            _ => return Ok(()),
        };
        set_parameter(card, "level", level)?;
        set_parameter(
            card,
            "version",
            self.version().context("missing selected MOS version")?,
        )
    }
}

/// Source declaration and effective model selection, before native spelling
/// replaces its physical selector.
///
/// For AKO the selection uses the inherited
/// effective declaration, while the declared card remains the source card.
#[derive(Debug)]
pub struct PhysicalProjection {
    /// Original lexical declaration, without inherited parameters.
    pub declaration: ModelCard,
    /// Enclosing lexical subcircuit path.
    pub scope: Vec<String>,
    /// Effective source level after AKO inheritance, zero when omitted.
    pub source_level: u16,
    /// Actual selected physical implementation.
    pub kind: PhysicalKind,
}

pub(super) fn project_junction(card: &mut ModelCard) -> Result<Option<(PhysicalKind, u16)>> {
    let family = card.family.to_ascii_lowercase();
    if !matches!(family.as_str(), "d" | "nmos" | "pmos") {
        return Ok(None);
    }
    let level = parameter(card, "level")
        .map(|value| {
            parse_spice_constant(&value).context("PSpice physical model level must be constant")
        })
        .transpose()?;
    // Preserve native passthrough for variants not covered by the typed
    // mixed-dialect contract. They cannot be selected by an occurrence binder.
    let numeric = level.unwrap_or(0.);
    if !numeric.is_finite()
        || !(0. ..=f64::from(u16::MAX)).contains(&numeric)
        || numeric.fract() != 0.
    {
        bail!("invalid PSpice physical model level {numeric}");
    }
    let source_level: u16 = format!("{numeric:.0}").parse()?;
    let kind = if family == "d" {
        match source_level {
            0 | 1 => PhysicalKind::Diode,
            _ => return Ok(None),
        }
    } else {
        match source_level {
            0 | 1 => PhysicalKind::Mos1,
            2 => PhysicalKind::Mos2,
            3 => PhysicalKind::Mos3,
            5 => bail!("PSpice MOS level 5 requires an EKV physical kernel"),
            6 | 7 => PhysicalKind::Bsim3V324,
            8 => PhysicalKind::Bsim4V450,
            _ => return Ok(None),
        }
    };
    if let Some(version) = parameter(card, "version")
        && let Some(expected) = kind.version()
    {
        let version = version.trim_matches(['\'', '"']);
        let accepted = match kind {
            PhysicalKind::Bsim3V324 => matches!(version, "3.2.4" | "3.24"),
            PhysicalKind::Bsim4V450 => matches!(version, "4.5.0" | "4.5" | "4.50"),
            _ => false,
        };
        ensure!(
            accepted,
            "PSpice model {} selects version {version}, not qualified version {expected}",
            card.name
        );
    }
    kind.apply(card)?;
    Ok(Some((kind, source_level)))
}
