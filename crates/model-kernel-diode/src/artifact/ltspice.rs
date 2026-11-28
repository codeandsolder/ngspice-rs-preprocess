//! Conservative bulk and sidewall topology for the shared prepared-model ABI.

use std::sync::Arc;

use model_kernel_api::{
    ABI_VERSION, Artifact, Binding, Capability, Descriptor, Equation, EquationKind, Error, Kernel,
    Point, Result, noise::Sink, trial::TrialCapabilities,
};

use super::{branch, domain, noise_branch, parameter};
use crate::ltspice_semiconductor::{Instance, PreparedBranch, PreparedInstance, SourceModel};

/// Explicit prepared family for the ordinary `LTspice` semiconductor law.
pub const FAMILY: &str = "ltspice_semiconductor_diode";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BranchTopology {
    Absent,
    Shared,
    Series,
}

impl BranchTopology {
    const fn new(branch: Option<&PreparedBranch>) -> Self {
        match branch {
            None => Self::Absent,
            Some(branch) if branch.resistance() != 0. => Self::Series,
            Some(_) => Self::Shared,
        }
    }

    const fn has_series(self) -> bool {
        matches!(self, Self::Series)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Topology {
    bulk: BranchTopology,
    sidewall: BranchTopology,
}

impl Topology {
    const fn new(instance: &PreparedInstance) -> Self {
        Self {
            bulk: BranchTopology::new(instance.bulk.as_ref()),
            sidewall: BranchTopology::new(instance.sidewall.as_ref()),
        }
    }

    const fn bulk_node(self) -> usize {
        if self.bulk.has_series() { 2 } else { 0 }
    }

    const fn side_node(self) -> usize {
        if self.sidewall.has_series() {
            if self.bulk.has_series() { 3 } else { 2 }
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
    model: &SourceModel,
    reference: Instance,
    absolute_temperature: bool,
    fingerprint: [u8; 16],
) -> Result<Arc<Artifact<SemiconductorKernel>>> {
    let prepared = model.bind(reference).map_err(domain)?;
    let topology = Topology::new(&prepared);
    check_resistance(&prepared)?;
    let equation = |name: &str, binding| Equation {
        name: name.into(),
        kind: EquationKind::Electrical,
        binding,
    };
    let mut equations = vec![
        equation("anode", Binding::Terminal),
        equation("cathode", Binding::Terminal),
    ];
    if topology.bulk.has_series() {
        equations.push(equation("bulk_anode", Binding::Hidden));
    }
    if topology.sidewall.has_series() {
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
            taylor: model_kernel_api::taylor::TaylorCapability::Unavailable,
            equations,
            parameters,
            charge: Capability::Computed,
            noise: Capability::Computed,
            trial: TrialCapabilities::ENGINE,
        },
        SemiconductorKernel {
            model: *model,
            topology,
            absolute_temperature,
        },
    )
}

fn check_resistance(instance: &PreparedInstance) -> Result<()> {
    for junction in [instance.bulk.as_ref(), instance.sidewall.as_ref()]
        .into_iter()
        .flatten()
    {
        let r = junction.resistance();
        if r != 0. && !r.recip().is_finite() {
            return Err(Error::Domain("nonrepresentable series conductance".into()));
        }
    }
    Ok(())
}

impl SemiconductorKernel {
    const fn junctions<'a>(
        &self,
        instance: &'a PreparedInstance,
    ) -> [(Option<&'a PreparedBranch>, usize, bool); 2] {
        [
            (instance.bulk.as_ref(), self.topology.bulk_node(), false),
            (instance.sidewall.as_ref(), self.topology.side_node(), true),
        ]
    }
}

impl Kernel for SemiconductorKernel {
    type Prepared = PreparedInstance;

    fn prepare(&self, values: &[f64], ambient_kelvin: f64) -> Result<Self::Prepared> {
        let (base, temperature) = match (self.absolute_temperature, values) {
            (false, [area, perimeter, multiplier, series, gmin]) => (
                [*area, *perimeter, *multiplier, *series, *gmin],
                ambient_kelvin,
            ),
            (true, [area, perimeter, multiplier, series, gmin, temp]) => {
                ([*area, *perimeter, *multiplier, *series, *gmin], *temp)
            }
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
        if Topology::new(&prepared) != self.topology {
            return Err(Error::Unsupported(
                "LTspice diode topology requires new artifact",
            ));
        }
        check_resistance(&prepared)?;
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
        if instance.bulk.is_none() && instance.sidewall.is_none() {
            return noise_branch(sink, "bulk_shot", 0, 1, 0.);
        }
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
