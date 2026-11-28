//! Source binding for the occurrence modifiers of HSPICE BSIM4 level 54.

use super::{
    Device, HspiceJunctionSourceOptions, HspiceMosJunctionMode, ModelSelection,
    bsim3_junction::{Coefficients, JunctionGeometry},
    finite_coefficient,
};
use anyhow::{Context, Result, ensure};
use model_compact_intrinsic::{
    Bsim4ChannelModifiers, Bsim4ModifiedSeries, Bsim4OccurrenceModifiers, Bsim4SeriesModifiers,
    MosSeriesResistanceRule,
};
use model_normalize_ngspice::model_card::ModelParameter;

impl ModelSelection {
    /// Bind the independently documented channel and series-resistance
    /// modifiers for an occurrence of an explicitly supported BSIM4 release.
    /// The model and occurrence evaluators are distinct, preserving their
    /// respective lexical scopes. This only binds the modifiers; it does not
    /// certify the underlying channel or junction implementation.
    ///
    /// # Errors
    /// Wrong source model family or revision, duplicate fields, nonfinite
    /// or unresolved coefficients.
    pub fn bind_hspice_bsim4_occurrence_modifiers(
        &self,
        instance: &[ModelParameter],
        mut evaluate_model: impl FnMut(&str) -> Result<f64>,
        mut evaluate_instance: impl FnMut(&str) -> Result<f64>,
    ) -> Result<Bsim4OccurrenceModifiers> {
        ensure!(
            self.mapping.device == Device::Bsim4 && self.mapping.key.level == 54,
            "not an HSPICE BSIM4 model"
        );
        ensure!(
            self.declared_version.is_some()
                && matches!(
                    self.effective_version.as_deref(),
                    Some("4.5" | "4.64" | "4.70")
                ),
            "unqualified HSPICE BSIM4 modifier revision"
        );
        let mut occurrence = |names: &[&str], default| {
            Ok::<_, anyhow::Error>(
                finite_coefficient(instance, names, &mut evaluate_instance)?.unwrap_or(default),
            )
        };
        let mut model = |names: &[&str], default| {
            Ok::<_, anyhow::Error>(
                finite_coefficient(&self.card.parameters, names, &mut evaluate_model)?
                    .unwrap_or(default),
            )
        };
        Ok(Bsim4OccurrenceModifiers {
            channel: Bsim4ChannelModifiers {
                threshold_shift_v: occurrence(&["DELVTO", "DELVT0"], 0.)?,
                mobility_multiplier: occurrence(&["MULU0"], 1.)?,
                body_effect_shift_sqrt_v: occurrence(&["DELK1"], 0.)?,
                subthreshold_factor_shift: occurrence(&["DELNFCT"], 0.)?,
            },
            series: Bsim4SeriesModifiers {
                contact_per_finger_ohm: [occurrence(&["RDC"], 0.)?, occurrence(&["RSC"], 0.)?],
                temperature_per_k: [model(&["TRD"], 0.)?, model(&["TRS"], 0.)?],
            },
        })
    }
}

/// Source-selected nominal diffusion resistance law, before contacts,
/// temperature correction and parallel instance multiplicity.
#[derive(Clone, Copy, Debug, PartialEq)]
enum DiffusionResistance {
    Native,
    Calculated {
        rule: MosSeriesResistanceRule,
        geometry: JunctionGeometry,
    },
}

/// Geometry and temperature already resolved in the occurrence scope.
/// Native BSIM4 preparation retains ownership of its own geometry.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bsim4SeriesInput {
    pub drawn_width_m: f64,
    pub finger_count: f64,
    /// Drain, source; omitted square counts are not explicit zeroes.
    pub squares: [Option<f64>; 2],
    pub default_squares: [f64; 2],
    pub operating_temperature_k: f64,
}

/// Bound source policy for BSIM4 extrinsic resistance and channel modifiers.
/// The native channel model and its intrinsic bias-dependent resistances are
/// not replaced by this preparation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HspiceBsim4Occurrence {
    diffusion: DiffusionResistance,
    modifiers: Bsim4OccurrenceModifiers,
    nominal_temperature_k: f64,
}

impl HspiceBsim4Occurrence {
    #[must_use = "apply occurrence modifiers to the selected channel bin"]
    pub const fn channel_modifiers(self) -> Bsim4ChannelModifiers {
        self.modifiers.channel
    }

    /// Whether native BSIM4 geometry must calculate nominal diffusion
    /// resistance (ACM12, independently of CALCACM).
    #[must_use = "select the nominal diffusion geometry before installing topology"]
    pub const fn uses_native_diffusion(self) -> bool {
        matches!(self.diffusion, DiffusionResistance::Native)
    }

    /// Resolve full-finger-group drain/source resistance before multiplicity.
    /// The native callback supplies unmodified, nominal resistances only;
    /// it is never invoked for the explicitly selected nonnative laws.
    ///
    /// # Errors
    /// Invalid physical geometry or resistance, or native preparation error.
    pub fn prepare_series(
        self,
        input: Bsim4SeriesInput,
        native_diffusion: impl FnOnce() -> Result<[f64; 2]>,
    ) -> Result<Bsim4ModifiedSeries> {
        let diffusion = match self.diffusion {
            DiffusionResistance::Native => native_diffusion()?,
            DiffusionResistance::Calculated { rule, geometry } => geometry.nominal_resistances(
                rule,
                input.drawn_width_m,
                input.squares,
                input.default_squares,
            )?,
        };
        Ok(self.modifiers.series.prepare(
            diffusion,
            input.finger_count,
            self.nominal_temperature_k,
            input.operating_temperature_k,
        )?)
    }
}

impl ModelSelection {
    /// Bind the documented BSIM4 parasitic-resistance law and occurrence
    /// modifiers. ACM12 retains native BSIM4 diffusion geometry; other modes
    /// select the corresponding chapter-2 resistance law.
    ///
    /// # Errors
    /// Unsupported source identity, revision, selector, or model coefficient.
    pub fn bind_hspice_bsim4_occurrence(
        &self,
        instance: &[ModelParameter],
        options: HspiceJunctionSourceOptions,
        mut evaluate_model: impl FnMut(&str) -> Result<f64>,
        evaluate_instance: impl FnMut(&str) -> Result<f64>,
    ) -> Result<HspiceBsim4Occurrence> {
        let modifiers = self.bind_hspice_bsim4_occurrence_modifiers(
            instance,
            &mut evaluate_model,
            evaluate_instance,
        )?;
        let selected = self
            .hspice_mos_junction()?
            .context("missing BSIM4 junction selector")?;
        let mut coefficients = Coefficients {
            selection: self,
            evaluate: evaluate_model,
        };
        let diffusion = if selected.mode == HspiceMosJunctionMode::BerkeleyIsolated {
            DiffusionResistance::Native
        } else {
            let scale = coefficients.model_scale(options.model_scale)?;
            let ldif = coefficients.real(&["LDIF"], 0.)?;
            let mut geometry = coefficients.diffusion_geometry(scale, ldif)?;
            geometry.reset_nonpositive_width_multiplier();
            DiffusionResistance::Calculated {
                rule: selected.mode.series_rule(options, ldif),
                geometry,
            }
        };
        Ok(HspiceBsim4Occurrence {
            diffusion,
            modifiers,
            nominal_temperature_k: coefficients
                .nominal_temperature(options.nominal_temperature_k)?,
        })
    }
}
