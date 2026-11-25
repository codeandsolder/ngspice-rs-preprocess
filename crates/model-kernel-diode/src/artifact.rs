//! Conservative nodal producer for the shared prepared-model ABI.
//!
//! The measured intrinsic junction remains the single physical implementation.
//! Series resistance determines topology at artifact construction; a candidate
//! changing that topology requires a fresh descriptor and circuit binding.

use std::sync::Arc;

use model_kernel_api::{
    ABI_VERSION, Artifact, Binding, Capability, Descriptor, Equation, EquationKind, Error, Kernel,
    Parameter, Point, Result,
    noise::{Injection, Sink, Source},
};

use super::{Instance, ModelError, PreparedInstance, PreparedModel};

/// Topology-specific `SIMetrix` level-1 diode kernel.
#[derive(Debug)]
pub struct DiodeKernel {
    model: PreparedModel,
    series: bool,
    absolute_temperature: bool,
}

fn domain(error: ModelError) -> Error {
    match error {
        ModelError::Invalid(name) => Error::Domain(name.into()),
        ModelError::Unsupported(name) => Error::Unsupported(name),
    }
}

/// Create an artifact for the topology and temperature-policy shape of a
/// reference instance.
///
/// Its numeric geometry, gmin and temperature overrides
/// become candidate slots. The caller supplies the semantic fingerprint,
/// including the model, series-node presence and temperature-policy shape.
///
/// # Errors
/// Invalid instance or fingerprint, or an unrepresentable series conductance.
pub fn artifact(
    model: PreparedModel,
    reference: Instance,
    fingerprint: [u8; 16],
) -> Result<Arc<Artifact<DiodeKernel>>> {
    let initial = model.bind(reference).map_err(domain)?;
    let series = initial.series_resistance() != 0.;
    let absolute_temperature = reference.absolute_temperature.is_some();
    let equation = |name: &str, binding| Equation {
        name: name.into(),
        kind: EquationKind::Electrical,
        binding,
    };
    let mut equations = vec![
        equation("anode", Binding::Terminal),
        equation("cathode", Binding::Terminal),
    ];
    if series {
        equations.push(equation("anode_prime", Binding::Hidden));
        series_conductance(&initial)?;
    }
    let mut parameters = [
        ("area", reference.area),
        ("multiplier", reference.multiplier),
        ("gmin", reference.gmin),
        ("relative_temperature", reference.relative_temperature),
    ]
    .map(|(name, default)| parameter(name, default))
    .to_vec();
    if let Some(temperature) = reference.absolute_temperature {
        parameters.push(parameter("absolute_temperature", temperature));
    }
    Artifact::new(
        Descriptor {
            family: "simetrix_diode_level1".into(),
            fingerprint,
            abi: ABI_VERSION,
            equations,
            parameters,
            charge: Capability::Computed,
            noise: Capability::Computed,
        },
        DiodeKernel {
            model,
            series,
            absolute_temperature,
        },
    )
}

fn parameter(name: &str, default: f64) -> Parameter {
    Parameter {
        name: name.into(),
        default,
        changes_topology: false,
    }
}

fn series_conductance(instance: &PreparedInstance) -> Result<f64> {
    let conductance = instance.series_resistance().recip();
    if conductance.is_finite() {
        Ok(conductance)
    } else {
        Err(Error::Domain(
            "nonrepresentable diode series conductance".into(),
        ))
    }
}

impl DiodeKernel {
    const fn junction_anode(&self) -> usize {
        if self.series { 2 } else { 0 }
    }

    fn junction_voltage(&self, coordinates: &[f64]) -> Result<f64> {
        match (self.series, coordinates) {
            (false, [anode, cathode]) => Ok(anode - cathode),
            (true, [_, cathode, intrinsic]) => Ok(intrinsic - cathode),
            _ => Err(Error::Shape("diode coordinate count")),
        }
    }
}

