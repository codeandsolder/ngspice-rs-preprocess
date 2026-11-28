//! Solver-independent BSIMSOI 4.4 prepared artifact and ordered model groups.
//! Copyright 2010 Regents of the University of California. See LICENSE.
mod evaluate;
mod layout;
mod stamp;

use layout::Layout;
use model_compact_intrinsic::{
    BoundParameters, BsimSoiInstanceInput, BsimSoiNode, ModelVariant, ParameterScope,
    ParameterSpec, ParameterValue, PreparedBsimSoiDrainChannel, PreparedBsimSoiExtrinsicCharges,
    PreparedBsimSoiGateInducedLeakage, PreparedBsimSoiGateTunneling,
    PreparedBsimSoiImpactIonization, PreparedBsimSoiIntrinsicCharges, PreparedBsimSoiJunctions,
    PreparedBsimSoiModel, PreparedBsimSoiNoise, PreparedBsimSoiParasitics, PreparedBsimSoiSetup,
    PreparedBsimSoiTransport, prepare_bsim_soi_setup,
};
use model_kernel_api::{
    ABI_VERSION, Artifact, Capability, Descriptor, Error, Kernel, Parameter, Point, Result, noise,
};
use stamp::Biases;
use std::sync::Arc;

/// Source instance inputs owned by the immutable model group.
#[derive(Clone, Debug)]
pub struct Instance {
    pub parameters: BoundParameters,
    pub terminal_count: u8,
    pub geometry_scale: f64,
}

impl Instance {
    const fn input(&self) -> BsimSoiInstanceInput<'_> {
        BsimSoiInstanceInput {
            parameters: &self.parameters,
            terminal_count: self.terminal_count,
            geometry_scale: self.geometry_scale,
        }
    }

    fn scale(&self, name: &str) -> f64 {
        match name {
            "l" | "w" | "pd" | "ps" => self.geometry_scale,
            "ad" | "as" => self.geometry_scale.powi(2),
            _ => 1.,
        }
    }

    fn external_thermal(&self, topology: model_compact_intrinsic::BsimSoiTopology) -> Option<u8> {
        if let BsimSoiNode::External(index) = topology.temperature {
            return Some(index);
        }
        let given = ModelVariant::BsimSoi
            .parameter(ParameterScope::Instance, "tnodeout")
            .is_some_and(|spec| self.parameters.get_slot(spec.slot).is_some());
        if self.terminal_count == 7
            || (given && topology.body != BsimSoiNode::Ground && self.terminal_count > 4)
        {
            Some(self.terminal_count - 1)
        } else {
            None
        }
    }
}

#[derive(Clone, Debug)]
struct Slot {
    index: usize,
    spec: ParameterSpec,
    default: f64,
    integer: bool,
}

impl Slot {
    fn new(
        index: usize,
        spec: ParameterSpec,
        source: &Instance,
        resolved: &BoundParameters,
    ) -> Option<Self> {
        // Preserve the raw selector (including auto) and original presence.
        // Real defaults and candidates are exposed in physical SI units.
        let (default, integer) = match (
            source.parameters.get_slot(spec.slot),
            resolved.get_slot(spec.slot),
        ) {
            (Some(ParameterValue::Integer(value)), _)
            | (_, Some(ParameterValue::Integer(value))) => (f64::from(*value), true),
            (_, Some(ParameterValue::Real(value))) => (*value, false),
            _ => return None,
        };
        Some(Self {
            index,
            spec,
            default,
            integer,
        })
    }

    fn parameter(&self) -> Parameter {
        Parameter {
            name: format!("i{}_{}", self.index, self.spec.name),
            default: self.default,
            changes_topology: matches!(self.spec.name, "soimod" | "rgatemod" | "rbodymod"),
        }
    }

    fn apply(&self, value: f64, instances: &mut [Instance]) -> Result<()> {
        if value.to_bits() == self.default.to_bits() {
            return Ok(());
        }
        let source = &mut instances[self.index];
        let value = if self.integer {
            // Conversion is exact after the range and integrality check.
            if value.fract() != 0. || value < f64::from(i32::MIN) || value > f64::from(i32::MAX) {
                return Err(Error::Domain("BSIMSOI integer parameter".into()));
            }
            #[expect(
                clippy::cast_possible_truncation,
                reason = "integral value and i32 range checked above"
            )]
            let exact = value as i32;
            ParameterValue::Integer(exact)
        } else {
            ParameterValue::Real(value / source.scale(self.spec.name))
        };
        source
            .parameters
            .assign(self.spec.name, value)
            .map_err(|error| super::domain(&error))
    }
}

