use ParameterValue::{Flag, Integer, Real};
use model_compact_intrinsic::{
    BoundParameters, BsimSoiInstanceInput, BsimSoiTemperaturePoint, BsimSoiThermalScalar,
    ModelVariant, ParameterScope, ParameterValue, PreparedBsimSoiSetup, prepare_bsim_soi_model,
    prepare_bsim_soi_setup,
};
type TestResult = Result<(), Box<dyn std::error::Error>>;

fn setup(
    model: &[(&str, ParameterValue)],
    instance: &[(&str, ParameterValue)],
) -> Result<PreparedBsimSoiSetup, Box<dyn std::error::Error>> {
    let model = BoundParameters::bind(
        ModelVariant::BsimSoi,
        ParameterScope::Model,
        model.iter().cloned(),
    )?;
    let model = prepare_bsim_soi_model(&model, 300.15)?;
    let raw = BoundParameters::bind(
        ModelVariant::BsimSoi,
        ParameterScope::Instance,
        instance.iter().cloned().chain([("tnodeout", Flag(true))]),
    )?;
    Ok(prepare_bsim_soi_setup(
        &model,
        &[BsimSoiInstanceInput {
            parameters: &raw,
            terminal_count: 6,
            geometry_scale: 1.,
        }],
    )?)
}

const fn quantities(p: BsimSoiTemperaturePoint) -> [BsimSoiThermalScalar; 10] {
    [
        p.thermal_voltage,
        p.bandgap,
        p.intrinsic_density,
        p.surface_potential,
        p.built_in_potential,
        p.backgate_flatband,
        p.depletion_width,
        p.depletion_capacitance,
        p.conductive_heat,
        p.stored_heat,
    ]
}

