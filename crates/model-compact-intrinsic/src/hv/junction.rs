//! Source/drain junction depletion charge, capacitance and local thermal
//! charge derivative from the complete HiSIM-HV1 and HiSIM-HV2 C junction
//! charge branches (`hsmhveval.c`, `hsmhv2eval_dio.c`).
//!
//! This covers the depletion *charge* component, not the independent junction
//! diode I/V laws. The real MOS channel I/Q law must supply its own terms.

use std::sync::OnceLock;

use super::{
    PreparedHiSimHvInstance, PreparedHiSimHvModel, PreparedHiSimHvTransport, compiled_hv_by_family,
    hv_finite_or_error, instance_real_at, lookup_slot, source_real_at,
};
use crate::{ModelError, ModelVariant, ParameterScope};

/// One original bulk/source or bulk/drain depletion-charge contribution.
///
/// Values include the native instance multiplicity `M`; capacitance is the
/// derivative of charge with respect to that junction's bulk voltage.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvJunctionCharge {
    pub charge_coulomb: f64,
    pub capacitance_farad: f64,
    pub charge_temperature_derivative_coulomb_per_kelvin: f64,
}

/// The complete **depletion** junction-charge outputs for both junctions.
/// The gate-channel charge and diode currents are separate physical branches.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvJunctionCharges {
    pub bulk_source: HiSimHvJunctionCharge,
    pub bulk_drain: HiSimHvJunctionCharge,
}

/// Junction voltages in source `vbs_jct` and drain `vbd_jct` coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvJunctionBias {
    pub bulk_source_v: f64,
    pub bulk_drain_v: f64,
}

#[derive(Clone, Copy)]
struct ComponentSlots {
    zero_bias_capacitance: u16,
    grading_coefficient: u16,
    built_in_potential: u16,
    temperature_coefficient: u16,
}

#[derive(Clone, Copy)]
struct JunctionSlots {
    source: [ComponentSlots; 3],
    drain: [ComponentSlots; 3],
    area_source: u16,
    perimeter_source: u16,
    area_drain: u16,
    perimeter_drain: u16,
    multiplicity: u16,
    rth0: u16,
}

static HV1_SLOTS: OnceLock<Result<JunctionSlots, ModelError>> = OnceLock::new();
static HV2_SLOTS: OnceLock<Result<JunctionSlots, ModelError>> = OnceLock::new();

fn compile_slots(variant: ModelVariant) -> Result<JunctionSlots, ModelError> {
    let component = |cap: &str, exponent: &str, potential: &str, temp: &str| {
        Ok(ComponentSlots {
            zero_bias_capacitance: lookup_slot(variant, cap)?,
            grading_coefficient: lookup_slot(variant, exponent)?,
            built_in_potential: lookup_slot(variant, potential)?,
            temperature_coefficient: lookup_slot(variant, temp)?,
        })
    };
    let (source, drain) = if variant == ModelVariant::HiSimHv1 {
        let src = [
            component("cj", "mj", "pb", "tcjbs")?,
            component("cjsw", "mjsw", "pbsw", "tcjbssw")?,
            component("cjswg", "mjswg", "pbswg", "tcjbsswg")?,
        ];
        let drn = [
            component("cj", "mj", "pb", "tcjbd")?,
            component("cjsw", "mjsw", "pbsw", "tcjbdsw")?,
            component("cjswg", "mjswg", "pbswg", "tcjbdswg")?,
        ];
        (src, drn)
    } else {
        let src = [
            component("cjs", "mjs", "pbs", "tcjbs")?,
            component("cjsws", "mjsws", "pbsws", "tcjbssw")?,
            component("cjswgs", "mjswgs", "pbswgs", "tcjbsswg")?,
        ];
        let drn = [
            component("cjd", "mjd", "pbd", "tcjbd")?,
            component("cjswd", "mjswd", "pbswd", "tcjbdsw")?,
            component("cjswgd", "mjswgd", "pbswgd", "tcjbdswg")?,
        ];
        (src, drn)
    };
    let inst = |name: &str| {
        variant
            .parameter(ParameterScope::Instance, name)
            .map(|spec| spec.slot)
            .ok_or_else(|| ModelError::UnknownParameter(name.to_owned()))
    };
    Ok(JunctionSlots {
        source,
        drain,
        area_source: inst("as")?,
        perimeter_source: inst("ps")?,
        area_drain: inst("ad")?,
        perimeter_drain: inst("pd")?,
        multiplicity: inst("m")?,
        rth0: lookup_slot(variant, "rth0")?,
    })
}

