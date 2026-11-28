//! HV2 physical drain drift-region resistance and source sheet resistance.
use super::{
    HiSimHvDrainOverlap, HiSimHvExternalBias, HiSimHvIntrinsicBias, HiSimHvIntrinsicScalar,
    PreparedHiSimHvInstance, PreparedHiSimHvModel, PreparedHiSimHvTemperature,
    instance::model_real,
    instance_real_at, lookup_slot, prepare_hisim_hv_electrostatic_material,
    smoothing::{positive, symmetry_add, upper},
};
use crate::{ModelError, ModelVariant, ParameterScope, ParameterValue, differential::Differential};

const Q: f64 = 1.602_191_8e-19;
const K: f64 = 1.380_622_6e-23;
const ESI: f64 = 1.034_943e-10;
const SMALL: f64 = 1e-50;
const EPS: f64 = 10. * f64::EPSILON;
const RES_MIN: f64 = 1e-4;

/// Jacobian in independent coordinates: intrinsic Vgs,Vds,Vbs, T,
/// external Vgs,Vds,Vbs, physical drain and source drops, substrate voltage.
pub type HiSimHvDriftScalar = Differential<10>;
type Jet = HiSimHvDriftScalar;

/// Normalized channel biases and physical series-branch voltage drops.
///
/// The drops always point from the named external node to its own prime
/// node, even when channel conduction is reversed. The optional substrate
/// voltage is referenced to the physical external source.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvDriftBias {
    pub intrinsic: HiSimHvIntrinsicBias,
    pub external: HiSimHvExternalBias,
    pub reversed: bool,
    pub drain_drop_v: f64,
    pub source_drop_v: f64,
    pub substrate_voltage_v: Option<f64>,
}

/// Physical external-to-prime currents, resistance and thermal spectra.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvDriftPoint {
    pub drain_resistance_ohm: Jet,
    pub source_resistance_ohm: f64,
    pub drain_current_a: Jet,
    pub source_current_a: Jet,
    pub drain_noise_a2_per_hz: f64,
    pub source_noise_a2_per_hz: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Geometry {
    length: f64,
    width: f64,
    maximum_depth: f64,
    lateral_depth: f64,
    junction_depth: f64,
    depletion_factor: f64,
    junction_factor: f64,
    doping: f64,
    multiplicity: f64,
    drain_sheet: f64,
    source_sheet: f64,
}
#[derive(Clone, Copy, Debug, PartialEq)]
struct Transport {
    mobility: f64,
    velocity: f64,
    mobility_exponent: f64,
    velocity_temperature: f64,
    exponent: f64,
    exponent_temperature: f64,
    depth_coefficient: f64,
    carrier_coefficient: f64,
    overlap_coefficient: f64,
    length_offset: f64,
    carrier_length_offset: f64,
}
#[derive(Clone, Copy, Debug, PartialEq)]
struct Substrate {
    thickness: f64,
    depletion_factor: Option<f64>,
    built_in: f64,
    drain_coefficient: f64,
    bias_coefficient: f64,
}
/// Size-dependent native HV2 drift law.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreparedHiSimHvDrift {
    geometry: Geometry,
    transport: Transport,
    substrate: Substrate,
    symmetry: f64,
    junction_potential: f64,
    thermal_noise: bool,
}

