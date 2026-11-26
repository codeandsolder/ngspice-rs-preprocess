//! Prepared Berkeley BSIM1 device, without solver topology, integration or history.
//!
//! The intrinsic current and charge law is owned by `model-compact-intrinsic`.
//! This crate composes it with the source-family junctions, overlaps, series
//! resistance and noise, and exposes the shared generalized equation contract.
//!
//! Based on ngspice 47 BSIM1 load/noise equations, Copyright 1990 Regents of
//! the University of California, Hong J. Park and Thomas L. Quarles. See LICENSE.

#![forbid(unsafe_code)]

mod junction;
mod noise;

use std::sync::Arc;

use model_compact_intrinsic::{
    BoundParameters, Bsim1Bias, Bsim1IntrinsicPoint, ModelError, ModelVariant, MosPolarity,
    ParameterScope, ParameterValue, PreparedLegacyBsimInstance, PreparedLegacyBsimModel,
    evaluate_bsim1_intrinsic,
};
use model_kernel_api::{
    ABI_VERSION, Artifact, Binding, Capability, Descriptor, Equation, EquationKind, Error, Kernel,
    Parameter, Point, Result,
};

/// Numeric instance slots in physical SI units and their descriptor names.
pub const INSTANCE_PARAMETERS: [&str; 10] =
    ["l", "w", "m", "ad", "as", "pd", "ps", "nrd", "nrs", "gmin"];

/// A numeric reference instance used to determine the structural descriptor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Instance {
    /// Drawn channel length (m).
    pub length: f64,
    /// Drawn channel width (m).
    pub width: f64,
    /// Parallel multiplicity.
    pub multiplier: f64,
    /// Drain junction area (m²).
    pub drain_area: f64,
    /// Source junction area (m²).
    pub source_area: f64,
    /// Drain junction perimeter (m).
    pub drain_perimeter: f64,
    /// Source junction perimeter (m).
    pub source_perimeter: f64,
    /// Drain sheet-resistance squares.
    pub drain_squares: f64,
    /// Source sheet-resistance squares.
    pub source_squares: f64,
    /// Junction regularization conductance (S), before multiplicity.
    pub gmin: f64,
}

impl Default for Instance {
    fn default() -> Self {
        Self {
            length: 5e-6,
            width: 5e-6,
            multiplier: 1.,
            drain_area: 0.,
            source_area: 0.,
            drain_perimeter: 0.,
            source_perimeter: 0.,
            drain_squares: 1.,
            source_squares: 1.,
            gmin: 1e-12,
        }
    }
}

impl Instance {
    /// Convert to descriptor order, ready for numeric candidate preparation.
    #[must_use = "the slots supply the instance's numeric inputs"]
    pub const fn slots(self) -> [f64; 10] {
        [
            self.length,
            self.width,
            self.multiplier,
            self.drain_area,
            self.source_area,
            self.drain_perimeter,
            self.source_perimeter,
            self.drain_squares,
            self.source_squares,
            self.gmin,
        ]
    }
}

fn domain(error: &ModelError) -> Error {
    Error::Domain(error.to_string())
}

fn prepare_instance(
    model: &PreparedLegacyBsimModel,
    values: &[f64],
    ambient: f64,
) -> Result<Bsim1Prepared> {
    if model.variant() != ModelVariant::Bsim1 {
        return Err(Error::Unsupported("not a BSIM1 model"));
    }
    let Some((&gmin, geometry)) = values.split_last() else {
        return Err(Error::Shape("BSIM1 parameter count"));
    };
    if geometry.len() != INSTANCE_PARAMETERS.len() - 1 {
        return Err(Error::Shape("BSIM1 parameter count"));
    }
    if !gmin.is_finite() || gmin < 0. || !ambient.is_finite() || ambient <= 0. {
        return Err(Error::Domain("BSIM1 gmin or ambient".into()));
    }
    let params = BoundParameters::bind(
        ModelVariant::Bsim1,
        ParameterScope::Instance,
        INSTANCE_PARAMETERS
            .iter()
            .copied()
            .zip(geometry.iter().copied().map(ParameterValue::Real)),
    )
    .map_err(|error| domain(&error))?;
    let physical = model
        .prepare_instance(&params)
        .map_err(|error| domain(&error))?;
    Ok(Bsim1Prepared {
        physical,
        ambient,
        gmin,
    })
}

/// Fixed intrinsic coordinate ordinals; externally bound drain, gate, source,
/// bulk are always 0..4. Hidden nodes are allocated only for nonzero resistance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Layout {
    drain: usize,
    source: usize,
    width: usize,
}

