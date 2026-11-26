//! Independent body junction mechanisms from `B4SOItemp` and `B4SOIload`.
//! Copyright 2010 Regents of the University of California. See package LICENSE.

mod charge;
mod neutral;

pub use charge::{BsimSoiJunctionCharge, BsimSoiJunctionCharges, BsimSoiJunctionStorage};
pub use neutral::{BsimSoiCollectorCurrent, BsimSoiNeutralBody};

use super::{
    BsimSoiNode, BsimSoiThermalScalar, K_OVER_Q, PreparedBsimSoiSetup, PreparedBsimSoiSize,
    integer, positive, real, slot,
};
use crate::{ModelError, MosPolarity, ParameterScope};

const EXP_MAX: f64 = 2.688_117_142e43;
const EXP_MIN: f64 = 3.720_075_976e-44;

/// Temperature-dependent native density coefficients before geometry scaling.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BsimSoiJunctionDensities {
    pub bipolar: BsimSoiThermalScalar,
    pub diffusion: BsimSoiThermalScalar,
    pub recombination: BsimSoiThermalScalar,
    pub tunneling: BsimSoiThermalScalar,
    pub high_level_injection: BsimSoiThermalScalar,
}

/// Physical body-to-terminal current and its local bias/temperature derivatives.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BsimSoiJunctionCurrent {
    pub current: f64,
    pub conductance: f64,
    pub d_temperature: f64,
}

impl BsimSoiJunctionCurrent {
    fn checked(self) -> Result<Self, ModelError> {
        if [self.current, self.conductance, self.d_temperature]
            .into_iter()
            .all(f64::is_finite)
        {
            Ok(self)
        } else {
            Err(ModelError::InvalidPhysicalParameter(
                "BSIMSOI junction current",
            ))
        }
    }

    /// Sum of independent junction contributions, excluding neutral-body BJT.
    #[must_use = "the sum is the branch current and its constitutive partials"]
    pub const fn plus(self, other: Self) -> Self {
        Self {
            current: self.current + other.current,
            conductance: self.conductance + other.conductance,
            d_temperature: self.d_temperature + other.d_temperature,
        }
    }
}

/// The independent mechanisms on one physical terminal. The coupled neutral
/// body BJT/recombination mechanism (`Ibs3/Ibd3`) is not included.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BsimSoiJunctionSide {
    pub densities: BsimSoiJunctionDensities,
    pub diffusion: BsimSoiJunctionCurrent,
    pub recombination: BsimSoiJunctionCurrent,
    pub tunneling: BsimSoiJunctionCurrent,
}

impl BsimSoiJunctionSide {
    #[must_use = "this sum excludes the coupled neutral-body BJT mechanism"]
    pub const fn independent_current(self) -> BsimSoiJunctionCurrent {
        self.diffusion.plus(self.recombination).plus(self.tunneling)
    }
}

/// Source and drain are kept in physical terminal order even for reverse VDS.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BsimSoiJunctionPoint {
    pub source: BsimSoiJunctionSide,
    pub drain: BsimSoiJunctionSide,
    pub neutral_body: BsimSoiNeutralBody,
    pub charges: BsimSoiJunctionCharges,
}

#[derive(Clone, Copy)]
struct ExponentialFactor {
    value: f64,
    slope: f64,
}

fn limited_exp(argument: f64) -> ExponentialFactor {
    let (value, slope) = if argument > 100. {
        (EXP_MAX * (argument - 99.), EXP_MAX)
    } else if argument < -100. {
        (EXP_MIN, 0.)
    } else {
        let value = argument.exp();
        (value, value)
    };
    ExponentialFactor { value, slope }
}

