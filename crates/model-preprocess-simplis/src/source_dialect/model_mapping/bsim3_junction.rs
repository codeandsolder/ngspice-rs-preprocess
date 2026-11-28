//! Source binding for both physical junction branches of BSIM3 levels 49/53.
//!
//! This resolves junction coefficients after occurrence-specific parameter
//! evaluation. It does not choose, qualify or lower the intrinsic channel.
//! HSPICE MOSFET Models Manual X-2005.09, pp. 402–404 and 423–425.

use super::{HspiceBsim3Defaults, HspiceMosJunctionMode, ModelSelection, finite_coefficient};

mod linear_reverse;
use anyhow::{Context, Result, ensure};

use model_compact_intrinsic::{
    Bsim3JunctionModel, Bsim3JunctionRules, Bsim3KneeReference, MosBottomDensity,
    MosDepletionShape, MosDepletionTemperatureCoefficients, MosDiffusionDimensions,
    MosDiffusionRule, MosDiffusionTerminal, MosJunctionDepletion, MosJunctionOccurrence,
    MosJunctionSaturation, MosLinearReverseGeometry, MosLinearReverseModel, MosSaturationFallback,
    MosSeriesResistance, MosSeriesResistanceRule, MosSidewallRule, PreparedMosJunction,
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

/// Global source settings in effect at a selected model occurrence.
/// Model-card SCALM, TNOM/TREF and thermal selectors override their defaults.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HspiceJunctionSourceOptions {
    pub nominal_temperature_k: f64,
    pub model_scale: f64,
    pub aspec: bool,
    pub update: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum JunctionLaw {
    Berkeley {
        model: Bsim3JunctionModel,
        rules: Bsim3JunctionRules,
    },
    LinearReverse {
        model: MosLinearReverseModel,
        direct_capacitance_f: [f64; 2],
        mode: MosLinearReverseGeometry,
        resistance: MosSeriesResistanceRule,
    },
}

/// Source-selected physical junction preparation, with no solver state.
#[derive(Clone, Debug, PartialEq)]
pub struct HspiceBsim3Junctions {
    law: JunctionLaw,
    geometry: JunctionGeometry,
}

impl HspiceBsim3Junctions {
    /// Prepare both terminals after occurrence geometry is resolved.
    /// Parallel multiplicity is applied by the consumer.
    ///
    /// # Errors
    /// Invalid physical occurrence geometry, temperature or coefficients.
    pub fn prepare(&self, input: HspiceBsim3JunctionInput) -> Result<[PreparedMosJunction; 2]> {
        let [drain, source] = self.geometry.occurrences(input);
        let prepare = |index: usize, occurrence| -> Result<PreparedMosJunction> {
            Ok(match self.law {
                JunctionLaw::Berkeley { model, rules } => {
                    PreparedMosJunction::BerkeleyBsim3(model.prepare(rules, occurrence)?)
                }
                JunctionLaw::LinearReverse {
                    model,
                    direct_capacitance_f,
                    mode,
                    resistance,
                } => {
                    let terminal = MosLinearReverseModel {
                        depletion: MosJunctionDepletion {
                            fallback_f: direct_capacitance_f[index],
                            ..model.depletion
                        },
                        ..model
                    };
                    PreparedMosJunction::LinearReverse(
                        terminal.prepare(mode, resistance, occurrence)?,
                    )
                }
            })
        };
        Ok([prepare(0, drain)?, prepare(1, source)?])
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct JunctionGeometry {
    width_multiplier: f64,
    width_offset_m: f64,
    heavy_diffusion_m: f64,
    series: [MosSeriesResistance; 2],
    resistance_temperature_per_k: [f64; 2],
}

impl JunctionGeometry {
    fn occurrences(self, input: HspiceBsim3JunctionInput) -> [MosJunctionOccurrence; 2] {
        let dimensions = MosDiffusionDimensions {
            width_m: input.drawn_width_m,
            width_multiplier: self.width_multiplier,
            width_offset_m: self.width_offset_m,
            heavy_diffusion_m: self.heavy_diffusion_m,
        };
        std::array::from_fn(|index| MosJunctionOccurrence {
            dimensions,
            terminal: input.diffusion[index],
            gate_width_m: input.gate_width_m,
            series: MosSeriesResistance {
                squares: input.squares[index],
                default_squares: input.default_squares[index],
                ..self.series[index]
            },
            resistance_temperature_per_k: self.resistance_temperature_per_k[index],
            operating_temperature_k: input.operating_temperature_k,
        })
    }
}

struct Coefficients<'a, F> {
    selection: &'a ModelSelection,
    evaluate: F,
}

impl<F: FnMut(&str) -> Result<f64>> Coefficients<'_, F> {
    fn optional(&mut self, names: &[&str]) -> Result<Option<f64>> {
        finite_coefficient(&self.selection.card.parameters, names, &mut self.evaluate)
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

    fn model_scale(&mut self, default: f64) -> Result<f64> {
        let scale = self.real(&["SCALM"], default)?;
        ensure!(
            scale.is_finite() && scale > 0.,
            "invalid BSIM3 model length scale"
        );
        Ok(scale)
    }

    fn geometry(&mut self, model_scale: f64, ldif: f64) -> Result<JunctionGeometry> {
        let light_path_m = (self.real(&["LD", "DLAT", "LATD"], 0.)? + ldif) * model_scale;
        let sheet = self.real(&["RSH", "RL"], 0.)?;
        let drain = MosSeriesResistance {
            terminal_resistance: self.real(&["RD"], 0.)?,
            sheet_resistance_ohm_per_square: sheet,
            contact_resistance_ohm: self.real(&["RDC"], 0.)?,
            squares: None,
            default_squares: 0.,
            light_path_m,
        };
        let source = MosSeriesResistance {
            terminal_resistance: self.real(&["RS"], 0.)?,
            contact_resistance_ohm: self.real(&["RSC"], 0.)?,
            ..drain
        };
        Ok(JunctionGeometry {
            width_multiplier: self.real(&["WMLT"], 1.)?,
            width_offset_m: self.real(&["XW"], 0.)? * model_scale,
            heavy_diffusion_m: self.real(&["HDIF"], 0.)? * model_scale,
            series: [drain, source],
            resistance_temperature_per_k: [self.real(&["TRD"], 0.)?, self.real(&["TRS"], 0.)?],
        })
    }

    fn nominal_temperature(&mut self, default_kelvin: f64) -> Result<f64> {
        Ok(self
            .optional(&["TNOM", "TREF"])?
            .map_or(default_kelvin, |celsius| celsius + 273.15))
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
        let nominal = self.nominal_temperature(nominal_temperature_k)?;
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
    /// Bind the selected source junction law with an occurrence-aware scalar
    /// evaluator. ACM selects the constitutive law and resistance policy;
    /// the caller does not repeat that decision. Model SCALM overrides the
    /// global length-unit scale; supplied occurrence geometry is in SI.
    /// This does not qualify or lower the intrinsic BSIM3 channel.
    ///
    /// # Errors
    /// Wrong source family, unsupported revision, duplicate aliases,
    /// invalid selectors or nonfinite coefficients.
    pub fn bind_hspice_bsim3_junctions(
        &self,
        options: HspiceJunctionSourceOptions,
        evaluate: impl FnMut(&str) -> Result<f64>,
    ) -> Result<HspiceBsim3Junctions> {
        self.hspice_bsim3_defaults()
            .context("not an HSPICE BSIM3 model")?;
        ensure!(
            matches!(self.effective_version.as_deref(), Some("3.24" | "3.3")),
            "unqualified BSIM3 junction revision"
        );
        let selected = self
            .hspice_mos_junction()?
            .context("missing BSIM3 junction selector")?;
        let mut c = Coefficients {
            selection: self,
            evaluate,
        };
        let scale = c.model_scale(options.model_scale)?;
        let ldif = c.real(&["LDIF"], 0.)?;
        let resistance = match selected.mode {
            HspiceMosJunctionMode::HspiceExplicit | HspiceMosJunctionMode::BerkeleyExplicit => {
                MosSeriesResistanceRule::Explicit
            }
            HspiceMosJunctionMode::HspiceWidthNormalized
            | HspiceMosJunctionMode::BerkeleyWidthNormalized => {
                if options.aspec && options.update >= 1 && ldif == 0. {
                    MosSeriesResistanceRule::DirectContact
                } else {
                    MosSeriesResistanceRule::WidthDependent
                }
            }
            HspiceMosJunctionMode::HspiceIsolated
            | HspiceMosJunctionMode::HspiceStacked
            | HspiceMosJunctionMode::BerkeleyIsolated
            | HspiceMosJunctionMode::BerkeleyStacked => MosSeriesResistanceRule::AutomaticSquares,
        };
        let mode = match selected.mode {
            HspiceMosJunctionMode::HspiceExplicit => Some(MosLinearReverseGeometry::Explicit),
            HspiceMosJunctionMode::HspiceWidthNormalized => {
                Some(MosLinearReverseGeometry::WidthNormalized)
            }
            HspiceMosJunctionMode::HspiceIsolated => Some(MosLinearReverseGeometry::Isolated),
            HspiceMosJunctionMode::HspiceStacked => Some(MosLinearReverseGeometry::Stacked),
            HspiceMosJunctionMode::BerkeleyExplicit
            | HspiceMosJunctionMode::BerkeleyWidthNormalized
            | HspiceMosJunctionMode::BerkeleyIsolated
            | HspiceMosJunctionMode::BerkeleyStacked => None,
        };
        let law = if let Some(mode) = mode {
            JunctionLaw::LinearReverse {
                model: c.linear_reverse_model(options, scale, mode)?,
                direct_capacitance_f: [c.real(&["CBD"], 0.)?, c.real(&["CBS"], 0.)?],
                mode,
                resistance,
            }
        } else {
            JunctionLaw::Berkeley {
                model: c.junction_model(options.nominal_temperature_k, scale)?,
                rules: Bsim3JunctionRules {
                    diffusion: if selected.automatic_bsim3_geometry {
                        MosDiffusionRule::Isolated
                    } else {
                        MosDiffusionRule::ExplicitPhysical
                    },
                    sidewall: MosSidewallRule::SplitPerimeter,
                    resistance,
                    knee_reference: Bsim3KneeReference::Resolved,
                },
            }
        };
        Ok(HspiceBsim3Junctions {
            law,
            geometry: c.geometry(scale, ldif)?,
        })
    }
}
