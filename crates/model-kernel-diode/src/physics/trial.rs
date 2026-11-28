//! Numerical policy for the exponential intrinsic junction, separate from
//! its constitutive equations. Logarithmic compression bounds the change of
//! an exponential's argument, not its current or its physical derivative.
//! The solver still evaluates and stamps the original physical equations.

use super::{Avalanche, ExponentialJunction, PreparedInstance};
use crate::{ModelError, finite};
use model_kernel_api::trial::limiting::ExponentialLimit;

#[derive(Clone, Copy, Debug)]
pub(super) struct TrialLimits {
    forward: [Option<ExponentialLimit>; 2],
    reverse: Option<ReverseLimits>,
}

#[derive(Clone, Copy, Debug)]
struct ReverseLimits {
    knee: f64,
    terms: [Option<ExponentialLimit>; 2],
}

fn restrict(terms: [Option<ExponentialLimit>; 2], proposed: f64, previous: f64) -> f64 {
    terms.into_iter().flatten().fold(proposed, |limited, term| {
        let candidate = term.limit(proposed, previous);
        if proposed >= previous {
            limited.min(candidate)
        } else {
            limited.max(candidate)
        }
    })
}

impl TrialLimits {
    pub(super) fn new(
        transport: ExponentialJunction,
        recombination: ExponentialJunction,
        avalanche: Option<[Avalanche; 2]>,
    ) -> Self {
        Self {
            forward: [transport, recombination]
                .map(|term| ExponentialLimit::new(term.log_saturation, term.inverse_vt)),
            reverse: avalanche.map(|terms| ReverseLimits {
                knee: terms[0].voltage,
                terms: terms.map(|term| ExponentialLimit::new(term.log_current, term.inverse_vt)),
            }),
        }
    }

    fn initial(self) -> f64 {
        let critical = self
            .forward
            .into_iter()
            .flatten()
            .map(ExponentialLimit::critical)
            .fold(f64::INFINITY, f64::min);
        if critical.is_finite() { critical } else { 0. }
    }

    fn limit(self, proposed: f64, reference: f64) -> f64 {
        if let Some(reverse) = self.reverse
            && proposed < -reverse.knee
        {
            // Reflection about the physical avalanche knee handles the
            // steep reverse current with the same dimensionless compression.
            let reflected = -proposed - reverse.knee;
            let previous = (-reference - reverse.knee).max(0.);
            let limited = restrict(reverse.terms, reflected, previous);
            if limited.to_bits() != reflected.to_bits() {
                return -reverse.knee - limited;
            }
            return proposed;
        }
        restrict(self.forward, proposed, reference)
    }
}

impl PreparedInstance {
    pub(crate) fn initial_junction_voltage(&self) -> f64 {
        self.trial_limits.initial()
    }

    pub(crate) fn limit_junction_voltage(
        &self,
        proposed: f64,
        previous: f64,
    ) -> Result<f64, ModelError> {
        finite("junction trial voltage", &[proposed, previous])?;
        let limited = self.trial_limits.limit(proposed, previous);
        finite("limited junction voltage", &[limited])?;
        Ok(limited)
    }
}
