//! Source-defined classical intrinsic charge and CV overdrive laws.
//! Copyright 2010 Regents of the University of California. See package LICENSE.
use super::super::{
    BsimSoiBodyMode, EPS_OX, PreparedBsimSoiSetup, PreparedBsimSoiSize, binned, integer, positive,
    real,
};
use super::{
    BsimSoiChannelBias, BsimSoiChannelPoint, BsimSoiChannelScalar, Jet, PreparedBsimSoiChannel,
    current::{Geometry, prepare_geometry},
};
use crate::ModelError;

/// Physical intrinsic terminal charges, excluding junction and overlap storage.
/// All derivatives use the original five physical channel coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BsimSoiIntrinsicCharges {
    pub gate: BsimSoiChannelScalar,
    pub drain: BsimSoiChannelScalar,
    pub source: BsimSoiChannelScalar,
    pub body: BsimSoiChannelScalar,
    pub backgate: BsimSoiChannelScalar,
    /// Signed intrinsic inversion charge for noise, before charge partition.
    pub inversion: BsimSoiChannelScalar,
}
impl BsimSoiIntrinsicCharges {
    const ZERO: Self = Self {
        gate: Jet::constant(0.),
        drain: Jet::constant(0.),
        source: Jet::constant(0.),
        body: Jet::constant(0.),
        backgate: Jet::constant(0.),
        inversion: Jet::constant(0.),
    };
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Partition {
    Disabled,
    FortySixty,
    Equal,
    ZeroHundred,
}
impl Partition {
    fn from_xpart(x: f64) -> Self {
        if x < 0. {
            Self::Disabled
        } else if x < 0.5 {
            Self::FortySixty
        } else if x > 0.5 {
            Self::ZeroHundred
        } else {
            Self::Equal
        }
    }
    fn source(self, overdrive: Jet, drop: Jet, scale: f64, inversion: Jet, bulk: Jet) -> Jet {
        let denominator = overdrive - 0.5 * drop + 1e-20;
        match self {
            Self::Disabled => Jet::constant(0.),
            Self::ZeroHundred => {
                -scale * (0.5 * overdrive + 0.25 * drop - drop * drop / (24. * denominator))
            }
            Self::FortySixty => {
                // Rationalized cubic expression avoids underflow in weak inversion.
                let x = overdrive / denominator;
                let y = drop / denominator;
                -0.5 * scale
                    * (overdrive * (2. / 3. * y * y + x * (x - 4. / 3. * y))
                        - 2. / 15. * drop * y * y)
            }
            Self::Equal => -0.5 * (inversion + bulk),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct CvOverdrive {
    mode: i32,
    noff: f64,
    offset: f64,
    dc_mstar: f64,
    dc_voff: f64,
    cv_mstar: f64,
    cv_voff: f64,
    cox: f64,
}
impl CvOverdrive {
    fn evaluate(self, point: &BsimSoiChannelPoint, vtm: Jet, cdep: Jet) -> [Jet; 2] {
        let raw = point.gate_voltage - point.threshold;
        let dc_scale = point.subthreshold_factor * vtm;
        let cv_slope = point.subthreshold_factor * self.noff;
        let scale = cv_slope * vtm;
        let shifted = raw - self.offset;
        match self.mode {
            0 => {
                let arg = self.dc_mstar * raw / dc_scale;
                let barrier = (self.dc_voff - (1. - self.dc_mstar) * raw) / dc_scale;
                if arg.value <= -100. || arg.value >= 100. {
                    [point.effective_overdrive, Jet::constant(0.)]
                } else {
                    let exponent = if barrier.value > 100. {
                        (raw - self.dc_voff) / dc_scale
                    } else {
                        arg
                    };
                    let exponent = 2. * exponent - self.offset / scale;
                    [
                        scale * softplus(exponent),
                        scale * softplus(exponent - 1.12 / scale),
                    ]
                }
            }
            1 => [
                scale * softplus(shifted / scale),
                scale * softplus((shifted - 1.12) / scale),
            ],
            _ => [shifted, shifted - 1.12].map(|value| {
                let arg = self.cv_mstar * value / scale;
                let barrier = (self.cv_voff - (1. - self.cv_mstar) * value) / scale;
                if arg.value > 100. {
                    value
                } else if barrier.value > 100. {
                    vtm * cdep / self.cox * ((value - self.cv_voff) / scale).exp()
                } else {
                    scale * softplus(arg)
                        / (self.cv_mstar + cv_slope * self.cox / cdep * barrier.exp())
                }
            }),
        }
    }
}
fn softplus(value: Jet) -> Jet {
    if value.value > 0. {
        value + (-value).exp().ln_1p()
    } else {
        value.exp().ln_1p()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ChargeArea {
    inversion: f64,
    bulk: f64,
}
impl ChargeArea {
    fn channel(
        self,
        overdrive: Jet,
        drain: Jet,
        bulk: Jet,
        partition: Partition,
        ideal: bool,
    ) -> [Jet; 3] {
        let saturation = overdrive / bulk;
        let delta = saturation - drain - 0.02;
        let effective = 2. * saturation * drain
            / (saturation + drain + 0.02 + (delta * delta + 0.08 * saturation).sqrt());
        let drop = bulk * effective;
        let denominator = 12. * (overdrive - 0.5 * drop + 1e-20);
        let inversion = self.inversion * (overdrive - 0.5 * drop + drop * drop / denominator);
        let body = if ideal {
            Jet::constant(0.)
        } else {
            self.bulk * (1. - bulk) * (0.5 * effective - drop * effective / denominator)
        };
        let source = partition.source(overdrive, drop, self.inversion, inversion, body);
        [inversion, body, source]
    }

    fn substrate(
        self,
        flatband: Jet,
        gate: Jet,
        body: Jet,
        overdrive: Jet,
        k1: f64,
        opposite: bool,
    ) -> Jet {
        let shift = flatband - gate + body - 0.08;
        let sign = if flatband.value <= 0. { -1. } else { 1. };
        let smoothing = if opposite { 8. } else { 0.32 };
        let accumulation = shift.half_positive(smoothing * sign * flatband);
        let effective = flatband - accumulation;
        let offset = gate - effective - body - overdrive;
        let depletion = if k1 == 0. {
            Jet::constant(0.)
        } else if offset.value < 0. {
            offset
        } else {
            // k1 * (sqrt(k1²/4+offset)-k1/2)
            k1 * offset / ((0.25 * k1 * k1 + offset).sqrt() + 0.5 * k1)
        };
        self.bulk * (depletion - accumulation)
    }
}

/// Prepared intrinsic CAPMOD=2 storage. The classical CV constitutive
/// equations are independent of the extrinsic and junction charges.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreparedBsimSoiCapmod2 {
    channel: PreparedBsimSoiChannel,
    geometry: Geometry,
    overdrive: CvOverdrive,
    normal: ChargeArea,
    opposite: Option<ChargeArea>,
    partition: Partition,
    bulk_cv_factor: f64,
    backgate_capacitance: f64,
    multiplicity: f64,
    ideal: bool,
}
impl PreparedBsimSoiCapmod2 {
    /// Evaluate conservative physical intrinsic terminal charges.
    ///
    /// # Errors
    /// Invalid physical bias or constitutive output.
    pub fn evaluate(
        &self,
        bias: BsimSoiChannelBias,
    ) -> Result<BsimSoiIntrinsicCharges, ModelError> {
        if self.partition == Partition::Disabled {
            return Ok(BsimSoiIntrinsicCharges::ZERO);
        }
        let point = self.channel.evaluate(bias)?;
        let material = self.channel.material.evaluate(bias.kelvin)?;
        let phi = Jet::thermal(material.surface_potential);
        let [normal, opposite] = self.overdrive.evaluate(
            &point,
            Jet::thermal(material.thermal_voltage),
            Jet::thermal(material.depletion_capacitance),
        );
        let bulk = self.geometry.bulk_factors(&point, phi).0 * self.bulk_cv_factor;
        let [mut inversion, mut body, mut source] = self.normal.channel(
            normal,
            point.drain_voltage,
            bulk,
            self.partition,
            self.ideal,
        );
        let flatband =
            point.threshold - phi - self.channel.threshold.k1 * point.surface_potential.sqrt()
                + self.overdrive.offset;
        let mut substrate = if self.ideal {
            Jet::constant(0.)
        } else {
            self.normal.substrate(
                flatband,
                point.gate_voltage,
                point.body_effective,
                normal,
                self.channel.threshold.k1ox,
                false,
            )
        };
        if let Some(area) = self.opposite {
            let [inv, bod, src] =
                area.channel(opposite, point.drain_voltage, bulk, self.partition, false);
            inversion = inversion + inv;
            body = body + bod;
            source = source + src;
            let sign = self.channel.polarity;
            let mut gate = Jet::coordinate(sign * bias.gate, 0, sign);
            if point.reversed {
                gate = gate + point.drain_voltage;
            }
            substrate = substrate
                + area.substrate(
                    flatband + 1.12,
                    gate,
                    point.body_effective,
                    opposite,
                    self.channel.threshold.k1ox,
                    true,
                );
        }
        let sign = self.channel.polarity;
        let mut backgate = Jet::coordinate(sign * bias.backgate, 3, sign);
        if point.reversed {
            backgate = backgate + point.drain_voltage;
        }
        let backgate = self.backgate_capacitance
            * (backgate - Jet::thermal(material.backgate_flatband) - point.body_voltage);
        let gate = inversion + substrate;
        let body = body - substrate - backgate;
        let drain = -(gate + body + source + backgate);
        let (drain, source) = if point.reversed {
            (source, drain)
        } else {
            (drain, source)
        };
        let scale = sign * self.multiplicity;
        Ok(BsimSoiIntrinsicCharges {
            gate: (gate * scale).checked("intrinsic gate charge")?,
            drain: (drain * scale).checked("intrinsic drain charge")?,
            source: (source * scale).checked("intrinsic source charge")?,
            body: (body * scale).checked("intrinsic body charge")?,
            backgate: (backgate * scale).checked("intrinsic backgate charge")?,
            inversion: (-inversion * scale).checked("intrinsic inversion charge")?,
        })
    }
}
impl CvOverdrive {
    fn prepare(
        size: &PreparedBsimSoiSize,
        channel: &PreparedBsimSoiChannel,
        mode: i32,
    ) -> Result<Self, ModelError> {
        let base = channel.electrostatic.base();
        Ok(Self {
            mode,
            noff: positive(binned(size, "noff")?, "CV noff")?,
            offset: binned(size, "delvt")?,
            dc_mstar: base.effective_inversion,
            dc_voff: channel.voff,
            cv_mstar: base.effective_inversion_cv,
            cv_voff: binned(size, "voffcv")?,
            cox: base.oxide_capacitance,
        })
    }
}
impl PreparedBsimSoiSetup {
    /// Bind the native CAPMOD=2 intrinsic charge law.
    ///
    /// # Errors
    /// Missing inputs, wrong CAPMOD or invalid charge geometry.
    pub fn prepare_capmod2(
        &self,
        index: usize,
        ambient: f64,
    ) -> Result<PreparedBsimSoiCapmod2, ModelError> {
        let model = self.model.parameters();
        if integer(model, "capmod")? != 2 {
            return Err(ModelError::InvalidPhysicalParameter("CAPMOD=2 required"));
        }
        let instance = self
            .instances
            .get(index)
            .ok_or(ModelError::MismatchedPreparedContext)?;
        let parameters = instance.parameters();
        let size = self
            .sizes
            .get(index)
            .ok_or(ModelError::MismatchedPreparedContext)?;
        let channel = self.prepare_channel(index, ambient)?;
        let geometry = prepare_geometry(model, parameters, size, &channel)?;
        let g = size.geometry();
        let ideal = channel.fd.is_some_and(|fd| fd.ideal);
        let width = g.weff_cv * positive(real(parameters, "nf")?, "CV fingers")?
            / positive(real(parameters, "nseg")?, "CV segments")?;
        let cox = channel.threshold.cox;
        let fbody = real(model, "fbody")?;
        let area = real(parameters, "agbcp")?;
        let normal = ChargeArea {
            inversion: cox * width.mul_add(g.leff_cv, area),
            bulk: cox * fbody * width.mul_add(g.leff_cv_body, area),
        };
        let opposite_area = real(parameters, "agbcp2")?;
        let opposite = if !ideal
            && instance.topology().body_mode != BsimSoiBodyMode::NoContact
            && opposite_area > 0.
        {
            Some(ChargeArea {
                inversion: cox * opposite_area,
                bulk: cox * fbody * opposite_area,
            })
        } else {
            None
        };
        let backgate_capacitance = if ideal {
            0.
        } else {
            binned(size, "kb1")? * fbody * EPS_OX
                / positive(real(model, "tbox")?, "CV buried oxide")?
                * width.mul_add(g.leff_cv_backgate, real(parameters, "aebcp")?)
        };
        Ok(PreparedBsimSoiCapmod2 {
            channel,
            geometry,
            overdrive: CvOverdrive::prepare(size, &channel, integer(model, "vgstcvmod")?)?,
            normal,
            opposite,
            partition: Partition::from_xpart(real(model, "xpart")?),
            bulk_cv_factor: g.abulk_cv_factor,
            backgate_capacitance,
            multiplicity: positive(real(parameters, "m")?, "CV multiplicity")?,
            ideal,
        })
    }
}
