use model_kernel_bipolar::{EvaluationError, ModelError, Temperature, hicum2::junction};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const fn params() -> junction::Parameters {
    junction::Parameters {
        intrinsic_emitter: junction::Forward {
            zero_bias_capacitance: 2.5e-15,
            potential: 0.9,
            grading: 0.5,
            max_cap_ratio: 2.5,
        },
        peripheral_emitter: junction::Forward {
            zero_bias_capacitance: 1.0e-15,
            potential: 0.87,
            grading: 0.47,
            max_cap_ratio: 3.0,
        },
        intrinsic_collector: junction::Collector {
            zero_bias_capacitance: 1.4e-15,
            potential: 0.7,
            grading: 0.4,
            punch_through: 100.0,
        },
        external_collector: junction::ExternalCollector {
            external_zero_bias_capacitance: 1.1e-15,
            peripheral_zero_bias_capacitance: 0.8e-15,
            potential: 0.74,
            grading: 0.41,
            punch_through: 3.0,
        },
        substrate: junction::Collector {
            zero_bias_capacitance: 0.9e-15,
            potential: 0.6,
            grading: 0.5,
            punch_through: 1.5,
        },
        peripheral_substrate: junction::PeripheralSubstrate {
            zero_bias_capacitance: 0.4e-15,
            potential: 0.0,
            grading: 0.5,
            punch_through: 100.0,
        },
    }
}

fn close(actual: f64, expected: f64, relative: f64, absolute: f64) {
    let limit = relative.mul_add(actual.abs().max(expected.abs()), absolute);
    assert!(
        (actual - expected).abs() <= limit,
        "actual={actual:.15e}; expected={expected:.15e}; tolerance={limit:.3e}"
    );
}

#[test]
fn all_seven_physical_junction_capacitances_are_charge_gradients() -> TestResult {
    let ambient = [233.15, 273.15, 300.15, 373.15, 398.15];
    for temperature in ambient {
        let prepared = junction::Prepared::new(params(), Temperature::new(temperature)?)?;
        for voltage in [-7.0, -3.0, -1.0, -0.5, 0.0, 0.4, 0.7, 0.9, 1.3] {
            let bias = junction::Bias {
                vbiei: voltage,
                vbpei: voltage,
                vbici: voltage,
                vbci: voltage,
                vbpci: voltage,
                vsici: voltage,
                vsc: voltage,
            };
            let output = prepared.evaluate(bias)?;
            let epsilon = 1e-6;
            let low = prepared.evaluate(junction::Bias {
                vbiei: voltage - epsilon,
                vbpei: voltage - epsilon,
                vbici: voltage - epsilon,
                vbci: voltage - epsilon,
                vbpci: voltage - epsilon,
                vsici: voltage - epsilon,
                vsc: voltage - epsilon,
            })?;
            let high = prepared.evaluate(junction::Bias {
                vbiei: voltage + epsilon,
                vbpei: voltage + epsilon,
                vbici: voltage + epsilon,
                vbci: voltage + epsilon,
                vbpci: voltage + epsilon,
                vsici: voltage + epsilon,
                vsc: voltage + epsilon,
            })?;
            let branches = [
                (
                    output.intrinsic_emitter,
                    low.intrinsic_emitter,
                    high.intrinsic_emitter,
                ),
                (
                    output.peripheral_emitter,
                    low.peripheral_emitter,
                    high.peripheral_emitter,
                ),
                (
                    output.intrinsic_collector,
                    low.intrinsic_collector,
                    high.intrinsic_collector,
                ),
                (
                    output.external_collector,
                    low.external_collector,
                    high.external_collector,
                ),
                (
                    output.peripheral_collector,
                    low.peripheral_collector,
                    high.peripheral_collector,
                ),
                (output.substrate, low.substrate, high.substrate),
                (
                    output.peripheral_substrate,
                    low.peripheral_substrate,
                    high.peripheral_substrate,
                ),
            ];
            for (middle, low, high) in branches {
                let slope = (high.coulombs - low.coulombs) / (2.0 * epsilon);
                close(middle.farads, slope, 2.5e-5, 1e-19);
                assert!(middle.farads >= 0.0);
            }
        }
    }
    Ok(())
}

