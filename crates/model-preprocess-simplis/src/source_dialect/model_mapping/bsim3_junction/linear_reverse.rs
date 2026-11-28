//! The separate linear-reverse source junction modes (ACM 0–3).

use super::{
    Coefficients, HspiceBsim3Defaults, HspiceBsim3JunctionInput, HspiceMosJunctionMode,
    JunctionGeometry, ModelSelection,
};
use anyhow::{Context, Result, bail, ensure};
use model_compact_intrinsic::{
    MosBottomDensity, MosDepletionShape, MosDepletionTemperatureCoefficients,
    MosDepletionThermalLaw, MosJunctionBandgap, MosJunctionDepletion, MosJunctionSaturation,
    MosLinearReverseGeometry, MosLinearReverseModel, MosSaturationFallback,
    MosSeriesResistanceRule, PreparedMosLinearReverseJunction,
};

/// Global source settings in effect at a selected model occurrence.
/// Model-card SCALM, TNOM/TREF and thermal selectors override their defaults.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HspiceJunctionSourceOptions {
    pub nominal_temperature_k: f64,
    pub model_scale: f64,
    pub aspec: bool,
    pub update: u32,
}

/// Both terminal laws of a source-selected linear-reverse BSIM3 junction.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HspiceBsim3LinearReverseJunctions {
    models: [MosLinearReverseModel; 2],
    mode: MosLinearReverseGeometry,
    resistance: MosSeriesResistanceRule,
    geometry: JunctionGeometry,
}

impl HspiceBsim3LinearReverseJunctions {
    /// Prepare drain/source independently after occurrence-specific evaluation.
    ///
    /// # Errors
    /// Invalid physical occurrence geometry, temperature or coefficients.
    pub fn prepare(
        self,
        input: HspiceBsim3JunctionInput,
    ) -> Result<[PreparedMosLinearReverseJunction; 2]> {
        let [drain, source] = self.geometry.occurrences(input);
        Ok([
            self.models[0].prepare(self.mode, self.resistance, drain)?,
            self.models[1].prepare(self.mode, self.resistance, source)?,
        ])
    }
}

impl<F: FnMut(&str) -> Result<f64>> Coefficients<'_, F> {
    fn bandgap(&mut self, aspec: bool) -> Result<MosJunctionBandgap> {
        match self.real(&["TLEV"], f64::from(u8::from(aspec)))? {
            0. | 1. => Ok(MosJunctionBandgap::SILICON),
            2. => Ok(MosJunctionBandgap {
                zero_temperature_ev: self.real(&["EG"], 1.16)?,
                alpha_ev_per_k: self.real(&["GAP1"], 7.02e-4)?,
                beta_k: self.real(&["GAP2"], 1108.)?,
            }),
            value => bail!("unsupported MOS TLEV selector {value}"),
        }
    }

    fn depletion_laws(&mut self, aspec: bool) -> Result<[MosDepletionThermalLaw; 3]> {
        let selector = self.real(&["TLEVC"], f64::from(u8::from(aspec)))?;
        let [bottom, side] = match selector {
            0. => [MosDepletionThermalLaw::Semiconductor; 2],
            1. => [
                MosDepletionThermalLaw::Linear(MosDepletionTemperatureCoefficients {
                    capacitance_per_k: self.real(&["CTA"], 0.)?,
                    potential_v_per_k: self.real(&["PTA"], 0.)?,
                }),
                MosDepletionThermalLaw::Linear(MosDepletionTemperatureCoefficients {
                    capacitance_per_k: self.real(&["CTP"], 0.)?,
                    potential_v_per_k: self.real(&["PTP"], 0.)?,
                }),
            ],
            2. => [
                MosDepletionThermalLaw::PotentialPower {
                    potential_v_per_k: self.real(&["PTA"], 0.)?,
                },
                MosDepletionThermalLaw::PotentialPower {
                    potential_v_per_k: self.real(&["PTP"], 0.)?,
                },
            ],
            3. => [MosDepletionThermalLaw::SemiconductorTangent; 2],
            value => bail!("unsupported MOS TLEVC selector {value}"),
        };
        Ok([bottom, side, side])
    }

    fn linear_reverse_model(
        &mut self,
        options: HspiceJunctionSourceOptions,
        scale: f64,
        mode: MosLinearReverseGeometry,
    ) -> Result<MosLinearReverseModel> {
        let width_normalized = mode == MosLinearReverseGeometry::WidthNormalized;
        let bottom_scale = if width_normalized {
            scale
        } else {
            scale * scale
        };
        ensure!(
            bottom_scale.is_finite() && bottom_scale > 0.,
            "unrepresentable MOS bottom density scale"
        );
        let density_units = |value| {
            if width_normalized {
                MosBottomDensity::PerMetre(value)
            } else {
                MosBottomDensity::PerSquareMetre(value)
            }
        };
        let nominal_temperature_k = self.nominal_temperature(options.nominal_temperature_k)?;
        let sidewall = self.density(
            &["CJSW", "CJP"],
            HspiceBsim3Defaults::SIDEWALL_CAPACITANCE_DENSITY,
            scale,
        )?;
        let sidewall_shape = MosDepletionShape {
            potential_v: self.real(&["PHP"], 1.)?,
            grading: self.real(&["MJSW", "EXP"], 0.33)?,
        };
        let direct_saturation = self.real(&["IS"], if options.aspec { 0. } else { 1e-14 })?;
        Ok(MosLinearReverseModel {
            saturation: MosJunctionSaturation {
                bottom: density_units(self.density(
                    &["JS"],
                    HspiceBsim3Defaults::JUNCTION_CURRENT_DENSITY,
                    bottom_scale,
                )?),
                sidewall_a_per_m: self.density(
                    &["JSW"],
                    HspiceBsim3Defaults::SIDEWALL_CURRENT_DENSITY,
                    scale,
                )?,
                fallback: MosSaturationFallback::ZeroCurrent(direct_saturation),
            },
            depletion: MosJunctionDepletion {
                bottom: density_units(self.density(
                    &["CJ", "CDB", "CSB", "CJA"],
                    HspiceBsim3Defaults::BOTTOM_CAPACITANCE_DENSITY,
                    bottom_scale,
                )?),
                bottom_shape: MosDepletionShape {
                    potential_v: self.real(&["PB", "PHIB", "PHA", "PHS", "PHD"], 1.)?,
                    grading: self.real(&["MJ", "EXA", "EXJ", "EXS", "EXD"], 0.5)?,
                },
                sidewall_f_per_m: sidewall,
                sidewall_shape,
                gate_edge_f_per_m: self.density(&["CJGATE"], sidewall, scale)?,
                gate_edge_shape: sidewall_shape,
                fallback_f: 0.,
            },
            depletion_temperature: self.depletion_laws(options.aspec)?,
            nominal_temperature_k,
            emission_coefficient: self.real(&["N"], 1.)?,
            saturation_temperature_exponent: self.real(&["XTI"], 3.)?,
            bandgap: self.bandgap(options.aspec)?,
            reverse_transition_v: self.real(&["VNDS"], -1.)?,
            reverse_slope_divisor: self.real(&["NDS"], 1.)?,
            transit_time_s: self.real(&["TT"], 0.)?,
            resistance_temperature_per_k: 0.,
        })
    }
}

