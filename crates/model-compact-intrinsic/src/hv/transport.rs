//! Layout/mobility and temperature-dependent transport preparation for the
//! Hiroshima University/STARC HiSIM-HV1 and HiSIM-HV2 models.
//!
//! These expressions track the same computations in ngspice 47
//! `hsmhv[2]temp.c` and `hsmhv[2]temp_eval.h`; they are coefficients to feed
//! the eventual nonlinear compact-model kernels, not the kernels themselves.

use super::{
    PreparedHiSimHvModel, PreparedHiSimHvTemperature, compiled_hv_by_family,
    effective::HiSimHvEffectiveGeometry,
    instance::{HiSimHvBinningGeometry, PreparedHiSimHvInstance, instance_real, model_real},
    lookup_slot, source_binned_at_name, source_real_at,
};
use crate::{BoundParameters, ModelError, ModelVariant, ParameterScope, ParameterValue};
use std::sync::{Arc, OnceLock};

/// Temperature-independent mobility and transport coefficients prepared once
/// when binding an individual HiSIM-HV transistor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvTransportGeometry {
    /// `Lod_half_ref` from layout reference SA/SB and drawn length, in meters.
    pub reference_half_layout_length_m: f64,
    /// `Lod_half`, using source/drain gate-to-STI spacing, NF and SD, in meters.
    pub half_layout_length_m: f64,
    /// `mueph` after DFM, L/W/area and STI corrections.
    pub phonon_scattering_coefficient: f64,
    /// `muesr` after width/length dependence.
    pub surface_roughness_coefficient: f64,
    /// `vmax0`, layout multiplier on the binned saturation velocity.
    pub saturation_velocity_geometry_factor: f64,
    /// `ninvd0`, length/width-dependent density-gradient coefficient.
    pub density_gradient_base: f64,
    /// `rthtemp0`, the inverse finger/width weighting on thermal resistance.
    pub thermal_resistance_geometry_factor: f64,
    /// `M * Weff * NF`, geometry factor used by native C thermal capacitance.
    pub thermal_capacitance_geometry_factor_m: f64,
}

const CM_PER_M: f64 = 100.0;
// Native C hsmhv[2]ld.c `C_RTH_MIN` for the passive thermal branch.
const RTH_MIN: f64 = 1e-4;
const MICROMETERS_PER_M: f64 = 1e6;

const fn finite(name: &'static str, value: f64) -> Result<f64, ModelError> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(ModelError::InvalidPhysicalParameter(name))
    }
}

fn positive(name: &'static str, value: f64) -> Result<f64, ModelError> {
    if value.is_finite() && value > 0.0 {
        Ok(value)
    } else {
        Err(ModelError::InvalidPhysicalParameter(name))
    }
}

fn model_integer(model: &PreparedHiSimHvModel, name: &str) -> Result<i32, ModelError> {
    let slot = lookup_slot(model.variant(), name)?;
    match model.get_slot(slot) {
        Some(ParameterValue::Integer(value)) => Ok(*value),
        _ => Err(ModelError::MissingDefaultDependency(slot)),
    }
}

fn explicitly_given(raw: &BoundParameters, name: &str) -> Result<bool, ModelError> {
    let spec = raw
        .variant()
        .parameter(ParameterScope::Instance, name)
        .ok_or_else(|| ModelError::UnknownParameter(name.to_owned()))?;
    Ok(raw.get_slot(spec.slot).is_some())
}

