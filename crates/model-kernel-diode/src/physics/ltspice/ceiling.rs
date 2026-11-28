//! Historical source-law conductance ceiling, separate from Newton limiting.
use super::{MAX_CONDUCTANCE, Slope};
use crate::physics::ExponentialJunction;
use crate::{ModelError, finite};

#[derive(Clone, Copy, Debug)]
pub(super) struct Tail {
    pivot: f64,
    value: f64,
    slope: f64,
    direction: f64,
}

impl Tail {
    /// Find a tangent at the physical conductance ceiling. The two bulk
    /// branches increase in opposite directions; use a signed coordinate
    /// to bracket the crossing even when it lies on the other side of zero.
    pub(super) fn find(law: impl Fn(f64) -> Slope, direction: f64) -> Result<Self, ModelError> {
        let exceeds = |x: f64| {
            let g = law(direction * x).derivative;
            !g.is_finite() || g > MAX_CONDUCTANCE
        };
        let (mut low, mut high) = if exceeds(0.) { (-1., 0.) } else { (0., 1.) };
        for _ in 0..1024 {
            if !exceeds(low) && exceeds(high) {
                break;
            }
            if exceeds(low) {
                low *= 2.;
            } else {
                high *= 2.;
            }
            if !low.is_finite() || !high.is_finite() {
                return Err(ModelError::Invalid(
                    "LTspice diode conductance ceiling bracket",
                ));
            }
        }
        if exceeds(low) || !exceeds(high) {
            return Err(ModelError::Invalid(
                "LTspice diode conductance ceiling crossing",
            ));
        }
        for _ in 0..128 {
            let middle = low.midpoint(high);
            if middle.to_bits() == low.to_bits() || middle.to_bits() == high.to_bits() {
                break;
            }
            if exceeds(middle) {
                high = middle;
            } else {
                low = middle;
            }
        }
        let pivot = direction * low;
        let point = law(pivot);
        finite(
            "LTspice diode ceiling",
            &[pivot, point.value, point.derivative],
        )?;
        Ok(Self {
            pivot,
            value: point.value,
            slope: point.derivative,
            direction,
        })
    }

    pub(super) fn sidewall(transport: ExponentialJunction) -> Result<Self, ModelError> {
        let vt = transport.inverse_vt.recip();
        let pivot = vt * (MAX_CONDUCTANCE.ln() + vt.ln() - transport.log_saturation);
        finite("LTspice sidewall ceiling", &[pivot])?;
        // Unlike the bulk tangent, the measured sidewall law omits the
        // exponential value at the ceiling: it restarts from zero there.
        Ok(Self {
            pivot,
            value: 0.,
            slope: MAX_CONDUCTANCE,
            direction: 1.,
        })
    }

    pub(super) fn evaluate(self, voltage: f64) -> Option<Slope> {
        if self.direction * (voltage - self.pivot) > 0. {
            Some(Slope {
                value: self.slope.mul_add(voltage - self.pivot, self.value),
                derivative: self.slope,
            })
        } else {
            None
        }
    }
}
