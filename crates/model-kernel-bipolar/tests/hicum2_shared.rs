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
                let expected = direct.prepare(0.)?.evaluate(Trial { unknowns: &x })?.local;
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
    let operating = prepared.evaluate(Trial { unknowns: &x })?.operating;
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
fn topology_changing_candidates_rejected_and_live_thermal_models_supported() -> TestResult {
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
    let thermal_artifact = shared::artifact(&thermal, ambient, reference)?;
    assert!(
        thermal_artifact
            .descriptor()
            .equations
            .iter()
            .any(|row| row.kind == EquationKind::Thermal)
    );
    thermal_artifact.prepare(&[1., 1., 0., 40.], 350.)?;
    Ok(())
}

#[path = "fixtures/hicum2_baseline.rs"]
mod baseline;

fn thermal_model(mask: u8, nqs: bool, polarity: Polarity, mode: u8) -> Nominal {
    let mut model = nominal(mask, nqs, polarity);
    model.transport = baseline::parameters();
    model.transport.t0 = 5e-12;
    model.transport.tbhrec = 2e-11;
    model.auxiliary.flsh = mode;
    model.auxiliary.rth = 200.;
    model.auxiliary.cth = 2e-9;
    model.auxiliary.favl = 0.04;
    model.auxiliary.qavl = 1e-15;
    model.auxiliary.kavl = 0.01;
    model.auxiliary.itss = 1e-16;
    model.auxiliary.tsf = 2e-12;
    model.auxiliary.ibets = 1e-15;
    model.auxiliary.abet = 10.;
    model.auxiliary.fcrbi = 0.2;
    model.auxiliary.fdqr0 = 0.2;
    model.auxiliary.fgeo = 0.5;
    model.auxiliary.fqi = 0.5;
    model.coefficients.dt = 3.;
    model.coefficients.zetarbi = 0.7;
    model.coefficients.zetarbx = 0.5;
    model.coefficients.zetare = 0.8;
    model.coefficients.zetarcx = -0.3;
    model.coefficients.zetarth = 0.2;
    model.coefficients.alrth = 1e-4;
    model.coefficients.alt0 = 1e-3;
    model.coefficients.kt0 = 1e-6;
    model.coefficients.alfav = 1e-3;
    model.coefficients.alqav = 1e-3;
    model.coefficients.alkav = 1e-3;
    model
}

#[test]
fn complete_thermal_column_matches_independent_trial_repreparation() -> TestResult {
    for polarity in [Polarity::Npn, Polarity::Pnp] {
        for mask in 0..32 {
            for nqs in [false, true] {
                for mode in [1, 2] {
                    let model = Model::new(&thermal_model(mask, nqs, polarity, mode), None)?;
                    let ambient = Temperature::new(300.15)?;
                    let options = Options {
                        area: 1.3,
                        multiplicity: 2.,
                        dt_kelvin: 4.,
                        temp_celsius: Some(42.),
                        ..Options::default()
                    };
                    let direct = model.bind(ambient, options)?;
                    let layout = direct.layout();
                    let width = layout.count();
                    let thermal = layout.thermal_slot().ok_or("thermal slot missing")?;
                    let artifact = shared::artifact(&model, ambient, options)?;
                    let instance = artifact.prepare(&[1.3, 2., 4., 42.], ambient.kelvin())?;
                    assert_eq!(
                        artifact.descriptor().equations[thermal].kind,
                        EquationKind::Thermal
                    );
                    assert_eq!(
                        artifact.descriptor().equations[thermal].binding,
                        Binding::Terminal
                    );
                    let sign = if polarity == Polarity::Npn { 1. } else { -1. };
                    for (vbe, vce, kelvin) in [
                        (0.73, 1.2, 335.),
                        (0.82, 0.1, 360.),
                        (-0.3, 2.5, 300.),
                        (0.55, 1.2, 160.),
                        (0.45, 1.2, 620.),
                    ] {
                        let mut x = vec![0.; width];
                        // Assign terminals last so each ideal short has one physical voltage.
                        for node in Node::ALL.into_iter().rev() {
                            x[layout.node(node)] = sign
                                * match node {
                                    Node::B => vbe,
                                    Node::Bp => vbe - 0.001,
                                    Node::Bi => vbe - 0.002,
                                    Node::C => vce,
                                    Node::Ci => vce - 0.003,
                                    Node::E => 0.,
                                    Node::Ei => 0.001,
                                    Node::S => -0.1,
                                    Node::Si => -0.099,
                                };
                        }
                        for (index, value) in [2e-5, 3e-5, 2e-15].into_iter().enumerate() {
                            if let Some(row) = layout.nqs_slot(index) {
                                x[row] = value;
                            }
                        }
                        x[thermal] = kelvin;
                        let mut center = PointBuffer::new(width)?;
                        let mut above = PointBuffer::new(width)?;
                        let mut below = PointBuffer::new(width)?;
                        let mut point = center.point();
                        artifact.evaluate(&instance, &x, &mut point)?;
                        let mut high = x.clone();
                        let mut low = x.clone();
                        let step = 1e-3;
                        high[thermal] += step;
                        low[thermal] -= step;
                        let mut high_point = above.point();
                        let mut low_point = below.point();
                        artifact.evaluate(&instance, &high, &mut high_point)?;
                        artifact.evaluate(&instance, &low, &mut low_point)?;
                        for row in 0..width {
                            for (name, exact, numeric, floor) in [
                                (
                                    "G",
                                    point.conductance[row * width + thermal],
                                    (high_point.current[row] - low_point.current[row])
                                        / (2. * step),
                                    1e-10,
                                ),
                                (
                                    "C",
                                    point.capacitance[row * width + thermal],
                                    (high_point.charge[row] - low_point.charge[row]) / (2. * step),
                                    1e-21,
                                ),
                            ] {
                                let tolerance =
                                    5e-3_f64.mul_add(exact.abs().max(numeric.abs()), floor);
                                assert!(
                                    (exact - numeric).abs() <= tolerance,
                                    "polarity={polarity:?} mask={mask} nqs={nqs} mode={mode} T={kelvin} VBE={vbe} VCE={vce} row={row} {name}={exact:e} numeric={numeric:e} tol={tolerance:e}"
                                );
                            }
                        }
                        let rise = kelvin - direct.thermal_reference_kelvin();
                        let mut source = x;
                        source[thermal] = rise;
                        let prepared = direct.prepare(rise)?;
                        let frame = prepared.evaluate(Trial { unknowns: &source })?;
                        assert_eq!(point.current, &frame.local.current[..width]);
                        assert_eq!(point.charge, &frame.local.charge[..width]);
                    }
                }
            }
        }
    }
    Ok(())
}