fn layout_half_lengths(
    model: &PreparedHiSimHvModel,
    source: &[Option<ParameterValue>],
) -> Result<(f64, f64), ModelError> {
    let variant = model.variant();
    let l = instance_real(source, variant, "l")?;
    let nf = instance_real(source, variant, "nf")?;
    let sa = instance_real(source, variant, "sa")?;
    let sb = instance_real(source, variant, "sb")?;
    let sd = instance_real(source, variant, "sd")?;
    let reference_source_spacing = model_real(model, "saref")?;
    let reference_drain_spacing = model_real(model, "sbref")?;
    let reference_sum = 1.0 / 0.5f64.mul_add(l, reference_source_spacing)
        + 1.0 / 0.5f64.mul_add(l, reference_drain_spacing);
    let reference = 2.0 / reference_sum;
    if !(sa > 0.0 && sb > 0.0 && (nf >= 1.0 && (nf <= 1.0 || sd > 0.0))) {
        return Ok((reference, 0.0));
    }
    // C uses `int i; for (i = 0; i < here->nf; ++i)`. Fractional NF
    // therefore loops ceil(NF) times but divides the sum by the real NF.
    if nf.ceil() > f64::from(i32::MAX) {
        return Err(ModelError::InvalidPhysicalParameter(
            "HiSIM-HV NF exceeds native C loop bounds",
        ));
    }
    let mut reciprocal_sum = 0.0;
    for index in 0..i32::MAX {
        if f64::from(index) >= nf {
            break;
        }
        let offset = f64::from(index) * (sd + l);
        reciprocal_sum +=
            1.0 / (0.5f64.mul_add(l, sa) + offset) + 1.0 / (0.5f64.mul_add(l, sb) + offset);
    }
    Ok((
        positive("HiSIM-HV reference layout half length", reference)?,
        positive("HiSIM-HV layout half length", 2.0 * nf / reciprocal_sum)?,
    ))
}

fn corrected_phonon_base(
    model: &PreparedHiSimHvModel,
    raw: &BoundParameters,
    binned: &[Option<f64>],
) -> Result<f64, ModelError> {
    let mut mueph1 = source_binned_at_name(model, binned, "mueph1")?;
    if model_integer(model, "codfm")? == 1 && explicitly_given(raw, "nsubcdfm")? {
        // Upstream pParam NSUBC and instance NSUBCDFM both convert cm^-3
        // to m^-3 by dividing by 1e-6 before this logarithm.
        let source_concentration = instance_real(&raw.values, model.variant(), "nsubcdfm")? / 1e-6;
        let reference_concentration = source_binned_at_name(model, binned, "nsubc")? / 1e-6;
        let source_concentration = positive("HiSIM-HV DFM NSUBCDFM", source_concentration)?;
        let reference_concentration = positive("HiSIM-HV DFM NSUBC", reference_concentration)?;
        let log_difference = source_concentration.ln() - reference_concentration.ln();
        mueph1 *= model_real(model, "mphdfm")?.mul_add(log_difference, 1.0);
    }
    positive("HiSIM-HV corrected MUEPH1", mueph1)
}

fn phonon_mobility(
    model: &PreparedHiSimHvModel,
    raw: &BoundParameters,
    binned: &[Option<f64>],
    geometry: HiSimHvBinningGeometry,
    half_layout: f64,
    reference: f64,
) -> Result<f64, ModelError> {
    let lg = geometry.gate_length_m * MICROMETERS_PER_M;
    let wg = geometry.gate_width_per_finger_m * MICROMETERS_PER_M;
    let area = lg * wg;
    let mueph1 = corrected_phonon_base(model, raw, binned)?;
    let phonon = mueph1
        * (1.0 + model_real(model, "muephw")? / wg.powf(model_real(model, "muepwp")?))
        * (1.0 + model_real(model, "muephl")? / lg.powf(model_real(model, "mueplp")?))
        * (1.0 + model_real(model, "muephs")? / area.powf(model_real(model, "muepsp")?));
    if half_layout > 0.0 {
        let delta = source_binned_at_name(model, binned, "muesti1")? / CM_PER_M;
        let exponent = source_binned_at_name(model, binned, "muesti3")?;
        let weight = 1.0 / (1.0 + source_binned_at_name(model, binned, "muesti2")?);
        let actual = (delta / half_layout).powf(exponent);
        let expected = (delta / reference).powf(exponent);
        positive(
            "HiSIM-HV LOD phonon coefficient",
            phonon * (weight.mul_add(actual, 1.0)) / (weight.mul_add(expected, 1.0)),
        )
    } else {
        positive("HiSIM-HV phonon coefficient", phonon)
    }
}

