//! Fully depleted body electrostatics, including both FD formulations.
//! Copyright 2010 Regents of the University of California. See package LICENSE.
use super::{Jet, Trial, threshold::Threshold};
use crate::ModelError;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct FullyDepleted {
    pub(super) ideal: bool,
    pub(super) alternate: bool,
    pub(super) csi: f64,
    pub(super) cbox: f64,
    pub(super) qsi: f64,
    pub(super) cdsbs: f64,
    pub(super) vbsa: f64,
    pub(super) vsce: f64,
    pub(super) lowering: f64,
    pub(super) backgate_coupling: f64,
    pub(super) noff: f64,
    pub(super) voff: f64,
    pub(super) moin: f64,
}

impl FullyDepleted {
    fn potential(&self, surface: Jet, trial: &Trial) -> (Jet, Jet) {
        let base = surface - (0.5 * self.qsi / self.csi) + self.vbsa;
        if self.alternate {
            let denominator = self.csi + self.cbox + self.cdsbs;
            let drain_shift = trial.drain + self.vsce;
            let front = (self.csi * base + self.cdsbs * self.lowering * drain_shift) / denominator;
            (front, front + (self.cbox / denominator) * trial.backgate)
        } else {
            let front = base + self.lowering * (trial.vbi - trial.phi);
            (
                front,
                front / (1. + self.cbox / self.csi) + self.backgate_coupling * trial.backgate,
            )
        }
    }
    fn mos(&self, front: Jet, interface: Jet) -> Jet {
        let shift = (front - interface - 0.005).half_positive(Jet::constant(2.5e-5));
        interface - (0.5 * self.csi / self.qsi) * shift * shift
    }

    pub(super) fn evaluate(
        &self,
        trial: &Trial,
        threshold: &Threshold,
        gate: Jet,
        body: Jet,
    ) -> Result<(Jet, Jet), ModelError> {
        let (front, interface) = self.potential(trial.phi, trial);
        let mos = self.mos(front, interface);
        let upper = trial.phi - 0.02;
        let mos = upper - (upper - mos - 0.005).half_positive(Jet::constant(0.02));
        let root = (trial.phi - mos).sqrt();
        let (preliminary, _, _) = threshold.evaluate(trial, mos, root, root)?;
        let scale = self.noff * trial.vtm;
        let below = scale
            * ((preliminary - gate - self.voff) / scale)
                .limited_exp()
                .ln_1p();
        let above = scale
            * ((gate - preliminary - self.voff) / scale)
                .limited_exp()
                .ln_1p();
        let inversion = trial.phi
            + trial.vtm
                * (above * (above + 2. * threshold.k1 * trial.phi.sqrt())
                    / (self.moin * threshold.k1ox * trial.vtm * trial.vtm))
                    .ln_1p();
        let coupling = threshold.cox / (threshold.cox + 1. / (1. / self.csi + 1. / self.cbox));
        let surface = inversion - coupling * below;
        let (front, interface) = self.potential(surface, trial);
        let interface = interface + 0.02;
        let interface = if self.ideal {
            interface
        } else {
            interface + (body - interface - 0.01).half_positive(Jet::constant(0.0001))
        };
        Ok((
            self.mos(front, interface),
            if self.ideal { interface } else { body },
        ))
    }
}
