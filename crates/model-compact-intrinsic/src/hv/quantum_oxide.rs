//! Version-specific HiSIM-HV quantum oxide-thickness correction.
use super::smoothing::{positive, upper};
use super::{
    HiSimHvIntrinsicBias, HiSimHvIntrinsicScalar as Jet, PreparedHiSimHvInstance,
    PreparedHiSimHvModel, instance::model_real, lookup_slot,
    prepare_hisim_hv_electrostatic_material,
};
use crate::{ModelError, ModelVariant};
const VACUUM_PERMITTIVITY: f64 = 8.854_187_8e-12;
const QME_SMOOTHING: f64 = 1e-11;

/// Prepared native QME coefficients and physical oxide reference.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreparedHiSimHvQuantumOxide {
    variant: ModelVariant,
    oxide: f64,
    permittivity: f64,
    flatband: f64,
    threshold: f64,
    qme1: f64,
    qme2: f64,
    qme3: f64,
}
/// Local effective oxide values and the Poisson capacitance factor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvQuantumOxidePoint {
    pub active: bool,
    pub thickness: Jet,
    pub capacitance: Jet,
    pub inverse_capacitance: Jet,
    pub poisson_capacitance_factor: Jet,
}
/// Prepare the original HV1/HV2 QME selector and reference threshold.
/// # Errors
/// Invalid model context, oxide geometry, or nonfinite model coefficients.
pub fn prepare_hisim_hv_quantum_oxide(
    model: &PreparedHiSimHvModel,
    instance: &PreparedHiSimHvInstance,
) -> Result<PreparedHiSimHvQuantumOxide, ModelError> {
    model.validate_instance(instance)?;
    let oxide = model_real(model, "tox")?;
    let permittivity = VACUUM_PERMITTIVITY * model_real(model, "kappa")?;
    let flatband_slot = lookup_slot(model.variant(), "vfbc")?;
    let flatband = instance
        .binned_model_slot(flatband_slot)
        .ok_or(ModelError::MissingDefaultDependency(flatband_slot))?;
    let material = prepare_hisim_hv_electrostatic_material(model, instance)?;
    let pb20 = material.reference_channel_potential;
    let qnsub_esi = 1.602_191_8e-19 * 1.034_943e-10 * material.channel_doping;
    let threshold = (2. * qnsub_esi * pb20)
        .sqrt()
        .mul_add(oxide / permittivity, pb20 + flatband);
    let qme1 = model_real(model, "qme1")?;
    let qme2 = model_real(model, "qme2")?;
    let qme3 = model_real(model, "qme3")?;
    if oxide <= 0.
        || permittivity <= 0.
        || ![oxide, permittivity, threshold, qme1, qme2, qme3]
            .into_iter()
            .all(f64::is_finite)
    {
        return Err(ModelError::InvalidPhysicalParameter(
            "HiSIM-HV quantum oxide parameters",
        ));
    }
    Ok(PreparedHiSimHvQuantumOxide {
        variant: model.variant(),
        oxide,
        permittivity,
        flatband,
        threshold,
        qme1,
        qme2,
        qme3,
    })
}
impl PreparedHiSimHvQuantumOxide {
    /// Classical oxide capacitance, before any QME correction.
    #[must_use]
    pub fn zero_field_capacitance(&self) -> f64 {
        self.permittivity / self.oxide
    }
    /// Evaluate the version-specific QME oxide law using the actual modified
    /// gate and Poisson coefficient. The full derivative includes the dynamic
    /// reciprocal ceiling in HV2, which the legacy derivative path omits.
    /// # Errors
    /// Nonfinite physical voltages, nonpositive oxide, or nonfinite derivatives.
    pub fn evaluate(
        &self,
        bias: HiSimHvIntrinsicBias,
        modified_gate: Jet,
        poisson_coefficient: Jet,
    ) -> Result<HiSimHvQuantumOxidePoint, ModelError> {
        if ![bias.vgs, bias.vds, bias.vbs]
            .into_iter()
            .all(f64::is_finite)
            || bias.vds < 0.
        {
            return Err(ModelError::InvalidPhysicalParameter(
                "HiSIM-HV quantum oxide bias",
            ));
        }
        modified_gate.checked("HiSIM-HV modified quantum gate")?;
        poisson_coefficient.checked("HiSIM-HV quantum Poisson coefficient")?;
        let mut correction = Jet::constant(0.);
        if (self.qme1 != 0. || self.qme3 != 0.) && self.qme2 != 0. {
            correction = if self.variant == ModelVariant::HiSimHv1 {
                let gate = positive(self.threshold + self.qme2 - modified_gate, 0.05) + 1e-25;
                let coefficient = self.qme1 / self.qme2.powi(2);
                upper(
                    coefficient * gate * gate + self.qme3,
                    Jet::constant(self.qme1),
                    QME_SMOOTHING,
                )
            } else {
                let gate = Jet::coordinate(bias.vgs, 0, 1.);
                let body = Jet::coordinate(bias.vbs, 2, 1.);
                let overdrive = gate - body - self.threshold + self.qme2;
                let dynamic = self.flatband - body - self.threshold + self.qme2;
                let lower = 2. * self.threshold.abs();
                let ceiling = if dynamic.value > lower {
                    dynamic
                } else {
                    Jet::constant(lower)
                };
                let inverse = 1. / (positive(overdrive, QME_SMOOTHING) + 1e-25);
                self.qme1 * upper(inverse, 1. / ceiling, QME_SMOOTHING) + self.qme3
            };
            if correction.value * 1e12 < self.oxide {
                correction = Jet::constant(0.);
            }
        }
        let thickness = self.oxide + correction;
        if thickness.value <= 0. {
            return Err(ModelError::InvalidPhysicalParameter(
                "HiSIM-HV effective quantum thickness",
            ));
        }
        let inverse = thickness / self.permittivity;
        let factor = poisson_coefficient * inverse;
        Ok(HiSimHvQuantumOxidePoint {
            active: correction.value != 0.,
            thickness: thickness.checked("HiSIM-HV quantum thickness")?,
            capacitance: (1. / inverse).checked("HiSIM-HV quantum capacitance")?,
            inverse_capacitance: inverse.checked("HiSIM-HV quantum inverse capacitance")?,
            poisson_capacitance_factor: (factor * factor)
                .checked("HiSIM-HV quantum Poisson factor")?,
        })
    }
}