/// Prepare all supported temperature-independent transport geometry terms.
///
/// This is performed only during instance preparation, so named setup fields
/// do not become per-iteration name lookup or logarithm/exponentiation costs.
pub(super) fn prepare_transport_geometry(
    model: &PreparedHiSimHvModel,
    raw: &BoundParameters,
    source: &[Option<ParameterValue>],
    geometry: HiSimHvBinningGeometry,
    effective: HiSimHvEffectiveGeometry,
    binned: &[Option<f64>],
) -> Result<HiSimHvTransportGeometry, ModelError> {
    let (reference, half_layout) = layout_half_lengths(model, source)?;
    let phonon = phonon_mobility(model, raw, binned, geometry, half_layout, reference)?;
    let lg = geometry.gate_length_m * MICROMETERS_PER_M;
    let wg = geometry.gate_width_per_finger_m * MICROMETERS_PER_M;
    let area = lg * wg;
    let surface_roughness = model_real(model, "muesr0")?
        * (1.0 + model_real(model, "muesrl")? / lg.powf(model_real(model, "mueslp")?))
        * (1.0 + model_real(model, "muesrw")? / wg.powf(model_real(model, "mueswp")?));
    let vmax0 = (1.0
        + source_binned_at_name(model, binned, "vover")? / lg.powf(model_real(model, "voverp")?))
        * (1.0 + model_real(model, "vovers")? / area.powf(model_real(model, "voversp")?));
    let ninvd0 = model_real(model, "ninvd")?
        * (1.0 + model_real(model, "ninvdw")? / wg.powf(model_real(model, "ninvdwp")?));
    let nf = instance_real(source, model.variant(), "nf")?;
    let m = instance_real(source, model.variant(), "m")?;
    let rthtemp0 =
        1.0 / nf.powf(model_real(model, "rth0nf")?) / (m * effective.channel_width_total_m)
            * (1.0 + model_real(model, "rth0w")? / wg.powf(model_real(model, "rth0wp")?));
    Ok(HiSimHvTransportGeometry {
        reference_half_layout_length_m: finite("HiSIM-HV reference layout half length", reference)?,
        half_layout_length_m: finite("HiSIM-HV layout half length", half_layout)?,
        phonon_scattering_coefficient: positive("HiSIM-HV phonon coefficient", phonon)?,
        surface_roughness_coefficient: finite("HiSIM-HV roughness coefficient", surface_roughness)?,
        saturation_velocity_geometry_factor: finite(
            "HiSIM-HV saturation velocity geometry",
            vmax0,
        )?,
        density_gradient_base: finite("HiSIM-HV density gradient geometry", ninvd0)?,
        thermal_resistance_geometry_factor: finite(
            "HiSIM-HV thermal resistance geometry",
            rthtemp0,
        )?,
        thermal_capacitance_geometry_factor_m: finite(
            "HiSIM-HV thermal capacitance geometry",
            m * effective.channel_width_total_m,
        )?,
    })
}

/// Source-derived depletion-MOS mobility and velocity terms, only when the
/// HiSIM-HV2 `CODEP=1` mode is selected.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvDepletionTransport {
    /// `depmphn0`, the temperature-dependent depletion phonon coefficient.
    pub phonon_coefficient: f64,
    /// `depmphn1`, the weighted depletion phonon coefficient.
    pub phonon_weighted_coefficient: f64,
    /// Derivative of `depmphn0` with respect to local temperature.
    pub phonon_coefficient_derivative_per_kelvin: f64,
    /// `depvmax`, saturation velocity for the depletion MOS device (m/s).
    pub saturation_velocity_m_per_s: f64,
    /// Source-model `depVmax_dT` with respect to local temperature.
    pub saturation_velocity_derivative_m_per_s_kelvin: f64,
}

