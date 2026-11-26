//! Common source-derived BSIM3SOI channel transport primitives.
//! Copyright 1999/1990 Regents of the University of California. See package LICENSE.

use super::{Context, Transport, channel_threshold::ThresholdGeometry, differential::Differential};
use crate::{
    ModelError,
    preparation::{integer, positive},
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct TemperatureCoefficients {
    pub ambient: f64,
    pub nominal: f64,
    mobility_exponent: f64,
    velocity_slope: f64,
    resistance_slope: f64,
    resistance_base: f64,
    mobility_slopes: [f64; 3],
    pub selfheat: bool,
}

impl TemperatureCoefficients {
    pub(super) fn prepare(c: &Context<'_>) -> Result<Self, ModelError> {
        let mut resistance_base = c.binned("rdsw")?;
        if c.checked()? && resistance_base < 0.0 {
            resistance_base = 0.0;
        }
        Ok(Self {
            ambient: c.material.kelvin,
            nominal: positive(c.model("tnom")?, "tnom")?,
            mobility_exponent: c.binned("ute")?,
            velocity_slope: c.binned("at")?,
            resistance_slope: c.binned("prt")?,
            resistance_base,
            mobility_slopes: [c.binned("ua1")?, c.binned("ub1")?, c.binned("uc1")?],
            selfheat: integer(c.model.parameters(), "shmod")? == 1 && c.instance("rth0")? != 0.0,
        })
    }
}

/// Geometry-dependent channel bulk factor parameters used by FD, DD and PD.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct BulkParameters {
    pub a0: f64,
    ags: f64,
    pub keta: f64,
    b0: f64,
    b1: f64,
    junction_depth: f64,
}

impl BulkParameters {
    pub(super) fn prepare(c: &Context<'_>) -> Result<Self, ModelError> {
        Ok(Self {
            a0: c.binned("a0")?,
            ags: c.binned("ags")?,
            keta: c.binned("keta")?,
            b0: c.binned("b0")?,
            b1: c.binned("b1")?,
            junction_depth: c.model("xj")?,
        })
    }

