//! Native MOS and bipolar impact-ionization currents.
//! Copyright 2010 Regents of the University of California. See package LICENSE.
use super::super::{PreparedBsimSoiJunctions, PreparedBsimSoiSetup, binned, integer, real};
use super::{
    BsimSoiChannelBias, BsimSoiChannelPoint, BsimSoiChannelScalar, Jet, PreparedBsimSoiChannel,
    PreparedBsimSoiDrainChannel,
};
use crate::ModelError;

/// Physical body-junction voltages, independent of the intrinsic body node.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BsimSoiImpactBias {
    pub channel: BsimSoiChannelBias,
    pub body_source_junction: f64,
    pub body_drain_junction: f64,
}

/// Signed current from the indicated physical terminal into the body.
/// The five ordinary bias partials hold both junction voltages fixed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BsimSoiImpactBranch {
    pub current: BsimSoiChannelScalar,
    pub d_body_source_junction: f64,
    pub d_body_drain_junction: f64,
}
impl BsimSoiImpactBranch {
    const ZERO: Self = Self {
        current: Jet::constant(0.),
        d_body_source_junction: 0.,
        d_body_drain_junction: 0.,
    };
}

/// The active high-field terminal changes when conduction reverses.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BsimSoiImpactIonization {
    pub drain_to_body: BsimSoiImpactBranch,
    pub source_to_body: BsimSoiImpactBranch,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct MosLaw {
    alpha: f64,
    beta: [f64; 3],
    voltage: f64,
    temperature: f64,
    length_offset: f64,
    gate_scale: f64,
    gate_slope: f64,
    gate_offset: f64,
    drain_slope: f64,
}