// One oriented conservative two-terminal contribution to the generalized
// residual or storage equation and its analytic Jacobian.
fn branch(
    residual: &mut [f64],
    jacobian: &mut [f64],
    positive: usize,
    negative: usize,
    value: f64,
    derivative: f64,
) {
    let width = residual.len();
    residual[positive] += value;
    residual[negative] -= value;
    jacobian[positive * width + positive] += derivative;
    jacobian[positive * width + negative] -= derivative;
    jacobian[negative * width + positive] -= derivative;
    jacobian[negative * width + negative] += derivative;
}

fn noise_branch(
    sink: &mut dyn Sink,
    name: &str,
    positive: usize,
    negative: usize,
    density: f64,
) -> Result<()> {
    sink.push(Source {
        name,
        density,
        injections: &[
            Injection {
                row: positive,
                re: 1.,
                im: 0.,
            },
            Injection {
                row: negative,
                re: -1.,
                im: 0.,
            },
        ],
    })
}

impl Kernel for DiodeKernel {
    type Prepared = PreparedInstance;

    fn prepare(&self, values: &[f64], ambient_kelvin: f64) -> Result<Self::Prepared> {
        let (base, absolute_temperature) = match (self.absolute_temperature, values) {
            (false, [area, multiplier, gmin, relative]) => {
                ([*area, *multiplier, *gmin, *relative], None)
            }
            (true, [area, multiplier, gmin, relative, absolute]) => {
                ([*area, *multiplier, *gmin, *relative], Some(*absolute))
            }
            _ => return Err(Error::Shape("diode parameter count")),
        };
        let [area, multiplier, gmin, relative_temperature] = base;
        let instance = self
            .model
            .bind(Instance {
                analysis_temperature: ambient_kelvin,
                absolute_temperature,
                relative_temperature,
                area,
                multiplier,
                gmin,
            })
            .map_err(domain)?;
        if (instance.series_resistance() != 0.) != self.series {
            return Err(Error::Unsupported(
                "diode series topology requires new artifact",
            ));
        }
        if self.series {
            series_conductance(&instance)?;
        }
        Ok(instance)
    }

    fn evaluate(
        &self,
        prepared: &Self::Prepared,
        coordinates: &[f64],
        out: &mut Point<'_>,
    ) -> Result<()> {
        let voltage = self.junction_voltage(coordinates)?;
        let width = coordinates.len();
        if out.current.len() != width
            || out.charge.len() != width
            || out.conductance.len() != width * width
            || out.capacitance.len() != width * width
        {
            return Err(Error::Shape("diode point"));
        }
        let evaluation = prepared.evaluate(voltage).map_err(domain)?;
        out.current.fill(0.);
        out.charge.fill(0.);
        out.conductance.fill(0.);
        out.capacitance.fill(0.);
        let intrinsic = self.junction_anode();
        branch(
            out.current,
            out.conductance,
            intrinsic,
            1,
            evaluation.current,
            evaluation.conductance,
        );
        branch(
            out.charge,
            out.capacitance,
            intrinsic,
            1,
            evaluation.charge,
            evaluation.capacitance,
        );
        if self.series {
            let conductance = series_conductance(prepared)?;
            branch(
                out.current,
                out.conductance,
                0,
                intrinsic,
                (coordinates[0] - coordinates[intrinsic]) * conductance,
                conductance,
            );
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
        let voltage = self.junction_voltage(coordinates)?;
        let evaluation = prepared.evaluate(voltage).map_err(domain)?;
        let densities = prepared
            .noise_density(evaluation.current, frequency)
            .map_err(domain)?;
        let intrinsic = self.junction_anode();
        noise_branch(sink, "shot", intrinsic, 1, densities.shot)?;
        noise_branch(sink, "flicker", intrinsic, 1, densities.flicker)?;
        if self.series {
            noise_branch(sink, "series", 0, intrinsic, densities.series_thermal)?;
        }
        Ok(())
    }
}
