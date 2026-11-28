//! Optional family-specific numerical trial policy. All history and acceptance
//! belong to the engine; a kernel can only propose a local linearization point
//! or report an additional convergence criterion.

use crate::{Artifact, Descriptor, EquationKind, Error, Kernel, Prepared, Result};

/// The explicitly selected owner of one numerical policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrialPolicy {
    /// No family-specific intervention; use the engine's numerical policy.
    Engine,
    /// Invoke the declared family-specific implementation.
    Kernel,
}

/// Independent policy choices, part of the artifact's immutable schema.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TrialCapabilities {
    pub initialization: TrialPolicy,
    pub limiting: TrialPolicy,
    pub convergence: TrialPolicy,
}

impl TrialCapabilities {
    /// The family explicitly requires no specialized numerical policy.
    pub const ENGINE: Self = Self {
        initialization: TrialPolicy::Engine,
        limiting: TrialPolicy::Engine,
        convergence: TrialPolicy::Engine,
    };
}

/// Trial phase chosen by the engine, with distinct history semantics.
///
/// A cold start is only for an unconstrained initial operating point.
/// Explicit initial conditions, exact residual checks, AC/noise, and loads
/// without valid reference history use Exact. The previous Newton point is
/// the actual *evaluated* point (possibly limited), not the raw proposal.
#[derive(Clone, Copy, Debug)]
pub enum TrialPhase<'a> {
    Exact,
    ColdStart,
    Newton { previous: &'a [f64] },
    Predictor { accepted: &'a [f64] },
}

/// The provenance of the reference is part of the family policy, not merely
/// a coordinate slice. Neither variant grants permission to modify history.
#[derive(Clone, Copy, Debug)]
pub enum TrialReference<'a> {
    PreviousNewton(&'a [f64]),
    AcceptedStep(&'a [f64]),
}

impl<'a> TrialReference<'a> {
    #[must_use = "the reference coordinates are required for trial limiting"]
    pub const fn coordinates(self) -> &'a [f64] {
        match self {
            Self::PreviousNewton(coordinates) | Self::AcceptedStep(coordinates) => coordinates,
        }
    }
}

/// All coordinates are in descriptor order: electrical potentials in volts,
/// thermal coordinates in absolute kelvin, internal coordinates in their
/// declared family units. Input slices are never mutated.
#[derive(Clone, Copy, Debug)]
pub struct TrialInput<'a> {
    pub proposed: &'a [f64],
    pub phase: TrialPhase<'a>,
}

/// Whether the actual linearization may represent the proposed iterate.
/// The engine must stamp at the returned coordinates, not the raw proposal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrialOutcome {
    Exact,
    Initialized,
    Limited,
}

/// Static residual and exact row-major Jacobian at the actual evaluated
/// coordinates. The engine owns integration of charge/flux and its tolerances.
#[derive(Clone, Copy, Debug)]
pub struct TrialSample<'a> {
    pub coordinates: &'a [f64],
    pub residual: &'a [f64],
    pub jacobian: &'a [f64],
}

/// Additional family convergence test.
///
/// Previous is the previous Newton load,
/// not the last accepted time step; candidate must be an exact load to pass.
/// Absolute tolerances are in each row's residual units (A, W, or family
/// defined internal units); the relative tolerance is dimensionless.
#[derive(Clone, Copy, Debug)]
pub struct ConvergenceInput<'a> {
    pub previous: TrialSample<'a>,
    pub candidate: TrialSample<'a>,
    pub candidate_outcome: TrialOutcome,
    pub relative_tolerance: f64,
    pub absolute_tolerances: &'a [f64],
}

/// A satisfied family check does not imply overall circuit convergence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Convergence {
    Satisfied,
    Retry,
}

impl ConvergenceInput<'_> {
    /// Compare a predicted and evaluated residual in the chosen row's units.
    /// Scaling avoids overflow in the difference and tolerance calculation.
    /// A nonrepresentable prediction always requires another iteration.
    ///
    /// # Errors
    /// Missing row tolerance or nonfinite evaluated residual/tolerance.
    pub fn compare_residual(&self, row: usize, predicted: f64, actual: f64) -> Result<Convergence> {
        let absolute = *self
            .absolute_tolerances
            .get(row)
            .ok_or(Error::Shape("residual tolerance row"))?;
        if !actual.is_finite()
            || !absolute.is_finite()
            || absolute < 0.
            || !self.relative_tolerance.is_finite()
            || self.relative_tolerance < 0.
        {
            return Err(Error::Domain("invalid residual comparison".into()));
        }
        if !predicted.is_finite() {
            return Ok(Convergence::Retry);
        }
        let scale = predicted.abs().max(actual.abs());
        if scale == 0.
            || (predicted / scale - actual / scale).abs()
                <= self.relative_tolerance + absolute / scale
        {
            Ok(Convergence::Satisfied)
        } else {
            Ok(Convergence::Retry)
        }
    }
}

fn check_coordinates(descriptor: &Descriptor, coordinates: &[f64]) -> Result<()> {
    if coordinates.len() != descriptor.equations.len() {
        return Err(Error::Shape("trial coordinates"));
    }
    if coordinates.iter().any(|value| !value.is_finite()) {
        return Err(Error::Domain("nonfinite trial coordinate".into()));
    }
    Ok(())
}

