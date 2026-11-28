//! HiSIM-HV1/HV2 inversion-layer tunneling and gate/body tunneling.
//! Direct gate/source and gate/drain edge currents are evaluated separately.
use super::{
    HiSimHvIntrinsicBias, HiSimHvIntrinsicScalar as Jet, PreparedHiSimHvInstance,
    PreparedHiSimHvModel, PreparedHiSimHvTemperature, compiled_hv_by_family, instance_real_at,
    lookup_slot, source_real_at,
};
use crate::{ModelError, ModelVariant, ParameterScope, ParameterValue};
use std::sync::OnceLock;

const ELECTRON_CHARGE: f64 = 1.602_191_8e-19;
const FIELD_FLOOR: f64 = 1e-25;
const EXP_THRESHOLD: f64 = 34.;
const MINIMUM_INVERSION_VOLTAGE: f64 = 1e-12;

/// The physical electrostatic solution used by the original gate-current
/// expressions.
///
/// The gradient axes are normalized Vgs, Vds, Vbs, and kelvin.
/// No electrostatic quantity is independently estimated by this subkernel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvGateChannelState {
    pub modified_gate: Jet,                // Vgsz
    pub modified_drain: Jet,               // Vdsz
    pub modified_body: Jet,                // Vbsz
    pub source_surface: Jet,               // Ps0z
    pub threshold_shift: Jet,              // dVth (total)
    pub poly_depletion: Jet,               // dPpg
    pub short_long_shift: Jet,             // dVthSC+dVthLP
    pub lateral_field: Jet,                // Ey
    pub inversion_density: Jet,            // Qiu
    pub poisson_coefficient: Jet,          // cnst0
    pub zero_field_oxide_capacitance: f64, // Cox0
    pub has_inversion: bool,               // flg_noqi == 0
}

/// Normalized gate current contributions, including multiplicity.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HiSimHvGateTunnelingCurrents {
    /// Original `Igate`, before drain/source partition.
    pub inversion: Jet,
    /// Original positive direct gate-body component before FN subtraction.
    pub direct_body: Jet,
    /// Original Fowler–Nordheim component (subtract from `direct_body`).
    pub fowler_nordheim: Jet,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct InversionCoefficients {
    amplitude: f64,
    barrier: f64,
    surface_factor: f64,
    threshold_factor: f64,
    field_scale: f64,
    drain_scale: f64,
    gate_offset: f64,
}
#[derive(Clone, Copy, Debug, PartialEq)]
struct BodyCoefficients {
    direct_amplitude: f64,
    direct_barrier: f64,
    direct_offset: f64,
    fn_amplitude: f64,
    fn_barrier: f64,
    fn_offset: f64,
    fn_body_factor: f64,
}
/// Prepared native coefficients for the non-edge gate currents.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreparedHiSimHvGateTunneling {
    enabled: bool,
    inverse_oxide: f64,
    flatband: f64,
    inversion: InversionCoefficients,
    body: BodyCoefficients,
}

fn positive_field(field: Jet, delta: f64) -> Jet {
    let root = field.value.hypot(2. * delta);
    let positive = if field.value >= 0. {
        f64::midpoint(field.value, root)
    } else {
        2. * delta * delta / (root - field.value)
    };
    field.map(positive, positive / root)
}
fn cutoff_exp(exponent: Jet) -> Jet {
    if exponent.value < -EXP_THRESHOLD {
        Jet::constant(0.)
    } else {
        exponent.exp()
    }
}
const fn thermal(value: f64, slope: f64) -> Jet {
    Jet::coordinate(value, 3, slope)
}

impl PreparedHiSimHvGateTunneling {
    fn inversion_current(
        &self,
        state: &HiSimHvGateChannelState,
        temperature: &PreparedHiSimHvTemperature,
    ) -> Jet {
        let c = self.inversion;
        if !state.has_inversion || c.amplitude == 0. {
            return Jet::default();
        }
        let surface = state.source_surface + state.modified_drain - 10. * f64::EPSILON;
        let field = state.modified_gate - self.flatband
            + c.threshold_factor * (state.threshold_shift - state.poly_depletion)
            - c.surface_factor * surface;
        let field = positive_field(
            field * field * self.inverse_oxide * (1. + state.lateral_field / c.field_scale),
            1.,
        );
        let gate_activation = (positive_field(state.modified_gate, 1e-3) - c.gate_offset) / 0.1;
        let gate_square = gate_activation * gate_activation;
        let field = field * gate_square / (1. + gate_square);
        let sqrt_gap = thermal(
            temperature.ig_bandgap_sqrt,
            0.5 * temperature.ig_bandgap_derivative_ev_per_kelvin / temperature.ig_bandgap_sqrt,
        );
        let gap32 = thermal(
            temperature.ig_bandgap_three_halves,
            1.5 * temperature.ig_bandgap_sqrt * temperature.ig_bandgap_derivative_ev_per_kelvin,
        );
        let exponent = -c.barrier * gap32 / (field + FIELD_FLOOR);
        if exponent.value < -EXP_THRESHOLD {
            return Jet::default();
        }
        let charge_factor = ((state.inversion_density
            + state.zero_field_oxide_capacitance * MINIMUM_INVERSION_VOLTAGE)
            / state.poisson_coefficient)
            .sqrt();
        c.amplitude / sqrt_gap * exponent.exp() * charge_factor * field * field * c.drain_scale
            / (c.drain_scale + state.modified_drain)
    }

