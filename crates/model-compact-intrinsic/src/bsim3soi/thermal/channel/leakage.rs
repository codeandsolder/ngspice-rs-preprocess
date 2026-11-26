//! PD 2.2.3 field-induced leakage, gate tunneling, and impact ionization.
//! Copyright 1999/1990 Regents of the University of California. See package LICENSE.

use super::super::{Context, JunctionCurrent};
use super::{Bias, D, PreparedPdChannel, State};
use crate::{
    ModelError,
    preparation::{integer, positive},
};

/// A normalized per-device electrical current and its bias/thermal gradient.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PdCurrent {
    /// Current in amperes, before polarity and M.
    pub current_a: f64,
    /// VGS, VDS, VBS and absolute-temperature partial derivatives.
    pub gradient: [f64; 4],
}

impl PdCurrent {
    fn from_d(value: D) -> Result<Self, ModelError> {
        if !value.finite() {
            return Err(ModelError::InvalidPhysicalParameter("PD leakage current"));
        }
        Ok(Self {
            current_a: value.value,
            gradient: value.gradient,
        })
    }
}

/// Nonlinear PD leakage contributions. Impact ionization is composed with
/// the independently evaluated parasitic collector current at the same bias.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PdLeakage {
    /// Drain-to-body gate-induced leakage.
    pub drain_gidl: PdCurrent,
    /// Source-to-body gate-induced leakage.
    pub source_gidl: PdCurrent,
    /// Gate-to-body tunneling (signed).
    pub gate_body: PdCurrent,
    impact_ratio: f64,
    impact_gradient: [f64; 4],
    collector_fraction: f64,
}

