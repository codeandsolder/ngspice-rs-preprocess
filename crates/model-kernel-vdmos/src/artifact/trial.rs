//! Family numerical policy for nonlinear channel, body diode and self-heating.
//! Work only on coherent local coordinate points, never independent branch
//! voltages that might alias the same physical node. The exact kernel and its
//! derivatives remain unchanged; the engine owns history and acceptance.

use model_kernel_api::{
    Error, Result,
    trial::limiting::{ExponentialLimit, fetlim, limit_temperature, limvds, restrict_step},
};

use super::{VdmosInstance, VdmosKernel};
use crate::{DiodeActivation, Jet, breakdown, junction_potential};

// ngspice 47 VDMOSload uses DEVlimitlog with a 10 K step threshold.
const THERMAL_STEP: f64 = 10.;

fn fet_step(vgs: f64, vds: f64, previous_gate: f64, previous_drain: f64, vto: f64) -> f64 {
    // Select the conducting end from the previous point, then restrict each
    // independent controlling voltage along the same proposed direction.
    // Reconstructing C's independently limited VGS/VDS pair could demand
    // a drain step opposite to the proposal when the gate is also limited.
    let (gate, old_gate, drain, old_drain) = if previous_drain >= 0. {
        (vgs, previous_gate, vds, previous_drain)
    } else {
        (
            vgs - vds,
            previous_gate - previous_drain,
            -vds,
            -previous_drain,
        )
    };
    let scale = restrict_step(1., gate, old_gate, fetlim(gate, old_gate, vto));
    restrict_step(scale, drain, old_drain, limvds(drain, old_drain))
}

/// Return a representable admissible point on the proposed scalar step.
/// The already evaluated reference is known admissible. Terminate even when
/// rounding a midpoint would otherwise repeat the invalid endpoint.
fn backtrack(reference: f64, mut next: f64, valid: impl Fn(f64) -> bool) -> f64 {
    while !valid(next) {
        let middle = f64::midpoint(reference, next);
        if middle.to_bits() == next.to_bits() {
            return reference;
        }
        next = middle;
    }
    next
}

impl VdmosKernel {
    fn limit_thermal(&self, reference: &[f64], coordinates: &mut [f64]) -> Result<()> {
        let Some([junction, case]) = self.layout.thermal else {
            return Ok(());
        };
        let valid_junction = |temperature: f64| {
            if temperature <= 0. || !temperature.is_finite() {
                return false;
            }
            let potential = junction_potential(&self.model, Jet::<f64>::constant(temperature)).v;
            potential > 0. && potential.is_finite()
        };
        let previous = reference[junction];
        if !valid_junction(previous) || reference[case] <= 0. {
            return Err(Error::Domain(
                "invalid VDMOS thermal trial reference".into(),
            ));
        }
        let next = limit_temperature(coordinates[junction], previous, THERMAL_STEP);
        // If a valid evaluated reference lies near a constitutive boundary,
        // even a compressed step may cross it. Bisect back toward that actual
        // reference, not a hard-coded operating-temperature clamp.
        if !next.is_finite() {
            return Err(Error::Domain("nonfinite VDMOS thermal trial step".into()));
        }
        coordinates[junction] = backtrack(previous, next, valid_junction);
        if coordinates[case] <= 0. {
            let next = limit_temperature(coordinates[case], reference[case], THERMAL_STEP);
            if !next.is_finite() {
                return Err(Error::Domain("nonfinite VDMOS case trial step".into()));
            }
            coordinates[case] = backtrack(reference[case], next, |temperature| temperature > 0.);
        }
        Ok(())
    }

    fn limit_diode(
        &self,
        prepared: &VdmosInstance,
        temperature: f64,
        raw: f64,
        old: f64,
    ) -> Result<f64> {
        let model = self.model.parameters();
        let diode = model.diode;
        let terms: DiodeActivation<f64> = DiodeActivation::new(
            Jet::constant(temperature),
            prepared.multiplier,
            diode,
            model.tnom,
        );
        let thermal = terms.thermal_voltage.v;
        let vte = terms.vte.v;
        let log_current = terms.saturation_log().v;
        if raw < -3. * vte
            && let Some(avalanche) = diode.breakdown
        {
            let saturation = log_current.exp();
            let knee: f64 = breakdown::effective_voltage(
                avalanche,
                Jet::constant(saturation),
                Jet::constant(thermal),
            )
            .map_err(|error| Error::Domain(error.to_string()))?
            .v;
            if raw < -knee
                && let Some(limiter) =
                    ExponentialLimit::new(log_current, (avalanche.nbv * thermal).recip())
            {
                let proposed = -raw - knee;
                let limited = limiter.limit(proposed, (-old - knee).max(0.));
                return Ok(if proposed.to_bits() == limited.to_bits() {
                    raw
                } else {
                    -knee - limited
                });
            }
        }
        Ok(ExponentialLimit::new(log_current, vte.recip())
            .map_or(raw, |limiter| limiter.limit(raw, old)))
    }

    pub(super) fn limit_coordinates(
        &self,
        prepared: &VdmosInstance,
        reference: &[f64],
        coordinates: &mut [f64],
    ) -> Result<()> {
        self.limit_thermal(reference, coordinates)?;
        let raw = self.operating_point(prepared, coordinates)?.bias;
        let previous = self.operating_point(prepared, reference)?.bias;
        if [
            raw.vgs,
            raw.vds,
            raw.vdiode,
            previous.vgs,
            previous.vds,
            previous.vdiode,
        ]
        .iter()
        .any(|value| !value.is_finite())
        {
            return Err(Error::Domain("nonfinite VDMOS controlling voltage".into()));
        }
        let model = self.model.parameters();
        let sign = model.polarity.sign();
        let channel = model.channel;
        let threshold = sign
            * channel
                .tcvth
                .mul_add(raw.temperature - model.tnom, channel.vth);
        let mut scale = fet_step(
            sign * raw.vgs,
            sign * raw.vds,
            sign * previous.vgs,
            sign * previous.vds,
            threshold,
        );
        let diode = self.limit_diode(
            prepared,
            raw.temperature,
            sign * raw.vdiode,
            sign * previous.vdiode,
        )?;
        scale = restrict_step(scale, sign * raw.vdiode, sign * previous.vdiode, diode);
        if scale < 1. {
            // Intrinsic source is the common-mode anchor. Preserve its raw
            // value, and contract all electrical voltage differences with
            // the same factor. No alias can acquire two incompatible values.
            let source = self.layout.intrinsic[2];
            let anchor = coordinates[source];
            let old_anchor = reference[source];
            let electrical = self
                .layout
                .external
                .into_iter()
                .chain(self.layout.intrinsic)
                .chain([self.layout.diode]);
            let mut visited = [false; super::MAX_ROWS];
            for row in electrical {
                if row != source && !visited[row] {
                    visited[row] = true;
                    let old_difference = reference[row] - old_anchor;
                    let difference = coordinates[row] - anchor;
                    if !old_difference.is_finite() || !difference.is_finite() {
                        return Err(Error::Domain("nonfinite VDMOS local voltage".into()));
                    }
                    let delta = difference - old_difference;
                    let limited = if delta.is_finite() {
                        scale.mul_add(delta, old_difference)
                    } else {
                        scale.mul_add(difference, (1. - scale) * old_difference)
                    };
                    coordinates[row] = anchor + limited;
                }
            }
        }
        Ok(())
    }
}