fn slots(variant: ModelVariant) -> Result<JunctionSlots, ModelError> {
    compiled_hv_by_family(variant, &HV1_SLOTS, &HV2_SLOTS, compile_slots).copied()
}

const fn checked(value: f64) -> Result<f64, ModelError> {
    hv_finite_or_error(value, "HiSIM-HV junction charge must be finite")
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct JunctionComponent {
    capacitance: f64,
    temperature_derivative: f64,
    grading: f64,
    potential: f64,
}

fn charge_component(
    part: JunctionComponent,
    bias: f64,
) -> Result<HiSimHvJunctionCharge, ModelError> {
    if bias < 0.0 && part.capacitance <= 0.0 {
        // Native C's depletion branch ignores negative/zero `cz` factors.
        return Ok(HiSimHvJunctionCharge {
            charge_coulomb: 0.0,
            capacitance_farad: 0.0,
            charge_temperature_derivative_coulomb_per_kelvin: 0.0,
        });
    }
    let (charge, capacitance, derivative) = if bias < 0.0 {
        let argument = checked(1.0 - bias / part.potential)?;
        if argument <= 0.0 || part.grading.to_bits() == 1.0f64.to_bits() {
            return Err(ModelError::InvalidPhysicalParameter(
                "HiSIM-HV junction charge physical potential or grading",
            ));
        }
        let sarg = if part.grading.to_bits() == 0.5f64.to_bits() {
            1.0 / argument.sqrt()
        } else {
            argument.powf(-part.grading)
        };
        let fraction =
            checked(part.potential * (argument.mul_add(-sarg, 1.0)) / (1.0 - part.grading))?;
        (
            part.capacitance * fraction,
            part.capacitance * sarg,
            part.temperature_derivative * fraction,
        )
    } else {
        let curvature = part.grading / part.potential;
        let quadratic = bias * curvature;
        (
            bias * part.capacitance * (0.5f64.mul_add(quadratic, 1.0)),
            part.capacitance * (1.0 + quadratic),
            bias * part.temperature_derivative * (0.5f64.mul_add(quadratic, 1.0)),
        )
    };
    Ok(HiSimHvJunctionCharge {
        charge_coulomb: checked(charge)?,
        capacitance_farad: checked(capacitance)?,
        charge_temperature_derivative_coulomb_per_kelvin: checked(derivative)?,
    })
}

fn charge_junction(
    prepared: [Option<JunctionComponent>; 3],
    multiplicity: f64,
    bias: f64,
) -> Result<HiSimHvJunctionCharge, ModelError> {
    let mut charge = 0.0;
    let mut capacitance = 0.0;
    let mut derivative = 0.0;
    for component in prepared.into_iter().flatten() {
        let evaluated = charge_component(component, bias)?;
        charge += evaluated.charge_coulomb;
        capacitance += evaluated.capacitance_farad;
        derivative += evaluated.charge_temperature_derivative_coulomb_per_kelvin;
    }
    Ok(HiSimHvJunctionCharge {
        charge_coulomb: checked(charge * multiplicity)?,
        capacitance_farad: checked(capacitance * multiplicity)?,
        charge_temperature_derivative_coulomb_per_kelvin: checked(derivative * multiplicity)?,
    })
}

fn prepare_component(
    model: &PreparedHiSimHvModel,
    slot: ComponentSlots,
    extent: f64,
    temperature_delta: f64,
) -> Result<Option<JunctionComponent>, ModelError> {
    if extent == 0.0 {
        return Ok(None);
    }
    let base = checked(source_real_at(model, slot.zero_bias_capacitance)? * extent)?;
    let tc = source_real_at(model, slot.temperature_coefficient)?;
    Ok(Some(JunctionComponent {
        capacitance: checked(base * tc.mul_add(temperature_delta, 1.0))?,
        temperature_derivative: checked(base * tc)?,
        grading: source_real_at(model, slot.grading_coefficient)?,
        potential: source_real_at(model, slot.built_in_potential)?,
    }))
}

fn prepare_junction(
    model: &PreparedHiSimHvModel,
    slots: [ComponentSlots; 3],
    geometry: [f64; 3],
    temperature_delta: f64,
) -> Result<[Option<JunctionComponent>; 3], ModelError> {
    Ok([
        prepare_component(model, slots[0], geometry[0], temperature_delta)?,
        prepare_component(model, slots[1], geometry[1], temperature_delta)?,
        prepare_component(model, slots[2], geometry[2], temperature_delta)?,
    ])
}

/// Prepared substrate junction depletion-charge state at one device temperature.
///
/// Both source and drain model-coefficient arrays, layout area/sidewalls, and
/// the temperature-dependent zero-bias capacitances are already resolved.
/// Repeated Newton trial voltages access only numerical arrays, never model
/// parameter slots, layout geometry, or temperature preparation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreparedHiSimHvJunctionCharges {
    source: [Option<JunctionComponent>; 3],
    drain: [Option<JunctionComponent>; 3],
    multiplicity: f64,
}

