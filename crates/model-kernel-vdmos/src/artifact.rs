//! VDMOS producer for the shared native equation contract.
//!
//! Converts the existing constitutive laws into conservative generalized
//! residual/storage rows. Circuit node assignment and integration remain
//! outside the model; no source strings are interpreted here.

use std::sync::Arc;

use model_kernel_api::{
    ABI_VERSION, Artifact, Binding, Capability, Descriptor, Equation, EquationKind, Error, Kernel,
    Parameter, Point, Result,
    noise::{Injection, Sink, Source},
    taylor::{
        MAX_ORDER, TaylorCapability, TaylorInput, TaylorPoint,
        arithmetic::{Scalar, Series},
    },
};

use super::{Bias, Conductance, Evaluation, Instance, PreparedInstance, PreparedModel};

// Three exposed electrical, two exposed thermal, four internal electrical,
// and one internal relaxation coordinate; all optional nodes are coalesced
// before creating the descriptor.
const MAX_ROWS: usize = 10;

#[derive(Clone, Copy, Debug)]
struct Layout {
    external: [usize; 3],
    intrinsic: [usize; 3],
    diode: usize,
    thermal: Option<[usize; 2]>,
    recovery: Option<usize>,
    width: usize,
}

impl Layout {
    fn new(model: &PreparedModel, self_heating: bool) -> (Self, Vec<Equation>) {
        let mut equations = Vec::with_capacity(MAX_ROWS);
        let mut add = |name: &str, kind, binding| {
            let index = equations.len();
            equations.push(Equation {
                name: name.into(),
                kind,
                binding,
            });
            index
        };
        let external = ["drain", "gate", "source"]
            .map(|name| add(name, EquationKind::Electrical, Binding::Terminal));
        let thermal = self_heating.then(|| {
            ["junction", "case"].map(|name| add(name, EquationKind::Thermal, Binding::Terminal))
        });
        let series = model.parameters().series;
        let mut inner = |index, name, resistance: f64| {
            if resistance > 0. {
                add(name, EquationKind::Electrical, Binding::Hidden)
            } else {
                index
            }
        };
        let intrinsic = [
            inner(external[0], "drain_prime", series.rd),
            inner(external[1], "gate_prime", series.rg),
            inner(external[2], "source_prime", series.rs),
        ];
        let diode = inner(external[2], "body_prime", series.rb);
        let d = model.parameters().diode;
        let recovery = (d.tt > 0. && d.vp > 0.)
            .then(|| add("recovery", EquationKind::Internal, Binding::Hidden));
        let width = equations.len();
        (
            Self {
                external,
                intrinsic,
                diode,
                thermal,
                recovery,
                width,
            },
            equations,
        )
    }
}

/// One immutable compiled model, including a fixed structural option set.
#[derive(Debug)]
pub struct VdmosKernel {
    model: Arc<PreparedModel>,
    layout: Layout,
}

/// Prepared instance-specific data. The model itself is shared by the artifact.
#[derive(Debug)]
pub struct VdmosInstance {
    multiplier: f64,
    ambient_kelvin: f64,
}

/// Compile a topology-specific artifact from validated model data.
///
/// The producer supplies its stable content fingerprint, including the
/// self-heating setting. Model parameter edits create a fresh artifact;
/// multiplicity is a nonstructural candidate slot.
///
/// # Errors
/// Invalid fingerprint or a bias-dependent drain resistance without a
/// permanently present drain series branch.
pub fn artifact(
    model: Arc<PreparedModel>,
    self_heating: bool,
    fingerprint: [u8; 16],
) -> Result<Arc<Artifact<VdmosKernel>>> {
    if model.parameters().series.rd == 0. && model.parameters().series.quasi_saturation.is_some() {
        return Err(Error::Unsupported(
            "quasi-saturation without a drain series branch",
        ));
    }
    let (layout, equations) = Layout::new(&model, self_heating);
    Artifact::new(
        Descriptor {
            family: "vdmos".into(),
            fingerprint,
            abi: ABI_VERSION,
            taylor: TaylorCapability::Computed {
                max_order: MAX_ORDER,
            },
            equations,
            parameters: vec![Parameter {
                name: "multiplier".into(),
                default: 1.,
                changes_topology: false,
            }],
            charge: Capability::Computed,
            noise: Capability::Computed,
            trial: model_kernel_api::trial::TrialCapabilities::ENGINE,
        },
        VdmosKernel { model, layout },
    )
}

