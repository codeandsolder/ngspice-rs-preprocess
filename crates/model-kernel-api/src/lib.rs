//! Native prepared compact-model contract. No circuit rows, source grammar,
//! integration method, allocation in evaluation, or mutable history belongs here.
//!
//! A model supplies F(x), Q(x), and their exact Jacobians for the generalized
//! residual F(x) + dQ(x)/dt = 0. The consumer binds physical coordinates to
//! circuit unknowns and owns accepted/trial history and numerical integration.

#![forbid(unsafe_code)]

use std::fmt::{Debug, Display, Formatter};
use std::sync::Arc;

pub mod batch;
pub mod noise;
pub mod trial;

/// Interface revision, independent of any model-family revision.
pub const ABI_VERSION: u32 = 4;

/// A missing contribution is not a known physical zero.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Capability {
    /// Identically zero.
    Zero,
    /// Supplied by the kernel.
    Computed,
    /// Analysis requiring this contribution must reject the model.
    Unavailable,
}

/// Equation and coordinate unit domain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EquationKind {
    /// Electrical potential (V), KCL (A), stored charge (C).
    Electrical,
    /// Absolute temperature (K), heat balance (W), stored energy (J).
    Thermal,
    /// Family-defined physical state, residual and stored quantity.
    Internal,
}

/// Whether the caller must bind a public terminal or allocate a local state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Binding {
    /// Exposed to the containing circuit.
    Terminal,
    /// Internal to one device instance.
    Hidden,
}

/// An ordered coordinate, including the topology binding requirement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Equation {
    pub name: String,
    pub kind: EquationKind,
    pub binding: Binding,
}

/// An ordered numeric parameter, in the units of the model contract.
#[derive(Clone, Debug, PartialEq)]
pub struct Parameter {
    pub name: String,
    pub default: f64,
    /// A changed structural slot requires a new artifact and row binding.
    pub changes_topology: bool,
}

/// Model metadata, frozen when an artifact is constructed.
#[derive(Clone, Debug, PartialEq)]
pub struct Descriptor {
    pub family: String,
    /// Stable producer-defined identity of the compiled model and topology.
    pub fingerprint: [u8; 16],
    pub abi: u32,
    pub equations: Vec<Equation>,
    pub parameters: Vec<Parameter>,
    pub charge: Capability,
    pub noise: Capability,
    pub trial: trial::TrialCapabilities,
}

