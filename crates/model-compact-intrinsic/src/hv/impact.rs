//! Channel and drift impact ionization, with independent physical voltage coordinates.
use super::{
    HiSimHvChannelRegion, HiSimHvDrainCurrent, HiSimHvExternalBias, HiSimHvIntrinsicBias,
    HiSimHvIntrinsicScalar, HiSimHvNormalChannelWithAccessPoint, PreparedHiSimHvInstance,
    PreparedHiSimHvModel, PreparedHiSimHvTemperature,
    instance::model_real,
    instance_real_at, lookup_slot, prepare_hisim_hv_quantum_oxide,
    smoothing::{positive, symmetry_add},
};
use crate::{ModelError, ModelVariant, ParameterScope, ParameterValue, differential::Differential};

/// Value and partial derivatives ordered [Vgs, Vds, Vbs, T, Vgse, Vdse, Vbse, Vddp].
/// The last coordinate is the independent HV2 drain drift-region voltage drop.
pub type HiSimHvImpactScalar = Differential<8>;
type Jet = HiSimHvImpactScalar;
const SMALL: f64 = 1e-25;
const EPS: f64 = 10. * f64::EPSILON;

/// Impact currents in normalized physical orientation, including multiplicity.
///
/// HV1 combines the channel and drift components in its intrinsic substrate
/// branch; HV2 stamps them at distinct drain-side nodes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvImpactPoint {
    pub channel_current_a: Jet,
    pub drift_current_a: Jet,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct ImpactIonization {
    variant: ModelVariant,
    enabled: bool,
    channel_enabled: bool,
    gate_coefficient: f64,
    body_coefficient: f64,
    length_ratio: f64,
    drain_coefficient: f64,
    amplitude: f64,
    barrier: f64,
    classical_oxide: f64,
    potential_symmetry: f64,
    drift_amplitude: f64,
    drift_barrier: f64,
    drift_screening: f64,
    drift_threshold: f64,
    drift_gate_factor: f64,
}

const fn promote(value: HiSimHvIntrinsicScalar) -> Jet {
    let [g, d, b, t] = value.gradient;
    Jet {
        value: value.value,
        gradient: [g, d, b, t, 0., 0., 0., 0.],
    }
}
const fn promote_current(value: HiSimHvDrainCurrent) -> Jet {
    let g = value.electrical_derivative_a_per_v;
    Jet {
        value: value.current_a,
        gradient: [
            g.inner_vgs,
            g.inner_vds,
            g.inner_vbs,
            value.temperature_derivative_a_per_kelvin,
            g.outer_vgs,
            g.outer_vds,
            g.outer_vbs,
            0.,
        ],
    }
}

impl ImpactIonization {
    pub(super) fn prepare(
        model: &PreparedHiSimHvModel,
        instance: &PreparedHiSimHvInstance,
    ) -> Result<Self, ModelError> {
        let real = |name| model_real(model, name);
        let binned = |name| {
            let slot = lookup_slot(model.variant(), name)?;
            instance
                .binned_model_slot(slot)
                .ok_or(ModelError::MissingDefaultDependency(slot))
        };
        let selector = |name| {
            let slot = lookup_slot(model.variant(), name)?;
            match model.get_slot(slot) {
                Some(ParameterValue::Integer(value)) => Ok(*value),
                _ => Err(ModelError::MissingDefaultDependency(slot)),
            }
        };
        let local = |name| {
            let slot = model
                .variant()
                .parameter(ParameterScope::Instance, name)
                .ok_or_else(|| ModelError::UnknownParameter(name.to_owned()))?
                .slot;
            instance_real_at(instance, slot)
        };
        let length = instance.geometry().gate_length_m;
        // The size-dependent impact coefficients are specified in centimetres.
        let cm = length * 100.;
        let width_cm = instance.effective_geometry().channel_width_per_finger_m * 100.;
        let factor = |coefficient, exponent| -> Result<f64, ModelError> {
            Ok(1. + real(coefficient)? / cm.powf(real(exponent)?))
        };
        let width_power = width_cm.powf(real("svgswp")?);
        let gate_coefficient = binned("svgs")? * factor("svgsl", "svgslp")? * width_power
            / (width_power + real("svgsw")?);
        let gate_length = real("slg")? / 100. * factor("slgl", "slglp")?;
        let drift_length = local("ldrift1")? + local("ldrift2")?;
        let (drift_amplitude, drift_barrier, drift_screening, drift_threshold, drift_gate_factor) =
            if model.variant() == ModelVariant::HiSimHv1 {
                let effective_drift = if selector("coldrift")? != 0 {
                    drift_length
                } else {
                    drift_length + local("loverld")?
                };
                (
                    local("subld1")?,
                    effective_drift * local("subld2")? * 100.,
                    0.,
                    0.,
                    0.,
                )
            } else {
                (
                    real("subld1")?
                        * (1. + real("subld1l")? / (length * 1e6).powf(real("subld1lp")?)),
                    drift_length * real("subld2")? * 100.,
                    (real("xpdv")? * real("xldld")?).min(1.),
                    real("xpvdth")?,
                    real("xpvdthg")?,
                )
            };
        let result = Self {
            variant: model.variant(),
            enabled: selector("coisub")? != 0,
            channel_enabled: binned("sub1")? > 0. && binned("vmax")? > 0.,
            gate_coefficient,
            body_coefficient: binned("svbs")? * factor("svbsl", "svbslp")?,
            length_ratio: length / (gate_length + length),
            drain_coefficient: binned("svds")?,
            amplitude: binned("sub1")? * factor("sub1l", "sub1lp")?,
            barrier: binned("sub2")? * (1. + real("sub2l")? / cm),
            classical_oxide: prepare_hisim_hv_quantum_oxide(model, instance)?
                .zero_field_capacitance(),
            potential_symmetry: real("pzadd0")?,
            drift_amplitude,
            drift_barrier,
            drift_screening,
            drift_threshold,
            drift_gate_factor,
        };
        if ![
            result.gate_coefficient,
            result.body_coefficient,
            result.length_ratio,
            result.drain_coefficient,
            result.amplitude,
            result.barrier,
            result.classical_oxide,
            result.potential_symmetry,
            drift_amplitude,
            drift_barrier,
            drift_screening,
            drift_threshold,
            drift_gate_factor,
        ]
        .into_iter()
        .all(f64::is_finite)
        {
            return Err(ModelError::InvalidPhysicalParameter(
                "HiSIM-HV impact coefficients",
            ));
        }
        Ok(result)
    }

