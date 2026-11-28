//! Shared numerical trial limiters; no constitutive law or history is changed.

/// Logarithmic compression of a steep exponential current-law trial.
#[derive(Clone, Copy, Debug)]
pub struct ExponentialLimit {
    scale: f64,
    critical: f64,
}

impl ExponentialLimit {
    /// Prepare a positive exponential voltage scale from a log-domain current.
    /// A disabled current or nonpositive/nonfinite scale has no limiter.
    #[must_use = "the returned policy bounds steep exponential trial steps"]
    pub fn new(log_current: f64, inverse_scale: f64) -> Option<Self> {
        // A disabled current law or a constant exponential needs no limiter.
        if !log_current.is_finite() || inverse_scale <= 0. {
            return None;
        }
        let scale = inverse_scale.recip();
        if !scale.is_finite() || scale <= 0. {
            return None;
        }
        // Compute the critical voltage in the log domain, including
        // saturation currents whose linear representations underflow.
        let critical = scale * (std::f64::consts::LN_2.mul_add(-0.5, scale.ln()) - log_current);
        Some(Self {
            scale,
            critical: critical.max(0.),
        })
    }

    /// Critical voltage of the exponential law.
    #[must_use = "the critical voltage identifies the exponential limiting region"]
    pub const fn critical(self) -> f64 {
        self.critical
    }

    /// Compress the proposal relative to its evaluated reference.
    #[must_use = "the returned voltage is the local numerical trial point"]
    pub fn limit(self, proposed: f64, reference: f64) -> f64 {
        if proposed <= self.critical {
            return proposed;
        }
        // In the nonconducting half-plane there is no useful exponential
        // tangent. Start the compressed step at its boundary instead.
        let anchor = reference.max(0.);
        let delta = proposed - anchor;
        if delta.abs() <= 2. * self.scale {
            return proposed;
        }
        let ratio = delta.abs() / self.scale;
        let logarithm = if ratio.is_finite() {
            ratio.ln_1p()
        } else {
            // Equivalent asymptotic expression when the quotient overflows.
            delta.abs().ln() - self.scale.ln()
        };
        let step = (self.scale * logarithm).copysign(delta);
        // Prevent a floating-point rounding excursion outside the original
        // interval, including when the proposed and reference voltages differ
        // by hundreds of orders of magnitude.
        (anchor + step).clamp(anchor.min(proposed), anchor.max(proposed))
    }
}

/// `DEVfetlim`: limits the per-iteration change of a FET gate voltage
/// relative to the threshold `vto` (Gillespie's `vtstlo`).
#[must_use = "the returned gate voltage bounds the iteration step relative to threshold"]
pub fn fetlim(vnew: f64, vold: f64, vto: f64) -> f64 {
    let vtsthi = 2_f64.mul_add((vold - vto).abs(), 2.);
    let vtstlo = (vold - vto).abs() + 1.;
    let vtox = vto + 3.5;
    let delv = vnew - vold;
    if vold >= vto {
        if vold >= vtox {
            if delv <= 0. {
                // Going off.
                if vnew >= vtox {
                    if -delv > vtstlo {
                        return vold - vtstlo;
                    }
                    vnew
                } else {
                    vnew.max(vto + 2.)
                }
            } else if delv >= vtsthi {
                // Staying on.
                vold + vtsthi
            } else {
                vnew
            }
        } else if delv <= 0. {
            // Middle region, decreasing.
            vnew.max(vto - 0.5)
        } else {
            // Middle region, increasing.
            vnew.min(vto + 4.)
        }
    } else if delv <= 0. {
        // Off.
        if -delv > vtsthi { vold - vtsthi } else { vnew }
    } else {
        let vtemp = vto + 0.5;
        if vnew <= vtemp {
            if delv > vtstlo { vold + vtstlo } else { vnew }
        } else {
            vtemp
        }
    }
}

/// `DEVlimvds`: limits the per-iteration change of a FET drain-source voltage.
#[must_use = "the returned drain-source voltage bounds the iteration step"]
pub fn limvds(vnew: f64, vold: f64) -> f64 {
    if vold >= 3.5 {
        if vnew > vold {
            vnew.min(3_f64.mul_add(vold, 2.))
        } else if vnew < 3.5 {
            vnew.max(2.)
        } else {
            vnew
        }
    } else if vnew > vold {
        vnew.min(4.)
    } else {
        vnew.max(-0.5)
    }
}

/// `DEVlimitlog`: compress a large thermal Newton step (a temperature
/// difference has identical numerical magnitude in Celsius and kelvin).
///
/// This does not clip the final physical temperature. The caller supplies
/// finite coordinates and a positive finite step threshold.
#[must_use = "the returned temperature is the local numerical trial point"]
pub fn limit_temperature(proposed: f64, reference: f64, threshold: f64) -> f64 {
    let delta = proposed - reference;
    if delta.abs() <= threshold {
        return proposed;
    }
    let ratio = delta.abs() / threshold;
    let log = if ratio.is_finite() {
        ratio.log10()
    } else {
        let log_delta = if delta.is_finite() {
            delta.abs().log10()
        } else {
            (proposed.mul_add(0.5, -reference * 0.5)).abs().log10() + std::f64::consts::LOG10_2
        };
        log_delta - threshold.log10()
    };
    let next = reference + (threshold + log).copysign(delta);
    next.clamp(reference.min(proposed), reference.max(proposed))
}

/// Restrict the common electrical step to the interval permitted by one
/// controlling voltage. This preserves all aliased rows, including the case
/// where different branches refer to the same voltage difference.
#[must_use = "the returned fraction restricts a common coordinate step"]
pub fn restrict_step(scale: f64, raw: f64, previous: f64, limited: f64) -> f64 {
    if raw.to_bits() == limited.to_bits() || raw.to_bits() == previous.to_bits() {
        return scale;
    }
    let delta = raw - previous;
    let allowed = limited - previous;
    let fraction = if delta.is_finite() && allowed.is_finite() {
        allowed / delta
    } else {
        previous.mul_add(-0.5, limited * 0.5) / previous.mul_add(-0.5, raw * 0.5)
    };
    scale.min(fraction.clamp(0., 1.))
}
