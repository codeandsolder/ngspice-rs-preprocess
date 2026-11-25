use model_kernel_bipolar::{
    EvaluationError, ModelError, Temperature,
    hicum2::noise::{self, FlickerLocation},
};

type TestResult = Result<(), Box<dyn std::error::Error>>;
const K: f64 = 1.380_650_3e-23;
const Q: f64 = 1.602_176_462e-19;

fn close(actual: f64, expected: f64, relative: f64, absolute: f64) {
    let limit = relative.mul_add(actual.abs().max(expected.abs()), absolute);
    assert!(
        (actual - expected).abs() <= limit,
        "actual={actual:.15e}; expected={expected:.15e}; tolerance={limit:.4e}"
    );
}

const fn model() -> noise::Parameters {
    noise::Parameters {
        kf: 1e-19,
        af: 1.8,
        flicker_location: FlickerLocation::Intrinsic,
        kfre: 2e-19,
        afre: 2.0,
        correlated: false,
        alit: 0.333,
        alqf: 0.167,
    }
}
const fn operating() -> noise::OperatingPoint {
    noise::OperatingPoint {
        conductances: noise::Conductances {
            rbx: 1.0 / 3.0,
            rbi: 1.0 / 10.0,
            rcx: 1.0 / 20.0,
            re: 1.0 / 2.0,
            rsu: 1.0 / 50.0,
        },
        currents: noise::Currents {
            it: 0.02,
            ibei: 2e-5,
            ibep: -1e-6,
            ibci: -3e-6,
            ijbcx: 2e-7,
            ijsc: -4e-10,
            iavl: 5e-4,
            emitter_resistor: -0.01,
        },
        forward_transit_time: 5e-12,
    }
}

#[test]
fn source_hicum24_individual_thermal_shot_and_combined_flicker_psds() -> TestResult {
    for kelvin in [173.15, 233.15, 300.15, 398.15, 600.0] {
        let kernel = noise::Prepared::new(model(), Temperature::new(kelvin)?)?;
        for frequency in [1.0, 10.0, 1e3, 1e6, 1e10] {
            let out = kernel.evaluate(operating(), frequency)?;
            let c = operating().conductances;
            let i = operating().currents;
            close(out.thermal.rbx, 4.0 * K * kelvin * c.rbx, 3e-15, 0.0);
            close(out.thermal.rbi, 4.0 * K * kelvin * c.rbi, 3e-15, 0.0);
            close(out.thermal.rcx, 4.0 * K * kelvin * c.rcx, 3e-15, 0.0);
            close(out.thermal.re, 4.0 * K * kelvin * c.re, 3e-15, 0.0);
            close(out.thermal.rsu, 4.0 * K * kelvin * c.rsu, 3e-15, 0.0);
            close(out.shot.iavl, 2.0 * Q * i.iavl, 3e-15, 0.0);
            close(out.shot.ibci, 2.0 * Q * i.ibci.abs(), 3e-15, 0.0);
            close(out.shot.ibep, 2.0 * Q * i.ibep.abs(), 3e-15, 0.0);
            close(out.shot.ijbcx, 2.0 * Q * i.ijbcx.abs(), 3e-15, 0.0);
            close(out.shot.ijsc, 2.0 * Q * i.ijsc.abs(), 3e-15, 0.0);
            close(out.be_ce_shot.base, 2.0 * Q * i.ibei.abs(), 3e-15, 0.0);
            close(out.be_ce_shot.collector, 2.0 * Q * i.it.abs(), 3e-15, 0.0);
            assert!(!out.be_ce_shot.correlated);
            assert_eq!(out.be_ce_shot.cross_imaginary, 0.0);
            close(
                out.flicker.be,
                model().kf * (i.ibei + i.ibep).abs().powf(model().af) / frequency,
                2e-15,
                0.0,
            );
            close(
                out.flicker.emitter_resistor,
                model().kfre * i.emitter_resistor.abs().powf(model().afre) / frequency,
                2e-15,
                0.0,
            );
            assert_eq!(out.flicker_location, FlickerLocation::Intrinsic);
        }
    }
    Ok(())
}