    fn direct_body_current(&self, bias: HiSimHvIntrinsicBias) -> Jet {
        let c = self.body;
        if c.direct_amplitude == 0. {
            return Jet::default();
        }
        let gate = Jet::coordinate(bias.vgs, 0, 1.);
        let body = Jet::coordinate(bias.vbs, 2, 1.);
        let field = positive_field(
            (body - gate + self.flatband + c.direct_offset) * self.inverse_oxide,
            1.,
        ) + FIELD_FLOOR;
        c.direct_amplitude * field * field * cutoff_exp(-c.direct_barrier / field)
    }

    fn fowler_nordheim_current(
        &self,
        state: &HiSimHvGateChannelState,
        temperature: &PreparedHiSimHvTemperature,
    ) -> Jet {
        let c = self.body;
        if c.fn_amplitude == 0. {
            return Jet::default();
        }
        let gap = thermal(
            temperature.bandgap_ev,
            temperature.bandgap_derivative_ev_per_kelvin,
        );
        let gap_sqrt = gap.sqrt();
        let field =
            (state.modified_gate - c.fn_body_factor * state.modified_body - state.short_long_shift
                + c.fn_offset)
                * self.inverse_oxide;
        let barrier = c.fn_barrier * gap * gap_sqrt;
        let amplitude = c.fn_amplitude / gap_sqrt;
        if 2_f64.mul_add(field.value, barrier.value) < 0. {
            // Native minimum branch. Differentiate its actual value, including
            // the thermal dependence of both barrier factors.
            0.25 * amplitude * barrier * barrier * 2_f64.exp()
        } else if field.value == 0. {
            Jet::default()
        } else {
            amplitude * field * field * cutoff_exp(-barrier / field)
        }
    }

    /// Evaluate non-edge gate currents with the channel's actual electrostatic
    /// state. Their analytic Jacobians are the derivatives of the constitutive
    /// values, including all supplied state and bandgap temperature partials.
    /// # Errors
    /// Rejects invalid physical trial states or nonfinite constitutive output.
    pub fn evaluate(
        &self,
        bias: HiSimHvIntrinsicBias,
        state: &HiSimHvGateChannelState,
        temperature: &PreparedHiSimHvTemperature,
    ) -> Result<HiSimHvGateTunnelingCurrents, ModelError> {
        if bias.vds < 0.
            || ![bias.vgs, bias.vds, bias.vbs]
                .into_iter()
                .all(f64::is_finite)
        {
            return Err(ModelError::InvalidPhysicalParameter(
                "HiSIM-HV gate trial bias",
            ));
        }
        if !self.enabled {
            return Ok(HiSimHvGateTunnelingCurrents::default());
        }
        state.modified_gate.checked("HiSIM-HV modified gate")?;
        state.modified_body.checked("HiSIM-HV modified body")?;
        state.short_long_shift.checked("HiSIM-HV threshold shift")?;
        if state.has_inversion {
            for field in [
                state.modified_drain,
                state.source_surface,
                state.threshold_shift,
                state.poly_depletion,
                state.lateral_field,
                state.inversion_density,
                state.poisson_coefficient,
            ] {
                field.checked("HiSIM-HV inversion state")?;
            }
            if !state.zero_field_oxide_capacitance.is_finite()
                || state.zero_field_oxide_capacitance <= 0.
                || state.poisson_coefficient.value <= 0.
            {
                return Err(ModelError::InvalidPhysicalParameter(
                    "HiSIM-HV gate inversion coefficient",
                ));
            }
        }
        if ![
            temperature.bandgap_ev,
            temperature.bandgap_derivative_ev_per_kelvin,
            temperature.ig_bandgap_sqrt,
            temperature.ig_bandgap_three_halves,
            temperature.ig_bandgap_derivative_ev_per_kelvin,
        ]
        .into_iter()
        .all(f64::is_finite)
            || temperature.bandgap_ev <= 0.
            || temperature.ig_bandgap_sqrt <= 0.
            || temperature.ig_bandgap_three_halves <= 0.
        {
            return Err(ModelError::InvalidPhysicalParameter(
                "HiSIM-HV gate bandgap",
            ));
        }
        let inversion = self
            .inversion_current(state, temperature)
            .checked("HiSIM-HV inversion tunneling")?;
        let direct_body = self
            .direct_body_current(bias)
            .checked("HiSIM-HV direct body tunneling")?;
        let fowler_nordheim = self
            .fowler_nordheim_current(state, temperature)
            .checked("HiSIM-HV Fowler-Nordheim tunneling")?;
        Ok(HiSimHvGateTunnelingCurrents {
            inversion,
            direct_body,
            fowler_nordheim,
        })
    }
}

