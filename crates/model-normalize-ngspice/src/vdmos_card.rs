//! Typed separation of LTspice/ngspice VDMOS source declarations and metadata.
//! Numerical VDMOS physics remains in `model-kernel-vdmos`.

use crate::model_card::{ModelCard, ModelParameter, parse_spice_constant};
use anyhow::{Result, bail, ensure};

/// Source-level VDMOS polarity with flags resolved once at model compilation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VdmosPolarity {
    /// N-channel device (the default with no explicit polarity flag).
    N,
    /// P-channel device selected by the PCHAN flag or VDMOSP family.
    P,
}

/// Original `LTspice` informational annotation, kept for provenance and
/// inventory but not evaluated as a physical model parameter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ModelAnnotation<'a> {
    /// Annotation name (`mfg`, `vds`, `ron`, `qg`).
    pub name: &'a str,
    /// Original annotation text (e.g. manufacturer identifier or rating).
    pub value: &'a str,
}

/// Native numeric physical parameters and retained non-physical annotations.
/// Symbols, expressions and unsupported flags fail explicitly on this path.
#[derive(Debug)]
pub struct VdmosLiterals<'a> {
    /// The model's original identifier.
    pub name: &'a str,
    /// Source-selected model polarity.
    pub polarity: VdmosPolarity,
    /// Scalar physical name/value pairs in original declaration order.
    pub parameters: Vec<(&'a str, f64)>,
    /// Metadata preserved verbatim, not silently dropped or sent to the kernel.
    pub annotations: Vec<ModelAnnotation<'a>>,
    /// Instance-only defaults retain their source expressions, separate from physics.
    pub instance_defaults: Vec<&'a ModelParameter>,
}

/// Extract a VDMOS literal-parameter card from an arbitrary parsed `.model`.
/// Returns `None` for unrelated model families without inspecting their laws.
///
/// Full source expression/symbol resolution remains a separate compiler phase.
/// This function deliberately requires scalar numeric *physical* fields and
/// retains ordinary `LTspice` catalog annotations separately. The actual typed
/// VDMOS physics compiler owns the recognition of numeric parameter names.
///
/// # Errors
///
/// Rejects conflicting polarity flags, unsupported bare flags, or nonnumeric
/// physical expressions instead of weakening the model silently.
pub fn vdmos_literals(card: &ModelCard) -> Result<Option<VdmosLiterals<'_>>> {
    vdmos_resolved(card, parse_spice_constant)
}

/// Compile scalar VDMOS source parameters with an external, explicit symbol
/// resolver; model metadata is preserved and never passed through this resolver.
///
/// # Errors
///
/// Rejects unresolved expressions, invalid polarity and unsupported flags.
pub fn vdmos_resolved(
    card: &ModelCard,
    mut resolve_number: impl FnMut(&str) -> Result<f64>,
) -> Result<Option<VdmosLiterals<'_>>> {
    let mut polarity = match card.family.to_ascii_lowercase().as_str() {
        "vdmos" | "vdmosn" => VdmosPolarity::N,
        "vdmosp" => VdmosPolarity::P,
        _ => return Ok(None),
    };
    let mut n_requested = false;
    let mut p_requested = false;
    let mut parameters = Vec::new();
    let mut annotations = Vec::new();
    let mut instance_defaults = Vec::new();
    for param in &card.parameters {
        if matches!(
            param.name.to_ascii_lowercase().as_str(),
            "temp" | "dtemp" | "thermal" | "off" | "ic" | "icvds" | "icvgs"
        ) {
            instance_defaults.push(param);
            continue;
        }
        if param.name.eq_ignore_ascii_case("pchan") || param.name.eq_ignore_ascii_case("nchan") {
            let enabled = match param.expression.as_deref() {
                None => true,
                Some(value) => {
                    let numeric = resolve_number(value)?;
                    let enabled = (numeric - 1.0).abs() < f64::EPSILON;
                    ensure!(
                        enabled || numeric.abs() < f64::EPSILON,
                        "VDMOS {} polarity flag must be 0 or 1",
                        param.name
                    );
                    enabled
                }
            };
            if param.name.eq_ignore_ascii_case("pchan") {
                p_requested |= enabled;
            } else {
                n_requested |= enabled;
            }
            continue;
        }
        ensure!(
            !param.name.eq_ignore_ascii_case("type"),
            "VDMOS TYPE is a read-only polarity query"
        );
        let expression = param
            .expression
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("unsupported VDMOS bare flag {}", param.name))?;
        if matches!(
            param.name.to_ascii_lowercase().as_str(),
            "mfg" | "vds" | "ron" | "qg"
        ) {
            annotations.push(ModelAnnotation {
                name: &param.name,
                value: expression,
            });
        } else {
            parameters.push((param.name.as_str(), resolve_number(expression)?));
        }
    }
    if n_requested && p_requested {
        bail!(
            "VDMOS model {} declares conflicting PCHAN and NCHAN",
            card.name
        );
    }
    if n_requested {
        polarity = VdmosPolarity::N;
    }
    if p_requested {
        polarity = VdmosPolarity::P;
    }
    Ok(Some(VdmosLiterals {
        name: &card.name,
        polarity,
        parameters,
        annotations,
        instance_defaults,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model_card::parse_model_cards;

    #[test]
    fn retains_vendor_annotations_and_selects_p_channel() -> Result<()> {
        let source = b".model irf P VTO=-3\n.model p VDMOS(pchan Rg=3 Vto=-1.2 Rd=2m Is=15p mfg=Rohm Vds=-30 Ron=4m Qg=19n)\n";
        let cards = parse_model_cards(source)?;
        assert!(vdmos_literals(&cards[0])?.is_none());
        let resolved =
            vdmos_literals(&cards[1])?.ok_or_else(|| anyhow::anyhow!("missing VDMOS model"))?;
        assert_eq!(resolved.polarity, VdmosPolarity::P);
        assert_eq!(resolved.parameters.len(), 4);
        assert_eq!(resolved.annotations.len(), 4);
        assert_eq!(resolved.annotations[0].value, "Rohm");
        assert_eq!(resolved.annotations[1].value, "-30");
        assert_eq!(resolved.parameters[2].1, 0.002);
        Ok(())
    }

    #[test]
    fn polarity_conflicts_and_unresolved_physics_are_errors() -> Result<()> {
        for (source, expected) in [
            (".model x vdmos (PCHAN NCHAN Kp=2)", "conflicting"),
            (
                ".model x vdmos (PCHAN PCHAN=0 UnsupportedFlag)",
                "unsupported",
            ),
            (".model x vdmos (Kp={multiplier * 4})", "not a scalar"),
            (".model x vdmos (PCHAN=2 Kp=2)", "0 or 1"),
            (".model x vdmos (TYPE=vdmosp)", "read-only"),
        ] {
            let cards = parse_model_cards(source.as_bytes())?;
            let Err(error) = vdmos_literals(&cards[0]) else {
                anyhow::bail!("expected fail-closed source error for {source}");
            };
            assert!(format!("{error:#}").contains(expected), "{source}: {error}");
        }
        Ok(())
    }
}
