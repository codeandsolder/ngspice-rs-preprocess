//! Compile explicitly source-selected EKV cards after lexical resolution.
//! Native NMOS/PMOS LEVEL=5 remains BSIM2.

use anyhow::{Context, Result};
use model_kernel_ekv::{
    Polarity,
    pspice::{CircuitDefaults, NMOS_FAMILY, PMOS_FAMILY, PspiceModel},
};

use crate::model_card::{ModelCard, parse_spice_constant};

/// Resolve the physical card using an explicit scalar-expression resolver.
///
/// # Errors
/// Unknown or invalid physical parameters, unsupported flags, or unresolved
/// expressions. Unrelated model families return `None`.
pub fn ekv_resolved(
    card: &ModelCard,
    defaults: CircuitDefaults,
    mut resolve_number: impl FnMut(&str) -> Result<f64>,
) -> Result<Option<PspiceModel>> {
    let polarity = if card.family.eq_ignore_ascii_case(NMOS_FAMILY) {
        Polarity::N
    } else if card.family.eq_ignore_ascii_case(PMOS_FAMILY) {
        Polarity::P
    } else {
        return Ok(None);
    };
    let parameters = card
        .parameters
        .iter()
        .map(|parameter| {
            let expression = parameter.expression.as_deref().with_context(|| {
                format!("EKV parameter {} requires a scalar value", parameter.name)
            })?;
            Ok((parameter.name.as_str(), resolve_number(expression)?))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Some(PspiceModel::compile(polarity, parameters, defaults)?))
}

/// Compile an EKV card containing only literal scalar values.
///
/// # Errors
/// Unsupported flags, invalid values or nonliteral expressions.
pub fn ekv_literals(card: &ModelCard, defaults: CircuitDefaults) -> Result<Option<PspiceModel>> {
    ekv_resolved(card, defaults, parse_spice_constant)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{linked_source::RootKind, parameter_scope::ModelLibrary};

    #[test]
    fn native_level_five_is_not_ekv_and_literal_values_are_resolved() -> Result<()> {
        let cards = crate::model_card::parse_model_cards(
            b".model a nmos(level=5)\n.model b pspice_ekv26_pmos(level=5 kp=90u vto=-0.6)\n",
        )?;
        let defaults = CircuitDefaults::default();
        assert!(ekv_literals(&cards[0], defaults)?.is_none());
        let device = ekv_literals(&cards[1], defaults)?
            .context("missing EKV")?
            .bind([])?;
        assert_eq!(device.device.channel.polarity, Polarity::P);
        assert!((device.device.channel.kp - 90e-6).abs() < 1e-18);
        assert!((device.device.channel.threshold + 0.6).abs() < 1e-14);
        Ok(())
    }

    #[test]
    fn lexical_model_and_occurrence_overrides_are_isolated() -> Result<()> {
        let source = ModelLibrary::parse_source(
            b".subckt cell d g s b params: gain=2\n.model a pspice_ekv26_nmos(level=5 kp={gain*50u})\n.ends cell\n",
            RootKind::Include,
        )?;
        let defaults = CircuitDefaults::default();
        for multiplier in [2., 4.] {
            let model = source
                .resolve_ekv(0, &[(1, "gain", multiplier)], defaults)?
                .context("missing scoped EKV model")?;
            let device = model.bind([])?;
            assert!(f64::mul_add(multiplier, -50e-6, device.device.channel.kp).abs() < 1e-18);
        }
        Ok(())
    }
}
