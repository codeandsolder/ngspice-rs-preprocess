//! Native gate-channel, edge, body and body-contact tunneling.
//! Copyright 2010 Regents of the University of California. See package LICENSE.
use super::super::{
    BsimSoiBodyMode, PreparedBsimSoiSetup, PreparedBsimSoiSize, binned, integer, positive, real,
};
use super::{
    BsimSoiChannelBias, BsimSoiChannelPoint, BsimSoiChannelScalar, Jet, PreparedBsimSoiChannel,
    PreparedBsimSoiDrainChannel,
};
use crate::{BoundParameters, ModelError};

/// Channel bias and independent physical contact-to-source-prime voltage.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BsimSoiGateBias {
    pub channel: BsimSoiChannelBias,
    pub contact: f64,
}

/// Currents leaving the gate into each physical terminal, including M and NF
/// where the source law uses them. The contact partial holds Vgs fixed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BsimSoiGateCurrents {
    pub source_edge: BsimSoiChannelScalar,
    pub drain_edge: BsimSoiChannelScalar,
    pub source_channel: BsimSoiChannelScalar,
    pub drain_channel: BsimSoiChannelScalar,
    pub body: BsimSoiChannelScalar,
    pub contact: BsimSoiChannelScalar,
    pub d_contact: f64,
}

