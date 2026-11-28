//! Composition of the intrinsic EKV channel with explicitly prepared
//! junction, overlap, shunt and series branches.
//!
//! Topology is fixed at
//! artifact creation; no numerical conductance substitutes for an absent
//! series resistance.

use std::sync::Arc;

use model_kernel_api::{
    ABI_VERSION, Artifact, Binding, Capability, Descriptor, Equation, EquationKind, Error, Kernel,
    Parameter, Point, Result,
    noise::{Injection, Sink, Source},
    taylor::TaylorCapability,
    trial::{TrialCapabilities, TrialPolicy, TrialReference},
};

use crate::{
    Geometry, Model, Prepared,
    artifact::Noise,
    junction::{Junction, PreparedJunction},
};

const TERMINALS: [&str; 4] = ["drain", "gate", "source", "bulk"];
const INNER: [&str; 4] = ["drain_prime", "gate_prime", "source_prime", "bulk_prime"];
const RESISTOR_NOISE: [&str; 4] = ["rd", "rg", "rs", "rb"];
const BOLTZMANN: f64 = 1.380_622_6e-23;

/// Nominal resistance temperature coefficients, in 1/K and 1/K².
#[derive(Clone, Copy, Debug, Default)]
pub struct ResistanceTemperature {
    pub linear: f64,
    pub quadratic: f64,
}

/// Resolved full-device model and per-finger parasitics. The two
/// junctions are drain-bulk and source-bulk, respectively.
#[derive(Clone, Copy, Debug)]
pub struct Device {
    pub channel: Model,
    pub junctions: [Junction; 2],
    /// Gate-drain, gate-source and gate-bulk overlap capacitance per
    /// unit of drawn width, width and length, respectively.
    pub overlap: [f64; 3],
    /// Per-finger drain, gate, source and bulk resistance.
    pub resistance: [f64; 4],
    pub resistance_temperature: [ResistanceTemperature; 4],
    /// Per-finger drain-source shunt conductance.
    pub shunt: f64,
    pub noise: Option<Noise>,
}

#[derive(Clone, Copy, Debug)]
struct Layout {
    inner: [usize; 4],
    width: usize,
}

impl Layout {
    fn new(resistance: [f64; 4]) -> (Self, Vec<Equation>) {
        let equation = |name: &str, binding| Equation {
            name: name.into(),
            kind: EquationKind::Electrical,
            binding,
        };
        let mut equations: Vec<_> = TERMINALS
            .iter()
            .map(|name| equation(name, Binding::Terminal))
            .collect();
        let inner = std::array::from_fn(|i| {
            if resistance[i] > 0. {
                let row = equations.len();
                equations.push(equation(INNER[i], Binding::Hidden));
                row
            } else {
                i
            }
        });
        (
            Self {
                inner,
                width: equations.len(),
            },
            equations,
        )
    }

    fn bias(self, values: &[f64]) -> Result<[f64; 4]> {
        if values.len() != self.width || values.iter().any(|v| !v.is_finite()) {
            return Err(Error::Shape("EKV device coordinates"));
        }
        Ok(self.inner.map(|i| values[i]))
    }
}

#[derive(Debug)]
pub struct DeviceKernel {
    device: Device,
    layout: Layout,
}

#[derive(Debug)]
pub struct Instance {
    channel: Prepared,
    junctions: [PreparedJunction; 2],
    overlap: [f64; 3],
    series_conductance: [f64; 4],
    shunt: f64,
    temperature: f64,
}