#[test]
fn source_correlated_shot_network_yields_hermitian_positive_spectral_matrix() -> TestResult {
    for alit in [0.0, 0.1, 0.333, 0.75, 1.0] {
        for alqf in [0.0, 0.05, 0.167, 1.0] {
            for frequency in [1.0, 1e6, 1e9, 1e10] {
                let p = noise::Parameters {
                    alit,
                    alqf,
                    correlated: true,
                    ..model()
                };
                let kernel = noise::Prepared::new(p, Temperature::new(300.15)?)?;
                let op = operating();
                let out = kernel.evaluate(op, frequency)?;
                let cov = out.be_ce_shot;
                let primary_psd = 2.0 * Q * op.currents.ibei.abs();
                let transfer_psd = 2.0 * Q * op.currents.it.abs();
                let enable = alit > 0.0 && alqf > 0.0;
                assert_eq!(cov.correlated, enable);
                if enable {
                    let beta_dc = op.currents.it / op.currents.ibei;
                    let root = beta_dc * alit.mul_add(-alit, 2.0 * alqf);
                    let n2 = if root > 0.0 {
                        op.forward_transit_time * root.sqrt()
                    } else {
                        0.0
                    };
                    let n1 = op.forward_transit_time * alit;
                    let omega = 2.0 * std::f64::consts::PI * frequency;
                    // Independent two-source network:
                    // I_b=(1+j*omega*n2)*W_be + j*omega*n1*W_ce
                    // I_c=W_ce. The covariance includes the off-diagonal
                    // source term; treating these as independent loses it.
                    let expected_base = (omega * n1).powi(2).mul_add(
                        transfer_psd,
                        (omega * n2).mul_add(omega * n2, 1.0) * primary_psd,
                    );
                    let expected_cross = omega * n1 * transfer_psd;
                    close(cov.base, expected_base, 4e-14, 0.0);
                    close(cov.collector, transfer_psd, 4e-14, 0.0);
                    close(cov.cross_imaginary, expected_cross, 4e-14, 0.0);
                    close(cov.n1_seconds, n1, 4e-14, 0.0);
                    close(cov.n2_seconds, n2, 4e-14, 0.0);
                    let determinant = cov
                        .cross_imaginary
                        .mul_add(-cov.cross_imaginary, cov.base * cov.collector);
                    let expected_det =
                        (omega * n2).mul_add(omega * n2, 1.0) * primary_psd * transfer_psd;
                    close(determinant, expected_det, 5e-12, 1e-102);
                    assert!(cov.cross_imaginary >= 0.0);
                } else {
                    close(cov.base, primary_psd, 0.0, 0.0);
                    close(cov.collector, transfer_psd, 0.0, 0.0);
                    assert_eq!(cov.cross_imaginary, 0.0);
                }
            }
        }
    }
    Ok(())
}

#[test]
fn source_flicker_placement_and_conditional_emitter_noise() -> TestResult {
    let mut p = model();
    p.flicker_location = FlickerLocation::Peripheral;
    let kernel = noise::Prepared::new(p, Temperature::new(300.15)?)?;
    let input = operating();
    let peripheral = kernel.evaluate(input, 1e3)?;
    assert_eq!(peripheral.flicker_location, FlickerLocation::Peripheral);
    let mut intrinsic_model = p;
    intrinsic_model.flicker_location = FlickerLocation::Intrinsic;
    let intrinsic =
        noise::Prepared::new(intrinsic_model, Temperature::new(300.15)?)?.evaluate(input, 1e3)?;
    assert_eq!(intrinsic.flicker.be, peripheral.flicker.be);
    // Only one B-E flicker source exists regardless of placement.
    let combined = (input.currents.ibei + input.currents.ibep).abs();
    let separate = input.currents.ibei.abs().powf(p.af) + input.currents.ibep.abs().powf(p.af);
    assert!((combined.powf(p.af) - separate).abs() > 0.0);
    let mut no_resistor = input;
    no_resistor.conductances.re = 0.0;
    let off = kernel.evaluate(no_resistor, 1e3)?;
    assert_eq!(off.thermal.re, 0.0);
    assert_eq!(off.flicker.emitter_resistor, 0.0);
    assert_eq!(off.flicker.be, peripheral.flicker.be);
    Ok(())
}