// Analytic local derivatives, propagated through the coordinate conversion
// and elementary branch assembly. No independent physical model is copied.
#[derive(Clone, Copy)]
struct Differential<S = f64> {
    value: S,
    derivative: [S; MAX_ROWS],
}

impl<S: Scalar> Differential<S> {
    fn constant(value: S) -> Self {
        Self {
            value,
            derivative: [S::constant(0.); MAX_ROWS],
        }
    }
    fn coordinate(value: S, row: usize) -> Self {
        let mut result = Self::constant(value);
        result.derivative[row] = S::constant(1.);
        result
    }
    fn compose<const N: usize>(value: S, terms: [(Self, S); N]) -> Self {
        let mut result = Self::constant(value);
        for (input, slope) in terms {
            for (dst, dx) in result.derivative.iter_mut().zip(input.derivative) {
                *dst = slope.mul_add(dx, *dst);
            }
        }
        result
    }
    fn minus(self, other: Self) -> Self {
        Self::compose(
            self.value - other.value,
            [(self, S::constant(1.)), (other, S::constant(-1.))],
        )
    }
    fn plus(self, other: Self) -> Self {
        Self::compose(
            self.value + other.value,
            [(self, S::constant(1.)), (other, S::constant(1.))],
        )
    }
    fn times(self, other: Self) -> Self {
        Self::compose(
            self.value * other.value,
            [(self, other.value), (other, self.value)],
        )
    }
    fn scale(self, factor: S) -> Self {
        Self::compose(self.value * factor, [(self, factor)])
    }
}

trait Stamp<S: Scalar> {
    fn add(&mut self, row: usize, term: Differential<S>, storage: bool);
    fn branch(&mut self, positive: usize, negative: usize, term: Differential<S>, storage: bool) {
        if positive != negative {
            self.add(positive, term, storage);
            self.add(negative, term.scale(S::constant(-1.)), storage);
        }
    }
}

struct Assembly<'a, 'b> {
    point: &'a mut Point<'b>,
    width: usize,
}

impl Stamp<f64> for Assembly<'_, '_> {
    fn add(&mut self, row: usize, term: Differential, storage: bool) {
        let (residual, jacobian) = if storage {
            (&mut self.point.charge, &mut self.point.capacitance)
        } else {
            (&mut self.point.current, &mut self.point.conductance)
        };
        residual[row] += term.value;
        let start = row * self.width;
        for (entry, derivative) in jacobian[start..start + self.width]
            .iter_mut()
            .zip(term.derivative)
        {
            *entry += derivative;
        }
    }
}

struct TaylorAssembly<'a, 'b> {
    point: &'a mut TaylorPoint<'b>,
    width: usize,
    order: usize,
}

impl Stamp<Series> for TaylorAssembly<'_, '_> {
    fn add(&mut self, row: usize, term: Differential<Series>, storage: bool) {
        let (residual, jacobian) = if storage {
            (&mut self.point.charge, &mut self.point.capacitance)
        } else {
            (&mut self.point.current, &mut self.point.conductance)
        };
        let width = self.width;
        for degree in 0..=self.order {
            residual[degree * width + row] += term.value.coefficient(degree);
            let start = (degree * width + row) * width;
            for (entry, derivative) in jacobian[start..start + width]
                .iter_mut()
                .zip(&term.derivative)
            {
                *entry += derivative.coefficient(degree);
            }
        }
    }
}