fn check_physical_coordinates(descriptor: &Descriptor, coordinates: &[f64]) -> Result<()> {
    check_coordinates(descriptor, coordinates)?;
    if descriptor
        .equations
        .iter()
        .zip(coordinates)
        .any(|(equation, value)| equation.kind == EquationKind::Thermal && *value <= 0.)
    {
        return Err(Error::Domain(
            "nonpositive absolute trial temperature".into(),
        ));
    }
    Ok(())
}

pub(super) fn linearize<K: Kernel>(
    artifact: &Artifact<K>,
    prepared: &K::Prepared,
    input: TrialInput<'_>,
    coordinates: &mut [f64],
) -> Result<TrialOutcome> {
    let descriptor = artifact.descriptor();
    check_coordinates(descriptor, input.proposed)?;
    if coordinates.len() != input.proposed.len() {
        return Err(Error::Shape("trial output coordinates"));
    }
    coordinates.copy_from_slice(input.proposed);
    let outcome = match input.phase {
        TrialPhase::ColdStart if descriptor.trial.initialization == TrialPolicy::Kernel => {
            artifact.kernel.initialize_trial(prepared, coordinates)?;
            TrialOutcome::Initialized
        }
        TrialPhase::Newton { previous } => limit(
            artifact,
            prepared,
            input.proposed,
            TrialReference::PreviousNewton(previous),
            coordinates,
        )?,
        TrialPhase::Predictor { accepted } => limit(
            artifact,
            prepared,
            input.proposed,
            TrialReference::AcceptedStep(accepted),
            coordinates,
        )?,
        _ => TrialOutcome::Exact,
    };
    check_physical_coordinates(descriptor, coordinates)?;
    Ok(outcome)
}

fn limit<K: Kernel>(
    artifact: &Artifact<K>,
    prepared: &K::Prepared,
    proposed: &[f64],
    reference: TrialReference<'_>,
    coordinates: &mut [f64],
) -> Result<TrialOutcome> {
    check_physical_coordinates(artifact.descriptor(), reference.coordinates())?;
    if artifact.descriptor().trial.limiting == TrialPolicy::Kernel {
        artifact
            .kernel
            .limit_trial(prepared, reference, coordinates)?;
    }
    Ok(
        if coordinates
            .iter()
            .zip(proposed)
            .any(|(actual, raw)| actual.to_bits() != raw.to_bits())
        {
            TrialOutcome::Limited
        } else {
            TrialOutcome::Exact
        },
    )
}

fn check_sample(descriptor: &Descriptor, sample: TrialSample<'_>) -> Result<()> {
    check_physical_coordinates(descriptor, sample.coordinates)?;
    let width = descriptor.equations.len();
    if sample.residual.len() != width || sample.jacobian.len() != width * width {
        return Err(Error::Shape("convergence sample"));
    }
    if sample
        .residual
        .iter()
        .chain(sample.jacobian)
        .any(|value| !value.is_finite())
    {
        return Err(Error::Domain("nonfinite convergence sample".into()));
    }
    Ok(())
}

pub(super) fn convergence<K: Kernel>(
    artifact: &Artifact<K>,
    prepared: &K::Prepared,
    input: ConvergenceInput<'_>,
) -> Result<Convergence> {
    let descriptor = artifact.descriptor();
    check_sample(descriptor, input.previous)?;
    check_sample(descriptor, input.candidate)?;
    if input.absolute_tolerances.len() != descriptor.equations.len() {
        return Err(Error::Shape("residual tolerances"));
    }
    if !input.relative_tolerance.is_finite()
        || input.relative_tolerance < 0.
        || input
            .absolute_tolerances
            .iter()
            .any(|value| !value.is_finite() || *value < 0.)
    {
        return Err(Error::Domain("invalid convergence tolerance".into()));
    }
    if input.candidate_outcome != TrialOutcome::Exact {
        return Ok(Convergence::Retry);
    }
    match descriptor.trial.convergence {
        TrialPolicy::Engine => Ok(Convergence::Satisfied),
        TrialPolicy::Kernel => artifact.kernel.trial_convergence(prepared, input),
    }
}

impl<K: Kernel> Artifact<K> {
    /// Prepare one local linearization point with caller-owned output.
    /// On failure discard the output; no history is stored in the artifact.
    ///
    /// # Errors
    /// Foreign instance, shape, invalid coordinate or unavailable family policy.
    pub fn linearize_trial(
        &self,
        instance: &Prepared<K>,
        input: TrialInput<'_>,
        coordinates: &mut [f64],
    ) -> Result<TrialOutcome> {
        self.check(instance)?;
        linearize(self, &instance.data, input, coordinates)
    }

    /// Apply only the additional declared family convergence criterion.
    /// Always reject initialized/limited candidates even when no extra
    /// family criterion is declared.
    ///
    /// # Errors
    /// Foreign instance, invalid sample or invalid tolerances.
    pub fn check_trial_convergence(
        &self,
        instance: &Prepared<K>,
        input: ConvergenceInput<'_>,
    ) -> Result<Convergence> {
        self.check(instance)?;
        convergence(self, &instance.data, input)
    }
}
