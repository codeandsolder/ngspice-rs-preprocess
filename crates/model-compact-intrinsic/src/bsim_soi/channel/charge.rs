//! Source-defined classical intrinsic charge and CV overdrive laws.
//! Copyright 2010 Regents of the University of California. See package LICENSE.
use super::super::{
    BsimSoiBodyMode, EPS_OX, PreparedBsimSoiSetup, PreparedBsimSoiSize, binned, integer, positive,
    real,
};
use super::{
    BsimSoiChannelBias, BsimSoiChannelPoint, BsimSoiChannelScalar, Jet, PreparedBsimSoiChannel,
    centroid::{Centroid, CentroidTrial, physical_oxide},
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
    fn source(self, overdrive: Jet, drop: Jet, scale: Jet, inversion: Jet, bulk: Jet) -> Jet {
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
struct LayerState {
    overdrive: Jet,
    inversion_scale: Jet,
    bulk_scale: Jet,
    centroid: bool,
}
impl LayerState {
    const fn classical(overdrive: Jet) -> Self {
        Self {
            overdrive,
            inversion_scale: Jet::constant(1.),
            bulk_scale: Jet::constant(1.),
            centroid: false,
        }
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
        layer: LayerState,
        drain: Jet,
        bulk: Jet,
        partition: Partition,
        ideal: bool,
    ) -> [Jet; 3] {
        let overdrive = layer.overdrive;
        let saturation = overdrive / bulk;
        let delta = saturation - drain - 0.02;
        let root = (delta * delta + 0.08 * saturation).sqrt();
        let effective = if saturation.value >= 0. {
            2. * saturation * drain / (saturation + drain + 0.02 + root)
        } else {
            saturation - 0.5 * (delta + root)
        };
        let drop = bulk * effective;
        let denominator = 12. * (overdrive - 0.5 * drop + 1e-20);
        let inversion_scale = self.inversion * layer.inversion_scale;
        let inversion = inversion_scale * (overdrive - 0.5 * drop + drop * drop / denominator);
        let body = if ideal {
            Jet::constant(0.)
        } else {
            self.bulk
                * layer.bulk_scale
                * (1. - bulk)
                * (0.5 * effective - drop * effective / denominator)
        };
        let source = partition.source(
            overdrive,
            drop,
            inversion_scale,
            inversion,
            if layer.centroid {
                Jet::constant(0.)
            } else {
                body
            },
        );
        [inversion, body, source]
    }

    fn substrate(
        self,
        flatband: Jet,
        gate: Jet,
        body: Jet,
        overdrive: Jet,
        k1: f64,
        smoothing: [f64; 2],
    ) -> Jet {
        let [delta, multiplier] = smoothing;
        let shift = flatband - gate + body - delta;
        let sign = if flatband.value <= 0. { -1. } else { 1. };
        let accumulation = shift.half_positive(multiplier * delta * sign * flatband);
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

/// Prepared intrinsic CAPMOD=2 or CAPMOD=3 storage, independent of
/// extrinsic and junction charges.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreparedBsimSoiIntrinsicCharges {
    channel: PreparedBsimSoiChannel,
    centroid: Option<Centroid>,
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
#[derive(Clone, Copy)]
struct LayerInput<'a> {
    area: ChargeArea,
    overdrive: Jet,
    gate: Jet,
    flatband: Jet,
    point: &'a BsimSoiChannelPoint,
    bulk: Jet,
    centroid: Option<CentroidTrial>,
    opposite: bool,
}
impl PreparedBsimSoiIntrinsicCharges {
    fn layer(&self, input: &LayerInput<'_>) -> (Jet, Jet, Jet, Jet) {
        let LayerInput {
            area,
            overdrive,
            gate,
            flatband,
            point,
            bulk,
            centroid,
            opposite,
        } = *input;
        let (layer, accumulation, smoothing) =
            if let (Some(law), Some(trial)) = (self.centroid, centroid) {
                let factor = law.inversion_factor(trial, point.threshold, overdrive);
                let acc = if self.ideal {
                    Jet::constant(0.)
                } else {
                    law.accumulation_factor(gate, point.body_effective, flatband)
                };
                (
                    LayerState {
                        overdrive: law.effective_overdrive(trial, overdrive),
                        inversion_scale: factor,
                        bulk_scale: factor,
                        centroid: true,
                    },
                    acc,
                    0.02,
                )
            } else {
                (LayerState::classical(overdrive), Jet::constant(1.), 0.08)
            };
        let [inversion, body, source] =
            area.channel(layer, point.drain_voltage, bulk, self.partition, self.ideal);
        let substrate = if self.ideal {
            Jet::constant(0.)
        } else {
            area.substrate(
                flatband,
                gate,
                point.body_effective,
                overdrive,
                self.channel.threshold.k1ox,
                [smoothing, if opposite { 100. } else { 4. }],
            ) * accumulation
        };
        (inversion, body, source, substrate)
    }

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
        let centroid = self
            .centroid
            .map(|law| law.trial(&self.channel, material, self.overdrive.offset, self.ideal));
        let flatband = centroid.map_or_else(
            || {
                point.threshold - phi - self.channel.threshold.k1 * point.surface_potential.sqrt()
                    + self.overdrive.offset
            },
            |trial| trial.flatband,
        );
        let (mut inversion, mut body, mut source, mut substrate) = self.layer(&LayerInput {
            area: self.normal,
            overdrive: normal,
            gate: point.gate_voltage,
            flatband,
            point: &point,
            bulk,
            centroid,
            opposite: false,
        });
        if let Some(area) = self.opposite {
            let sign = self.channel.polarity;
            let mut gate = Jet::coordinate(sign * bias.gate, 0, sign);
            if point.reversed {
                gate = gate + point.drain_voltage;
            }
            let (inv, bod, src, sub) = self.layer(&LayerInput {
                area,
                overdrive: opposite,
                gate,
                flatband: flatband + 1.12,
                point: &point,
                bulk,
                centroid,
                opposite: true,
            });
            inversion = inversion + inv;
            body = body + bod;
            source = source + src;
            substrate = substrate + sub;
        }
        let sign = self.channel.polarity;
        let mut backgate = Jet::coordinate(sign * bias.backgate, 3, sign);
        if point.reversed {
            backgate = backgate + point.drain_voltage;
        }
        let backgate = self.backgate_capacitance
            * (backgate - Jet::thermal(material.backgate_flatband) - point.body_voltage);
        let gate = inversion + substrate
            - if self.centroid.is_some() {
                body
            } else {
                Jet::constant(0.)
            };
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
    /// Bind the native CAPMOD=2 or 3 intrinsic charge law.
    ///
    /// # Errors
    /// Missing inputs, unsupported CAPMOD or invalid charge geometry.
    pub fn prepare_intrinsic_charges(
        &self,
        index: usize,
        ambient: f64,
    ) -> Result<PreparedBsimSoiIntrinsicCharges, ModelError> {
        let oxides = self.charge_oxides(ambient)?;
        self.prepare_charge_with_oxides(index, ambient, &oxides)
    }

    /// Bind every instance in native model order, sharing the CV oxide preparation.
    /// # Errors
    /// Invalid instance, model or charge geometry.
    pub fn prepare_all_intrinsic_charges(
        &self,
        ambient: f64,
    ) -> Result<Vec<PreparedBsimSoiIntrinsicCharges>, ModelError> {
        let oxides = self.charge_oxides(ambient)?;
        (0..self.instances.len())
            .map(|index| self.prepare_charge_with_oxides(index, ambient, &oxides))
            .collect()
    }

    fn charge_oxides(&self, ambient: f64) -> Result<Vec<[f64; 2]>, ModelError> {
        let model = self.model.parameters();
        if integer(model, "capmod")? != 3 {
            return Ok(Vec::new());
        }
        let mut cache = std::collections::HashMap::new();
        let mut last = 0.;
        let mut oxides = Vec::with_capacity(self.instances.len());
        for (index, instance) in self.instances.iter().enumerate() {
            let key = super::super::native_size_key(instance)?;
            let size_oxide = match cache.entry(key) {
                std::collections::hash_map::Entry::Occupied(entry) => *entry.get(),
                std::collections::hash_map::Entry::Vacant(entry) => {
                    last = physical_oxide(
                        model,
                        &self.sizes[index],
                        &self.prepare_channel(index, ambient)?,
                    )?;
                    *entry.insert(last)
                }
            };
            oxides.push([0., size_oxide]);
        }
        for oxide in &mut oxides {
            oxide[0] = last;
        }
        Ok(oxides)
    }

    fn prepare_charge_with_oxides(
        &self,
        index: usize,
        ambient: f64,
        oxides: &[[f64; 2]],
    ) -> Result<PreparedBsimSoiIntrinsicCharges, ModelError> {
        let model = self.model.parameters();
        let mode = integer(model, "capmod")?;
        if !matches!(mode, 2 | 3) {
            return Err(ModelError::InvalidPhysicalParameter("unsupported CAPMOD"));
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
        let mut normal = ChargeArea {
            inversion: cox * width.mul_add(g.leff_cv, area),
            bulk: cox * fbody * width.mul_add(g.leff_cv_body, area),
        };
        let opposite_area = real(parameters, "agbcp2")?;
        let mut opposite = if !ideal
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
        let centroid = if mode == 3 {
            let oxides = *oxides
                .get(index)
                .ok_or(ModelError::MismatchedPreparedContext)?;
            let oxide = oxides[0];
            let physical_ratio = real(model, "tox")? / oxide;
            normal.inversion *= channel.threshold.toxe / oxide;
            normal.bulk *= physical_ratio;
            if let Some(area) = opposite.as_mut() {
                area.inversion *= physical_ratio;
                area.bulk *= physical_ratio;
            }
            let stress = self.prepare_transport(index, ambient)?.stress_offsets();
            let unstressed = channel
                .polarity
                .mul_add(-stress.threshold, channel.threshold.vth0);
            Some(Centroid::prepare(
                model, size, &channel, unstressed, oxides,
            )?)
        } else {
            None
        };
        Ok(PreparedBsimSoiIntrinsicCharges {
            channel,
            centroid,
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
