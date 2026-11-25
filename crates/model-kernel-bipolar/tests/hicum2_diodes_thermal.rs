use model_kernel_bipolar::{
    ModelError, Temperature,
    hicum2::{self, device, nqs, temperature},
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn close(actual: f64, expected: f64, relative: f64, absolute: f64) {
    let tolerance = relative.mul_add(actual.abs().max(expected.abs()), absolute);
    assert!(
        (actual - expected).abs() <= tolerance,
        "actual={actual:.15e} expected={expected:.15e} tolerance={tolerance:.6e}"
    );
}

const fn physical_model(flcomp: f64) -> temperature::Nominal {
    let mut p = temperature::Nominal::source_defaults();
    p.transport.flcomp = flcomp;
    p.diodes.ibeis = 2e-18;
    p.diodes.ireis = 1e-18;
    p.diodes.ibeps = 3e-18;
    p.diodes.ireps = 4e-18;
    p.diodes.ibcis = 5e-16;
    p.diodes.ibcxs = 6e-17;
    p.diodes.iscs = 8e-17;
    p.diodes.mbei = 1.1;
    p.diodes.mrei = 1.9;
    p.diodes.mbep = 1.2;
    p.diodes.mrep = 2.1;
    p.diodes.mbci = 1.05;
    p.diodes.mbcx = 1.2;
    p.diodes.msc = 1.5;
    p.transport.mcf = 1.2;
    p.coefficients.alb = 0.003;
    p.coefficients.zetabet = 4.0;
    p.coefficients.zetaci = 0.4;
    p.coefficients.zetacx = 0.8;
    p
}

fn diode_bias(v: f64) -> hicum2::Bias {
    hicum2::Bias {
        vbiei: v,
        vbpei: v * 0.9,
        vbici: v * 0.7,
        vbpci: v * 1.1,
        vsici: v * 0.8,
    }
}

fn compare_all(
    sensitivity: hicum2::CurrentSensitivity,
    low: hicum2::Currents,
    high: hicum2::Currents,
    step: f64,
) {
    for (actual, minus, plus) in [
        (sensitivity.ibei, low.ibei.amperes, high.ibei.amperes),
        (sensitivity.irei, low.irei.amperes, high.irei.amperes),
        (sensitivity.ibep, low.ibep.amperes, high.ibep.amperes),
        (sensitivity.irep, low.irep.amperes, high.irep.amperes),
        (sensitivity.ibci, low.ibci.amperes, high.ibci.amperes),
        (sensitivity.ijbcx, low.ijbcx.amperes, high.ijbcx.amperes),
        (sensitivity.ijsc, low.ijsc.amperes, high.ijsc.amperes),
    ] {
        close(actual, (plus - minus) / (2.0 * step), 1e-5, 2e-14);
    }
}

#[test]
fn seven_exact_diode_thermal_slopes_match_all_flcomp_and_exp_limit_modes() -> TestResult {
    let mut checks = 0;
    for flcomp in [0.0, 2.1, 2.2, 2.3] {
        for kelvin in [233.15, 300.15, 373.15] {
            let mut p = physical_model(flcomp);
            for missing in [false, true] {
                if missing {
                    p.diodes.ireps = 0.0;
                    p.diodes.ibcxs = 0.0;
                }
                let h = 0.005;
                let op = p.prepare(Temperature::new(kelvin)?, 0.0)?;
                let below = p.prepare(Temperature::new(kelvin - h)?, 0.0)?;
                let above = p.prepare(Temperature::new(kelvin + h)?, 0.0)?;
                for voltage in [-0.8, -0.2, 0.0, 0.62, 1.0, 5.0] {
                    let bias = diode_bias(voltage);
                    let operating = op.diode_kernel()?.evaluate(bias)?;
                    let sensitivity = op
                        .diode_kernel()?
                        .temperature_sensitivity(bias, operating)?;
                    let low = below.diode_kernel()?.evaluate(bias)?;
                    let high = above.diode_kernel()?.evaluate(bias)?;
                    compare_all(sensitivity, low, high, h);
                    // FLSH=2 source power excludes reverse recombination
                    // and only counts five named diode power branches.
                    let power = |d: hicum2::Currents| {
                        bias.vbiei.mul_add(
                            d.ibei.amperes,
                            bias.vbici.mul_add(
                                d.ibci.amperes,
                                bias.vbpei.mul_add(
                                    d.ibep.amperes,
                                    bias.vbpci
                                        .mul_add(d.ijbcx.amperes, bias.vsici * d.ijsc.amperes),
                                ),
                            ),
                        )
                    };
                    close(
                        sensitivity.flsh2_diode_power_slope(bias),
                        (power(high) - power(low)) / (2.0 * h),
                        1e-5,
                        2e-14,
                    );
                    checks += 7;
                    if missing {
                        assert_eq!(sensitivity.irep, 0.0);
                        assert_eq!(sensitivity.ijbcx, 0.0);
                    }
                }
            }
        }
    }
    assert_eq!(checks, 1008);
    Ok(())
}

#[test]
fn physical_device_exposes_identical_prepared_diode_temperature_slopes() -> TestResult {
    for polarity in [hicum2::Polarity::Npn, hicum2::Polarity::Pnp] {
        for flcomp in [0.0, 2.2, 2.3] {
            let mut nominal = physical_model(flcomp);
            nominal.polarity = polarity;
            let prepared = nominal.prepare(Temperature::new(330.15)?, 0.0)?;
            let kernel = prepared.device_kernel()?;
            let sign = if matches!(polarity, hicum2::Polarity::Npn) {
                1.0
            } else {
                -1.0
            };
            let nodes = device::Nodes {
                b: sign * 0.72,
                bp: sign * 0.71,
                bi: sign * 0.70,
                c: sign * 1.25,
                ci: sign * 1.24,
                e: 0.0,
                ei: sign * 0.01,
                s: 0.0,
                si: 0.0,
            };
            let op = kernel.evaluate(&device::Bias {
                nodes,
                nqs: nqs::State::default(),
            })?;
            let direct = prepared.diode_kernel()?.temperature_sensitivity(
                hicum2::Bias {
                    vbiei: op.voltage.vbiei,
                    vbpei: op.voltage.vbpei,
                    vbici: op.voltage.vbici,
                    vbpci: op.voltage.vbpci,
                    vsici: op.voltage.vsici,
                },
                op.diode,
            )?;
            assert_eq!(kernel.diode_temperature_sensitivity(&op)?, direct);
        }
    }
    Ok(())
}

#[test]
fn nonfinite_saturation_temperature_slopes_are_rejected_with_source_names() -> TestResult {
    let p = physical_model(2.3);
    let nominal = p.prepare(Temperature::new(300.15)?, 0.0)?;
    let sample = nominal.diode_saturation_slopes;
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let mut slopes = sample;
        slopes.ibci = bad;
        assert!(matches!(
            hicum2::Prepared::with_temperature_slopes(
                nominal.diodes,
                nominal.effective_temperature,
                slopes,
            ),
            Err(ModelError::InvalidParameter("ibci_temperature_slope"))
        ));
    }
    Ok(())
}
