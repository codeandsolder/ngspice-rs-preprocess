//! Bias-local SOI junction currents, including the family-specific bipolar transport.
//! Copyright 1999/1990 Regents of the University of California. See package LICENSE.

mod charge;

use charge::ChargeLaw;
pub use charge::JunctionCharge;

use super::{Context, JunctionDensities, K_OVER_Q, differential::Differential as D, finite};
use crate::{
    ModelError, ModelVariant,
    preparation::{integer, positive},
};

/// A current and its analytic Newton tangents in normalized coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct JunctionCurrent {
    /// Current in amperes, before instance multiplicity and polarity stamping.
    pub current_a: f64,
    /// Partial derivative with respect to normalized VBS.
    pub body_conductance_s: f64,
    /// Partial derivative with respect to normalized VDS.
    pub drain_conductance_s: f64,
    /// Partial derivative with respect to absolute temperature.
    pub temperature_derivative_a_per_k: f64,
}

impl JunctionCurrent {
    fn from_d(value: D) -> Result<Self, ModelError> {
        if !value.finite() {
            return Err(ModelError::InvalidPhysicalParameter(
                "junction current or tangent",
            ));
        }
        Ok(Self {
            current_a: value.value,
            body_conductance_s: value.gradient[0],
            drain_conductance_s: value.gradient[1],
            temperature_derivative_a_per_k: value.gradient[2],
        })
    }
}

/// The four physical contributions to one body-to-diffusion junction.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct JunctionComponents {
    /// Ideal diffusion.
    pub diffusion: JunctionCurrent,
    /// Depletion/trap-assisted recombination.
    pub recombination: JunctionCurrent,
    /// Neutral-body bipolar recombination.
    pub bipolar: JunctionCurrent,
    /// Reverse tunnelling.
    pub tunneling: JunctionCurrent,
    /// Sum of the four components.
    pub total: JunctionCurrent,
    /// PD effective diffusion current used for stored charge.
    pub stored_charge_diffusion: JunctionCurrent,
}

