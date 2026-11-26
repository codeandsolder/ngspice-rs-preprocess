//! Shared native reference emitter for prepared PWL forcing.

use super::{Knot, Repeat};
use anyhow::{Context, Result, ensure};
use std::fmt::Write as _;

/// Exact native PWL reference spelling for resolved prepared knots.
pub struct NativePwlReference {
    knots: Vec<Knot>,
    delay: f64,
    repeat_at: Option<f64>,
}

impl NativePwlReference {
    fn push(&mut self, knot: Knot) -> Result<()> {
        ensure!(
            knot.time.is_finite() && knot.time >= 0.,
            "native PWL time shift overflows"
        );
        ensure!(
            self.knots.last().is_none_or(|last| last.time < knot.time),
            "native PWL cannot represent shifted knot times distinctly"
        );
        self.knots.push(knot);
        Ok(())
    }

    /// Materialize an aperiodic or unbounded periodic reference.
    /// # Errors
    /// Nonrepresentable knot shifts or finite repetition.
    pub fn build(knots: &[Knot], repeat: Option<Repeat>, dc: f64) -> Result<Self> {
        let first = knots.first().context("missing waveform knots")?;
        let Some(cycle) = repeat else {
            let delay = first.time.min(0.);
            let mut result = Self {
                knots: Vec::new(),
                delay,
                repeat_at: None,
            };
            for knot in knots {
                result.push(Knot {
                    time: knot.time - delay,
                    ..*knot
                })?;
            }
            return Ok(result);
        };
        ensure!(
            cycle.limit.is_none(),
            "finite repetition needs typed waveform binding"
        );
        let prefix = cycle.hold.is_none() && cycle.origin > 0.;
        let mut result = Self {
            knots: Vec::new(),
            delay: if prefix { 0. } else { cycle.origin },
            repeat_at: Some(if prefix { cycle.origin } else { 0. }),
        };
        if prefix {
            // Native PWL holds before TD. Emit the rest of the preceding
            // cycle, then repeat from the actual positive boundary.
            result.push(Knot {
                time: 0.,
                left: dc,
                right: dc,
            })?;
            for knot in knots {
                let time = cycle.origin + (knot.time - cycle.period);
                if time > 0. {
                    ensure!(time < cycle.origin, "native PWL prefix loses a knot");
                    result.push(Knot { time, ..*knot })?;
                }
            }
        }
        let shift = if prefix { cycle.origin } else { 0. };
        for (index, knot) in knots.iter().enumerate() {
            let left = if index == 0 && !prefix {
                cycle.hold.unwrap_or(knot.right)
            } else {
                knot.left
            };
            result.push(Knot {
                time: knot.time + shift,
                left,
                ..*knot
            })?;
        }
        result.push(Knot {
            time: cycle.period + shift,
            left: first.left,
            right: first.left,
        })?;
        Ok(result)
    }

    /// Whether all emitted coordinates have one continuous ordinate.
    #[must_use = "discontinuous coordinates require typed source execution"]
    pub fn is_continuous(&self) -> bool {
        self.knots
            .iter()
            .all(|knot| knot.left.to_bits() == knot.right.to_bits())
    }

    /// Write native PWL and its explicit shift/repetition fields.
    /// # Errors
    /// Formatting failed.
    pub fn write(&self, output: &mut String) -> Result<()> {
        output.push_str(" PWL(");
        for knot in &self.knots {
            write!(output, " {:.17e} {:.17e}", knot.time, knot.left)?;
            if knot.left.to_bits() != knot.right.to_bits() {
                write!(output, " {:.17e} {:.17e}", knot.time, knot.right)?;
            }
        }
        output.push(')');
        if let Some(repeat) = self.repeat_at {
            write!(output, " r={repeat:.17e}")?;
        }
        write!(output, " td={:.17e}", self.delay)?;
        Ok(())
    }
}