/// Failures carry model or interface context, never solver-specific types.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Error {
    Schema(&'static str),
    Shape(&'static str),
    Domain(String),
    Unsupported(&'static str),
    Conservation,
    Allocation,
    ForeignArtifact,
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Schema(message) => write!(f, "kernel schema: {message}"),
            Self::Shape(message) => write!(f, "kernel shape: {message}"),
            Self::Domain(message) => write!(f, "kernel domain: {message}"),
            Self::Unsupported(message) => write!(f, "kernel unsupported: {message}"),
            Self::Conservation => f.write_str("kernel electrical conservation or gauge invariance"),
            Self::Allocation => f.write_str("kernel workspace allocation failed"),
            Self::ForeignArtifact => {
                f.write_str("prepared instance belongs to a different artifact")
            }
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

impl Descriptor {
    /// Validate metadata once, before the numerical path.
    ///
    /// # Errors
    /// Bad version, identity, dimensions, slot names or parameter defaults.
    pub fn validate(&self) -> Result<()> {
        let n = self.equations.len();
        if self.abi != ABI_VERSION
            || !valid_name(&self.family)
            || self.fingerprint == [0; 16]
            || n.checked_mul(n).is_none()
            || self
                .equations
                .iter()
                .filter(|e| e.kind == EquationKind::Electrical)
                .count()
                < 2
        {
            return Err(Error::Schema("version, identity or equation dimensions"));
        }
        for (i, row) in self.equations.iter().enumerate() {
            if !valid_name(&row.name)
                || self.equations[..i]
                    .iter()
                    .any(|other| other.name == row.name)
            {
                return Err(Error::Schema("invalid or duplicate coordinate"));
            }
        }
        for (i, slot) in self.parameters.iter().enumerate() {
            if !valid_name(&slot.name)
                || !slot.default.is_finite()
                || self.parameters[..i]
                    .iter()
                    .any(|other| other.name == slot.name)
            {
                return Err(Error::Schema("invalid or duplicate parameter"));
            }
        }
        Ok(())
    }
}

fn allocate(len: usize) -> Result<Vec<f64>> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(len)
        .map_err(|_| Error::Allocation)?;
    values.resize(len, 0.);
    Ok(values)
}

/// A borrowed row-major output. All coordinates and equations use descriptor order.
pub struct Point<'a> {
    pub current: &'a mut [f64],
    pub conductance: &'a mut [f64],
    pub charge: &'a mut [f64],
    pub capacitance: &'a mut [f64],
}

fn balanced(values: impl Iterator<Item = f64> + Clone) -> bool {
    // Scale BEFORE summation: two near-MAX terms must not overflow the
    // conservation tolerance into infinity and accidentally pass.
    let max = values
        .clone()
        .fold(0_f64, |max, value| max.max(value.abs()));
    if max == 0. {
        return true;
    }
    let (sum, norm) = values.fold((0_f64, 0_f64), |(sum, norm), value| {
        let normalized = value / max;
        (sum + normalized, norm + normalized.abs())
    });
    sum.abs() <= 1e-12 * norm
}

impl Point<'_> {
    /// Initialize complete output arrays, including known-zero storage.
    pub fn reset(&mut self, charge: Capability) {
        self.current.fill(f64::NAN);
        self.conductance.fill(f64::NAN);
        let initial = if charge == Capability::Zero {
            0.
        } else {
            f64::NAN
        };
        self.charge.fill(initial);
        self.capacitance.fill(initial);
    }

    /// Check finite and complete output, electrical KCL/storage conservation
    /// and electrical reference invariance of every physical equation.
    ///
    /// # Errors
    /// Shape, incomplete output, declared-zero violation or conservation.
    pub fn validate(&self, descriptor: &Descriptor) -> Result<()> {
        let n = descriptor.equations.len();
        let entries = n.checked_mul(n).ok_or(Error::Shape("point dimensions"))?;
        if self.current.len() != n
            || self.charge.len() != n
            || self.conductance.len() != entries
            || self.capacitance.len() != entries
        {
            return Err(Error::Shape("point dimensions"));
        }
        let electrical = || {
            descriptor
                .equations
                .iter()
                .enumerate()
                .filter(|(_, row)| row.kind == EquationKind::Electrical)
                .map(|(index, _)| index)
        };
        let finite = |values: &[f64]| values.iter().all(|value| value.is_finite());
        let conserved = |values: &[f64], derivatives: &[f64]| {
            balanced(electrical().map(|row| values[row]))
                && (0..n).all(|col| balanced(electrical().map(|row| derivatives[row * n + col])))
                && (0..n).all(|row| balanced(electrical().map(|col| derivatives[row * n + col])))
        };
        if !finite(self.current) || !finite(self.conductance) {
            return Err(Error::Domain(
                "incomplete static residual or Jacobian".into(),
            ));
        }
        if !conserved(self.current, self.conductance) {
            return Err(Error::Conservation);
        }
        if descriptor.charge != Capability::Unavailable {
            if !finite(self.charge) || !finite(self.capacitance) {
                return Err(Error::Domain("incomplete storage or Jacobian".into()));
            }
            if descriptor.charge == Capability::Zero
                && self
                    .charge
                    .iter()
                    .chain(self.capacitance.iter())
                    .any(|v| *v != 0.)
            {
                return Err(Error::Domain("nonzero declared-zero storage".into()));
            }
            if !conserved(self.charge, self.capacitance) {
                return Err(Error::Conservation);
            }
        }
        Ok(())
    }
}

/// Reusable point output storage, allocated outside evaluation.
#[derive(Debug)]
pub struct PointBuffer {
    current: Vec<f64>,
    conductance: Vec<f64>,
    charge: Vec<f64>,
    capacitance: Vec<f64>,
}

impl PointBuffer {
    /// # Errors
    /// Dimension overflow or allocation failure.
    pub fn new(width: usize) -> Result<Self> {
        let n = width
            .checked_mul(width)
            .ok_or(Error::Shape("point dimensions"))?;
        Ok(Self {
            current: allocate(width)?,
            conductance: allocate(n)?,
            charge: allocate(width)?,
            capacitance: allocate(n)?,
        })
    }

