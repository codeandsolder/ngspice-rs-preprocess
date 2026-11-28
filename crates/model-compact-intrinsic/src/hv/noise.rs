//! Native HiSIM-HV enhancement-mode source-referred noise.
use super::{
    HiSimHvChannelRegion, HiSimHvIntrinsicBias, HiSimHvNormalChannelWithParasiticsPoint,
    PreparedHiSimHvInstance, PreparedHiSimHvModel, PreparedHiSimHvTemperature,
    instance::model_real, instance_real_at, lookup_slot,
};
use crate::{ModelError, ModelVariant, ParameterScope, ParameterValue};

const Q: f64 = 1.602_191_8e-19;
const K: f64 = 1.380_622_6e-23;
const EPS: f64 = 10. * f64::EPSILON;

/// One-sided physical current noise power spectral densities (A²/Hz).
///
/// The native noise analysis injects the induced-gate contribution at
/// the drain/source pair, weighted by the squared correlation coefficient.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HiSimHvChannelNoise {
    pub thermal: f64,
    pub flicker: f64,
    pub induced_gate: f64,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Noise {
    flicker: bool,
    thermal: bool,
    induced: bool,
    trap_density: f64,
    mobility_correlation: f64,
    interface_capacitance: f64,
    frequency_exponent: f64,
    velocity_exponent: f64,
    width: f64,
    length: f64,
    multiplicity: f64,
    small: f64,
}
#[derive(Clone, Copy)]
struct OperatingPoint {
    length: f64,
    thermal_voltage: f64,
    oxide: f64,
    inversion_voltage: f64,
    source_density: f64,
    source_body_voltage: f64,
    inversion_charge: f64,
    current: f64,
    mobility: f64,
    universal: f64,
    field: f64,
    drain_field: f64,
    alpha: f64,
    integral: f64,
    source_capacitance: f64,
    drain_voltage: f64,
}
fn smooth_positive(x: f64) -> f64 {
    let root = x.hypot(0.002);
    if x >= 0. {
        f64::midpoint(x, root)
    } else {
        2e-6 / (root - x)
    }
}
impl Noise {
    pub(super) fn prepare(
        model: &PreparedHiSimHvModel,
        instance: &PreparedHiSimHvInstance,
    ) -> Result<Self, ModelError> {
        let selector = |name| {
            let slot = lookup_slot(model.variant(), name)?;
            match model.get_slot(slot) {
                Some(ParameterValue::Integer(value)) => Ok(*value != 0),
                _ => Err(ModelError::MissingDefaultDependency(slot)),
            }
        };
        let binned = |name| {
            let slot = lookup_slot(model.variant(), name)?;
            instance
                .binned_model_slot(slot)
                .ok_or(ModelError::MissingDefaultDependency(slot))
        };
        let m = model
            .variant()
            .parameter(ParameterScope::Instance, "m")
            .ok_or_else(|| ModelError::UnknownParameter("m".to_owned()))?
            .slot;
        let g = instance.effective_geometry();
        Ok(Self {
            flicker: selector("coflick")?,
            thermal: selector("cothrml")?,
            induced: selector("coign")?,
            trap_density: binned("nftrp")? * 1e4,
            mobility_correlation: binned("nfalp")? / 100.,
            interface_capacitance: model_real(model, "cit")? * 1e4,
            frequency_exponent: model_real(model, "falph")?,
            velocity_exponent: model_real(model, "bb")?,
            width: g.channel_width_total_m,
            length: g.channel_length_m,
            multiplicity: instance_real_at(instance, m)?,
            small: if model.variant() == ModelVariant::HiSimHv1 {
                1e-50
            } else {
                1e-25
            },
        })
    }
    fn flicker(self, p: OperatingPoint) -> f64 {
        let source = p.source_density / Q;
        let offset =
            (p.oxide + p.source_density / p.source_body_voltage + self.interface_capacitance)
                * p.thermal_voltage
                / Q;
        let drain = 2. * p.inversion_charge / (Q * p.length * self.width) - source;
        let first = source + offset;
        let second = drain + offset;
        let delta = drain - source;
        let quotient = if delta == 0. {
            1. / first
        } else {
            (delta / first).ln_1p() / delta
        };
        let correlation = self.mobility_correlation * p.field * p.mobility;
        let factor = correlation.mul_add(
            correlation,
            (2. * correlation).mul_add(quotient, 1. / first / second),
        );
        self.multiplicity * p.current * p.current * self.trap_density * p.thermal_voltage
            / (p.length * self.width)
            * factor
    }
    fn drain_mobility(self, p: OperatingPoint) -> f64 {
        let x = p.universal * p.drain_field / 1e7;
        let term = if (self.velocity_exponent - 1.).abs() <= EPS {
            x
        } else if (self.velocity_exponent - 2.).abs() <= EPS {
            x * x
        } else {
            x * p.drain_field.powf(self.velocity_exponent - 1.)
        };
        p.universal * (1. + term).powf(-1. / self.velocity_exponent)
    }
    fn thermal(self, p: OperatingPoint, drain_mobility: f64) -> f64 {
        // Normalize the two mobilities by their average, avoiding tiny squared factors.
        let average = f64::midpoint(p.mobility, drain_mobility);
        let source = p.mobility / average;
        let drain = drain_mobility / average;
        let a = p.alpha;
        let dd = a.mul_add(6_f64.mul_add(a, 3.), 1.);
        let ds = a.mul_add(3_f64.mul_add(a, 4.), 3.);
        let ss = a.mul_add(a + 3., 6.);
        let factor =
            (ss * source).mul_add(source, (ds * drain).mul_add(source, dd * drain * drain));
        self.width * p.oxide * p.inversion_voltage * p.mobility * factor
            / (15. * p.length * (1. + a))
    }
    fn induced(self, p: OperatingPoint, drain_mobility: f64, thermal_conductance: f64) -> f64 {
        let source_square = p.inversion_voltage * p.inversion_voltage;
        let drain_square = (2. * p.thermal_voltage / p.oxide).mul_add(-p.integral, source_square);
        let source = smooth_positive(source_square) + self.small;
        let drain = smooth_positive(drain_square) + self.small;
        let difference = source - drain;
        if p.source_density < EPS
            || difference < EPS
            || p.source_capacitance <= 1e-6 * p.oxide * self.width * self.length
        {
            return 0.;
        }
        let root = drain.sqrt();
        let sum = p.inversion_voltage + root;
        let shape = (20. * root * p.inversion_voltage).mul_add(
            source + drain,
            (42. * source).mul_add(drain, 4. * drain.mul_add(drain, source * source)),
        );
        let conductance = self.width / p.length * p.mobility * p.oxide;
        let gamma = thermal_conductance / (conductance * p.inversion_voltage);
        let cross =
            15_f64.sqrt() * difference * (4. * p.inversion_voltage).mul_add(root, source + drain)
                / (6. * sum * (gamma * sum * p.inversion_voltage * shape).sqrt());
        let scale =
            (16. / 135.) * Q * p.thermal_voltage * p.source_capacitance.powi(2) / conductance;
        let correction = if p.drain_voltage > EPS {
            let a = p.universal / p.mobility;
            let b = (p.universal / drain_mobility - a) / p.drain_voltage;
            a + (2. / 3.) * b * p.inversion_voltage.mul_add(root, source + drain) / sum
        } else {
            p.universal / drain_mobility
        };
        self.multiplicity * (scale * shape / sum.powi(5) * correction).max(0.) * cross * cross
    }
    pub(super) fn evaluate(
        self,
        bias: HiSimHvIntrinsicBias,
        state: &HiSimHvNormalChannelWithParasiticsPoint,
        temperature: &PreparedHiSimHvTemperature,
        frequency: f64,
    ) -> Result<HiSimHvChannelNoise, ModelError> {
        if !frequency.is_finite() || frequency <= 0. {
            return Err(ModelError::InvalidPhysicalParameter(
                "HiSIM-HV noise frequency",
            ));
        }
        let channel = &state.channel.intrinsic;
        let electro = &channel.electrostatics;
        let charge = &electro.charge;
        if !matches!(
            charge.region,
            HiSimHvChannelRegion::Transition | HiSimHvChannelRegion::StrongInversion
        ) {
            return Ok(HiSimHvChannelNoise::default());
        }
        let gate = if state.nqs.is_some() {
            state.parasitic_charges.intrinsic.gate
        } else {
            state.quasistatic_charge_network()?.intrinsic.gate
        };
        let length = electro.modulation.effective_length_m.value;
        let p = OperatingPoint {
            length,
            thermal_voltage: 1. / temperature.inverse_thermal_voltage,
            oxide: electro.oxide.capacitance.value,
            inversion_voltage: charge.inversion_voltage.value,
            source_density: charge.source_inversion_density.value,
            source_body_voltage: charge.source_surface.value
                - electro.conditioned.confined_body.value,
            inversion_charge: (-electro.terminal_charges.drain.value
                - electro.terminal_charges.source.value)
                / self.multiplicity,
            current: state.channel.access.current.current_a / self.multiplicity,
            mobility: channel.mobility.mobility_m2_per_vs.value,
            universal: channel.mobility.universal_mobility_m2_per_vs.value,
            field: channel.mobility.longitudinal_field.value,
            drain_field: (electro.modulation.drain_surface.value - charge.source_surface.value)
                / length
                + self.small,
            alpha: charge.alpha.value,
            integral: charge.drift_diffusion_integral.value,
            source_capacitance: gate.gradient[..3].iter().sum::<f64>() / self.multiplicity,
            drain_voltage: bias.vds,
        };
        let flicker = if self.flicker {
            self.flicker(p) / frequency.powf(self.frequency_exponent)
        } else {
            0.
        };
        let (thermal, induced_gate) = if self.thermal {
            let drain = self.drain_mobility(p);
            let conductance = self.thermal(p, drain);
            (
                4. * K * temperature.device_temperature_kelvin * self.multiplicity * conductance,
                if self.induced {
                    self.induced(p, drain, conductance) * frequency * frequency
                } else {
                    0.
                },
            )
        } else {
            (0., 0.)
        };
        let result = HiSimHvChannelNoise {
            thermal,
            flicker,
            induced_gate,
        };
        if [thermal, flicker, induced_gate]
            .into_iter()
            .all(|v| v.is_finite() && v >= 0.)
        {
            Ok(result)
        } else {
            Err(ModelError::InvalidPhysicalParameter(
                "HiSIM-HV channel noise power",
            ))
        }
    }
}