/// Passive thermal one-port from native C `hsmhv[2]ld.c`.
///
/// Both the conductive heat flow and the stored heat are linear in the local
/// temperature rise. Device-generated heat (`P=Ids*Veffpower`) is a separate
/// nonlinear source and must be added by the physical current kernel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvThermalBranch {
    /// `Gth=1/max(Rth,1e-4)` in W/K, preserving the native C threshold.
    pub conductance_w_per_kelvin: f64,
    /// `Cth=CTH0*100*(M*Weff*NF)` in J/K.
    pub heat_capacity_j_per_kelvin: f64,
    /// `Ith=Gth*deltemp`, dissipated power in W.
    pub conductive_heat_w: f64,
    /// `Qth=Cth*deltemp`, integrated heat storage in J.
    pub stored_heat_j: f64,
}

/// Source-derived temperature-sensitive transport inputs for one HiSIM-HV device.
/// This is **not** a current/charge/device-state kernel.
#[derive(Clone, Debug, PartialEq)]
pub struct PreparedHiSimHvTransport {
    /// Immutable bandgap, thermal-voltage and intrinsic-density precomputation.
    pub temperature: PreparedHiSimHvTemperature,
    /// `mphn0`, the inverse phonon scattering coefficient.
    pub phonon_coefficient: f64,
    /// `mphn1`, weighted by the model `MUEPH0`.
    pub phonon_weighted_coefficient: f64,
    /// Analytic source derivative of `mphn0` with respect to temperature.
    pub phonon_coefficient_derivative_per_kelvin: f64,
    /// `vmax`, layout- and temperature-corrected saturation velocity (m/s).
    pub saturation_velocity_m_per_s: f64,
    /// Analytic `Vmax_dT`, with COTEMP and self-heating branch semantics.
    pub saturation_velocity_derivative_m_per_s_kelvin: f64,
    /// `ninvd`, temperature-scaled density-gradient coefficient.
    pub density_gradient_coefficient: f64,
    /// Analytic temperature derivative; zero in upstream `COTEMP != 2` mode.
    pub density_gradient_derivative_per_kelvin: f64,
    /// `rth`, C's geometric and temperature-corrected thermal resistance.
    pub thermal_resistance_kelvin_per_watt: f64,
    /// `powratio`, twice-smoothed power coupling coefficient in [0, 1].
    pub power_coupling_ratio: f64,
    /// Additional depletion physics input for HV2 CODEP=1, otherwise absent.
    pub depletion: Option<HiSimHvDepletionTransport>,
    /// Passive thermal branch, only present if this instance enables self-heating.
    pub thermal_branch: Option<HiSimHvThermalBranch>,
    /// Source's `SHEMAX` (K), used by the nonlinear power ceiling.
    pub(crate) maximum_self_heating_rise_kelvin: f64,
    /// The source tests *binned*, not temperature-smoothed `POWRAT==1`.
    pub(crate) exact_external_drain_power_voltage: bool,
    /// Native stamp multiplies electrical thermal-row columns by MOS `type`.
    pub(crate) polarity_sign: f64,
    /// Retains the creating prepared instance's provenance (not a per-trial
    /// identity allocation) so later model/instance mixing fails explicitly.
    pub(super) instance_identity: Arc<()>,
}

#[derive(Clone, Copy)]
struct TransportSlots {
    muetmp: u16,
    mueph0: u16,
    vmax: u16,
    vtmp: u16,
    vmaxt1: u16,
    vmaxt2: u16,
    cotemp: u16,
    ninvdt1: u16,
    ninvdt2: u16,
    rth0: u16,
    cth0: u16,
    shemax: u16,
    coselfheat: u16,
    rthtemp1: u16,
    rthtemp2: u16,
    powrat: u16,
    prattemp1: u16,
    prattemp2: u16,
    codep: Option<u16>,
    depmuetmp: Option<u16>,
    depmueph1: Option<u16>,
    depmueph0: Option<u16>,
    depvtmp: Option<u16>,
    depvmax: Option<u16>,
}

static HV1_TRANSPORT_SLOTS: OnceLock<Result<TransportSlots, ModelError>> = OnceLock::new();
static HV2_TRANSPORT_SLOTS: OnceLock<Result<TransportSlots, ModelError>> = OnceLock::new();

