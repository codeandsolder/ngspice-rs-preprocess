//! PD mobility, saturation, output resistance and channel current.
//! Copyright 1999/1990 Regents of the University of California. See package LICENSE.

use super::super::Context;
use super::{Bias, D, PdChannelEvaluation, PreparedPdChannel, State};
use crate::{ModelError, preparation::integer};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct ChannelTransport {
    width: [f64; 2],
    resistance: [f64; 2],
    bulk: [f64; 6],
    junction_depth: f64,
    mobility_mode: i32,
    velocity: f64,
    saturation: [f64; 2],
    delta: f64,
    output: [f64; 3],
}

impl ChannelTransport {
    pub(super) fn prepare(c: &Context<'_>) -> Result<Self, ModelError> {
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
            width: [c.binned("dwg")?, c.binned("dwb")?],
            resistance: [c.binned("prwg")?, c.binned("prwb")?],
            bulk: [
                c.binned("a0")?,
                c.binned("ags")?,
                c.binned("keta")?,
                c.binned("ketas")?,
                c.binned("b0")?,
                c.binned("b1")?,
            ],
            junction_depth: c.model("xj")?,
            mobility_mode: integer(c.model.parameters(), "mobmod")?,
            velocity: c.binned("vsat")?,
            saturation: [a1, a2],
            delta: c.binned("delta")?,
            output: [c.binned("pclm")?, c.binned("pdiblcb")?, c.binned("pvag")?],
        })
    }
}

fn one_plus_nine(x: D) -> D {
    if x.value >= -0.9 {
        D::constant(1.0) + x
    } else {
        (D::constant(0.8) + x) / (D::constant(17.0) + x.scale(20.0))
    }
}

fn one_plus_eight(x: D) -> D {
    if x.value >= -0.8 {
        D::constant(1.0) + x
    } else {
        (D::constant(0.6) + x) / (D::constant(7.0) + x.scale(10.0))
    }
}

fn effective_width(w: D) -> D {
    if w.value < 2e-8 {
        (D::constant(4e-8) - w).scale(2e-8) / (D::constant(6e-8) - w.scale(2.0))
    } else {
        w
    }
}

fn effective_bulk(b: D) -> D {
    if b.value < 0.01 {
        (D::constant(0.02) - b) / (D::constant(3.0) - b.scale(200.0))
    } else {
        b
    }
}

#[derive(Clone, Copy)]
struct Geometry {
    width: D,
    resistance: D,
    bulk: D,
}

#[derive(Clone, Copy)]
struct Mobility {
    mobility: D,
    velocity: D,
}

#[derive(Clone, Copy)]
struct Saturation {
    voltage: D,
    effective: D,
    excess: D,
    field_length: D,
    wvcox_rds: D,
    lambda: D,
    drive: D,
}

impl PreparedPdChannel {
    fn geometry(&self, b: &Bias, s: &State) -> Geometry {
        let p = self.channel;
        let root_delta = s.root - D::constant(b.phi.sqrt());
        let width = effective_width(
            D::constant(self.width)
                - (s.overdrive.scale(p.width[0]) + root_delta.scale(p.width[1]))
                    .scale(2.0 - self.body_contacts),
        );
        let rds0 = if self.temperature.selfheat {
            (D::constant(self.temperature.resistance_base)
                + b.ratio_delta.scale(self.temperature.resistance_slope))
            .scale(self.transport.resistance_denominator.recip())
        } else {
            D::constant(self.transport.series_resistance)
        };
        let resistance = rds0
            * one_plus_nine(s.overdrive.scale(p.resistance[0]) + root_delta.scale(p.resistance[1]));
        let bulk = self.bulk(s);
        Geometry {
            width,
            resistance,
            bulk,
        }
    }

