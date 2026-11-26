//! PD-specific bulk factor and transport assembly.
//! Copyright 1999/1990 Regents of the University of California. See package LICENSE.

use super::super::channel_transport::{TransportInput, effective_bulk, one_plus_nine};
use super::{Bias, D, PdChannelEvaluation, PreparedPdChannel, State};
use crate::ModelError;

impl PreparedPdChannel {
    pub(super) fn bulk_at(&self, s: &State, overdrive: D) -> D {
        let p = self.bulk_params;
        if p.a0 == 0.0 {
            return D::constant(1.0);
        }
        let factor = s.body_high / one_plus_nine(s.body_high.scale(p.keta));
        let x = factor.scale((self.electrostatics.phi + self.ketas).recip());
        let limited = if x.value < 0.96 {
            (D::constant(1.0) - x).sqrt().reciprocal()
        } else {
            (D::constant(6.00167) - x.scale(6.26044)) / (D::constant(1.0) - x.scale(1.043_406))
        };
        let k = limited.scale(
            0.5 * self.electrostatics.effective_k1 / (self.electrostatics.phi + self.ketas).sqrt(),
        );
        effective_bulk(D::constant(1.0) + k * p.base(&self.threshold_geometry, s.depth, overdrive))
    }

    pub(super) fn channel_current(
        &self,
        b: &Bias,
        s: &State,
    ) -> Result<PdChannelEvaluation, ModelError> {
        let bulk = self.bulk_at(s, s.overdrive);
        let point = self.channel.evaluate(
            &self.threshold_geometry,
            &TransportInput {
                drain: b.drain,
                thermal: b.thermal,
                ratio_delta: b.ratio_delta,
                root_delta: s.root - D::constant(b.phi.sqrt()),
                body: s.body,
                overdrive: s.overdrive,
                threshold: s.threshold,
                bulk,
                width_factor: 2.0 - self.body_contacts,
            },
        );
        let ids = point.current.scale(self.segments.recip());
        if ![
            ids,
            s.threshold,
            s.overdrive,
            s.body,
            point.mobility,
            point.saturation,
            point.effective_drain,
            point.resistance,
            bulk,
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
            mobility: point.mobility.value,
            saturation_voltage_v: point.saturation.value,
            effective_drain_voltage_v: point.effective_drain.value,
            series_resistance_ohm: point.resistance.value,
            bulk_factor: bulk.value,
            leakage: self.leakage(b, s, point.width)?,
        })
    }
}
