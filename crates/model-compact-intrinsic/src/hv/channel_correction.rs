//! HiSIM-HV2 optional normal-channel parasitic current and ohmic leakage.
use super::{
    HiSimHvChannelElectrostaticPoint, HiSimHvChannelMobilityPoint, HiSimHvChannelRegion,
    HiSimHvIntrinsicBias, HiSimHvIntrinsicScalar as Jet, PreparedHiSimHvInstance,
    PreparedHiSimHvModel, instance::model_real, smoothing::positive,
};
use crate::{ModelError, ModelVariant};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct ChannelCorrection {
    punchthrough: f64,
    punchthrough_exponent: f64,
    drain_factor: f64,
    body_factor: f64,
    drain_induced: f64,
    ohmic_conductance: f64,
    width_multiplicity: f64,
}

/// Additional normalized current terms for the normal HiSIM-HV channel.
///
/// The parasitic channel term precedes the native access-resistance
/// correction; the ohmic term is added afterwards.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HiSimHvChannelCorrectionPoint {
    pub parasitic_channel_current_a: Jet,
    pub ohmic_leakage_a: Jet,
}

/// Shared physical inputs for the enhancement and depletion channel corrections.
#[derive(Clone, Copy)]
pub(super) struct CorrectionInputs {
    pub(super) bias: HiSimHvIntrinsicBias,
    pub(super) conditioned: super::HiSimHvBiasConditioningPoint,
    pub(super) surface: Jet,
    pub(super) surface_difference: Jet,
    pub(super) oxide: Jet,
    pub(super) mobility: Jet,
    pub(super) length: Jet,
    pub(super) active: bool,
}

impl ChannelCorrection {
    pub(super) fn prepare(
        model: &PreparedHiSimHvModel,
        instance: &PreparedHiSimHvInstance,
        multiplicity: f64,
    ) -> Result<Self, ModelError> {
        if model.variant() != ModelVariant::HiSimHv2 {
            return Ok(Self::default());
        }
        let length = instance.geometry().gate_length_m * 1e6;
        let geometry = instance.effective_geometry();
        let punchthrough = model_real(model, "ptl")? * length.powf(-model_real(model, "ptlp")?);
        let body_factor = model_real(model, "pt4")? * length.powf(-model_real(model, "pt4p")?);
        let gdl = model_real(model, "gdl")?;
        let drain_induced = if gdl == 0. {
            0.
        } else {
            gdl * model_real(model, "gdld")?
                .mul_add(1e6, length)
                .powf(-model_real(model, "gdlp")?)
        };
        let ohmic_conductance = multiplicity * geometry.channel_width_per_finger_m
            / geometry.channel_length_m
            * model_real(model, "gdsleak")?;
        let result = Self {
            punchthrough,
            punchthrough_exponent: model_real(model, "ptp")?,
            drain_factor: model_real(model, "pt2")?,
            body_factor,
            drain_induced,
            ohmic_conductance,
            width_multiplicity: geometry.channel_width_total_m * multiplicity,
        };
        if ![
            punchthrough,
            body_factor,
            drain_induced,
            ohmic_conductance,
            result.punchthrough_exponent,
            result.drain_factor,
            result.width_multiplicity,
        ]
        .into_iter()
        .all(f64::is_finite)
        {
            return Err(ModelError::InvalidPhysicalParameter(
                "HiSIM-HV channel correction coefficients",
            ));
        }
        Ok(result)
    }

    pub(super) fn evaluate(
        &self,
        bias: HiSimHvIntrinsicBias,
        state: &HiSimHvChannelElectrostaticPoint,
        mobility: &HiSimHvChannelMobilityPoint,
    ) -> Result<HiSimHvChannelCorrectionPoint, ModelError> {
        self.evaluate_core(&CorrectionInputs {
            bias,
            conditioned: state.conditioned,
            surface: state.charge.source_surface,
            surface_difference: state.charge.surface_difference,
            oxide: state.oxide.capacitance,
            mobility: mobility.mobility_m2_per_vs,
            length: state.modulation.effective_length_m,
            active: !matches!(
                state.charge.region,
                HiSimHvChannelRegion::Accumulation | HiSimHvChannelRegion::NoInversion
            ),
        })
    }

    pub(super) fn evaluate_core(
        &self,
        input: &CorrectionInputs,
    ) -> Result<HiSimHvChannelCorrectionPoint, ModelError> {
        let drain = Jet::coordinate(input.bias.vds, 1, 1.);
        let leakage = self.ohmic_conductance * drain;
        let zero = Jet::constant(0.);
        let mut correction = zero;
        if input.active && (self.punchthrough != 0. || self.drain_induced != 0.) {
            let conditioned = input.conditioned;
            let surface = input.surface;
            let difference = input.surface_difference;
            let drain_modified = conditioned.modified_drain;
            let mut coefficient = self.drain_induced * drain_modified;
            if self.punchthrough != 0. {
                // Fn_SymAdd((Vds-Pds)/2, 0.01): native sixth-order
                // reciprocal polynomial. Differentiate the complete value.
                let surface_modified =
                    surface + super::smoothing::symmetry_add(drain - difference, 0.01, 0.);
                let overdrive = positive(1.1 - surface_modified, 0.05) + 1e-25;
                let power = overdrive.positive_pow(self.punchthrough_exponent);
                let modulation = 1.
                    + drain_modified
                        * (self.drain_factor
                            + self.body_factor * (surface_modified - conditioned.modified_body));
                coefficient = coefficient + self.punchthrough * power * modulation;
            }
            if coefficient.value > 0. {
                // beta cancels between Idd1 and the betaWL transport factor.
                correction = self.width_multiplicity
                    * input.oxide
                    * difference
                    * coefficient
                    * input.mobility
                    / input.length;
            }
        }
        Ok(HiSimHvChannelCorrectionPoint {
            parasitic_channel_current_a: correction
                .checked("HiSIM-HV parasitic channel current")?,
            ohmic_leakage_a: leakage.checked("HiSIM-HV ohmic leakage current")?,
        })
    }
}
