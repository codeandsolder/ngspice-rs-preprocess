//! Native HiSIM-HV shallow-trench-isolation edge channel current.
use super::{
    HiSimHvChannelElectrostaticPoint, HiSimHvChannelMobilityPoint, HiSimHvChannelRegion,
    HiSimHvIntrinsicBias, HiSimHvIntrinsicScalar as Jet, PreparedHiSimHvInstance,
    PreparedHiSimHvModel, PreparedHiSimHvTemperature,
    instance::model_real,
    instance_real_at, lookup_slot,
    smoothing::{positive, unit_ceiling_power4},
};
use crate::{ModelError, ModelVariant, ParameterScope, ParameterValue};

const SILICON_PERMITTIVITY: f64 = 1.034_943e-10;
const ELECTRON_CHARGE: f64 = 1.602_191_8e-19;
const SMALL: f64 = 1e-25;
const PSISTI_SMOOTHING: f64 = 0.005;
const STI_SMOOTHING: f64 = 0.002;
const CHARGE_SMOOTHING: f64 = 0.01;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct StiCorrection {
    enabled: bool,
    variant: ModelVariant,
    width_factor: f64,
    doping: f64,
    poisson_factor: f64,
    short_channel_factor: f64,
    flatband: f64,
    threshold_shift: f64,
    drain_threshold_factor: f64,
    built_in: f64,
    scsti1: f64,
    scsti2: f64,
}

impl StiCorrection {
    pub(super) fn prepare(
        model: &PreparedHiSimHvModel,
        instance: &PreparedHiSimHvInstance,
        multiplicity: f64,
    ) -> Result<Self, ModelError> {
        let binned = |name| {
            let slot = lookup_slot(model.variant(), name)?;
            instance
                .binned_model_slot(slot)
                .ok_or(ModelError::MissingDefaultDependency(slot))
        };
        let slot = lookup_slot(model.variant(), "coisti")?;
        let enabled = match model.get_slot(slot) {
            Some(ParameterValue::Integer(value)) => *value != 0,
            _ => return Err(ModelError::MissingDefaultDependency(slot)),
        };
        let geometry = instance.effective_geometry();
        let nf_slot = model
            .variant()
            .parameter(ParameterScope::Instance, "nf")
            .ok_or_else(|| ModelError::UnknownParameter("nf".to_owned()))?
            .slot;
        let nf = instance_real_at(instance, nf_slot)?;
        let doping = binned("nsti")? * 1e6;
        let length = geometry.small_device_gate_length_m - model_real(model, "parl2")?;
        let result = Self {
            enabled: enabled && geometry.width_sti != 0.,
            variant: model.variant(),
            width_factor: 2. * geometry.width_sti * nf * multiplicity,
            doping,
            poisson_factor: (2. * ELECTRON_CHARGE * doping * SILICON_PERMITTIVITY).sqrt(),
            short_channel_factor: 2. * SILICON_PERMITTIVITY / (length * length),
            flatband: binned("vfbc")?,
            threshold_shift: binned("vthsti")?,
            drain_threshold_factor: model_real(model, "vdsti")?,
            built_in: model_real(model, "vbi")?,
            scsti1: binned("scsti1")?,
            scsti2: binned("scsti2")?,
        };
        if result.enabled
            && (doping <= 0.
                || ![
                    result.width_factor,
                    result.poisson_factor,
                    result.short_channel_factor,
                    result.flatband,
                    result.threshold_shift,
                    result.drain_threshold_factor,
                    result.built_in,
                    result.scsti1,
                    result.scsti2,
                ]
                .into_iter()
                .all(f64::is_finite))
        {
            return Err(ModelError::InvalidPhysicalParameter(
                "HiSIM-HV STI coefficients",
            ));
        }
        Ok(result)
    }

