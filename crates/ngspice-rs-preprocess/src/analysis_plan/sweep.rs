//! Typed source sweep declarations. Binding is separate from parsing: source
//! names are resolved against the occurrence graph, not inferred from a
//! flattened native device spelling.

use anyhow::{Context, Result, ensure};
use model_normalize_ngspice::model_card::parse_spice_constant;
use model_preprocess_command::{Resource, WorkBudget};
use serde::{Deserialize, Serialize};

/// Unresolved source target, relative to the enclosing plan entry's scope.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SweepTarget {
    /// Lexically bound circuit parameter.
    Parameter {
        /// Original parameter name.
        name: String,
    },
    /// Independent voltage or current source.
    Source {
        /// Original source identity; the consumer verifies its device type.
        name: String,
    },
    /// Device's source-language default instance parameter (not necessarily V/I).
    Device {
        /// Original device identity.
        name: String,
    },
    /// Excitation frequency in hertz.
    Frequency,
    /// Actual circuit temperature in Celsius, not the model's nominal temperature.
    Temperature,
    /// Source-selected model parameter; resolution may require model binning
    /// or occurrence-specific physical preparation.
    ModelParameter {
        /// Explicit source model family, when the syntax specifies it.
        family: Option<String>,
        /// Source model name.
        model: String,
        /// Source parameter name.
        parameter: String,
    },
}

/// How independent coordinates are generated. Raw scalar spellings are
/// preserved until the source expression and occurrence binder resolves them.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CoordinateLaw {
    /// Explicit values, retaining source order and duplicate coordinates.
    List {
        /// Source scalar values.
        values: Vec<String>,
    },
    /// Uniform arithmetic increments, not a count of total points.
    Linear {
        /// First coordinate.
        start: String,
        /// Requested final bound.
        stop: String,
        /// Signed increment.
        increment: String,
    },
    /// Arithmetic range whose positive increment is a magnitude, not a direction.
    LinearMagnitude {
        /// First coordinate.
        start: String,
        /// Requested final bound.
        stop: String,
        /// Positive increment magnitude.
        increment: String,
    },
    /// Uniform arithmetic grid including both endpoints, with a total point count.
    LinearCount {
        /// First coordinate.
        start: String,
        /// Last coordinate.
        stop: String,
        /// Total number of coordinates, not an increment.
        count: String,
    },
    /// Logarithmic coordinates, with the density specified per interval.
    Logarithmic {
        /// First coordinate.
        start: String,
        /// Requested final bound.
        stop: String,
        /// Points per octave or decade.
        points_per_interval: String,
        /// Base of the logarithmic interval.
        interval: LogInterval,
        /// Permitted direction.
        direction: LogDirection,
    },
}

/// Unit of logarithmic sweep density.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogInterval {
    /// Factors of two.
    Octave,
    /// Factors of ten.
    Decade,
}

impl LogInterval {
    const fn base(self) -> f64 {
        match self {
            Self::Octave => 2.,
            Self::Decade => 10.,
        }
    }
}

/// Source-specific logarithmic range direction.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogDirection {
    /// Either direction is permitted.
    Either,
    /// Stop must be greater than start.
    Increasing,
}

/// Relationship between successive solves.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SweepExecution {
    /// Each coordinate starts an independent analysis with a new candidate.
    Independent,
    /// Coordinates share the continuation state of one analysis.
    Continuation,
}

/// Coordinate-order policy. This is separate from the order of the axes in
/// the plan so sorting the plotted dimension cannot reorder other dimensions.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoordinateOrder {
    /// Preserve the source-declared order.
    Declared,
    /// Increasing physical coordinate.
    Ascending,
    /// Require monotonic source order without sorting.
    Monotonic,
}

/// Source-specific minimum coordinate policy. A one-coordinate independent
/// run is meaningful in some dialects, but LTspice STEP requires variation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cardinality {
    /// At least one coordinate.
    Nonempty,
    /// At least two distinct coordinates.
    Distinct,
}

/// One source sweep axis, prior to occurrence binding.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SweepAxis {
    /// Unresolved target.
    pub target: SweepTarget,
    /// Coordinate generation law.
    pub law: CoordinateLaw,
    /// Independent or continuation execution.
    pub execution: SweepExecution,
    /// Per-axis ordering policy, selected by the source plan.
    pub order: CoordinateOrder,
    /// Minimum coordinate requirement of the original source directive.
    pub cardinality: Cardinality,
}

