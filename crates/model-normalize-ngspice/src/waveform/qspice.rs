//! Occurrence-bound QSPICE source coordinates. Resolve increments once in
//! order instead of expanding them into a quadratic chain of expressions.

use super::{Forcing, Knot, Scalar, pulse_knots, repeat, scalar};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

/// A source PWL point. Relative coordinates increment the preceding absolute
/// time, never the previous interval or the ordinate.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct QspicePoint {
    pub time: Scalar,
    pub relative: bool,
    pub value: Scalar,
}

/// Qualified QSPICE waveform declarations with explicit timing.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "form", content = "fields", rename_all = "kebab-case")]
pub enum QspiceWaveform {
    Pwl(Vec<QspicePoint>),
    Pulse {
        first: Scalar,
        second: Scalar,
        delay: Scalar,
        rise: Scalar,
        fall: Scalar,
        width: Scalar,
        period: Scalar,
    },
}

fn pwl(points: &[QspicePoint], resolve: &mut impl FnMut(&str) -> Result<f64>) -> Result<Forcing> {
    ensure!(points.len() >= 2, "QSPICE PWL requires at least two points");
    let mut previous = 0.;
    let mut knots = Vec::with_capacity(points.len());
    for (index, point) in points.iter().enumerate() {
        let coordinate = scalar(&point.time, resolve)?;
        let time = if point.relative {
            previous + coordinate
        } else {
            coordinate
        };
        ensure!(
            time.is_finite() && time >= 0. && (index == 0 || time > previous),
            "QSPICE PWL coordinates must be finite, nonnegative and strictly increasing"
        );
        let value = scalar(&point.value, resolve)?;
        knots.push(Knot {
            time,
            left: value,
            right: value,
        });
        previous = time;
    }
    Ok(Forcing::PiecewiseLinear {
        knots,
        repeat: None,
    })
}

impl QspiceWaveform {
    pub(super) fn forcing(&self, resolve: &mut impl FnMut(&str) -> Result<f64>) -> Result<Forcing> {
        match self {
            Self::Pwl(points) => pwl(points, resolve),
            Self::Pulse {
                first,
                second,
                delay,
                rise,
                fall,
                width,
                period,
            } => {
                let first = scalar(first, resolve)?;
                let second = scalar(second, resolve)?;
                let delay = scalar(delay, resolve)?;
                let rise = scalar(rise, resolve)?;
                let fall = scalar(fall, resolve)?;
                let width = scalar(width, resolve)?;
                let period = scalar(period, resolve)?;
                ensure!(
                    delay >= 0. && rise > 0. && fall > 0. && width > 0. && period > 0.,
                    "QSPICE signed delay or default pulse durations require a separately qualified law"
                );
                let knots = pulse_knots(first, second, rise, width, fall, period)?;
                Ok(Forcing::PiecewiseLinear {
                    knots,
                    repeat: Some(repeat(period, delay, true, first)),
                })
            }
        }
    }
}
