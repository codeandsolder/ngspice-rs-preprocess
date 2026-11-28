//! Shared equation ABI for the intrinsic EKV channel. Source-specific junction,
//! overlap, and series elements are not implicitly supplied by this artifact.

use std::sync::Arc;

use model_kernel_api::{
    ABI_VERSION, Artifact, Binding, Capability, Descriptor, Equation, EquationKind, Error, Kernel,
    Parameter, Point, Result,
    noise::{Injection, Sink, Source},
    taylor::TaylorCapability,
    trial::TrialCapabilities,
};

use crate::{Geometry, Model, Prepared};

// Reference revision 15 thermal-noise convention.
const NOISE_BOLTZMANN: f64 = 1.380_622_6e-23;
const DRAIN_SOURCE: [Injection; 2] = [
    Injection {
        row: 0,
        re: 1.,
        im: 0.,
    },
    Injection {
        row: 2,
        re: -1.,
        im: 0.,
    },
];

/// Intrinsic independent channel and flicker sources.
#[derive(Clone, Copy, Debug)]
pub struct Noise {
    pub flicker_coefficient: f64,
    pub flicker_frequency_exponent: f64,
}

/// Compiled intrinsic EKV kernel. All geometry and temperature-dependent
/// quantities are prepared per candidate, before nonlinear evaluation.
#[derive(Debug)]
pub struct IntrinsicKernel {
    model: Model,
    noise: Option<Noise>,
}

#[derive(Debug)]
pub struct Instance {
    intrinsic: Prepared,
    temperature: f64,
    series: f64,
}

/// Construct a four-terminal intrinsic artifact. This is not a full
/// source-dialect MOS device: no implicit junction or parasitic network.
///
/// # Errors
/// Invalid fingerprint, default geometry, model or noise parameters.
pub fn artifact(
    model: Model,
    geometry: Geometry,
    noise: Option<Noise>,
    fingerprint: [u8; 16],
) -> Result<Arc<Artifact<IntrinsicKernel>>> {
    model.prepare(geometry, model.nominal_temperature)?;
    if noise.is_some_and(|n| {
        !n.flicker_coefficient.is_finite()
            || n.flicker_coefficient < 0.
            || !n.flicker_frequency_exponent.is_finite()
            || n.flicker_frequency_exponent <= 0.
    }) {
        return Err(Error::Domain(
            "invalid EKV intrinsic noise parameters".into(),
        ));
    }
    let equations = ["drain", "gate", "source", "bulk"]
        .into_iter()
        .map(|name| Equation {
            name: name.into(),
            kind: EquationKind::Electrical,
            binding: Binding::Terminal,
        })
        .collect();
    let parameters = [
        ("length", geometry.length),
        ("width", geometry.width),
        ("parallel", geometry.parallel),
        ("series", geometry.series),
    ]
    .into_iter()
    .map(|(name, default)| Parameter {
        name: name.into(),
        default,
        changes_topology: false,
    })
    .collect();
    Artifact::new(
        Descriptor {
            family: "ekv26_intrinsic".into(),
            fingerprint,
            abi: ABI_VERSION,
            equations,
            parameters,
            charge: Capability::Computed,
            noise: if noise.is_some() {
                Capability::Computed
            } else {
                Capability::Zero
            },
            trial: TrialCapabilities::ENGINE,
            taylor: TaylorCapability::Unavailable,
        },
        IntrinsicKernel { model, noise },
    )
}

fn bias(values: &[f64]) -> Result<[f64; 4]> {
    values
        .try_into()
        .map_err(|_| Error::Shape("EKV terminal coordinates"))
}

impl Kernel for IntrinsicKernel {
    type Prepared = Instance;

    fn prepare(&self, values: &[f64], temperature: f64) -> Result<Instance> {
        let [length, width, parallel, series] = values else {
            return Err(Error::Shape("EKV intrinsic geometry parameters"));
        };
        let intrinsic = self.model.prepare(
            Geometry {
                length: *length,
                width: *width,
                parallel: *parallel,
                series: *series,
            },
            temperature,
        )?;
        Ok(Instance {
            intrinsic,
            temperature,
            series: *series,
        })
    }

    fn evaluate(
        &self,
        instance: &Instance,
        coordinates: &[f64],
        out: &mut Point<'_>,
    ) -> Result<()> {
        if out.current.len() != 4
            || out.charge.len() != 4
            || out.conductance.len() != 16
            || out.capacitance.len() != 16
        {
            return Err(Error::Shape("EKV intrinsic point"));
        }
        let linear = instance.intrinsic.linearize(bias(coordinates)?)?;
        out.current.copy_from_slice(&linear.point.current);
        out.charge.copy_from_slice(&linear.point.charge);
        out.conductance
            .copy_from_slice(linear.conductance.as_flattened());
        out.capacitance
            .copy_from_slice(linear.capacitance.as_flattened());
        Ok(())
    }

    fn evaluate_noise(
        &self,
        instance: &Instance,
        coordinates: &[f64],
        frequency: f64,
        sink: &mut dyn Sink,
    ) -> Result<()> {
        let Some(noise) = self.noise else {
            return Ok(());
        };
        if !frequency.is_finite() || frequency <= 0. {
            return Err(Error::Domain("invalid EKV noise frequency".into()));
        }
        let linear = instance.intrinsic.linearize(bias(coordinates)?)?;
        let thermal =
            4. * NOISE_BOLTZMANN * instance.temperature * linear.point.thermal_conductance;
        let flicker = noise.flicker_coefficient * linear.channel_transconductance.powi(2)
            / (instance.intrinsic.area_cox
                * instance.series
                * frequency.powf(noise.flicker_frequency_exponent));
        for (name, density) in [("channel", thermal), ("flicker", flicker)] {
            if !density.is_finite() || density < 0. {
                return Err(Error::Domain(
                    "nonfinite EKV intrinsic noise density".into(),
                ));
            }
            sink.push(Source {
                name,
                density,
                injections: &DRAIN_SOURCE,
            })?;
        }
        Ok(())
    }
}