pub(super) fn name(raw: &str) -> Result<String> {
    ensure!(
        !raw.is_empty()
            && raw
                .chars()
                .all(|c| !c.is_whitespace() && !matches!(c, '(' | ')' | '{' | '}' | '=' | ',')),
        "invalid sweep target identifier {raw:?}"
    );
    Ok(raw.to_owned())
}

pub(super) fn target(
    args: &[String],
    allow_display_name: bool,
) -> Result<(SweepTarget, &[String])> {
    let (first, rest) = args.split_first().context("missing sweep target")?;
    if first.eq_ignore_ascii_case("param") {
        let (parameter, rest) = rest.split_first().context("missing swept parameter name")?;
        return Ok((
            SweepTarget::Parameter {
                name: name(parameter)?,
            },
            rest,
        ));
    }
    if first.eq_ignore_ascii_case("temp") {
        return Ok((SweepTarget::Temperature, rest));
    }
    if rest.len() >= 3
        && let Some(model) = rest.first()
        && let Some((model_name, parameter)) = model.split_once('(')
        && !model_name.starts_with(['{', '\'', '"'])
    {
        let parameter = parameter
            .strip_suffix(')')
            .context("unterminated model-parameter target")?;
        return Ok((
            SweepTarget::ModelParameter {
                family: Some(name(first)?),
                model: name(model_name)?,
                parameter: name(parameter)?,
            },
            &rest[1..],
        ));
    }
    ensure!(
        allow_display_name
            || first
                .as_bytes()
                .first()
                .is_some_and(|c| matches!(c.to_ascii_lowercase(), b'v' | b'i')),
        "unsupported independent-source sweep target {first:?}"
    );
    Ok((SweepTarget::Source { name: name(first)? }, rest))
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum RangeMode {
    Linear,
    Logarithmic(LogInterval),
}

pub(super) fn range_mode(value: &str) -> Option<RangeMode> {
    match value.to_ascii_lowercase().as_str() {
        "lin" => Some(RangeMode::Linear),
        "oct" => Some(RangeMode::Logarithmic(LogInterval::Octave)),
        "dec" => Some(RangeMode::Logarithmic(LogInterval::Decade)),
        _ => None,
    }
}

fn list(values: &[String]) -> Result<CoordinateLaw> {
    ensure!(
        values.len() >= 2,
        "LTspice STEP requires at least two coordinates"
    );
    let first = parse_spice_constant(&values[0])?;
    let mut distinct = false;
    for value in &values[1..] {
        let value = parse_spice_constant(value)?;
        distinct |= value.partial_cmp(&first) != Some(std::cmp::Ordering::Equal);
    }
    ensure!(distinct, "LTspice STEP has only one distinct coordinate");
    Ok(CoordinateLaw::List {
        values: values.to_vec(),
    })
}

fn law(args: &[String], mode: Option<RangeMode>) -> Result<CoordinateLaw> {
    if args
        .first()
        .is_some_and(|arg| arg.eq_ignore_ascii_case("list"))
    {
        ensure!(mode.is_none(), "LIST cannot be combined with a range mode");
        return list(&args[1..]);
    }
    let [start, stop, third] = args else {
        anyhow::bail!(
            "STEP range requires start, stop and increment/density; external file lists require admitted resource binding"
        );
    };
    Ok(match mode.unwrap_or(RangeMode::Linear) {
        RangeMode::Linear => CoordinateLaw::Linear {
            start: start.clone(),
            stop: stop.clone(),
            increment: third.clone(),
        },
        RangeMode::Logarithmic(interval) => CoordinateLaw::Logarithmic {
            start: start.clone(),
            stop: stop.clone(),
            points_per_interval: third.clone(),
            interval,
            direction: LogDirection::Either,
        },
    })
}

impl SweepAxis {
    /// Parse the documented LTspice STEP target and coordinate syntax.
    /// The caller retains the statement origin and lexical scope in the plan.
    ///
    /// # Errors
    /// Missing or ambiguous target, malformed grid, invalid numeric list,
    /// or file-backed coordinates that have not been admitted.
    pub fn ltspice_step(args: &[String]) -> Result<Self> {
        Self::compatibility_step(args, false)
    }

    /// Parse the SIMetrix single-level compatibility STEP form.
    ///
    /// # Errors
    /// Malformed target or range; nested or file-backed forms require the
    /// separately typed multi-step analysis grammar.
    pub fn simetrix_step(args: &[String]) -> Result<Self> {
        Self::compatibility_step(args, true)
    }

    fn compatibility_step(args: &[String], simetrix: bool) -> Result<Self> {
        let (first, rest) = args.split_first().context("missing STEP operands")?;
        let mode = range_mode(first);
        let (target, fields) = target(if mode.is_some() { rest } else { args }, simetrix)?;
        ensure!(
            !simetrix
                || !fields
                    .first()
                    .is_some_and(|field| field.eq_ignore_ascii_case("LIST")),
            "SIMetrix STEP list requires an explicit analysis SWEEP LIST declaration"
        );
        Ok(Self {
            target,
            law: law(fields, mode)?,
            execution: SweepExecution::Independent,
            order: CoordinateOrder::Declared,
            cardinality: if simetrix {
                Cardinality::Nonempty
            } else {
                Cardinality::Distinct
            },
        })
    }

    /// The ordered axes of one LTspice DC continuation analysis.
    /// The first axis is the innermost; each outer coordinate starts a
    /// separate continuation of the inner analysis.
    ///
    /// # Errors
    /// Missing fields, unsupported target types, or source syntax outside
    /// the documented independent-source/temperature DC form.
    pub fn ltspice_dc(args: &[String]) -> Result<Vec<Self>> {
        let (groups, remainder) = args.as_chunks::<4>();
        ensure!(
            !groups.is_empty() && remainder.is_empty(),
            "LTspice DC needs source/temperature, start, stop and increment per axis"
        );
        groups
            .iter()
            .map(|[target, start, stop, increment]| {
                let target = if target.eq_ignore_ascii_case("temp") {
                    SweepTarget::Temperature
                } else {
                    ensure!(
                        target
                            .as_bytes()
                            .first()
                            .is_some_and(|byte| matches!(byte.to_ascii_lowercase(), b'v' | b'i')),
                        "LTspice DC target must be an independent source or temperature"
                    );
                    SweepTarget::Source {
                        name: name(target)?,
                    }
                };
                Ok(Self {
                    target,
                    law: CoordinateLaw::Linear {
                        start: start.clone(),
                        stop: stop.clone(),
                        increment: increment.clone(),
                    },
                    execution: SweepExecution::Continuation,
                    order: CoordinateOrder::Declared,
                    cardinality: Cardinality::Nonempty,
                })
            })
            .collect()
    }

    /// The multi-temperature directive uses the same independent list law.
    ///
    /// # Errors
    /// Invalid or duplicate-only numeric temperature list.
    pub fn ltspice_temperature(values: &[String]) -> Result<Self> {
        Ok(Self {
            target: SweepTarget::Temperature,
            law: list(values)?,
            execution: SweepExecution::Independent,
            order: CoordinateOrder::Declared,
            cardinality: Cardinality::Distinct,
        })
    }

    /// Bind one axis without expanding a circuit per coordinate. The caller
    /// supplies the source-specific scalar resolver, including lexical scope.
    ///
    /// # Errors
    /// Unbound or invalid scalar, non-progressing or excessive grid,
    /// invalid physical temperature, or cumulative work exhaustion.
    pub fn bind(
        &self,
        mut resolve: impl FnMut(&str) -> Result<f64>,
        budget: &WorkBudget,
    ) -> Result<BoundAxis> {
        let coordinates = self.bind_coordinates(&mut resolve, budget)?;
        let count = coordinates.len();
        ensure!(count > 0, "empty sweep axis");
        budget.charge(Resource::EvaluationSteps, count)?;
        self.validate_coordinates(&coordinates, count)?;
        Ok(BoundAxis { coordinates })
    }

    fn bind_coordinates(
        &self,
        mut resolve: impl FnMut(&str) -> Result<f64>,
        budget: &WorkBudget,
    ) -> Result<Coordinates> {
        Ok(match &self.law {
            CoordinateLaw::List { values } => {
                budget.charge(Resource::GeneratedNodes, values.len())?;
                let mut values = values
                    .iter()
                    .map(|value| resolve(value))
                    .collect::<Result<Vec<_>>>()?;
                ensure!(
                    values.iter().all(|value| value.is_finite()),
                    "nonfinite sweep coordinate"
                );
                if self.order == CoordinateOrder::Ascending {
                    values.sort_by(f64::total_cmp);
                }
                if self.order == CoordinateOrder::Monotonic {
                    ensure!(
                        values.windows(2).all(|pair| pair[0] <= pair[1])
                            || values.windows(2).all(|pair| pair[0] >= pair[1]),
                        "sweep list must be monotonic"
                    );
                }
                Coordinates::List(values)
            }
            CoordinateLaw::Linear {
                start,
                stop,
                increment,
            }
            | CoordinateLaw::LinearMagnitude {
                start,
                stop,
                increment,
            } => {
                let (start, stop, mut increment) =
                    (resolve(start)?, resolve(stop)?, resolve(increment)?);
                if matches!(self.law, CoordinateLaw::LinearMagnitude { .. }) {
                    ensure!(increment > 0., "sweep increment magnitude must be positive");
                    if stop < start {
                        increment = -increment;
                    }
                }
                Coordinates::Range(Range::linear(start, stop, increment, self.order)?)
            }
            CoordinateLaw::LinearCount { start, stop, count } => {
                let (start, stop, count) = (resolve(start)?, resolve(stop)?, resolve(count)?);
                Coordinates::Range(Range::linear_count(start, stop, count, self.order)?)
            }
            CoordinateLaw::Logarithmic {
                start,
                stop,
                points_per_interval,
                interval,
                direction,
            } => {
                let (start, stop, density) = (
                    resolve(start)?,
                    resolve(stop)?,
                    resolve(points_per_interval)?,
                );
                ensure!(
                    *direction != LogDirection::Increasing || stop > start,
                    "logarithmic sweep must increase"
                );
                Coordinates::Range(Range::logarithmic(
                    start, stop, density, *interval, self.order,
                )?)
            }
        })
    }

    fn validate_coordinates(&self, coordinates: &Coordinates, count: usize) -> Result<()> {
        let mut previous = None;
        let mut distinct = false;
        for index in 0..count {
            let value = coordinates
                .value(index)
                .context("invalid sweep coordinate")?;
            ensure!(value.is_finite(), "nonfinite sweep coordinate");
            if self.target == SweepTarget::Frequency {
                ensure!(value >= 0., "swept frequency cannot be negative");
            }
            if self.target == SweepTarget::Temperature {
                ensure!(
                    value > -273.15,
                    "swept temperature must be above absolute zero"
                );
            }
            if let Some(previous) = previous {
                let changed = value.partial_cmp(&previous) != Some(std::cmp::Ordering::Equal);
                ensure!(
                    changed || matches!(coordinates, Coordinates::List(_)),
                    "sweep increment does not change the coordinate"
                );
                distinct |= changed;
            }
            previous = Some(value);
        }
        ensure!(
            self.cardinality == Cardinality::Nonempty || distinct,
            "source sweep requires at least two distinct coordinates"
        );
        Ok(())
    }
}

/// Validated coordinate sequence with O(1) range storage and random access.
#[derive(Debug)]
pub struct BoundAxis {
    coordinates: Coordinates,
}

impl BoundAxis {
    /// Number of coordinates, including repeated list entries.
    #[must_use = "the count bounds analysis work"]
    pub fn len(&self) -> usize {
        self.coordinates.len()
    }

    /// Whether the axis is empty. Successful binding never produces an empty axis.
    #[must_use = "an empty axis cannot drive an analysis"]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Coordinate at a zero-based index in the bound ordering.
    #[must_use = "the coordinate is the candidate override"]
    pub fn value(&self, index: usize) -> Option<f64> {
        self.coordinates.value(index)
    }
}

#[derive(Debug)]
enum Coordinates {
    List(Vec<f64>),
    Range(Range),
}

impl Coordinates {
    fn len(&self) -> usize {
        match self {
            Self::List(values) => values.len(),
            Self::Range(range) => range.count as usize,
        }
    }

    fn value(&self, index: usize) -> Option<f64> {
        match self {
            Self::List(values) => values.get(index).copied(),
            Self::Range(range) => range.value(u32::try_from(index).ok()?),
        }
    }
}

#[derive(Debug)]
struct Range {
    start: f64,
    stop: f64,
    spacing: RangeSpacing,
    count: u32,
    snap_stop: bool,
    reverse: bool,
}

#[derive(Debug)]
enum RangeSpacing {
    Increment(f64),
    Logarithmic(f64),
    Interpolated,
}

// Count arithmetic is performed before allocation. u32 is more than the
// default request's coordinate-work budget and permits exact f64 indexing.
#[expect(
    clippy::cast_possible_truncation,
    reason = "finite integral value is checked against u32::MAX before the cast"
)]
#[expect(
    clippy::cast_sign_loss,
    reason = "the nonnegative bound is checked before conversion"
)]
fn checked_count(intervals: f64) -> Result<u32> {
    ensure!(
        intervals.is_finite() && intervals >= 0. && intervals < f64::from(u32::MAX),
        "sweep coordinate count overflow"
    );
    Ok(intervals as u32 + 1)
}