impl Device {
    /// Build a model artifact with explicit series topology. Geometry slots
    /// describe per-element dimensions. Junction capacitance/current and
    /// ohmic resistance are already resolved per finger; if the source
    /// changes their geometry-derived values, rebind the artifact.
    ///
    /// # Errors
    /// Invalid physical parameters, nominal geometry or identity.
    pub fn artifact(
        self,
        geometry: Geometry,
        fingerprint: [u8; 16],
    ) -> Result<Arc<Artifact<DeviceKernel>>> {
        if self
            .resistance
            .into_iter()
            .chain(self.overlap)
            .chain([self.shunt])
            .any(|x| !x.is_finite() || x < 0.)
            || self
                .resistance_temperature
                .iter()
                .any(|t| !t.linear.is_finite() || !t.quadratic.is_finite())
            || self.noise.is_some_and(|n| {
                !n.flicker_coefficient.is_finite()
                    || n.flicker_coefficient < 0.
                    || !n.flicker_frequency_exponent.is_finite()
                    || n.flicker_frequency_exponent <= 0.
            })
        {
            return Err(Error::Domain("invalid EKV device parameters".into()));
        }
        let (layout, equations) = Layout::new(self.resistance);
        let kernel = DeviceKernel {
            device: self,
            layout,
        };
        let values = [
            geometry.length,
            geometry.width,
            geometry.parallel,
            geometry.series,
        ];
        kernel.prepare(&values, self.channel.nominal_temperature)?;
        let parameters = ["length", "width", "parallel", "series"]
            .into_iter()
            .zip(values)
            .map(|(name, default)| Parameter {
                name: name.into(),
                default,
                changes_topology: false,
            })
            .collect();
        Artifact::new(
            Descriptor {
                abi: ABI_VERSION,
                family: "ekv26".into(),
                fingerprint,
                equations,
                parameters,
                charge: Capability::Computed,
                noise: if self.noise.is_some()
                    || self.resistance.iter().any(|r| *r > 0.)
                    || self.shunt > 0.
                {
                    Capability::Computed
                } else {
                    Capability::Zero
                },
                trial: TrialCapabilities {
                    limiting: TrialPolicy::Kernel,
                    ..TrialCapabilities::ENGINE
                },
                taylor: TaylorCapability::Unavailable,
            },
            kernel,
        )
    }
}

fn stamp(
    values: &mut [f64],
    matrix: &mut [f64],
    width: usize,
    ends: [usize; 2],
    value: f64,
    slope: f64,
) {
    let [p, n] = ends;
    values[p] += value;
    values[n] -= value;
    matrix[p * width + p] += slope;
    matrix[p * width + n] -= slope;
    matrix[n * width + p] -= slope;
    matrix[n * width + n] += slope;
}

fn noise_branch(sink: &mut dyn Sink, name: &str, ends: [usize; 2], density: f64) -> Result<()> {
    if !density.is_finite() || density < 0. {
        return Err(Error::Domain("nonfinite EKV noise density".into()));
    }
    let injections = [
        Injection {
            row: ends[0],
            re: 1.,
            im: 0.,
        },
        Injection {
            row: ends[1],
            re: -1.,
            im: 0.,
        },
    ];
    sink.push(Source {
        name,
        density,
        injections: &injections,
    })
}

impl Kernel for DeviceKernel {
    type Prepared = Instance;

    fn prepare(&self, values: &[f64], temperature: f64) -> Result<Instance> {
        let &[length, width, parallel, series] = values else {
            return Err(Error::Shape("EKV device geometry slots"));
        };
        let geometry = Geometry {
            length,
            width,
            parallel,
            series,
        };
        let d = self.device;
        let channel = d.channel.prepare_segmented(geometry, temperature)?;
        let delta = temperature - d.channel.nominal_temperature;
        let mut conductance = [0.; 4];
        for (i, dst) in conductance.iter_mut().enumerate() {
            if d.resistance[i] > 0. {
                let tc = d.resistance_temperature[i];
                let factor = tc.quadratic.mul_add(delta, tc.linear).mul_add(delta, 1.);
                *dst = parallel / (d.resistance[i] * factor);
                if !dst.is_finite() || *dst <= 0. {
                    return Err(Error::Domain(
                        "invalid EKV series resistance at temperature".into(),
                    ));
                }
            }
        }
        let [drain, source] = d.junctions;
        let nominal = d.channel.nominal_temperature;
        let junctions = [
            drain.prepare(nominal, temperature, channel.vt, parallel)?,
            source.prepare(nominal, temperature, channel.vt, parallel)?,
        ];
        let overlap = [
            d.overlap[0] * width * parallel,
            d.overlap[1] * width * parallel,
            d.overlap[2] * length * series * parallel,
        ];
        let shunt = d.shunt * parallel;
        if overlap.into_iter().chain([shunt]).any(|x| !x.is_finite()) {
            return Err(Error::Domain("nonfinite EKV device parasitic".into()));
        }
        Ok(Instance {
            channel,
            junctions,
            overlap,
            series_conductance: conductance,
            shunt,
            temperature,
        })
    }