    fn bulk(&self, s: &State) -> D {
        let [a0, ags, keta, ketas, b0, b1] = self.channel.bulk;
        if a0 == 0.0 {
            return D::constant(1.0);
        }
        let factor = s.body_high / one_plus_nine(s.body_high.scale(keta));
        let x = factor.scale((self.electrostatics.phi + ketas).recip());
        let limited = if x.value < 0.96 {
            (D::constant(1.0) - x).sqrt().reciprocal()
        } else {
            (D::constant(6.00167) - x.scale(6.26044)) / (D::constant(1.0) - x.scale(1.043_406))
        };
        let k = limited.scale(
            0.5 * self.electrostatics.effective_k1 / (self.electrostatics.phi + ketas).sqrt(),
        );
        let t = D::constant(self.length)
            / (D::constant(self.length)
                + (s.depth.scale(self.channel.junction_depth))
                    .sqrt()
                    .scale(2.0));
        let base = D::constant(1.0) + k * (t.scale(a0) + D::constant(b0 / (self.width + b1)));
        // Native uses the stored ambient phi for the keta/ketas factor.
        effective_bulk(base - k * (t * t * t).scale(ags * a0) * s.overdrive)
    }

    fn mobility(&self, b: &Bias, s: &State) -> Mobility {
        let p = self.channel;
        let (coeff, u0, velocity) = if self.temperature.selfheat {
            let coeff = std::array::from_fn(|i| {
                D::constant(self.transport.nominal_mobility_coefficients[i])
                    + b.ratio_delta.scale(self.temperature.mobility_slopes[i])
            });
            let u0 = (D::constant(1.0) + b.ratio_delta)
                .powf(self.temperature.mobility_exponent)
                .scale(self.transport.nominal_mobility);
            let velocity =
                D::constant(p.velocity) - b.ratio_delta.scale(self.temperature.velocity_slope);
            (coeff, u0, velocity)
        } else {
            (
                self.transport.mobility_coefficients.map(D::constant),
                D::constant(self.transport.mobility),
                D::constant(self.transport.saturation_velocity),
            )
        };
        let [ua, ub, uc] = coeff;
        let field = (s.overdrive + s.threshold.scale(2.0)).scale(self.tox.recip());
        let correction = match p.mobility_mode {
            1 => field * (ua + uc * s.body + ub * field),
            2 => {
                let f = s.overdrive.scale(self.tox.recip());
                f * (ua + uc * s.body + ub * f)
            }
            _ => field * (ua + ub * field) * (D::constant(1.0) + uc * s.body),
        };
        Mobility {
            mobility: u0 / one_plus_eight(correction),
            velocity,
        }
    }

    fn lambda(&self, drive: D) -> D {
        let [a1, a2] = self.channel.saturation;
        if a1 == 0.0 {
            D::constant(a2)
        } else if a1 > 0.0 {
            let t = D::constant(1.0 - a2 - 0.0001) - drive.scale(a1);
            D::constant(1.0) - (t + (t * t + D::constant(0.0004 * (1.0 - a2))).sqrt()).scale(0.5)
        } else {
            let t = D::constant(a2 - 0.0001) + drive.scale(a1);
            (t + (t * t + D::constant(0.0004 * a2)).sqrt()).scale(0.5)
        }
    }

