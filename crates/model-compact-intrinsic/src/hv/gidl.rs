//! HiSIM-HV1/HV2 gate-induced drain/source leakage from the original
//! `hsmhv[2]eval.c` GIDL/GISL branch. All voltages use normalized internal
//! coordinates; source/drain orientation and polarity are handled by assembly.

use std::sync::OnceLock;

use super::{
    PreparedHiSimHvInstance, PreparedHiSimHvModel, PreparedHiSimHvTemperature,
    compiled_hv_by_family, instance_real_at, lookup_slot, source_real_at,
};
use crate::{ModelError, ModelVariant, ParameterScope, ParameterValue};

const ELECTRON_CHARGE: f64 = 1.602_191_8e-19;
const FIELD_SMOOTHING: f64 = 1.;
const DRAIN_SMOOTHING: f64 = 1e-5;
const FIELD_FLOOR: f64 = 1e-25;
const EXP_THRESHOLD: f64 = 34.;
const JUNCTION_SMOOTHING: f64 = 0.5;

/// One scalar and its normalized internal partials in Vgs, Vds, Vbs, T order.
pub type HiSimHvIntrinsicScalar = crate::differential::Differential<4>;

/// Intrinsic normalized bias (Vds is nonnegative after mode selection).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvIntrinsicBias {
    pub vgs: f64,
    pub vds: f64,
    pub vbs: f64,
}

/// The two independent positive junction leakage currents, including M.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HiSimHvGidlCurrents {
    pub drain_to_body: HiSimHvIntrinsicScalar,
    pub source_to_body: HiSimHvIntrinsicScalar,
}

/// Prepared geometry-dependent GIDL/GISL coefficients, without a trial bias.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreparedHiSimHvGidl {
    enabled: bool,
    amplitude: f64,
    field_exponent: f64,
    drain_field: f64,
    field_offset: f64,
    threshold_factor: f64,
    inverse_oxide_thickness: f64,
}

fn modified_drain(drain: f64) -> (f64, f64) {
    // Native Vdsp = Vds - 0.5*(T1 + sqrt(T1*T1 + 4*delta*Vds)).
    let t1 = drain.mul_add(-100., 1.).mul_add(drain, -DRAIN_SMOOTHING);
    let root = t1.hypot((4. * DRAIN_SMOOTHING * drain).sqrt());
    let dt1 = (-200_f64).mul_add(drain, 1.);
    // Rationalize the sum for negative T1, and its derivative follows from
    // differentiating the quadratic r*r - T1*r - delta*Vds = 0.
    let correction = if t1 >= 0. {
        f64::midpoint(t1, root)
    } else {
        2. * DRAIN_SMOOTHING * drain / (root - t1)
    };
    let derivative = 1. - dt1.mul_add(correction, DRAIN_SMOOTHING) / root;
    (drain - correction, derivative)
}

fn positive_field(field: f64) -> (f64, f64) {
    // Fn_SZ, rationalized on the negative side to retain small fields.
    let root = field.hypot(2. * FIELD_SMOOTHING);
    let positive = if field >= 0. {
        f64::midpoint(field, root)
    } else {
        2. * FIELD_SMOOTHING * FIELD_SMOOTHING / (root - field)
    };
    (positive, positive / root)
}

fn junction_gate(voltage: f64) -> (f64, f64) {
    // V^3 / (V^3 + 0.5); use reciprocal form to avoid overflow at high V.
    if voltage <= 0. {
        return (0., 0.);
    }
    let ratio = JUNCTION_SMOOTHING / voltage.powi(3);
    let fraction = 1. / (1. + ratio);
    let derivative = if ratio.is_finite() {
        3. * fraction * (ratio * fraction) / voltage
    } else {
        0.
    };
    (fraction, derivative)
}

#[derive(Clone, Copy)]
struct Trial<'a> {
    bias: HiSimHvIntrinsicBias,
    threshold_shift: HiSimHvIntrinsicScalar,
    temperature: &'a PreparedHiSimHvTemperature,
    drain: (f64, f64),
}

