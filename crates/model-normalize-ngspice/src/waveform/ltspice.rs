//! Occurrence-bound `LTspice` independent-source parameters and timing laws.
//! The dialect-specific input is resolved into shared, numeric forcing.

use super::{
    ExponentialForcing, Forcing, Knot, Repeat, RepeatLimit, Scalar, optional, pulse_knots, scalar,
};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

/// A PULSE positional declaration. Optional trailing fields remain absent
/// until occurrence binding, where `LTspice` defaults can be applied.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LtspicePulse {
    pub first: Scalar,
    pub second: Scalar,
    pub delay: Scalar,
    pub rise: Scalar,
    pub fall: Scalar,
    pub width: Option<Scalar>,
    pub period: Option<Scalar>,
    pub count: Option<Scalar>,
}

/// One PWL pair; a leading '+' on a time field denotes relative time,
/// before applying the signed time scale.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LtspicePoint {
    pub time: Scalar,
    pub relative: bool,
    pub value: Scalar,
}

/// Ordered numeric PWL coordinate declarations.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LtspicePwl {
    pub points: Vec<LtspicePoint>,
    pub time_scale: Scalar,
    pub value_scale: Scalar,
}

/// A damped sine, with independent phase and optional finite cycle count.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LtspiceSine {
    pub offset: Scalar,
    pub amplitude: Scalar,
    pub frequency: Option<Scalar>,
    pub delay: Option<Scalar>,
    pub damping: Option<Scalar>,
    pub phase: Option<Scalar>,
    pub count: Option<Scalar>,
}

/// Exponential source with explicit timing fields. Analysis-dependent
/// omitted/zero time constants and fall delay need a bound analysis plan.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LtspiceExp {
    pub initial: Scalar,
    pub pulsed: Scalar,
    pub rise_delay: Scalar,
    pub rise_tau: Scalar,
    pub fall_delay: Scalar,
    pub fall_tau: Scalar,
}

impl LtspiceExp {
    fn forcing(&self, resolve: &mut impl FnMut(&str) -> Result<f64>) -> Result<Forcing> {
        let exponential = ExponentialForcing {
            initial: scalar(&self.initial, resolve)?,
            pulsed: scalar(&self.pulsed, resolve)?,
            rise_delay: scalar(&self.rise_delay, resolve)?,
            rise_tau: scalar(&self.rise_tau, resolve)?,
            fall_delay: scalar(&self.fall_delay, resolve)?,
            fall_tau: scalar(&self.fall_tau, resolve)?,
        };
        ensure!(
            exponential.rise_tau > 0. && exponential.fall_tau > 0. && exponential.fall_delay != 0.,
            "LTspice EXP zero/default timing or nonpositive time constant needs qualified analysis binding"
        );
        Ok(Forcing::Exponential(exponential))
    }
}

/// Source-specific declarations resolved by the generic prepared-waveform
/// binder, not by a runtime compatibility-mode expression interpreter.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "form", content = "fields", rename_all = "kebab-case")]
pub enum LtspiceWaveform {
    Exp(LtspiceExp),
    Pulse(LtspicePulse),
    Pwl(LtspicePwl),
    Sine(LtspiceSine),
}

/// Resolve `LTspice`'s analysis-independent defaults. Explicit negative
/// durations clamp to zero; an undersized explicit period is raised first.
fn pulse_timing(values: &[f64]) -> Result<[f64; 6]> {
    let [delay, rise, fall, rest @ ..] = values else {
        anyhow::bail!("LTspice PULSE requires at least five total arguments");
    };
    ensure!(
        rest.len() <= 3 && values.iter().all(|v| v.is_finite()),
        "invalid LTspice pulse timing"
    );
    let delay = delay.max(0.);
    let rise = rise.max(0.);
    let fall = fall.max(0.);
    let width = rest.first().copied().unwrap_or(0.).max(0.);
    let supplied_period = rest.get(1).copied().map(|value| value.max(0.));
    let sum = rise + fall + width;
    let period = supplied_period.map_or(0., |period| {
        if period <= sum {
            sum * (1. + 2e-15)
        } else {
            period
        }
    });
    let default_basis = if period > 0. {
        let margin = period - sum;
        if width > 0. {
            width.min(margin)
        } else {
            margin
        }
    } else if width > 0. {
        width
    } else if sum > 0. {
        sum
    } else {
        1e-7
    };
    let default_edge = 0.1 * default_basis;
    let rise = if rise > 0. { rise } else { default_edge };
    let fall = if fall > 0. { fall } else { default_edge };
    let active = rise + fall + width;
    let one_shot = supplied_period.is_none() || period == 0.;
    let period = if one_shot {
        active * (1. + 2e-15)
    } else {
        period
    };
    let count = if one_shot {
        1.
    } else {
        rest.get(2).copied().unwrap_or(0.)
    };
    let timing = [delay, rise, fall, width, period, count];
    ensure!(
        timing.iter().all(|value| value.is_finite())
            && rise > 0.
            && fall > 0.
            && period > 0.
            && active <= period,
        "LTspice PULSE timing cannot be represented as a finite pulse"
    );
    Ok(timing)
}