    fn saturation(
        &self,
        bias: &Bias,
        state: &State,
        geometry: Geometry,
        mobility: Mobility,
    ) -> Saturation {
        let drive = state.overdrive + bias.thermal.scale(2.0);
        let field_length = mobility.velocity.scale(2.0 * self.length) / mobility.mobility;
        let wvcox_rds = geometry.width * mobility.velocity * geometry.resistance.scale(self.oxide);
        let lambda = self.lambda(state.overdrive);
        let voltage = if geometry.resistance.value == 0.0 && (lambda.value - 1.0).abs() == 0.0 {
            field_length * drive / (geometry.bulk * field_length + drive)
        } else {
            let bulk_resistance = geometry.bulk * wvcox_rds;
            let quadratic = geometry.bulk.scale(2.0)
                * (bulk_resistance - D::constant(1.0) + lambda.reciprocal());
            let linear = drive * (lambda.reciprocal().scale(2.0) - D::constant(1.0))
                + geometry.bulk * field_length
                + (drive * bulk_resistance).scale(3.0);
            let constant = drive * (field_length + (drive * wvcox_rds).scale(2.0));
            // Rationalized quadratic root also covers the removable a=0 limit.
            constant.scale(2.0)
                / (linear + (linear * linear - (quadratic * constant).scale(2.0)).sqrt())
        };
        let delta = self.channel.delta;
        let difference = voltage - bias.drain - D::constant(delta);
        let raw = voltage
            - (difference + (difference * difference + voltage.scale(4.0 * delta)).sqrt())
                .scale(0.5);
        let effective = if raw.value > bias.drain.value {
            D {
                value: bias.drain.value,
                ..raw
            }
        } else {
            raw
        };
        Saturation {
            voltage,
            effective,
            excess: bias.drain - effective,
            field_length,
            wvcox_rds,
            lambda,
            drive,
        }
    }

    fn early_voltage(&self, s: &State, g: Geometry, v: &Saturation) -> D {
        let [pclm, pdiblcb, pvag] = self.channel.output;
        let t = D::constant(1.0) - (g.bulk * v.voltage / v.drive).scale(0.5);
        let vasat = (v.field_length + v.voltage + (v.wvcox_rds * s.overdrive * t).scale(2.0))
            / (v.lambda.reciprocal().scale(2.0) - D::constant(1.0) + v.wvcox_rds * g.bulk);
        let large = D::constant(2.688_117_142e43);
        let vaclm = if pclm > 0.0 && v.excess.value > 1e-10 {
            (g.bulk + s.overdrive / v.field_length)
                * v.excess
                    .scale(self.length / (pclm * self.electrostatics.lateral_length))
                / g.bulk
        } else {
            large
        };
        let theta = self.electrostatics.output_resistance_factor;
        let vadibl = if theta > 0.0 {
            let t = g.bulk * v.voltage;
            (v.drive - v.drive * t / (v.drive + t)).scale(theta.recip())
                / one_plus_nine(s.body.scale(pdiblcb))
        } else {
            large
        };
        vasat
            + one_plus_nine((s.overdrive / v.field_length).scale(pvag)) * vaclm * vadibl
                / (vaclm + vadibl)
    }

    pub(super) fn channel_current(
        &self,
        b: &Bias,
        s: &State,
    ) -> Result<PdChannelEvaluation, ModelError> {
        let geometry = self.geometry(b, s);
        let mobility = self.mobility(b, s);
        let sat = self.saturation(b, s, geometry, mobility);
        let va = self.early_voltage(s, geometry, &sat);
        let beta = mobility.mobility * geometry.width.scale(self.oxide / self.length);
        let gche = beta
            * s.overdrive
            * (D::constant(1.0) - (geometry.bulk * sat.effective / sat.drive).scale(0.5))
            / (D::constant(1.0) + sat.effective / sat.field_length);
        let idl = gche * sat.effective / (D::constant(1.0) + gche * geometry.resistance);
        let ids = (idl * (D::constant(1.0) + sat.excess / va)).scale(self.segments.recip());
        if ![
            ids,
            s.threshold,
            s.overdrive,
            s.body,
            mobility.mobility,
            sat.voltage,
            sat.effective,
            geometry.resistance,
            geometry.bulk,
        ]
        .into_iter()
        .all(D::finite)
        {
            return Err(ModelError::InvalidPhysicalParameter("PD channel transport"));
        }
        Ok(PdChannelEvaluation {
            current_a: ids.value,
            current_gradient: ids.gradient,
            threshold_v: s.threshold.value,
            overdrive_v: s.overdrive.value,
            body_effective_v: s.body.value,
            mobility: mobility.mobility.value,
            saturation_voltage_v: sat.voltage.value,
            effective_drain_voltage_v: sat.effective.value,
            series_resistance_ohm: geometry.resistance.value,
            bulk_factor: geometry.bulk.value,
        })
    }
}