impl PreparedHiSimHvGidl {
    fn side(self, trial: Trial<'_>, drain_side: bool) -> HiSimHvIntrinsicScalar {
        let (junction_voltage, junction_gradient, slope) = if drain_side {
            (
                trial.bias.vds - trial.bias.vbs,
                [0., 1., -1., 0.],
                self.drain_field,
            )
        } else {
            (-trial.bias.vbs, [0., 0., -1., 0.], 1. - self.drain_field)
        };
        let (gate, gate_derivative) = junction_gate(junction_voltage);
        if gate == 0. {
            return HiSimHvIntrinsicScalar::default();
        }
        let field = slope.mul_add(
            trial.drain.0,
            self.drain_field.mul_add(self.field_offset, -trial.bias.vgs),
        );
        let field = self
            .threshold_factor
            .mul_add(trial.threshold_shift.value, field)
            * self.inverse_oxide_thickness;
        let (field, field_derivative) = positive_field(field);
        let field_grad = [-1., slope * trial.drain.1, 0., 0.];
        let bandgap = trial.temperature.ig_bandgap_sqrt;
        let bandgap_slope = trial.temperature.ig_bandgap_derivative_ev_per_kelvin;
        let barrier = self.field_exponent * trial.temperature.ig_bandgap_three_halves;
        let inverse = 1. / (field + FIELD_FLOOR);
        let exponent = -barrier * inverse;
        if exponent < -EXP_THRESHOLD {
            return HiSimHvIntrinsicScalar::default();
        }
        let scale = self.amplitude / bandgap * exponent.exp();
        let current = scale * field * field;
        let current_field = scale * field * ((barrier * field * inverse).mul_add(inverse, 2.));
        let current_temperature = current
            * ((-1.5 * self.field_exponent * bandgap * inverse)
                .mul_add(bandgap_slope, -0.5 * bandgap_slope / (bandgap * bandgap)));
        let gradient = std::array::from_fn(|axis| {
            let de = self
                .threshold_factor
                .mul_add(trial.threshold_shift.gradient[axis], field_grad[axis])
                * self.inverse_oxide_thickness
                * field_derivative;
            let di = current_field.mul_add(de, if axis == 3 { current_temperature } else { 0. });
            di.mul_add(gate, current * gate_derivative * junction_gradient[axis])
        });
        HiSimHvIntrinsicScalar {
            value: current * gate,
            gradient,
        }
    }

    /// Evaluate both junction leakage currents with physical channel threshold
    /// shifts `dVthSC+dVthLP` and their exact four-coordinate derivatives.
    /// A caller must supply the actual electrostatic solution, not an
    /// independently approximated threshold. Temperature is the same trial
    /// temperature used by the channel (including instance DTEMP).
    ///
    /// # Errors
    /// Rejects negative normalized Vds, nonfinite trial input or output.
    pub fn evaluate(
        &self,
        bias: HiSimHvIntrinsicBias,
        threshold_shift: HiSimHvIntrinsicScalar,
        temperature: &PreparedHiSimHvTemperature,
    ) -> Result<HiSimHvGidlCurrents, ModelError> {
        if ![
            bias.vgs,
            bias.vds,
            bias.vbs,
            threshold_shift.value,
            temperature.ig_bandgap_sqrt,
            temperature.ig_bandgap_three_halves,
            temperature.ig_bandgap_derivative_ev_per_kelvin,
        ]
        .into_iter()
        .chain(threshold_shift.gradient)
        .all(f64::is_finite)
            || bias.vds < 0.
            || temperature.ig_bandgap_sqrt <= 0.
            || temperature.ig_bandgap_three_halves <= 0.
        {
            return Err(ModelError::InvalidPhysicalParameter("HiSIM-HV GIDL trial"));
        }
        if !self.enabled || self.amplitude == 0. {
            return Ok(HiSimHvGidlCurrents::default());
        }
        let trial = Trial {
            bias,
            threshold_shift,
            temperature,
            drain: modified_drain(bias.vds),
        };
        let drain_to_body = self.side(trial, true);
        let source_to_body = self.side(trial, false);
        if [drain_to_body, source_to_body]
            .into_iter()
            .all(|point| point.value.is_finite() && point.gradient.into_iter().all(f64::is_finite))
        {
            Ok(HiSimHvGidlCurrents {
                drain_to_body,
                source_to_body,
            })
        } else {
            Err(ModelError::InvalidPhysicalParameter(
                "HiSIM-HV GIDL current",
            ))
        }
    }
}