impl ModelSelection {
    /// Bind source ACM 0–3 junction coefficients with an occurrence-aware
    /// scalar evaluator. Unlike the Berkeley modes these use N, VNDS, NDS,
    /// IS, TT, PHP, CJGATE and the independently selected thermal laws.
    /// The caller must still qualify the intrinsic channel and its geometry.
    ///
    /// # Errors
    /// Wrong family/mode, unsupported release, ambiguous aliases or invalid
    /// coefficients and source options.
    pub fn bind_hspice_bsim3_linear_reverse_junctions(
        &self,
        options: HspiceJunctionSourceOptions,
        evaluate: impl FnMut(&str) -> Result<f64>,
    ) -> Result<HspiceBsim3LinearReverseJunctions> {
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
        let (mode, resistance) = match selected.mode {
            HspiceMosJunctionMode::HspiceExplicit => (
                MosLinearReverseGeometry::Explicit,
                MosSeriesResistanceRule::Explicit,
            ),
            HspiceMosJunctionMode::HspiceWidthNormalized => (
                MosLinearReverseGeometry::WidthNormalized,
                if options.aspec && options.update >= 1 && ldif == 0. {
                    MosSeriesResistanceRule::DirectContact
                } else {
                    MosSeriesResistanceRule::WidthDependent
                },
            ),
            HspiceMosJunctionMode::HspiceIsolated => (
                MosLinearReverseGeometry::Isolated,
                MosSeriesResistanceRule::AutomaticSquares,
            ),
            HspiceMosJunctionMode::HspiceStacked => (
                MosLinearReverseGeometry::Stacked,
                MosSeriesResistanceRule::AutomaticSquares,
            ),
            _ => bail!("Berkeley BSIM3 junction requires its distinct constitutive law"),
        };
        let common = c.linear_reverse_model(options, scale, mode)?;
        let drain = MosLinearReverseModel {
            depletion: MosJunctionDepletion {
                fallback_f: c.real(&["CBD"], 0.)?,
                ..common.depletion
            },
            resistance_temperature_per_k: c.real(&["TRD"], 0.)?,
            ..common
        };
        let source = MosLinearReverseModel {
            depletion: MosJunctionDepletion {
                fallback_f: c.real(&["CBS"], 0.)?,
                ..common.depletion
            },
            resistance_temperature_per_k: c.real(&["TRS"], 0.)?,
            ..common
        };
        Ok(HspiceBsim3LinearReverseJunctions {
            models: [drain, source],
            mode,
            resistance,
            geometry: c.geometry(scale, ldif)?,
        })
    }
}