struct OperatingPoint<S = f64> {
    bias: Bias<S>,
    vgs: Differential<S>,
    vds: Differential<S>,
    external_vds: Differential<S>,
    diode: Differential<S>,
    recovery: Differential<S>,
    temperature: Differential<S>,
    coordinates: [Differential<S>; MAX_ROWS],
}

impl VdmosKernel {
    fn operating_point<S: Scalar>(
        &self,
        prepared: &VdmosInstance,
        x: &[S],
    ) -> Result<OperatingPoint<S>> {
        if x.len() != self.layout.width {
            return Err(Error::Shape("VDMOS coordinate count"));
        }
        let mut coordinates = [Differential::constant(S::constant(0.)); MAX_ROWS];
        for (index, (slot, &value)) in coordinates.iter_mut().zip(x).enumerate() {
            *slot = Differential::coordinate(value, index);
        }
        let [d, g, s] = self.layout.intrinsic.map(|index| coordinates[index]);
        let [de, _, se] = self.layout.external.map(|index| coordinates[index]);
        let vgs = g.minus(s);
        let vds = d.minus(s);
        let external_vds = de.minus(se);
        let diode = coordinates[self.layout.diode].minus(de);
        let recovery = self
            .layout
            .recovery
            .map_or(Differential::constant(S::constant(0.)), |i| coordinates[i]);
        let temperature = self.layout.thermal.map_or_else(
            || Differential::constant(S::constant(prepared.ambient_kelvin)),
            |[j, _]| coordinates[j],
        );
        if let Some([_, case]) = self.layout.thermal
            && (!coordinates[case].value.is_finite() || coordinates[case].value.value() <= 0.)
        {
            return Err(Error::Domain("VDMOS case temperature".into()));
        }
        let bias = Bias {
            vgs: vgs.value,
            vds: vds.value,
            vds_external: external_vds.value,
            vdiode: diode.value,
            recovery_node: recovery.value,
            temperature: temperature.value,
        };
        Ok(OperatingPoint {
            bias,
            vgs,
            vds,
            external_vds,
            diode,
            recovery,
            temperature,
            coordinates,
        })
    }

