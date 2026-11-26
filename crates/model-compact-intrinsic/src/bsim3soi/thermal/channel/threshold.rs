//! PD-specific body-potential smoothing.
use super::super::channel_threshold::{ThresholdBias, overdrive, poly_depletion};
use super::{Bias, D, PreparedPdChannel, State};
use crate::{ModelError, preparation::positive};

fn lower_bound_body(v: D) -> D {
    let t = v + D::constant(4.999);
    D::constant(-5.0) + (t + (t * t + D::constant(0.02)).sqrt()).scale(0.5)
}

fn upper_bound_body(v: D, limit: f64) -> D {
    let t = D::constant(limit - 0.002) - v;
    D::constant(limit) - (t + (t * t + D::constant(0.008 * limit)).sqrt()).scale(0.5)
}

impl PreparedPdChannel {
    pub(super) fn state(&self, b: &Bias) -> Result<State, ModelError> {
        let phi = positive(b.phi, "surface potential")?;
        let body_high = upper_bound_body(lower_bound_body(b.body), 1.5);
        let body = upper_bound_body(body_high, 0.95 * phi);
        let gate = poly_depletion(
            b.gate,
            self.electrostatics.flatband + phi,
            self.threshold.poly_doping(),
            self.oxide,
        );
        let point = self.threshold.evaluate(
            &self.threshold_geometry,
            &ThresholdBias {
                phi,
                body,
                drain: b.drain,
                built_in: b.built_in,
                ratio_delta: b.ratio_delta,
                root_adjustment: (body_high - body).scale(2.2361 / phi.sqrt()),
            },
            true,
        );
        let overdrive = overdrive(
            gate - point.threshold,
            point.n,
            b.thermal,
            self.voff,
            self.electrostatics.depletion_capacitance,
            self.oxide,
            100.0,
        );
        Ok(State {
            gate_effective: gate,
            body_high,
            body,
            root: point.root,
            depth: point.depth,
            threshold: point.threshold,
            overdrive,
        })
    }
}