    pub(super) fn base<const N: usize>(
        &self,
        g: &ThresholdGeometry,
        depth: Differential<N>,
        overdrive: Differential<N>,
    ) -> Differential<N> {
        let t = Differential::constant(g.length)
            / (Differential::constant(g.length)
                + depth.scale(self.junction_depth).sqrt().scale(2.0));
        t.scale(self.a0) + Differential::constant(self.b0 / (g.width + self.b1))
            - (t * t * t).scale(self.ags * self.a0) * overdrive
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct ChannelTransport {
    transport: Transport,
    pub temperature: TemperatureCoefficients,
    width: [f64; 2],
    resistance: [f64; 2],
    mobility_mode: i32,
    velocity: f64,
    saturation: [f64; 2],
    delta: f64,
    output: [f64; 3],
    max_exp: f64,
}

impl ChannelTransport {
    pub(super) fn prepare(c: &Context<'_>, transport: Transport) -> Result<Self, ModelError> {
        let mut a1 = c.binned("a1")?;
        let mut a2 = c.binned("a2")?;
        if c.checked()? {
            if a2 > 1.0 {
                a2 = 1.0;
                a1 = 0.0;
            } else if a2 < 0.01 {
                a2 = 0.01;
            }
        }
        Ok(Self {
            transport,
            temperature: TemperatureCoefficients::prepare(c)?,
            width: [c.binned("dwg")?, c.binned("dwb")?],
            resistance: [c.binned("prwg")?, c.binned("prwb")?],
            mobility_mode: integer(c.model.parameters(), "mobmod")?,
            velocity: c.binned("vsat")?,
            saturation: [a1, a2],
            delta: c.binned("delta")?,
            output: [c.binned("pclm")?, c.binned("pdiblcb")?, c.binned("pvag")?],
            max_exp: if c.pd() {
                2.688_117_142e43
            } else {
                5.834_617_425e14
            },
        })
    }
}

pub(super) fn one_plus_nine<const N: usize>(x: Differential<N>) -> Differential<N> {
    if x.value >= -0.9 {
        Differential::constant(1.0) + x
    } else {
        (Differential::constant(0.8) + x) / (Differential::constant(17.0) + x.scale(20.0))
    }
}

fn one_plus_eight<const N: usize>(x: Differential<N>) -> Differential<N> {
    if x.value >= -0.8 {
        Differential::constant(1.0) + x
    } else {
        (Differential::constant(0.6) + x) / (Differential::constant(7.0) + x.scale(10.0))
    }
}

fn effective_width<const N: usize>(w: Differential<N>) -> Differential<N> {
    if w.value < 2e-8 {
        (Differential::constant(4e-8) - w).scale(2e-8)
            / (Differential::constant(6e-8) - w.scale(2.0))
    } else {
        w
    }
}

pub(super) fn effective_bulk<const N: usize>(b: Differential<N>) -> Differential<N> {
    if b.value < 0.01 {
        (Differential::constant(0.02) - b) / (Differential::constant(3.0) - b.scale(200.0))
    } else {
        b
    }
}

pub(super) struct TransportInput<const N: usize> {
    pub drain: Differential<N>,
    pub thermal: Differential<N>,
    pub ratio_delta: Differential<N>,
    pub root_delta: Differential<N>,
    pub body: Differential<N>,
    pub overdrive: Differential<N>,
    pub threshold: Differential<N>,
    pub bulk: Differential<N>,
    pub width_factor: f64,
}

#[derive(Clone, Copy)]
struct Geometry<const N: usize> {
    width: Differential<N>,
    resistance: Differential<N>,
}

#[derive(Clone, Copy)]
struct Mobility<const N: usize> {
    mobility: Differential<N>,
    velocity: Differential<N>,
}

struct Saturation<const N: usize> {
    voltage: Differential<N>,
    effective: Differential<N>,
    excess: Differential<N>,
    field_length: Differential<N>,
    wvcox_rds: Differential<N>,
    lambda: Differential<N>,
    drive: Differential<N>,
}

pub(super) struct TransportPoint<const N: usize> {
    pub current: Differential<N>,
    pub width: Differential<N>,
    pub mobility: Differential<N>,
    pub saturation: Differential<N>,
    pub effective_drain: Differential<N>,
    pub resistance: Differential<N>,
    pub field_length: Differential<N>,
}

impl ChannelTransport {
    fn geometry<const N: usize>(
        &self,
        g: &ThresholdGeometry,
        b: &TransportInput<N>,
    ) -> Geometry<N> {
        let width = effective_width(
            Differential::constant(g.width)
                - (b.overdrive.scale(self.width[0]) + b.root_delta.scale(self.width[1]))
                    .scale(b.width_factor),
        );
        let rds0 = if self.temperature.selfheat {
            (Differential::constant(self.temperature.resistance_base)
                + b.ratio_delta.scale(self.temperature.resistance_slope))
            .scale(self.transport.resistance_denominator.recip())
        } else {
            Differential::constant(self.transport.series_resistance)
        };
        let resistance = rds0
            * one_plus_nine(
                b.overdrive.scale(self.resistance[0]) + b.root_delta.scale(self.resistance[1]),
            );
        Geometry { width, resistance }
    }

    fn mobility<const N: usize>(
        &self,
        g: &ThresholdGeometry,
        b: &TransportInput<N>,
    ) -> Mobility<N> {
        let (coeff, u0, velocity) = if self.temperature.selfheat {
            let coeff = std::array::from_fn(|i| {
                Differential::constant(self.transport.nominal_mobility_coefficients[i])
                    + b.ratio_delta.scale(self.temperature.mobility_slopes[i])
            });
            let u0 = (Differential::constant(1.0) + b.ratio_delta)
                .powf(self.temperature.mobility_exponent)
                .scale(self.transport.nominal_mobility);
            let velocity = Differential::constant(self.velocity)
                - b.ratio_delta.scale(self.temperature.velocity_slope);
            (coeff, u0, velocity)
        } else {
            (
                self.transport
                    .mobility_coefficients
                    .map(Differential::constant),
                Differential::constant(self.transport.mobility),
                Differential::constant(self.transport.saturation_velocity),
            )
        };
        let [ua, ub, uc] = coeff;
        let field = (b.overdrive + b.threshold.scale(2.0)).scale(g.tox.recip());
        let correction = match self.mobility_mode {
            1 => field * (ua + uc * b.body + ub * field),
            2 => {
                let f = b.overdrive.scale(g.tox.recip());
                f * (ua + uc * b.body + ub * f)
            }
            _ => field * (ua + ub * field) * (Differential::constant(1.0) + uc * b.body),
        };
        Mobility {
            mobility: u0 / one_plus_eight(correction),
            velocity,
        }
    }

    fn lambda<const N: usize>(&self, drive: Differential<N>) -> Differential<N> {
        let [a1, a2] = self.saturation;
        if a1 == 0.0 {
            Differential::constant(a2)
        } else if a1 > 0.0 {
            let t = Differential::constant(1.0 - a2 - 0.0001) - drive.scale(a1);
            Differential::constant(1.0)
                - (t + (t * t + Differential::constant(0.0004 * (1.0 - a2))).sqrt()).scale(0.5)
        } else {
            let t = Differential::constant(a2 - 0.0001) + drive.scale(a1);
            (t + (t * t + Differential::constant(0.0004 * a2)).sqrt()).scale(0.5)
        }
    }

    fn saturation<const N: usize>(
        &self,
        g: &ThresholdGeometry,
        b: &TransportInput<N>,
        geometry: &Geometry<N>,
        mobility: &Mobility<N>,
    ) -> Saturation<N> {
        let drive = b.overdrive + b.thermal.scale(2.0);
        let field_length = mobility.velocity.scale(2.0 * g.length) / mobility.mobility;
        let wvcox_rds = geometry.width * mobility.velocity * geometry.resistance.scale(g.oxide);
        let lambda = self.lambda(b.overdrive);
        let voltage = if geometry.resistance.value == 0.0 && (lambda.value - 1.0).abs() == 0.0 {
            field_length * drive / (b.bulk * field_length + drive)
        } else {
            let bulk_resistance = b.bulk * wvcox_rds;
            let quadratic = b.bulk.scale(2.0)
                * (bulk_resistance - Differential::constant(1.0) + lambda.reciprocal());
            let linear = drive * (lambda.reciprocal().scale(2.0) - Differential::constant(1.0))
                + b.bulk * field_length
                + (drive * bulk_resistance).scale(3.0);
            let constant = drive * (field_length + (drive * wvcox_rds).scale(2.0));
            constant.scale(2.0)
                / (linear + (linear * linear - (quadratic * constant).scale(2.0)).sqrt())
        };
        let difference = voltage - b.drain - Differential::constant(self.delta);
        let raw = voltage
            - (difference + (difference * difference + voltage.scale(4.0 * self.delta)).sqrt())
                .scale(0.5);
        let effective = if raw.value > b.drain.value {
            Differential {
                value: b.drain.value,
                ..raw
            }
        } else {
            raw
        };
        Saturation {
            voltage,
            effective,
            excess: b.drain - effective,
            field_length,
            wvcox_rds,
            lambda,
            drive,
        }
    }

    fn early_voltage<const N: usize>(
        &self,
        g: &ThresholdGeometry,
        b: &TransportInput<N>,
        v: &Saturation<N>,
    ) -> Differential<N> {
        let [pclm, pdiblcb, pvag] = self.output;
        let t = Differential::constant(1.0) - (b.bulk * v.voltage / v.drive).scale(0.5);
        let vasat = (v.field_length + v.voltage + (v.wvcox_rds * b.overdrive * t).scale(2.0))
            / (v.lambda.reciprocal().scale(2.0) - Differential::constant(1.0)
                + v.wvcox_rds * b.bulk);
        let large = Differential::constant(self.max_exp);
        let vaclm = if pclm > 0.0 && v.excess.value > 1e-10 {
            (b.bulk + b.overdrive / v.field_length)
                * v.excess
                    .scale(g.length / (pclm * g.electrostatics.lateral_length))
                / b.bulk
        } else {
            large
        };
        let theta = g.electrostatics.output_resistance_factor;
        let vadibl = if theta > 0.0 {
            let t = b.bulk * v.voltage;
            (v.drive - v.drive * t / (v.drive + t)).scale(theta.recip())
                / one_plus_nine(b.body.scale(pdiblcb))
        } else {
            large
        };
        vasat
            + one_plus_nine((b.overdrive / v.field_length).scale(pvag)) * vaclm * vadibl
                / (vaclm + vadibl)
    }

    pub(super) fn evaluate<const N: usize>(
        &self,
        g: &ThresholdGeometry,
        b: &TransportInput<N>,
    ) -> TransportPoint<N> {
        let geometry = self.geometry(g, b);
        let mobility = self.mobility(g, b);
        let sat = self.saturation(g, b, &geometry, &mobility);
        let va = self.early_voltage(g, b, &sat);
        let beta = mobility.mobility * geometry.width.scale(g.oxide / g.length);
        let gche = beta
            * b.overdrive
            * (Differential::constant(1.0) - (b.bulk * sat.effective / sat.drive).scale(0.5))
            / (Differential::constant(1.0) + sat.effective / sat.field_length);
        let idl = gche * sat.effective / (Differential::constant(1.0) + gche * geometry.resistance);
        let current = idl * (Differential::constant(1.0) + sat.excess / va);
        TransportPoint {
            current,
            width: geometry.width,
            mobility: mobility.mobility,
            saturation: sat.voltage,
            effective_drain: sat.effective,
            resistance: geometry.resistance,
            field_length: sat.field_length,
        }
    }
}