impl Range {
    fn new(
        start: f64,
        stop: f64,
        increment: f64,
        ratio: f64,
        order: CoordinateOrder,
        logarithmic: bool,
    ) -> Result<Self> {
        ensure!(
            start.is_finite()
                && stop.is_finite()
                && increment.is_finite()
                && increment != 0.
                && ratio.is_finite()
                && ratio >= 0.,
            "invalid sweep bounds or increment direction"
        );
        let slack = 32. * f64::EPSILON * ratio.max(1.);
        let intervals = (ratio + slack).floor();
        Ok(Self {
            start,
            stop,
            spacing: if logarithmic {
                RangeSpacing::Logarithmic(increment)
            } else {
                RangeSpacing::Increment(increment)
            },
            count: checked_count(intervals)?,
            snap_stop: intervals > 0. && (ratio - intervals).abs() <= slack,
            reverse: order == CoordinateOrder::Ascending && stop < start,
        })
    }

    fn linear(start: f64, stop: f64, increment: f64, order: CoordinateOrder) -> Result<Self> {
        let difference = stop - start;
        let ratio = if difference.is_finite() {
            difference / increment
        } else {
            // Opposite-sign finite endpoints can have an overflowing
            // difference even when the number of increments is small.
            stop / increment - start / increment
        };
        Self::new(start, stop, increment, ratio, order, false)
    }