fn compile_transport_slots(variant: ModelVariant) -> Result<TransportSlots, ModelError> {
    let has_depletion = variant == ModelVariant::HiSimHv2;
    let optional = |name: &str| {
        if has_depletion {
            lookup_slot(variant, name).map(Some)
        } else {
            Ok(None)
        }
    };
    Ok(TransportSlots {
        muetmp: lookup_slot(variant, "muetmp")?,
        mueph0: lookup_slot(variant, "mueph0")?,
        vmax: lookup_slot(variant, "vmax")?,
        vtmp: lookup_slot(variant, "vtmp")?,
        vmaxt1: lookup_slot(variant, "vmaxt1")?,
        vmaxt2: lookup_slot(variant, "vmaxt2")?,
        cotemp: lookup_slot(variant, "cotemp")?,
        ninvdt1: lookup_slot(variant, "ninvdt1")?,
        ninvdt2: lookup_slot(variant, "ninvdt2")?,
        rth0: lookup_slot(variant, "rth0")?,
        cth0: lookup_slot(variant, "cth0")?,
        shemax: lookup_slot(variant, "shemax")?,
        coselfheat: variant
            .parameter(ParameterScope::Instance, "coselfheat")
            .ok_or_else(|| ModelError::UnknownParameter("coselfheat".to_owned()))?
            .slot,
        rthtemp1: lookup_slot(variant, "rthtemp1")?,
        rthtemp2: lookup_slot(variant, "rthtemp2")?,
        powrat: lookup_slot(variant, "powrat")?,
        prattemp1: lookup_slot(variant, "prattemp1")?,
        prattemp2: lookup_slot(variant, "prattemp2")?,
        codep: optional("codep")?,
        depmuetmp: optional("depmuetmp")?,
        depmueph1: optional("depmueph1")?,
        depmueph0: optional("depmueph0")?,
        depvtmp: optional("depvtmp")?,
        depvmax: optional("depvmax")?,
    })
}

fn transport_slots(variant: ModelVariant) -> Result<TransportSlots, ModelError> {
    compiled_hv_by_family(
        variant,
        &HV1_TRANSPORT_SLOTS,
        &HV2_TRANSPORT_SLOTS,
        compile_transport_slots,
    )
    .copied()
}

fn slot_model_integer(model: &PreparedHiSimHvModel, slot: u16) -> Result<i32, ModelError> {
    match model.get_slot(slot) {
        Some(ParameterValue::Integer(value)) => Ok(*value),
        _ => Err(ModelError::MissingDefaultDependency(slot)),
    }
}

fn slot_binned_real(instance: &PreparedHiSimHvInstance, slot: u16) -> Result<f64, ModelError> {
    instance
        .binned_model_slot(slot)
        .ok_or(ModelError::MissingDefaultDependency(slot))
}

fn source_temperature_factor(ratio: f64, vtmp: f64) -> Result<(f64, f64), ModelError> {
    let factor = 0.1f64.mul_add(
        ratio * ratio,
        vtmp.mul_add(-(1.0 - ratio), 0.4f64.mul_add(ratio, 1.8)),
    );
    let slope_per_ratio = 0.2f64.mul_add(ratio, 0.4 + vtmp);
    Ok((
        positive("HiSIM-HV velocity temperature denominator", factor)?,
        finite(
            "HiSIM-HV velocity temperature denominator derivative",
            slope_per_ratio,
        )?,
    ))
}