    pub fn point(&mut self) -> Point<'_> {
        Point {
            current: &mut self.current,
            conductance: &mut self.conductance,
            charge: &mut self.charge,
            capacitance: &mut self.capacitance,
        }
    }
}

/// Family-specific physical equations, statically dispatched over typed state.
pub trait Kernel: Debug + Send + Sync + 'static {
    type Prepared: Debug + Send + Sync + 'static;

    /// Derive constants outside Newton, using absolute Kelvin.
    ///
    /// # Errors
    /// Invalid or unsupported model/instance parameters.
    fn prepare(&self, values: &[f64], ambient_kelvin: f64) -> Result<Self::Prepared>;

    /// Fill every declared residual and derivative; do not mutate history.
    ///
    /// # Errors
    /// Invalid physical state or output dimensions.
    fn evaluate(
        &self,
        prepared: &Self::Prepared,
        coordinates: &[f64],
        out: &mut Point<'_>,
    ) -> Result<()>;

    /// Homogeneous lane-major batch; families may specialize this for SIMD.
    ///
    /// # Errors
    /// Shape or any lane's physics failure. Do not consume a failed batch.
    fn evaluate_batch(
        &self,
        prepared: &[Self::Prepared],
        coordinates: &[f64],
        out: &mut batch::BatchOutput,
    ) -> Result<()> {
        batch::evaluate_scalar_tile(self, prepared, coordinates, out)
    }

    /// Modify a cold-start local point in place, without retaining history.
    /// Called only when the descriptor declares this family capability.
    ///
    /// # Errors
    /// Invalid initialization or an unsupported declared policy.
    fn initialize_trial(&self, _prepared: &Self::Prepared, _coordinates: &mut [f64]) -> Result<()> {
        Err(Error::Unsupported("trial initialization"))
    }

    /// Limit a proposal against the selected immutable evaluated reference.
    /// The engine distinguishes previous Newton from accepted-step reference.
    ///
    /// # Errors
    /// Invalid local point or an unsupported declared policy.
    fn limit_trial(
        &self,
        _prepared: &Self::Prepared,
        _reference: trial::TrialReference<'_>,
        _coordinates: &mut [f64],
    ) -> Result<()> {
        Err(Error::Unsupported("trial limiting"))
    }

    /// Evaluate an additional family-specific static convergence criterion.
    ///
    /// # Errors
    /// Invalid physical sample or an unsupported declared policy.
    fn trial_convergence(
        &self,
        _prepared: &Self::Prepared,
        _input: trial::ConvergenceInput<'_>,
    ) -> Result<trial::Convergence> {
        Err(Error::Unsupported("trial convergence"))
    }

    /// Enumerate independent source generators at a solved point. No circuit
    /// node IDs or adjoint transfer functions cross the boundary.
    ///
    /// # Errors
    /// Unsupported noise or invalid physics.
    fn evaluate_noise(
        &self,
        _prepared: &Self::Prepared,
        _coordinates: &[f64],
        _frequency: f64,
        _sink: &mut dyn noise::Sink,
    ) -> Result<()> {
        Err(Error::Unsupported("noise"))
    }
}

/// Immutable validated model identity and its family implementation.
#[derive(Debug)]
pub struct Artifact<K: Kernel> {
    descriptor: Descriptor,
    kernel: K,
}

/// Instance data is tied to the exact artifact that produced it, not merely
/// to a matching family name or even a matching content fingerprint.
#[derive(Debug)]
pub struct Prepared<K: Kernel> {
    artifact: Arc<Artifact<K>>,
    data: K::Prepared,
}

impl<K: Kernel> Artifact<K> {
    /// # Errors
    /// Invalid metadata.
    pub fn new(descriptor: Descriptor, kernel: K) -> Result<Arc<Self>> {
        descriptor.validate()?;
        Ok(Arc::new(Self { descriptor, kernel }))
    }

    #[must_use = "the descriptor defines the artifact binding contract"]
    pub const fn descriptor(&self) -> &Descriptor {
        &self.descriptor
    }

    /// Prepare a candidate without editing another candidate's data or history.
    ///
    /// # Errors
    /// Invalid slots, temperature, topology-changing edit or family domain.
    pub fn prepare(self: &Arc<Self>, values: &[f64], ambient_kelvin: f64) -> Result<Prepared<K>> {
        Ok(Prepared {
            artifact: Arc::clone(self),
            data: self.prepare_data(values, ambient_kelvin)?,
        })
    }

