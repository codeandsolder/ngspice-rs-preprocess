//! Shared prepared-model ABI producer for the complete isothermal HICUM/L2
//! residual.
//!
//! The native family artifact remains the only physical evaluator.
//! Live self-heating requires a complete temperature Jacobian, and is not
//! silently represented by an isothermal kernel.

use std::{fmt::Display, sync::Arc};

use model_kernel_api::{
    ABI_VERSION, Artifact, Binding, Capability, Descriptor, Equation, EquationKind, Error, Kernel,
    Parameter, Point, Result,
    noise::{Injection, Sink, Source},
};
use sha2::{Digest, Sha256};

use super::{
    artifact::{Model, Prepared, StructureKey, Trial},
    instance::Options,
    nodal::Layout,
    noise::{FlickerLocation, Output, ShotCovariance},
    topology::{Branch, Node},
};
use crate::Temperature;

/// Fixed, reduced electrical and NQS topology.
#[derive(Debug)]
pub struct HicumKernel {
    model: Model,
    layout: Layout,
    temperature_override: bool,
}

fn domain(error: impl Display) -> Error {
    Error::Domain(error.to_string())
}

fn parameter(name: &str, default: f64) -> Parameter {
    Parameter {
        name: name.into(),
        default,
        changes_topology: false,
    }
}

fn equations(layout: Layout) -> Result<Vec<Equation>> {
    let mut slots = vec![None; layout.count()];
    for (node, name) in Node::ALL
        .into_iter()
        .zip(["c", "b", "e", "s", "ci", "ei", "bp", "bi", "si"])
    {
        let row = layout.node(node);
        slots[row].get_or_insert_with(|| Equation {
            name: name.into(),
            kind: EquationKind::Electrical,
            binding: if Node::TERMINALS.contains(&node) {
                Binding::Terminal
            } else {
                Binding::Hidden
            },
        });
    }
    for (state, name) in ["nqs_current_first", "nqs_current_second", "nqs_charge"]
        .into_iter()
        .enumerate()
    {
        if let Some(row) = layout.nqs_slot(state) {
            slots[row] = Some(Equation {
                name: name.into(),
                kind: EquationKind::Internal,
                binding: Binding::Hidden,
            });
        }
    }
    slots
        .into_iter()
        .map(|slot| slot.ok_or(Error::Shape("unbound HICUM row")))
        .collect()
}

/// Bind a source model to a stable shared ABI descriptor. Initial voltage and
/// OFF settings are solver initial conditions, not physical model parameters.
///
/// # Errors
/// Invalid instance, unrepresentable descriptor, or live thermal topology.
pub fn artifact(
    model: &Model,
    ambient: Temperature,
    reference: Options,
) -> Result<Arc<Artifact<HicumKernel>>> {
    let instance = model.bind(ambient, reference).map_err(domain)?;
    let layout = instance.layout();
    if layout.thermal_slot().is_some() {
        return Err(Error::Unsupported("HICUM full electrothermal Jacobian"));
    }
    let mut parameters = vec![
        parameter("area", reference.area),
        parameter("multiplicity", reference.multiplicity),
        parameter("dt_kelvin", reference.dt_kelvin),
    ];
    if let Some(temperature) = reference.temp_celsius {
        parameters.push(parameter("temp_celsius", temperature));
    }
    let mut hash = Sha256::new();
    hash.update(b"model-kernel-bipolar/hicum2/shared-abi\0");
    hash.update(ABI_VERSION.to_le_bytes());
    hash.update(model.identity().0);
    hash.update([StructureKey::from_layout(layout).0]);
    hash.update([u8::from(reference.temp_celsius.is_some())]);
    for slot in &parameters {
        hash.update(slot.default.to_bits().to_le_bytes());
    }
    let digest = hash.finalize();
    let mut fingerprint = [0; 16];
    fingerprint.copy_from_slice(&digest[..16]);
    Artifact::new(
        Descriptor {
            family: "hicum2".into(),
            fingerprint,
            abi: ABI_VERSION,
            equations: equations(layout)?,
            parameters,
            charge: Capability::Computed,
            noise: Capability::Computed,
        },
        HicumKernel {
            model: *model,
            layout,
            temperature_override: reference.temp_celsius.is_some(),
        },
    )
}

const fn injection(layout: Layout, node: Node, re: f64, im: f64) -> Injection {
    Injection {
        row: layout.node(node),
        re,
        im,
    }
}

fn noise_branch(
    sink: &mut dyn Sink,
    layout: Layout,
    branch: Branch,
    name: &str,
    density: f64,
) -> Result<()> {
    let (positive, negative) = branch.nodes();
    if layout.node(positive) == layout.node(negative) {
        return Ok(());
    }
    sink.push(Source {
        name,
        density,
        injections: &[
            injection(layout, positive, 1., 0.),
            injection(layout, negative, -1., 0.),
        ],
    })
}