fn exp_minus_one(argument: f64, value: f64) -> f64 {
    if (-100. ..=100.).contains(&argument) {
        argument.exp_m1()
    } else {
        value - 1.
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct DensityLaw {
    nominal: f64,
    exponent: f64,
}

impl DensityLaw {
    fn evaluate(self, factor: BsimSoiThermalScalar) -> BsimSoiThermalScalar {
        let exponential = limited_exp(self.exponent * factor.value);
        BsimSoiThermalScalar {
            value: self.nominal * exponential.value,
            d_temperature: self.nominal * exponential.slope * self.exponent * factor.d_temperature,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct SideLaw {
    bipolar: DensityLaw,
    diffusion: DensityLaw,
    recombination: DensityLaw,
    tunneling: DensityLaw,
    high_level: DensityLaw,
    ideality: f64,
    forward_recombination: f64,
    reverse_recombination: f64,
    forward_temp: f64,
    reverse_temp: f64,
    recombination_voltage: f64,
    tunnel_ideality: f64,
    tunnel_voltage: f64,
    area: f64,
}

#[derive(Clone, Copy)]
struct Bias {
    voltage: f64,
    temperature: f64,
    ratio: BsimSoiThermalScalar,
    activation: BsimSoiThermalScalar,
    live: bool,
    polarity: f64,
}

#[derive(Clone, Copy)]
struct ExponentialPoint {
    value: f64,
    d_voltage: f64,
    d_temperature: f64,
}

fn forward_exp(voltage: f64, thermal_voltage: BsimSoiThermalScalar) -> ExponentialPoint {
    let argument = voltage / thermal_voltage.value;
    let exp = limited_exp(argument);
    let slope = exp.slope / thermal_voltage.value;
    ExponentialPoint {
        value: exp.value,
        d_voltage: slope,
        d_temperature: -slope * argument * thermal_voltage.d_temperature,
    }
}

// Native reverse exponent uses a 1 mV denominator floor and ordinary exp in
// that branch. Its historical stamp suppresses some derivatives in the floor
// branch; return the actual derivatives of the constitutive value here.
fn reverse_exp(
    voltage: f64,
    thermal_voltage: BsimSoiThermalScalar,
    critical: f64,
) -> ExponentialPoint {
    let distance = critical - voltage;
    let floored = distance < 1e-3;
    let denominator = distance.max(1e-3);
    let argument = -voltage * critical / (thermal_voltage.value * denominator);
    let (value, slope) = if floored {
        let value = argument.exp();
        (value, value)
    } else {
        let exp = limited_exp(argument);
        (exp.value, exp.slope)
    };
    let voltage_slope = if floored {
        -critical / (thermal_voltage.value * denominator)
    } else {
        -critical * critical / (thermal_voltage.value * denominator * denominator)
    };
    ExponentialPoint {
        value,
        d_voltage: slope * voltage_slope,
        d_temperature: -slope * argument * thermal_voltage.d_temperature / thermal_voltage.value,
    }
}

impl SideLaw {
    fn densities(self, bias: Bias) -> BsimSoiJunctionDensities {
        BsimSoiJunctionDensities {
            bipolar: self.bipolar.evaluate(bias.activation),
            diffusion: self.diffusion.evaluate(bias.activation),
            recombination: self.recombination.evaluate(bias.activation),
            tunneling: self.tunneling.evaluate(bias.ratio),
            high_level_injection: self.high_level.evaluate(bias.activation),
        }
    }

    fn scale_current(
        self,
        bias: Bias,
        density: BsimSoiThermalScalar,
        factor: ExponentialPoint,
    ) -> Result<BsimSoiJunctionCurrent, ModelError> {
        let area = self.area;
        BsimSoiJunctionCurrent {
            current: bias.polarity * area * density.value * factor.value,
            conductance: area * density.value * factor.d_voltage,
            d_temperature: bias.polarity
                * area
                * density
                    .value
                    .mul_add(factor.d_temperature, density.d_temperature * factor.value),
        }
        .checked()
    }

    fn diffusion(
        self,
        bias: Bias,
        density: BsimSoiThermalScalar,
    ) -> Result<BsimSoiJunctionCurrent, ModelError> {
        if density.value == 0. {
            return Ok(BsimSoiJunctionCurrent::default());
        }
        let voltage = BsimSoiThermalScalar {
            value: self.ideality * K_OVER_Q * bias.temperature,
            d_temperature: if bias.live {
                self.ideality * K_OVER_Q
            } else {
                0.
            },
        };
        let mut factor = forward_exp(bias.voltage, voltage);
        factor.value = exp_minus_one(bias.voltage / voltage.value, factor.value);
        self.scale_current(bias, density, factor)
    }

    fn recombination(
        self,
        bias: Bias,
        density: BsimSoiThermalScalar,
    ) -> Result<BsimSoiJunctionCurrent, ModelError> {
        if density.value == 0. {
            return Ok(BsimSoiJunctionCurrent::default());
        }
        let scale = |ideality: f64, temp: f64| -> Result<BsimSoiThermalScalar, ModelError> {
            let base = 0.026 * ideality;
            Ok(BsimSoiThermalScalar {
                value: positive(
                    base * temp.mul_add(bias.ratio.value, 1.),
                    "recombination thermal voltage",
                )?,
                d_temperature: base * temp * bias.ratio.d_temperature,
            })
        };
        let forward = forward_exp(
            bias.voltage,
            scale(self.forward_recombination, self.forward_temp)?,
        );
        let reverse = reverse_exp(
            bias.voltage,
            scale(self.reverse_recombination, self.reverse_temp)?,
            self.recombination_voltage,
        );
        self.scale_current(
            bias,
            density,
            ExponentialPoint {
                value: forward.value - reverse.value,
                d_voltage: forward.d_voltage - reverse.d_voltage,
                d_temperature: forward.d_temperature - reverse.d_temperature,
            },
        )
    }

    fn tunneling(
        self,
        bias: Bias,
        density: BsimSoiThermalScalar,
    ) -> Result<BsimSoiJunctionCurrent, ModelError> {
        if density.value == 0. {
            return Ok(BsimSoiJunctionCurrent::default());
        }
        let reverse = reverse_exp(
            bias.voltage,
            BsimSoiThermalScalar {
                value: 0.026 * self.tunnel_ideality,
                d_temperature: 0.,
            },
            self.tunnel_voltage,
        );
        self.scale_current(
            bias,
            density,
            ExponentialPoint {
                value: 1. - reverse.value,
                d_voltage: -reverse.d_voltage,
                d_temperature: 0.,
            },
        )
    }

    fn evaluate(self, bias: Bias, disabled: bool) -> Result<BsimSoiJunctionSide, ModelError> {
        let densities = self.densities(bias);
        for density in [
            densities.bipolar,
            densities.diffusion,
            densities.recombination,
            densities.tunneling,
            densities.high_level_injection,
        ] {
            if !density.value.is_finite() || !density.d_temperature.is_finite() {
                return Err(ModelError::InvalidPhysicalParameter(
                    "junction saturation density",
                ));
            }
        }
        if disabled {
            return Ok(BsimSoiJunctionSide {
                densities,
                diffusion: BsimSoiJunctionCurrent::default(),
                recombination: BsimSoiJunctionCurrent::default(),
                tunneling: BsimSoiJunctionCurrent::default(),
            });
        }
        Ok(BsimSoiJunctionSide {
            densities,
            diffusion: self.diffusion(bias, densities.diffusion)?,
            recombination: self.recombination(bias, densities.recombination)?,
            tunneling: self.tunneling(bias, densities.tunneling)?,
        })
    }
}

/// Immutable asymmetric junction preparation at a fixed instance topology.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreparedBsimSoiJunctions {
    sides: [SideLaw; 2],
    neutral: neutral::NeutralLaw,
    charge: charge::JunctionChargeLaw,
    nominal: f64,
    ambient: f64,
    polarity: f64,
    live: bool,
    ideal_fd: bool,
}

impl PreparedBsimSoiJunctions {
    /// Evaluate independent junction mechanisms using physical B-S and B-D
    /// voltages and an absolute-kelvin thermal coordinate.
    ///
    /// # Errors
    /// Invalid constitutive domain or a nonfinite residual/derivative.
    pub fn evaluate(
        &self,
        kelvin: f64,
        body_source: f64,
        body_drain: f64,
    ) -> Result<BsimSoiJunctionPoint, ModelError> {
        let temperature = positive(
            if self.live { kelvin } else { self.ambient },
            "junction temperature",
        )?;
        if !body_source.is_finite() || !body_drain.is_finite() {
            return Err(ModelError::InvalidPhysicalParameter("junction bias"));
        }
        let ratio = BsimSoiThermalScalar {
            value: temperature / self.nominal - 1.,
            d_temperature: if self.live { 1. / self.nominal } else { 0. },
        };
        let activation = BsimSoiThermalScalar {
            value: 1.115 / (K_OVER_Q * temperature) * ratio.value,
            d_temperature: if self.live {
                1.115 / (K_OVER_Q * temperature * temperature)
            } else {
                0.
            },
        };
        let bias = |voltage| Bias {
            voltage: self.polarity * voltage,
            temperature,
            ratio,
            activation,
            live: self.live,
            polarity: self.polarity,
        };
        let biases = [bias(body_source), bias(body_drain)];
        let source = self.sides[0].evaluate(biases[0], self.ideal_fd)?;
        let drain = self.sides[1].evaluate(biases[1], self.ideal_fd)?;
        let neutral_body = self.neutral.evaluate(
            biases,
            &self.sides,
            [source.densities, drain.densities],
            self.ideal_fd,
        )?;
        let charges = self
            .charge
            .evaluate(biases, self.nominal, neutral_body, self.ideal_fd)?;
        Ok(BsimSoiJunctionPoint {
            source,
            drain,
            neutral_body,
            charges,
        })
    }
}

fn binned(size: &PreparedBsimSoiSize, name: &str) -> Result<f64, ModelError> {
    let id = slot(ParameterScope::Model, name)?;
    size.binned_model_slot(id)
        .ok_or(ModelError::MissingDefaultDependency(id))
}

struct SideNames {
    bipolar: &'static str,
    diffusion: &'static str,
    recombination: &'static str,
    tunneling: &'static str,
    high_level: &'static str,
    diode: &'static str,
    forward_rec: &'static str,
    reverse_rec: &'static str,
    recombination_voltage: &'static str,
    tunnel_ideality: &'static str,
    tunnel_voltage: &'static str,
    x_diffusion: &'static str,
    x_rec: &'static str,
    x_tunnel: &'static str,
}
const SOURCE: SideNames = SideNames {
    bipolar: "isbjt",
    diffusion: "isdif",
    recombination: "isrec",
    tunneling: "istun",
    high_level: "ahli",
    diode: "ndiode",
    forward_rec: "nrecf0",
    reverse_rec: "nrecr0",
    recombination_voltage: "vrec0",
    tunnel_ideality: "ntun",
    tunnel_voltage: "vtun0",
    x_diffusion: "xdif",
    x_rec: "xrec",
    x_tunnel: "xtun",
};
const DRAIN: SideNames = SideNames {
    bipolar: "idbjt",
    diffusion: "iddif",
    recombination: "idrec",
    tunneling: "idtun",
    high_level: "ahlid",
    diode: "ndioded",
    forward_rec: "nrecf0d",
    reverse_rec: "nrecr0d",
    recombination_voltage: "vrec0d",
    tunnel_ideality: "ntund",
    tunnel_voltage: "vtun0d",
    x_diffusion: "xdifd",
    x_rec: "xrecd",
    x_tunnel: "xtund",
};

fn prepare_side(
    size: &PreparedBsimSoiSize,
    names: &SideNames,
    area: f64,
) -> Result<SideLaw, ModelError> {
    let get = |name| binned(size, name);
    let density = |name, exponent| -> Result<DensityLaw, ModelError> {
        Ok(DensityLaw {
            nominal: get(name)?,
            exponent,
        })
    };
    let ideality = positive(get(names.diode)?, names.diode)?;
    let forward = positive(get(names.forward_rec)?, names.forward_rec)?;
    let bipolar_exponent = get("xbjt")? / ideality;
    Ok(SideLaw {
        bipolar: density(names.bipolar, bipolar_exponent)?,
        diffusion: density(names.diffusion, get(names.x_diffusion)? / ideality)?,
        recombination: density(names.recombination, get(names.x_rec)? / forward)?,
        tunneling: density(names.tunneling, get(names.x_tunnel)?)?,
        high_level: density(names.high_level, bipolar_exponent)?,
        ideality,
        forward_recombination: forward,
        reverse_recombination: positive(get(names.reverse_rec)?, names.reverse_rec)?,
        forward_temp: get("ntrecf")?,
        reverse_temp: get("ntrecr")?,
        recombination_voltage: get(names.recombination_voltage)?,
        tunnel_ideality: positive(get(names.tunnel_ideality)?, names.tunnel_ideality)?,
        tunnel_voltage: get(names.tunnel_voltage)?,
        area: positive(area, "junction area")?,
    })
}

impl PreparedBsimSoiSetup {
    /// Bind the native temperature scaling and independent junction laws.
    ///
    /// # Errors
    /// Missing instance or parameter, or invalid geometry, thermal reference,
    /// or junction ideality.
    pub fn prepare_junctions(
        &self,
        index: usize,
        ambient: f64,
    ) -> Result<PreparedBsimSoiJunctions, ModelError> {
        let instance = self
            .instances
            .get(index)
            .ok_or(ModelError::InvalidPhysicalParameter(
                "BSIMSOI instance index",
            ))?;
        let size = self
            .sizes
            .get(index)
            .ok_or(ModelError::MismatchedPreparedContext)?;
        let model = self.model.parameters();
        let p = instance.parameters();
        let thickness = positive(real(model, "tsi")?, "tsi")?;
        let factor = thickness * real(p, "nf")? * real(p, "m")?;
        let geometry = size.geometry();
        Ok(PreparedBsimSoiJunctions {
            sides: [
                prepare_side(size, &SOURCE, factor * geometry.source_diode_width)?,
                prepare_side(size, &DRAIN, factor * geometry.drain_diode_width)?,
            ],
            neutral: neutral::prepare(model, p, size, factor * geometry.weff / real(p, "nseg")?)?,
            charge: charge::prepare(model, size, factor)?,
            nominal: positive(real(model, "tnom")?, "tnom")?,
            ambient: positive(ambient, "ambient temperature")?,
            polarity: if self.model.polarity() == MosPolarity::Nmos {
                1.
            } else {
                -1.
            },
            live: instance.topology().temperature != BsimSoiNode::Ground,
            ideal_fd: integer(p, "soimod")? == 2,
        })
    }
}
