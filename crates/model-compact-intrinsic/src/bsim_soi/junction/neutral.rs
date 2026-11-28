//! Neutral-body recombination, bipolar transport, and diffusion storage inputs.
//! Copyright 2010 Regents of the University of California. See package LICENSE.

use super::{
    Bias, BsimSoiJunctionCurrent, BsimSoiJunctionDensities, BsimSoiThermalScalar, ExponentialPoint,
    K_OVER_Q, SideLaw, binned, exp_minus_one, forward_exp, limited_exp,
};
use crate::ParameterScope;
use crate::bsim_soi::{PreparedBsimSoiSize, positive, real, slot};
use crate::{BoundParameters, ModelError, ParameterValue};

/// Collector current from the physical drain to the physical source.
/// Its voltage derivatives use independent physical B-S and B-D biases.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BsimSoiCollectorCurrent {
    pub current: f64,
    pub d_body_source: f64,
    pub d_body_drain: f64,
    pub d_temperature: f64,
}

/// Neutral-body recombination and bipolar transport, with effective
/// diffusion currents for the later junction charge equations.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BsimSoiNeutralBody {
    pub source_recombination: BsimSoiJunctionCurrent,
    pub drain_recombination: BsimSoiJunctionCurrent,
    pub source_diffusion: BsimSoiJunctionCurrent,
    pub drain_diffusion: BsimSoiJunctionCurrent,
    pub collector: BsimSoiCollectorCurrent,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct NeutralLaw {
    alpha: f64,
    length_ratio: f64,
    diffusion_ratio: f64,
    early: f64,
    area: f64,
    collector_disabled: bool,
}

#[derive(Clone, Copy)]
struct NeutralSide {
    exponential: ExponentialPoint,
    injection: ExponentialPoint,
    bipolar: BsimSoiThermalScalar,
}

impl NeutralSide {
    fn evaluate(law: SideLaw, bias: Bias, densities: BsimSoiJunctionDensities) -> Self {
        let voltage = BsimSoiThermalScalar {
            value: law.ideality * K_OVER_Q * bias.temperature,
            d_temperature: if bias.live {
                law.ideality * K_OVER_Q
            } else {
                0.
            },
        };
        let mut exponential = forward_exp(bias.voltage, voltage);
        exponential.value = exp_minus_one(bias.voltage / voltage.value, exponential.value);
        let high = densities.high_level_injection;
        let value = high.value * exponential.value;
        let injection = if value < 1e-5 {
            ExponentialPoint {
                value: 0.,
                d_voltage: 0.,
                d_temperature: 0.,
            }
        } else {
            ExponentialPoint {
                value,
                d_voltage: high.value * exponential.d_voltage,
                d_temperature: high.value.mul_add(
                    exponential.d_temperature,
                    high.d_temperature * exponential.value,
                ),
            }
        };
        Self {
            exponential,
            injection,
            bipolar: densities.bipolar,
        }
    }

    fn reduced_exponential(self) -> ExponentialPoint {
        let base = 1. + self.injection.value;
        let factor = 1. / base.sqrt();
        let slope = -0.5 * factor / base;
        ExponentialPoint {
            value: self.exponential.value * factor,
            d_voltage: factor.mul_add(
                self.exponential.d_voltage,
                self.exponential.value * slope * self.injection.d_voltage,
            ),
            d_temperature: factor.mul_add(
                self.exponential.d_temperature,
                self.exponential.value * slope * self.injection.d_temperature,
            ),
        }
    }

    fn current(
        self,
        coefficient: f64,
        polarity: f64,
    ) -> Result<BsimSoiJunctionCurrent, ModelError> {
        let exp = self.reduced_exponential();
        BsimSoiJunctionCurrent {
            current: polarity * coefficient * self.bipolar.value * exp.value,
            conductance: coefficient * self.bipolar.value * exp.d_voltage,
            d_temperature: polarity
                * coefficient
                * self
                    .bipolar
                    .value
                    .mul_add(exp.d_temperature, self.bipolar.d_temperature * exp.value),
        }
        .checked()
    }
}