    fn device(&self, prepared: &VdmosInstance) -> PreparedInstance<'_> {
        // Multiplicity was validated during preparation.
        PreparedInstance {
            model: &self.model,
            multiplier: prepared.multiplier,
            self_heating: self.layout.thermal.is_some(),
        }
    }

    fn evaluation(&self, prepared: &VdmosInstance, bias: Bias) -> Result<Evaluation> {
        self.device(prepared)
            .evaluate(bias)
            .map_err(|error| Error::Domain(error.to_string()))
    }

    fn intrinsic<S: Scalar>(
        &self,
        point: &OperatingPoint<S>,
        evaluation: &Evaluation<S>,
        out: &mut impl Stamp<S>,
    ) -> Result<()> {
        let [drain, gate, source] = self.layout.intrinsic;
        let de = self.layout.external[0];
        let dp = self.layout.diode;
        let channel = Differential::compose(
            evaluation.channel.value,
            [
                (point.vgs, evaluation.channel.d_vgs),
                (point.vds, evaluation.channel.d_vds),
                (point.temperature, evaluation.channel.d_temperature),
            ],
        );
        out.branch(drain, source, channel, false);
        let current = evaluation.diode.current;
        let mut diode = Differential::compose(
            current.value,
            [
                (point.diode, current.d_voltage),
                (point.temperature, current.d_temperature),
            ],
        );
        match (self.layout.recovery, evaluation.diode.recovery) {
            (Some(row), Some(recovery)) => {
                let residual = Differential::compose(
                    recovery.residual,
                    [
                        (point.recovery, recovery.d_node),
                        (point.diode, recovery.d_voltage),
                        (point.temperature, recovery.d_temperature),
                    ],
                );
                let stored = Differential::compose(
                    recovery.stored_charge.value,
                    [
                        (point.recovery, recovery.stored_charge.d_voltage),
                        (point.temperature, recovery.stored_charge.d_temperature),
                    ],
                );
                let gain = Differential::compose(
                    recovery.diode_charge_rate_gain,
                    [(point.temperature, recovery.d_gain_d_temperature)],
                );
                // Eliminate the non-exact differential gain*dQs/dt from KCL
                // using the independent recovery equation dQs/dt = -Fq.
                // This is a row transformation, not Q= gain*Qs, which would
                // incorrectly introduce a Qs*drain(gain)/dt term.
                diode = diode.minus(gain.times(residual));
                out.add(row, residual, false);
                out.add(row, stored, true);
            }
            (None, None) => {}
            _ => {
                return Err(Error::Domain(
                    "temperature changed recovery topology".into(),
                ));
            }
        }
        out.branch(dp, de, diode, false);
        let charge = |q: super::Charge<S>, voltage| {
            Differential::compose(
                q.value,
                [(voltage, q.d_voltage), (point.temperature, q.d_temperature)],
            )
        };
        out.branch(gate, source, charge(evaluation.qgs, point.vgs), true);
        out.branch(
            gate,
            drain,
            charge(evaluation.qgd, point.vgs.minus(point.vds)),
            true,
        );
        out.branch(
            dp,
            de,
            charge(evaluation.diode.depletion, point.diode)
                .plus(charge(evaluation.diode.diffusion, point.diode)),
            true,
        );
        Ok(())
    }

    fn series_and_heat<S: Scalar>(
        &self,
        point: &OperatingPoint<S>,
        evaluation: &Evaluation<S>,
        ambient: f64,
        out: &mut impl Stamp<S>,
    ) -> Result<()> {
        let [de, ge, se] = self.layout.external;
        let [drain, gate, source] = self.layout.intrinsic;
        let mut power = Differential::compose(
            evaluation.heat.intrinsic,
            [
                (point.vgs, evaluation.heat.d_vgs),
                (point.vds, evaluation.heat.d_vds),
                (point.diode, evaluation.heat.d_vdiode),
                (point.temperature, evaluation.heat.d_temperature),
            ],
        );
        let mut branch = |external: usize,
                          internal: usize,
                          conductance: Conductance<S>,
                          dissipates: bool|
         -> Result<()> {
            if external == internal {
                if conductance.value.is_some() {
                    return Err(Error::Domain("series branch changed topology".into()));
                }
                return Ok(());
            }
            let value = conductance
                .value
                .ok_or_else(|| Error::Domain("series branch collapsed at bias".into()))?;
            let drop = point.coordinates[external].minus(point.coordinates[internal]);
            let differential_g = Differential::compose(
                value,
                [
                    (point.external_vds, conductance.d_vds_external),
                    (point.temperature, conductance.d_temperature),
                ],
            );
            let current = differential_g.times(drop);
            out.branch(external, internal, current, false);
            if dissipates {
                power = power.plus(current.times(drop));
            }
            Ok(())
        };
        branch(de, drain, evaluation.series.drain, true)?;
        branch(ge, gate, evaluation.series.gate, false)?;
        branch(se, source, evaluation.series.source, true)?;
        branch(se, self.layout.diode, evaluation.series.body, true)?;
        out.branch(
            de,
            se,
            point.external_vds.scale(evaluation.series.shunt),
            false,
        );
        if let Some([junction, case]) = self.layout.thermal {
            let tcase = point.coordinates[case];
            let heat = point
                .temperature
                .minus(tcase)
                .scale(evaluation.heat.junction_to_case);
            out.branch(junction, case, heat, false);
            out.add(junction, power.scale(S::constant(-1.)), false);
            out.add(
                case,
                tcase
                    .minus(Differential::constant(S::constant(ambient)))
                    .scale(evaluation.heat.case_to_ambient),
                false,
            );
            out.add(
                junction,
                point
                    .temperature
                    .scale(evaluation.heat.junction_capacitance),
                true,
            );
        }
        Ok(())
    }
}

impl Kernel for VdmosKernel {
    type Prepared = VdmosInstance;