const fn promote(value: HiSimHvIntrinsicScalar) -> Jet {
    let [g, d, b, t] = value.gradient;
    Jet {
        value: value.value,
        gradient: [g, d, b, t, 0., 0., 0., 0., 0., 0.],
    }
}
fn floor(value: Jet, minimum: f64) -> Jet {
    if value.value < minimum {
        Jet::constant(minimum)
    } else {
        value
    }
}
fn absolute(value: Jet) -> Jet {
    if value.value < 0. { -value } else { value }
}
fn smooth_abs(value: Jet, symmetry: f64) -> Jet {
    let magnitude = absolute(value);
    magnitude + 2. * symmetry_add(magnitude, symmetry, 1e-12)
}
fn limit_resistance(value: Jet) -> Jet {
    // Native Fn_SU_CP, soft saturation above 999 kΩ.
    if value.value <= 999e3 {
        return value;
    }
    let ratio = (value - 999e3) / 1e3;
    let inverse = 1. / ratio;
    let fraction = if ratio.value > 1. {
        1. / (1. + inverse * inverse * inverse * inverse).sqrt().sqrt()
    } else {
        ratio / (1. + ratio * ratio * ratio * ratio).sqrt().sqrt()
    };
    999e3 + 1e3 * fraction
}
fn positive_depth(value: Jet, delta: f64) -> Jet {
    if delta == 0. {
        floor(value, 0.)
    } else {
        positive(value, delta.abs())
    }
}
fn saturation(ratio: Jet, exponent: Jet) -> Jet {
    let z = exponent * ratio.ln();
    let softplus = if z.value > 0. {
        z + (-z).exp().ln_1p()
    } else {
        z.exp().ln_1p()
    };
    (-softplus / exponent).exp()
}
impl PreparedHiSimHvDrift {
    fn transport(
        self,
        voltage: Jet,
        temperature: &PreparedHiSimHvTemperature,
    ) -> Result<(Jet, Jet), ModelError> {
        let t = self.transport;
        let kelvin = Jet::coordinate(temperature.device_temperature_kelvin, 3, 1.);
        let ratio = kelvin / temperature.nominal_temperature_kelvin;
        let mobility = t.mobility / ratio.positive_pow(t.mobility_exponent);
        let velocity = t.velocity
            / (1.8 + 0.4 * ratio + 0.1 * ratio * ratio - t.velocity_temperature * (1. - ratio))
            + SMALL;
        let exponent =
            t.exponent + t.exponent_temperature * (kelvin - temperature.nominal_temperature_kelvin);
        if mobility.value <= 0. || velocity.value <= 0. || exponent.value <= 0. {
            return Err(ModelError::InvalidPhysicalParameter(
                "HiSIM-HV2 drift mobility",
            ));
        }
        let velocity_ratio = mobility * absolute(voltage) / (self.geometry.length * velocity);
        let saturated = mobility * saturation(velocity_ratio, exponent);
        let carrier = self.geometry.doping
            * (1.
                + t.carrier_coefficient * velocity_ratio / (1. + velocity_ratio) * voltage
                    / (self.geometry.length - t.carrier_length_offset));
        Ok((saturated, carrier))
    }
    fn substrate_multiplier(self, bias: HiSimHvDriftBias) -> Jet {
        let Some(sub) = bias.substrate_voltage_v else {
            return Jet::constant(1.);
        };
        let s = self.substrate;
        let Some(factor) = s.depletion_factor else {
            return Jet::constant(1.);
        };
        let normalized = Jet::coordinate(bias.external.vds, 5, 1.);
        let physical = if bias.reversed {
            -normalized
        } else {
            normalized
        };
        let substrate = Jet::coordinate(sub, 9, 1.)
            - if physical.value < 0. {
                physical
            } else {
                Jet::constant(0.)
            };
        let sym = smooth_abs(physical, self.symmetry);
        let potential = positive(
            s.built_in - s.drain_coefficient * sym - s.bias_coefficient * substrate,
            10.,
        ) + EPS;
        let width = if factor == 0. {
            Jet::constant(SMALL)
        } else {
            (factor * potential).sqrt() + SMALL
        };
        let depletion = upper(width, Jet::constant(s.thickness), 0.1 * s.thickness);
        self.geometry.length / (positive(s.thickness - depletion, 1e-9) + EPS)
    }

    /// Evaluate the physical nonlinear drift and sheet branches.
    /// # Errors
    /// Invalid material, geometry, temperature or nonfinite local state.
    pub fn evaluate(
        self,
        bias: HiSimHvDriftBias,
        overlap: HiSimHvDrainOverlap,
        temperature: &PreparedHiSimHvTemperature,
    ) -> Result<HiSimHvDriftPoint, ModelError> {
        let g = self.geometry;
        let t = self.transport;
        let drop = Jet::coordinate(bias.drain_drop_v, 7, 1.);
        let signed = if drop.value < 0. {
            -smooth_abs(drop, self.symmetry)
        } else {
            smooth_abs(drop, self.symmetry)
        };
        let (mobility, carrier) = self.transport(signed, temperature)?;
        let carrier = carrier - promote(overlap.depletion_density) * t.overlap_coefficient / Q;
        let gate_depletion = (g.depletion_factor
            * (positive(-promote(overlap.surface_potential), 0.01) + EPS))
            .sqrt();
        let body = Jet::coordinate(bias.intrinsic.vbs, 2, 1.);
        let drain = Jet::coordinate(bias.intrinsic.vds, 1, 1.);
        let junction_voltage = if bias.reversed { -body } else { drain - body };
        let junction = upper(
            (g.junction_factor
                * (positive(junction_voltage + self.junction_potential, 0.01) + EPS))
                .sqrt(),
            Jet::constant(g.lateral_depth + SMALL),
            0.01 * (g.lateral_depth + SMALL),
        );
        let depth = g.maximum_depth
            - t.depth_coefficient
                * g.maximum_depth
                * (gate_depletion / (g.junction_depth + EPS)
                    + junction / (g.lateral_depth + SMALL));
        let depth = positive_depth(depth, (1. - t.depth_coefficient) * g.maximum_depth / 100.);
        let conductance = Q * depth * mobility * carrier / (g.length + t.length_offset);
        let conductance = if conductance.value <= 0. {
            Jet::constant(EPS)
        } else {
            conductance
        };
        let resistance = limit_resistance(1. / (conductance * g.width))
            * self.substrate_multiplier(bias)
            + g.drain_sheet;
        let drain_resistance = floor(resistance / g.multiplicity, RES_MIN)
            .checked("HiSIM-HV2 drain drift resistance")?;
        let source_resistance = (g.source_sheet / g.multiplicity).max(RES_MIN);
        let factor = 4. * K * temperature.device_temperature_kelvin * g.multiplicity;
        let noise = |res: f64| {
            if self.thermal_noise && res > RES_MIN {
                factor / res
            } else {
                0.
            }
        };
        Ok(HiSimHvDriftPoint {
            drain_resistance_ohm: drain_resistance,
            source_resistance_ohm: source_resistance,
            drain_current_a: (drop / drain_resistance).checked("HiSIM-HV2 drain drift current")?,
            source_current_a: (Jet::coordinate(bias.source_drop_v, 8, 1.) / source_resistance)
                .checked("HiSIM-HV2 source sheet current")?,
            drain_noise_a2_per_hz: noise(resistance.value),
            source_noise_a2_per_hz: noise(g.source_sheet),
        })
    }
}

