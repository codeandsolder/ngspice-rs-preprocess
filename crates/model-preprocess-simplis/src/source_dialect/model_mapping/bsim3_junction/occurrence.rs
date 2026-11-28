//! Source-instance geometry and temperature binding. Defaults must not turn
//! an omitted automatic diffusion geometry into an explicit zero.

use super::{HspiceBsim3JunctionInput, HspiceBsim3Junctions, JunctionLaw, finite_coefficient};
use anyhow::{Result, bail, ensure};
use model_compact_intrinsic::{MosDiffusionRule, MosDiffusionTerminal, MosLinearReverseGeometry};
use model_normalize_ngspice::model_card::ModelParameter;

/// Global element options resolved in their own lexical scope.
///
/// Explicit option dimensions are already SI; SCALE applies to raw instance fields.
/// The absent area/perimeter defaults are distinct from explicit zeroes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HspiceBsim3OccurrenceOptions {
    pub element_scale: f64,
    pub default_width_m: f64,
    /// Drain, source order.
    pub default_area_m2: [Option<f64>; 2],
    pub default_perimeter_m: [Option<f64>; 2],
    pub default_squares: [f64; 2],
    /// Circuit operating temperature before the occurrence DTEMP offset.
    pub temperature_k: f64,
}

impl Default for HspiceBsim3OccurrenceOptions {
    fn default() -> Self {
        Self {
            element_scale: 1.,
            default_width_m: 1e-4,
            default_area_m2: [None; 2],
            default_perimeter_m: [None; 2],
            default_squares: [0.; 2],
            temperature_k: 298.15,
        }
    }
}

fn scaled(value: Option<f64>, scale: f64) -> Result<Option<f64>> {
    value
        .map(|raw| {
            let physical = raw * scale;
            ensure!(
                physical.is_finite(),
                "nonrepresentable scaled MOS instance geometry"
            );
            Ok(physical)
        })
        .transpose()
}

fn resolve_sharing(geo: f64) -> Result<[bool; 2]> {
    // HSPICE MOSFET Models Manual X-2005.09, pp. 53–54.
    match geo {
        0. => Ok([false, false]),
        1. => Ok([true, false]),
        2. => Ok([false, true]),
        3. => Ok([true, true]),
        _ => bail!("unsupported HSPICE BSIM3 GEO selector {geo}"),
    }
}

impl HspiceBsim3Junctions {
    const fn takes_default_geometry(&self) -> bool {
        match self.law {
            JunctionLaw::Berkeley { rules, .. } => {
                matches!(rules.diffusion, MosDiffusionRule::ExplicitPhysical)
            }
            JunctionLaw::LinearReverse { mode, .. } => {
                matches!(mode, MosLinearReverseGeometry::Explicit)
            }
        }
    }

    /// Resolve raw element coefficients in the occurrence scope, without
    /// applying the selected model's width multiplier twice. The caller
    /// supplies the effective electrical channel width from channel prep.
    /// ACM2/3 (and Berkeley automatic area calculation) ignore global area
    /// and perimeter defaults. Explicit element zeroes remain present.
    ///
    /// # Errors
    /// Invalid or duplicate instance coefficients, geometry, temperature,
    /// scaling or GEO selector.
    pub fn bind_occurrence_geometry(
        &self,
        fields: &[ModelParameter],
        options: HspiceBsim3OccurrenceOptions,
        gate_width_m: f64,
        mut evaluate: impl FnMut(&str) -> Result<f64>,
    ) -> Result<HspiceBsim3JunctionInput> {
        let scale = options.element_scale;
        let area_scale = scale * scale;
        ensure!(
            scale.is_finite() && scale > 0. && area_scale.is_finite() && area_scale > 0.,
            "invalid HSPICE MOS element scale"
        );
        let mut coefficient = |name| finite_coefficient(fields, &[name], &mut evaluate);
        let drawn_width_m = scaled(coefficient("W")?, scale)?.unwrap_or(options.default_width_m);
        let shared = resolve_sharing(coefficient("GEO")?.unwrap_or(0.))?;
        let mut diffusion = [MosDiffusionTerminal::default(); 2];
        for (index, (area, perimeter)) in [("AD", "PD"), ("AS", "PS")].into_iter().enumerate() {
            let mut area_m2 = scaled(coefficient(area)?, area_scale)?;
            let mut perimeter_m = scaled(coefficient(perimeter)?, scale)?;
            if self.takes_default_geometry() {
                area_m2 = area_m2.or(options.default_area_m2[index]);
                perimeter_m = perimeter_m.or(options.default_perimeter_m[index]);
            }
            diffusion[index] = MosDiffusionTerminal {
                area_m2,
                perimeter_m,
                shared: shared[index],
            };
        }
        let squares = [coefficient("NRD")?, coefficient("NRS")?];
        let operating_temperature_k = options.temperature_k + coefficient("DTEMP")?.unwrap_or(0.);
        ensure!(
            drawn_width_m.is_finite()
                && drawn_width_m > 0.
                && gate_width_m.is_finite()
                && gate_width_m > 0.,
            "invalid BSIM3 occurrence width"
        );
        ensure!(
            operating_temperature_k.is_finite() && operating_temperature_k > 0.,
            "invalid BSIM3 occurrence temperature"
        );
        ensure!(
            diffusion
                .iter()
                .flat_map(|terminal| [terminal.area_m2, terminal.perimeter_m])
                .chain(squares)
                .flatten()
                .chain(options.default_squares)
                .all(|value| value.is_finite() && value >= 0.),
            "invalid BSIM3 occurrence geometry or diffusion squares"
        );
        Ok(HspiceBsim3JunctionInput {
            drawn_width_m,
            gate_width_m,
            diffusion,
            squares,
            default_squares: options.default_squares,
            operating_temperature_k,
        })
    }
}
