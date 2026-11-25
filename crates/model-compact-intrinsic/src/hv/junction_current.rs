//! Physical substrate/source and substrate/drain junction diode current laws
//! from HiSIM-HV1 `hsmhvtemp_eval.h` + `hsmhveval.c` and HiSIM-HV2
//! `hsmhv2temp_eval_dio.h` + `hsmhv2eval_dio.c` in pinned ngspice 47.
//!
//! Currents, conductances and self-heating derivatives are exact separate
//! junction components. They do not approximate intrinsic channel Ids.

use std::sync::OnceLock;

use super::{
    PreparedHiSimHvInstance, PreparedHiSimHvModel, PreparedHiSimHvTransport, compiled_hv_by_family,
    hv_finite_or_error, instance_real_at, junction::HiSimHvJunctionBias, lookup_slot,
    source_real_at,
};
use crate::{ModelError, ModelVariant, ParameterScope};

const ELECTRON_CHARGE_C: f64 = 1.602_191_8e-19;
const BOLTZMANN_J_PER_K: f64 = 1.380_622_6e-23;
const JUNCTION_SMALL: f64 = 1e-50;
const EXP_THR: f64 = 34.0;

/// Source-defined junction diode output, *before* additional solver `gmin`.
///
/// The temperature derivative is zero unless the native electrothermal
/// junction calculation is active (thermal node with nonzero binned RTH0).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvJunctionCurrent {
    pub current_a: f64,
    pub conductance_siemens: f64,
    pub temperature_derivative_a_per_kelvin: f64,
}

/// Original C substrate/drain and substrate/source diode current outputs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvJunctionCurrents {
    pub bulk_source: HiSimHvJunctionCurrent,
    pub bulk_drain: HiSimHvJunctionCurrent,
}

#[derive(Clone, Copy)]
struct DiodeSlots {
    js0: u16,
    js0sw: u16,
    ideality: u16,
    sidewall_ideality: u16,
    xti: u16,
    xti2: u16,
    transition_current: u16,
    reverse_exponent: u16,
    second_reverse_exponent: u16,
    reverse_amplitude: u16,
    back_current: u16,
    temperature_exponent: u16,
    reverse_linear: u16,
}

#[derive(Clone, Copy)]
struct FamilySlots {
    source: DiodeSlots,
    drain: DiodeSlots,
    bgtmp1: u16,
    bgtmp2: u16,
    rth0: u16,
    area_source: u16,
    area_drain: u16,
    perimeter_source: u16,
    perimeter_drain: u16,
    multiplicity: u16,
}

static HV1_SLOTS: OnceLock<Result<FamilySlots, ModelError>> = OnceLock::new();
static HV2_SLOTS: OnceLock<Result<FamilySlots, ModelError>> = OnceLock::new();

fn compile_slots(variant: ModelVariant) -> Result<FamilySlots, ModelError> {
    let slot = |name| lookup_slot(variant, name);
    let diode = |params: [&'static str; 13]| {
        Ok(DiodeSlots {
            js0: slot(params[0])?,
            js0sw: slot(params[1])?,
            ideality: slot(params[2])?,
            sidewall_ideality: slot(params[3])?,
            xti: slot(params[4])?,
            xti2: slot(params[5])?,
            transition_current: slot(params[6])?,
            reverse_exponent: slot(params[7])?,
            second_reverse_exponent: slot(params[8])?,
            reverse_amplitude: slot(params[9])?,
            back_current: slot(params[10])?,
            temperature_exponent: slot(params[11])?,
            reverse_linear: slot(params[12])?,
        })
    };
    let (source, drain) = if variant == ModelVariant::HiSimHv1 {
        let common = diode([
            "js0", "js0sw", "nj", "njsw", "xti", "xti2", "vdiffj", "cvb", "cvbk", "cisb", "cisbk",
            "ctemp", "divx",
        ])?;
        (common, common)
    } else {
        (
            diode([
                "js0s", "js0sws", "njs", "njsws", "xtis", "xti2s", "vdiffjs", "cvbs", "cvbs",
                "cisbs", "cisbks", "ctemps", "divxs",
            ])?,
            diode([
                "js0d", "js0swd", "njd", "njswd", "xtid", "xti2d", "vdiffjd", "cvbd", "cvbd",
                "cisbd", "cisbkd", "ctempd", "divxd",
            ])?,
        )
    };
    let inst = |name: &str| {
        variant
            .parameter(ParameterScope::Instance, name)
            .map(|spec| spec.slot)
            .ok_or_else(|| ModelError::UnknownParameter(name.to_owned()))
    };
    Ok(FamilySlots {
        source,
        drain,
        bgtmp1: slot("bgtmp1")?,
        bgtmp2: slot("bgtmp2")?,
        rth0: slot("rth0")?,
        area_source: inst("as")?,
        area_drain: inst("ad")?,
        perimeter_source: inst("ps")?,
        perimeter_drain: inst("pd")?,
        multiplicity: inst("m")?,
    })
}