#[test]
fn punchthrough_and_forward_models_integrate_capacitance_consistently() -> TestResult {
    // An independent quadrature identity verifies the charge integral over
    // a substantial voltage interval, not just local finite differences.
    let prepared = junction::Prepared::new(params(), Temperature::new(300.15)?)?;
    let start = -4.0;
    let end = 0.95;
    let n = 4800;
    let step = (end - start) / f64::from(n);
    let mut integral = [0.0; 7];
    let mut first = [0.0; 7];
    let mut last = [0.0; 7];
    let mut prev = [0.0; 7];
    for i in 0..=n {
        let voltage = f64::from(i).mul_add(step, start);
        let result = prepared.evaluate(junction::Bias {
            vbiei: voltage,
            vbpei: voltage,
            vbici: voltage,
            vbci: voltage,
            vbpci: voltage,
            vsici: voltage,
            vsc: voltage,
        })?;
        let charges = [
            result.intrinsic_emitter,
            result.peripheral_emitter,
            result.intrinsic_collector,
            result.external_collector,
            result.peripheral_collector,
            result.substrate,
            result.peripheral_substrate,
        ];
        for (j, charge) in charges.iter().enumerate() {
            if i == 0 {
                first[j] = charge.coulombs;
            }
            if i > 0 {
                integral[j] = ((prev[j] + charge.farads) * step).mul_add(0.5, integral[j]);
            }
            prev[j] = charge.farads;
            if i == n {
                last[j] = charge.coulombs;
            }
        }
    }
    for (j, integrated) in integral.iter().enumerate() {
        close(*integrated, last[j] - first[j], 1.5e-5, 3e-19);
    }
    Ok(())
}

#[test]
fn punchthrough_switch_and_hicum_24_perimeter_constant_capacitance() -> TestResult {
    let mut model = params();
    let prepared = junction::Prepared::new(model, Temperature::new(300.15)?)?;
    let bias = junction::Bias {
        vbiei: 0.3,
        vbpei: 0.5,
        vbici: -1.0,
        vbci: -1.0,
        vbpci: 0.9,
        vsici: -2.0,
        vsc: -0.75,
    };
    let result = prepared.evaluate(bias)?;
    close(
        result.peripheral_substrate.coulombs,
        -0.75 * 0.4e-15,
        0.0,
        0.0,
    );
    close(result.peripheral_substrate.farads, 0.4e-15, 0.0, 0.0);
    assert_eq!(
        result.peripheral_substrate.source_capacitance,
        result.peripheral_substrate.farads
    );

    // The published QJMOD C expression is not exactly d(QJMOD Q)/dV
    // around the reach-through transition. Both meanings are retained.
    let mut source_difference = 0.0f64;
    for collector_voltage in [-7.0, -3.0, -1.0, 0.0, 0.6, 0.9] {
        let test_bias = junction::Bias {
            vbici: collector_voltage,
            vbci: collector_voltage,
            ..bias
        };
        let output = prepared.evaluate(test_bias)?;
        source_difference = source_difference.max(
            (output.external_collector.source_capacitance - output.external_collector.farads).abs(),
        );
    }
    assert!(source_difference > 1e-22);
    model.external_collector.punch_through = 100.0;
    let hard_off = junction::Prepared::new(model, Temperature::new(300.15)?)?;
    let forward_cap = hard_off.evaluate(bias)?;
    assert_ne!(
        forward_cap.external_collector.coulombs,
        result.external_collector.coulombs
    );
    model.external_collector.punch_through = 101.0;
    assert_eq!(
        junction::Prepared::new(model, Temperature::new(300.15)?)?.evaluate(bias)?,
        forward_cap
    );
    Ok(())
}

#[test]
fn zero_depletion_capacitances_do_not_inject_phantom_charge() -> TestResult {
    let mut model = params();
    model.intrinsic_emitter.zero_bias_capacitance = 0.0;
    model.peripheral_emitter.zero_bias_capacitance = 0.0;
    model.intrinsic_collector.zero_bias_capacitance = 0.0;
    model.external_collector.external_zero_bias_capacitance = 0.0;
    model.external_collector.peripheral_zero_bias_capacitance = 0.0;
    model.substrate.zero_bias_capacitance = 0.0;
    model.peripheral_substrate.zero_bias_capacitance = 0.0;
    let kernel = junction::Prepared::new(model, Temperature::new(300.15)?)?;
    assert_eq!(
        kernel.evaluate(junction::Bias {
            vbiei: 0.9,
            vbpei: 0.8,
            vbici: -3.0,
            vbci: -3.0,
            vbpci: 0.9,
            vsici: -1.0,
            vsc: -4.0,
        })?,
        junction::Output::default()
    );
    Ok(())
}

