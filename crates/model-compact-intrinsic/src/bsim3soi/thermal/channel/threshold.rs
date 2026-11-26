//! PD body-potential smoothing, poly depletion and threshold/overdrive.
//! Copyright 1999/1990 Regents of the University of California. See package LICENSE.

use super::super::{CHARGE, Context, EPS_SI};
use super::{Bias, D, PreparedPdChannel, State};
use crate::{ModelError, preparation::positive};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Threshold {
    poly_doping: f64,
    dvt: [f64; 3],
    dvtw: [f64; 3],
    temperature: [f64; 3],
    lateral: f64,
    narrow: [f64; 3],
    dibl: [f64; 2],
    subthreshold: [f64; 5],
}

impl Threshold {
    pub(super) fn prepare(c: &Context<'_>) -> Result<Self, ModelError> {
        Ok(Self {
            poly_doping: c.binned("ngate")?,
            dvt: [c.binned("dvt0")?, c.binned("dvt1")?, c.binned("dvt2")?],
            dvtw: [c.binned("dvt0w")?, c.binned("dvt1w")?, c.binned("dvt2w")?],
            temperature: [c.binned("kt1")?, c.binned("kt1l")?, c.binned("kt2")?],
            lateral: c.binned("nlx")?,
            narrow: [c.binned("k3")?, c.binned("k3b")?, c.binned("w0")?],
            dibl: [c.binned("eta0")?, c.binned("etab")?],
            subthreshold: [
                c.binned("nfactor")?,
                c.binned("cdsc")?,
                c.binned("cdscb")?,
                c.binned("cdscd")?,
                c.binned("cit")?,
            ],
        })
    }
}

pub(super) fn one_plus(x: D) -> D {
    if x.value >= -0.5 {
        D::constant(1.0) + x
    } else {
        (D::constant(1.0) + x.scale(3.0)) / (D::constant(3.0) + x.scale(8.0))
    }
}

fn lower_bound_body(v: D) -> D {
    let t = v + D::constant(4.999);
    D::constant(-5.0) + (t + (t * t + D::constant(0.02)).sqrt()).scale(0.5)
}

fn upper_bound_body(v: D, limit: f64) -> D {
    let t = D::constant(limit - 0.002) - v;
    D::constant(limit) - (t + (t * t + D::constant(0.008 * limit)).sqrt()).scale(0.5)
}

fn theta(x: D) -> D {
    let e = if x.value > -100.0 {
        x.exp()
    } else {
        D::constant(3.720_075_976e-44)
    };
    e * (D::constant(1.0) + e.scale(2.0))
}

fn poly_depletion(gate: D, onset: f64, doping: f64, oxide: f64) -> D {
    if doping <= 1e18 || doping >= 1e25 || gate.value <= onset {
        return gate;
    }
    let coefficient = 1e6 * CHARGE * EPS_SI * doping / oxide / oxide;
    let voltage = gate - D::constant(onset);
    let root = (D::constant(1.0) + voltage.scale(2.0 / coefficient)).sqrt();
    let t = voltage.scale(2.0) / (root + D::constant(1.0));
    let poly = (t * t).scale(0.5 / coefficient);
    let remainder = D::constant(1.07) - poly;
    gate - D::constant(1.12)
        + (remainder + (remainder * remainder + D::constant(0.224)).sqrt()).scale(0.5)
}

fn overdrive(vgst: D, n: D, thermal: D, voff: f64, cdep: f64, cox: f64) -> D {
    let scale = (n * thermal).scale(2.0);
    let x = vgst / scale;
    let y = (D::constant(2.0 * voff) - vgst) / scale;
    if x.value > 100.0 {
        vgst
    } else if y.value > 100.0 {
        thermal.scale(cdep / cox) * ((vgst - D::constant(voff)) / (n * thermal)).exp()
    } else {
        scale * x.exp().ln_1p() / (D::constant(1.0) + n.scale(2.0 * cox / cdep) * y.exp())
    }
}

impl PreparedPdChannel {
    pub(super) fn state(&self, b: &Bias) -> Result<State, ModelError> {
        let t = self.threshold;
        let phi = positive(b.phi, "surface potential")?;
        let root_phi = phi.sqrt();
        let body_high = upper_bound_body(lower_bound_body(b.body), 1.5);
        let body = upper_bound_body(body_high, 0.95 * phi);
        let root = (D::constant(phi) - body).sqrt();
        let depth =
            root.scale((2.0 * EPS_SI / (CHARGE * self.electrostatics.channel_doping * 1e6)).sqrt());
        let gate = poly_depletion(
            b.gate,
            self.electrostatics.flatband + phi,
            t.poly_doping,
            self.oxide,
        );
        let scale = depth
            .sqrt()
            .scale((EPS_SI * self.tox / self.oxide_permittivity()).sqrt());
        let short = theta(
            D::constant(-0.5 * t.dvt[1] * self.length) / (scale * one_plus(body.scale(t.dvt[2]))),
        );
        let narrow = theta(
            D::constant(-0.5 * t.dvtw[1] * self.width * self.length)
                / (scale * one_plus(body.scale(t.dvtw[2]))),
        );
        let v0 = b.built_in - D::constant(phi);
        let k1 = self.electrostatics.effective_k1;
        let temp_shift =
            D::constant(k1 * ((1.0 + t.lateral / self.length).sqrt() - 1.0) * root_phi)
                + (D::constant(t.temperature[0] + t.temperature[1] / self.length)
                    + body.scale(t.temperature[2]))
                    * b.ratio_delta;
        let narrow_shift = (D::constant(t.narrow[0]) + body.scale(t.narrow[1]))
            .scale(self.tox * phi / (self.width + t.narrow[2]));
        let eta = D::constant(t.dibl[0]) + body.scale(t.dibl[1]);
        let eta = if eta.value < 1e-4 {
            (D::constant(2e-4) - eta) / (D::constant(3.0) - eta.scale(2e4))
        } else {
            eta
        };
        let dibl = eta * b.drain.scale(self.electrostatics.short_channel_factor);
        let extended = root - (body_high - body).scale(2.2361 / root_phi);
        let threshold = D::constant(self.sign * self.electrostatics.threshold)
            + (extended - D::constant(root_phi)).scale(k1)
            - body.scale(self.electrostatics.k2)
            - short.scale(t.dvt[0]) * v0
            - narrow.scale(t.dvtw[0]) * v0
            + narrow_shift
            + temp_shift
            - dibl;
        let n = one_plus(
            (D::constant(t.subthreshold[0] * EPS_SI) / depth
                + (D::constant(t.subthreshold[1])
                    + body.scale(t.subthreshold[2])
                    + b.drain.scale(t.subthreshold[3]))
                    * short
                + D::constant(t.subthreshold[4]))
            .scale(self.oxide.recip()),
        );
        let overdrive = overdrive(
            gate - threshold,
            n,
            b.thermal,
            self.voff,
            self.electrostatics.depletion_capacitance,
            self.oxide,
        );
        Ok(State {
            body_high,
            body,
            root,
            depth,
            threshold,
            overdrive,
        })
    }

    fn oxide_permittivity(&self) -> f64 {
        self.oxide * self.tox
    }
}
