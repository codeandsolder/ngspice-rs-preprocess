//! Conservative bulk and sidewall topology for the shared prepared-model ABI.

use std::sync::Arc;

use model_kernel_api::{
    ABI_VERSION, Artifact, Binding, Capability, Descriptor, Equation, EquationKind, Error, Kernel,
    Point, Result, noise::Sink, trial::TrialCapabilities,
};

use super::{branch, domain, noise_branch, parameter};
use crate::ltspice_semiconductor::{Instance, PreparedBranch, PreparedInstance, SourceModel};

/// Explicit prepared family for the ordinary LTspice semiconductor law.
pub const FAMILY: &str = "ltspice_semiconductor_diode";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Topology {
    bulk: bool,
    sidewall: bool,
    bulk_series: bool,
    side_series: bool,
}

impl Topology {
    fn new(instance: PreparedInstance) -> Self {
        Self {
            bulk: instance.bulk.is_some(),
            sidewall: instance.sidewall.is_some(),
            bulk_series: instance.bulk.is_some_and(|b| b.resistance() != 0.),
            side_series: instance.independent_sidewall,
        }
    }

    const fn bulk_node(self) -> usize {
        if self.bulk_series { 2 } else { 0 }
    }

    const fn side_node(self) -> usize {
        if self.side_series {
            if self.bulk_series { 3 } else { 2 }
        } else {
            self.bulk_node()
        }
    }
}

/// Immutable topology and resolved source model.
#[derive(Debug)]
pub struct SemiconductorKernel {
    model: SourceModel,
    topology: Topology,
    absolute_temperature: bool,
}

/// Construct an artifact for the reference instance's topology and temperature
/// policy. The semantic fingerprint is supplied by the source binder.
///
/// # Errors
/// Invalid model, instance, series conductance or fingerprint.
pub fn artifact(
    model: SourceModel,
    reference: Instance,
    absolute_temperature: bool,
    fingerprint: [u8; 16],
) -> Result<Arc<Artifact<SemiconductorKernel>>> {
    let prepared = model.bind(reference).map_err(domain)?;
    let topology = Topology::new(prepared);
    check_resistance(prepared)?;
    let equation = |name: &str, binding| Equation {
        name: name.into(),
        kind: EquationKind::Electrical,
        binding,
    };
    let mut equations = vec![
        equation("anode", Binding::Terminal),
        equation("cathode", Binding::Terminal),
    ];
    if topology.bulk_series {
        equations.push(equation("bulk_anode", Binding::Hidden));
    }
    if topology.side_series {
        equations.push(equation("sidewall_anode", Binding::Hidden));
    }
    let mut parameters = [
        ("area", reference.area),
        ("perimeter", reference.perimeter.unwrap_or(0.)),
        ("multiplier", reference.multiplier),
        ("series", reference.series),
        ("gmin", reference.gmin),
    ]
    .map(|(name, value)| parameter(name, value))
    .to_vec();
    if absolute_temperature {
        parameters.push(parameter("absolute_temperature", reference.temperature));
    }
    Artifact::new(
        Descriptor {
            family: FAMILY.into(),
            fingerprint,
            abi: ABI_VERSION,
            equations,
            parameters,
            charge: Capability::Computed,
            noise: Capability::Computed,
            trial: TrialCapabilities::ENGINE,
        },
        SemiconductorKernel {
            model,
            topology,
            absolute_temperature,
        },
    )
}

fn check_resistance(instance: PreparedInstance) -> Result<()> {
    for junction in [instance.bulk, instance.sidewall].into_iter().flatten() {
        let r = junction.resistance();
        if r != 0. && !r.recip().is_finite() {
            return Err(Error::Domain("nonrepresentable series conductance".into()));
        }
    }
    Ok(())
}

impl SemiconductorKernel {
    fn junctions(&self, instance: &PreparedInstance) -> [(Option<PreparedBranch>, usize, bool); 2] {
        [
            (instance.bulk, self.topology.bulk_node(), false),
            (instance.sidewall, self.topology.side_node(), true),
        ]
    }
}

impl Kernel for SemiconductorKernel {
    type Prepared = PreparedInstance;

    fn prepare(&self, values: &[f64], ambient_kelvin: f64) -> Result<Self::Prepared> {
        let (base, temperature) = match (self.absolute_temperature, values) {
            (false, [a, p, m, n, g]) => ([*a, *p, *m, *n, *g], ambient_kelvin),
            (true, [a, p, m, n, g, t]) => ([*a, *p, *m, *n, *g], *t),
            _ => return Err(Error::Shape("LTspice diode parameter count")),
        };
        let [area, perimeter, multiplier, series, gmin] = base;
        let prepared = self
            .model
            .bind(Instance {
                temperature,
                area,
                perimeter: Some(perimeter),
                multiplier,
                series,
                gmin,
            })
            .map_err(domain)?;
        if Topology::new(prepared) != self.topology {
            return Err(Error::Unsupported(
                "LTspice diode topology requires new artifact",
            ));
        }
        check_resistance(prepared)?;
        Ok(prepared)
    }

    fn evaluate(
        &self,
        instance: &Self::Prepared,
        coordinates: &[f64],
        out: &mut Point<'_>,
    ) -> Result<()> {
        out.current.fill(0.);
        out.charge.fill(0.);
        out.conductance.fill(0.);
        out.capacitance.fill(0.);
        for (junction, node, _) in self.junctions(instance) {
            let Some(junction) = junction else {
                continue;
            };
            let value = junction
                .evaluate(coordinates[node] - coordinates[1])
                .map_err(domain)?
                .total;
            branch(
                out.current,
                out.conductance,
                node,
                1,
                value.current,
                value.conductance,
            );
            branch(
                out.charge,
                out.capacitance,
                node,
                1,
                value.charge,
                value.capacitance,
            );
            if junction.resistance() != 0. {
                let g = junction.resistance().recip();
                branch(
                    out.current,
                    out.conductance,
                    0,
                    node,
                    (coordinates[0] - coordinates[node]) * g,
                    g,
                );
            }
        }
        Ok(())
    }

    fn evaluate_noise(
        &self,
        instance: &Self::Prepared,
        coordinates: &[f64],
        frequency: f64,
        sink: &mut dyn Sink,
    ) -> Result<()> {
        for (junction, node, sidewall) in self.junctions(instance) {
            let Some(junction) = junction else {
                continue;
            };
            let value = junction
                .evaluate(coordinates[node] - coordinates[1])
                .map_err(domain)?;
            let [shot, flicker] = instance
                .branch_noise(value, sidewall, frequency)
                .map_err(domain)?;
            let names = if sidewall {
                ["sidewall_shot", "sidewall_flicker", "sidewall_series"]
            } else {
                ["bulk_shot", "bulk_flicker", "bulk_series"]
            };
            noise_branch(sink, names[0], node, 1, shot)?;
            noise_branch(sink, names[1], node, 1, flicker)?;
            if junction.resistance() != 0. {
                noise_branch(
                    sink,
                    names[2],
                    0,
                    node,
                    junction.series_thermal().map_err(domain)?,
                )?;
            }
        }
        Ok(())
    }
}
