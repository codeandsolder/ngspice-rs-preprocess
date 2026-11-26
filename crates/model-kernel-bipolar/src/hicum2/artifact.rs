//! Native HICUM/L2 2.4.0 solver-facing prepared artifact.
//!
//! Compiling a scalar model card establishes immutable parameter identity.
//! Binding scales and freezes independent instance geometry and topology.
//! Trial evaluation borrows solver-owned local unknowns and returns the
//! physical I/Q and complete electrothermal G/C Jacobians, without runtime
//! parsing, dispatch, allocation or accepted-step state mutation.
//! The circuit owns netlist syntax, global sparse matrix, accepted state,
//! analysis policy, integration and frequency/noise transfer.

use super::{
    binding, device, instance, nodal, nqs,
    parameters::{Id, SPECIFICATIONS},
    temperature,
    topology::{Node, Resistor},
};
use crate::{EvaluationError, ModelError, Temperature};
use sha2::{Digest, Sha256};

pub const FAMILY: &str = "hicum2";
pub const SOURCE_VERSION: &str = "2.4.0";
pub const SCHEMA_REVISION: u16 = 1;

/// SHA-256 of family, source version, semantic schema and the source 136
/// distinct named parameter values in canonical little-endian bit form.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SemanticDigest(pub [u8; 32]);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceFormat {
    NativeSpiceModel,
    VerilogA,
    Normalized,
}

