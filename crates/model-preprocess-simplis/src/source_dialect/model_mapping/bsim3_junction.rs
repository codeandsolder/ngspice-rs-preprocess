//! Source binding for the Berkeley junction branch of BSIM3 levels 49/53.
//!
//! This resolves junction coefficients after occurrence-specific parameter
//! evaluation. It does not choose, qualify or lower the intrinsic channel.
//! HSPICE MOSFET Models Manual X-2005.09, pp. 402–404 and 423–425.

use super::{HspiceBsim3Defaults, HspiceMosJunctionMode, ModelSelection, one};
use anyhow::{Context, Result, bail, ensure};
use model_compact_intrinsic::{
    Bsim3JunctionModel, Bsim3JunctionOccurrence, Bsim3JunctionRules, Bsim3KneeReference,
    MosBottomDensity, MosDepletionShape, MosDepletionTemperatureCoefficients,
    MosDiffusionDimensions, MosDiffusionRule, MosDiffusionTerminal, MosJunctionDepletion,
    MosJunctionSaturation, MosSaturationFallback, MosSeriesResistance, MosSeriesResistanceRule,
    MosSidewallRule, PreparedBsim3Junction,
};

/// The two independently supplied terminal geometries of one occurrence.
/// All dimensions have already been converted to SI by source unit handling.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HspiceBsim3JunctionInput {
    pub drawn_width_m: f64,
    /// Effective electrical channel width, before multiplicity.
    pub gate_width_m: f64,
    /// Drain, source order; omitted area/perimeter must remain absent.
    pub diffusion: [MosDiffusionTerminal; 2],
    /// Drain, source order; an explicitly zero square count is not omission.
    pub squares: [Option<f64>; 2],
    /// Global NRD/NRS defaults, only used in the explicit/width laws.
    pub default_squares: [f64; 2],
    pub operating_temperature_k: f64,
}

/// Source-selected Berkeley junction preparation. It holds no solver state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HspiceBsim3BerkeleyJunctions {
    model: Bsim3JunctionModel,
    rules: Bsim3JunctionRules,
    width_multiplier: f64,
    width_offset_m: f64,
    heavy_diffusion_m: f64,
    series: [MosSeriesResistance; 2],
}

impl HspiceBsim3BerkeleyJunctions {
    /// Prepare drain/source junctions after channel and occurrence geometry
    /// have been resolved. Parallel multiplicity is applied by the consumer.
    ///
    /// # Errors
    /// Invalid physical occurrence geometry, temperature or coefficients.
    pub fn prepare(self, input: HspiceBsim3JunctionInput) -> Result<[PreparedBsim3Junction; 2]> {
        let dimensions = MosDiffusionDimensions {
            width_m: input.drawn_width_m,
            width_multiplier: self.width_multiplier,
            width_offset_m: self.width_offset_m,
            heavy_diffusion_m: self.heavy_diffusion_m,
        };
        let prepare = |index: usize| -> Result<PreparedBsim3Junction> {
            self.model
                .prepare(
                    self.rules,
                    Bsim3JunctionOccurrence {
                        dimensions,
                        terminal: input.diffusion[index],
                        gate_width_m: input.gate_width_m,
                        series: MosSeriesResistance {
                            squares: input.squares[index],
                            default_squares: input.default_squares[index],
                            ..self.series[index]
                        },
                        operating_temperature_k: input.operating_temperature_k,
                    },
                )
                .map_err(Into::into)
        };
        Ok([prepare(0)?, prepare(1)?])
    }
}

struct Coefficients<'a, F> {
    selection: &'a ModelSelection,
    evaluate: F,
}