impl PreparedHiSimHvJunctionCharges {
    /// Evaluate physical source and drain depletion Q, dQ/dV, and dQ/dT.
    ///
    /// # Errors
    /// Rejects nonfinite trial voltages and nonphysical power-law domains.
    pub fn evaluate(
        &self,
        bias: HiSimHvJunctionBias,
    ) -> Result<HiSimHvJunctionCharges, ModelError> {
        Ok(HiSimHvJunctionCharges {
            bulk_source: charge_junction(
                self.source,
                self.multiplicity,
                checked(bias.bulk_source_v)?,
            )?,
            bulk_drain: charge_junction(
                self.drain,
                self.multiplicity,
                checked(bias.bulk_drain_v)?,
            )?,
        })
    }
}

/// Prepare both physical HiSIM-HV substrate junction charge branches.
///
/// The source/device geometry and temperature coefficients are captured for
/// an entire sequence of Newton trial voltages.
///
/// The source branches at `perimeter > Weff*NF`, use full perimeter for the
/// gate-edge contribution otherwise, branch at `vbs_jct`/`vbd_jct` zero,
/// retain the native `mj==0.5` optimized power law, and multiply all results
/// by instance `M`. When thermal topology exists but the size-binned `RTH0`
/// is zero, the original C charge evaluator uses ambient rather than local
/// device temperature. The caller supplies a matching prepared model, device
/// and transport state from the same source family.
///
/// # Errors
/// Rejects inconsistent source variants and nonfinite or nonphysical values,
/// rather than manufacturing current/charge values for invalid states.
pub fn prepare_hisim_hv_junction_charges(
    model: &PreparedHiSimHvModel,
    instance: &PreparedHiSimHvInstance,
    transport: &PreparedHiSimHvTransport,
) -> Result<PreparedHiSimHvJunctionCharges, ModelError> {
    model.validate_instance(instance)?;
    instance.validate_transport(transport)?;
    let slots = slots(model.variant())?;
    let t = transport.temperature;
    let rth0 = instance
        .binned_model_slot(slots.rth0)
        .ok_or(ModelError::MissingDefaultDependency(slots.rth0))?;
    let thermal_active = transport.thermal_branch.is_some() && rth0 != 0.0;
    let junction_temperature = if thermal_active {
        t.device_temperature_kelvin
    } else {
        t.ambient_device_temperature_kelvin
    };
    let diff = checked(junction_temperature - t.nominal_temperature_kelvin)?;
    let m = checked(instance_real_at(instance, slots.multiplicity)?)?;
    let width = checked(instance.effective_geometry().channel_width_total_m)?;
    let prepare_side = |family_slots: [ComponentSlots; 3], area_slot, perimeter_slot| {
        let perimeter = checked(instance_real_at(instance, perimeter_slot)?)?;
        let area = checked(instance_real_at(instance, area_slot)?)?;
        let geometry = if perimeter > width {
            [area, perimeter - width, width]
        } else {
            [area, 0.0, perimeter]
        };
        prepare_junction(model, family_slots, geometry, diff)
    };
    Ok(PreparedHiSimHvJunctionCharges {
        source: prepare_side(slots.source, slots.area_source, slots.perimeter_source)?,
        drain: prepare_side(slots.drain, slots.area_drain, slots.perimeter_drain)?,
        multiplicity: m,
    })
}
