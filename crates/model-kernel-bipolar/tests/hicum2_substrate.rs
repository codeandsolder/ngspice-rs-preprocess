use model_kernel_bipolar::{EvaluationError, ModelError, Temperature, hicum2::substrate};
type TestResult = Result<(), Box<dyn std::error::Error>>;

fn close(actual: f64, expected: f64, relative: f64, absolute: f64) {
    let tolerance = relative.mul_add(actual.abs().max(expected.abs()), absolute);
    assert!(
        (actual - expected).abs() <= tolerance,
        "actual={actual:.15e}, expected={expected:.15e}, tolerance={tolerance:.15e}"
    );
}

fn source_exp(voltage: f64, nvt: f64) -> (f64, f64) {
    let normalized = voltage / nvt;
    if normalized > 80.0 {
        let upper = 80.0f64.exp();
        (upper * (1.0 + normalized - 80.0), upper)
    } else {
        let value = normalized.exp();
        (value, value)
    }
}

#[test]
fn source_sub_transfer_two_axis_jacobian_and_conserved_forward_storage() -> TestResult {
    let p = substrate::Parameters {
        saturation: 1.5e-12,
        ideality: 1.2,
        forward_storage_time: 3e-12,
    };
    for kelvin in [233.15, 273.15, 300.15, 373.15, 398.15] {
        let temperature = Temperature::new(kelvin)?;
        let kernel = substrate::Prepared::new(p, temperature)?;
        let nvt = 1.380_650_3e-23 / 1.602_176_462e-19 * p.ideality * kelvin;
        for peripheral in [-2.0, -0.8, -0.3, 0.0, 0.2, 0.55, 1.0, 2.5] {
            for substrate_voltage in [-2.0, -0.2, 0.0, 0.3, 0.5, 0.9] {
                let bias = substrate::Bias {
                    vbpci: peripheral,
                    vsici: substrate_voltage,
                };
                let output = kernel.evaluate(bias)?;
                let (forward_factor, forward_gradient) = source_exp(peripheral, nvt);
                let (reverse_factor, reverse_gradient) = source_exp(substrate_voltage, nvt);
                let expected_forward = p.saturation * forward_factor;
                let expected_reverse = p.saturation * reverse_factor;
                close(
                    output.current.value,
                    expected_forward - expected_reverse,
                    5e-13,
                    1e-22,
                );
                close(
                    output.diffusion_charge.value,
                    expected_forward * p.forward_storage_time,
                    5e-13,
                    1e-32,
                );
                close(
                    output.diffusion_charge.d_vbpci,
                    p.saturation * p.forward_storage_time * forward_gradient / nvt,
                    5e-13,
                    1e-31,
                );
                close(
                    output.current.d_vbpci,
                    p.saturation * forward_gradient / nvt,
                    5e-13,
                    1e-23,
                );
                close(
                    output.current.d_vsici,
                    -p.saturation * reverse_gradient / nvt,
                    5e-13,
                    1e-23,
                );
                assert_eq!(output.diffusion_charge.d_vsici, 0.0);

                let h = 1e-6;
                let emitter_plus = kernel.evaluate(substrate::Bias {
                    vbpci: peripheral + h,
                    ..bias
                })?;
                let emitter_minus = kernel.evaluate(substrate::Bias {
                    vbpci: peripheral - h,
                    ..bias
                })?;
                let substrate_plus = kernel.evaluate(substrate::Bias {
                    vsici: substrate_voltage + h,
                    ..bias
                })?;
                let substrate_minus = kernel.evaluate(substrate::Bias {
                    vsici: substrate_voltage - h,
                    ..bias
                })?;
                // If a partial is smaller than roundoff in the *total*
                // current, its finite difference cannot be observed.
                // Every partial is still checked unconditionally against
                // its independent source-law derivative above.
                let observable =
                    |gradient: f64| gradient.abs() * h > 1e-8 * output.current.value.abs();
                if observable(output.current.d_vbpci) {
                    close(
                        output.current.d_vbpci,
                        (emitter_plus.current.value - emitter_minus.current.value) / (2.0 * h),
                        3e-7,
                        1e-20,
                    );
                }
                if observable(output.current.d_vsici) {
                    close(
                        output.current.d_vsici,
                        (substrate_plus.current.value - substrate_minus.current.value) / (2.0 * h),
                        3e-7,
                        1e-20,
                    );
                }
                close(
                    output.diffusion_charge.d_vbpci,
                    (emitter_plus.diffusion_charge.value - emitter_minus.diffusion_charge.value)
                        / (2.0 * h),
                    3e-7,
                    1e-32,
                );
            }
        }
    }
    Ok(())
}

