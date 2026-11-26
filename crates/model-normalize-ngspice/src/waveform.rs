//! Resolved independent-source forcing for prepared circuits.
//!
//! A solver binds a declaration once per elaborated occurrence and
//! parameter candidate. This contract preserves exact sided discontinuities
//! without changing the compatibility semantics of native PULSE/PWL.

use anyhow::{Context, Result, ensure};
use model_preprocess_command::{Resource, WorkBudget};
use serde::{Deserialize, Serialize};

mod qspice;
pub use qspice::{QspicePoint, QspiceWaveform};

/// Scalar expressions use the emitted circuit's parameter grammar.
pub type Scalar = String;

/// Source identity after hierarchy linkage and device-name normalization.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PreparedWaveform {
    pub artifact: String,
    pub scope: Vec<String>,
    pub source: String,
    pub spec: WaveformSpec,
    /// Explicit source bias value, independent of transient time-zero forcing.
    pub dc_override: Option<Scalar>,
    /// Freeze at the startup value during source periodic operating point
    /// and its associated AC analysis.
    pub idle_in_pop: Scalar,
}

/// Time-domain source specification, prior to occurrence parameter binding.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "shape", rename_all = "kebab-case")]
pub enum WaveformSpec {
    /// Occurrence-bound QSPICE positional source laws.
    Qspice(QspiceWaveform),
    Pulse(PulseSpec),
    Saw {
        first: Scalar,
        last: Scalar,
        period: Period,
        delay: Scalar,
        hold_before_delay: Scalar,
    },
    PiecewiseLinear {
        segments: Scalar,
        points: Vec<SymbolicPoint>,
    },
    Sine {
        offset: Scalar,
        amplitude: Scalar,
        frequency: Scalar,
        delay: SineDelay,
        damping: Scalar,
        hold_before_delay: Scalar,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "kebab-case")]
pub enum Period {
    Seconds(Scalar),
    Frequency(Scalar),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "kebab-case")]
pub enum SineDelay {
    Seconds(Scalar),
    Degrees(Scalar),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SymbolicPoint {
    pub time: Scalar,
    pub value: Scalar,
}

/// Both named PUL syntaxes normalize to this pulse specification. Missing
/// slopes/width use the documented PWLS defaults; explicit zero slopes
/// are permitted only by the raw SIMPLIS pulse grammar.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PulseSpec {
    pub first: Scalar,
    pub second: Scalar,
    pub period: Period,
    pub delay: Scalar,
    pub hold_before_delay: Scalar,
    pub edges: PulseEdges,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum PulseEdges {
    Rectangular {
        duty: Scalar,
    },
    Trapezoidal {
        rise: Option<Scalar>,
        width: Option<Scalar>,
        fall: Option<Scalar>,
        positive_slopes: bool,
    },
}

/// External left and right limits at one unique, finite event time.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Knot {
    pub time: f64,
    pub left: f64,
    pub right: f64,
}

/// Periodic forcing timing.
///
/// One period begins at origin. The knots are strictly ordered in
/// [0, period); the first is at zero. Interpolate from the last right
/// ordinate to the first left ordinate at period. If hold is present,
/// origin is the actual positive delay and forcing before origin is hold.
/// Otherwise the cycle extends in both directions; origin is the nearest
/// signed cycle boundary to zero. A positive ungated origin preserves a
/// small delay which subtraction from the period would round away.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Repeat {
    pub origin: f64,
    pub period: f64,
    pub hold: Option<f64>,
}

