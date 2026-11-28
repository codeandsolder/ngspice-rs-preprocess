//! Source binding for the occurrence modifiers of HSPICE BSIM4 level 54.

use super::{Device, ModelSelection, finite_coefficient};
use anyhow::{Result, ensure};
use model_compact_intrinsic::{
    Bsim4ChannelModifiers, Bsim4OccurrenceModifiers, Bsim4SeriesModifiers,
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