fn depletion_transport(
    model: &PreparedHiSimHvModel,
    temperature: PreparedHiSimHvTemperature,
    slots: TransportSlots,
) -> Result<Option<HiSimHvDepletionTransport>, ModelError> {
    let Some(codep) = slots.codep else {
        return Ok(None);
    };
    if slot_model_integer(model, codep)? != 1 {
        return Ok(None);
    }
    let dependency = |slot: Option<u16>| {
        slot.ok_or_else(|| {
            ModelError::CorruptSourceCatalog("missing HV2 depletion transport slot".to_owned())
        })
    };
    let exponent = source_real_at(model, dependency(slots.depmuetmp)?)?;
    let scattering = positive(
        "HiSIM-HV depletion phonon base",
        source_real_at(model, dependency(slots.depmueph1)?)?,
    )?;
    let weighted = source_real_at(model, dependency(slots.depmueph0)?)?;
    let ratio = temperature.temperature_ratio;
    let phonon = positive(
        "HiSIM-HV depletion phonon coefficient",
        ratio.powf(exponent) / scattering,
    )?;
    let derivative = finite(
        "HiSIM-HV depletion phonon derivative",
        exponent * ratio.powf(exponent - 1.0) / temperature.nominal_temperature_kelvin / scattering,
    )?;
    let depvtmp = source_real_at(model, dependency(slots.depvtmp)?)?;
    let (velocity_denominator, velocity_slope_per_ratio) =
        source_temperature_factor(ratio, depvtmp)?;
    let nominal_velocity = source_real_at(model, dependency(slots.depvmax)?)? / CM_PER_M;
    let vmax = finite(
        "HiSIM-HV depletion saturation velocity",
        nominal_velocity / velocity_denominator,
    )?;
    let vmax_derivative = finite(
        "HiSIM-HV depletion velocity derivative",
        -nominal_velocity * velocity_slope_per_ratio
            / (velocity_denominator
                * velocity_denominator
                * temperature.nominal_temperature_kelvin),
    )?;
    Ok(Some(HiSimHvDepletionTransport {
        phonon_coefficient: phonon,
        phonon_weighted_coefficient: finite(
            "HiSIM-HV depletion weighted phonon",
            phonon * weighted,
        )?,
        phonon_coefficient_derivative_per_kelvin: derivative,
        saturation_velocity_m_per_s: vmax,
        saturation_velocity_derivative_m_per_s_kelvin: vmax_derivative,
    }))
}

fn saturation_velocity(
    model: &PreparedHiSimHvModel,
    instance: &PreparedHiSimHvInstance,
    temperature: PreparedHiSimHvTemperature,
    slots: TransportSlots,
) -> Result<(f64, f64), ModelError> {
    let ratio = temperature.temperature_ratio;
    let nominal = temperature.nominal_temperature_kelvin;
    let (denominator, slope) =
        source_temperature_factor(ratio, slot_binned_real(instance, slots.vtmp)?)?;
    let include_self_heating = slot_model_integer(model, slots.cotemp)? == 2;
    let actual_temperature = if include_self_heating {
        temperature.device_temperature_kelvin
    } else {
        temperature.ambient_device_temperature_kelvin
    };
    let diff = actual_temperature - nominal;
    let diff2 = nominal.mul_add(-nominal, actual_temperature * actual_temperature);
    let term1 = source_real_at(model, slots.vmaxt1)?;
    let term2 = source_real_at(model, slots.vmaxt2)?;
    let numerator = term2.mul_add(diff2, term1.mul_add(diff, 1.0));
    let base = instance
        .transport_geometry()
        .saturation_velocity_geometry_factor
        * slot_binned_real(instance, slots.vmax)?
        / CM_PER_M;
    let vmax = finite(
        "HiSIM-HV saturation velocity",
        base * numerator / denominator,
    )?;
    let numerator_derivative = if include_self_heating {
        (2.0 * actual_temperature).mul_add(term2, term1)
    } else {
        0.0
    };
    let derivative = finite(
        "HiSIM-HV saturation velocity derivative",
        base * (numerator_derivative.mul_add(denominator, -numerator * slope / nominal))
            / (denominator * denominator),
    )?;
    Ok((vmax, derivative))
}