impl LtspicePulse {
    fn coefficients(&self, resolve: &mut impl FnMut(&str) -> Result<f64>) -> Result<[f64; 8]> {
        let mut timing = vec![
            scalar(&self.delay, resolve)?,
            scalar(&self.rise, resolve)?,
            scalar(&self.fall, resolve)?,
        ];
        let optional = [
            self.width.as_ref(),
            self.period.as_ref(),
            self.count.as_ref(),
        ];
        let count = optional.iter().take_while(|field| field.is_some()).count();
        ensure!(
            optional[count..].iter().all(Option::is_none),
            "noncontiguous LTspice PULSE arguments"
        );
        for value in optional.into_iter().flatten() {
            timing.push(scalar(value, resolve)?);
        }
        let [delay, rise, fall, width, period, count] = pulse_timing(&timing)?;
        Ok([
            scalar(&self.first, resolve)?,
            scalar(&self.second, resolve)?,
            delay,
            rise,
            fall,
            width,
            period,
            count,
        ])
    }

    fn forcing(&self, resolve: &mut impl FnMut(&str) -> Result<f64>) -> Result<Forcing> {
        let [first, second, delay, rise, fall, width, period, count] =
            self.coefficients(resolve)?;
        let limit = if count > 0. {
            let time = count.mul_add(period, delay);
            ensure!(
                time.is_finite() && time > delay,
                "unrepresentable pulse repetition cutoff"
            );
            Some(RepeatLimit { time, value: first })
        } else {
            None
        };
        Ok(Forcing::PiecewiseLinear {
            knots: pulse_knots(first, second, rise, width, fall, period)?,
            repeat: Some(Repeat {
                origin: delay,
                period,
                hold: Some(first),
                limit,
            }),
        })
    }
}

impl LtspicePwl {
    fn forcing(&self, resolve: &mut impl FnMut(&str) -> Result<f64>) -> Result<Forcing> {
        let time_scale = scalar(&self.time_scale, resolve)?;
        let value_scale = scalar(&self.value_scale, resolve)?;
        ensure!(time_scale != 0., "LTspice PWL time scale must be nonzero");
        let mut previous = 0.;
        let mut pairs = Vec::with_capacity(self.points.len());
        for point in &self.points {
            let time = scalar(&point.time, resolve)?;
            previous = if point.relative {
                previous + time
            } else {
                time
            };
            let time = previous * time_scale;
            let value = scalar(&point.value, resolve)? * value_scale;
            ensure!(
                time.is_finite()
                    && value.is_finite()
                    && pairs.last().is_none_or(|(last, _)| time >= *last),
                "LTspice PWL coordinates must be finite and chronological after scaling"
            );
            pairs.push((time, value));
        }
        ensure!(!pairs.is_empty(), "empty LTspice PWL");
        let mut knots = Vec::new();
        let mut index = 0;
        while index < pairs.len() {
            let time = pairs[index].0;
            let end = pairs[index..]
                .iter()
                .position(|(next, _)| *next > time)
                .map_or(pairs.len(), |offset| index + offset);
            let count = f64::from(u32::try_from(end - index)?);
            let value = pairs[index..end]
                .iter()
                .map(|(_, value)| value / count)
                .sum::<f64>();
            ensure!(value.is_finite(), "nonfinite averaged LTspice PWL ordinate");
            knots.push(Knot {
                time,
                left: value,
                right: value,
            });
            index = end;
        }
        Ok(Forcing::PiecewiseLinear {
            knots,
            repeat: None,
        })
    }
}

impl LtspiceSine {
    fn forcing(&self, resolve: &mut impl FnMut(&str) -> Result<f64>) -> Result<Forcing> {
        let frequency = optional(self.frequency.as_deref(), 0., resolve)?;
        let count = optional(self.count.as_deref(), 0., resolve)?;
        let stop_after = if count > 0. && frequency != 0. {
            let stop = count / frequency;
            ensure!(stop.is_finite(), "unrepresentable sine repetition cutoff");
            Some(stop)
        } else {
            None
        };
        Ok(Forcing::Sine {
            offset: scalar(&self.offset, resolve)?,
            amplitude: scalar(&self.amplitude, resolve)?,
            frequency,
            delay: optional(self.delay.as_deref(), 0., resolve)?,
            damping: optional(self.damping.as_deref(), 0., resolve)?,
            phase: optional(self.phase.as_deref(), 0., resolve)?,
            hold_before_delay: true,
            stop_after,
        })
    }
}

