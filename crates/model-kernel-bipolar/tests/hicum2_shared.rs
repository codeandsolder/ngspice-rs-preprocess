use model_kernel_api::{
    Binding, EquationKind, Error, PointBuffer, Result as KernelResult,
    noise::{Injection, Sink, Source},
};
use model_kernel_bipolar::{
    Temperature,
    hicum2::{
        Polarity,
        artifact::{Model, Trial},
        instance::Options,
        noise::FlickerLocation,
        shared,
        temperature::Nominal,
        topology::{Node, Resistor},
    },
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn nominal(mask: u8, nqs: bool, polarity: Polarity) -> Nominal {
    let mut model = Nominal::source_defaults();
    model.polarity = polarity;
    for resistor in Resistor::ALL {
        let value = if mask & (1 << resistor.index()) == 0 {
            0.
        } else {
            3.
        };
        match resistor {
            Resistor::Rbx => model.auxiliary.rbx = value,
            Resistor::Rbi => model.auxiliary.rbi0 = value,
            Resistor::Rcx => model.auxiliary.rcx = value,
            Resistor::Re => model.auxiliary.re = value,
            Resistor::Rsu => model.auxiliary.rsu = value,
        }
    }
    model.transport.flcomp = 2.3;
    model.transport.t0 = 5e-12;
    model.auxiliary.flnqs = nqs;
    model.noise.correlated = true;
    model.noise.alit = 0.5;
    model.noise.alqf = 0.5;
    model
}

#[test]
fn every_reduced_topology_reuses_native_physics() -> TestResult {
    for polarity in [Polarity::Npn, Polarity::Pnp] {
        for mask in 0..32 {
            for nqs in [false, true] {
                let model = Model::new(&nominal(mask, nqs, polarity), None)?;
                let temperature = Temperature::new(330.)?;
                let options = Options {
                    area: 1.25,
                    multiplicity: 2.,
                    dt_kelvin: 4.,
                    ..Options::default()
                };
                let direct = model.bind(temperature, options)?;
                let artifact = shared::artifact(&model, temperature, options)?;
                let layout = direct.layout();
                let rows = &artifact.descriptor().equations;
                assert_eq!(rows.len(), layout.count());
                for (index, row) in rows.iter().enumerate() {
                    assert_eq!(
                        row.binding,
                        if index < 4 {
                            Binding::Terminal
                        } else {
                            Binding::Hidden
                        }
                    );
                    assert_eq!(
                        row.kind,
                        if index < layout.electrical_count() {
                            EquationKind::Electrical
                        } else {
                            EquationKind::Internal
                        }
                    );
                }
                let instance = artifact.prepare(
                    &[options.area, options.multiplicity, options.dt_kelvin],
                    temperature.kelvin(),
                )?;
                let sign = if polarity == Polarity::Npn { 1. } else { -1. };
                let mut x = vec![0.; layout.count()];
                for node in Node::ALL {
                    x[layout.node(node)] = match node {
                        Node::B | Node::Bp | Node::Bi => sign * 0.72,
                        Node::C | Node::Ci => sign,
                        _ => 0.,
                    };
                }
                for (slot, value) in [1e-4, 2e-4, 1e-14].into_iter().enumerate() {
                    if let Some(row) = layout.nqs_slot(slot) {
                        x[row] = value;
                    }
                }
                let expected = direct
                    .prepare(0.)?
                    .evaluate(Trial {
                        unknowns: &x,
                        electrothermal: None,
                    })?
                    .local;
                let mut buffer = PointBuffer::new(x.len())?;
                let mut point = buffer.point();
                artifact.evaluate(&instance, &x, &mut point)?;
                let n = x.len();
                assert_eq!(point.current, &expected.current[..n]);
                assert_eq!(point.charge, &expected.charge[..n]);
                for row in 0..n {
                    assert_eq!(
                        &point.conductance[row * n..(row + 1) * n],
                        &expected.conductance[row][..n]
                    );
                    assert_eq!(
                        &point.capacitance[row * n..(row + 1) * n],
                        &expected.capacitance[row][..n]
                    );
                }
            }
        }
    }
    Ok(())
}

#[derive(Default)]
struct Noise(Vec<(String, f64, Vec<Injection>)>);
impl Sink for Noise {
    fn push(&mut self, source: Source<'_>) -> KernelResult<()> {
        self.0.push((
            source.name.into(),
            source.density,
            source.injections.to_vec(),
        ));
        Ok(())
    }
}

fn close(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() <= 1e-13 * actual.abs().max(expected.abs()),
        "{actual:e} != {expected:e}"
    );
}