fn temperature_dependent_density(
    model: &PreparedHiSimHvModel,
    instance: &PreparedHiSimHvInstance,
    temperature: PreparedHiSimHvTemperature,
    slots: TransportSlots,
) -> Result<(f64, f64), ModelError> {
    let include_self_heating = slot_model_integer(model, slots.cotemp)? == 2;
    let actual_t = if include_self_heating {
        temperature.device_temperature_kelvin
    } else {
        temperature.ambient_device_temperature_kelvin
    };
    let nominal = temperature.nominal_temperature_kelvin;
    let diff = actual_t - nominal;
    let diff2 = nominal.mul_add(-nominal, actual_t * actual_t);
    let coeff1 = source_real_at(model, slots.ninvdt1)?;
    let coeff2 = source_real_at(model, slots.ninvdt2)?;
    let base = instance.transport_geometry().density_gradient_base;
    let coefficient = finite(
        "HiSIM-HV temperature density gradient",
        base * coeff2.mul_add(diff2, coeff1.mul_add(diff, 1.0)),
    )?;
    let derivative = if include_self_heating {
        finite(
            "HiSIM-HV density gradient derivative",
            base * (2.0 * actual_t).mul_add(coeff2, coeff1),
        )?
    } else {
        0.0
    };
    Ok((coefficient, derivative))
}

fn thermal_and_power(
    model: &PreparedHiSimHvModel,
    instance: &PreparedHiSimHvInstance,
    temperature: PreparedHiSimHvTemperature,
    slots: TransportSlots,
) -> Result<(f64, f64), ModelError> {
    // Both versions use TTEMP0 (circuit+DTEMP), not the local heated TTEMP.
    let nominal = temperature.nominal_temperature_kelvin;
    let ambient = temperature.ambient_device_temperature_kelvin;
    let diff = ambient - nominal;
    let diff2 = nominal.mul_add(-nominal, ambient * ambient);
    let resistance = finite(
        "HiSIM-HV temperature thermal resistance",
        instance
            .transport_geometry()
            .thermal_resistance_geometry_factor
            * source_real_at(model, slots.rthtemp2)?.mul_add(
                diff2,
                source_real_at(model, slots.rthtemp1)?
                    .mul_add(diff, slot_binned_real(instance, slots.rth0)? / CM_PER_M),
            ),
    )?;
    let power = source_real_at(model, slots.prattemp2)?.mul_add(
        diff2,
        source_real_at(model, slots.prattemp1)?
            .mul_add(diff, slot_binned_real(instance, slots.powrat)?),
    );
    // Fn_SL(y,x,xmin=0,delta=.05) is exactly max(x-.05, 0).
    // Fn_SU(y,x,xmax=1,delta=.05) uses a source smooth-upper bound.
    let positive_power = (power - 0.05).max(0.0);
    let delta = 0.95 - positive_power;
    let ratio = 1.0 - f64::midpoint(delta, delta.mul_add(delta, 0.2).sqrt());
    Ok((resistance, finite("HiSIM-HV power coupling", ratio)?))
}

fn passive_thermal_branch(
    model: &PreparedHiSimHvModel,
    instance: &PreparedHiSimHvInstance,
    slots: TransportSlots,
    enabled: bool,
    thermal_resistance: f64,
    local_temperature_rise: f64,
) -> Result<Option<HiSimHvThermalBranch>, ModelError> {
    if !enabled {
        return Ok(None);
    }
    // hsmhv[2]ld.c defines C_RTH_MIN=1e-4, then uses 1/Rth or 1/C_RTH_MIN.
    let conductance = finite(
        "HiSIM-HV passive thermal conductance",
        1.0 / thermal_resistance.max(RTH_MIN),
    )?;
    // hsmhv[2]set.c: modelMKS.cth0=cth0*C_m2cm, with C_m2cm=100.
    // hsmhv[2]temp.c: Cth=modelMKS.cth0 * M * Weff * NF.
    let heat_capacity = finite(
        "HiSIM-HV passive heat capacity",
        source_real_at(model, slots.cth0)?
            * CM_PER_M
            * instance
                .transport_geometry()
                .thermal_capacitance_geometry_factor_m,
    )?;
    Ok(Some(HiSimHvThermalBranch {
        conductance_w_per_kelvin: conductance,
        heat_capacity_j_per_kelvin: heat_capacity,
        conductive_heat_w: finite(
            "HiSIM-HV passive thermal heat flow",
            conductance * local_temperature_rise,
        )?,
        stored_heat_j: finite(
            "HiSIM-HV stored thermal energy",
            heat_capacity * local_temperature_rise,
        )?,
    }))
}

