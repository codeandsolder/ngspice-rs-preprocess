use model_kernel_bipolar::{EvaluationError, ModelError, Temperature, vbic::noise};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn close(actual: f64, expected: f64, relative: f64, absolute: f64) {
    let tolerance = relative.mul_add(actual.abs().max(expected.abs()), absolute);
    assert!(
        (actual - expected).abs() <= tolerance,
        "actual={actual:.16e}; expected={expected:.16e}"
    );
}

const fn operating() -> noise::OperatingPoint {
    noise::OperatingPoint {
        conductances: noise::Conductances {
            rc: 0.1,
            rci: 0.012,
            rb: 0.06,
            rbi: 0.002,
            re: 0.08,
            rbp: 0.0001,
            rs: 0.05,
        },
        currents: noise::Currents {
            ic: 0.003,
            ibe: -0.00003,
            ibep: -0.00001,
            iccp: -0.00006,
        },
    }
}

#[test]
fn white_noise_uses_native_ngspice_analysis_constants() -> TestResult {
    // ngspice/src/include/ngspice/const.h (not VBIC transport's K/Q):
    // BOLTZMANN=1.3806226e-23 J/K, elementary charge=1.6021918e-19 C.
    let temperature = Temperature::new(300.15)?;
    let prepared = noise::Prepared::new(
        noise::Parameters {
            kfn: 0.0,
            afn: 1.0,
            bfn: 1.0,
        },
        temperature,
        1.0,
    )?;
    let value = prepared.evaluate(operating(), 10_000.0)?;
    let four_kt = 4.0 * 1.380_622_6e-23 * temperature.kelvin();
    let twice_charge = 2.0 * 1.602_191_8e-19;
    for (actual, conductance) in [
        (value.thermal.rc, 0.1),
        (value.thermal.rci, 0.012),
        (value.thermal.rb, 0.06),
        (value.thermal.rbi, 0.002),
        (value.thermal.re, 0.08),
        (value.thermal.rbp, 0.0001),
        (value.thermal.rs, 0.05),
    ] {
        close(actual, four_kt * conductance, 2e-15, 0.0);
    }
    close(value.shot.ic, twice_charge * 0.003, 2e-15, 0.0);
    close(value.shot.ibe, twice_charge * 0.00003, 2e-15, 0.0);
    close(value.shot.ibep, twice_charge * 0.00001, 2e-15, 0.0);
    close(value.shot.iccp, twice_charge * 0.00006, 2e-15, 0.0);
    assert_eq!(value.flicker, noise::Flicker::default());
    let expected = [
        value.thermal.rc,
        value.thermal.rci,
        value.thermal.rb,
        value.thermal.rbi,
        value.thermal.re,
        value.thermal.rbp,
        value.thermal.rs,
        value.shot.ic,
        value.shot.ibe,
        value.shot.ibep,
        value.shot.iccp,
    ]
    .into_iter()
    .sum();
    assert_eq!(value.total_source, expected);
    Ok(())
}

#[test]
fn flicker_multiplicity_frequency_exponents_and_current_magnitude() -> TestResult {
    let temperature = Temperature::new(300.15)?;
    let params = noise::Parameters {
        kfn: 1e-12,
        afn: 1.7,
        bfn: 1.2,
    };
    let prepared = noise::Prepared::new(params, temperature, 4.0)?;
    let mut operating = operating();
    operating.currents.ibe = -0.0024;
    operating.currents.ibep = 0.0008;
    let at_one_khz = prepared.evaluate(operating, 1000.0)?;
    let at_ten_khz = prepared.evaluate(operating, 10000.0)?;
    let reference =
        4.0 * params.kfn * (0.0024_f64 / 4.0).powf(params.afn) / 1000.0_f64.powf(params.bfn);
    close(at_one_khz.flicker.ibe, reference, 3e-15, 0.0);
    close(
        at_one_khz.flicker.ibep,
        4.0 * params.kfn * (0.0008_f64 / 4.0).powf(params.afn) / 1000.0_f64.powf(params.bfn),
        3e-15,
        0.0,
    );
    close(
        at_one_khz.flicker.ibe / at_ten_khz.flicker.ibe,
        10.0_f64.powf(params.bfn),
        2e-15,
        0.0,
    );
    assert_eq!(at_one_khz.thermal, at_ten_khz.thermal);
    assert_eq!(at_one_khz.shot, at_ten_khz.shot);
    Ok(())
}