impl BsimSoiGateCurrents {
    const ZERO: Self = Self {
        source_edge: Jet::constant(0.),
        drain_edge: Jet::constant(0.),
        source_channel: Jet::constant(0.),
        drain_channel: Jet::constant(0.),
        body: Jet::constant(0.),
        contact: Jet::constant(0.),
        d_contact: 0.,
    };
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Barrier {
    a: f64,
    b: f64,
    c: f64,
    exponent: f64,
}

impl Barrier {
    fn transmission(self, field: Jet) -> Jet {
        (self.exponent
            * (self.a + (self.a.mul_add(self.c, -self.b) - self.b * self.c * field) * field))
            .bounded_exp()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ChannelLaw {
    barrier: Barrier,
    prefactor: f64,
    ideality: f64,
    partition: f64,
}

impl ChannelLaw {
    fn evaluate(
        self,
        point: &BsimSoiChannelPoint,
        field: Jet,
        vtm: Jet,
        threshold: f64,
    ) -> (Jet, Jet) {
        let voltage = point.gate_voltage;
        let scale = vtm * self.ideality;
        let offset = voltage - threshold;
        let argument = offset / scale;
        let auxiliary = if argument.value > 100. {
            offset
        } else if argument.value < -100. {
            Jet::constant(0.)
        } else {
            scale * (1. + argument.exp()).ln()
        };
        let current = self.prefactor * voltage * auxiliary * self.barrier.transmission(field);
        let x = -self.partition * point.drain_voltage;
        let e = x.bounded_exp();
        let denominator = x * x + 2e-4;
        // expm1 prevents loss of partition accuracy near Vds=0.
        let difference = if x.value.abs() <= 100. {
            x.exp_m1()
        } else {
            e - 1.
        };
        let source = (difference - x + 1e-4) / denominator;
        let drain = (x * e - difference + 1e-4) / denominator;
        (current * source, current * drain)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct EdgeLaw {
    barrier: Barrier,
    source: f64,
    drain: f64,
    flatband: f64,
}

impl EdgeLaw {
    fn evaluate(self, voltage: Jet, prefactor: f64) -> Jet {
        let offset = voltage - self.flatband;
        let field = (offset * offset + 1e-4).sqrt();
        prefactor * voltage * field * self.barrier.transmission(field)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct OxideFields {
    flatband: Jet,
    accumulation: Jet,
    depletion: Jet,
    gate_body: Jet,
}

impl OxideFields {
    fn evaluate(channel: &PreparedBsimSoiChannel, point: &BsimSoiChannelPoint, phi: Jet) -> Self {
        let flatband = channel.threshold.vth0 - phi - channel.threshold.k1 * phi.sqrt();
        let gate_body = point.gate_voltage - point.body_voltage;
        let sign = if flatband.value <= 0. { -1. } else { 1. };
        let accumulation = (flatband - gate_body - 0.02).half_positive(0.08 * sign * flatband);
        let effective = flatband - accumulation;
        let k1 = channel.threshold.k1ox;
        let depletion = if k1 == 0. {
            Jet::constant(0.)
        } else {
            let offset =
                point.gate_voltage - point.effective_overdrive - effective - point.body_effective;
            let root = if offset.value < 0. {
                offset / k1
            } else {
                2. * offset / (k1 * (1. + (1. + 4. * offset / (k1 * k1)).sqrt()))
            };
            gate_body - root * root - flatband
        };
        Self {
            flatband,
            accumulation,
            depletion,
            gate_body,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct BodyBarrier {
    prefactor: f64,
    exponent: f64,
    alpha: f64,
    beta: f64,
    voltage: f64,
    auxiliary_scale: f64,
}
impl BodyBarrier {
    fn evaluate(self, field: Jet, auxiliary: Jet, area: Jet, gate_body: Jet) -> Jet {
        let denominator = if self.voltage == 0. {
            Jet::constant(1.)
        } else {
            1. - field / self.voltage
        };
        let denominator = if denominator.value < 0.01 {
            Jet::constant(0.01)
        } else {
            denominator
        };
        let transmission =
            (self.exponent * (self.alpha - self.beta * field) / denominator).limited_exp();
        let auxiliary =
            self.auxiliary_scale * (1. + (auxiliary / self.auxiliary_scale).limited_exp()).ln();
        self.prefactor * area * gate_body * auxiliary * transmission
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct BodyLaw {
    inversion: BodyBarrier,
    accumulation: BodyBarrier,
    max_field: f64,
    delta: f64,
    bandgap: f64,
    length_per_segment: f64,
    extra_area: f64,
}
impl BodyLaw {
    fn evaluate(self, fields: OxideFields, width: Jet) -> Jet {
        let area = width * self.length_per_segment + self.extra_area;
        let limit = |field: Jet| {
            self.max_field
                - (self.max_field - field - self.delta)
                    .half_positive(Jet::constant(4. * self.delta * self.max_field))
        };
        if fields.gate_body.value >= 0. {
            let field = limit(fields.depletion);
            self.inversion
                .evaluate(field, field - self.bandgap, area, fields.gate_body)
        } else {
            let field = limit(fields.accumulation);
            self.accumulation.evaluate(
                field,
                fields.flatband - fields.gate_body,
                area,
                fields.gate_body,
            )
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ContactLaw {
    barrier: Barrier,
    prefactor: f64,
}
impl ContactLaw {
    fn evaluate(self, voltage: Jet, flatband: Jet) -> Jet {
        let offset = voltage - flatband - 1.12;
        if offset.value >= 0. {
            return Jet::constant(0.);
        }
        let field = 0.5 * ((offset * offset + 1e-4).sqrt() - offset - 0.01);
        self.prefactor * voltage * field * self.barrier.transmission(field)
    }
}

/// Prepared gate tunneling laws; disabled selectors do not evaluate unused laws.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreparedBsimSoiGateTunneling {
    channel: PreparedBsimSoiChannel,
    drain: Option<PreparedBsimSoiDrainChannel>,
    conduction: Option<ChannelLaw>,
    edge: Option<EdgeLaw>,
    body: Option<BodyLaw>,
    contact: Option<ContactLaw>,
    scale: f64,
    contact_scale: f64,
}

impl PreparedBsimSoiGateTunneling {
    /// Evaluate physical terminal currents and analytic bias derivatives.
    ///
    /// # Errors
    /// Nonfinite bias or invalid constitutive domain.
    pub fn evaluate(&self, bias: BsimSoiGateBias) -> Result<BsimSoiGateCurrents, ModelError> {
        if !bias.contact.is_finite() {
            return Err(ModelError::InvalidPhysicalParameter("gate contact bias"));
        }
        let point = self.channel.evaluate(bias.channel)?;
        let material = self.channel.material.evaluate(bias.channel.kelvin)?;
        let fields = OxideFields::evaluate(
            &self.channel,
            &point,
            Jet::thermal(material.surface_potential),
        );
        let mut output = BsimSoiGateCurrents::ZERO;
        if let Some(law) = self.conduction {
            let (source, drain) = law.evaluate(
                &point,
                fields.depletion,
                Jet::thermal(material.thermal_voltage),
                self.channel.threshold.vth0,
            );
            let (source, drain) = if point.reversed {
                (drain, source)
            } else {
                (source, drain)
            };
            output.source_channel = (source * self.scale).checked("gate source channel current")?;
            output.drain_channel = (drain * self.scale).checked("gate drain channel current")?;
        }
        let sign = self.channel.polarity;
        let gate = Jet::coordinate(sign * bias.channel.gate, 0, sign);
        let drain = Jet::coordinate(sign * bias.channel.drain, 1, sign);
        if let Some(law) = self.edge {
            output.source_edge = (law.evaluate(gate, law.source) * self.scale)
                .checked("gate source edge current")?;
            output.drain_edge = (law.evaluate(gate - drain, law.drain) * self.scale)
                .checked("gate drain edge current")?;
        }
        if let (Some(law), Some(channel)) = (self.body, self.drain.as_ref()) {
            output.body = (law.evaluate(fields, channel.evaluate(bias.channel)?.effective_width)
                * self.scale)
                .checked("gate body current")?;
        }
        if let Some(law) = self.contact {
            let voltage = gate - sign * bias.contact;
            output.contact = (law.evaluate(voltage, fields.flatband) * self.contact_scale)
                .checked("gate contact current")?;
            output.d_contact = law
                .evaluate(
                    Jet::coordinate(voltage.value, 0, -sign),
                    Jet::constant(fields.flatband.value),
                )
                .gradient[0]
                * self.contact_scale;
            if !output.d_contact.is_finite() {
                return Err(ModelError::InvalidPhysicalParameter(
                    "gate contact derivative",
                ));
            }
        }
        Ok(output)
    }
}

fn barrier(
    size: &PreparedBsimSoiSize,
    names: [&str; 3],
    exponent: f64,
) -> Result<Barrier, ModelError> {
    Ok(Barrier {
        a: binned(size, names[0])?,
        b: binned(size, names[1])?,
        c: binned(size, names[2])?,
        exponent,
    })
}

#[derive(Clone, Copy)]
struct TunnelingGeometry {
    width_per_segment: f64,
    length_per_segment: f64,
    area: f64,
    extra_area: f64,
    fingers: f64,
    oxide_ratio: f64,
    oxide: f64,
}

fn geometry(
    model: &BoundParameters,
    p: &BoundParameters,
    size: &PreparedBsimSoiSize,
) -> Result<TunnelingGeometry, ModelError> {
    let oxide = positive(real(model, "toxqm")?, "gate tunneling oxide")?;
    let oxide_ratio = (positive(real(model, "toxref")?, "gate reference oxide")? / oxide)
        .powf(real(model, "ntox")?)
        / oxide.powi(2);
    let g = size.geometry();
    let segments = positive(real(p, "nseg")?, "gate tunneling segments")?;
    let fingers = positive(real(p, "nf")?, "gate tunneling fingers")?;
    let extra_area = real(p, "agbcpd")? / fingers;
    Ok(TunnelingGeometry {
        width_per_segment: g.weff / segments,
        length_per_segment: g.leff / segments,
        area: (g.weff / segments).mul_add(g.leff, extra_area),
        extra_area,
        fingers,
        oxide_ratio,
        oxide,
    })
}

fn channel_laws(
    model: &BoundParameters,
    p: &BoundParameters,
    size: &PreparedBsimSoiSize,
    g: TunnelingGeometry,
    sign: f64,
    flatband: f64,
) -> Result<(ChannelLaw, EdgeLaw), ModelError> {
    let (a, b) = if sign > 0. {
        (4.97232e-7, 7.45669e11)
    } else {
        (3.42537e-7, 1.16645e12)
    };
    let edge_factor = positive(binned(size, "poxedge")?, "gate edge oxide factor")?;
    let edge_ratio = (real(model, "toxref")? / (g.oxide * edge_factor)).powf(real(model, "ntox")?)
        / (g.oxide * edge_factor).powi(2);
    let edge_prefactor = a * size.geometry().gate_tunneling_length_correction * edge_ratio;
    Ok((
        ChannelLaw {
            barrier: barrier(size, ["aigc", "bigc", "cigc"], -b * g.oxide)?,
            prefactor: a * g.area * g.oxide_ratio,
            ideality: positive(binned(size, "nigc")?, "gate channel ideality")?,
            partition: binned(size, "pigcd")?,
        },
        EdgeLaw {
            barrier: barrier(
                size,
                ["aigsd", "bigsd", "cigsd"],
                -b * g.oxide * edge_factor,
            )?,
            source: edge_prefactor * (g.width_per_segment + real(p, "psbcp")?),
            drain: edge_prefactor * (g.width_per_segment + real(p, "pdbcp")?),
            flatband,
        },
    ))
}

fn body_law(
    model: &BoundParameters,
    size: &PreparedBsimSoiSize,
    g: TunnelingGeometry,
) -> Result<BodyLaw, ModelError> {
    Ok(BodyLaw {
        inversion: BodyBarrier {
            prefactor: 3.7622e-7 * g.oxide_ratio,
            exponent: -3.1051e10 * g.oxide,
            alpha: binned(size, "alphagb1")?,
            beta: binned(size, "betagb1")?,
            voltage: real(model, "vgb1")?,
            auxiliary_scale: positive(real(model, "vevb")?, "gate inversion auxiliary scale")?,
        },
        accumulation: BodyBarrier {
            prefactor: 4.9758e-7 * g.oxide_ratio,
            exponent: -2.357e10 * g.oxide,
            alpha: binned(size, "alphagb2")?,
            beta: binned(size, "betagb2")?,
            voltage: real(model, "vgb2")?,
            auxiliary_scale: positive(real(model, "vecb")?, "gate accumulation auxiliary scale")?,
        },
        max_field: positive(real(model, "voxh")?, "gate oxide voltage limit")?,
        delta: positive(real(model, "deltavox")?, "gate oxide smoothing")?,
        bandgap: real(model, "ebg")?,
        length_per_segment: g.length_per_segment,
        extra_area: g.extra_area,
    })
}

impl PreparedBsimSoiSetup {
    /// Bind source-defined gate tunneling laws for a resolved instance.
    ///
    /// # Errors
    /// Missing model data or invalid gate geometry.
    pub fn prepare_gate_tunneling(
        &self,
        index: usize,
        ambient: f64,
    ) -> Result<PreparedBsimSoiGateTunneling, ModelError> {
        let channel = self.prepare_channel(index, ambient)?;
        let model = self.model.parameters();
        let instance = self
            .instances
            .get(index)
            .ok_or(ModelError::MismatchedPreparedContext)?;
        let parameters = instance.parameters();
        let size = self
            .sizes
            .get(index)
            .ok_or(ModelError::MismatchedPreparedContext)?;
        let igc = integer(model, "igcmod")? != 0;
        let igb = integer(model, "igbmod")? != 0 && !channel.fd.is_some_and(|fd| fd.ideal);
        let sign = channel.polarity;
        let multiplicity = positive(real(parameters, "m")?, "gate current multiplicity")?;
        let geom = geometry(model, parameters, size)?;
        let (conduction, edge) = if igc {
            let (channel, edge) = channel_laws(
                model,
                parameters,
                size,
                geom,
                sign,
                self.prepare_transport(index, ambient)?
                    .source_drain_flatband(),
            )?;
            (Some(channel), Some(edge))
        } else {
            (None, None)
        };
        let body = if igb {
            Some(body_law(model, size, geom)?)
        } else {
            None
        };
        let drain = if igb {
            Some(self.prepare_drain_channel(index, ambient)?)
        } else {
            None
        };
        let area = real(parameters, "agbcp2")?;
        let contact =
            if igb && instance.topology().body_mode != BsimSoiBodyMode::NoContact && area > 0. {
                let (a, b) = if sign > 0. {
                    (3.42537e-7, 1.16645e12)
                } else {
                    (4.97232e-7, 7.45669e11)
                };
                Some(ContactLaw {
                    barrier: barrier(size, ["aigbcp2", "bigbcp2", "cigbcp2"], -b * geom.oxide)?,
                    prefactor: a * area * geom.oxide_ratio,
                })
            } else {
                None
            };
        Ok(PreparedBsimSoiGateTunneling {
            channel,
            drain,
            conduction,
            edge,
            body,
            contact,
            scale: sign * geom.fingers * multiplicity,
            contact_scale: sign * multiplicity,
        })
    }
}