    fn prepare_data(&self, values: &[f64], ambient_kelvin: f64) -> Result<K::Prepared> {
        if values.len() != self.descriptor.parameters.len() {
            return Err(Error::Shape("parameter count"));
        }
        if !ambient_kelvin.is_finite()
            || ambient_kelvin <= 0.
            || values.iter().any(|value| !value.is_finite())
        {
            return Err(Error::Domain("parameter or ambient temperature".into()));
        }
        if self
            .descriptor
            .parameters
            .iter()
            .zip(values)
            .any(|(slot, value)| slot.changes_topology && slot.default.to_bits() != value.to_bits())
        {
            return Err(Error::Unsupported(
                "structural parameter edit requires new artifact",
            ));
        }
        self.kernel.prepare(values, ambient_kelvin)
    }

    fn check(&self, instance: &Prepared<K>) -> Result<()> {
        if !std::ptr::eq(self, Arc::as_ptr(&instance.artifact)) {
            return Err(Error::ForeignArtifact);
        }
        Ok(())
    }

    fn check_coordinates(&self, coordinates: &[f64]) -> Result<()> {
        if coordinates.len() != self.descriptor.equations.len() {
            return Err(Error::Shape("coordinate count"));
        }
        if coordinates.iter().any(|value| !value.is_finite()) {
            return Err(Error::Domain("nonfinite coordinate".into()));
        }
        Ok(())
    }

    /// Evaluate one instance into reusable buffers.
    ///
    /// # Errors
    /// Artifact mismatch, input shape, domain or invalid output.
    pub fn evaluate(
        &self,
        instance: &Prepared<K>,
        coordinates: &[f64],
        out: &mut Point<'_>,
    ) -> Result<()> {
        self.check(instance)?;
        self.evaluate_data(&instance.data, coordinates, out)
    }

    fn evaluate_data(
        &self,
        data: &K::Prepared,
        coordinates: &[f64],
        out: &mut Point<'_>,
    ) -> Result<()> {
        self.check_coordinates(coordinates)?;
        let n = coordinates.len();
        if out.current.len() != n
            || out.charge.len() != n
            || out.conductance.len() != n * n
            || out.capacitance.len() != n * n
        {
            return Err(Error::Shape("point dimensions"));
        }
        out.reset(self.descriptor.charge);
        self.kernel.evaluate(data, coordinates, out)?;
        out.validate(&self.descriptor)
    }

    /// Evaluate independent source noise using validated family-local ordinals.
    ///
    /// # Errors
    /// Artifact mismatch, invalid bias/frequency or unsupported/invalid noise.
    pub fn evaluate_noise(
        &self,
        instance: &Prepared<K>,
        coordinates: &[f64],
        frequency: f64,
        sink: &mut dyn noise::Sink,
    ) -> Result<()> {
        self.check(instance)?;
        self.evaluate_noise_data(&instance.data, coordinates, frequency, sink)
    }

    fn evaluate_noise_data(
        &self,
        data: &K::Prepared,
        coordinates: &[f64],
        frequency: f64,
        sink: &mut dyn noise::Sink,
    ) -> Result<()> {
        self.check_coordinates(coordinates)?;
        if !frequency.is_finite() || frequency <= 0. {
            return Err(Error::Domain("noise frequency".into()));
        }
        match self.descriptor.noise {
            Capability::Zero => Ok(()),
            Capability::Unavailable => Err(Error::Unsupported("noise")),
            Capability::Computed => {
                let mut checked = noise::CheckedSink::new(&self.descriptor, sink);
                self.kernel
                    .evaluate_noise(data, coordinates, frequency, &mut checked)
            }
        }
    }
}
/// Model preparation or runtime domain failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModelError {
    /// Invalid input or nonrepresentable derived value.
    Invalid(&'static str),
    /// Named, currently unsupported physical model feature.
    Unsupported(&'static str),
}

impl std::fmt::Display for ModelError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(name) => write!(formatter, "invalid model parameter or bias: {name}"),
            Self::Unsupported(name) => write!(formatter, "unsupported model physics: {name}"),
        }
    }
}

impl std::error::Error for ModelError {}