#[test]
fn source_noise_borrowed_batch_and_invalid_physical_domains() -> TestResult {
    let kernel = noise::Prepared::new(model(), Temperature::new(300.15)?)?;
    let input = operating();
    let mut outputs = [noise::Output::default(); 3];
    kernel.evaluate_batch(&[input; 3], &[1e3, 1e3, 1e3], &mut outputs)?;
    assert!(outputs.iter().all(|value| *value == outputs[0]));
    assert!(matches!(
        kernel.evaluate_batch(&[input; 2], &[1e3, 1e3, 1e3], &mut outputs),
        Err(EvaluationError::LengthMismatch)
    ));
    assert!(matches!(
        kernel.evaluate_batch(&[input; 3], &[1e3], &mut outputs),
        Err(EvaluationError::LengthMismatch)
    ));
    for frequency in [0.0, -1.0, f64::NAN] {
        assert!(matches!(
            kernel.evaluate(input, frequency),
            Err(EvaluationError::InvalidPhysicalDomain)
        ));
    }
    let mut bad = input;
    bad.conductances.rcx = -1.0;
    assert!(matches!(
        kernel.evaluate(bad, 1e3),
        Err(EvaluationError::InvalidPhysicalDomain)
    ));
    bad = input;
    bad.currents.iavl = -1.0;
    assert!(matches!(
        kernel.evaluate(bad, 1e3),
        Err(EvaluationError::InvalidPhysicalDomain)
    ));
    bad = input;
    bad.currents.ibei = f64::NAN;
    assert!(matches!(
        kernel.evaluate(bad, 1e3),
        Err(EvaluationError::NonFiniteBias)
    ));
    bad = input;
    bad.forward_transit_time = -1e-12;
    assert!(matches!(
        kernel.evaluate(bad, 1e3),
        Err(EvaluationError::InvalidPhysicalDomain)
    ));
    for (name, value) in [
        ("kf", -1.0),
        ("kfre", -2.0),
        ("af", 0.0),
        ("afre", 0.0),
        ("af", 11.0),
        ("afre", 11.0),
        ("alit", -0.2),
        ("alit", 1.1),
        ("alqf", -0.1),
        ("alqf", 1.1),
    ] {
        let mut p = model();
        match name {
            "kf" => p.kf = value,
            "kfre" => p.kfre = value,
            "af" => p.af = value,
            "afre" => p.afre = value,
            "alit" => p.alit = value,
            "alqf" => p.alqf = value,
            _ => return Err("unexpected invalid noise parameter".into()),
        }
        assert!(matches!(noise::Prepared::new(p,Temperature::new(300.15)?),
            Err(ModelError::InvalidParameter(key)) if key==name));
    }
    let correlated = noise::Prepared::new(
        noise::Parameters {
            correlated: true,
            ..model()
        },
        Temperature::new(300.15)?,
    )?;
    assert!(matches!(
        correlated.evaluate(input, 1e308),
        Err(EvaluationError::NonFiniteResult)
    ));
    let mut overflowing = model();
    overflowing.kf = f64::MAX;
    overflowing.af = 10.0;
    let strong = noise::Prepared::new(overflowing, Temperature::new(300.15)?)?;
    bad = input;
    bad.currents.ibei = 1e150;
    assert!(matches!(
        strong.evaluate(bad, 1e3),
        Err(EvaluationError::NonFiniteResult)
    ));
    Ok(())
}