fn setup(model: &PreparedBsimSoiModel, instances: &[Instance]) -> Result<PreparedBsimSoiSetup> {
    let inputs = instances.iter().map(Instance::input).collect::<Vec<_>>();
    prepare_bsim_soi_setup(model, &inputs).map_err(|error| super::domain(&error))
}

/// Immutable group preparation shared by individual artifact descriptors.
///
/// Native preparation mutates model-wide defaults and CV oxide thickness in
/// traversal order. Every candidate contains the entire group's numeric
/// instance slots, so simultaneous geometry edits cannot silently use a
/// stale neighbor's physical preparation. Each artifact selects one instance.
#[derive(Debug)]
pub struct BsimSoiGroup {
    model: Arc<PreparedBsimSoiModel>,
    instances: Vec<Instance>,
    slots: Vec<Slot>,
    reference: PreparedBsimSoiSetup,
}

impl BsimSoiGroup {
    /// Bind a model group in native traversal order, preserving source presence.
    ///
    /// # Errors
    /// Invalid source model or instance topology.
    pub fn new(model: Arc<PreparedBsimSoiModel>, instances: Vec<Instance>) -> Result<Arc<Self>> {
        if instances.is_empty() {
            return Err(Error::Shape("empty BSIMSOI model group"));
        }
        let reference = setup(&model, &instances)?;
        let mut slots = Vec::new();
        for (index, (source, resolved)) in instances.iter().zip(reference.instances()).enumerate() {
            let mut seen = std::collections::HashSet::new();
            for &spec in ModelVariant::BsimSoi.parameters(ParameterScope::Instance) {
                if seen.insert(spec.slot)
                    && let Some(slot) = Slot::new(index, spec, source, resolved.parameters())
                {
                    slots.push(slot);
                }
            }
        }
        Ok(Arc::new(Self {
            model,
            instances,
            slots,
            reference,
        }))
    }

    /// Descriptor-order numeric defaults for every instance in the group.
    /// The final slot is the common solver gmin (S).
    #[must_use = "candidate parameter slots are used for immutable preparation"]
    pub fn defaults(&self, gmin: f64) -> Vec<f64> {
        self.slots
            .iter()
            .map(|slot| slot.default)
            .chain([gmin])
            .collect()
    }

    fn layout(&self, setup: &PreparedBsimSoiSetup, index: usize) -> Result<Layout> {
        let instance = setup
            .instances()
            .get(index)
            .ok_or(Error::Shape("BSIMSOI group index"))?;
        let source = &self.instances[index];
        let topology = instance.topology();
        Layout::new(
            topology,
            source.terminal_count,
            source.external_thermal(topology),
        )
    }

    /// Make an artifact selecting one instance from this group.
    ///
    /// The fingerprint must identify the source model, ordered group and selected
    /// topology. All electrical coordinates are physical volts, and the thermal
    /// coordinate is absolute Kelvin. Candidate topology changes require rebinding.
    ///
    /// # Errors
    /// Bad selected instance, gmin or descriptor identity.
    pub fn artifact(
        self: &Arc<Self>,
        index: usize,
        gmin: f64,
        fingerprint: [u8; 16],
    ) -> Result<Arc<Artifact<BsimSoiKernel>>> {
        if !gmin.is_finite() || gmin < 0. {
            return Err(Error::Domain("BSIMSOI gmin".into()));
        }
        let layout = self.layout(&self.reference, index)?;
        let mut parameters = self.slots.iter().map(Slot::parameter).collect::<Vec<_>>();
        parameters.push(Parameter {
            name: "gmin".into(),
            default: gmin,
            changes_topology: false,
        });
        Artifact::new(
            Descriptor {
                family: "bsimsoi_4_4".into(),
                fingerprint,
                abi: ABI_VERSION,
                equations: layout.equations(),
                parameters,
                charge: Capability::Computed,
                noise: Capability::Computed,
                trial: model_kernel_api::trial::TrialCapabilities::ENGINE,
                taylor: model_kernel_api::taylor::TaylorCapability::Unavailable,
            },
            BsimSoiKernel {
                group: Arc::clone(self),
                index,
                layout,
            },
        )
    }

