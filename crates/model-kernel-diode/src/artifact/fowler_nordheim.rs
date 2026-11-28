//! Conservative two-terminal producer for the prepared-model ABI.
//!
//! The physical law is shared with direct typed evaluation. No circuit
//! coordinates, accepted/trial history or integration coefficients are stored.

use std::sync::Arc;

use model_kernel_api::{
    ABI_VERSION, Artifact, Binding, Capability, Descriptor, Equation, EquationKind, Error, Kernel,
    Point, Result, noise::Sink, trial::TrialCapabilities,
};

use super::{branch, domain, noise_branch, parameter};
use crate::fowler_nordheim::{Instance, Model, PreparedInstance};

/// Physical family identity after occurrence-specific source dispatch.
pub const FAMILY: &str = "ltspice_fowler_nordheim";

/// Immutable model and four numeric occurrence slots. L and W are the
/// resolved dimensions (zero is distinct from an absent source override).
#[derive(Debug)]
pub struct FowlerNordheimKernel {
    model: Model,
}

/// Create the two-terminal artifact for one resolved model.
///
/// Slot order is L, W, M, N. The caller supplies the semantic model
/// fingerprint and may prepare multiple instances with different slot values.
/// The model defaults are exposed in the descriptor; the source binder
/// applies instance overrides before preparation.
///
/// # Errors
/// Invalid model coefficients or artifact fingerprint.
pub fn artifact(
    model: Model,
    fingerprint: [u8; 16],
) -> Result<Arc<Artifact<FowlerNordheimKernel>>> {
    // Validate the model itself, not a hypothetical default occurrence.
    // The actual instance may override either dimension or multiplicity.
    model.validate().map_err(domain)?;
    Artifact::new(
        Descriptor {
            family: FAMILY.into(),
            fingerprint,
            abi: ABI_VERSION,
            taylor: model_kernel_api::taylor::TaylorCapability::Unavailable,
            equations: ["anode", "cathode"]
                .into_iter()
                .map(|name| Equation {
                    name: name.into(),
                    kind: EquationKind::Electrical,
                    binding: Binding::Terminal,
                })
                .collect(),
            parameters: [
                ("l", model.length),
                ("w", model.width),
                ("m", 1.),
                ("n", 1.),
            ]
            .into_iter()
            .map(|(name, default)| parameter(name, default))
            .collect(),
            charge: Capability::Computed,
            noise: Capability::Computed,
            trial: TrialCapabilities::ENGINE,
        },
        FowlerNordheimKernel { model },
    )
}

fn voltage(coordinates: &[f64]) -> Result<f64> {
    let [anode, cathode] = coordinates else {
        return Err(Error::Shape("F-N diode coordinates"));
    };
    Ok(anode - cathode)
}

impl Kernel for FowlerNordheimKernel {
    type Prepared = PreparedInstance;

    fn prepare(&self, values: &[f64], _ambient_kelvin: f64) -> Result<Self::Prepared> {
        let [length, width, multiplier, series] = values else {
            return Err(Error::Shape("F-N diode parameter count"));
        };
        self.model
            .bind(Instance {
                length: Some(*length),
                width: Some(*width),
                multiplier: *multiplier,
                series: *series,
            })
            .map_err(domain)
    }

    fn evaluate(
        &self,
        prepared: &Self::Prepared,
        coordinates: &[f64],
        out: &mut Point<'_>,
    ) -> Result<()> {
        let value = prepared.evaluate(voltage(coordinates)?).map_err(domain)?;
        if out.current.len() != 2
            || out.charge.len() != 2
            || out.conductance.len() != 4
            || out.capacitance.len() != 4
        {
            return Err(Error::Shape("F-N diode point"));
        }
        // Every element is overwritten so raw Kernel calls and Artifact
        // calls have the same behavior on reused workspaces.
        out.current.fill(0.);
        out.charge.fill(0.);
        out.conductance.fill(0.);
        out.capacitance.fill(0.);
        branch(
            out.current,
            out.conductance,
            0,
            1,
            value.current,
            value.conductance,
        );
        branch(
            out.charge,
            out.capacitance,
            0,
            1,
            value.charge,
            value.capacitance,
        );
        Ok(())
    }

    fn evaluate_noise(
        &self,
        prepared: &Self::Prepared,
        coordinates: &[f64],
        _frequency: f64,
        sink: &mut dyn Sink,
    ) -> Result<()> {
        let value = prepared.evaluate(voltage(coordinates)?).map_err(domain)?;
        let density = prepared.shot_noise(value.current).map_err(domain)?;
        noise_branch(sink, "shot", 0, 1, density)
    }
}
