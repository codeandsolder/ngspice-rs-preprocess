//! Temperature, layout stress and series resistance from `B4SOItemp` and `B4SOIload`.
//! Copyright 2010 Regents of the University of California. See package LICENSE.

use super::stress::{self, BsimSoiStressOffsets, Stress};
use super::{
    BsimSoiNode, BsimSoiThermalScalar, K_OVER_Q, PreparedBsimSoiSetup, PreparedBsimSoiSize, binned,
    integer, positive, real,
};
use crate::{BoundParameters, ModelError, MosPolarity};

fn product(a: BsimSoiThermalScalar, b: BsimSoiThermalScalar) -> BsimSoiThermalScalar {
    BsimSoiThermalScalar::new(
        a.value * b.value,
        a.value.mul_add(b.d_temperature, a.d_temperature * b.value),
    )
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Linear {
    base: f64,
    slope: f64,
}

impl Linear {
    fn evaluate(self, ratio: BsimSoiThermalScalar) -> BsimSoiThermalScalar {
        BsimSoiThermalScalar::new(
            self.slope.mul_add(ratio.value, self.base),
            self.slope * ratio.d_temperature,
        )
    }
}

/// Base resistance parameters at one temperature. External values are
/// already normalized by finger count, but not parallel multiplicity.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BsimSoiSeriesParameters {
    Internal {
        resistance: BsimSoiThermalScalar,
    },
    External {
        source: BsimSoiThermalScalar,
        drain: BsimSoiThermalScalar,
        source_minimum: BsimSoiThermalScalar,
        drain_minimum: BsimSoiThermalScalar,
    },
}

/// Native temperature-dependent transport coefficients before bias-dependent
/// channel mobility degradation or velocity saturation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BsimSoiTransportPoint {
    pub temperature: f64,
    pub mobility: BsimSoiThermalScalar,
    pub saturation_velocity: BsimSoiThermalScalar,
    pub ua: BsimSoiThermalScalar,
    pub ub: BsimSoiThermalScalar,
    pub uc: BsimSoiThermalScalar,
    pub series: BsimSoiSeriesParameters,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Series {
    Internal(Linear),
    External {
        source: Linear,
        drain: Linear,
        source_min: Linear,
        drain_min: Linear,
    },
}

impl Series {
    fn evaluate(
        self,
        ratio: BsimSoiThermalScalar,
        live: bool,
    ) -> Result<BsimSoiSeriesParameters, ModelError> {
        let external = |law: Linear| {
            let mut value = law.evaluate(ratio);
            if !live && value.value < 0. {
                value = BsimSoiThermalScalar::new(0., 0.);
            }
            value.checked("external series resistance")
        };
        match self {
            Self::Internal(law) => {
                let mut value = law.evaluate(ratio);
                if !live && value.value < 0.001 {
                    value = BsimSoiThermalScalar::new(0., 0.);
                }
                Ok(BsimSoiSeriesParameters::Internal {
                    resistance: value.checked("internal series resistance")?,
                })
            }
            Self::External {
                source,
                drain,
                source_min,
                drain_min,
            } => Ok(BsimSoiSeriesParameters::External {
                source: external(source)?,
                drain: external(drain)?,
                source_minimum: external(source_min)?,
                drain_minimum: external(drain_min)?,
            }),
        }
    }
}

/// Local resistance value and independent partials with respect to effective
/// gate overdrive and square-root surface-potential difference.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BsimSoiInternalResistance {
    pub resistance: f64,
    pub d_overdrive: f64,
    pub d_root_potential: f64,
    pub d_temperature: f64,
}

/// Conductance of a physical external-to-prime branch, including M.
/// Gate/body derivatives are with respect to physical terminal differences.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BsimSoiSeriesConductance {
    pub conductance: f64,
    pub d_gate: f64,
    pub d_body: f64,
    pub d_temperature: f64,
}

/// Bias coordinates relative to the physical prime node on one terminal.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BsimSoiSeriesBias {
    pub gate: f64,
    pub body: f64,
}