impl LtspiceWaveform {
    pub(super) fn resolve(&self, resolve: &mut impl FnMut(&str) -> Result<f64>) -> Result<Forcing> {
        match self {
            Self::Exp(exp) => exp.forcing(resolve),
            Self::Pulse(pulse) => pulse.forcing(resolve),
            Self::Pwl(pwl) => pwl.forcing(resolve),
            Self::Sine(sine) => sine.forcing(resolve),
        }
    }

    /// Map each scalar expression exactly once, preserving structural omission,
    /// relative-time flags and source field order.
    ///
    /// # Errors
    /// A source scalar could not be normalized.
    pub fn map_scalars(&mut self, mut map: impl FnMut(&str) -> Result<Scalar>) -> Result<()> {
        let mut apply = |value: &mut Scalar| -> Result<()> {
            *value = map(value)?;
            Ok(())
        };
        match self {
            Self::Exp(exp) => {
                for value in [
                    &mut exp.initial,
                    &mut exp.pulsed,
                    &mut exp.rise_delay,
                    &mut exp.rise_tau,
                    &mut exp.fall_delay,
                    &mut exp.fall_tau,
                ] {
                    apply(value)?;
                }
            }
            Self::Pulse(pulse) => {
                for value in [
                    &mut pulse.first,
                    &mut pulse.second,
                    &mut pulse.delay,
                    &mut pulse.rise,
                    &mut pulse.fall,
                ]
                .into_iter()
                .chain(
                    [&mut pulse.width, &mut pulse.period, &mut pulse.count]
                        .into_iter()
                        .filter_map(Option::as_mut),
                ) {
                    apply(value)?;
                }
            }
            Self::Pwl(pwl) => {
                apply(&mut pwl.time_scale)?;
                apply(&mut pwl.value_scale)?;
                for point in &mut pwl.points {
                    apply(&mut point.time)?;
                    apply(&mut point.value)?;
                }
            }
            Self::Sine(sine) => {
                for value in [&mut sine.offset, &mut sine.amplitude].into_iter().chain(
                    [
                        &mut sine.frequency,
                        &mut sine.delay,
                        &mut sine.damping,
                        &mut sine.phase,
                        &mut sine.count,
                    ]
                    .into_iter()
                    .filter_map(Option::as_mut),
                ) {
                    apply(value)?;
                }
            }
        }
        Ok(())
    }

    /// Native reference rendering for a completely bound source. Return
    /// `None` when the typed forcing is mandatory (e.g. finite sine train).
    ///
    /// # Errors
    /// Unbound/invalid scalar or unrepresentable native timing.
    pub fn native_reference(
        &self,
        mut resolve: impl FnMut(&str) -> Result<f64>,
    ) -> Result<Option<String>> {
        use std::fmt::Write as _;
        let mut output = String::new();
        if let Self::Pulse(pulse) = self {
            let values = pulse.coefficients(&mut resolve)?;
            output.push_str("PULSE(");
            for (index, value) in values.iter().enumerate() {
                if index != 0 {
                    output.push(' ');
                }
                write!(&mut output, "{value}")?;
            }
            output.push(')');
            return Ok(Some(output));
        }
        match self.resolve(&mut resolve)? {
            Forcing::Exponential(exp) => return Ok(exp.native_reference()),
            Forcing::PiecewiseLinear {
                knots,
                repeat: None,
            } => {
                let Ok(reference) = super::NativePwlReference::build(&knots, None, 0.) else {
                    return Ok(None);
                };
                reference.write(&mut output)?;
                output.drain(..output.len() - output.trim_start().len());
            }
            Forcing::Sine {
                offset,
                amplitude,
                frequency,
                delay,
                damping,
                phase,
                stop_after: None,
                ..
            } => {
                if frequency == 0. {
                    if damping != 0. && amplitude != 0. {
                        return Ok(None);
                    }
                    write!(
                        &mut output,
                        "PWL(0 {})",
                        amplitude.mul_add(phase.to_radians().sin(), offset)
                    )?;
                } else {
                    let shift = delay.max(0.);
                    let elapsed = shift - delay;
                    let peak = if amplitude == 0. {
                        0.
                    } else {
                        amplitude * (-damping * elapsed).exp()
                    };
                    let phase = (elapsed * frequency % 1.).mul_add(360., phase) % 360.;
                    ensure!(
                        peak.is_finite() && phase.is_finite(),
                        "nonfinite native sine coefficients"
                    );
                    write!(
                        &mut output,
                        "SIN({offset} {peak} {frequency} {shift} {damping} {phase})"
                    )?;
                }
            }
            _ => return Ok(None),
        }
        Ok(Some(output))
    }
}