    fn linear_count(start: f64, stop: f64, count: f64, order: CoordinateOrder) -> Result<Self> {
        ensure!(
            start.is_finite()
                && stop.is_finite()
                && count.is_finite()
                && count >= 1.
                && count.fract() == 0.,
            "linear point-count sweep requires finite endpoints and positive integral count"
        );
        let count = checked_count(count - 1.)?;
        ensure!(
            count > 1 || start.partial_cmp(&stop) == Some(std::cmp::Ordering::Equal),
            "single-point linear sweep must identify one coordinate"
        );
        Ok(Self {
            start,
            stop,
            spacing: RangeSpacing::Interpolated,
            count,
            snap_stop: true,
            reverse: order == CoordinateOrder::Ascending && stop < start,
        })
    }

    fn logarithmic(
        start: f64,
        stop: f64,
        density: f64,
        interval: LogInterval,
        order: CoordinateOrder,
    ) -> Result<Self> {
        ensure!(
            start > 0. && stop > 0. && density.is_finite() && density > 0. && density.fract() == 0.,
            "logarithmic sweep needs positive endpoints and integral positive density"
        );
        let increment = interval.base().ln() / density;
        let direction = if stop < start { -1. } else { 1. };
        let increment = direction * increment;
        let ratio = (stop.ln() - start.ln()) / increment;
        Self::new(start, stop, increment, ratio, order, true)
    }