impl Layout {
    fn new(instance: &PreparedLegacyBsimInstance) -> Self {
        let mut width = 4;
        let mut inner = |external, conductance| {
            if conductance > 0. {
                let index = width;
                width += 1;
                index
            } else {
                external
            }
        };
        let drain = inner(0, instance.drain.series_conductance);
        let source = inner(2, instance.source.series_conductance);
        Self {
            drain,
            source,
            width,
        }
    }

    fn equations(self) -> Vec<Equation> {
        let equation = |name: &str, binding| Equation {
            name: name.into(),
            binding,
            kind: EquationKind::Electrical,
        };
        let mut rows = ["drain", "gate", "source", "bulk"]
            .map(|name| equation(name, Binding::Terminal))
            .to_vec();
        if self.drain != 0 {
            rows.push(equation("drain_prime", Binding::Hidden));
        }
        if self.source != 2 {
            rows.push(equation("source_prime", Binding::Hidden));
        }
        rows
    }

    fn orientation(
        self,
        physical: &PreparedLegacyBsimInstance,
        coordinates: &[f64],
    ) -> Orientation {
        let sign = match physical.polarity {
            MosPolarity::Nmos => 1.,
            MosPolarity::Pmos => -1.,
        };
        let forward = sign * (coordinates[self.drain] - coordinates[self.source]) >= 0.;
        let (high, low) = if forward {
            (self.drain, self.source)
        } else {
            (self.source, self.drain)
        };
        let bias = Bsim1Bias {
            vds: sign * (coordinates[high] - coordinates[low]),
            vgs: sign * (coordinates[1] - coordinates[low]),
            vbs: sign * (coordinates[3] - coordinates[low]),
        };
        Orientation {
            rows: [high, 1, low, 3],
            bias,
            sign,
            forward,
        }
    }
}

#[derive(Clone, Copy)]
struct Orientation {
    rows: [usize; 4],
    bias: Bsim1Bias,
    sign: f64,
    forward: bool,
}

/// Compiled BSIM1 model, fixed series-node shape and shared immutable data.
#[derive(Debug)]
pub struct Bsim1Kernel {
    model: Arc<PreparedLegacyBsimModel>,
    layout: Layout,
}

/// Immutable instance derived from the kernel's numeric slots.
#[derive(Debug)]
pub struct Bsim1Prepared {
    physical: PreparedLegacyBsimInstance,
    ambient: f64,
    gmin: f64,
}

/// Build a BSIM1 artifact.
///
/// Geometry and gmin are numeric candidate slots.
/// The producer supplies a stable fingerprint of model and series-node shape.
/// Zero/nonzero series-resistance transitions require a new artifact.
///
/// # Errors
/// Wrong model family, invalid instance physics or descriptor identity.
pub fn bsim1_artifact(
    model: Arc<PreparedLegacyBsimModel>,
    reference: Instance,
    fingerprint: [u8; 16],
) -> Result<Arc<Artifact<Bsim1Kernel>>> {
    let slots = reference.slots();
    let initial = prepare_instance(&model, &slots, 300.15)?;
    let layout = Layout::new(&initial.physical);
    let parameters = INSTANCE_PARAMETERS
        .iter()
        .zip(slots)
        .map(|(&name, default)| Parameter {
            name: name.into(),
            default,
            changes_topology: false,
        })
        .collect();
    Artifact::new(
        Descriptor {
            family: "bsim1".into(),
            fingerprint,
            abi: ABI_VERSION,
            equations: layout.equations(),
            parameters,
            charge: Capability::Computed,
            noise: Capability::Computed,
        },
        Bsim1Kernel { model, layout },
    )
}

/// Conservative two-terminal branch contribution (current or stored charge).
fn branch(rows: &mut [f64], jacobian: &mut [f64], a: usize, b: usize, value: f64, derivative: f64) {
    if a == b {
        return;
    }
    let width = rows.len();
    rows[a] += value;
    rows[b] -= value;
    jacobian[a * width + a] += derivative;
    jacobian[b * width + b] += derivative;
    jacobian[a * width + b] -= derivative;
    jacobian[b * width + a] -= derivative;
}

fn gradient(row: &mut [f64], coordinates: [usize; 4], gradient: [f64; 3], scale: f64) {
    let [drain, gate, source, bulk] = coordinates;
    let [dd, dg, db] = gradient.map(|value| value * scale);
    row[drain] += dd;
    row[gate] += dg;
    row[bulk] += db;
    row[source] -= dd + dg + db;
}