/// Bind the native HV2 CORDRIFT model. Disabled for HV1, CORDRIFT=0 or NOVER=0.
/// # Errors
/// Invalid physical geometry or model parameters.
pub fn prepare_hisim_hv_drift(
    model: &PreparedHiSimHvModel,
    instance: &PreparedHiSimHvInstance,
) -> Result<Option<PreparedHiSimHvDrift>, ModelError> {
    model.validate_instance(instance)?;
    if model.variant() != ModelVariant::HiSimHv2 {
        return Ok(None);
    }
    let selector = |name| {
        let slot = lookup_slot(model.variant(), name)?;
        match model.get_slot(slot) {
            Some(ParameterValue::Integer(value)) => Ok(*value),
            _ => Err(ModelError::MissingDefaultDependency(slot)),
        }
    };
    let binned = |name| {
        let slot = lookup_slot(model.variant(), name)?;
        instance
            .binned_model_slot(slot)
            .ok_or(ModelError::MissingDefaultDependency(slot))
    };
    let real = |name| model_real(model, name);
    let local = |name: &str| {
        let slot = model
            .variant()
            .parameter(ParameterScope::Instance, name)
            .ok_or_else(|| ModelError::UnknownParameter(name.to_owned()))?
            .slot;
        instance_real_at(instance, slot)
    };
    let doping = binned("nover")? * 1e6;
    if selector("cordrift")? != 1 || doping == 0. {
        return Ok(None);
    }
    let g = instance.geometry();
    let lg = g.gate_length_m * 1e6;
    let wg = g.gate_width_per_finger_m * 1e6;
    let length = local("ldrift1")? + local("ldrift2")?;
    let width = instance
        .effective_geometry()
        .lateral_diffusion_width_per_finger_m
        * local("nf")?;
    let lateral_depth = real("xldld")?;
    let junction_depth = real("rdrdjunc")?;
    let bulk = prepare_hisim_hv_electrostatic_material(model, instance)?.bulk_doping;
    let sheet = real("rsh")?.max(0.);
    let substrate = real("nsubsub")? * 1e6;
    let carrier_length_offset = real("rdrdl2")?;
    let length_offset = real("rdrdl1")?;
    if doping <= 0.
        || width <= 0.
        || length <= 0.
        || length + length_offset <= 0.
        || length <= carrier_length_offset
    {
        return Err(ModelError::InvalidPhysicalParameter(
            "HiSIM-HV2 drift geometry",
        ));
    }
    Ok(Some(PreparedHiSimHvDrift {
        geometry: Geometry {
            length,
            width,
            maximum_depth: junction_depth.hypot(lateral_depth),
            lateral_depth,
            junction_depth,
            depletion_factor: 2. * ESI / (Q * doping),
            junction_factor: 2. * ESI / Q * bulk / (doping + bulk) / doping,
            doping,
            multiplicity: local("m")?,
            drain_sheet: sheet * local("nrd")?,
            source_sheet: sheet * local("nrs")?,
        },
        transport: Transport {
            mobility: real("rdrmue")? / 1e4 * (1. + real("rdrmuel")? / lg.powf(real("rdrmuelp")?)),
            velocity: real("rdrvmax")? / 100.
                * (1. + real("rdrvmaxw")? / wg.powf(real("rdrvmaxwp")?))
                * (1. + real("rdrvmaxl")? / lg.powf(real("rdrvmaxlp")?)),
            mobility_exponent: real("rdrmuetmp")?,
            velocity_temperature: real("rdrvtmp")?,
            exponent: real("rdrbb")?,
            exponent_temperature: real("rdrbbtmp")?,
            depth_coefficient: real("rdrcx")?,
            carrier_coefficient: real("rdrcar")?,
            overlap_coefficient: real("rdrqover")?,
            length_offset,
            carrier_length_offset,
        },
        substrate: Substrate {
            thickness: real("ddrift")?,
            depletion_factor: if doping * (substrate + doping) > 0. {
                Some(2. * ESI / Q * substrate / (doping * (substrate + doping)))
            } else {
                None
            },
            built_in: real("vbisub")?,
            drain_coefficient: real("rdvdsub")?,
            bias_coefficient: real("rdvsub")?,
        },
        symmetry: real("vzadd0")?,
        junction_potential: real("vbi")?,
        thermal_noise: selector("cothrml")? != 0,
    }))
}