/// External source/drain access conductances in physical terminal order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BsimSoiExternalSeries {
    pub source: BsimSoiSeriesConductance,
    pub drain: BsimSoiSeriesConductance,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreparedBsimSoiTransport {
    nominal: f64,
    ambient: f64,
    live: bool,
    mobility: f64,
    mobility_exponent: f64,
    velocity: Linear,
    degradation: [Linear; 3],
    stress: Option<Stress>,
    series: Series,
    prwg: f64,
    prwb: f64,
    polarity: f64,
    flatband: f64,
    sheet: [f64; 2],
    multiplicity: f64,
    fingers: f64,
}

impl PreparedBsimSoiTransport {
    #[must_use = "stress offsets are applied to the ambient electrostatic coefficients"]
    pub fn stress_offsets(&self) -> BsimSoiStressOffsets {
        self.stress
            .map_or_else(BsimSoiStressOffsets::default, |stress| stress.offsets)
    }

    /// Evaluate temperature-dependent transport coefficients.
    ///
    /// # Errors
    /// Nonfinite coefficients or invalid physical temperature/mobility.
    pub fn evaluate(&self, kelvin: f64) -> Result<BsimSoiTransportPoint, ModelError> {
        let temperature = positive(
            if self.live { kelvin } else { self.ambient },
            "transport temperature",
        )?;
        let ratio = BsimSoiThermalScalar::new(
            temperature / self.nominal - 1.,
            if self.live { 1. / self.nominal } else { 0. },
        );
        let mobility = self.mobility * (temperature / self.nominal).powf(self.mobility_exponent);
        let mut mobility = BsimSoiThermalScalar::new(
            mobility,
            if self.live {
                mobility * self.mobility_exponent / temperature
            } else {
                0.
            },
        );
        let mut velocity = self.velocity.evaluate(ratio);
        if let Some(stress) = self.stress {
            mobility = product(mobility, stress.correction(ratio, 1.)?);
            velocity = product(velocity, stress.correction(ratio, stress.velocity_weight)?);
        }
        positive(mobility.value, "mobility")?;
        positive(velocity.value, "saturation velocity")?;
        let [ua, ub, uc] = self.degradation.map(|law| law.evaluate(ratio));
        Ok(BsimSoiTransportPoint {
            temperature,
            mobility: mobility.checked("mobility")?,
            saturation_velocity: velocity.checked("saturation velocity")?,
            ua: ua.checked("ua")?,
            ub: ub.checked("ub")?,
            uc: uc.checked("uc")?,
            series: self.series.evaluate(ratio, self.live)?,
        })
    }

    /// Native internal resistance law, expressed for the entire parallel
    /// instance. The caller supplies Vgsteff and sqrt(Phis)-sqrt(Phi).
    ///
    /// # Errors
    /// Invalid input or nonfinite resistance.
    pub fn internal_resistance(
        &self,
        kelvin: f64,
        overdrive: f64,
        root_potential: f64,
    ) -> Result<BsimSoiInternalResistance, ModelError> {
        let BsimSoiSeriesParameters::Internal { resistance } = self.evaluate(kelvin)?.series else {
            return Ok(BsimSoiInternalResistance::default());
        };
        let x = self.prwg.mul_add(overdrive, self.prwb * root_potential);
        let (factor, slope) = if x >= -0.9 {
            (1. + x, 1.)
        } else {
            let denominator = 20.0_f64.mul_add(x, 17.);
            ((0.8 + x) / denominator, 1. / denominator.powi(2))
        };
        let scale = 1. / (self.fingers * self.multiplicity);
        let value = resistance.value * scale;
        let result = BsimSoiInternalResistance {
            resistance: value * factor,
            d_overdrive: value * slope * self.prwg,
            d_root_potential: value * slope * self.prwb,
            d_temperature: resistance.d_temperature * scale * factor,
        };
        if [
            result.resistance,
            result.d_overdrive,
            result.d_root_potential,
            result.d_temperature,
        ]
        .into_iter()
        .all(f64::is_finite)
        {
            Ok(result)
        } else {
            Err(ModelError::InvalidPhysicalParameter(
                "internal resistance bias",
            ))
        }
    }

