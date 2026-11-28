//! Source-defined shared FD preliminary and final threshold laws.
//! Copyright 2010 Regents of the University of California. See package LICENSE.
use super::{Jet, Trial};
use crate::ModelError;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Threshold {
    pub(super) vth0: f64,
    pub(super) k1: f64,
    pub(super) k1ox: f64,
    pub(super) k2ox: f64,
    pub(super) eta0: f64,
    pub(super) etab: f64,
    pub(super) cox: f64,
    pub(super) epssub: f64,
    pub(super) factor1: f64,
    pub(super) length: f64,
    pub(super) width: f64,
    pub(super) toxe: f64,
    pub(super) w0: f64,
    pub(super) dvt: [f64; 3],
    pub(super) dvtw: [f64; 3],
    pub(super) cdsc: [f64; 3],
    pub(super) nfactor: f64,
    pub(super) cit: f64,
    pub(super) dvtp: [f64; 4],
    pub(super) lpe: [f64; 2],
    pub(super) kt: [f64; 2],
    pub(super) k3: [f64; 2],
}

fn smooth_one(x: Jet) -> Jet {
    if x.value >= -0.5 {
        1. + x
    } else {
        (1. + 3. * x) / (3. + 8. * x)
    }
}

impl Threshold {
    fn short_channel(&self, body: Jet, depletion: Jet, coefficients: [f64; 3], length: f64) -> Jet {
        let scale = self.factor1 * depletion.sqrt() * smooth_one(coefficients[2] * body);
        let exponential = (-0.5 * coefficients[1] * length / scale).floor_exp();
        exponential * (1. + 2. * exponential)
    }

    pub(super) fn evaluate(
        &self,
        trial: &Trial,
        body: Jet,
        root: Jet,
        root_extended: Jet,
    ) -> Result<(Jet, Jet, Jet), ModelError> {
        let depletion = trial.depletion * root / trial.phi.sqrt();
        let theta = self.short_channel(body, depletion, self.dvt, self.length);
        let theta_width = self.short_channel(body, depletion, self.dvtw, self.length * self.width);
        let [coupling, body_coupling, drain_coupling] = self.cdsc;
        let n = smooth_one(
            (self.nfactor * self.epssub / depletion
                + (coupling + body_coupling * body + drain_coupling * trial.drain) * theta
                + self.cit)
                / self.cox,
        );
        let dits = if self.dvtp[0] > 0. {
            let length =
                self.length + self.dvtp[0] * (1. + (-self.dvtp[1] * trial.drain).floor_exp());
            n * trial.vtm * (self.length / length).ln()
        } else {
            Jet::constant(0.)
        };
        let temp = self.k1ox * self.lpe[0] * trial.phi.sqrt()
            + (self.kt[0] + self.kt[1] * body) * trial.temp_ratio;
        let narrow =
            (self.k3[0] + self.k3[1] * body) * (self.toxe / (self.width + self.w0)) * trial.phi;
        let eta = self.eta0 + self.etab * body;
        let eta = if eta.value < 1e-4 {
            (2e-4 - eta) / (3. - 2e4 * eta)
        } else {
            eta
        };
        let dibl = eta * trial.theta * trial.drain;
        let shift = (self.dvt[0] * theta + self.dvtw[0] * theta_width) * (trial.vbi - trial.phi);
        let value = self.vth0
            + (self.k1ox * root_extended - self.k1 * trial.phi.sqrt()) * self.lpe[1]
            - self.k2ox * body
            - shift
            + narrow
            + temp
            - dibl
            - dits
            - self.dvtp[2] * (self.dvtp[3] * trial.drain).tanh();
        Ok((
            value.checked("channel threshold")?,
            n.checked("subthreshold slope")?,
            depletion.checked("channel depletion")?,
        ))
    }
}