#[test]
fn source_sub_transfer_disable_negative_and_forward_limiter() -> TestResult {
    let temperature = Temperature::new(300.15)?;
    let p = substrate::Parameters {
        saturation: 0.0,
        ideality: 1.0,
        forward_storage_time: 2e-12,
    };
    let disabled = substrate::Prepared::new(p, temperature)?;
    assert_eq!(
        disabled.evaluate(substrate::Bias {
            vbpci: 1.0,
            vsici: 0.5
        })?,
        substrate::Output::default()
    );
    let active = substrate::Prepared::new(
        substrate::Parameters {
            saturation: 1e-12,
            ideality: 1.0,
            forward_storage_time: 0.0,
        },
        temperature,
    )?;
    assert_eq!(
        active
            .evaluate(substrate::Bias {
                vbpci: 0.1,
                vsici: 0.1
            })?
            .current
            .value,
        0.0
    );
    assert_eq!(
        active
            .evaluate(substrate::Bias {
                vbpci: 0.0,
                vsici: 0.0
            })?
            .diffusion_charge,
        substrate::Value2::default()
    );
    let thermal = 1.380_650_3e-23 / 1.602_176_462e-19 * temperature.kelvin();
    let upper = active.evaluate(substrate::Bias {
        vbpci: 90.0 * thermal,
        vsici: 0.0,
    })?;
    close(
        upper.current.d_vbpci,
        1e-12 * 80.0f64.exp() / thermal,
        1e-14,
        0.0,
    );
    assert!(upper.current.value.is_finite());
    assert!(matches!(
        active.evaluate(substrate::Bias {
            vbpci: f64::NAN,
            vsici: 0.0
        }),
        Err(EvaluationError::NonFiniteBias)
    ));
    assert!(matches!(
        substrate::Prepared::new(
            substrate::Parameters {
                saturation: -1.0,
                ideality: 1.0,
                forward_storage_time: 1e-12,
            },
            temperature
        ),
        Err(ModelError::InvalidParameter("itss"))
    ));
    Ok(())
}

#[test]
fn source_sub_transfer_borrowed_batch_contract() -> TestResult {
    let kernel = substrate::Prepared::new(
        substrate::Parameters {
            saturation: 1e-13,
            ideality: 1.6,
            forward_storage_time: 5e-12,
        },
        Temperature::new(300.15)?,
    )?;
    let input = [substrate::Bias {
        vbpci: 0.2,
        vsici: -0.3,
    }; 4];
    let mut results = [substrate::Output::default(); 4];
    kernel.evaluate_batch(&input, &mut results)?;
    assert!(results.iter().all(|result| *result == results[0]));
    assert!(matches!(
        kernel.evaluate_batch(&input, &mut results[..3]),
        Err(EvaluationError::LengthMismatch)
    ));
    Ok(())
}