/// Source and drain junction currents and charges, and parasitic collector current.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct JunctionEvaluation {
    /// Body-to-source current components.
    pub source: JunctionComponents,
    /// Body-to-drain current components.
    pub drain: JunctionComponents,
    /// Drain-to-source parasitic bipolar collector current.
    pub collector: JunctionCurrent,
    /// Intrinsic body-to-source stored charge and tangents.
    pub source_charge: JunctionCharge,
    /// Intrinsic body-to-drain stored charge and tangents.
    pub drain_charge: JunctionCharge,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum TemperatureLaw {
    Dd {
        exponent: [f64; 4],
        ambient_gap: f64,
        selfheat_tunneling: f64,
    },
    Pd {
        exponent: [f64; 4],
        injection: f64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct DensityLaw {
    base: [f64; 4],
    nominal: f64,
    law: TemperatureLaw,
}

#[derive(Clone, Copy)]
struct Densities {
    bipolar: D,
    diffusion: D,
    recombination: D,
    tunneling: D,
    injection: D,
}

fn limited_exp(x: D, pd: bool) -> D {
    if pd {
        if x.value > 100.0 {
            x.chain(2.688_117_142e43 * (x.value - 99.0), 2.688_117_142e43)
        } else if x.value < -100.0 {
            D::constant(3.720_075_976e-44)
        } else {
            x.exp()
        }
    } else if x.value < 30.0 {
        x.exp()
    } else {
        x.chain(1.0686e13 * (x.value - 29.0), 1.0686e13)
    }
}

fn diode_exp_m1(x: D, pd: bool) -> D {
    if (pd && (-100.0..=100.0).contains(&x.value)) || (!pd && x.value < 30.0) {
        x.exp_m1()
    } else {
        limited_exp(x, pd) - D::constant(1.0)
    }
}

impl DensityLaw {
    pub(super) fn prepare(c: &Context<'_>) -> Result<Self, ModelError> {
        let n = positive(c.binned("ndiode")?, "ndiode")?;
        let xbjt = c.model("xbjt")? / n;
        let xdif = c.model.diffusion_temperature_exponent() / n;
        let xrec = c.model("xrec")?;
        let xtun = c.model("xtun")?;
        let law = if c.pd() {
            TemperatureLaw::Pd {
                exponent: [
                    xbjt,
                    xdif,
                    xrec / positive(c.binned("nrecf0")?, "nrecf0")?,
                    xtun,
                ],
                injection: c.binned("ahli")?,
            }
        } else {
            TemperatureLaw::Dd {
                exponent: [
                    xbjt,
                    xdif,
                    xrec / n / 2.0,
                    xtun / positive(c.binned("ntun")?, "ntun")?,
                ],
                ambient_gap: c.material.nominal_gap / n,
                selfheat_tunneling: c.model("istun")?,
            }
        };
        Ok(Self {
            base: [
                c.binned("isbjt")?,
                c.binned("isdif")?,
                c.binned("isrec")?,
                c.binned("istun")?,
            ],
            nominal: positive(c.model("tnom")?, "tnom")?,
            law,
        })
    }

    fn evaluate(self, t: D, selfheat: bool, n: f64) -> Densities {
        let ratio = t.scale(self.nominal.recip());
        let delta = ratio - D::constant(1.0);
        let vt = t.scale(K_OVER_Q);
        match self.law {
            TemperatureLaw::Dd {
                exponent,
                ambient_gap,
                selfheat_tunneling,
            } => {
                let gap = if selfheat { 1.115 / n } else { ambient_gap };
                let activation = (delta / vt).scale(gap);
                let exponential = activation.exp();
                let base_tun = if selfheat {
                    selfheat_tunneling
                } else {
                    self.base[3]
                };
                Densities {
                    bipolar: (ratio.powf(exponent[0]) * exponential).scale(self.base[0]),
                    diffusion: (ratio.powf(exponent[1]) * exponential).scale(self.base[1]),
                    recombination: (ratio.powf(exponent[2]) * exponential.sqrt())
                        .scale(self.base[2]),
                    tunneling: ratio.powf(exponent[3]).scale(base_tun),
                    injection: D::constant(0.0),
                }
            }
            TemperatureLaw::Pd {
                exponent,
                injection,
            } => {
                let activation = (delta / vt).scale(1.115);
                let bjt = limited_exp(activation.scale(exponent[0]), true);
                Densities {
                    bipolar: bjt.scale(self.base[0]),
                    diffusion: limited_exp(activation.scale(exponent[1]), true).scale(self.base[1]),
                    recombination: limited_exp(activation.scale(exponent[2]), true)
                        .scale(self.base[2]),
                    tunneling: limited_exp(delta.scale(exponent[3]), true).scale(self.base[3]),
                    injection: bjt.scale(injection),
                }
            }
        }
    }

    pub(super) fn ambient(self, c: &Context<'_>) -> Result<JunctionDensities, ModelError> {
        let d = self.evaluate(D::constant(c.material.kelvin), false, c.binned("ndiode")?);
        Ok(JunctionDensities {
            bipolar: finite(d.bipolar.value, "jbjt")?,
            diffusion: finite(d.diffusion.value, "jdif")?,
            recombination: finite(d.recombination.value, "jrec")?,
            tunneling: finite(d.tunneling.value, "jtun")?,
            high_level_injection: if c.pd() {
                Some(finite(d.injection.value, "ahli0")?)
            } else {
                None
            },
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Dd {
    area: f64,
    length: f64,
    diffusion_length: f64,
    length_modulation: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Pd {
    area: [f64; 2],
    neutral_area: f64,
    recombination_ideality: [f64; 2],
    recombination_temperature: [f64; 2],
    recombination_limit: f64,
    tunneling_limit: f64,
    alpha: f64,
    length_ratio: f64,
    diffusion_ratio: f64,
    early_voltage: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Law {
    Fd,
    Dd(Dd),
    Pd(Pd),
}

/// Instance-local nonlinear junction evaluator for a particular model and layout.
///
/// The caller supplies normalized source-oriented voltages (VDS >= 0) and
/// the absolute device temperature after solver limiting. Values are
/// normalized, per-device currents; the caller applies polarity and M.
/// FD explicitly returns zero, as the original FD load routine does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreparedJunctions {
    law: Law,
    charge: ChargeLaw,
    density: DensityLaw,
    ambient: f64,
    selfheat: bool,
    bjt_disabled: bool,
    diode_ideality: f64,
    tunneling_ideality: f64,
}

fn pd_layout(c: &Context<'_>) -> Result<Pd, ModelError> {
    let g = c.instance.geometry();
    let pd = g.pd.ok_or(ModelError::MismatchedPreparedContext)?;
    let film = positive(c.model("tsi")?, "tsi")?;
    let length = c.model("ln")?.max(1e-15);
    let ratio = g.length_m / length;
    let t0 = c.binned("lbjt0")? * (g.length_m.recip() + length.recip());
    let alpha = limited_exp(D::constant(-0.5 * ratio * ratio), true).value;
    Ok(Pd {
        area: [pd.source_diode_width * film, pd.drain_diode_width * film],
        neutral_area: g.width_m / positive(c.instance("nseg")?, "nseg")? * film,
        recombination_ideality: [
            positive(c.binned("nrecf0")?, "nrecf0")?,
            positive(c.binned("nrecr0")?, "nrecr0")?,
        ],
        recombination_temperature: [c.model("ntrecf")?, c.model("ntrecr")?],
        recombination_limit: c.binned("vrec0")?,
        tunneling_limit: c.binned("vtun0")?,
        alpha,
        length_ratio: finite(t0.powf(c.binned("nbjt")?), "BJT length ratio")?,
        diffusion_ratio: finite(
            c.model("ldif0")?.mul_add(t0.powf(c.model("ndif")?), 1.0),
            "BJT diffusion ratio",
        )?,
        early_voltage: positive(
            c.binned("aely")?
                .mul_add(g.length_m, c.binned("vabjt")?)
                .max(1.0),
            "Early voltage",
        )?,
    })
}

pub(super) fn prepare(
    c: &Context<'_>,
    density: DensityLaw,
) -> Result<PreparedJunctions, ModelError> {
    let law = match c.model.parameters().variant() {
        ModelVariant::Bsim3SoiFd => Law::Fd,
        ModelVariant::Bsim3SoiDd => Law::Dd(Dd {
            area: c.instance.geometry().width_m * positive(c.model("tsi")?, "tsi")?,
            length: c.instance.geometry().length_m,
            diffusion_length: positive(c.binned("edl")?, "edl")?,
            length_modulation: c.binned("kbjt1")?,
        }),
        ModelVariant::Bsim3SoiPd => Law::Pd(pd_layout(c)?),
        _ => return Err(ModelError::WrongModelScope),
    };
    Ok(PreparedJunctions {
        law,
        charge: charge::prepare(c)?,
        density,
        ambient: c.material.kelvin,
        selfheat: integer(c.model.parameters(), "shmod")? == 1 && c.instance("rth0")? != 0.0,
        bjt_disabled: integer(c.instance.parameters(), "bjtoff")? == 1,
        diode_ideality: positive(c.binned("ndiode")?, "ndiode")?,
        tunneling_ideality: positive(c.binned("ntun")?, "ntun")?,
    })
}

#[derive(Clone, Copy, Default)]
struct Components {
    diffusion: D,
    recombination: D,
    bipolar: D,
    tunneling: D,
    stored: D,
}
impl Components {
    fn output(self) -> Result<JunctionComponents, ModelError> {
        Ok(JunctionComponents {
            diffusion: JunctionCurrent::from_d(self.diffusion)?,
            recombination: JunctionCurrent::from_d(self.recombination)?,
            bipolar: JunctionCurrent::from_d(self.bipolar)?,
            tunneling: JunctionCurrent::from_d(self.tunneling)?,
            total: JunctionCurrent::from_d(
                self.diffusion + self.recombination + self.bipolar + self.tunneling,
            )?,
            stored_charge_diffusion: JunctionCurrent::from_d(self.stored)?,
        })
    }
}

struct Bias {
    charge: ChargeLaw,
    junction: [D; 2],
    drain: D,
    temperature: D,
    diode_voltage: D,
    tunnel_voltage: D,
}

fn output(
    b: &Bias,
    sides: [Components; 2],
    collector: D,
) -> Result<JunctionEvaluation, ModelError> {
    Ok(JunctionEvaluation {
        source: sides[0].output()?,
        drain: sides[1].output()?,
        collector: JunctionCurrent::from_d(collector)?,
        source_charge: b.charge.evaluate(b, sides[0], 0)?,
        drain_charge: b.charge.evaluate(b, sides[1], 1)?,
    })
}

fn dd_currents(p: Dd, b: &Bias, j: Densities, off: bool) -> Result<JunctionEvaluation, ModelError> {
    let exponents = b.junction.map(|v| limited_exp(v / b.diode_voltage, false));
    let mut collector = D::constant(0.0);
    let mut sides = [Components::default(); 2];
    for (i, v) in b.junction.into_iter().enumerate() {
        let e = exponents[i];
        let rec = if e.value.sqrt() > 1e-20 {
            e.sqrt()
        } else {
            D::constant(e.value.sqrt())
        };
        sides[i].diffusion = (j.diffusion * diode_exp_m1(v / b.diode_voltage, false)).scale(p.area);
        sides[i].recombination = (j.recombination * (rec - D::constant(1.0))).scale(p.area);
        sides[i].tunneling =
            -(j.tunneling * diode_exp_m1(-v / b.tunnel_voltage, false)).scale(p.area);
    }
    if !off && b.drain.value != 0.0 && j.bipolar.value != 0.0 {
        let length = (D::constant(p.length) - b.drain.scale(p.length_modulation))
            .scale(p.diffusion_length.recip());
        let limited = if length.value < 1e-3 {
            (D::constant(2e-3) - length) / (D::constant(3.0) - length.scale(2e3))
        } else if length.value > 1.0 {
            D::constant(1.0)
        } else {
            length
        };
        let loss = (limited * limited).scale(0.5);
        let injection = j.bipolar.scale(p.area);
        for (side, exp) in sides.iter_mut().zip(exponents) {
            side.bipolar = loss * injection * exp;
        }
        collector = (D::constant(1.0) - loss) * injection * (exponents[0] - exponents[1]);
    }
    output(b, sides, collector)
}

fn reverse_exp(voltage: D, scale: D, limit: f64) -> (D, bool) {
    let remaining = D::constant(limit) - voltage;
    if remaining.value < 1e-3 {
        // Native v2.2.3 limits the reciprocal to 1000 and freezes
        // the Newton tangents in this branch.
        (
            D::constant((-voltage.value / scale.value * limit * 1000.0).exp()),
            true,
        )
    } else {
        (
            limited_exp((-voltage / scale).scale(limit) / remaining, true),
            false,
        )
    }
}

fn pd_side(p: Pd, b: &Bias, j: Densities, ratio: D, i: usize) -> Components {
    let voltage = b.junction[i];
    let forward = diode_exp_m1(voltage / b.diode_voltage, true);
    let forward_scale = (D::constant(1.0)
        + (ratio - D::constant(1.0)).scale(p.recombination_temperature[0]))
    .scale(0.026 * p.recombination_ideality[0]);
    let reverse_scale = (D::constant(1.0)
        + (ratio - D::constant(1.0)).scale(p.recombination_temperature[1]))
    .scale(0.026 * p.recombination_ideality[1]);
    let recombination = if j.recombination.value == 0.0 {
        D::constant(0.0)
    } else {
        let (reverse, _) = reverse_exp(voltage, reverse_scale, p.recombination_limit);
        (j.recombination * (limited_exp(voltage / forward_scale, true) - reverse)).scale(p.area[i])
    };
    let tunneling = if j.tunneling.value == 0.0 {
        D::constant(0.0)
    } else {
        let (reverse, frozen) = reverse_exp(voltage, b.tunnel_voltage, p.tunneling_limit);
        let value = (j.tunneling * (D::constant(1.0) - reverse)).scale(p.area[i]);
        if frozen {
            D::constant(value.value)
        } else {
            value
        }
    };
    Components {
        diffusion: (j.diffusion * forward).scale(p.area[i]),
        recombination,
        tunneling,
        ..Components::default()
    }
}

fn high_injection(exponent: D, injection: D) -> D {
    let high = injection * exponent;
    if high.value < 1e-5 {
        D::constant(0.0)
    } else {
        high
    }
}

fn pd_currents(
    p: Pd,
    b: &Bias,
    j: Densities,
    ratio: D,
    off: bool,
) -> Result<JunctionEvaluation, ModelError> {
    let mut sides: [Components; 2] = std::array::from_fn(|i| pd_side(p, b, j, ratio, i));
    let mut collector = D::constant(0.0);
    if j.bipolar.value != 0.0 {
        let forward = b.junction.map(|v| diode_exp_m1(v / b.diode_voltage, true));
        let high = forward.map(|e| high_injection(e, j.injection));
        let injection = j.bipolar.scale(p.neutral_area * p.length_ratio);
        let diffusion = j.bipolar.scale(p.neutral_area * p.diffusion_ratio);
        for i in 0..2 {
            let factor = (D::constant(1.0) + high[i]).sqrt().reciprocal();
            let effective = forward[i] * factor;
            if 1.0 - p.alpha >= 1e-2 {
                sides[i].bipolar = injection.scale(1.0 - p.alpha) * effective;
            }
            sides[i].stored = diffusion * effective;
        }
        if !off && b.drain.value != 0.0 {
            let early =
                D::constant(1.0) + (b.junction[0] + b.junction[1]).scale(p.early_voltage.recip());
            let root = (early * early + (high[0] + high[1]).scale(4.0)).sqrt();
            let denom = (early + root).scale(0.5);
            let second_order = if denom.value < 0.1 {
                D::constant(10.0)
            } else {
                denom.reciprocal()
            };
            collector = injection.scale(p.alpha) * (forward[0] - forward[1]) * second_order;
        }
    }
    output(b, sides, collector)
}

impl PreparedJunctions {
    /// Evaluate junctions at normalized source-oriented bias and device temperature.
    ///
    /// For isothermal instances the prepared ambient temperature is used and
    /// thermal tangents are zero. Native DD and PD disable collector transport
    /// at exactly zero VDS; PD retains neutral-body recombination and storage
    /// when BJTOFF is set. The caller owns source/drain reversal.
    ///
    /// # Errors
    /// Nonfinite bias, negative normalized VDS, invalid temperature, or
    /// nonfinite physical current or Newton tangent.
    pub fn evaluate(
        self,
        vbs: f64,
        vds: f64,
        kelvin: f64,
    ) -> Result<JunctionEvaluation, ModelError> {
        finite(vbs, "junction VBS")?;
        finite(vds, "junction VDS")?;
        if vds < 0.0 {
            return Err(ModelError::InvalidPhysicalParameter("normalized VDS"));
        }
        if matches!(self.law, Law::Fd) {
            return Ok(JunctionEvaluation::default());
        }
        let temperature = if self.selfheat {
            D::variable(positive(kelvin, "junction temperature")?, 2)
        } else {
            D::constant(self.ambient)
        };
        let body = D::variable(vbs, 0);
        let drain = D::variable(vds, 1);
        let diode_voltage = temperature.scale(K_OVER_Q * self.diode_ideality);
        let tunnel_voltage = if matches!(self.law, Law::Pd(_)) {
            D::constant(0.026 * self.tunneling_ideality)
        } else {
            temperature.scale(K_OVER_Q * self.tunneling_ideality)
        };
        let bias = Bias {
            charge: self.charge,
            junction: [body, body - drain],
            drain,
            temperature,
            diode_voltage,
            tunnel_voltage,
        };
        let j = self
            .density
            .evaluate(temperature, self.selfheat, self.diode_ideality);
        match self.law {
            Law::Fd => Ok(JunctionEvaluation::default()),
            Law::Dd(p) => dd_currents(p, &bias, j, self.bjt_disabled),
            Law::Pd(p) => pd_currents(
                p,
                &bias,
                j,
                bias.temperature.scale(self.density.nominal.recip()),
                self.bjt_disabled,
            ),
        }
    }
}