    fn candidate(&self, values: &[f64]) -> Result<PreparedBsimSoiSetup> {
        if values.len() != self.slots.len() {
            return Err(Error::Shape("BSIMSOI group slot count"));
        }
        let mut instances = self.instances.clone();
        for (slot, &value) in self.slots.iter().zip(values) {
            slot.apply(value, &mut instances)?;
        }
        setup(&self.model, &instances)
    }
}

/// Shared generalized-equation artifact implementation for BSIMSOI.
#[derive(Debug)]
pub struct BsimSoiKernel {
    group: Arc<BsimSoiGroup>,
    index: usize,
    layout: Layout,
}

/// Immutable candidate-local physical preparation.
#[derive(Debug)]
pub struct BsimSoiPrepared {
    ambient: f64,
    gmin: f64,
    drain: PreparedBsimSoiDrainChannel,
    intrinsic: PreparedBsimSoiIntrinsicCharges,
    extrinsic: PreparedBsimSoiExtrinsicCharges,
    junctions: PreparedBsimSoiJunctions,
    gate: PreparedBsimSoiGateTunneling,
    leakage: PreparedBsimSoiGateInducedLeakage,
    impact: PreparedBsimSoiImpactIonization,
    parasitics: PreparedBsimSoiParasitics,
    transport: PreparedBsimSoiTransport,
    noise: PreparedBsimSoiNoise,
}

impl Kernel for BsimSoiKernel {
    type Prepared = BsimSoiPrepared;

    fn prepare(&self, values: &[f64], ambient: f64) -> Result<Self::Prepared> {
        let Some((&gmin, values)) = values.split_last() else {
            return Err(Error::Shape("BSIMSOI gmin slot"));
        };
        if !gmin.is_finite() || gmin < 0. {
            return Err(Error::Domain("BSIMSOI gmin".into()));
        }
        let setup = self.group.candidate(values)?;
        if self.group.layout(&setup, self.index)? != self.layout {
            return Err(Error::Unsupported(
                "BSIMSOI candidate changes node topology",
            ));
        }
        let i = self.index;
        let instance = &setup.instances()[i];
        let m_slot = ModelVariant::BsimSoi
            .parameter(ParameterScope::Instance, "m")
            .ok_or(Error::Schema("BSIMSOI multiplicity slot"))?;
        let Some(ParameterValue::Real(m)) = instance.parameters().get_slot(m_slot.slot) else {
            return Err(Error::Schema("BSIMSOI multiplicity"));
        };
        let result =
            (|| -> std::result::Result<BsimSoiPrepared, model_compact_intrinsic::ModelError> {
                Ok(BsimSoiPrepared {
                    ambient,
                    gmin: gmin * 1e-6 * m,
                    drain: setup.prepare_drain_channel(i, ambient)?,
                    intrinsic: setup.prepare_intrinsic_charges(i, ambient)?,
                    extrinsic: setup.prepare_extrinsic_charges(i, ambient)?,
                    junctions: setup.prepare_junctions(i, ambient)?,
                    gate: setup.prepare_gate_tunneling(i, ambient)?,
                    leakage: setup.prepare_gate_induced_leakage(i, ambient)?,
                    impact: setup.prepare_impact_ionization(i, ambient)?,
                    parasitics: setup.prepare_parasitics(i, ambient)?,
                    transport: setup.prepare_transport(i, ambient)?,
                    noise: setup.prepare_noise(i, ambient)?,
                })
            })();
        result.map_err(|error| super::domain(&error))
    }

    fn evaluate(
        &self,
        prepared: &Self::Prepared,
        coordinates: &[f64],
        out: &mut Point<'_>,
    ) -> Result<()> {
        evaluate::evaluate(&self.layout, prepared, coordinates, out)
    }

    fn evaluate_noise(
        &self,
        prepared: &Self::Prepared,
        coordinates: &[f64],
        frequency: f64,
        sink: &mut dyn noise::Sink,
    ) -> Result<()> {
        if coordinates.len() != self.layout.width {
            return Err(Error::Shape("BSIMSOI coordinate count"));
        }
        let bias = Biases::new(&self.layout, coordinates, prepared.ambient);
        let point = prepared
            .noise
            .evaluate(model_compact_intrinsic::BsimSoiNoiseBias {
                channel: bias.channel(),
                body_source_junction: bias.junctions[0].value,
                body_drain_junction: bias.junctions[1].value,
                contact: bias.contact.value,
                frequency,
            })
            .map_err(|error| super::domain(&error))?;
        evaluate::noise(&self.layout, point, sink)
    }
}