impl<F: FnMut(&str) -> Result<f64>> Coefficients<'_, F> {
    fn optional(&mut self, names: &[&str]) -> Result<Option<f64>> {
        let mut selected = None;
        for name in names {
            if let Some(raw) = one(&self.selection.card.parameters, name)? {
                ensure!(
                    selected.is_none(),
                    "duplicate BSIM3 junction aliases {names:?}"
                );
                let value = (self.evaluate)(raw)
                    .with_context(|| format!("BSIM3 junction parameter {name}"))?;
                ensure!(
                    value.is_finite(),
                    "nonfinite BSIM3 junction parameter {name}"
                );
                selected = Some(value);
            }
        }
        Ok(selected)
    }

    fn real(&mut self, names: &[&str], default: f64) -> Result<f64> {
        Ok(self.optional(names)?.unwrap_or(default))
    }

    /// Model-unit density inputs are converted to SI; documented defaults
    /// and inherited values have already been expressed in physical units.
    fn density(&mut self, names: &[&str], default_si: f64, scale: f64) -> Result<f64> {
        let density = self.optional(names)?.map_or(default_si, |raw| raw / scale);
        ensure!(
            density.is_finite(),
            "nonrepresentable BSIM3 density {names:?}"
        );
        Ok(density)
    }

    fn junction_model(
        &mut self,
        nominal_temperature_k: f64,
        model_scale: f64,
    ) -> Result<Bsim3JunctionModel> {
        let bottom_scale = model_scale * model_scale;
        ensure!(
            bottom_scale.is_finite() && bottom_scale > 0.,
            "unrepresentable BSIM3 model area scale"
        );
        let sidewall_cap = self.density(
            &["CJSW"],
            HspiceBsim3Defaults::SIDEWALL_CAPACITANCE_DENSITY,
            model_scale,
        )?;
        let sidewall_potential = self.real(&["PBSW"], 1.)?;
        let sidewall_grading = self.real(&["MJSW"], 0.33)?;
        let nominal = self
            .optional(&["TNOM", "TREF"])?
            .map_or(nominal_temperature_k, |celsius| celsius + 273.15);
        Ok(Bsim3JunctionModel {
            saturation: MosJunctionSaturation {
                bottom: MosBottomDensity::PerSquareMetre(self.density(
                    &["JS"],
                    HspiceBsim3Defaults::JUNCTION_CURRENT_DENSITY,
                    bottom_scale,
                )?),
                sidewall_a_per_m: self.density(
                    &["JSW"],
                    HspiceBsim3Defaults::SIDEWALL_CURRENT_DENSITY,
                    model_scale,
                )?,
                // The original Berkeley current law uses a direct current
                // only when both junction geometry measures are zero.
                fallback: MosSaturationFallback::ZeroGeometry(1e-14),
            },
            depletion: MosJunctionDepletion {
                bottom: MosBottomDensity::PerSquareMetre(self.density(
                    &["CJ"],
                    HspiceBsim3Defaults::BOTTOM_CAPACITANCE_DENSITY,
                    bottom_scale,
                )?),
                bottom_shape: MosDepletionShape {
                    potential_v: self.real(&["PB", "PHIB"], 1.)?,
                    grading: self.real(&["MJ"], 0.5)?,
                },
                sidewall_f_per_m: sidewall_cap,
                sidewall_shape: MosDepletionShape {
                    potential_v: sidewall_potential,
                    grading: sidewall_grading,
                },
                gate_edge_f_per_m: self.density(&["CJSWG"], sidewall_cap, model_scale)?,
                gate_edge_shape: MosDepletionShape {
                    potential_v: self.real(&["PBSWG"], sidewall_potential)?,
                    grading: self.real(&["MJSWG"], sidewall_grading)?,
                },
                fallback_f: 0.,
            },
            depletion_temperature: [
                MosDepletionTemperatureCoefficients {
                    capacitance_per_k: self.real(&["TCJ"], 0.)?,
                    potential_v_per_k: self.real(&["TPB"], 0.)?,
                },
                MosDepletionTemperatureCoefficients {
                    capacitance_per_k: self.real(&["TCJSW"], 0.)?,
                    potential_v_per_k: self.real(&["TPBSW"], 0.)?,
                },
                MosDepletionTemperatureCoefficients {
                    capacitance_per_k: self.real(&["TCJSWG"], 0.)?,
                    potential_v_per_k: self.real(&["TPBSWG"], 0.)?,
                },
            ],
            nominal_temperature_k: nominal,
            emission_coefficient: self.real(&["NJ"], 1.)?,
            saturation_temperature_exponent: self.real(&["XTI"], 3.)?,
            limiting_current_a: self.real(&["IJTH", "JTH"], 0.1)?,
        })
    }
}