/// Evaluate the shared temperature-dependent phonon, velocity, density-gradient
/// and thermal terms using immutable model, instance and ambient/thermal input.
///
/// # Errors
/// Returns explicit errors for unavailable data, inconsistent geometry or
/// nonphysical temperature-law domains. Numerical compact-model current and
/// charge kernels remain absent.
pub fn prepare_hisim_hv_transport(
    model: &PreparedHiSimHvModel,
    instance: &PreparedHiSimHvInstance,
    circuit_temperature_kelvin: f64,
    local_self_heating_kelvin: f64,
) -> Result<PreparedHiSimHvTransport, ModelError> {
    model.validate_instance(instance)?;
    let slots = transport_slots(model.variant())?;
    let self_heating_enabled = match instance.parameters().get_slot(slots.coselfheat) {
        Some(ParameterValue::Integer(value)) => *value > 0,
        _ => return Err(ModelError::MissingDefaultDependency(slots.coselfheat)),
    };
    if !self_heating_enabled && local_self_heating_kelvin != 0.0 {
        return Err(ModelError::InvalidPhysicalParameter(
            "HiSIM-HV local temperature rise requires COSELFHEAT",
        ));
    }
    let temperature = super::prepare_hisim_hv_temperature(
        model,
        instance,
        circuit_temperature_kelvin,
        local_self_heating_kelvin,
    )?;
    let exponent = slot_binned_real(instance, slots.muetmp)?;
    let base = positive(
        "HiSIM-HV phonon scattering base",
        instance.transport_geometry().phonon_scattering_coefficient,
    )?;
    let ratio = temperature.temperature_ratio;
    let phonon = finite(
        "HiSIM-HV temperature phonon coefficient",
        ratio.powf(exponent) / base,
    )?;
    let phonon_derivative = finite(
        "HiSIM-HV phonon coefficient derivative",
        exponent * ratio.powf(exponent - 1.0) / temperature.nominal_temperature_kelvin / base,
    )?;
    let weighted_phonon = finite(
        "HiSIM-HV weighted phonon coefficient",
        phonon * source_real_at(model, slots.mueph0)?,
    )?;
    let (velocity, velocity_derivative) = saturation_velocity(model, instance, temperature, slots)?;
    let (density, density_derivative) =
        temperature_dependent_density(model, instance, temperature, slots)?;
    let (thermal_resistance, power_ratio) = thermal_and_power(model, instance, temperature, slots)?;
    let maximum_self_heating_rise_kelvin = positive(
        "HiSIM-HV maximum self-heating rise SHEMAX",
        source_real_at(model, slots.shemax)?,
    )?;
    let exact_external_drain_power_voltage =
        slot_binned_real(instance, slots.powrat)?.to_bits() == 1.0f64.to_bits();
    let depletion = depletion_transport(model, temperature, slots)?;
    let thermal_branch = passive_thermal_branch(
        model,
        instance,
        slots,
        self_heating_enabled,
        thermal_resistance,
        local_self_heating_kelvin,
    )?;
    Ok(PreparedHiSimHvTransport {
        temperature,
        phonon_coefficient: phonon,
        phonon_weighted_coefficient: weighted_phonon,
        phonon_coefficient_derivative_per_kelvin: phonon_derivative,
        saturation_velocity_m_per_s: velocity,
        saturation_velocity_derivative_m_per_s_kelvin: velocity_derivative,
        density_gradient_coefficient: density,
        density_gradient_derivative_per_kelvin: density_derivative,
        thermal_resistance_kelvin_per_watt: thermal_resistance,
        power_coupling_ratio: power_ratio,
        depletion,
        thermal_branch,
        maximum_self_heating_rise_kelvin,
        exact_external_drain_power_voltage,
        polarity_sign: match model.polarity() {
            crate::MosPolarity::Nmos => 1.0,
            crate::MosPolarity::Pmos => -1.0,
        },
        instance_identity: Arc::clone(&instance.identity),
    })
}
