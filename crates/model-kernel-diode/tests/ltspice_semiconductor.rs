//! Independent LTspice numeric witnesses, with resolved numeric parameters.
use model_kernel_diode::{
    compile_ltspice,
    ltspice_semiconductor::{Instance, PreparedInstance, SourceModel},
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn assignments(text: &str) -> Result<Vec<(&str, f64)>> {
    text.split_whitespace()
        .map(|pair| {
            let (name, value) = pair.split_once('=').ok_or("missing assignment")?;
            Ok((name, value.parse()?))
        })
        .collect()
}

fn source_cases(text: &str) -> Result<Vec<(&str, SourceModel, Instance)>> {
    text.lines()
        .map(|row| {
            let mut fields = row.split('|');
            let name = fields.next().ok_or("missing name")?;
            let model = compile_ltspice(assignments(fields.next().ok_or("missing model")?)?)?;
            let mut instance = Instance::default();
            for (key, value) in assignments(fields.next().ok_or("missing instance")?)? {
                match key {
                    "area" => instance.area = value,
                    "m" => instance.multiplier = value,
                    "n" => instance.series = value,
                    "perim" => instance.perimeter = Some(value),
                    "temp" => instance.temperature = value + 273.15,
                    // PJ is a native spelling ignored by this source.
                    "pj" => {}
                    _ => return Err(format!("unknown fixture field {key}").into()),
                }
            }
            Ok((name, model, instance))
        })
        .collect()
}

fn cases(text: &str) -> Result<Vec<(&str, PreparedInstance)>> {
    source_cases(text)?
        .into_iter()
        .map(|(name, model, instance)| Ok((name, model.bind(instance)?)))
        .collect()
}

fn close(name: &str, voltage: f64, actual: f64, expected: f64, abs: f64, rel: f64) -> Result<()> {
    if (actual - expected).abs() <= rel.mul_add(expected.abs(), abs) {
        Ok(())
    } else {
        Err(format!("{name} at {voltage}: {actual:e} != {expected:e}").into())
    }
}

fn dc_rows(cases: &[(&str, PreparedInstance)], data: &str, rel: f64) -> Result<usize> {
    let mut count = 0;
    for row in data.lines().skip(1) {
        let mut fields = row.split('\t');
        let voltage: f64 = fields.next().ok_or("missing bias")?.parse()?;
        for ((name, device), expected) in cases.iter().zip(fields) {
            let mut actual = 0.;
            let mut conductance = 0.;
            for branch in [device.bulk, device.sidewall].into_iter().flatten() {
                assert_eq!(branch.resistance().to_bits(), 0_f64.to_bits());
                let value = branch.evaluate(voltage)?.total;
                actual += value.current;
                conductance += value.conductance;
            }
            let expected = expected.parse()?;
            // Sweep accumulation near zero leaves a bias uncertainty at
            // the scale of the sweep increment, not the tiny residual bias.
            let bias_uncertainty = if voltage.abs() < f64::EPSILON {
                2. * f64::EPSILON * conductance.abs()
            } else {
                0.
            };
            close(
                name,
                voltage,
                actual,
                expected,
                2e-18 + bias_uncertainty,
                rel,
            )?;
            count += 1;
        }
    }
    Ok(count)
}

#[test]
fn intrinsic_dc() -> Result<()> {
    let cases = cases(include_str!("fixtures/ltspice/intrinsic-cases.tsv"))?;
    assert_eq!(
        dc_rows(
            &cases,
            include_str!("fixtures/ltspice/intrinsic-dc.tsv"),
            3e-8
        )?,
        236 * 15,
    );
    Ok(())
}

#[test]
fn transport_ceiling() -> Result<()> {
    let cases = cases(include_str!("fixtures/ltspice/clip-cases.tsv"))?;
    // The vendor numerically locates the combined forward-law tangent.
    // The reconstructed mathematical root differs by a few ppm.
    assert_eq!(
        dc_rows(&cases, include_str!("fixtures/ltspice/clip-dc.tsv"), 2e-5)?,
        401 * 14,
    );
    Ok(())
}

#[test]
fn intrinsic_ac_charge() -> Result<()> {
    let cases = cases(include_str!("fixtures/ltspice/charge-cases.tsv"))?;
    let mut count = 0;
    for row in include_str!("fixtures/ltspice/charge-ac-compact.tsv").lines() {
        let mut fields = row.split('\t');
        let voltage: f64 = fields.next().ok_or("missing bias")?.parse()?;
        for ((name, device), expected) in cases.iter().zip(fields) {
            if device.independent_sidewall {
                continue;
            }
            let (real, imaginary) = expected.split_once(',').ok_or("missing complex field")?;
            let mut g = 0.;
            let mut c = 0.;
            for branch in [device.bulk, device.sidewall].into_iter().flatten() {
                let evaluated = branch.evaluate(voltage)?.total;
                g += evaluated.conductance;
                c += evaluated.capacitance;
            }
            close(name, voltage, g, real.parse()?, 1e-16, 3e-8)?;
            close(
                name,
                voltage,
                c * std::f64::consts::TAU * 1000.,
                imaginary.parse()?,
                1e-18,
                if device.sidewall.is_some() {
                    1e-6
                } else {
                    3e-8
                },
            )?;
            count += 1;
        }
    }
    assert_eq!(count, 11 * 24);
    Ok(())
}

// The test circuit consists of a voltage clamp, ground and zero, one or two
// independently connected internal diode nodes. Solve KCL independently of
// the consumer and reduce the ABI's complex nodal Jacobian analytically.
use model_kernel_api::{
    Artifact, PointBuffer, Prepared,
    noise::{Sink, Source},
};
use model_kernel_diode::artifact::ltspice::{SemiconductorKernel, artifact};

struct Circuit {
    artifact: std::sync::Arc<Artifact<SemiconductorKernel>>,
    candidate: Prepared<SemiconductorKernel>,
    point: PointBuffer,
    coordinates: Vec<f64>,
}

impl Circuit {
    fn new(model: &SourceModel, instance: Instance) -> Result<Self> {
        let artifact = artifact(model, instance, true, [5; 16])?;
        let values = artifact
            .descriptor()
            .parameters
            .iter()
            .map(|p| p.default)
            .collect::<Vec<_>>();
        let candidate = artifact.prepare(&values, 300.15)?;
        let width = artifact.descriptor().equations.len();
        Ok(Self {
            artifact,
            candidate,
            point: PointBuffer::new(width)?,
            coordinates: vec![0.; width],
        })
    }

    fn bias(&mut self, voltage: f64) -> Result<()> {
        self.coordinates[0] = voltage;
        for row in 2..self.coordinates.len() {
            let mut low = voltage.min(0.);
            let mut high = voltage.max(0.);
            for _ in 0..80 {
                let mid = low.midpoint(high);
                self.coordinates[row] = mid;
                self.artifact.evaluate(
                    &self.candidate,
                    &self.coordinates,
                    &mut self.point.point(),
                )?;
                if self.point.point().current[row] > 0. {
                    high = mid;
                } else {
                    low = mid;
                }
            }
            self.coordinates[row] = low.midpoint(high);
        }
        self.artifact
            .evaluate(&self.candidate, &self.coordinates, &mut self.point.point())?;
        Ok(())
    }

    fn admittance(&mut self, frequency: f64) -> (f64, f64) {
        let width = self.coordinates.len();
        let point = self.point.point();
        let omega = std::f64::consts::TAU * frequency;
        let mut g = -point.conductance[1];
        let mut b = -point.capacitance[1] * omega;
        for row in 2..width {
            let diagonal = row * width + row;
            let x = point.conductance[diagonal];
            let y = point.capacitance[diagonal] * omega;
            let coupling = -point.conductance[row];
            let junction_g = -point.conductance[row * width + 1];
            let junction_b = -point.capacitance[row * width + 1] * omega;
            let scale = coupling / x.mul_add(x, y * y);
            g += scale * junction_g.mul_add(x, junction_b * y);
            b += scale * junction_b.mul_add(x, -junction_g * y);
        }
        (g, b)
    }

    fn noise(&mut self, frequency: f64) -> Result<[f64; 3]> {
        // Noise witnesses have no capacitance; transfer coefficients are real.
        let width = self.coordinates.len();
        let point = self.point.point();
        let mut gain = vec![1., 0.];
        for row in 2..width {
            gain.push(-point.conductance[row] / point.conductance[row * width + row]);
        }
        struct Collector {
            gain: Vec<f64>,
            psd: [f64; 3],
        }
        impl Sink for Collector {
            fn push(&mut self, source: Source<'_>) -> model_kernel_api::Result<()> {
                let index = if source.name.ends_with("series") {
                    0
                } else if source.name.ends_with("shot") {
                    1
                } else {
                    2
                };
                let factor: f64 = source
                    .injections
                    .iter()
                    .map(|i| self.gain[i.row] * i.re)
                    .sum();
                self.psd[index] += factor * factor * source.density;
                Ok(())
            }
        }
        let mut collector = Collector { gain, psd: [0.; 3] };
        self.artifact.evaluate_noise(
            &self.candidate,
            &self.coordinates,
            frequency,
            &mut collector,
        )?;
        let (g, b) = self.admittance(frequency);
        Ok(collector.psd.map(|density| density.sqrt() / g.hypot(b)))
    }
}

#[test]
fn sidewall_topology_ac() -> Result<()> {
    let mut circuits = source_cases(include_str!("fixtures/ltspice/side-cases.tsv"))?
        .into_iter()
        .map(|(name, model, instance)| Ok((name, Circuit::new(&model, instance)?)))
        .collect::<Result<Vec<_>>>()?;
    let mut count = 0;
    for row in include_str!("fixtures/ltspice/side-ac-compact.tsv").lines() {
        let mut fields = row.split('\t');
        let voltage: f64 = fields.next().ok_or("missing bias")?.parse()?;
        for ((name, circuit), expected) in circuits.iter_mut().zip(fields) {
            circuit.bias(voltage)?;
            let (g, b) = circuit.admittance(1000.);
            let (real, imaginary) = expected.split_once(',').ok_or("missing admittance")?;
            close(name, voltage, g, real.parse()?, 1e-16, 2e-6)?;
            close(name, voltage, b, imaginary.parse()?, 1e-18, 2e-6)?;
            count += 1;
        }
    }
    assert_eq!(count, 72);
    Ok(())
}

fn value(row: &str, name: &str) -> Result<f64> {
    row.split('\t')
        .find_map(|field| field.strip_prefix(name))
        .ok_or_else(|| format!("missing {name}"))?
        .parse()
        .map_err(Into::into)
}

fn noise_rows(cases: &str, measured: &str, prefix: &str) -> Result<usize> {
    let mut count = 0;
    for (name, model, instance) in source_cases(cases)? {
        let label = format!("{prefix}{name}\t");
        let mut rows = measured.lines().filter(|row| row.starts_with(&label));
        let voltage = value(rows.next().ok_or("missing bias")?, "V(out)=")?;
        let mut circuit = Circuit::new(&model, instance)?;
        circuit.bias(voltage)?;
        for row in rows {
            let frequency = value(row, "frequency=")?;
            let (g, b) = circuit.admittance(frequency);
            close(
                name,
                voltage,
                g.hypot(b).recip(),
                value(row, "gain=")?,
                1e-12,
                2e-6,
            )?;
            let densities = circuit.noise(frequency)?;
            for (actual, field) in
                densities
                    .into_iter()
                    .zip(["V(d1.rs)=", "V(d1.sid)=", "V(d1.fid)="])
            {
                close(name, voltage, actual, value(row, field)?, 1e-20, 2e-6)?;
            }
            count += 1;
        }
    }
    Ok(count)
}

#[test]
fn measured_noise() -> Result<()> {
    assert_eq!(
        noise_rows(
            include_str!("fixtures/ltspice/noise-cases.tsv"),
            include_str!("fixtures/ltspice/noise-measured.tsv"),
            "noise-"
        )?,
        51
    );
    assert_eq!(
        noise_rows(
            include_str!("fixtures/ltspice/shot-cases.tsv"),
            include_str!("fixtures/ltspice/shot-measured.tsv"),
            "shot-"
        )?,
        48
    );
    assert_eq!(
        noise_rows(
            include_str!("fixtures/ltspice/noise-side-extra-cases.tsv"),
            include_str!("fixtures/ltspice/noise-side-extra.tsv"),
            "noise-side-extra-"
        )?,
        6
    );
    Ok(())
}

#[test]
fn sidewall_thermal_capacitance() -> Result<()> {
    for (definitions, measured) in [
        (
            include_str!("fixtures/ltspice/side-thermal-cases.tsv"),
            include_str!("fixtures/ltspice/side-thermal-ac.tsv"),
        ),
        (
            include_str!("fixtures/ltspice/side-thermal-extra-cases.tsv"),
            include_str!("fixtures/ltspice/side-thermal-extra-ac.tsv"),
        ),
    ] {
        let models = cases(definitions)?;
        for row in measured.lines() {
            let mut fields = row.split('\t');
            let voltage: f64 = fields.next().ok_or("missing bias")?.parse()?;
            for ((name, device), expected) in models.iter().zip(fields) {
                let mut g = 0.;
                let mut c = 0.;
                for branch in [device.bulk, device.sidewall].into_iter().flatten() {
                    let value = branch.evaluate(voltage)?.total;
                    g += value.conductance;
                    c += value.capacitance;
                }
                let (real, imaginary) = expected.split_once(',').ok_or("missing admittance")?;
                close(name, voltage, g, real.parse()?, 1e-17, 3e-8)?;
                close(
                    name,
                    voltage,
                    c * std::f64::consts::TAU * 1000.,
                    imaginary.parse()?,
                    1e-18,
                    3e-8,
                )?;
            }
        }
    }
    Ok(())
}

#[test]
fn sidewall_signed_emission_and_inheritance() -> Result<()> {
    let models = cases(include_str!("fixtures/ltspice/side-signed-cases.tsv"))?;
    assert_eq!(
        dc_rows(
            &models,
            include_str!("fixtures/ltspice/side-signed-dc.tsv"),
            2e-6
        )?,
        9 * 12
    );
    let models = source_cases(include_str!(
        "fixtures/ltspice/side-default-emission-cases.tsv"
    ))?;
    let measured = include_str!("fixtures/ltspice/side-default-emission-ac.tsv").trim();
    for ((name, model, instance), expected) in models.into_iter().zip(measured.split('\t')) {
        let (g, b) = expected.split_once(',').ok_or("missing admittance")?;
        let (g, b): (f64, f64) = (g.parse()?, b.parse()?);
        if !g.is_finite() || !b.is_finite() {
            // Some nonfinite entries in the mixed vendor deck disappear when
            // isolated. The physically singular zero-emission case must fail;
            // finite cases are checked in the separate signed DC sweep.
            if model.bulk.transport.emission == 0. {
                assert!(model.bind(instance).is_err());
            }
            continue;
        }
        let device = model.bind(instance)?;
        let mut actual_g = 0.;
        let mut actual_b = 0.;
        for branch in [device.bulk, device.sidewall].into_iter().flatten() {
            let value = branch.evaluate(0.)?.total;
            actual_g += value.conductance;
            actual_b += value.capacitance * std::f64::consts::TAU * 1000.;
        }
        close(name, 0., actual_g, g, 1e-17, 3e-8)?;
        close(name, 0., actual_b, b, 1e-18, 3e-8)?;
    }
    Ok(())
}