#[test]
fn full_temperature_repreparation_matches_substrate_current_and_charge_slopes() -> TestResult {
    use model_kernel_bipolar::hicum2::temperature;
    let mut current_points = 0;
    let mut charge_points = 0;
    for flcomp in [0.0, 2.1, 2.2, 2.3] {
        for kelvin in [233.15, 300.15, 373.15] {
            for saturation in [0.0, 1e-15, 1e-12] {
                for storage in [0.0, 3e-12] {
                    let mut model = temperature::Nominal::source_defaults();
                    model.transport.flcomp = flcomp;
                    model.auxiliary.itss = saturation;
                    model.auxiliary.tsf = storage;
                    model.auxiliary.msf = 1.3;
                    model.coefficients.zetacx = 1.3;
                    let delta = 0.003;
                    let mid = model
                        .prepare(Temperature::new(kelvin)?, 0.0)?
                        .substrate_kernel()?;
                    let low = model
                        .prepare(Temperature::new(kelvin - delta)?, 0.0)?
                        .substrate_kernel()?;
                    let high = model
                        .prepare(Temperature::new(kelvin + delta)?, 0.0)?
                        .substrate_kernel()?;
                    for (peripheral, substrate_voltage) in [
                        (-0.3, -0.2),
                        (0.45, 0.1),
                        (0.9, 0.2),
                        (2.8, 0.7),
                        (-2.0, -0.5),
                    ] {
                        let bias = substrate::Bias {
                            vbpci: peripheral,
                            vsici: substrate_voltage,
                        };
                        let operating = mid.evaluate(bias)?;
                        let sensitivity = mid.temperature_sensitivity(bias, operating)?;
                        let lower = low.evaluate(bias)?;
                        let upper = high.evaluate(bias)?;
                        close(
                            sensitivity.current,
                            (upper.current.value - lower.current.value) / (2.0 * delta),
                            2e-5,
                            1e-20,
                        );
                        close(
                            sensitivity.diffusion_charge,
                            (upper.diffusion_charge.value - lower.diffusion_charge.value)
                                / (2.0 * delta),
                            2e-5,
                            1e-31,
                        );
                        current_points += 1;
                        charge_points += 1;
                        if saturation == 0.0 {
                            assert_eq!(sensitivity, substrate::TemperatureSensitivity::default());
                        }
                        if storage == 0.0 {
                            assert_eq!(sensitivity.diffusion_charge, 0.0);
                        }
                    }
                }
            }
        }
    }
    assert_eq!(current_points, 360);
    assert_eq!(charge_points, 360);
    Ok(())
}

#[test]
fn physical_device_substrate_sensitivity_preserves_polarity_and_storage() -> TestResult {
    use model_kernel_bipolar::hicum2::{self, device, nqs, temperature};
    for kind in [hicum2::Polarity::Npn, hicum2::Polarity::Pnp] {
        for flcomp in [0.0, 2.2, 2.3] {
            let mut model = temperature::Nominal::source_defaults();
            model.transport.flcomp = flcomp;
            model.polarity = kind;
            model.auxiliary.itss = 1e-15;
            model.auxiliary.tsf = 3e-12;
            model.auxiliary.msf = 1.3;
            let prepared = model.prepare(Temperature::new(330.15)?, 0.0)?;
            let evaluator = prepared.device_kernel()?;
            let sign = if matches!(kind, hicum2::Polarity::Npn) {
                1.0
            } else {
                -1.0
            };
            let op = evaluator.evaluate(&device::Bias {
                nodes: device::Nodes {
                    b: sign * 0.69,
                    bp: sign * 0.65,
                    bi: sign * 0.63,
                    c: sign * 0.9,
                    ci: sign * 0.8,
                    e: 0.0,
                    ei: sign * 0.01,
                    s: sign * 0.02,
                    si: sign * 0.04,
                },
                nqs: nqs::State::default(),
            })?;
            let direct = prepared.substrate_kernel()?.temperature_sensitivity(
                substrate::Bias {
                    vbpci: op.voltage.vbpci,
                    vsici: op.voltage.vsici,
                },
                op.substrate,
            )?;
            assert_eq!(evaluator.substrate_temperature_sensitivity(&op)?, direct);
        }
    }
    Ok(())
}

#[test]
fn invalid_substrate_temperature_slopes_rejected_before_evaluation() -> TestResult {
    let p = substrate::Parameters {
        saturation: 1e-13,
        ideality: 1.4,
        forward_storage_time: 3e-12,
    };
    let temperature = Temperature::new(300.15)?;
    let baseline = substrate::Prepared::new(p, temperature)?;
    let output = baseline.evaluate(substrate::Bias {
        vbpci: 0.4,
        vsici: -0.2,
    })?;
    assert_eq!(
        baseline
            .temperature_sensitivity(
                substrate::Bias {
                    vbpci: 0.4,
                    vsici: -0.2
                },
                output
            )?
            .diffusion_charge,
        -(0.4 / temperature.kelvin()) * output.diffusion_charge.d_vbpci,
    );
    for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        for (name, slopes) in [
            (
                "itss_temperature_slope",
                substrate::TemperatureSlopes {
                    saturation: invalid,
                    storage_time: 0.0,
                },
            ),
            (
                "tsf_temperature_slope",
                substrate::TemperatureSlopes {
                    saturation: 0.0,
                    storage_time: invalid,
                },
            ),
        ] {
            assert!(matches!(
                substrate::Prepared::with_temperature_slopes(p,temperature,slopes),
                Err(ModelError::InvalidParameter(parameter)) if parameter==name
            ));
        }
    }
    Ok(())
}