impl NeutralLaw {
    fn collector(
        self,
        biases: [Bias; 2],
        sides: [NeutralSide; 2],
    ) -> Result<BsimSoiCollectorCurrent, ModelError> {
        let [source, drain] = sides;
        let [bs, bd] = biases;
        if self.collector_disabled || bs.voltage - bd.voltage == 0. {
            return Ok(BsimSoiCollectorCurrent::default());
        }
        let density = if bs.voltage > bd.voltage {
            drain.bipolar
        } else {
            source.bipolar
        };
        let early = (bs.voltage + bd.voltage) / self.early + 1.;
        let injection = source.injection.value + drain.injection.value;
        let radical = early.mul_add(early, 4. * injection).sqrt();
        let denominator = f64::midpoint(early, radical);
        // Source charge-control denominator floor.
        let (factor, slope) = if denominator < 0.1 {
            (10., 0.)
        } else {
            (1. / denominator, -1. / denominator.powi(2))
        };
        let charge_slope = |early_slope: f64, injection_slope: f64| -> f64 {
            if slope == 0. {
                return 0.;
            }
            slope * 0.5 * (early_slope + early.mul_add(early_slope, 2. * injection_slope) / radical)
        };
        let diff = source.exponential.value - drain.exponential.value;
        let prefactor = self.area * self.alpha * self.length_ratio;
        let voltage_slope = 1. / self.early;
        let source_factor_slope = charge_slope(voltage_slope, source.injection.d_voltage);
        let drain_factor_slope = charge_slope(voltage_slope, drain.injection.d_voltage);
        let thermal_factor_slope = charge_slope(
            0.,
            source.injection.d_temperature + drain.injection.d_temperature,
        );
        let product = diff * factor;
        let result = BsimSoiCollectorCurrent {
            current: bs.polarity * prefactor * density.value * product,
            d_body_source: prefactor
                * density.value
                * factor.mul_add(source.exponential.d_voltage, diff * source_factor_slope),
            d_body_drain: prefactor
                * density.value
                * factor.mul_add(-drain.exponential.d_voltage, diff * drain_factor_slope),
            d_temperature: bs.polarity
                * prefactor
                * density.value.mul_add(
                    factor.mul_add(
                        source.exponential.d_temperature - drain.exponential.d_temperature,
                        diff * thermal_factor_slope,
                    ),
                    density.d_temperature * product,
                ),
        };
        if [
            result.current,
            result.d_body_source,
            result.d_body_drain,
            result.d_temperature,
        ]
        .into_iter()
        .all(f64::is_finite)
        {
            Ok(result)
        } else {
            Err(ModelError::InvalidPhysicalParameter(
                "neutral body collector",
            ))
        }
    }

    pub(super) fn evaluate(
        self,
        biases: [Bias; 2],
        laws: &[SideLaw; 2],
        densities: [BsimSoiJunctionDensities; 2],
        disabled: bool,
    ) -> Result<BsimSoiNeutralBody, ModelError> {
        if disabled || densities.iter().all(|side| side.bipolar.value == 0.) {
            return Ok(BsimSoiNeutralBody::default());
        }
        let sides = std::array::from_fn(|index| {
            NeutralSide::evaluate(laws[index], biases[index], densities[index])
        });
        let [source, drain] = sides;
        let polarity = biases[0].polarity;
        let neutral = self.area * (1. - self.alpha) * self.length_ratio;
        let storage = self.area * self.diffusion_ratio;
        Ok(BsimSoiNeutralBody {
            source_recombination: source.current(neutral, polarity)?,
            drain_recombination: drain.current(neutral, polarity)?,
            source_diffusion: source.current(storage, polarity)?,
            drain_diffusion: drain.current(storage, polarity)?,
            collector: self.collector(biases, sides)?,
        })
    }
}

pub(super) fn prepare(
    model: &BoundParameters,
    instance: &BoundParameters,
    size: &PreparedBsimSoiSize,
    area: f64,
) -> Result<NeutralLaw, ModelError> {
    let length = size.geometry().leff;
    let ln = real(model, "ln")?.max(1e-15);
    let alpha = limited_exp(-0.5 * (length / ln).powi(2)).value;
    let ratio = binned(size, "lbjt0")? * (1. / length + 1. / ln);
    let length_ratio = ratio.powf(binned(size, "nbjt")?);
    let diffusion_ratio = real(model, "ldif0")?.mul_add(ratio.powf(binned(size, "ndif")?), 1.);
    let early = binned(size, "aely")?
        .mul_add(length, binned(size, "vabjt")?)
        .max(1.);
    if ![alpha, length_ratio, diffusion_ratio, early]
        .into_iter()
        .all(f64::is_finite)
    {
        return Err(ModelError::InvalidPhysicalParameter(
            "neutral body geometry",
        ));
    }
    let off_slot = slot(ParameterScope::Instance, "bjtoff")?;
    let collector_disabled = matches!(
        instance.get_slot(off_slot),
        Some(ParameterValue::Integer(1))
    );
    Ok(NeutralLaw {
        alpha,
        length_ratio,
        diffusion_ratio,
        early,
        area: positive(area, "neutral body area")?,
        collector_disabled,
    })
}