#[test]
fn zero_current_flicker_matches_ngspice_n_minlog_floor() -> TestResult {
    let p = noise::Parameters {
        kfn: 1e-8,
        afn: 1.0,
        bfn: 1.0,
    };
    let prepared = noise::Prepared::new(p, Temperature::new(300.15)?, 1.0)?;
    let operating = noise::OperatingPoint::default();
    let out = prepared.evaluate(operating, 1.0)?;
    close(out.flicker.ibe, 1e-46, 1e-15, 0.0);
    assert_eq!(out.flicker.ibe, out.flicker.ibep);
    assert_eq!(out.total_source, out.flicker.ibe + out.flicker.ibep);
    Ok(())
}

#[test]
fn noise_batch_validation_and_negative_conductance_fail_closed() -> TestResult {
    let prepared = noise::Prepared::new(
        noise::Parameters {
            kfn: 0.0,
            afn: 1.0,
            bfn: 1.0,
        },
        Temperature::new(300.15)?,
        1.0,
    )?;
    let operating = [operating(); 3];
    let mut outputs = [noise::Output::default(); 3];
    prepared.evaluate_batch(&operating, &mut outputs, 150.0)?;
    assert!(outputs.iter().all(|item| *item == outputs[0]));
    assert!(matches!(
        prepared.evaluate_batch(&operating, &mut outputs[..2], 150.0),
        Err(EvaluationError::LengthMismatch)
    ));
    assert!(matches!(
        prepared.evaluate(operating[0], 0.0),
        Err(EvaluationError::InvalidPhysicalDomain)
    ));
    let mut bad = operating[0];
    bad.conductances.rb = -0.001;
    assert!(matches!(
        prepared.evaluate(bad, 150.0),
        Err(EvaluationError::InvalidPhysicalDomain)
    ));
    let sentinel = noise::Output::default();
    let mut outputs = [sentinel; 3];
    prepared.evaluate_batch(&[], &mut [], 0.0)?;
    assert_eq!(
        prepared.evaluate_batch(&operating, &mut outputs[..2], 150.0),
        Err(EvaluationError::LengthMismatch)
    );
    assert_eq!(outputs, [sentinel; 3]);
    let first = prepared.evaluate(operating[0], 150.0)?;
    assert_ne!(first, sentinel);
    assert_eq!(
        prepared.evaluate_batch(&[operating[0], bad, operating[2]], &mut outputs, 150.0),
        Err(EvaluationError::InvalidPhysicalDomain)
    );
    assert_eq!(outputs, [first, sentinel, sentinel]);
    bad = operating[0];
    bad.currents.ic = f64::NAN;
    assert!(matches!(
        prepared.evaluate(bad, 150.0),
        Err(EvaluationError::NonFiniteBias)
    ));
    Ok(())
}

#[test]
fn noise_prepared_model_parameter_validation() -> TestResult {
    let temperature = Temperature::new(300.15)?;
    for multiplicity in [f64::NAN, f64::INFINITY, 0.0, -1.0] {
        assert!(matches!(
            noise::Prepared::new(
                noise::Parameters {
                    kfn: 1e-12,
                    afn: 1.0,
                    bfn: 1.0
                },
                temperature,
                multiplicity
            ),
            Err(ModelError::InvalidParameter("m"))
        ));
    }
    assert!(matches!(
        noise::Prepared::new(
            noise::Parameters {
                kfn: -1e-12,
                afn: 1.0,
                bfn: 1.0
            },
            temperature,
            1.0,
        ),
        Err(ModelError::InvalidParameter("kfn"))
    ));
    Ok(())
}