fn stamp_intrinsic(
    out: &mut Point<'_>,
    orientation: Orientation,
    point: &Bsim1IntrinsicPoint,
    multiplier: f64,
) {
    let width = out.current.len();
    let [drain, _, source, _] = orientation.rows;
    let signed = multiplier * orientation.sign;
    let current = point.drain_current * signed;
    out.current[drain] += current;
    out.current[source] -= current;
    for (row, scale) in [(drain, multiplier), (source, -multiplier)] {
        gradient(
            &mut out.conductance[row * width..(row + 1) * width],
            orientation.rows,
            point.conductance,
            scale,
        );
    }
    for (index, row) in orientation.rows.into_iter().enumerate() {
        out.charge[row] = point.charge[index].mul_add(signed, out.charge[row]);
        gradient(
            &mut out.capacitance[row * width..(row + 1) * width],
            orientation.rows,
            point.capacitance[index],
            multiplier,
        );
    }
}

fn stamp_junctions(
    out: &mut Point<'_>,
    layout: Layout,
    prepared: &Bsim1Prepared,
    coordinates: &[f64],
    sign: f64,
) -> Result<()> {
    let p = &prepared.physical;
    let m = p.geometry.multiplier;
    for (row, data) in [(layout.drain, &p.drain), (layout.source, &p.source)] {
        let voltage = sign * (coordinates[3] - coordinates[row]);
        let (current, conductance) =
            junction::current(data.saturation_current, voltage, prepared.gmin)?;
        let (charge, capacitance) = junction::charge(data, &p.junction_law, voltage)?;
        branch(
            out.current,
            out.conductance,
            3,
            row,
            sign * m * current,
            m * conductance,
        );
        branch(
            out.charge,
            out.capacitance,
            3,
            row,
            sign * m * charge,
            m * capacitance,
        );
    }
    Ok(())
}

fn stamp_passive(
    out: &mut Point<'_>,
    layout: Layout,
    p: &PreparedLegacyBsimInstance,
    coordinates: &[f64],
) {
    let m = p.geometry.multiplier;
    for (external, inner, conductance) in [
        (0, layout.drain, p.drain.series_conductance),
        (2, layout.source, p.source.series_conductance),
    ] {
        let g = m * conductance;
        branch(
            out.current,
            out.conductance,
            external,
            inner,
            g * (coordinates[external] - coordinates[inner]),
            g,
        );
    }
    // One coherent charge law for AC and transient: the reference temperature-stage
    // overlap coefficients are also the ones consumed by original ngspice AC/PZ.
    for (row, capacitance) in [
        (layout.drain, p.overlap.drain),
        (layout.source, p.overlap.source),
        (3, p.overlap.bulk),
    ] {
        let c = m * capacitance;
        branch(
            out.charge,
            out.capacitance,
            1,
            row,
            c * (coordinates[1] - coordinates[row]),
            c,
        );
    }
}

impl Kernel for Bsim1Kernel {
    type Prepared = Bsim1Prepared;

    fn prepare(&self, values: &[f64], ambient_kelvin: f64) -> Result<Self::Prepared> {
        let prepared = prepare_instance(&self.model, values, ambient_kelvin)?;
        if Layout::new(&prepared.physical) != self.layout {
            return Err(Error::Unsupported(
                "BSIM1 series-node edit requires new artifact",
            ));
        }
        Ok(prepared)
    }

    fn evaluate(
        &self,
        prepared: &Self::Prepared,
        coordinates: &[f64],
        out: &mut Point<'_>,
    ) -> Result<()> {
        let orientation = self.layout.orientation(&prepared.physical, coordinates);
        let intrinsic = evaluate_bsim1_intrinsic(&prepared.physical, orientation.bias)
            .map_err(|error| domain(&error))?;
        out.current.fill(0.);
        out.conductance.fill(0.);
        out.charge.fill(0.);
        out.capacitance.fill(0.);
        stamp_intrinsic(
            out,
            orientation,
            &intrinsic,
            prepared.physical.geometry.multiplier,
        );
        stamp_junctions(out, self.layout, prepared, coordinates, orientation.sign)?;
        stamp_passive(out, self.layout, &prepared.physical, coordinates);
        Ok(())
    }

    fn evaluate_noise(
        &self,
        prepared: &Self::Prepared,
        coordinates: &[f64],
        frequency: f64,
        sink: &mut dyn model_kernel_api::noise::Sink,
    ) -> Result<()> {
        noise::evaluate(self.layout, prepared, coordinates, frequency, sink)
    }
}