/// Dialect-neutral resolved source forcing. No SPICE expression evaluation
/// is needed during transient stepping.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Forcing {
    PiecewiseLinear {
        knots: Vec<Knot>,
        repeat: Option<Repeat>,
    },
    Sine {
        offset: f64,
        amplitude: f64,
        frequency: f64,
        delay: f64,
        damping: f64,
        hold_before_delay: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResolvedWaveform {
    /// Operating-point bias, honoring an explicit source DC override.
    pub dc: f64,
    /// Source-defined transient initial value, independent of DC override.
    pub startup: f64,
    pub forcing: Forcing,
    pub idle_in_pop: bool,
}

fn scalar(expression: &str, resolve: &mut impl FnMut(&str) -> Result<f64>) -> Result<f64> {
    let value = resolve(expression).with_context(|| format!("waveform expression {expression}"))?;
    ensure!(
        value.is_finite(),
        "nonfinite waveform expression {expression}"
    );
    Ok(value)
}

fn flag(expression: &str, resolve: &mut impl FnMut(&str) -> Result<f64>) -> Result<bool> {
    Ok(scalar(expression, resolve)? != 0.)
}

impl Period {
    fn resolve(&self, resolve: &mut impl FnMut(&str) -> Result<f64>) -> Result<f64> {
        let period = match self {
            Self::Seconds(value) => scalar(value, resolve)?,
            Self::Frequency(value) => 1. / scalar(value, resolve)?,
        };
        ensure!(
            period.is_finite() && period > 0.,
            "waveform period must be finite and positive"
        );
        Ok(period)
    }
}

fn optional(
    expression: Option<&str>,
    default: f64,
    resolve: &mut impl FnMut(&str) -> Result<f64>,
) -> Result<f64> {
    expression.map_or(Ok(default), |text| scalar(text, resolve))
}

impl PulseEdges {
    fn resolve(
        &self,
        period: f64,
        resolve: &mut impl FnMut(&str) -> Result<f64>,
    ) -> Result<[f64; 3]> {
        match self {
            Self::Rectangular { duty } => {
                let duty = scalar(duty, resolve)?;
                ensure!(
                    duty > 0. && duty < 1.,
                    "rectangular pulse duty ratio must lie in (0,1)"
                );
                let width = period * duty;
                ensure!(
                    width > 0. && width < period,
                    "rectangular pulse width cannot be resolved in its period"
                );
                Ok([0., width, 0.])
            }
            Self::Trapezoidal {
                rise,
                width,
                fall,
                positive_slopes,
            } => {
                let rise = optional(rise.as_deref(), period / 1000., resolve)?;
                let fall = optional(fall.as_deref(), rise, resolve)?;
                let width = optional(width.as_deref(), (period - rise - fall) / 2., resolve)?;
                ensure!(
                    rise >= 0. && width >= 0. && fall >= 0.,
                    "negative pulse duration"
                );
                ensure!(
                    !positive_slopes || (rise > 0. && fall > 0.),
                    "pulse slopes must be positive"
                );
                Ok([rise, width, fall])
            }
        }
    }
}

/// Collapse coincident transitions in declaration order, retaining both
/// exterior limits. Intermediate zero-duration levels carry no area.
fn knots(points: impl IntoIterator<Item = (f64, f64)>) -> Result<Vec<Knot>> {
    let mut knots: Vec<Knot> = Vec::new();
    for (time, value) in points {
        ensure!(
            time.is_finite() && value.is_finite(),
            "nonfinite waveform point"
        );
        if let Some(last) = knots.last_mut() {
            ensure!(time >= last.time, "waveform times must be nondecreasing");
            if time <= last.time {
                last.right = value;
                continue;
            }
        }
        knots.push(Knot {
            time,
            left: value,
            right: value,
        });
    }
    ensure!(!knots.is_empty(), "waveform has no points");
    Ok(knots)
}

/// Move the final knot's incoming limit to the beginning of the cycle.
/// The end point is not an additional event in the repeating timeline.
fn cycle_knots(points: impl IntoIterator<Item = (f64, f64)>) -> Result<Vec<Knot>> {
    let mut result = knots(points)?;
    let end = result.pop().context("empty periodic waveform")?;
    let first = result
        .first_mut()
        .context("periodic waveform needs a nonzero interval")?;
    first.left = end.left;
    Ok(result)
}

fn repeat(period: f64, delay: f64, hold: bool, first: f64) -> Repeat {
    if hold && delay > 0. {
        Repeat {
            period,
            origin: delay,
            hold: Some(first),
        }
    } else {
        let remainder = delay % period;
        let origin = if remainder.abs() > period * 0.5 {
            remainder.signum().mul_add(-period, remainder)
        } else {
            remainder
        };
        Repeat {
            period,
            origin: if origin == 0. { 0. } else { origin },
            hold: None,
        }
    }
}

fn pulse_forcing(
    spec: &PulseSpec,
    resolve: &mut impl FnMut(&str) -> Result<f64>,
) -> Result<Forcing> {
    let period = spec.period.resolve(resolve)?;
    let first = scalar(&spec.first, resolve)?;
    let second = scalar(&spec.second, resolve)?;
    let delay = scalar(&spec.delay, resolve)?;
    let hold = flag(&spec.hold_before_delay, resolve)?;
    let [rise, width, fall] = spec.edges.resolve(period, resolve)?;
    let knots = pulse_knots(first, second, rise, width, fall, period)?;
    Ok(Forcing::PiecewiseLinear {
        knots,
        repeat: Some(repeat(period, delay, hold, first)),
    })
}

fn pulse_knots(
    first: f64,
    second: f64,
    rise: f64,
    width: f64,
    fall: f64,
    period: f64,
) -> Result<Vec<Knot>> {
    let high_end = rise + width;
    let end = high_end + fall;
    // At a full-period boundary two rounded additions can produce one
    // final ULP beyond the period. Never insert an artificial idle edge.
    ensure!(
        rise <= period && end.is_finite() && end <= period.next_up(),
        "pulse rise, width and fall exceed period"
    );
    let end = end.min(period);
    let high_end = high_end.min(period);
    ensure!(
        (width <= 0. || high_end > rise) && (fall <= 0. || end > high_end),
        "pulse duration is smaller than a representable interval"
    );
    Ok(if end == 0. {
        vec![Knot {
            time: 0.,
            left: first,
            right: first,
        }]
    } else {
        cycle_knots([
            (0., first),
            (rise, second),
            (high_end, second),
            (end, first),
            (period, first),
        ])?
    })
}

fn pwl_forcing(
    segments: &str,
    points: &[SymbolicPoint],
    resolve: &mut impl FnMut(&str) -> Result<f64>,
) -> Result<Forcing> {
    let count = scalar(segments, resolve)?;
    // Compare exact structural arity, not a truncating float->int cast.
    let declared = u32::try_from(points.len().saturating_sub(1)).context("too many PWL points")?;
    ensure!(
        declared > 0 && count.to_bits() == f64::from(declared).to_bits(),
        "PWL segment count does not match its point declarations"
    );
    let points = points
        .iter()
        .map(|point| {
            Ok((
                scalar(&point.time, resolve)?,
                scalar(&point.value, resolve)?,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Forcing::PiecewiseLinear {
        knots: knots(points)?,
        repeat: None,
    })
}

impl WaveformSpec {
    /// Resolve the occurrence's symbolic time-domain law and check all
    /// physical domain constraints after parameter evaluation.
    ///
    /// # Errors
    /// Invalid timing, unresolved symbols, nonfinite coordinates, or
    /// an inconsistent structural point count.
    pub fn resolve(&self, mut resolve: impl FnMut(&str) -> Result<f64>) -> Result<Forcing> {
        match self {
            Self::Qspice(spec) => spec.forcing(&mut resolve),
            Self::Pulse(spec) => pulse_forcing(spec, &mut resolve),
            Self::Saw {
                first,
                last,
                period,
                delay,
                hold_before_delay,
            } => {
                let period = period.resolve(&mut resolve)?;
                let first = scalar(first, &mut resolve)?;
                let last = scalar(last, &mut resolve)?;
                let delay = scalar(delay, &mut resolve)?;
                let hold = flag(hold_before_delay, &mut resolve)?;
                Ok(Forcing::PiecewiseLinear {
                    knots: vec![Knot {
                        time: 0.,
                        left: last,
                        right: first,
                    }],
                    repeat: Some(repeat(period, delay, hold, first)),
                })
            }
            Self::PiecewiseLinear { segments, points } => {
                pwl_forcing(segments, points, &mut resolve)
            }
            Self::Sine {
                offset,
                amplitude,
                frequency,
                delay,
                damping,
                hold_before_delay,
            } => {
                let frequency = scalar(frequency, &mut resolve)?;
                let amplitude = scalar(amplitude, &mut resolve)?;
                ensure!(
                    frequency > 0. && amplitude >= 0.,
                    "invalid sine frequency or amplitude"
                );
                let delay = match delay {
                    SineDelay::Seconds(text) => scalar(text, &mut resolve)?,
                    SineDelay::Degrees(text) => (scalar(text, &mut resolve)? / 360.) / frequency,
                };
                ensure!(delay.is_finite(), "sine delay overflows");
                Ok(Forcing::Sine {
                    offset: scalar(offset, &mut resolve)?,
                    amplitude,
                    frequency,
                    delay,
                    damping: scalar(damping, &mut resolve)?,
                    hold_before_delay: flag(hold_before_delay, &mut resolve)?,
                })
            }
        }
    }
}

// Startup is evaluated here, not by a transient evaluator in preprocessing.
fn affine(left: f64, right: f64, fraction: f64) -> f64 {
    left.mul_add(1. - fraction, right * fraction)
}

fn startup_pwl(knots: &[Knot], repeat: Option<Repeat>) -> Result<f64> {
    let first = knots.first().context("empty resolved waveform")?;
    let last = knots.last().context("empty resolved waveform")?;
    let (time, shift) = if let Some(cycle) = repeat {
        if let Some(hold) = cycle.hold {
            return Ok(hold);
        }
        // Before a positive ungated origin, use the preceding cycle with
        // its endpoint at zero. Forming period-origin loses tiny delays.
        (
            -cycle.origin,
            if cycle.origin > 0. { -cycle.period } else { 0. },
        )
    } else {
        (0., 0.)
    };
    if time < first.time + shift {
        return Ok(first.left);
    }
    let upper = knots.partition_point(|knot| knot.time + shift <= time);
    let left = knots[upper - 1];
    let right = if let Some(right) = knots.get(upper) {
        *right
    } else if let Some(cycle) = repeat {
        Knot {
            time: cycle.period,
            left: first.left,
            right: first.right,
        }
    } else {
        return Ok(last.right);
    };
    let left_time = left.time + shift;
    let right_time = right.time + shift;
    let span = right_time - left_time;
    if shift < 0. {
        return Ok(affine(right.left, left.right, (right_time - time) / span));
    }
    let fraction = if span.is_finite() {
        (time - left_time) / span
    } else {
        left_time.mul_add(-0.5, time * 0.5) / left_time.mul_add(-0.5, right_time * 0.5)
    };
    Ok(affine(left.right, right.left, fraction))
}

impl Forcing {
    /// Right-sided time-zero forcing, with pre-delay hold applied.
    ///
    /// # Errors
    /// Invalid resolved knots or a nonfinite startup value.
    fn startup(&self) -> Result<f64> {
        let value = match self {
            Self::PiecewiseLinear { knots, repeat } => startup_pwl(knots, *repeat)?,
            Self::Sine {
                offset,
                amplitude,
                frequency,
                delay,
                damping,
                hold_before_delay,
            } => {
                if *amplitude == 0. || (*hold_before_delay && *delay > 0.) {
                    *offset
                } else {
                    let phase = (-(delay * frequency) % 1.) * std::f64::consts::TAU;
                    (amplitude * (damping * delay).exp()).mul_add(phase.sin(), *offset)
                }
            }
        };
        ensure!(value.is_finite(), "nonfinite waveform startup value");
        Ok(value)
    }
}

impl PreparedWaveform {
    /// Bind this declaration in the actual elaborated occurrence's scope.
    ///
    /// # Errors
    /// Invalid or unbound waveform expressions and timing.
    pub fn resolve(
        &self,
        mut resolve: impl FnMut(&str) -> Result<f64>,
    ) -> Result<ResolvedWaveform> {
        let forcing = self.spec.resolve(&mut resolve)?;
        let startup = forcing.startup()?;
        let dc = optional(self.dc_override.as_deref(), startup, &mut resolve)?;
        let idle_in_pop = flag(&self.idle_in_pop, &mut resolve)?;
        Ok(ResolvedWaveform {
            dc,
            startup,
            forcing,
            idle_in_pop,
        })
    }
}

/// Verify that every source declaration binds exactly one emitted independent
/// source in its recorded artifact and lexical definition scope.
///
/// # Errors
/// Duplicate metadata, missing source, or ambiguous emitted source identity.
pub fn validate_artifact(
    declarations: &[PreparedWaveform],
    artifact: &str,
    bytes: &[u8],
    budget: &WorkBudget,
) -> Result<()> {
    use std::collections::BTreeMap;
    budget.charge(Resource::EvaluationSteps, declarations.len())?;
    let mut expected = BTreeMap::new();
    for declaration in declarations.iter().filter(|item| item.artifact == artifact) {
        budget.charge(Resource::GeneratedNodes, 1)?;
        charge_scope(&declaration.scope, declaration.source.len(), budget)?;
        let scope = declaration
            .scope
            .iter()
            .map(|name| name.to_ascii_lowercase())
            .collect::<Vec<_>>();
        let key = (scope, declaration.source.to_ascii_lowercase());
        ensure!(
            expected.insert(key, false).is_none(),
            "duplicate waveform declaration {}",
            declaration.source
        );
    }
    if expected.is_empty() {
        return Ok(());
    }
    let mut scope = Vec::new();
    let mut control = false;
    budget.charge(Resource::ScannedBytes, bytes.len())?;
    for statement in crate::lexical::statements(bytes)? {
        budget.charge(Resource::Statements, 1)?;
        let text = std::str::from_utf8(&statement.text)?;
        let mut fields = text.split_ascii_whitespace();
        let Some(first) = fields.next() else { continue };
        if first.eq_ignore_ascii_case(".control") {
            control = true;
        } else if first.eq_ignore_ascii_case(".endc") {
            control = false;
        } else if !control && first.eq_ignore_ascii_case(".subckt") {
            budget.check_depth(scope.len().saturating_add(1))?;
            scope.push(
                fields
                    .next()
                    .context("missing emitted waveform scope")?
                    .to_ascii_lowercase(),
            );
        } else if !control && first.eq_ignore_ascii_case(".ends") {
            ensure!(scope.pop().is_some(), "unmatched emitted .ends");
        } else if !control
            && first
                .as_bytes()
                .first()
                .is_some_and(|byte| matches!(byte.to_ascii_uppercase(), b'V' | b'I'))
        {
            charge_scope(&scope, first.len(), budget)?;
            let key = (scope.clone(), first.to_ascii_lowercase());
            if let Some(seen) = expected.get_mut(&key) {
                ensure!(!*seen, "ambiguous waveform source {first} in {scope:?}");
                *seen = true;
            }
        }
    }
    for ((scope, source), seen) in expected {
        ensure!(
            seen,
            "missing emitted waveform source {source} in {scope:?} ({artifact})"
        );
    }
    Ok(())
}

fn charge_scope(scope: &[String], name_bytes: usize, budget: &WorkBudget) -> Result<()> {
    budget.charge(Resource::EmittedBytes, name_bytes)?;
    budget.charge(Resource::GeneratedNodes, scope.len())?;
    for part in scope {
        budget.charge(Resource::EmittedBytes, part.len())?;
    }
    Ok(())
}