#[test]
fn junction_parameter_and_batch_contracts_reject_invalid_domains() -> TestResult {
    let temperature = Temperature::new(300.15)?;
    let mut model = params();
    model.intrinsic_emitter.grading = 0.0;
    assert!(matches!(
        junction::Prepared::new(model, temperature),
        Err(ModelError::InvalidParameter("intrinsic_emitter"))
    ));
    model = params();
    model.external_collector.punch_through = -0.3;
    assert!(matches!(
        junction::Prepared::new(model, temperature),
        Err(ModelError::InvalidParameter("external_collector"))
    ));
    model = params();
    model.external_collector.potential = 50.0;
    model.external_collector.punch_through = 0.1;
    assert!(matches!(
        junction::Prepared::new(model, temperature),
        Err(ModelError::InvalidParameter("external_collector"))
    ));
    model = params();
    model.peripheral_substrate.potential = f64::NAN;
    assert!(matches!(
        junction::Prepared::new(model, temperature),
        Err(ModelError::InvalidParameter(
            "peripheral_substrate_potential"
        ))
    ));
    let prepared = junction::Prepared::new(params(), temperature)?;
    let input = junction::Bias {
        vbiei: 0.6,
        vbpei: 0.4,
        vbici: -1.0,
        vbci: -1.0,
        vbpci: -0.5,
        vsici: -0.7,
        vsc: -1.0,
    };
    let mut outputs = [junction::Output::default(); 2];
    prepared.evaluate_batch(&[input, input], &mut outputs)?;
    assert_eq!(outputs[0], outputs[1]);
    assert!(matches!(
        prepared.evaluate_batch(&[input], &mut outputs),
        Err(EvaluationError::LengthMismatch)
    ));
    let mut invalid = input;
    invalid.vbiei = f64::NAN;
    assert!(matches!(
        prepared.evaluate(invalid),
        Err(EvaluationError::NonFiniteBias)
    ));
    Ok(())
}

#[test]
fn published_hicum_capacitance_gradient_matches_finite_difference() -> TestResult {
    // HICUM's source C is not everywhere dQ/dV. Its *own* analytic
    // derivative is needed by avalanche where v_q=qavl/Cjci.
    for temp in [233.15, 300.15, 398.15] {
        let kernel = junction::Prepared::new(params(), Temperature::new(temp)?)?;
        for voltage in [-7.0, -3.0, -1.0, -0.25, 0.0, 0.6, 0.9, 1.3] {
            let h = 2e-6;
            let shared = |value| junction::Bias {
                vbiei: value,
                vbpei: value,
                vbici: value,
                vbci: value,
                vbpci: value,
                vsici: value,
                vsc: value,
            };
            let center = kernel.evaluate(shared(voltage))?;
            let upper = kernel.evaluate(shared(voltage + h))?;
            let lower = kernel.evaluate(shared(voltage - h))?;
            let derivative =
                |middle: junction::Junction, high: junction::Junction, low: junction::Junction| {
                    close(
                        middle.d_source_capacitance,
                        (high.source_capacitance - low.source_capacitance) / (2.0 * h),
                        3e-5,
                        1e-20,
                    );
                };
            derivative(
                center.intrinsic_emitter,
                upper.intrinsic_emitter,
                lower.intrinsic_emitter,
            );
            derivative(
                center.peripheral_emitter,
                upper.peripheral_emitter,
                lower.peripheral_emitter,
            );
            derivative(
                center.intrinsic_collector,
                upper.intrinsic_collector,
                lower.intrinsic_collector,
            );
            derivative(
                center.external_collector,
                upper.external_collector,
                lower.external_collector,
            );
            derivative(
                center.peripheral_collector,
                upper.peripheral_collector,
                lower.peripheral_collector,
            );
            derivative(center.substrate, upper.substrate, lower.substrate);
            derivative(
                center.peripheral_substrate,
                upper.peripheral_substrate,
                lower.peripheral_substrate,
            );
        }
    }
    Ok(())
}

#[test]
fn external_collector_charge_uses_external_base_voltage_independently() -> TestResult {
    let prepared = junction::Prepared::new(params(), Temperature::new(300.15)?)?;
    let bias = junction::Bias {
        vbiei: 0.73,
        vbpei: 0.68,
        vbici: -0.43,
        vbci: -0.71,
        vbpci: -0.60,
        vsici: -1.0,
        vsc: -1.0,
    };
    let center = prepared.evaluate(bias)?;
    let h = 1e-6;
    let varied = |delta_external: f64, delta_intrinsic: f64| {
        prepared.evaluate(junction::Bias {
            vbci: bias.vbci + delta_external,
            vbici: bias.vbici + delta_intrinsic,
            ..bias
        })
    };
    let external_high = varied(h, 0.0)?;
    let external_low = varied(-h, 0.0)?;
    let intrinsic_high = varied(0.0, h)?;
    let intrinsic_low = varied(0.0, -h)?;
    close(
        (external_high.external_collector.coulombs - external_low.external_collector.coulombs)
            / (2.0 * h),
        center.external_collector.farads,
        1e-7,
        1e-22,
    );
    assert_eq!(
        external_high.intrinsic_collector,
        external_low.intrinsic_collector,
    );
    assert_eq!(
        intrinsic_high.external_collector,
        intrinsic_low.external_collector,
    );
    close(
        (intrinsic_high.intrinsic_collector.coulombs - intrinsic_low.intrinsic_collector.coulombs)
            / (2.0 * h),
        center.intrinsic_collector.farads,
        1e-7,
        1e-22,
    );
    assert_ne!(
        center.intrinsic_collector.coulombs,
        center.external_collector.coulombs,
    );
    Ok(())
}
