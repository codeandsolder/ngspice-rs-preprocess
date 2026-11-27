//! HiSIM-HV surface impurity profile, pocket overlap, and normal-mode
//! Poisson material inputs from `hsmhv[2]temp.c` and `*temp_eval.h`.
use super::{
    HiSimHvIntrinsicScalar as Jet, PreparedHiSimHvInstance, PreparedHiSimHvModel,
    PreparedHiSimHvTemperature, instance::model_real, lookup_slot,
};
use crate::{ModelError, ModelVariant, ParameterScope, ParameterValue};
const ELECTRON_CHARGE: f64 = 1.602_191_8e-19;
const SILICON_PERMITTIVITY: f64 = 1.034_943e-10;
const METER_TO_MICROMETER: f64 = 1e6;

/// Geometry-dependent source doping and drain smoothing parameters.
/// Concentrations are in SI m^-3, after DFM, pocket, and STI corrections.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreparedHiSimHvElectrostaticMaterial {
    pub channel_doping: f64,
    pub reference_channel_potential: f64,
    pub reference_bulk_potential: f64,
    pub bulk_doping: f64,
    pub pocket_doping: f64,
    pub effective_pocket_doping: f64,
    pub pocket_extension_doping: f64,
    pub pocket_overlap_log: f64,
    pub drain_smoothing_exponent: f64,
    /// HV2 `CODEP=1` selects the distinct depletion-device channel law.
    pub depletion_mode: bool,
    pub depletion_doping: f64,
}
/// Local-temperature material inputs to the physical Poisson equations.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvElectrostaticMaterialPoint {
    pub depletion: Option<HiSimHvDepletionMaterialPoint>,
    pub twice_bulk_potential: Jet,
    pub poisson_coefficient: Jet,
    pub minority_coefficient: Jet,
    pub pocket_overlap_potential: Jet,
    pub bulk_depletion_width_factor: f64,
    pub pocket_depletion_width_factor: f64,
    pub charge_density_permittivity: f64,
}
/// HV2 depletion-mode bulk potential and built-in junction potential.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvDepletionMaterialPoint {
    pub twice_depletion_potential: Jet,
    pub built_in_potential: Jet,
}
fn positive(value: f64, name: &'static str) -> Result<f64, ModelError> {
    if value.is_finite() && value > 0. {
        Ok(value)
    } else {
        Err(ModelError::InvalidPhysicalParameter(name))
    }
}
fn selector(model: &PreparedHiSimHvModel, name: &str) -> Result<i32, ModelError> {
    let slot = lookup_slot(model.variant(), name)?;
    match model.get_slot(slot) {
        Some(ParameterValue::Integer(value)) => Ok(*value),
        _ => Err(ModelError::MissingDefaultDependency(slot)),
    }
}
fn binned(
    model: &PreparedHiSimHvModel,
    instance: &PreparedHiSimHvInstance,
    name: &str,
) -> Result<f64, ModelError> {
    let slot = lookup_slot(model.variant(), name)?;
    instance
        .binned_model_slot(slot)
        .ok_or(ModelError::MissingDefaultDependency(slot))
}
#[derive(Clone, Copy)]
struct DopingProfile {
    bulk: f64,
    pocket: f64,
    extension: f64,
}
fn initial_doping(
    model: &PreparedHiSimHvModel,
    instance: &PreparedHiSimHvInstance,
) -> Result<DopingProfile, ModelError> {
    let mut profile = DopingProfile {
        bulk: binned(model, instance, "nsubc")? * 1e6,
        pocket: binned(model, instance, "nsubp")? * 1e6,
        extension: binned(model, instance, "npext")? * 1e6,
    };
    if selector(model, "codfm")? == 1 {
        let spec = model
            .variant()
            .parameter(ParameterScope::Instance, "nsubcdfm")
            .ok_or_else(|| ModelError::UnknownParameter("nsubcdfm".to_owned()))?;
        if instance.explicitly_given(spec.slot) {
            let Some(ParameterValue::Real(value)) = instance.parameters().get_slot(spec.slot)
            else {
                return Err(ModelError::MissingDefaultDependency(spec.slot));
            };
            let corrected = *value * 1e6;
            let difference = corrected - profile.bulk;
            profile.pocket += difference;
            profile.extension += difference;
            profile.bulk = corrected;
        }
    }
    Ok(profile)
}
fn effective_pocket(
    model: &PreparedHiSimHvModel,
    instance: &PreparedHiSimHvInstance,
    pocket: f64,
    width_um: f64,
) -> Result<f64, ModelError> {
    let adjusted =
        pocket * (1. + model_real(model, "nsubp0")? / width_um.powf(model_real(model, "nsubwp")?));
    let layout = instance.transport_geometry();
    if layout.half_layout_length_m <= 0. {
        return Ok(adjusted);
    }
    let delta = binned(model, instance, "nsubpsti1")? / 100.;
    let weight = 1. / (1. + binned(model, instance, "nsubpsti2")?);
    let exponent = binned(model, instance, "nsubpsti3")?;
    let actual = (delta / layout.half_layout_length_m).powf(exponent);
    let reference = (delta / layout.reference_half_layout_length_m).powf(exponent);
    Ok(adjusted * weight.mul_add(actual, 1.) / weight.mul_add(reference, 1.))
}
fn drain_exponent(model: &PreparedHiSimHvModel, length_um: f64) -> Result<f64, ModelError> {
    let alternate = model.variant() == ModelVariant::HiSimHv2 && selector(model, "coddlt")? != 0;
    let intercept = model_real(model, "ddltict")?;
    let slope = model_real(model, "ddltslp")?;
    let maximum = model_real(model, "ddltmax")?;
    let raw = slope
        .mul_add(length_um, if alternate { 0. } else { intercept })
        .max(0.);
    positive(
        raw * maximum / (raw + maximum) + if alternate { intercept + 1e-25 } else { 1. },
        "HiSIM-HV drain exponent",
    )
}
/// Prepare native doping and pocket overlap, including DFM and STI effects.
/// # Errors
/// Invalid model context, nonpositive physical doping, or singular geometry.
pub fn prepare_hisim_hv_electrostatic_material(
    model: &PreparedHiSimHvModel,
    instance: &PreparedHiSimHvInstance,
) -> Result<PreparedHiSimHvElectrostaticMaterial, ModelError> {
    model.validate_instance(instance)?;
    let geometry = instance.geometry();
    let length = geometry.gate_length_m;
    let width_um = geometry.gate_width_per_finger_m * METER_TO_MICROMETER;
    let profile = initial_doping(model, instance)?;
    let pocket = effective_pocket(model, instance, profile.pocket, width_um)?;
    let bulk = profile.bulk
        * (1. + model_real(model, "nsubcw")? / width_um.powf(model_real(model, "nsubcwp")?));
    let lp = model_real(model, "lp")?;
    let lpext = model_real(model, "lpext")?;
    let base = if length > lp {
        pocket.mul_add(lp, bulk * (length - lp)) / length
    } else {
        (pocket - bulk).mul_add((lp - length) / lp, pocket)
    };
    let extension_length = 0.5_f64.mul_add(length, -lp);
    let correction = (extension_length.recip() + lpext.recip()).recip().max(0.);
    let channel = (correction / length).mul_add(profile.extension - bulk, base);
    let overlap = if length <= 2. * lp {
        ((pocket - bulk).mul_add(-length / lp, 2_f64.mul_add(pocket, -bulk)) / bulk).ln()
    } else {
        0.
    };
    let depletion_mode =
        model.variant() == ModelVariant::HiSimHv2 && selector(model, "codep")? == 1;
    let depletion_doping = if depletion_mode {
        positive(
            model_real(model, "ndepm")? * 1e6,
            "HiSIM-HV depletion doping",
        )?
    } else {
        channel
    };
    if !overlap.is_finite() || !profile.extension.is_finite() {
        return Err(ModelError::InvalidPhysicalParameter(
            "HiSIM-HV pocket profile",
        ));
    }
    Ok(PreparedHiSimHvElectrostaticMaterial {
        reference_channel_potential: 2. / 38.682_83
            * (positive(channel, "HiSIM-HV effective channel doping")? / 1.04e16).ln(),
        reference_bulk_potential: 2. / 38.682_83
            * (positive(bulk, "HiSIM-HV bulk doping")? / 1.04e16).ln(),
        channel_doping: positive(channel, "HiSIM-HV effective channel doping")?,
        bulk_doping: positive(bulk, "HiSIM-HV bulk doping")?,
        pocket_doping: positive(profile.pocket, "HiSIM-HV pocket doping")?,
        effective_pocket_doping: positive(pocket, "HiSIM-HV effective pocket doping")?,
        pocket_extension_doping: profile.extension,
        pocket_overlap_log: overlap,
        drain_smoothing_exponent: drain_exponent(model, length * METER_TO_MICROMETER)?,
        depletion_mode,
        depletion_doping,
    })
}
impl PreparedHiSimHvElectrostaticMaterial {
    /// Compute the native local-temperature material terms. For `CODEP=1`,
    /// `cnst0/cnst1` use the depletion doping, not the pocket channel doping.
    /// # Errors
    /// Nonpositive or nonfinite physical material/temperature inputs.
    pub fn evaluate(
        &self,
        temperature: &PreparedHiSimHvTemperature,
    ) -> Result<HiSimHvElectrostaticMaterialPoint, ModelError> {
        let beta = Jet::coordinate(
            positive(
                temperature.inverse_thermal_voltage,
                "HiSIM-HV thermal voltage",
            )?,
            3,
            temperature.inverse_thermal_voltage_derivative,
        );
        let ni = Jet::coordinate(
            positive(
                temperature.intrinsic_carrier_concentration_m3,
                "HiSIM-HV intrinsic carrier density",
            )?,
            3,
            temperature.intrinsic_carrier_derivative_m3_per_kelvin,
        );
        let doping = self.depletion_doping;
        let coefficient = (2. * SILICON_PERMITTIVITY * ELECTRON_CHARGE * doping / beta).sqrt();
        let ratio = ni / doping;
        let bulk_log = (self.channel_doping / ni).ln();
        let twice_bulk = 2. / beta * bulk_log;
        let depletion = if self.depletion_mode {
            let depletion_log = (doping / ni).ln();
            Some(HiSimHvDepletionMaterialPoint {
                twice_depletion_potential: (2. / beta * depletion_log)
                    .checked("HiSIM-HV depletion potential")?,
                built_in_potential: ((depletion_log + bulk_log) / beta)
                    .checked("HiSIM-HV depletion built-in potential")?,
            })
        } else {
            None
        };
        Ok(HiSimHvElectrostaticMaterialPoint {
            depletion,
            twice_bulk_potential: twice_bulk.checked("HiSIM-HV bulk potential")?,
            poisson_coefficient: coefficient.checked("HiSIM-HV Poisson coefficient")?,
            minority_coefficient: (ratio * ratio).checked("HiSIM-HV minority coefficient")?,
            pocket_overlap_potential: (self.pocket_overlap_log / beta)
                .checked("HiSIM-HV pocket overlap")?,
            bulk_depletion_width_factor: (2. * SILICON_PERMITTIVITY
                / (ELECTRON_CHARGE * self.channel_doping))
                .sqrt(),
            pocket_depletion_width_factor: (2. * SILICON_PERMITTIVITY
                / (ELECTRON_CHARGE * self.pocket_doping))
                .sqrt(),
            charge_density_permittivity: ELECTRON_CHARGE
                * self.channel_doping
                * SILICON_PERMITTIVITY,
        })
    }
}