    fn external_side(
        &self,
        bias: BsimSoiSeriesBias,
        sheet: f64,
        base: BsimSoiThermalScalar,
        minimum: BsimSoiThermalScalar,
    ) -> Result<BsimSoiSeriesConductance, ModelError> {
        if !bias.gate.is_finite() || !bias.body.is_finite() {
            return Err(ModelError::InvalidPhysicalParameter(
                "external resistance bias",
            ));
        }
        if sheet == 0. {
            return Ok(BsimSoiSeriesConductance::default());
        }
        let x = self.polarity.mul_add(bias.gate, -self.flatband);
        let radical = x.hypot(0.01);
        let effective = if x >= 0. {
            f64::midpoint(x, radical)
        } else {
            0.00005 / (radical - x)
        };
        let gate_factor = self.prwg.mul_add(effective, 1.);
        let argument = (self.prwb * self.polarity).mul_add(-bias.body, 1. / gate_factor);
        let root = argument.hypot(0.1);
        let shape = if argument >= 0. {
            argument + root
        } else {
            0.01 / (root - argument)
        };
        let d_shape = shape / root;
        let resistance = base.value.mul_add(0.5 * shape, minimum.value);
        let conductance = 1. / (1. / sheet + resistance);
        let slope = -conductance * conductance * self.multiplicity;
        let result = BsimSoiSeriesConductance {
            conductance: conductance * self.multiplicity,
            d_gate: slope
                * base.value
                * 0.5
                * d_shape
                * (-self.prwg / gate_factor.powi(2))
                * effective
                / radical
                * self.polarity,
            d_body: slope * base.value * 0.5 * d_shape * (-self.prwb) * self.polarity,
            d_temperature: slope
                * base
                    .d_temperature
                    .mul_add(0.5 * shape, minimum.d_temperature),
        };
        if [
            result.conductance,
            result.d_gate,
            result.d_body,
            result.d_temperature,
        ]
        .into_iter()
        .all(f64::is_finite)
        {
            Ok(result)
        } else {
            Err(ModelError::InvalidPhysicalParameter(
                "external resistance conductance",
            ))
        }
    }

    /// Evaluate physical external access branches. RDSMOD=0 still returns
    /// the constant sheet conductances on resolved prime nodes.
    ///
    /// # Errors
    /// Invalid bias or nonfinite branch coefficients.
    pub fn external_series(
        &self,
        kelvin: f64,
        source: BsimSoiSeriesBias,
        drain: BsimSoiSeriesBias,
    ) -> Result<BsimSoiExternalSeries, ModelError> {
        match self.evaluate(kelvin)?.series {
            BsimSoiSeriesParameters::Internal { .. } => Ok(BsimSoiExternalSeries {
                source: BsimSoiSeriesConductance {
                    conductance: self.sheet[0] * self.multiplicity,
                    ..Default::default()
                },
                drain: BsimSoiSeriesConductance {
                    conductance: self.sheet[1] * self.multiplicity,
                    ..Default::default()
                },
            }),
            BsimSoiSeriesParameters::External {
                source: rs,
                drain: rd,
                source_minimum,
                drain_minimum,
            } => Ok(BsimSoiExternalSeries {
                source: self.external_side(source, self.sheet[0], rs, source_minimum)?,
                drain: self.external_side(drain, self.sheet[1], rd, drain_minimum)?,
            }),
        }
    }
}

