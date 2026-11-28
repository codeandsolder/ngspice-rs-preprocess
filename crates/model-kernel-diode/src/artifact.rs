//! Conservative nodal producer for the shared prepared-model ABI.
//!
//! The measured intrinsic junction remains the single physical implementation.
//! Series resistance determines topology at artifact construction; a candidate
//! changing that topology requires a fresh descriptor and circuit binding.

/// Prepared Fowler–Nordheim source family.
pub mod fowler_nordheim;

use std::sync::Arc;

use model_kernel_api::{
    ABI_VERSION, Artifact, Binding, Capability, Descriptor, Equation, EquationKind, Error, Kernel,
    Parameter, Point, Result,
    noise::{Injection, Sink, Source},
    taylor::{
        MAX_ORDER, TaylorCapability, TaylorInput, TaylorPoint,
        arithmetic::{Scalar, Series},
    },
    trial::{Convergence, ConvergenceInput, TrialCapabilities, TrialPolicy},
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
            family: super::MODEL_FAMILY.into(),
            fingerprint,
            abi: ABI_VERSION,
            taylor: TaylorCapability::Computed {
                max_order: MAX_ORDER,
            },
            equations,
            parameters,
            charge: Capability::Computed,
            noise: Capability::Computed,
            trial: TrialCapabilities {
                initialization: TrialPolicy::Kernel,
                limiting: TrialPolicy::Kernel,
                convergence: TrialPolicy::Kernel,
            },
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

    fn set_junction_voltage(&self, coordinates: &mut [f64], voltage: f64) -> Result<()> {
        self.junction_voltage(coordinates)?;
        let intrinsic = self.junction_anode();
        let potential = coordinates[1] + voltage;
        if !potential.is_finite() {
            return Err(Error::Domain(
                "nonrepresentable diode trial potential".into(),
            ));
        }
        coordinates[intrinsic] = potential;
        Ok(())
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

    fn evaluate_taylor(
        &self,
        prepared: &Self::Prepared,
        input: TaylorInput<'_>,
        out: &mut TaylorPoint<'_>,
    ) -> Result<()> {
        let width = if self.series { 3 } else { 2 };
        let intrinsic = self.junction_anode();
        let voltage = input.coordinate(width, intrinsic)? - input.coordinate(width, 1)?;
        let evaluation = prepared
            .evaluate_physical(
                voltage,
                &mut model_kernel_api::taylor::GuardRecorder::new(out.guards),
            )
            .map_err(domain)?;
        let series = if self.series {
            let conductance = series_conductance(prepared)?;
            Some((
                (input.coordinate(width, 0)? - input.coordinate(width, intrinsic)?)
                    .scale(conductance),
                Series::constant(conductance),
            ))
        } else {
            None
        };
        // Direct trait callers receive the same checked dimensions as the
        // artifact interface. There is no partially indexed output on failure.
        let rows = width * (input.order + 1);
        let matrices = rows * width;
        if out.current.len() != rows
            || out.charge.len() != rows
            || out.conductance.len() != matrices
            || out.capacitance.len() != matrices
        {
            return Err(Error::Shape("diode Taylor point"));
        }
        out.current.fill(0.);
        out.charge.fill(0.);
        out.conductance.fill(0.);
        out.capacitance.fill(0.);
        for degree in 0..=input.order {
            let row = degree * width;
            let matrix = row * width;
            let residual = &mut out.current[row..row + width];
            let jacobian = &mut out.conductance[matrix..matrix + width * width];
            branch(
                residual,
                jacobian,
                intrinsic,
                1,
                evaluation.current.coefficient(degree),
                evaluation.conductance.coefficient(degree),
            );
            if let Some((current, conductance)) = &series {
                branch(
                    residual,
                    jacobian,
                    0,
                    intrinsic,
                    current.coefficient(degree),
                    conductance.coefficient(degree),
                );
            }
            branch(
                &mut out.charge[row..row + width],
                &mut out.capacitance[matrix..matrix + width * width],
                intrinsic,
                1,
                evaluation.charge.coefficient(degree),
                evaluation.capacitance.coefficient(degree),
            );
        }
        Ok(())
    }

    fn initialize_trial(&self, prepared: &Self::Prepared, coordinates: &mut [f64]) -> Result<()> {
        self.set_junction_voltage(coordinates, prepared.initial_junction_voltage())
    }

    fn limit_trial(
        &self,
        prepared: &Self::Prepared,
        reference: model_kernel_api::trial::TrialReference<'_>,
        coordinates: &mut [f64],
    ) -> Result<()> {
        let raw = self.junction_voltage(coordinates)?;
        let previous = self.junction_voltage(reference.coordinates())?;
        let limited = prepared
            .limit_junction_voltage(raw, previous)
            .map_err(domain)?;
        if limited.to_bits() != raw.to_bits() {
            self.set_junction_voltage(coordinates, limited)?;
        }
        Ok(())
    }

    fn trial_convergence(
        &self,
        _prepared: &Self::Prepared,
        input: ConvergenceInput<'_>,
    ) -> Result<Convergence> {
        // The cathode row contains only the negative intrinsic junction
        // current even when the anode has a separate series resistor.
        let old_voltage = self.junction_voltage(input.previous.coordinates)?;
        let new_voltage = self.junction_voltage(input.candidate.coordinates)?;
        let width = input.previous.coordinates.len();
        let old_current = *input
            .previous
            .residual
            .get(1)
            .ok_or(Error::Shape("diode convergence residual"))?;
        let new_current = *input
            .candidate
            .residual
            .get(1)
            .ok_or(Error::Shape("diode convergence residual"))?;
        let derivative = *input
            .previous
            .jacobian
            .get(width + self.junction_anode())
            .ok_or(Error::Shape("diode convergence Jacobian"))?;
        let predicted = derivative.mul_add(new_voltage - old_voltage, old_current);
        input.compare_residual(1, predicted, new_current)
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