#[test]
fn material_and_heat_derivatives() -> TestResult {
    for material in [0, 1] {
        for pmos in [false, true] {
            for substrate in [-1e16, 1e16] {
                let setup = setup(
                    &[
                        ("mtrlmod", Integer(material)),
                        ("pmos", Flag(pmos)),
                        ("nsub", Real(substrate)),
                        ("nch", Real(1e17)),
                        ("tnom", Real(42.)),
                        ("shmod", Integer(1)),
                        ("rth0", Real(0.005)),
                        ("cth0", Real(1e-6)),
                        ("bg0sub", Real(1.25)),
                        ("tbgasub", Real(6e-4)),
                        ("tbgbsub", Real(900.)),
                        ("epsrsub", Real(12.5)),
                        ("ni0sub", Real(1.1e10)),
                    ],
                    &[
                        ("w", Real(4e-6)),
                        ("nf", Real(2.)),
                        ("nseg", Real(2.)),
                        ("m", Real(2.3)),
                    ],
                )?;
                for ambient in [233.15, 300.15, 358.15] {
                    let law = setup.prepare_temperature(0, ambient)?;
                    for rise in [-5., 0., 10., 60.] {
                        let temperature = ambient + rise;
                        let point = law.evaluate(temperature)?;
                        let step = 0.001;
                        let high = quantities(law.evaluate(temperature + step)?);
                        let low = quantities(law.evaluate(temperature - step)?);
                        for ((exact, a), b) in quantities(point).into_iter().zip(high).zip(low) {
                            let numeric = (a.value - b.value) / (2. * step);
                            let tol = 3e-6_f64
                                .mul_add(numeric.abs().max(exact.d_temperature.abs()), 1e-28);
                            assert!(
                                (numeric - exact.d_temperature).abs() <= tol,
                                "material={material} pmos={pmos} substrate={substrate} T={temperature} exact={exact:?} numeric={numeric:e}"
                            );
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

#[test]
fn inactive_thermal_coordinate_and_gamma_doping() -> TestResult {
    let setup = setup(&[("gamma1", Real(0.5)), ("shmod", Integer(0))], &[])?;
    let law = setup.prepare_temperature(0, 300.15)?;
    assert!(law.network().is_none());
    let point = law.evaluate(400.)?;
    assert_eq!(point.kelvin, 300.15);
    for q in quantities(point) {
        assert_eq!(q.d_temperature, 0.);
    }
    let oxide = 3.453_133e-11 / 1e-8;
    let doping = 3.021e22_f64 * (0.5_f64 * oxide).powi(2);
    let derived = point.intrinsic_density.value
        * (point.surface_potential.value / (2. * point.thermal_voltage.value)).exp();
    assert!((derived - doping).abs() < 1e-12 * doping);
    assert!(setup.prepare_temperature(1, 300.15).is_err());
    assert!(setup.prepare_temperature(0, 0.).is_err());
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires local ngspice 47 binary"]
fn native_passive_heat_port() -> TestResult {
    use std::{fs, process::Command};
    for pmos in [false, true] {
        for (nf, nseg, m, wth0) in [(1., 1., 1., 0.), (2., 2., 2.3, 3e-7), (4., 3., 0.8, 1e-7)] {
            let model = [
                ("pmos", Flag(pmos)),
                ("shmod", Integer(1)),
                ("rth0", Real(0.005)),
                ("cth0", Real(1e-6)),
                ("wth0", Real(wth0)),
                ("lint", Real(1e-8)),
                ("wint", Real(2e-8)),
            ];
            let instance = [
                ("l", Real(5e-7)),
                ("w", Real(4e-6)),
                ("nf", Real(nf)),
                ("nseg", Real(nseg)),
                ("m", Real(m)),
            ];
            let prepared = setup(&model, &instance)?;
            for ambient_c in [-40., 85.] {
                let ambient = ambient_c + 273.15;
                let law = prepared.prepare_temperature(0, ambient)?;
                let dir = tempfile::tempdir()?;
                let polarity = if pmos { "pmos" } else { "nmos" };
                let deck = format!(
                    "BSIMSOI passive heat reference\nVd d 0 0\nVg g 0 0\nVe e 0 0\nVb b 0 0\nVt t 0 10 AC 1\nM1 d g 0 e b t soi tnodeout l=.5u w=4u nf={nf} nseg={nseg} m={m} pd=4u ps=4u\n.model soi {polarity} level=10 soimod=0 shmod=1 rth0=.005 cth0=1e-6 wth0={wth0:e} lint=1e-8 wint=2e-8\n.temp {ambient_c}\n.options gmin=1e-18 reltol=1e-9 abstol=1e-15\n.control\nset numdgt=17\nset wr_singlescale\nop\nwrdata op.dat i(vt)\nac lin 1 1e6 1e6\nwrdata ac.dat i(vt)\nquit\n.endc\n.end\n"
                );
                fs::write(dir.path().join("deck.cir"), deck)?;
                let output = Command::new("ngspice")
                    .current_dir(dir.path())
                    .args(["-b", "-n", "deck.cir"])
                    .output()?;
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                let read = |filename: &str| -> Result<Vec<f64>, Box<dyn std::error::Error>> {
                    Ok(fs::read_to_string(dir.path().join(filename))?
                        .split_whitespace()
                        .map(str::parse)
                        .collect::<Result<_, _>>()?)
                };
                let op = read("op.dat")?;
                let ac = read("ac.dat")?;
                assert_eq!(op.len(), 2);
                assert_eq!(ac.len(), 3);
                let point = law.evaluate(ambient + 10.)?;
                let network = law.network().ok_or("no thermal network")?;
                for (actual, reference) in [
                    (point.conductive_heat.value, -op[1]),
                    (network.conductance, -ac[1]),
                    (
                        network.heat_capacity,
                        -ac[2] / (2. * std::f64::consts::PI * ac[0]),
                    ),
                ] {
                    assert!(
                        (actual - reference).abs() <= 1e-9_f64.mul_add(reference.abs(), 1e-20),
                        "passive heat {actual:e} vs {reference:e}, pmos={pmos} nf={nf} nseg={nseg} m={m} T={ambient}"
                    );
                }
            }
        }
    }
    Ok(())
}