fn bounded_exp(argument: Jet) -> Jet {
    if argument.value > 100. {
        Jet::constant(2.688_117_142e43)
    } else {
        argument.floor_exp()
    }
}
impl MosLaw {
    fn ratio(self, point: &BsimSoiChannelPoint, rise: Jet) -> Jet {
        if self.alpha <= 0. {
            return Jet::constant(0.);
        }
        let base = self.voltage * (1. + self.temperature * rise) - self.length_offset;
        let raw_overdrive = point.gate_voltage - point.threshold;
        let gate = self.gate_scale
            * raw_overdrive
            * (1. / (1. + self.gate_slope * point.effective_overdrive) + self.gate_offset);
        let difference =
            point.drain_voltage - base - gate / (1. + self.drain_slope * point.drain_voltage);
        let denominator = self.beta[0] + difference * (self.beta[1] + self.beta[2] * difference);
        let denominator = if denominator.value < 1e-5 {
            Jet::constant(1e-5)
        } else {
            denominator
        };
        let ratio = self.alpha * bounded_exp(difference / denominator);
        if ratio.value > 10. {
            Jet::constant(10.)
        } else {
            ratio
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum BipolarLaw {
    SharedRatio(f64),
    Independent {
        scale: f64,
        voltage: f64,
        temperature: f64,
        activation: f64,
        exponent: f64,
    },
}
impl BipolarLaw {
    fn factor(self, mos_ratio: Jet, rise: Jet, reverse_bias: Jet) -> Jet {
        match self {
            Self::SharedRatio(scale) => scale * mos_ratio,
            Self::Independent {
                scale,
                voltage,
                temperature,
                activation,
                exponent,
            } => {
                let difference = voltage * (1. + temperature * rise) - reverse_bias;
                let argument = if difference.value <= 0. {
                    Jet::constant(0.)
                } else {
                    -activation * (exponent * difference.ln()).exp()
                };
                scale * difference * bounded_exp(argument)
            }
        }
    }
}

/// Bind both impact-ionization formulations to the physical channel and BJT law.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreparedBsimSoiImpactIonization {
    channel: PreparedBsimSoiChannel,
    drain: PreparedBsimSoiDrainChannel,
    junction: PreparedBsimSoiJunctions,
    mos: MosLaw,
    bipolar: BipolarLaw,
}
impl PreparedBsimSoiImpactIonization {
    /// Evaluate channel and bipolar avalanche branches.
    /// # Errors
    /// Invalid physical bias or nonfinite constitutive result.
    pub fn evaluate(&self, bias: BsimSoiImpactBias) -> Result<BsimSoiImpactIonization, ModelError> {
        if self.channel.fd.is_some_and(|fd| fd.ideal) {
            return Ok(BsimSoiImpactIonization {
                drain_to_body: BsimSoiImpactBranch::ZERO,
                source_to_body: BsimSoiImpactBranch::ZERO,
            });
        }
        let point = self.channel.evaluate(bias.channel)?;
        let material = self.channel.material.evaluate(bias.channel.kelvin)?;
        let rise = Jet::coordinate(
            material.kelvin / self.channel.nominal - 1.,
            4,
            if self.channel.material.network().is_some() {
                1. / self.channel.nominal
            } else {
                0.
            },
        );
        let orientation = if point.reversed { -1. } else { 1. };
        let sign = self.channel.polarity;
        let mos_current = self.drain.evaluate(bias.channel)?.current * (orientation * sign);
        let collector = self
            .junction
            .evaluate(
                bias.channel.kelvin,
                bias.body_source_junction,
                bias.body_drain_junction,
            )?
            .neutral_body
            .collector;
        let bipolar_current =
            Jet::coordinate(collector.current, 4, collector.d_temperature) * (orientation * sign);
        let source_junction = if point.reversed {
            bias.body_drain_junction
        } else {
            bias.body_source_junction
        };
        let reverse = sign * source_junction - point.drain_voltage;
        let ratio = self.mos.ratio(&point, rise);
        let factor = self.bipolar.factor(ratio, rise, reverse);
        let factor_junction_derivative = self
            .bipolar
            .factor(
                Jet::constant(ratio.value),
                Jet::constant(rise.value),
                Jet::coordinate(reverse.value, 0, sign),
            )
            .gradient[0];
        let current = (sign * (ratio * mos_current + factor * bipolar_current))
            .checked("impact ionization")?;
        let derivative = sign * bipolar_current.value * factor_junction_derivative;
        let branch = BsimSoiImpactBranch {
            current,
            d_body_source_junction: (orientation * factor.value).mul_add(
                collector.d_body_source,
                if point.reversed { 0. } else { derivative },
            ),
            d_body_drain_junction: (orientation * factor.value).mul_add(
                collector.d_body_drain,
                if point.reversed { derivative } else { 0. },
            ),
        };
        if !branch.d_body_source_junction.is_finite() || !branch.d_body_drain_junction.is_finite() {
            return Err(ModelError::InvalidPhysicalParameter(
                "impact junction derivative",
            ));
        }
        Ok(if point.reversed {
            BsimSoiImpactIonization {
                drain_to_body: BsimSoiImpactBranch::ZERO,
                source_to_body: branch,
            }
        } else {
            BsimSoiImpactIonization {
                drain_to_body: branch,
                source_to_body: BsimSoiImpactBranch::ZERO,
            }
        })
    }
}
impl PreparedBsimSoiSetup {
    /// Bind the native MOS and bipolar impact-ionization law.
    /// # Errors
    /// Missing instance or model inputs or invalid constitutive domain.
    pub fn prepare_impact_ionization(
        &self,
        index: usize,
        ambient: f64,
    ) -> Result<PreparedBsimSoiImpactIonization, ModelError> {
        let channel = self.prepare_channel(index, ambient)?;
        let drain = self.prepare_drain_channel(index, ambient)?;
        let junction = self.prepare_junctions(index, ambient)?;
        let model = self.model.parameters();
        let size = self
            .sizes
            .get(index)
            .ok_or(ModelError::MismatchedPreparedContext)?;
        let get = |name| binned(size, name);
        let length = size.geometry().leff;
        let field = get("esatii")? * length;
        let mos = MosLaw {
            alpha: get("alpha0")?,
            beta: [get("beta2")?, get("beta1")?, get("beta0")?],
            voltage: get("vdsatii0")?,
            temperature: real(model, "tii")?,
            length_offset: get("lii")? / length,
            gate_scale: get("sii0")? * field / (1. + field),
            gate_slope: get("sii1")?,
            gate_offset: get("sii2")?,
            drain_slope: get("siid")?,
        };
        let bipolar = if integer(model, "iiimod")? == 0 {
            BipolarLaw::SharedRatio(get("fbjtii")?)
        } else {
            BipolarLaw::Independent {
                scale: get("cbjtii")? / length + get("ebjtii")?,
                voltage: get("vbci")?,
                temperature: real(model, "tvbci")?,
                activation: get("abjtii")?,
                exponent: get("mbjtii")? - 1.,
            }
        };
        Ok(PreparedBsimSoiImpactIonization {
            channel,
            drain,
            junction,
            mos,
            bipolar,
        })
    }
}