#[derive(Clone, Copy)]
struct Slots {
    enabled: u16,
    multiplier: u16,
    oxide: u16,
    flatband: u16,
    inversion: [u16; 7],
    direct: [u16; 3],
    fowler: [u16; 4],
    offset: u16,
}
static HV1_SLOTS: OnceLock<Result<Slots, ModelError>> = OnceLock::new();
static HV2_SLOTS: OnceLock<Result<Slots, ModelError>> = OnceLock::new();
fn slots(variant: ModelVariant) -> Result<Slots, ModelError> {
    let lookup = |name| lookup_slot(variant, name);
    Ok(Slots {
        enabled: lookup("coiigs")?,
        multiplier: variant
            .parameter(ParameterScope::Instance, "m")
            .ok_or_else(|| ModelError::UnknownParameter("m".to_owned()))?
            .slot,
        oxide: lookup("tox")?,
        flatband: lookup("vfbc")?,
        inversion: [
            lookup("gleak1")?,
            lookup("gleak2")?,
            lookup("gleak3")?,
            lookup("gleak4")?,
            lookup("gleak5")?,
            lookup("gleak6")?,
            lookup("gleak7")?,
        ],
        direct: [lookup("glkb1")?, lookup("glkb2")?, lookup("glkb3")?],
        fowler: [
            lookup("fn1")?,
            lookup("fn2")?,
            lookup("fn3")?,
            lookup("fvbs")?,
        ],
        offset: lookup("vzadd0")?,
    })
}
/// Prepare original HiSIM-HV1/HV2 inversion and body-gate tunneling constants.
/// # Errors
/// Invalid model context, coefficient or physical geometry.
pub fn prepare_hisim_hv_gate_tunneling(
    model: &PreparedHiSimHvModel,
    instance: &PreparedHiSimHvInstance,
) -> Result<PreparedHiSimHvGateTunneling, ModelError> {
    model.validate_instance(instance)?;
    let slots = compiled_hv_by_family(model.variant(), &HV1_SLOTS, &HV2_SLOTS, slots)?;
    let enabled = match model.get_slot(slots.enabled) {
        Some(ParameterValue::Integer(value)) => *value != 0,
        _ => return Err(ModelError::MissingDefaultDependency(slots.enabled)),
    };
    let source = |slot| source_real_at(model, slot);
    let binned = |slot| {
        instance
            .binned_model_slot(slot)
            .ok_or(ModelError::MissingDefaultDependency(slot))
    };
    let [
        channel_amplitude,
        channel_barrier,
        surface_factor,
        threshold_factor,
        field_scale,
        drain_scale,
        area_slot,
    ] = slots.inversion;
    let [direct_first, direct_second, direct_third] = slots.direct;
    let [fn_first, fn_second, fn_third, body_factor] = slots.fowler;
    let oxide = source(slots.oxide)?;
    let geometry = instance.effective_geometry();
    let area = geometry.channel_length_m * geometry.channel_width_total_m;
    let multiplier = instance_real_at(instance, slots.multiplier)?;
    let area_scale = source(area_slot)? / 1e4;
    let point = PreparedHiSimHvGateTunneling {
        enabled,
        inverse_oxide: oxide.recip(),
        flatband: binned(slots.flatband)?,
        inversion: InversionCoefficients {
            amplitude: multiplier
                * ELECTRON_CHARGE
                * binned(channel_amplitude)?
                * area
                * area_scale
                / (area_scale + area),
            barrier: binned(channel_barrier)? * 100.,
            surface_factor: binned(surface_factor)?,
            threshold_factor: source(threshold_factor)? * 100. * geometry.channel_length_m,
            field_scale: source(field_scale)? * 100.,
            drain_scale: binned(drain_scale)?,
            gate_offset: source(slots.offset)?,
        },
        body: BodyCoefficients {
            direct_amplitude: multiplier * binned(direct_first)? * area,
            direct_barrier: binned(direct_second)? * 100.,
            direct_offset: source(direct_third)?,
            fn_amplitude: multiplier
                * ELECTRON_CHARGE
                * binned(fn_first)?
                * geometry.channel_width_total_m
                * instance.geometry().gate_length_m,
            fn_barrier: binned(fn_second)? * 100.,
            fn_offset: binned(fn_third)?,
            fn_body_factor: binned(body_factor)?,
        },
    };
    if oxide <= 0.
        || ![
            point.inverse_oxide,
            point.flatband,
            point.inversion.amplitude,
            point.inversion.barrier,
            point.inversion.surface_factor,
            point.inversion.threshold_factor,
            point.inversion.field_scale,
            point.inversion.drain_scale,
            point.inversion.gate_offset,
            point.body.direct_amplitude,
            point.body.direct_barrier,
            point.body.direct_offset,
            point.body.fn_amplitude,
            point.body.fn_barrier,
            point.body.fn_offset,
            point.body.fn_body_factor,
        ]
        .into_iter()
        .all(f64::is_finite)
    {
        return Err(ModelError::InvalidPhysicalParameter(
            "HiSIM-HV gate tunneling preparation",
        ));
    }
    Ok(point)
}
