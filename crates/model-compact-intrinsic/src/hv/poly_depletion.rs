//! HiSIM-HV poly-silicon gate depletion and intrinsic effective gate voltage.
use super::smoothing::{lower, positive, upper};
use super::{
    HiSimHvBiasConditioningPoint, HiSimHvIntrinsicBias, HiSimHvIntrinsicScalar as Jet,
    PreparedHiSimHvInstance, PreparedHiSimHvModel, PreparedHiSimHvTemperature,
    instance::model_real, lookup_slot, prepare_hisim_hv_electrostatic_material,
    prepare_hisim_hv_quantum_oxide,
};
use crate::{ModelError, ModelVariant};

/// Native size-dependent gate depletion parameters.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreparedHiSimHvPolyDepletion {
    variant: ModelVariant,
    coefficient: f64,
    onset: f64,
    drain_factor: f64,
    flatband: f64,
    reference_bulk: f64,
    reference_threshold: f64,
    saturation_scale: f64,
}
/// Native poly depletion, gate voltage and accumulation/symmetry factors.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvPolyDepletionPoint {
    pub depletion: Jet,
    pub effective_gate: Jet,
    pub flatband_voltage: Jet,
    pub symmetry_factor: Jet,
    pub accumulation_damping: Jet,
}
fn exp_limit(x: Jet) -> Jet {
    if x.value < -3. {
        Jet::constant(0.)
    } else if x.value < 0. {
        1. + x * (1. + x * (1. / 3. + x / 27.))
    } else {
        1. + x * (1. + x * (1. / 3. + x * (0.040_205_293_451_395_1 + x * 0.148_148_111_111_111)))
    }
}
fn upper_polynomial4(x: Jet) -> Jet {
    if x.value <= 1. {
        let tail = 1. + x * (1. + x * (1. + x));
        x * tail / (1. + x * tail)
    } else {
        let z = 1. / x;
        let z2 = z * z;
        1. - z2 * z2 / (1. + z * (1. + z * (1. + z * (1. + z))))
    }
}
/// Prepare native poly-depletion and saturation geometry from the bound model.
/// # Errors
/// Invalid model instance or nonfinite coefficients.
pub fn prepare_hisim_hv_poly_depletion(
    model: &PreparedHiSimHvModel,
    instance: &PreparedHiSimHvInstance,
) -> Result<PreparedHiSimHvPolyDepletion, ModelError> {
    model.validate_instance(instance)?;
    let binned = |name| {
        let slot = lookup_slot(model.variant(), name)?;
        instance
            .binned_model_slot(slot)
            .ok_or(ModelError::MissingDefaultDependency(slot))
    };
    let material = prepare_hisim_hv_electrostatic_material(model, instance)?;
    let oxide = prepare_hisim_hv_quantum_oxide(model, instance)?;
    let flatband = binned("vfbc")?;
    let density = 1.602_191_8e-19 * 1.034_943e-10 * material.channel_doping;
    let length = instance.geometry().gate_length_m * 1e6;
    let result = PreparedHiSimHvPolyDepletion {
        variant: model.variant(),
        coefficient: (1. + length.recip()).powf(model_real(model, "pgd4")?) * binned("pgd1")?,
        onset: model_real(model, "pgd2")?,
        drain_factor: if model.variant() == ModelVariant::HiSimHv1 {
            binned("pgd3")?
        } else {
            0.
        },
        flatband,
        reference_bulk: material.reference_bulk_potential,
        reference_threshold: material.reference_channel_potential
            + flatband
            + (2. * density * material.reference_channel_potential).sqrt()
                / oxide.zero_field_capacitance(),
        saturation_scale: density / oxide.zero_field_capacitance().powi(2),
    };
    if result.saturation_scale <= 0.
        || ![
            result.coefficient,
            result.onset,
            result.drain_factor,
            result.flatband,
            result.reference_bulk,
            result.reference_threshold,
            result.saturation_scale,
        ]
        .into_iter()
        .all(f64::is_finite)
    {
        return Err(ModelError::InvalidPhysicalParameter(
            "HiSIM-HV poly coefficients",
        ));
    }
    Ok(result)
}
impl PreparedHiSimHvPolyDepletion {
    fn symmetry(
        &self,
        gate: Jet,
        drain: Jet,
        body: Jet,
        temperature: &PreparedHiSimHvTemperature,
    ) -> Jet {
        let drive = gate - self.flatband;
        let scale = self.saturation_scale;
        let radicand = positive(
            1. + 2. / scale * (drive - 1. / temperature.nominal_inverse_thermal_voltage - body),
            0.001,
        );
        let saturation = lower(
            drive + scale * (1. - radicand.sqrt()) - self.reference_bulk,
            Jet::constant(0.1),
            0.05,
        );
        let factor = upper_polynomial4(drain / saturation);
        factor * factor
    }
    /// Evaluate intrinsic gate depletion. HV1 retains its drain partition and
    /// accumulation damping; HV2 uses the modified gate and smooth onset.
    /// # Errors
    /// Invalid bias or nonfinite physical result.
    pub fn evaluate(
        &self,
        bias: HiSimHvIntrinsicBias,
        conditioned: &HiSimHvBiasConditioningPoint,
        threshold_shift: Jet,
        temperature: &PreparedHiSimHvTemperature,
    ) -> Result<HiSimHvPolyDepletionPoint, ModelError> {
        if ![bias.vgs, bias.vds, bias.vbs]
            .into_iter()
            .all(f64::is_finite)
            || bias.vds < 0.
        {
            return Err(ModelError::InvalidPhysicalParameter("HiSIM-HV poly bias"));
        }
        let gate = Jet::coordinate(bias.vgs, 0, 1.);
        let drain = Jet::coordinate(bias.vds, 1, 1.);
        let hv1 = self.variant == ModelVariant::HiSimHv1;
        let symmetry = if hv1 {
            self.symmetry(gate, drain, conditioned.confined_body, temperature)
        } else {
            Jet::constant(0.)
        };
        let mut damping = Jet::constant(0.);
        let depletion = if self.coefficient == 0. {
            Jet::constant(0.)
        } else if hv1 {
            let gate_factor = if self.onset > self.flatband {
                let factor = upper(
                    positive((gate - self.flatband) / (self.onset - self.flatband), 0.001),
                    Jet::constant(1.),
                    0.001,
                );
                factor * factor
            } else {
                Jet::constant(1.)
            };
            let factor = if self.drain_factor == 0. {
                Jet::constant(0.)
            } else {
                0.5 + symmetry * gate_factor * (self.drain_factor - 0.5)
            };
            let base = self.flatband + conditioned.modified_body;
            let span = positive(0.6 * (self.reference_threshold - base), 0.01);
            damping = upper(
                positive((conditioned.modified_gate - base) / span, 0.3),
                Jet::constant(1.),
                0.1,
            );
            upper(
                self.coefficient * exp_limit(gate - self.onset - factor * drain),
                Jet::constant(1.),
                0.05,
            )
        } else {
            upper(
                self.coefficient
                    * positive(exp_limit(conditioned.modified_gate - self.onset) - 1., 0.05),
                Jet::constant(1.),
                0.05,
            )
        };
        let effective_gate = gate - self.flatband + threshold_shift - depletion;
        let flatband_voltage =
            self.flatband - threshold_shift + depletion + conditioned.confined_body;
        Ok(HiSimHvPolyDepletionPoint {
            depletion: depletion.checked("HiSIM-HV poly depletion")?,
            effective_gate: effective_gate.checked("HiSIM-HV effective gate")?,
            flatband_voltage: flatband_voltage.checked("HiSIM-HV flatband")?,
            symmetry_factor: symmetry.checked("HiSIM-HV drain symmetry factor")?,
            accumulation_damping: damping.checked("HiSIM-HV accumulation damping")?,
        })
    }
}