    fn value(&self, index: u32) -> Option<f64> {
        if index >= self.count {
            return None;
        }
        let index = if self.reverse {
            self.count - 1 - index
        } else {
            index
        };
        Some(if index == 0 {
            self.start
        } else if self.snap_stop && index == self.count - 1 {
            self.stop
        } else {
            match self.spacing {
                RangeSpacing::Logarithmic(increment) => {
                    increment.mul_add(f64::from(index), self.start.ln()).exp()
                }
                RangeSpacing::Increment(increment) => {
                    increment.mul_add(f64::from(index), self.start)
                }
                RangeSpacing::Interpolated => {
                    let fraction = f64::from(index) / f64::from(self.count - 1);
                    self.start.mul_add(1. - fraction, self.stop * fraction)
                }
            }
        })
    }
}

/// Reserve the full Cartesian product before execution. The caller binds the
/// axes in source order and does not allocate a deck or vector for each tuple.
///
/// # Errors
/// Integer overflow or cumulative request budget exhaustion.
pub fn reserve_product<'a>(
    axes: impl IntoIterator<Item = &'a BoundAxis>,
    budget: &WorkBudget,
) -> Result<usize> {
    let count = axes.into_iter().try_fold(1usize, |count, axis| {
        count
            .checked_mul(axis.len())
            .context("Cartesian sweep cardinality overflow")
    })?;
    budget.charge(Resource::GeneratedNodes, count)?;
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use model_preprocess_command::{Cancellation, WorkLimits};

    fn parse(text: &str) -> Result<SweepAxis> {
        SweepAxis::ltspice_step(&super::super::arguments(text)?)
    }

    #[test]
    fn source_targets_and_coordinate_laws_are_distinct() -> Result<()> {
        assert_eq!(
            parse(".step NPN Q2N3904(BF) 50 200 50")?.target,
            SweepTarget::ModelParameter {
                family: Some("NPN".into()),
                model: "Q2N3904".into(),
                parameter: "BF".into()
            }
        );
        let axis = parse(".step dec param freq 1k 1Meg 10")?;
        assert_eq!(
            axis.target,
            SweepTarget::Parameter {
                name: "freq".into()
            }
        );
        assert!(matches!(
            axis.law,
            CoordinateLaw::Logarithmic {
                interval: LogInterval::Decade,
                ..
            }
        ));
        assert_eq!(
            parse(".step temp list -40 25 125")?.target,
            SweepTarget::Temperature
        );
        assert_eq!(
            parse(".step V1 0 5 .5")?.target,
            SweepTarget::Source { name: "V1".into() }
        );
        Ok(())
    }

    #[test]
    fn first_dimension_sort_and_exact_endpoint_are_independent() -> Result<()> {
        let budget = WorkBudget::default();
        let mut descending = parse(".step param x 0.3 0 -0.1")?;
        descending.order = CoordinateOrder::Ascending;
        let grid = descending.bind(parse_spice_constant, &budget)?;
        assert_eq!(grid.len(), 4);
        assert_eq!(grid.value(0).map(f64::to_bits), Some(0_f64.to_bits()));
        assert_eq!(grid.value(3).map(f64::to_bits), Some(0.3_f64.to_bits()));
        let mut axis = parse(".step param x list 3 1 2 1")?;
        let declared = axis.bind(parse_spice_constant, &budget)?;
        axis.order = CoordinateOrder::Ascending;
        let ascending = axis.bind(parse_spice_constant, &budget)?;
        assert_eq!(
            (0..declared.len())
                .filter_map(|index| declared.value(index))
                .map(f64::to_bits)
                .collect::<Vec<_>>(),
            [3., 1., 2., 1.].map(f64::to_bits)
        );
        assert_eq!(
            (0..ascending.len())
                .filter_map(|index| ascending.value(index))
                .map(f64::to_bits)
                .collect::<Vec<_>>(),
            [1., 1., 2., 3.].map(f64::to_bits)
        );
        Ok(())
    }

    #[test]
    fn logarithmic_coordinates_are_bounded_and_do_not_overshoot() -> Result<()> {
        let budget = WorkBudget::default();
        let grid = parse(".step oct V1 1 20 2")?.bind(parse_spice_constant, &budget)?;
        assert_eq!(grid.len(), 9);
        let last = grid.value(8).context("missing final point")?;
        assert!((last - 16.).abs() < 1e-12);
        assert!(grid.value(9).is_none());
        let exact =
            parse(".step dec param freq 1k 1Meg 10")?.bind(parse_spice_constant, &budget)?;
        assert_eq!(exact.len(), 31);
        assert_eq!(exact.value(30).map(f64::to_bits), Some(1e6_f64.to_bits()));
        Ok(())
    }

    #[test]
    fn invalid_or_unbound_grids_fail_before_cartesian_expansion() -> Result<()> {
        let budget = WorkBudget::default();
        for statement in [
            ".step param x list 1",
            ".step param x list 1 1.0",
            ".step dec param x list 1 2",
            ".step R1 1 2 1",
            ".step param x list {y} 2",
        ] {
            assert!(parse(statement).is_err(), "{statement}");
        }
        for statement in [
            ".step param x 1 0 1",
            ".step param x 1 1 1",
            ".step param x 1 2 0",
            ".step oct param x -1 4 2",
            ".step dec param x 1 10 1.5",
            ".step temp -300 -200 10",
            ".step param x {a} 2 1",
        ] {
            assert!(
                parse(statement)?
                    .bind(parse_spice_constant, &budget)
                    .is_err(),
                "{statement}"
            );
        }
        let budget = WorkBudget::new(
            WorkLimits {
                generated_nodes: 100,
                ..WorkLimits::default()
            },
            Cancellation::default(),
        );
        let axis = parse(".step param x 0 10 1")?;
        let a = axis.bind(parse_spice_constant, &budget)?;
        let b = axis.bind(parse_spice_constant, &budget)?;
        assert!(reserve_product(&[a, b], &budget).is_err());
        Ok(())
    }
}