/// Digest of actual input content, supplied by the upstream converter.
/// No source digest is manufactured when provenance was not retained.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Provenance {
    pub format: SourceFormat,
    pub source_sha256: Option<[u8; 32]>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalPhysics {
    /// Source I, Q, dI/dV, dQ/dV, and local noise PSD are present.
    CurrentsChargesNoise,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Electrothermal {
    /// Source thermal branch is structurally absent.
    NotPresent,
    /// Complete coupled thermal residual and Jacobian.
    Computed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capabilities {
    pub local: LocalPhysics,
    pub vertical_nqs: bool,
    pub electrothermal: Electrothermal,
}

/// Five resistor-presence bits, a vertical NQS flag and a thermal flag.
/// It is a structure identity, not the physics parameter content hash.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StructureKey(pub u8);

impl StructureKey {
    #[must_use = "the returned key identifies the structural topology"]
    pub const fn from_layout(layout: nodal::Layout) -> Self {
        let topology = layout.topology();
        let mut mask = 0u8;
        let mut index = 0;
        while index < Resistor::ALL.len() {
            if topology.present(Resistor::ALL[index]) {
                mask |= 1 << index;
            }
            index += 1;
        }
        if topology.nqs_state_count() > 0 {
            mask |= 1 << 5;
        }
        if layout.thermal_slot().is_some() {
            mask |= 1 << 6;
        }
        Self(mask)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct InstanceIdentity {
    pub model: SemanticDigest,
    pub scaled: SemanticDigest,
    pub ambient_kelvin_bits: u64,
    pub structure: StructureKey,
}

/// Parser-facing semantic artifact error: the ordered source
/// assignment index stays inside a Binding failure, while invalid
/// already-typed coefficients retain their physical model validation error.
#[derive(Clone, Debug, PartialEq)]
pub enum ArtifactError {
    Binding(binding::Error),
    Model(ModelError),
}

impl std::fmt::Display for ArtifactError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Binding(source) => source.fmt(f),
            Self::Model(source) => source.fmt(f),
        }
    }
}

impl std::error::Error for ArtifactError {}

/// Once-only typed nominal source model, shared across independent instances.
#[derive(Clone, Copy, Debug)]
pub struct Model {
    nominal: temperature::Nominal,
    digest: SemanticDigest,
    provenance: Option<Provenance>,
}

impl Model {
    /// Check every published source parameter range, resolve structural
    /// gates once, and construct source-versioned semantic identity.
    ///
    /// # Errors
    /// Invalid source coefficient or topology.
    pub fn new(
        nominal: &temperature::Nominal,
        provenance: Option<Provenance>,
    ) -> Result<Self, ModelError> {
        nominal.physical_topology()?;
        let digest = digest_nominal(nominal, true)?;
        Ok(Self {
            nominal: *nominal,
            digest,
            provenance,
        })
    }

    /// Direct semantic ingestion boundary for the existing ngspice
    /// model-card AST after scalar/expression resolution. This
    /// performs model-card setter checks exactly once, not at runtime.
    ///
    /// # Errors
    /// Source setter error includes the original assignment index.
    pub fn from_resolved_model_card(
        base: &str,
        level: Option<f64>,
        assignments: &[binding::Assignment<'_>],
        provenance: Option<Provenance>,
    ) -> Result<Self, ArtifactError> {
        let nominal = binding::model(base, level, assignments).map_err(ArtifactError::Binding)?;
        Self::new(&nominal, provenance).map_err(ArtifactError::Model)
    }

    /// Bind already-resolved ordered Q instance scalar, flag, and
    /// IC-vector settings through the same source instance semantics.
    ///
    /// # Errors
    /// Bad setter kind/name or physical instance geometry.
    pub fn bind_resolved_instance(
        &self,
        global_ambient: Temperature,
        assignments: &[binding::Assignment<'_>],
    ) -> Result<Instance, ArtifactError> {
        let bound = binding::instance(&self.nominal, global_ambient, assignments)
            .map_err(ArtifactError::Binding)?;
        self.materialize_instance(&bound)
            .map_err(ArtifactError::Model)
    }

    #[must_use = "the returned identity identifies the artifact or instance"]
    pub const fn identity(&self) -> SemanticDigest {
        self.digest
    }

    #[must_use = "the result distinguishes absent from recorded source provenance"]
    pub const fn provenance(&self) -> Option<Provenance> {
        self.provenance
    }

    /// Independently scale AREA/M, ambient, and source-short topology.
    ///
    /// # Errors
    /// Invalid instance scaling, source bounds or temperature.
    pub fn bind(
        &self,
        global_ambient: Temperature,
        options: instance::Options,
    ) -> Result<Instance, ModelError> {
        let bound = instance::Bound::new(&self.nominal, global_ambient, options)?;
        self.materialize_instance(&bound)
    }

    fn materialize_instance(&self, bound: &instance::Bound) -> Result<Instance, ModelError> {
        let layout = nodal::Layout::new(bound.topology);
        let identity = InstanceIdentity {
            model: self.digest,
            // AREA/M transforms can legally exceed model-card input bounds.
            // The scaled material remains fully finite and source-checked
            // by instance::Bound; only *input* coefficients use VA ranges.
            scaled: digest_nominal(&bound.nominal, false)?,
            ambient_kelvin_bits: bound.ambient.kelvin().to_bits(),
            structure: StructureKey::from_layout(layout),
        };
        Ok(Instance {
            bound: *bound,
            layout,
            identity,
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Instance {
    bound: instance::Bound,
    layout: nodal::Layout,
    identity: InstanceIdentity,
}

impl Instance {
    #[must_use = "the returned identity identifies the artifact or instance"]
    pub const fn identity(&self) -> InstanceIdentity {
        self.identity
    }

    #[must_use = "the returned layout determines the allocation and stamping of unknowns"]
    pub const fn layout(&self) -> nodal::Layout {
        self.layout
    }

    #[must_use = "the returned voltages initialize the device state"]
    pub const fn initial_voltages(&self) -> instance::InitialVoltages {
        self.bound.initial
    }

    #[must_use = "the flag controls initial device conduction"]
    pub const fn off(&self) -> bool {
        self.bound.off
    }

    #[must_use = "the returned capabilities determine solver integration"]
    pub const fn capabilities(&self) -> Capabilities {
        Capabilities {
            local: LocalPhysics::CurrentsChargesNoise,
            vertical_nqs: self.layout.topology().nqs_state_count() > 0,
            electrothermal: if self.layout.thermal_slot().is_some() {
                Electrothermal::Computed
            } else {
                Electrothermal::NotPresent
            },
        }
    }

    /// Absolute unclamped zero-rise temperature, including model and
    /// instance offsets. The constitutive temperature clamp is applied
    /// inside preparation, not to the thermal-network coordinate.
    #[must_use = "the value maps absolute thermal coordinates to source rise"]
    pub fn thermal_reference_kelvin(&self) -> f64 {
        self.bound.ambient.kelvin() + self.bound.nominal.coefficients.dt
    }

    /// Prepare a temperature-dependent physical kernel once per temperature
    /// candidate. A nonthermal instance cannot accept a thermal rise.
    ///
    /// # Errors
    /// Nonphysical effective coefficients or thermal rise.
    pub fn prepare(&self, thermal_rise: f64) -> Result<Prepared, ModelError> {
        if self.layout.thermal_slot().is_none() && thermal_rise.to_bits() << 1 != 0 {
            return Err(ModelError::InvalidParameter("thermal_rise"));
        }
        let kernel = self.bound.prepare(thermal_rise)?.device_kernel()?;
        Ok(Prepared {
            layout: self.layout,
            kernel,
            thermal_rise,
        })
    }
}

/// Trial voltages are in exact source reduced C/B/E/S, hidden, NQS,
/// optional thermal order, with one element per physical unknown.
#[derive(Clone, Copy, Debug)]
pub struct Trial<'a> {
    pub unknowns: &'a [f64],
}

#[derive(Clone, Copy, Debug)]
pub struct Frame {
    pub operating: device::Output,
    pub local: nodal::Complete,
}

#[derive(Clone, Copy, Debug)]
pub struct Prepared {
    layout: nodal::Layout,
    kernel: device::Prepared,
    thermal_rise: f64,
}

impl Prepared {
    #[must_use = "the returned layout determines the allocation and stamping of unknowns"]
    pub const fn layout(&self) -> nodal::Layout {
        self.layout
    }

    /// Borrowed trial evaluation; analytically completes implicit transit
    /// time chain rule and the complete live thermal derivative.
    ///
    /// # Errors
    /// Wrong vector size, stale thermal coefficients or failed physical
    /// law or coupled derivative.
    pub fn evaluate(&self, trial: Trial<'_>) -> Result<Frame, EvaluationError> {
        if trial.unknowns.len() != self.layout.count() {
            return Err(EvaluationError::LengthMismatch);
        }
        let values = trial.unknowns;
        if !values.iter().all(|v| v.is_finite()) {
            return Err(EvaluationError::NonFiniteBias);
        }
        if let Some(slot) = self.layout.thermal_slot()
            && values[slot].to_bits() != self.thermal_rise.to_bits()
            && !(values[slot].to_bits() << 1 == 0 && self.thermal_rise.to_bits() << 1 == 0)
        {
            return Err(EvaluationError::InvalidPhysicalDomain);
        }
        let voltage = |node| values[self.layout.node(node)];
        let nqs = nqs::State {
            current_first: self.layout.nqs_slot(0).map_or(0.0, |index| values[index]),
            current_second: self.layout.nqs_slot(1).map_or(0.0, |index| values[index]),
            stored_charge: self.layout.nqs_slot(2).map_or(0.0, |index| values[index]),
        };
        let bias = device::Bias {
            nodes: device::Nodes {
                c: voltage(Node::C),
                b: voltage(Node::B),
                e: voltage(Node::E),
                s: voltage(Node::S),
                ci: voltage(Node::Ci),
                ei: voltage(Node::Ei),
                bp: voltage(Node::Bp),
                bi: voltage(Node::Bi),
                si: voltage(Node::Si),
            },
            nqs,
        };
        let operating = self.kernel.evaluate(&bias)?;
        let source = self.kernel.source_stamps(&operating)?;
        let thermal = if self.layout.thermal_slot().is_some() {
            Some(self.kernel.temperature_coupling(self.layout, &operating)?)
        } else {
            None
        };
        let local = self
            .layout
            .reduce(&source)?
            .complete_from_operating(&operating, thermal)?;
        Ok(Frame { operating, local })
    }

    /// Homogeneous borrowed batch; source trial states are never accepted
    /// implicitly. Earlier output slots remain written on a later failure.
    ///
    /// # Errors
    /// Length mismatch or first rejected physical trial.
    pub fn evaluate_batch(
        &self,
        inputs: &[Trial<'_>],
        outputs: &mut [Option<Frame>],
    ) -> Result<(), EvaluationError> {
        if inputs.len() != outputs.len() {
            return Err(EvaluationError::LengthMismatch);
        }
        for (trial, output) in inputs.iter().zip(outputs) {
            *output = Some(self.evaluate(*trial)?);
        }
        Ok(())
    }

    /// The actual local source noise PSD/covariance data; noise transfer
    /// and global integration belong to the external circuit solver.
    ///
    /// # Errors
    /// Invalid frequency or source numerical output.
    pub fn evaluate_noise(
        &self,
        operating: &device::Output,
        frequency_hz: f64,
    ) -> Result<super::noise::Output, EvaluationError> {
        self.kernel.evaluate_noise(operating, frequency_hz)
    }
}

fn digest_nominal(
    nominal: &temperature::Nominal,
    enforce_source_card_bounds: bool,
) -> Result<SemanticDigest, ModelError> {
    let mut hasher = Sha256::new();
    hasher.update(b"model-kernel-bipolar/semantic-model");
    hasher.update(FAMILY.as_bytes());
    hasher.update([0]);
    hasher.update(SOURCE_VERSION.as_bytes());
    hasher.update([0]);
    hasher.update(SCHEMA_REVISION.to_le_bytes());
    for (index, specification) in SPECIFICATIONS.iter().enumerate() {
        let id = Id::at(index).ok_or(ModelError::InvalidParameter("parameter_slot"))?;
        let value = nominal.parameter(id);
        if !value.is_finite() || (enforce_source_card_bounds && !specification.accepts(value)) {
            return Err(ModelError::InvalidParameter(specification.name));
        }
        hasher.update(specification.name.as_bytes());
        hasher.update([0]);
        // IEEE -0.0 and +0.0 are the same source coefficient under
        // every native HICUM parameter comparison and physical law.
        // Keep physically equivalent decks in the same model cache.
        let bits = if value.to_bits() << 1 == 0 {
            0u64
        } else {
            value.to_bits()
        };
        hasher.update(bits.to_le_bytes());
    }
    Ok(SemanticDigest(hasher.finalize().into()))
}