    pub(super) fn evaluate(
        &self,
        bias: HiSimHvIntrinsicBias,
        external: HiSimHvExternalBias,
        drift_voltage: f64,
        state: &HiSimHvNormalChannelWithAccessPoint,
        temperature: &PreparedHiSimHvTemperature,
    ) -> Result<HiSimHvImpactPoint, ModelError> {
        let zero = Jet::constant(0.);
        let electro = &state.intrinsic.electrostatics;
        let charge = &electro.charge;
        let no_inversion = matches!(
            charge.region,
            HiSimHvChannelRegion::Accumulation
                | HiSimHvChannelRegion::NoInversion
                | HiSimHvChannelRegion::WeakInversion
        );
        if !drift_voltage.is_finite() {
            return Err(ModelError::InvalidPhysicalParameter(
                "HiSIM-HV drift voltage",
            ));
        }
        if !self.enabled || (self.variant == ModelVariant::HiSimHv1 && no_inversion) {
            return Ok(HiSimHvImpactPoint {
                channel_current_a: zero,
                drift_current_a: zero,
            });
        }
        let current = promote_current(state.access.current);
        let drain = Jet::coordinate(bias.vds, 1, 1.);
        let surface = promote(charge.source_surface);
        let difference = promote(charge.surface_difference);
        let modified_source =
            surface + symmetry_add(drain - difference, self.potential_symmetry, EPS);
        let channel = if self.channel_enabled {
            let beta = Jet::coordinate(
                temperature.inverse_thermal_voltage,
                3,
                temperature.inverse_thermal_voltage_derivative,
            );
            let gate = self.gate_coefficient * promote(electro.poly.effective_gate);
            let factor =
                electro.material.charge_density_permittivity / self.classical_oxide.powi(2);
            let overdrive = gate
                - 1. / beta
                - self.body_coefficient * promote(electro.conditioned.modified_body);
            let root = if self.variant == ModelVariant::HiSimHv1 {
                (1. + 2. / factor) * overdrive
            } else {
                1. + 2. / factor * overdrive
            };
            let saturation = gate + factor * (1. - (positive(root, 0.001) + SMALL).sqrt());
            let excess = positive(
                self.drain_coefficient * promote(electro.conditioned.modified_drain)
                    + modified_source
                    - self.length_ratio * saturation,
                0.001,
            ) + SMALL;
            self.amplitude * excess * current * (-self.barrier / excess).exp()
        } else {
            zero
        };
        let drift = if self.drift_amplitude == 0. {
            zero
        } else {
            let voltage = if self.variant == ModelVariant::HiSimHv1 {
                positive(
                    Jet::coordinate(external.vds, 5, 1.) + surface
                        - promote(electro.modulation.drain_surface),
                    1e-6,
                )
            } else {
                let raw = positive(Jet::coordinate(drift_voltage, 7, 1.), 1e-6);
                let gate = Jet::coordinate(bias.vgs, 0, 1.);
                let excess = positive(
                    raw - self.drift_threshold * (1. + self.drift_gate_factor * gate),
                    0.01,
                ) + SMALL;
                positive(
                    raw - self.drift_screening * excess * (-1. / excess).exp(),
                    0.01,
                ) + SMALL
            };
            let inversion = (promote(charge.inversion_voltage) + SMALL).sqrt();
            self.drift_amplitude
                * current
                * voltage
                * (-self.drift_barrier / (voltage * inversion)).exp()
        };
        Ok(HiSimHvImpactPoint {
            channel_current_a: channel.checked("HiSIM-HV channel impact current")?,
            drift_current_a: drift.checked("HiSIM-HV drift impact current")?,
        })
    }
}