    pub(super) fn evaluate(
        &self,
        bias: HiSimHvIntrinsicBias,
        state: &HiSimHvChannelElectrostaticPoint,
        mobility: &HiSimHvChannelMobilityPoint,
        temperature: &PreparedHiSimHvTemperature,
    ) -> Result<Jet, ModelError> {
        if !self.enabled
            || matches!(
                state.charge.region,
                HiSimHvChannelRegion::Accumulation | HiSimHvChannelRegion::NoInversion
            )
        {
            return Ok(Jet::constant(0.));
        }
        let beta = Jet::coordinate(
            temperature.inverse_thermal_voltage,
            3,
            temperature.inverse_thermal_voltage_derivative,
        );
        let carrier = Jet::coordinate(
            temperature.intrinsic_carrier_concentration_m3,
            3,
            temperature.intrinsic_carrier_derivative_m3_per_kelvin,
        );
        let drain = Jet::coordinate(bias.vds, 1, 1.);
        let gate = state.conditioned.modified_gate;
        let body = state.conditioned.modified_body;
        let shift = self.short_channel_factor
            * (self.built_in - state.threshold.reference_bulk_potential)
            * state.oxide.inverse_capacitance
            * state.material.bulk_depletion_width_factor
            * state.threshold.depletion_potential_sum.sqrt()
            * (self.scsti1 + self.scsti2 * state.conditioned.modified_drain);
        let effective_gate = gate - self.flatband + self.threshold_shift
            - self.drain_threshold_factor * drain
            + shift;
        let charge_factor = self.poisson_factor / beta.sqrt();
        let minority = carrier / self.doping;
        let minority = minority * minority;
        let oxide_factor = charge_factor * state.oxide.inverse_capacitance;
        let oxide_factor = oxide_factor * oxide_factor;
        let potential_floor =
            1. / beta - 0.25 * oxide_factor * beta + self.flatband - self.threshold_shift - shift
                + SMALL;
        let abs_floor = if potential_floor.value < 0. {
            -potential_floor
        } else {
            potential_floor
        };
        let gate_floor = potential_floor
            + (gate - potential_floor - PSISTI_SMOOTHING)
                .half_positive(4. * PSISTI_SMOOTHING * abs_floor);
        let target = gate_floor - self.flatband + self.threshold_shift + shift - body;
        let hv2_small = if self.variant == ModelVariant::HiSimHv2 {
            SMALL
        } else {
            0.
        };
        let sqrt_arg = positive(
            1. + 4. * (beta * target - 1.) / (oxide_factor * beta * beta),
            CHARGE_SMOOTHING,
        ) + hv2_small;
        let first = effective_gate + 0.5 * oxide_factor * beta * (1. - sqrt_arg.sqrt());
        let gate_squared = effective_gate * effective_gate;
        let second = (gate_squared / (minority * oxide_factor)).ln()
            / (beta + 2. / (effective_gate + SMALL));
        let diff = second - first - STI_SMOOTHING;
        let root = (diff * diff + 4. * STI_SMOOTHING * second).sqrt();
        let surface = if diff.value >= 0. {
            first + STI_SMOOTHING - 2. * STI_SMOOTHING * second / (root + diff)
        } else {
            second - 2. * STI_SMOOTHING * second / (root - diff)
        };
        let charge_argument = beta * (surface - body) - 1.;
        let inversion_argument = minority * (beta * surface).exp();
        let depletion = positive(charge_argument, CHARGE_SMOOTHING);
        let total = positive(charge_argument + inversion_argument, CHARGE_SMOOTHING);
        let epsilon = if self.variant == ModelVariant::HiSimHv2 {
            SMALL
        } else {
            10. * f64::EPSILON
        };
        // p(x) - delta^2/p(x) = x, giving an exact rational difference
        // of the two smoothed roots without subtracting nearly equal charges.
        let positive_difference =
            inversion_argument / (1. + (CHARGE_SMOOTHING / total) * (CHARGE_SMOOTHING / depletion));
        let density = charge_factor * positive_difference
            / ((total + epsilon).sqrt() + (depletion + epsilon).sqrt());
        let saturation = positive(first - surface, 0.1) + hv2_small;
        let drain_factor = unit_ceiling_power4(drain / saturation);
        (self.width_factor * mobility.mobility_m2_per_vs * density * drain_factor
            / (beta * state.modulation.effective_length_m))
            .checked("HiSIM-HV STI current")
    }
}