impl PdLeakage {
    /// Combine the channel and collector contributions to impact ionization.
    ///
    /// The inputs must be normalized, per-device currents at the same
    /// bias and temperature as this leakage evaluation.
    ///
    /// # Errors
    /// A nonfinite current or gradient.
    pub fn impact_ionization(
        self,
        channel: PdCurrent,
        collector: JunctionCurrent,
    ) -> Result<PdCurrent, ModelError> {
        let channel = D {
            value: channel.current_a,
            gradient: channel.gradient,
        };
        let collector = D {
            value: collector.current_a,
            gradient: [
                0.0,
                collector.drain_conductance_s,
                collector.body_conductance_s,
                collector.temperature_derivative_a_per_k,
            ],
        };
        let ratio = D {
            value: self.impact_ratio,
            gradient: self.impact_gradient,
        };
        PdCurrent::from_d(ratio * (channel + collector.scale(self.collector_fraction)))
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Gidl {
    coefficient: f64,
    field: f64,
    onset: f64,
    width: [f64; 2],
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Tunnel {
    field_cap: f64,
    smooth: f64,
    oxide_ratio: f64,
    quantum_oxide: f64,
    energy_gap: f64,
    inversion: [f64; 4],
    accumulation: [f64; 4],
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Ionization {
    alpha: f64,
    beta: [f64; 3],
    saturation: f64,
    thermal: f64,
    length_correction: f64,
    step: [f64; 5],
    collector_fraction: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Leakage {
    gidl: Gidl,
    tunnel: Option<Tunnel>,
    ionization: Ionization,
}

impl Leakage {
    pub(super) fn prepare(c: &Context<'_>, oxide_ratio: Option<f64>) -> Result<Self, ModelError> {
        let pd = c
            .instance
            .geometry()
            .pd
            .ok_or(ModelError::MismatchedPreparedContext)?;
        Ok(Self {
            gidl: Gidl {
                coefficient: c.binned("agidl")?,
                field: c.binned("bgidl")?,
                onset: c.binned("ngidl")?,
                width: [pd.source_diode_width, pd.drain_diode_width],
            },
            tunnel: if integer(c.model.parameters(), "igmod")? >= 1 {
                Some(Tunnel {
                    field_cap: positive(c.model("voxh")?, "voxh")?,
                    smooth: positive(c.model("deltavox")?, "deltavox")?,
                    oxide_ratio: oxide_ratio.ok_or(ModelError::MismatchedPreparedContext)?,
                    quantum_oxide: positive(c.model("toxqm")?, "toxqm")?,
                    energy_gap: c.model("ebg")?,
                    inversion: [
                        c.model("alphagb1")?,
                        c.model("betagb1")?,
                        c.model("vgb1")?,
                        positive(c.model("vevb")?, "vevb")?,
                    ],
                    accumulation: [
                        c.model("alphagb2")?,
                        c.model("betagb2")?,
                        c.model("vgb2")?,
                        positive(c.model("vecb")?, "vecb")?,
                    ],
                })
            } else {
                None
            },
            ionization: Ionization {
                alpha: c.binned("alpha0")?,
                beta: [c.binned("beta0")?, c.binned("beta1")?, c.binned("beta2")?],
                saturation: c.binned("vdsatii0")?,
                thermal: c.model("tii")?,
                length_correction: c.binned("lii")?,
                step: [
                    c.binned("esatii")?,
                    c.binned("sii0")?,
                    c.binned("sii1")?,
                    c.binned("sii2")?,
                    c.binned("siid")?,
                ],
                collector_fraction: c.binned("fbjtii")?,
            },
        })
    }
}

const MAX_EXP: f64 = 2.688_117_142e43;
const MIN_EXP: f64 = 3.720_075_976e-44;

fn continued_exp(x: D) -> D {
    if x.value > 100.0 {
        x.chain(MAX_EXP * (x.value - 99.0), MAX_EXP)
    } else if x.value < -100.0 {
        D::constant(MIN_EXP)
    } else {
        x.exp()
    }
}

fn floor(x: D, min: f64) -> D {
    if x.value < min { D::constant(min) } else { x }
}

impl Gidl {
    fn evaluate(self, bias: D, width: f64, tox: f64) -> D {
        let field = (bias - D::constant(self.onset)).scale((3.0 * tox).recip());
        if self.coefficient <= 0.0 || self.field <= 0.0 || field.value <= 0.0 {
            return D::constant(0.0);
        }
        let exponent = D::constant(-self.field) / field;
        let exp = if exponent.value > -100.0 {
            exponent.exp()
        } else {
            D::constant(MIN_EXP)
        };
        (field * exp).scale(width * self.coefficient)
    }
}

struct Oxide {
    gate_body: D,
    flatband: f64,
    depletion: D,
    accumulation: D,
}

fn oxide(channel: &PreparedPdChannel, bias: &Bias, state: &State) -> Oxide {
    let gate_body = state.gate_effective - bias.body;
    let k1 = channel.electrostatics.effective_k1;
    let flatband =
        channel.sign * channel.electrostatics.threshold - bias.phi - k1 * bias.phi.sqrt();
    let difference = D::constant(flatband - 0.02) - gate_body;
    let root = (difference * difference + D::constant(0.08 * flatband.abs())).sqrt();
    let raw = (difference + root).scale(0.5);
    let accumulation = floor(raw, 0.0);
    let depletion = if k1.abs() == 0.0 {
        D::constant(0.0)
    } else {
        let drive =
            state.gate_effective - state.overdrive - D::constant(flatband) + raw - state.body;
        let potential = if drive.value < 0.0 {
            drive.scale(k1.recip())
        } else {
            // Stable version of k1/2 * (sqrt(1+4*drive/k1^2)-1).
            drive.scale(2.0 / k1)
                / ((D::constant(1.0) + drive.scale(4.0 / (k1 * k1))).sqrt() + D::constant(1.0))
        };
        gate_body - potential * potential - D::constant(flatband)
    };
    Oxide {
        gate_body,
        flatband,
        depletion,
        accumulation,
    }
}

impl Tunnel {
    fn limit(self, voltage: D) -> D {
        let t = D::constant(self.field_cap - self.smooth) - voltage;
        D::constant(self.field_cap)
            - (t + (t * t + D::constant(4.0 * self.smooth * self.field_cap)).sqrt()).scale(0.5)
    }

    fn evaluate(self, channel: &PreparedPdChannel, bias: &Bias, state: &State, width: D) -> D {
        let ox = oxide(channel, bias, state);
        let (voltage, aux, coefficients, prefactor, barrier) = if ox.gate_body.value >= 0.0 {
            let v = self.limit(ox.depletion);
            let aux =
                continued_exp((v - D::constant(self.energy_gap)).scale(self.inversion[3].recip()))
                    .ln_1p()
                    .scale(self.inversion[3]);
            (v, aux, self.inversion, 3.7622e-7, 3.1051e10)
        } else {
            let v = self.limit(ox.accumulation);
            let aux = continued_exp(
                (D::constant(ox.flatband) - ox.gate_body).scale(self.accumulation[3].recip()),
            )
            .ln_1p()
            .scale(self.accumulation[3]);
            (v, aux, self.accumulation, 4.9758e-7, 2.357e10)
        };
        let [alpha, beta, divisor, _] = coefficients;
        let denominator = if divisor.abs() == 0.0 {
            D::constant(1.0)
        } else {
            floor(D::constant(1.0) - voltage.scale(divisor.recip()), 0.01)
        };
        let exponential = continued_exp(
            (D::constant(alpha) - voltage.scale(beta)).scale(-barrier * self.quantum_oxide)
                / denominator,
        );
        // The source value depends on effective width. Its historical
        // hand-written gate current Jacobian omits that product derivative;
        // the forward differential retains the complete physical derivative.
        (width * ox.gate_body * aux * exponential)
            .scale(channel.length * prefactor * self.oxide_ratio / channel.segments)
    }
}

impl Ionization {
    fn evaluate(self, channel: &PreparedPdChannel, bias: &Bias, state: &State) -> D {
        if self.alpha <= 0.0 {
            return D::constant(0.0);
        }
        let saturation = (D::constant(1.0) + bias.ratio_delta.scale(self.thermal))
            .scale(self.saturation)
            - D::constant(self.length_correction / channel.length);
        let [field, step0, step1, step2, step_drain] = self.step;
        let length_field = field * channel.length;
        let gate_factor = step0 * length_field / (1.0 + length_field);
        let gate_step = (state.gate_effective - state.threshold)
            * ((D::constant(1.0) + state.overdrive.scale(step1)).reciprocal() + D::constant(step2))
            / (D::constant(1.0) + bias.drain.scale(step_drain));
        let difference = bias.drain - saturation - gate_step.scale(gate_factor);
        let [beta0, beta1, beta2] = self.beta;
        let denominator = floor(
            D::constant(beta2) + difference.scale(beta1) + (difference * difference).scale(beta0),
            1e-5,
        );
        let exponent = difference / denominator;
        let ratio = if exponent.value > 100.0 {
            D::constant(MAX_EXP * self.alpha)
        } else if exponent.value < -100.0 {
            D::constant(MIN_EXP * self.alpha)
        } else {
            exponent.exp().scale(self.alpha)
        };
        if ratio.value > 10.0 {
            D::constant(10.0)
        } else {
            ratio
        }
    }
}

impl PreparedPdChannel {
    pub(super) fn leakage(
        &self,
        bias: &Bias,
        state: &State,
        width: D,
    ) -> Result<PdLeakage, ModelError> {
        let p = self.leakage;
        let gidl = p.gidl;
        let ratio = p.ionization.evaluate(self, bias, state);
        if !ratio.finite() {
            return Err(ModelError::InvalidPhysicalParameter("PD impact ratio"));
        }
        Ok(PdLeakage {
            drain_gidl: PdCurrent::from_d(gidl.evaluate(
                bias.drain - state.gate_effective,
                gidl.width[1],
                self.tox,
            ))?,
            source_gidl: PdCurrent::from_d(gidl.evaluate(
                -state.gate_effective,
                gidl.width[0],
                self.tox,
            ))?,
            gate_body: PdCurrent::from_d(p.tunnel.map_or_else(
                || D::constant(0.0),
                |t| t.evaluate(self, bias, state, width),
            ))?,
            impact_ratio: ratio.value,
            impact_gradient: ratio.gradient,
            collector_fraction: p.ionization.collector_fraction,
        })
    }
}
