//! Family-owned trial policy. The returned local coordinates are one coherent
//! linearization point; no constitutive value or derivative is clipped.
//! Startup and FET/PN limiting regions follow the Berkeley BSIM1/2 load paths.

use model_compact_intrinsic::{
    BSIM1_REFERENCE_THERMAL_VOLTAGE, MosPolarity, PreparedLegacyBsimInstance, legacy_bsim_threshold,
};
use model_kernel_api::{
    Error, Result,
    trial::{
        Convergence, ConvergenceInput,
        limiting::{ExponentialLimit, fetlim, limvds, restrict_step},
    },
};

use super::{Layout, domain};

const fn sign(instance: &PreparedLegacyBsimInstance) -> f64 {
    match instance.polarity {
        MosPolarity::Nmos => 1.,
        MosPolarity::Pmos => -1.,
    }
}

fn difference(coordinates: &[f64], high: usize, low: usize, polarity: f64) -> Result<f64> {
    let voltage = polarity * (coordinates[high] - coordinates[low]);
    if voltage.is_finite() {
        Ok(voltage)
    } else {
        Err(Error::Domain("nonfinite legacy BSIM trial voltage".into()))
    }
}

impl Layout {
    pub(super) fn initialize(self, instance: &PreparedLegacyBsimInstance, coordinates: &mut [f64]) {
        // C MODEINITJCT: VDS=0, VGS=von, VBS=-1. Source-supplied OFF and
        // initial conditions are already handled by the consuming engine.
        let polarity = sign(instance);
        let source = coordinates[self.source];
        coordinates[self.drain] = source;
        coordinates[1] = polarity.mul_add(instance.threshold, source);
        coordinates[3] = source - polarity;
    }

    pub(super) fn limit(
        self,
        instance: &PreparedLegacyBsimInstance,
        reference: &[f64],
        coordinates: &mut [f64],
    ) -> Result<()> {
        // Choose the controlling gate/drain pair from the previously
        // evaluated orientation; its threshold includes the body/drain bias.
        let orientation = self.orientation(instance, reference);
        let [high, gate, low, bulk] = orientation.rows;
        let polarity = orientation.sign;
        let threshold =
            legacy_bsim_threshold(instance, orientation.bias).map_err(|error| domain(&error))?;
        let old_gate = orientation.bias.vgs;
        let old_drain = orientation.bias.vds;
        let proposed_gate = difference(coordinates, gate, low, polarity)?;
        let proposed_drain = difference(coordinates, high, low, polarity)?;
        let mut scale = restrict_step(
            1.,
            proposed_gate,
            old_gate,
            fetlim(proposed_gate, old_gate, threshold),
        );
        scale = restrict_step(
            scale,
            proposed_drain,
            old_drain,
            limvds(proposed_drain, old_drain),
        );
        // Both actual body junction differences are constrained, including
        // a step crossing the source/drain orientation boundary.
        for (row, junction) in [
            (self.drain, &instance.drain),
            (self.source, &instance.source),
        ] {
            let previous = difference(reference, bulk, row, polarity)?;
            let raw = difference(coordinates, bulk, row, polarity)?;
            if let Some(limiter) = ExponentialLimit::new(
                junction.saturation_current.ln(),
                BSIM1_REFERENCE_THERMAL_VOLTAGE.recip(),
            ) {
                scale = restrict_step(scale, raw, previous, limiter.limit(raw, previous));
            }
        }
        if scale < 1. {
            // Contract intrinsic voltage differences around the proposed
            // intrinsic source. External series-node potentials remain
            // physical coordinates, with their linear law evaluated exactly.
            let anchor = coordinates[self.source];
            for row in [self.drain, gate, bulk] {
                let previous = difference(reference, row, self.source, 1.)?;
                let raw = difference(coordinates, row, self.source, 1.)?;
                let delta = raw - previous;
                let voltage = if delta.is_finite() {
                    scale.mul_add(delta, previous)
                } else {
                    scale.mul_add(raw, (1. - scale) * previous)
                };
                coordinates[row] = anchor + voltage;
            }
        }
        Ok(())
    }

    pub(super) fn convergence(self, input: ConvergenceInput<'_>) -> Result<Convergence> {
        // All independent nonlinear current rows must be consistent with
        // the previous exact Jacobian; series contributions are linear.
        let width = self.width;
        for row in [self.drain, self.source, 3] {
            let mut prediction = input.previous.residual[row];
            for (column, derivative) in input.previous.jacobian[row * width..(row + 1) * width]
                .iter()
                .copied()
                .enumerate()
            {
                let previous =
                    input.previous.coordinates[column] - input.previous.coordinates[self.source];
                let candidate =
                    input.candidate.coordinates[column] - input.candidate.coordinates[self.source];
                prediction = derivative.mul_add(candidate - previous, prediction);
            }
            if input.compare_residual(row, prediction, input.candidate.residual[row])?
                == Convergence::Retry
            {
                return Ok(Convergence::Retry);
            }
        }
        Ok(Convergence::Satisfied)
    }
}