impl ModelSelection {
    /// Resolve the source's Berkeley-mode junction coefficients using the
    /// caller's occurrence-aware expression evaluator.
    ///
    /// `model_scale` is the global model length-unit scale, overridden by an
    /// explicit card-local SCALM; the instance
    /// dimensions supplied later are in SI. The source's junction geometry
    /// and parasitic resistance modes are intentionally resolved separately.
    /// This API prepares the junction branch only, not a complete BSIM3 model.
    ///
    /// # Errors
    /// Wrong source family, a non-Berkeley junction selector, duplicate
    /// aliases, nonfinite coefficients or unsupported source revision.
    pub fn bind_hspice_bsim3_berkeley_junctions(
        &self,
        nominal_temperature_k: f64,
        model_scale: f64,
        evaluate: impl FnMut(&str) -> Result<f64>,
    ) -> Result<HspiceBsim3BerkeleyJunctions> {
        self.hspice_bsim3_defaults()
            .context("not an HSPICE BSIM3 model")?;
        ensure!(
            matches!(self.effective_version.as_deref(), Some("3.24" | "3.3")),
            "unqualified BSIM3 junction revision"
        );
        let selected = self
            .hspice_mos_junction()?
            .context("missing BSIM3 junction selector")?;
        let resistance = match selected.mode {
            HspiceMosJunctionMode::BerkeleyExplicit => MosSeriesResistanceRule::Explicit,
            HspiceMosJunctionMode::BerkeleyWidthNormalized => {
                MosSeriesResistanceRule::WidthDependent
            }
            HspiceMosJunctionMode::BerkeleyIsolated | HspiceMosJunctionMode::BerkeleyStacked => {
                MosSeriesResistanceRule::AutomaticSquares
            }
            _ => bail!("non-Berkeley BSIM3 junction requires its distinct constitutive law"),
        };
        let rules = Bsim3JunctionRules {
            diffusion: if selected.automatic_bsim3_geometry {
                MosDiffusionRule::Isolated
            } else {
                MosDiffusionRule::ExplicitPhysical
            },
            sidewall: MosSidewallRule::SplitPerimeter,
            resistance,
            knee_reference: Bsim3KneeReference::Resolved,
        };
        let mut c = Coefficients {
            selection: self,
            evaluate,
        };
        let model_scale = c.real(&["SCALM"], model_scale)?;
        ensure!(
            model_scale.is_finite() && model_scale > 0.,
            "invalid BSIM3 model length scale"
        );
        let model = c.junction_model(nominal_temperature_k, model_scale)?;
        // Both light-diffusion lengths enter the extrinsic resistance law.
        // BSIM3's separate intrinsic XJ is not itself a supplied lateral LD.
        let light_path_m =
            (c.real(&["LD", "DLAT", "LATD"], 0.)? + c.real(&["LDIF"], 0.)?) * model_scale;
        let sheet = c.real(&["RSH", "RL"], 0.)?;
        let drain = MosSeriesResistance {
            terminal_resistance: c.real(&["RD"], 0.)?,
            sheet_resistance_ohm_per_square: sheet,
            contact_resistance_ohm: c.real(&["RDC"], 0.)?,
            squares: None,
            default_squares: 0.,
            light_path_m,
        };
        let source = MosSeriesResistance {
            terminal_resistance: c.real(&["RS"], 0.)?,
            contact_resistance_ohm: c.real(&["RSC"], 0.)?,
            ..drain
        };
        Ok(HspiceBsim3BerkeleyJunctions {
            model,
            rules,
            width_multiplier: c.real(&["WMLT"], 1.)?,
            width_offset_m: c.real(&["XW"], 0.)? * model_scale,
            heavy_diffusion_m: c.real(&["HDIF"], 0.)? * model_scale,
            series: [drain, source],
        })
    }
}