#[derive(Clone, Copy)]
struct Slots {
    first: u16,
    second: u16,
    third: u16,
    fourth: u16,
    fifth: u16,
    oxide: u16,
    enabled: u16,
    multiplier: u16,
}
static HV1_SLOTS: OnceLock<Result<Slots, ModelError>> = OnceLock::new();
static HV2_SLOTS: OnceLock<Result<Slots, ModelError>> = OnceLock::new();
fn slots(variant: ModelVariant) -> Result<Slots, ModelError> {
    Ok(Slots {
        first: lookup_slot(variant, "gidl1")?,
        second: lookup_slot(variant, "gidl2")?,
        third: lookup_slot(variant, "gidl3")?,
        fourth: lookup_slot(variant, "gidl4")?,
        fifth: lookup_slot(variant, "gidl5")?,
        oxide: lookup_slot(variant, "tox")?,
        enabled: lookup_slot(variant, "cogidl")?,
        multiplier: variant
            .parameter(ParameterScope::Instance, "m")
            .ok_or_else(|| ModelError::UnknownParameter("m".to_owned()))?
            .slot,
    })
}

/// Prepare source-defined GIDL/GISL coefficients for both HiSIM-HV families.
/// The native binned GIDL1 divides by 10 and GIDL2 multiplies by 100.
///
/// # Errors
/// Invalid model/instance context or nonfinite physical coefficients.
pub fn prepare_hisim_hv_gidl(
    model: &PreparedHiSimHvModel,
    instance: &PreparedHiSimHvInstance,
) -> Result<PreparedHiSimHvGidl, ModelError> {
    model.validate_instance(instance)?;
    let slots = compiled_hv_by_family(model.variant(), &HV1_SLOTS, &HV2_SLOTS, slots)?;
    let enabled = match model.get_slot(slots.enabled) {
        Some(ParameterValue::Integer(value)) => *value != 0,
        _ => return Err(ModelError::MissingDefaultDependency(slots.enabled)),
    };
    let binned = |slot| {
        instance
            .binned_model_slot(slot)
            .ok_or(ModelError::MissingDefaultDependency(slot))
    };
    let oxide = source_real_at(model, slots.oxide)?;
    let point = PreparedHiSimHvGidl {
        enabled,
        amplitude: binned(slots.first)? / 10.
            * ELECTRON_CHARGE
            * instance.effective_geometry().channel_width_total_m
            * instance_real_at(instance, slots.multiplier)?,
        field_exponent: binned(slots.second)? * 100.,
        drain_field: source_real_at(model, slots.third)?,
        field_offset: source_real_at(model, slots.fourth)?,
        threshold_factor: source_real_at(model, slots.fifth)?,
        inverse_oxide_thickness: oxide.recip(),
    };
    if oxide <= 0.
        || ![
            point.amplitude,
            point.field_exponent,
            point.drain_field,
            point.field_offset,
            point.threshold_factor,
            point.inverse_oxide_thickness,
        ]
        .into_iter()
        .all(f64::is_finite)
    {
        return Err(ModelError::InvalidPhysicalParameter(
            "HiSIM-HV GIDL coefficient",
        ));
    }
    Ok(point)
}