#[test]
fn correlated_noise_preserves_complex_spectrum_and_physical_nodes() -> TestResult {
    let mut nominal = nominal(31, false, Polarity::Npn);
    nominal.noise.flicker_location = FlickerLocation::Peripheral;
    let model = Model::new(&nominal, None)?;
    let temperature = Temperature::new(300.15)?;
    let options = Options::default();
    let direct = model.bind(temperature, options)?;
    let layout = direct.layout();
    let artifact = shared::artifact(&model, temperature, options)?;
    let candidate = artifact.prepare(&[1., 1., 0.], temperature.kelvin())?;
    let mut x = vec![0.; layout.count()];
    for node in Node::ALL {
        x[layout.node(node)] = match node {
            Node::B | Node::Bp | Node::Bi => 0.72,
            Node::C | Node::Ci => 1.,
            _ => 0.,
        };
    }
    let prepared = direct.prepare(0.)?;
    let operating = prepared
        .evaluate(Trial {
            unknowns: &x,
            electrothermal: None,
        })?
        .operating;
    for frequency in [1., 1e6, 1e9, 1e12] {
        let expected = prepared.evaluate_noise(&operating, frequency)?;
        let mut sink = Noise::default();
        artifact.evaluate_noise(&candidate, &x, frequency, &mut sink)?;
        let base = layout.node(Node::Bi);
        let collector = layout.node(Node::Ci);
        let emitter = layout.node(Node::Ei);
        let mut base_psd = 0.;
        let mut collector_psd = 0.;
        let mut cross = 0.;
        for (name, density, terms) in &sink.0 {
            if name == "ijbcx" {
                assert_eq!(terms[0].row, layout.node(Node::Bp));
                assert_eq!(terms[1].row, collector);
                close(*density, expected.shot.ijbcx);
            }
            if name == "fbe" {
                assert_eq!(terms[0].row, layout.node(Node::Bp));
                assert_eq!(terms[1].row, emitter);
            }
            if name != "ibei" && name != "it" {
                continue;
            }
            let at = |row| {
                terms
                    .iter()
                    .filter(|term| term.row == row)
                    .fold((0., 0.), |(re, im), term| (re + term.re, im + term.im))
            };
            let (br, bi) = at(base);
            let (cr, ci) = at(collector);
            base_psd += density * br.mul_add(br, bi * bi);
            collector_psd += density * cr.mul_add(cr, ci * ci);
            cross += density * bi.mul_add(cr, -br * ci);
        }
        close(base_psd, expected.be_ce_shot.base);
        close(collector_psd, expected.be_ce_shot.collector);
        close(cross, expected.be_ce_shot.cross_imaginary);
    }
    Ok(())
}

#[test]
fn topology_changing_candidates_and_live_thermal_models_are_rejected() -> TestResult {
    let mut nominal = nominal(0, false, Polarity::Npn);
    nominal.auxiliary.rcx = 0.002;
    let ambient = Temperature::new(300.15)?;
    let model = Model::new(&nominal, None)?;
    let reference = Options {
        temp_celsius: Some(40.),
        ..Options::default()
    };
    let artifact = shared::artifact(&model, ambient, reference)?;
    assert!(artifact.prepare(&[1., 1., 0., 60.], 350.).is_ok());
    assert!(matches!(
        artifact.prepare(&[10., 1., 0., 40.], 350.),
        Err(Error::Unsupported(_))
    ));
    nominal.auxiliary.flsh = 2;
    nominal.auxiliary.rth = 250.;
    let thermal = Model::new(&nominal, None)?;
    assert!(matches!(
        shared::artifact(&thermal, ambient, reference),
        Err(Error::Unsupported(_))
    ));
    Ok(())
}