    fn prepare(&self, values: &[f64], ambient_kelvin: f64) -> Result<Self::Prepared> {
        let [multiplier] = values else {
            return Err(Error::Shape("VDMOS parameter count"));
        };
        if !ambient_kelvin.is_finite() || ambient_kelvin <= 0. {
            return Err(Error::Domain("VDMOS ambient temperature".into()));
        }
        self.model
            .bind(Instance {
                multiplier: *multiplier,
                self_heating: self.layout.thermal.is_some(),
            })
            .map_err(|error| Error::Domain(error.to_string()))?;
        Ok(VdmosInstance {
            multiplier: *multiplier,
            ambient_kelvin,
        })
    }

    fn evaluate(
        &self,
        prepared: &Self::Prepared,
        coordinates: &[f64],
        out: &mut Point<'_>,
    ) -> Result<()> {
        let width = self.layout.width;
        if out.current.len() != width
            || out.charge.len() != width
            || out.conductance.len() != width * width
            || out.capacitance.len() != width * width
        {
            return Err(Error::Shape("VDMOS point"));
        }
        let point = self.operating_point(prepared, coordinates)?;
        let evaluation = self.evaluation(prepared, point.bias)?;
        out.current.fill(0.);
        out.charge.fill(0.);
        out.conductance.fill(0.);
        out.capacitance.fill(0.);
        let mut assembly = Assembly { point: out, width };
        self.intrinsic(&point, &evaluation, &mut assembly)?;
        self.series_and_heat(&point, &evaluation, prepared.ambient_kelvin, &mut assembly)
    }

    fn evaluate_taylor(
        &self,
        prepared: &Self::Prepared,
        input: TaylorInput<'_>,
        out: &mut TaylorPoint<'_>,
    ) -> Result<()> {
        let width = self.layout.width;
        let mut coordinates = [Series::default(); MAX_ROWS];
        for (index, coordinate) in coordinates[..width].iter_mut().enumerate() {
            *coordinate = input.coordinate(width, index)?;
        }
        let rows = width * (input.order + 1);
        let matrices = rows * width;
        if out.current.len() != rows
            || out.charge.len() != rows
            || out.conductance.len() != matrices
            || out.capacitance.len() != matrices
        {
            return Err(Error::Shape("VDMOS Taylor point"));
        }
        let point = self.operating_point(prepared, &coordinates[..width])?;
        let evaluation = self
            .device(prepared)
            .evaluate_physical(point.bias)
            .map_err(|error| Error::Domain(error.to_string()))?;
        out.current.fill(0.);
        out.charge.fill(0.);
        out.conductance.fill(0.);
        out.capacitance.fill(0.);
        let mut assembly = TaylorAssembly {
            point: out,
            width,
            order: input.order,
        };
        self.intrinsic(&point, &evaluation, &mut assembly)?;
        self.series_and_heat(&point, &evaluation, prepared.ambient_kelvin, &mut assembly)
    }

    fn evaluate_noise(
        &self,
        prepared: &Self::Prepared,
        coordinates: &[f64],
        frequency: f64,
        sink: &mut dyn Sink,
    ) -> Result<()> {
        let point = self.operating_point(prepared, coordinates)?;
        let evaluation = self.evaluation(prepared, point.bias)?;
        let densities = self
            .device(prepared)
            .noise_density(&evaluation, frequency)
            .map_err(|error| Error::Domain(error.to_string()))?;
        let [d, _, s] = self.layout.intrinsic;
        let [de, _, se] = self.layout.external;
        for (name, positive, negative, density) in [
            ("rd", de, d, densities.drain),
            ("rs", se, s, densities.source),
            ("channel", d, s, densities.channel),
            ("flicker", d, s, densities.flicker),
        ] {
            if positive != negative {
                let injections = [
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
                ];
                sink.push(Source {
                    name,
                    density,
                    injections: &injections,
                })?;
            }
        }
        Ok(())
    }
}