fn slots(variant: ModelVariant) -> Result<FamilySlots, ModelError> {
    compiled_hv_by_family(variant, &HV1_SLOTS, &HV2_SLOTS, compile_slots).copied()
}

fn binned(instance: &PreparedHiSimHvInstance, slot: u16) -> Result<f64, ModelError> {
    instance
        .binned_model_slot(slot)
        .ok_or(ModelError::MissingDefaultDependency(slot))
}

const fn finite(value: f64) -> Result<f64, ModelError> {
    hv_finite_or_error(value, "HiSIM-HV junction diode expression must be finite")
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct DiodeTemperature {
    nominal: f64,
    actual: f64,
    ratio: f64,
    beta: f64,
    beta_nominal: f64,
    beta_derivative: f64,
    bandgap: f64,
    nominal_bandgap: f64,
    bandgap_derivative: f64,
    active: bool,
}

fn diode_temperature(
    instance: &PreparedHiSimHvInstance,
    transport: &PreparedHiSimHvTransport,
    family_slots: FamilySlots,
) -> Result<DiodeTemperature, ModelError> {
    let prepared = transport.temperature;
    let active = transport.thermal_branch.is_some() && binned(instance, family_slots.rth0)? != 0.0;
    let actual = if active {
        prepared.device_temperature_kelvin
    } else {
        prepared.ambient_device_temperature_kelvin
    };
    let nominal = prepared.nominal_temperature_kelvin;
    let ratio = finite(actual / nominal)?;
    let beta = finite(ELECTRON_CHARGE_C / (BOLTZMANN_J_PER_K * actual))?;
    let dt = actual - nominal;
    let dt2 = nominal.mul_add(-nominal, actual * actual);
    let bgtmp1 = binned(instance, family_slots.bgtmp1)?;
    let bgtmp2 = binned(instance, family_slots.bgtmp2)?;
    let bandgap = finite(f64::mul_add(
        bgtmp2,
        -dt2,
        f64::mul_add(bgtmp1, -dt, prepared.nominal_bandgap_ev),
    ))?;
    let bandgap_derivative = if active {
        finite(f64::mul_add(2.0 * actual, -bgtmp2, -bgtmp1))?
    } else {
        0.0
    };
    if !actual.is_finite() || actual <= 0.0 || ratio <= 0.0 {
        return Err(ModelError::InvalidPhysicalParameter(
            "HiSIM-HV junction diode temperature",
        ));
    }
    Ok(DiodeTemperature {
        nominal,
        actual,
        ratio,
        beta,
        beta_nominal: prepared.nominal_inverse_thermal_voltage,
        beta_derivative: if active { -beta / actual } else { 0.0 },
        bandgap,
        nominal_bandgap: prepared.nominal_bandgap_ev,
        bandgap_derivative,
        active,
    })
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct DiodeSidePrepared {
    saturation: f64,
    saturation_derivative: f64,
    secondary_saturation: f64,
    secondary_derivative: f64,
    reverse_amplitude: f64,
    reverse_amplitude_derivative: f64,
    back_current: f64,
    reverse_exponent: f64,
    second_reverse_exponent: f64,
    nvtm_inverse: f64,
    nvtm_inverse_derivative: f64,
    transition_voltage: f64,
    transition_derivative: f64,
    transition_exponential: f64,
    transition_exp_derivative: f64,
    linear_amplitude: f64,
}

#[derive(Clone, Copy)]
struct JunctionSaturation {
    primary: f64,
    secondary: f64,
    primary_derivative: f64,
    secondary_derivative: f64,
    ideality: f64,
}

fn junction_saturation(
    model: &PreparedHiSimHvModel,
    instance: &PreparedHiSimHvInstance,
    slots: DiodeSlots,
    thermal: DiodeTemperature,
    area: f64,
    perimeter: f64,
) -> Result<JunctionSaturation, ModelError> {
    let binn = |slot| binned(instance, slot);
    let real = |slot| source_real_at(model, slot);
    let n = binn(slots.ideality)?;
    let nsw = real(slots.sidewall_ideality)?;
    if n <= 0.0 || nsw <= 0.0 || !n.is_finite() || !nsw.is_finite() {
        return Err(ModelError::InvalidPhysicalParameter(
            "HiSIM-HV diode ideality",
        ));
    }
    let log_ratio = thermal.ratio.ln();
    let base_exp = thermal.bandgap.mul_add(
        -thermal.beta,
        thermal.nominal_bandgap * thermal.beta_nominal,
    );
    let beta_component = if thermal.active {
        thermal
            .bandgap_derivative
            .mul_add(-thermal.beta, -thermal.bandgap * thermal.beta_derivative)
    } else {
        0.0
    };
    let xti = real(slots.xti)?;
    let xti2 = real(slots.xti2)?;
    let current_density = |reference: f64, ideality: f64, exponent: f64| {
        finite(reference * (exponent.mul_add(log_ratio, base_exp) / ideality).exp())
    };
    let area_reference = binn(slots.js0)?;
    let perimeter_reference = binn(slots.js0sw)?;
    let area_primary = current_density(area_reference, n, xti)?;
    let perimeter_primary = current_density(perimeter_reference, nsw, xti)?;
    let area_secondary = current_density(area_reference, n, xti2)?;
    let perimeter_secondary = current_density(perimeter_reference, nsw, xti2)?;
    let t1 = if thermal.active {
        (beta_component + xti / thermal.actual) / n
    } else {
        0.0
    };
    let ts1 = if thermal.active {
        (beta_component + xti / thermal.actual) / nsw
    } else {
        0.0
    };
    let t2 = if thermal.active {
        (beta_component + xti2 / thermal.actual) / n
    } else {
        0.0
    };
    let ts2 = if thermal.active {
        (beta_component + xti2 / thermal.actual) / nsw
    } else {
        0.0
    };
    Ok(JunctionSaturation {
        primary: finite(perimeter.mul_add(perimeter_primary, area * area_primary))?,
        secondary: finite(perimeter.mul_add(perimeter_secondary, area * area_secondary))?,
        primary_derivative: finite(
            perimeter.mul_add(perimeter_primary * ts1, area * area_primary * t1),
        )?,
        secondary_derivative: finite(
            perimeter.mul_add(perimeter_secondary * ts2, area * area_secondary * t2),
        )?,
        ideality: n,
    })
}

fn prepare_diode_side(
    model: &PreparedHiSimHvModel,
    instance: &PreparedHiSimHvInstance,
    slots: DiodeSlots,
    thermal: DiodeTemperature,
    area: f64,
    perimeter: f64,
) -> Result<DiodeSidePrepared, ModelError> {
    let binn = |slot| binned(instance, slot);
    let real = |slot| source_real_at(model, slot);
    let saturation_state = junction_saturation(model, instance, slots, thermal, area, perimeter)?;
    let JunctionSaturation {
        primary: saturation,
        secondary: secondary_saturation,
        primary_derivative: saturation_derivative,
        secondary_derivative,
        ideality: n,
    } = saturation_state;
    let diff_current = binn(slots.transition_current)?;
    let t0 = thermal.ratio * thermal.ratio;
    let t0_derivative = if thermal.active {
        2.0 * thermal.ratio / thermal.nominal
    } else {
        0.0
    };
    let t2 = saturation + JUNCTION_SMALL;
    let trans_input = diff_current * t0 / t2;
    let transition_voltage = finite(n / thermal.beta * trans_input.ln_1p())?;
    let transition_derivative = if thermal.active {
        finite(
            (n / thermal.beta * diff_current / (trans_input + 1.0)).mul_add(
                (t0 / t2 / t2).mul_add(-saturation_derivative, t0_derivative / t2),
                -thermal.beta_derivative / thermal.beta * transition_voltage,
            ),
        )?
    } else {
        0.0
    };
    let nvtm_inverse = finite(thermal.beta / n)?;
    let nvtm_inverse_derivative = if thermal.active {
        thermal.beta_derivative / n
    } else {
        0.0
    };
    let transition_exponential = finite((transition_voltage * nvtm_inverse).exp())?;
    let transition_exp_derivative = if thermal.active {
        finite(
            transition_exponential
                * f64::mul_add(
                    transition_voltage,
                    nvtm_inverse_derivative,
                    transition_derivative * nvtm_inverse,
                ),
        )?
    } else {
        0.0
    };
    let ctemp = real(slots.temperature_exponent)?;
    let exptemp = finite(((thermal.ratio - 1.0) * ctemp).exp())?;
    let exptemp_derivative = if thermal.active {
        ctemp / thermal.nominal * exptemp
    } else {
        0.0
    };
    let reverse_amplitude = real(slots.reverse_amplitude)?;
    let reverse_amplitude_derivative = if thermal.active {
        exptemp_derivative * reverse_amplitude
    } else {
        0.0
    };
    let linear_amplitude = real(slots.reverse_linear)?;
    Ok(DiodeSidePrepared {
        saturation,
        saturation_derivative,
        secondary_saturation,
        secondary_derivative,
        reverse_amplitude: finite(reverse_amplitude * exptemp)?,
        reverse_amplitude_derivative: finite(reverse_amplitude_derivative)?,
        back_current: binn(slots.back_current)?,
        reverse_exponent: real(slots.reverse_exponent)?,
        second_reverse_exponent: real(slots.second_reverse_exponent)?,
        nvtm_inverse,
        nvtm_inverse_derivative,
        transition_voltage,
        transition_derivative,
        transition_exponential,
        transition_exp_derivative,
        linear_amplitude,
    })
}

fn exp_reverse(x: f64, clip_negative: bool) -> Result<f64, ModelError> {
    if clip_negative && x < -EXP_THR {
        Ok(0.0)
    } else {
        finite(x.exp())
    }
}

fn forward_diode_terms(
    prepared: DiodeSidePrepared,
    bias: f64,
    family: ModelVariant,
    reverse_temp_factor: f64,
) -> Result<(f64, f64, f64), ModelError> {
    let inverse = prepared.nvtm_inverse;
    let inverse_derivative = prepared.nvtm_inverse_derivative;
    let result = if bias < prepared.transition_voltage {
        let argument = bias * inverse;
        let threshold = if family == ModelVariant::HiSimHv1 {
            -EXP_THR
        } else {
            -3.0 * EXP_THR
        };
        let forward_exp = if argument < threshold {
            0.0
        } else {
            finite(argument.exp())?
        };
        let forward_temperature_slope = forward_exp * argument * reverse_temp_factor;
        (
            prepared.saturation * (forward_exp - 1.0),
            prepared.saturation * forward_exp * inverse,
            prepared.saturation_derivative.mul_add(
                forward_exp - 1.0,
                prepared.saturation * forward_temperature_slope,
            ),
        )
    } else {
        let forward_exp = prepared.transition_exponential;
        let tangent = prepared.saturation * inverse * forward_exp;
        let tangent_dt = (prepared.saturation * inverse).mul_add(
            prepared.transition_exp_derivative,
            (prepared.saturation * inverse_derivative).mul_add(
                forward_exp,
                prepared.saturation_derivative * inverse * forward_exp,
            ),
        );
        let excess = bias - prepared.transition_voltage;
        (
            tangent.mul_add(excess, prepared.saturation * (forward_exp - 1.0)),
            tangent,
            tangent.mul_add(
                -prepared.transition_derivative,
                tangent_dt.mul_add(
                    excess,
                    prepared.saturation.mul_add(
                        prepared.transition_exp_derivative,
                        prepared.saturation_derivative * (forward_exp - 1.0),
                    ),
                ),
            ),
        )
    };
    Ok(result)
}

fn evaluate_diode_side(
    prepared: DiodeSidePrepared,
    bias: f64,
    thermal: DiodeTemperature,
    family: ModelVariant,
    is_source: bool,
    multiplicity: f64,
) -> Result<HiSimHvJunctionCurrent, ModelError> {
    let inverse = prepared.nvtm_inverse;
    let cvb = prepared.reverse_exponent;
    let cvbk = prepared.second_reverse_exponent;
    let clip_reverse = is_source && family == ModelVariant::HiSimHv1;
    let reverse_argument = -bias * cvb * inverse;
    let back_argument = -bias * cvbk * inverse;
    let reverse_exp = exp_reverse(reverse_argument, clip_reverse)?;
    let back_exp = if family == ModelVariant::HiSimHv2 {
        reverse_exp
    } else {
        exp_reverse(back_argument, clip_reverse)?
    };
    let reverse_conductance = -reverse_exp * cvb * inverse;
    let back_conductance = -back_exp * cvbk * inverse;
    let reverse_temp_factor = if thermal.active {
        thermal.beta_derivative / thermal.beta
    } else {
        0.0
    };
    let reverse_temperature_slope = reverse_exp * reverse_argument * reverse_temp_factor;
    let back_temperature_slope = back_exp * back_argument * reverse_temp_factor;
    let secondary_reverse_current = prepared.secondary_saturation * prepared.reverse_amplitude;
    let secondary_reverse_temperature_slope = prepared.secondary_saturation.mul_add(
        prepared.reverse_amplitude_derivative,
        prepared.secondary_derivative * prepared.reverse_amplitude,
    );
    let back = prepared.back_current;
    let linear = prepared.linear_amplitude * prepared.secondary_saturation;
    let linear_dt = prepared.linear_amplitude * prepared.secondary_derivative;
    let (forward_current, forward_conductance, forward_derivative) =
        forward_diode_terms(prepared, bias, family, reverse_temp_factor)?;
    let current = linear.mul_add(
        bias,
        back.mul_add(
            back_exp - 1.0,
            secondary_reverse_current.mul_add(reverse_exp - 1.0, forward_current),
        ),
    );
    let conductance = back.mul_add(
        back_conductance,
        secondary_reverse_current.mul_add(reverse_conductance, forward_conductance),
    ) + linear;
    let derivative = linear_dt.mul_add(
        bias,
        back.mul_add(
            back_temperature_slope,
            secondary_reverse_temperature_slope.mul_add(
                reverse_exp - 1.0,
                secondary_reverse_current.mul_add(reverse_temperature_slope, forward_derivative),
            ),
        ),
    );
    Ok(HiSimHvJunctionCurrent {
        current_a: finite(current * multiplicity)?,
        conductance_siemens: finite(conductance * multiplicity)?,
        temperature_derivative_a_per_kelvin: finite(derivative * multiplicity)?,
    })
}

/// Precomputed diode coefficients at one thermal candidate.
///
/// Reuse across Newton trial voltages without recalculating saturation,
/// bandgap, leakage or tangent prelude coefficients.
///
/// The source and drain retain independent HV2 parameters. All nonlinear
/// current and Jacobian calculations remain source-faithful when evaluated.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreparedHiSimHvJunctionDiodes {
    source: DiodeSidePrepared,
    drain: DiodeSidePrepared,
    temperature: DiodeTemperature,
    family: ModelVariant,
    multiplicity: f64,
}

impl PreparedHiSimHvJunctionDiodes {
    /// Evaluate original source/drain diode I, `dI/dV`, and local `dI/dT`
    /// from the prepared temperature state, with no model or instance lookup.
    ///
    /// # Errors
    /// Trial biases and resulting current/derivatives must be finite.
    pub fn evaluate(
        &self,
        bias: HiSimHvJunctionBias,
    ) -> Result<HiSimHvJunctionCurrents, ModelError> {
        Ok(HiSimHvJunctionCurrents {
            bulk_source: evaluate_diode_side(
                self.source,
                finite(bias.bulk_source_v)?,
                self.temperature,
                self.family,
                true,
                self.multiplicity,
            )?,
            bulk_drain: evaluate_diode_side(
                self.drain,
                finite(bias.bulk_drain_v)?,
                self.temperature,
                self.family,
                false,
                self.multiplicity,
            )?,
        })
    }
}

/// Prepare physical diode currents at one device temperature.
///
/// This avoids per-bias parameter resolution and temperature exponentials.
///
/// Original C junction current depends on area and full junction perimeter
/// (not the charge model's split sidewall perimeter); HV1 shares the source
/// and drain ideality family, while HV2 defines independent sides. Additional
/// solver `gmin` is intentionally outside these physical diode equations.
///
/// # Errors
/// Rejects incompatible prepared models/instances, invalid physical diode
/// parameters, and nonfinite thermal expressions.
pub fn prepare_hisim_hv_junction_diodes(
    model: &PreparedHiSimHvModel,
    instance: &PreparedHiSimHvInstance,
    transport: &PreparedHiSimHvTransport,
) -> Result<PreparedHiSimHvJunctionDiodes, ModelError> {
    model.validate_instance(instance)?;
    instance.validate_transport(transport)?;
    let slots = slots(model.variant())?;
    let thermal = diode_temperature(instance, transport, slots)?;
    let multiplicity = finite(instance_real_at(instance, slots.multiplicity)?)?;
    let side = |family_slots: DiodeSlots, area_slot, perimeter_slot| {
        let area = finite(instance_real_at(instance, area_slot)?)?;
        let perimeter = finite(instance_real_at(instance, perimeter_slot)?)?;
        prepare_diode_side(model, instance, family_slots, thermal, area, perimeter)
    };
    Ok(PreparedHiSimHvJunctionDiodes {
        source: side(slots.source, slots.area_source, slots.perimeter_source)?,
        drain: side(slots.drain, slots.area_drain, slots.perimeter_drain)?,
        temperature: thermal,
        family: model.variant(),
        multiplicity,
    })
}
