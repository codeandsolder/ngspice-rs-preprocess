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
        let drain = Jet::coordinate(bias.vds, 1, 1.);
        let leakage = self.ohmic_conductance * drain;
        let zero = Jet::constant(0.);
        let active = !matches!(
            state.charge.region,
            HiSimHvChannelRegion::Accumulation | HiSimHvChannelRegion::NoInversion
        );
        let mut correction = zero;
        if active && (self.punchthrough != 0. || self.drain_induced != 0.) {
            let conditioned = state.conditioned;
            let surface = state.charge.source_surface;
            let difference = state.charge.surface_difference;
            let drain_modified = conditioned.modified_drain;
            let mut coefficient = self.drain_induced * drain_modified;
            if self.punchthrough != 0. {
                // Fn_SymAdd((Vds-Pds)/2, 0.01): native sixth-order
                // reciprocal polynomial. Differentiate the complete value.
                let ratio = (drain - difference) / 0.01;
                let denominator = 1.
                    + ratio
                        * (0.5
                            + ratio
                                * (1. / 6.
                                    + ratio
                                        * (1. / 24.
                                            + ratio
                                                * (1. / 120.
                                                    + ratio * (1. / 720. + ratio / 5040.)))));
                let surface_modified = surface + 0.01 / denominator;
                let overdrive = positive(1.1 - surface_modified, 0.05) + 1e-25;
                let power = overdrive.value.powf(self.punchthrough_exponent);
                let power =
                    overdrive.map(power, self.punchthrough_exponent * power / overdrive.value);
                let modulation = 1.
                    + drain_modified
                        * (self.drain_factor
                            + self.body_factor * (surface_modified - conditioned.modified_body));
                coefficient = coefficient + self.punchthrough * power * modulation;
            }
            if coefficient.value > 0. {
                // beta cancels between Idd1 and the betaWL transport factor.
                correction = self.width_multiplicity
                    * state.oxide.capacitance
                    * difference
                    * coefficient
                    * mobility.mobility_m2_per_vs
                    / state.modulation.effective_length_m;
            }
        }
        Ok(HiSimHvChannelCorrectionPoint {
            parasitic_channel_current_a: correction
                .checked("HiSIM-HV parasitic channel current")?,
            ohmic_leakage_a: leakage.checked("HiSIM-HV ohmic leakage current")?,
        })
    }
}