impl PreparedBsimSoiSetup {
    /// Prepare temperature-dependent transport and source/drain access laws.
    ///
    /// # Errors
    /// Missing input or invalid geometry, material or transport parameters.
    pub fn prepare_transport(
        &self,
        index: usize,
        ambient: f64,
    ) -> Result<PreparedBsimSoiTransport, ModelError> {
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
        let get = |name| binned(size, name);
        let linear = |base, slope| -> Result<Linear, ModelError> {
            Ok(Linear {
                base: get(base)?,
                slope: get(slope)?,
            })
        };
        let raw_mobility = get("u0")?;
        let mobility = if raw_mobility > 1. {
            raw_mobility / 1e4
        } else {
            raw_mobility
        };
        let mut velocity = linear("vsat", "at")?;
        velocity.slope = -velocity.slope;
        let fingers = positive(real(p, "nf")?, "nf")?;
        let series = prepare_series(model, size, fingers)?;
        let topology = instance.topology();
        let sheet = |squares: &str, node| -> Result<f64, ModelError> {
            let resistance = real(model, "rsh")? * real(p, squares)?;
            Ok(
                if matches!(node, BsimSoiNode::SourcePrime | BsimSoiNode::DrainPrime)
                    && resistance > 0.
                {
                    1. / resistance
                } else {
                    0.
                },
            )
        };
        let nominal = positive(real(model, "tnom")?, "tnom")?;
        let polarity = if self.model.polarity() == MosPolarity::Nmos {
            1.
        } else {
            -1.
        };
        let flatband = if integer(model, "mtrlmod")? == 0 {
            silicon_flatband(size, nominal)?
        } else {
            generalized_flatband(
                model,
                size,
                self.prepare_temperature(index, ambient)?
                    .ambient_point()?
                    .intrinsic_density
                    .value,
                nominal,
                polarity,
            )?
        };
        Ok(PreparedBsimSoiTransport {
            nominal,
            ambient: positive(ambient, "ambient temperature")?,
            live: topology.temperature != BsimSoiNode::Ground,
            mobility: positive(mobility, "u0")?,
            mobility_exponent: get("ute")?,
            velocity,
            degradation: [
                linear("ua", "ua1")?,
                linear("ub", "ub1")?,
                linear("uc", "uc1")?,
            ],
            stress: stress::prepare(model, p, size)?,
            series,
            prwg: get("prwg")?,
            prwb: get("prwb")?,
            polarity,
            flatband,
            sheet: [
                sheet("nrs", topology.source_prime)?,
                sheet("nrd", topology.drain_prime)?,
            ],
            multiplicity: positive(real(p, "m")?, "multiplicity")?,
            fingers,
        })
    }
}

fn prepare_series(
    model: &BoundParameters,
    size: &PreparedBsimSoiSize,
    fingers: f64,
) -> Result<Series, ModelError> {
    let geometry = size.geometry();
    let get = |name| binned(size, name);
    let denominator = positive(
        (geometry.weff * 1e6).powf(get("wr")?),
        "series resistance width",
    )?;
    let prt = get("prt")?;
    Ok(if integer(model, "rdsmod")? == 0 {
        Series::Internal(Linear {
            base: get("rdsw")?.max(0.) / denominator,
            slope: prt / denominator,
        })
    } else {
        let denom = denominator * fingers;
        let law = |base| Linear {
            base: base / denom,
            slope: prt / denom,
        };
        Series::External {
            source: law(get("rsw")?),
            drain: law(get("rdw")?),
            source_min: law(real(model, "rswmin")?),
            drain_min: law(real(model, "rdwmin")?),
        }
    })
}
fn silicon_flatband(size: &PreparedBsimSoiSize, nominal: f64) -> Result<f64, ModelError> {
    let ngate = binned(size, "ngate")?;
    Ok(if ngate > 0. {
        K_OVER_Q * nominal * (ngate / 1e20).ln()
    } else {
        0.
    })
}
fn generalized_flatband(
    model: &BoundParameters,
    size: &PreparedBsimSoiSize,
    ambient_density: f64,
    nominal: f64,
    polarity: f64,
) -> Result<f64, ModelError> {
    let gap = real(model, "bg0sub")?
        - real(model, "tbgasub")? * nominal * nominal / (nominal + real(model, "tbgbsub")?);
    let potential =
        (K_OVER_Q * nominal * (binned(size, "nsd")? / ambient_density).ln()).min(0.5 * gap);
    Ok(real(model, "phig")?
        - polarity.mul_add(-potential, 0.5_f64.mul_add(gap, real(model, "easub")?)))
}
