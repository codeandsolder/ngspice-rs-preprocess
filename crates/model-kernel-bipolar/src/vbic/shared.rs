//! Prepared native VBIC producer for the shared physical-model ABI.

use super::{
    binding,
    device::{self, Layout, Node},
};
use crate::Temperature;
use model_kernel_api::{
    ABI_VERSION, Artifact, Binding, Capability, Descriptor, Equation, EquationKind, Error, Kernel,
    Parameter, Point, Result, noise::Sink,
};
use sha2::{Digest, Sha256};
use std::sync::Arc;

#[derive(Debug)]
pub struct VbicKernel {
    model: binding::Model,
    layout: Layout,
    temperature_override: bool,
}

fn domain(error: impl std::fmt::Display) -> Error {
    Error::Domain(error.to_string())
}

fn equations(layout: Layout) -> Result<Vec<Equation>> {
    let mut slots = vec![None; layout.count()];
    for node in Node::ALL {
        let row = layout.node(node);
        slots[row].get_or_insert_with(|| Equation {
            name: node.name().into(),
            kind: EquationKind::Electrical,
            binding: if row < 4 {
                Binding::Terminal
            } else {
                Binding::Hidden
            },
        });
    }
    if let Some(rows) = layout.phase_rows() {
        for (row, name) in rows
            .into_iter()
            .zip(["excess_phase_first", "excess_phase_second"])
        {
            slots[row] = Some(Equation {
                name: name.into(),
                kind: EquationKind::Internal,
                binding: Binding::Hidden,
            });
        }
    }
    if let Some(row) = layout.thermal_row() {
        slots[row] = Some(Equation {
            name: "junction_temperature".into(),
            kind: EquationKind::Thermal,
            binding: Binding::Terminal,
        });
    }
    slots
        .into_iter()
        .map(|slot| slot.ok_or(Error::Shape("unbound VBIC row")))
        .collect()
}

/// Create one stable physical artifact for the resolved nominal model.
/// # Errors
/// Invalid parameters or descriptor.
pub fn artifact(
    model: &binding::Model,
    ambient: Temperature,
    reference: binding::Instance,
) -> Result<Arc<Artifact<VbicKernel>>> {
    let prepared = device::Prepared::new(model, reference, ambient)?;
    let layout = prepared.layout();
    let mut parameters = vec![
        ("area", reference.area()),
        ("multiplicity", reference.multiplicity()),
        ("dt_kelvin", reference.delta_kelvin()),
    ];
    if let Some(t) = reference.temperature_celsius() {
        parameters.push(("temp_celsius", t));
    }
    let mut hash = Sha256::new();
    hash.update(b"model-kernel-bipolar/vbic/shared-abi\0");
    hash.update(ABI_VERSION.to_le_bytes());
    hash.update(model.digest());
    hash.update([u8::from(reference.temperature_celsius().is_some())]);
    for (_, value) in &parameters {
        hash.update(value.to_bits().to_le_bytes());
    }
    let mut fingerprint = [0; 16];
    fingerprint.copy_from_slice(&hash.finalize()[..16]);
    Artifact::new(
        Descriptor {
            family: "vbic".into(),
            fingerprint,
            abi: ABI_VERSION,
            taylor: model_kernel_api::taylor::TaylorCapability::Unavailable,
            equations: equations(layout)?,
            parameters: parameters
                .into_iter()
                .map(|(name, default)| Parameter {
                    name: name.into(),
                    default,
                    changes_topology: false,
                })
                .collect(),
            charge: Capability::Computed,
            noise: Capability::Computed,
            trial: model_kernel_api::trial::TrialCapabilities::ENGINE,
        },
        VbicKernel {
            model: *model,
            layout,
            temperature_override: reference.temperature_celsius().is_some(),
        },
    )
}

impl Kernel for VbicKernel {
    type Prepared = device::Prepared;
    fn prepare(&self, values: &[f64], ambient_kelvin: f64) -> Result<Self::Prepared> {
        let (area, m, delta, temp) = match (self.temperature_override, values) {
            (false, [a, m, d]) => (*a, *m, *d, None),
            (true, [a, m, d, t]) => (*a, *m, *d, Some(*t)),
            _ => return Err(Error::Shape("VBIC parameters")),
        };
        let instance = binding::Instance::new(area, m, temp, delta).map_err(domain)?;
        device::Prepared::new(
            &self.model,
            instance,
            Temperature::new(ambient_kelvin).map_err(domain)?,
        )
    }
    fn evaluate(
        &self,
        prepared: &Self::Prepared,
        coordinates: &[f64],
        out: &mut Point<'_>,
    ) -> Result<()> {
        prepared.evaluate(coordinates, out)?;
        Ok(())
    }
    fn evaluate_noise(
        &self,
        prepared: &Self::Prepared,
        coordinates: &[f64],
        frequency: f64,
        sink: &mut dyn Sink,
    ) -> Result<()> {
        // Fixed-size stack storage for all possible electrical, phase and thermal rows.
        let n = self.layout.count();
        let mut current = [0.; 14];
        let mut charge = [0.; 14];
        let mut conductance = [0.; 196];
        let mut capacitance = [0.; 196];
        let mut point = Point {
            current: &mut current[..n],
            charge: &mut charge[..n],
            conductance: &mut conductance[..n * n],
            capacitance: &mut capacitance[..n * n],
        };
        let op = prepared.evaluate(coordinates, &mut point)?;
        let spectrum = prepared.noise(op, frequency).map_err(domain)?;
        device::inject_noise(self.layout, spectrum, sink)
    }
}
