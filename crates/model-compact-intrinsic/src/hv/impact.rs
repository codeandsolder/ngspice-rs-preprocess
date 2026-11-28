//! Channel and drift impact ionization, with independent physical voltage coordinates.
use super::{
    HiSimHvChannelRegion, HiSimHvDrainCurrent, HiSimHvExternalBias, HiSimHvIntrinsicBias,
    HiSimHvIntrinsicScalar, HiSimHvNormalChannelWithAccessPoint, PreparedHiSimHvInstance,
    PreparedHiSimHvModel, PreparedHiSimHvTemperature, instance::model_real, instance_real_at,
    lookup_slot, smoothing::positive,
};
use crate::{ModelError, ModelVariant, ParameterScope, ParameterValue, differential::Differential};

/// Value and partial derivatives ordered [Vgs, Vds, Vbs, T, Vgse, Vdse, Vbse, Vddp].
/// The last coordinate is the independent HV2 drain drift-region voltage drop.
pub type HiSimHvImpactScalar = Differential<8>;
type Jet = HiSimHvImpactScalar;
const SMALL: f64 = 1e-25;

/// Impact currents in normalized physical orientation, including multiplicity.
///
/// HV1 combines the channel and drift components in its intrinsic substrate
/// branch; HV2 stamps them at distinct drain-side nodes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvImpactPoint {
    pub channel_current_a: Jet,
    pub drift_current_a: Jet,
    /// Impact-ionization induced bulk-potential change, per unit device.
    pub bulk_potential_change_v: Jet,
    /// Additional drain-source current induced by the bulk potential.
    pub bulk_feedback_current_a: Jet,
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
    drift_amplitude: f64,
    drift_barrier: f64,
    drift_screening: f64,
    drift_threshold: f64,
    drift_gate_factor: f64,
    feedback_coefficient: f64,
    feedback_threshold_factor: f64,
    width_m: f64,
    multiplicity: f64,
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
            drift_amplitude,
            drift_barrier,
            drift_screening,
            drift_threshold,
            drift_gate_factor,
            feedback_coefficient: binned("ibpc1")?
                * if model.variant() == ModelVariant::HiSimHv2 {
                    1. + real("ibpc1l")? / (length * 1e6).powf(real("ibpc1lp")?)
                } else {
                    1.
                },
            feedback_threshold_factor: binned("ibpc2")?,
            width_m: instance.effective_geometry().channel_width_total_m,
            multiplicity: local("m")?,
        };
        result.validate()
    }

    fn validate(self) -> Result<Self, ModelError> {
        if ![
            self.gate_coefficient,
            self.body_coefficient,
            self.length_ratio,
            self.drain_coefficient,
            self.amplitude,
            self.barrier,
            self.drift_amplitude,
            self.drift_barrier,
            self.drift_screening,
            self.drift_threshold,
            self.drift_gate_factor,
            self.feedback_coefficient,
            self.feedback_threshold_factor,
            self.width_m,
            self.multiplicity,
        ]
        .into_iter()
        .all(f64::is_finite)
        {
            return Err(ModelError::InvalidPhysicalParameter(
                "HiSIM-HV impact coefficients",
            ));
        }
        Ok(self)
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
                bulk_potential_change_v: zero,
                bulk_feedback_current_a: zero,
            });
        }
        let current = promote_current(state.access.current);
        let surface = promote(charge.source_surface);
        let modified_source = promote(electro.modified_source_surface);
        let channel = if self.channel_enabled {
            let beta = Jet::coordinate(
                temperature.inverse_thermal_voltage,
                3,
                temperature.inverse_thermal_voltage_derivative,
            );
            let gate = self.gate_coefficient * promote(electro.poly.effective_gate);
            let factor = electro.material.charge_density_permittivity
                / electro.classical_oxide_capacitance.powi(2);
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
        let (bulk_potential_change_v, bulk_feedback_current_a) =
            self.bulk_feedback(state, channel + drift)?;
        Ok(HiSimHvImpactPoint {
            bulk_potential_change_v,
            bulk_feedback_current_a,
            channel_current_a: channel.checked("HiSIM-HV channel impact current")?,
            drift_current_a: drift.checked("HiSIM-HV drift impact current")?,
        })
    }
    fn bulk_feedback(
        &self,
        state: &HiSimHvNormalChannelWithAccessPoint,
        impact: Jet,
    ) -> Result<(Jet, Jet), ModelError> {
        let zero = Jet::constant(0.);
        if impact.value <= 0. || self.feedback_coefficient == 0. {
            return Ok((zero, zero));
        }
        let electro = &state.intrinsic.electrostatics;
        let delta = self.feedback_coefficient
            * (1. + self.feedback_threshold_factor * promote(electro.threshold.total_shift))
            * impact
            / self.multiplicity;
        // beta cancels between dIdd and betaWL. Apply the per-unit bulk
        // change to the full device geometry, including M exactly once.
        let current = self.width_m
            * self.multiplicity
            * promote(state.intrinsic.mobility.mobility_m2_per_vs)
            * promote(electro.material.poisson_coefficient)
            * delta
            * promote(electro.charge.bulk_feedback_shape)
            / promote(electro.modulation.effective_length_m);
        Ok((
            delta.checked("HiSIM-HV impact bulk-potential change")?,
            current.checked("HiSIM-HV impact bulk-feedback current")?,
        ))
    }
}