    fn evaluate(
        &self,
        instance: &Instance,
        coordinates: &[f64],
        out: &mut Point<'_>,
    ) -> Result<()> {
        let layout = self.layout;
        let width = layout.width;
        let voltage = layout.bias(coordinates)?;
        if out.current.len() != width
            || out.charge.len() != width
            || out.conductance.len() != width * width
            || out.capacitance.len() != width * width
        {
            return Err(Error::Shape("EKV device output"));
        }
        let linear = instance.channel.linearize(voltage)?;
        out.current.fill(0.);
        out.charge.fill(0.);
        out.conductance.fill(0.);
        out.capacitance.fill(0.);
        for (r, row) in layout.inner.into_iter().enumerate() {
            out.current[row] = linear.point.current[r];
            out.charge[row] = linear.point.charge[r];
            for (c, col) in layout.inner.into_iter().enumerate() {
                out.conductance[row * width + col] = linear.conductance[r][c];
                out.capacitance[row * width + col] = linear.capacitance[r][c];
            }
        }
        let [drain, gate, source, bulk] = layout.inner;
        let sign = instance.channel.sign;
        for (i, terminal) in [drain, source].into_iter().enumerate() {
            let eval = instance.junctions[i]
                .evaluate(sign * (coordinates[bulk] - coordinates[terminal]))?;
            stamp(
                out.current,
                out.conductance,
                width,
                [bulk, terminal],
                sign * eval.current,
                eval.conductance,
            );
            stamp(
                out.charge,
                out.capacitance,
                width,
                [bulk, terminal],
                sign * eval.charge,
                eval.capacitance,
            );
        }
        for (i, terminal) in [drain, source, bulk].into_iter().enumerate() {
            let capacitance = instance.overlap[i];
            stamp(
                out.charge,
                out.capacitance,
                width,
                [gate, terminal],
                capacitance * (coordinates[gate] - coordinates[terminal]),
                capacitance,
            );
        }
        for (i, inner) in layout.inner.into_iter().enumerate() {
            if i != inner {
                let conductance = instance.series_conductance[i];
                stamp(
                    out.current,
                    out.conductance,
                    width,
                    [i, inner],
                    conductance * (coordinates[i] - coordinates[inner]),
                    conductance,
                );
            }
        }
        stamp(
            out.current,
            out.conductance,
            width,
            [drain, source],
            instance.shunt * (coordinates[drain] - coordinates[source]),
            instance.shunt,
        );
        Ok(())
    }

    fn limit_trial(
        &self,
        instance: &Instance,
        reference: TrialReference<'_>,
        coordinates: &mut [f64],
    ) -> Result<()> {
        let before = self.layout.bias(reference.coordinates())?;
        let _ = self.layout.bias(coordinates)?;
        let [d, _, s, b] = self.layout.inner;
        let sign = instance.channel.sign;
        for (i, terminal) in [d, s].into_iter().enumerate() {
            if let Some(limiter) = instance.junctions[i].limiter() {
                let old = sign * (before[3] - before[2 * i]);
                let raw = sign * (coordinates[b] - coordinates[terminal]);
                let limited = limiter.limit(raw, old);
                if limited.to_bits() != raw.to_bits() {
                    coordinates[terminal] = (-sign).mul_add(limited, coordinates[b]);
                }
            }
        }
        Ok(())
    }

    fn evaluate_noise(
        &self,
        instance: &Instance,
        coordinates: &[f64],
        frequency: f64,
        sink: &mut dyn Sink,
    ) -> Result<()> {
        let voltage = self.layout.bias(coordinates)?;
        if !frequency.is_finite() || frequency <= 0. {
            return Err(Error::Domain("invalid EKV device noise frequency".into()));
        }
        let [d, _, s, _] = self.layout.inner;
        let thermal_factor = 4. * BOLTZMANN * instance.temperature;
        if let Some(noise) = self.device.noise {
            let point = instance.channel.linearize(voltage)?;
            let thermal = thermal_factor * point.point.thermal_conductance;
            let flicker = noise.flicker_coefficient * point.channel_transconductance.powi(2)
                / (instance.channel.area_cox * frequency.powf(noise.flicker_frequency_exponent));
            noise_branch(sink, "channel", [d, s], thermal)?;
            noise_branch(sink, "flicker", [d, s], flicker)?;
        }
        for (i, inner) in self.layout.inner.into_iter().enumerate() {
            if i != inner {
                noise_branch(
                    sink,
                    RESISTOR_NOISE[i],
                    [i, inner],
                    thermal_factor * instance.series_conductance[i],
                )?;
            }
        }
        if instance.shunt > 0. {
            noise_branch(sink, "rds", [d, s], thermal_factor * instance.shunt)?;
        }
        Ok(())
    }
}