fn shot_noise(sink: &mut dyn Sink, layout: Layout, shot: ShotCovariance) -> Result<()> {
    noise_branch(
        sink,
        layout,
        Branch::IntrinsicEmitter,
        "ibei",
        shot.base_independent,
    )?;
    // Source covariance S_be,ce = +j*gain*S_ce. One collector generator
    // injects coherently in both branches. No subtraction of nearly equal
    // diagonal spectral densities or division by a vanishing collector PSD.
    let gain = shot.collector_to_base_imaginary;
    sink.push(Source {
        name: "it",
        density: shot.collector,
        injections: &[
            injection(layout, Node::Ci, 1., 0.),
            injection(layout, Node::Ei, -1., -gain),
            injection(layout, Node::Bi, 0., gain),
        ],
    })
}

fn noise_sources(sink: &mut dyn Sink, layout: Layout, spectra: Output) -> Result<()> {
    let t = spectra.thermal;
    let s = spectra.shot;
    let f = spectra.flicker;
    let flicker_branch = match spectra.flicker_location {
        FlickerLocation::Intrinsic => Branch::IntrinsicEmitter,
        FlickerLocation::Peripheral => Branch::PeripheralEmitter,
    };
    for (branch, name, density) in [
        (Branch::Rbx, "rbx", t.rbx),
        (Branch::Rbi, "rbi", t.rbi),
        (Branch::Rcx, "rcx", t.rcx),
        (Branch::Re, "re", t.re),
        (Branch::Rsu, "rsu", t.rsu),
        (Branch::Avalanche, "iavl", s.iavl),
        (Branch::IntrinsicCollector, "ibci", s.ibci),
        (Branch::PeripheralEmitter, "ibep", s.ibep),
        (Branch::PeripheralCollector, "ijbcx", s.ijbcx),
        (Branch::SubstrateJunction, "ijsc", s.ijsc),
        (flicker_branch, "fbe", f.be),
        (Branch::Re, "fre", f.emitter_resistor),
    ] {
        noise_branch(sink, layout, branch, name, density)?;
    }
    shot_noise(sink, layout, spectra.be_ce_shot)
}

impl Kernel for HicumKernel {
    type Prepared = Prepared;

    fn prepare(&self, values: &[f64], ambient_kelvin: f64) -> Result<Self::Prepared> {
        let (area, multiplicity, dt_kelvin, temp_celsius) =
            match (self.temperature_override, values) {
                (false, [a, m, dt]) => (*a, *m, *dt, None),
                (true, [a, m, dt, t]) => (*a, *m, *dt, Some(*t)),
                _ => return Err(Error::Shape("HICUM parameters")),
            };
        let candidate = self
            .model
            .bind(
                Temperature::new(ambient_kelvin).map_err(domain)?,
                Options {
                    area,
                    multiplicity,
                    temp_celsius,
                    dt_kelvin,
                    ..Options::default()
                },
            )
            .map_err(domain)?;
        if candidate.layout() != self.layout {
            return Err(Error::Unsupported("HICUM topology requires new artifact"));
        }
        candidate.prepare(0.).map_err(domain)
    }

    fn evaluate(
        &self,
        prepared: &Self::Prepared,
        coordinates: &[f64],
        out: &mut Point<'_>,
    ) -> Result<()> {
        let n = self.layout.count();
        if out.current.len() != n
            || out.charge.len() != n
            || out.conductance.len() != n * n
            || out.capacitance.len() != n * n
        {
            return Err(Error::Shape("HICUM point"));
        }
        let frame = prepared
            .evaluate(Trial {
                unknowns: coordinates,
                electrothermal: None,
            })
            .map_err(domain)?;
        out.current.copy_from_slice(&frame.local.current[..n]);
        out.charge.copy_from_slice(&frame.local.charge[..n]);
        for (row, (conductance, capacitance)) in out
            .conductance
            .chunks_exact_mut(n)
            .zip(out.capacitance.chunks_exact_mut(n))
            .enumerate()
        {
            conductance.copy_from_slice(&frame.local.conductance[row][..n]);
            capacitance.copy_from_slice(&frame.local.capacitance[row][..n]);
        }
        Ok(())
    }

    fn evaluate_noise(
        &self,
        prepared: &Self::Prepared,
        coordinates: &[f64],
        frequency: f64,
        sink: &mut dyn Sink,
    ) -> Result<()> {
        let frame = prepared
            .evaluate(Trial {
                unknowns: coordinates,
                electrothermal: None,
            })
            .map_err(domain)?;
        let spectra = prepared
            .evaluate_noise(&frame.operating, frequency)
            .map_err(domain)?;
        noise_sources(sink, self.layout, spectra)
    }
}
