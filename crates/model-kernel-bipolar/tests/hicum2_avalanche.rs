use model_kernel_bipolar::{
    EvaluationError, ModelError, Temperature,
    hicum2::{avalanche, junction, transport},
};
#[path = "fixtures/hicum2_baseline.rs"]
mod baseline;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn close(actual: f64, expected: f64, relative: f64, absolute: f64) {
    let limit = relative.mul_add(actual.abs().max(expected.abs()), absolute);
    assert!(
        (actual - expected).abs() <= limit,
        "actual={actual:.15e}, expected={expected:.15e}, tolerance={limit:.3e}",
    );
}

fn physics(
    transport_kernel: &transport::Prepared,
    junction_kernel: &junction::Prepared,
    avalanche_kernel: &avalanche::Prepared,
    emitter_voltage: f64,
    collector_voltage: f64,
) -> Result<avalanche::Output, EvaluationError> {
    let forward = transport_kernel
        .evaluate(transport::Bias {
            vbiei: emitter_voltage,
            vbici: collector_voltage,
        })?
        .forward_current;
    let collector = junction_kernel
        .intrinsic_for_transport(emitter_voltage, collector_voltage)?
        .collector;
    avalanche_kernel.evaluate(avalanche::Bias {
        vbici: collector_voltage,
        forward,
        intrinsic_collector: collector,
    })
}

#[test]
fn source_hicavl_gradient_matches_full_hiccr_and_hicjq_finite_difference() -> TestResult {
    let mut p = baseline::parameters();
    p.latb = 0.0;
    p.latl = 0.0;
    p.vcbar = 0.0;
    let temperature = Temperature::new(300.15)?;
    let transport_kernel = transport::Prepared::new(&p, temperature)?;
    let junction_kernel = junction::Prepared::new(p.junction, temperature)?;
    for favl in [0.015, 0.12] {
        for qavl in [0.0, 1e-18, 1e-16, 5e-16, 1e-15] {
            for kavl in [0.0, 0.4, 1.5] {
                let kernel = avalanche::Prepared::new(avalanche::Parameters {
                    favl,
                    qavl,
                    kavl,
                    collector_zero_capacitance: p
                        .junction
                        .intrinsic_collector
                        .zero_bias_capacitance,
                    collector_potential: p.junction.intrinsic_collector.potential,
                })?;
                for be in [0.55, 0.68, 0.75, 0.9] {
                    for bc in [-3.0, -0.9, -0.3, 0.2, 0.8] {
                        let center = physics(&transport_kernel, &junction_kernel, &kernel, be, bc)?;
                        let step = 1e-6;
                        let emitter_plus =
                            physics(&transport_kernel, &junction_kernel, &kernel, be + step, bc)?;
                        let emitter_minus =
                            physics(&transport_kernel, &junction_kernel, &kernel, be - step, bc)?;
                        let collector_plus =
                            physics(&transport_kernel, &junction_kernel, &kernel, be, bc + step)?;
                        let collector_minus =
                            physics(&transport_kernel, &junction_kernel, &kernel, be, bc - step)?;
                        close(
                            center.generation.d_vbiei,
                            (emitter_plus.generation.value - emitter_minus.generation.value)
                                / (2.0 * step),
                            5e-5,
                            1e-13,
                        );
                        close(
                            center.generation.d_vbici,
                            (collector_plus.generation.value - collector_minus.generation.value)
                                / (2.0 * step),
                            5e-5,
                            1e-13,
                        );
                        close(
                            center.d_gain_vbici,
                            (collector_plus.gain - collector_minus.gain) / (2.0 * step),
                            5e-5,
                            1e-8,
                        );
                        if bc >= p.junction.intrinsic_collector.potential {
                            assert_eq!(center, avalanche::Output::default());
                        } else {
                            assert!(center.gain >= 0.0);
                            assert!(center.generation.value >= 0.0);
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

#[test]
fn source_zero_width_and_disabled_avalanche_do_not_emit_phantom_current() -> TestResult {
    let source = transport::Value2 {
        value: 0.02,
        d_vbiei: 0.3,
        d_vbici: -0.002,
    };
    let p = baseline::parameters();
    let j = junction::Prepared::new(p.junction, Temperature::new(300.15)?)?;
    let collector = j.intrinsic_for_transport(0.85, -0.5)?.collector;
    for (favl, zero_cap) in [(0.0, 1e-15), (0.12, 0.0)] {
        let kernel = avalanche::Prepared::new(avalanche::Parameters {
            favl,
            qavl: 1e-16,
            kavl: 0.4,
            collector_zero_capacitance: zero_cap,
            collector_potential: 0.7,
        })?;
        assert_eq!(
            kernel.evaluate(avalanche::Bias {
                vbici: -0.5,
                forward: source,
                intrinsic_collector: collector,
            })?,
            avalanche::Output::default(),
        );
    }
    let kernel = avalanche::Prepared::new(avalanche::Parameters {
        favl: 0.12,
        qavl: 1e-16,
        kavl: 0.4,
        collector_zero_capacitance: 1.4e-15,
        collector_potential: 0.7,
    })?;
    assert_eq!(
        kernel.evaluate(avalanche::Bias {
            vbici: 0.7,
            forward: source,
            intrinsic_collector: collector,
        })?,
        avalanche::Output::default()
    );
    assert!(matches!(
        avalanche::Prepared::new(avalanche::Parameters {
            favl: -0.1,
            qavl: 1e-16,
            kavl: 0.4,
            collector_zero_capacitance: 1.4e-15,
            collector_potential: 0.7,
        }),
        Err(ModelError::InvalidParameter("favl"))
    ));
    Ok(())
}

#[test]
fn source_hicavl_batch_and_input_contract() -> TestResult {
    let p = baseline::parameters();
    let junction = junction::Prepared::new(p.junction, Temperature::new(300.15)?)?;
    let collector = junction.intrinsic_for_transport(0.75, -0.5)?.collector;
    let kernel = avalanche::Prepared::new(avalanche::Parameters {
        favl: 0.04,
        qavl: 4e-16,
        kavl: 0.4,
        collector_zero_capacitance: p.junction.intrinsic_collector.zero_bias_capacitance,
        collector_potential: p.junction.intrinsic_collector.potential,
    })?;
    let input = avalanche::Bias {
        vbici: -0.5,
        intrinsic_collector: collector,
        forward: transport::Value2 {
            value: 0.001,
            d_vbiei: 0.06,
            d_vbici: -0.03,
        },
    };
    let mut results = [avalanche::Output::default(); 2];
    kernel.evaluate_batch(&[input, input], &mut results)?;
    assert_eq!(results[0], results[1]);
    assert!(matches!(
        kernel.evaluate_batch(&[input], &mut results),
        Err(EvaluationError::LengthMismatch)
    ));
    let mut invalid = input;
    invalid.intrinsic_collector.source_capacitance = f64::NAN;
    assert!(matches!(
        kernel.evaluate(invalid),
        Err(EvaluationError::NonFiniteBias)
    ));
    invalid = input;
    invalid.intrinsic_collector.source_capacitance = 0.0;
    assert!(matches!(
        kernel.evaluate(invalid),
        Err(EvaluationError::InvalidPhysicalDomain)
    ));
    invalid = input;
    invalid.forward.value = -0.2;
    assert!(matches!(
        kernel.evaluate(invalid),
        Err(EvaluationError::InvalidPhysicalDomain)
    ));
    Ok(())
}

#[test]
fn native_ngspice47_hicavl_generation_oracle() -> TestResult {
    let csv = include_str!("fixtures/hicum2_ngspice47_avalanche.csv");
    let mut model = baseline::parameters();
    model.latb = 0.0;
    model.latl = 0.0;
    model.vcbar = 0.0;
    let temp = Temperature::new(300.15)?;
    let forward = transport::Prepared::new(&model, temp)?;
    let mut count = 0;
    let mut worst = (0.0f64, String::new());
    for row in csv.lines().filter(|line| !line.starts_with('#')) {
        let [
            variant,
            _source_emitter,
            _source_collector,
            intrinsic_emitter,
            intrinsic_collector,
            generated,
            _ic,
        ] = <[&str; 7]>::try_from(row.split(',').collect::<Vec<_>>())
            .map_err(|_| "bad avalanche oracle row")?;
        let (favl, qavl, kavl, punchthrough) = match variant {
            "weak" => (0.04, 4e-16, 0.0, 3.0),
            "strong" => (0.04, 4e-16, 0.4, 3.0),
            "very_strong" => (0.4, 4e-16, 1.2, 3.0),
            "reachthrough" => (0.04, 4e-16, 0.4, 100.0),
            "off" => (0.0, 4e-16, 0.4, 3.0),
            _ => return Err("invalid oracle avalanche model".into()),
        };
        let mut emitter_model = model;
        emitter_model.junction.intrinsic_collector.punch_through = punchthrough;
        let effective_junction = junction::Prepared::new(emitter_model.junction, temp)?;
        let avalanche = avalanche::Prepared::new(avalanche::Parameters {
            favl,
            qavl,
            kavl,
            collector_zero_capacitance: emitter_model
                .junction
                .intrinsic_collector
                .zero_bias_capacitance,
            collector_potential: emitter_model.junction.intrinsic_collector.potential,
        })?;
        let emitter: f64 = intrinsic_emitter.parse()?;
        let collector: f64 = intrinsic_collector.parse()?;
        let output = physics(
            &forward,
            &effective_junction,
            &avalanche,
            emitter,
            collector,
        )?;
        let expected: f64 = generated.parse()?;
        let relative = (output.generation.value - expected).abs()
            / output.generation.value.abs().max(expected.abs()).max(1e-30);
        if relative > worst.0 {
            worst = (relative, format!("{variant}: {emitter} {collector}"));
        }
        // The forward charge solve retains source's 1e-5 RTOLC stopping
        // rule. Check native source avalanche at <2e-4 relative, including
        // collector reach-through and strong multiplication, not just its
        // own synthetic derivatives.
        close(output.generation.value, expected, 2e-4, 1e-18);
        count += 1;
    }
    assert_eq!(count, 40);
    assert!(worst.0 < 2e-4, "worst native HICAVL discrepancy: {worst:?}");
    Ok(())
}

fn source_thermal_avalanche(
    prepared: &model_kernel_bipolar::hicum2::temperature::Prepared,
    vbici: f64,
    forward_current: f64,
    forward_slope: f64,
) -> Result<(avalanche::Output, avalanche::TemperatureSensitivity), Box<dyn std::error::Error>> {
    let junction = prepared.junction_kernel()?;
    let source_bias = junction::Bias {
        vbiei: 0.71,
        vbpei: 0.69,
        vbici,
        vbci: vbici - 0.05,
        vbpci: vbici - 0.08,
        vsici: vbici - 0.3,
        vsc: vbici - 0.15,
    };
    let collector = junction.evaluate(source_bias)?.intrinsic_collector;
    let bias = avalanche::Bias {
        vbici,
        forward: transport::Value2 {
            value: forward_current,
            d_vbiei: 0.14,
            d_vbici: -0.02,
        },
        intrinsic_collector: collector,
    };
    let kernel = prepared.avalanche_kernel()?;
    let op = kernel.evaluate(bias)?;
    let temp = kernel.temperature_sensitivity(
        bias,
        op,
        junction
            .collector_temperature_sensitivity(source_bias)?
            .intrinsic,
        prepared.junction_parameter_slopes.intrinsic_collector,
        forward_slope,
    )?;
    Ok((op, temp))
}

#[test]
fn exact_hicavl_gain_and_generated_current_temperature_derivatives_match_repreparation()
-> TestResult {
    use model_kernel_bipolar::hicum2::temperature;
    let mut checked = 0;
    let mut strong_cases = 0;
    for flcomp in [0.0, 2.3] {
        for favl in [0.0, 0.05, 0.3] {
            for qavl in [0.0, 1e-16, 4e-16] {
                for kavl in [0.0, 0.4, 1.1] {
                    for punch in [3.0, 100.0] {
                        let mut nominal = temperature::Nominal::source_defaults();
                        nominal.transport.flcomp = flcomp;
                        nominal.auxiliary.favl = favl;
                        nominal.auxiliary.qavl = qavl;
                        nominal.auxiliary.kavl = kavl;
                        nominal.transport.junction.intrinsic_collector.punch_through = punch;
                        nominal.coefficients.alfav = 0.003;
                        nominal.coefficients.alqav = -0.002;
                        nominal.coefficients.alkav = 0.004;
                        for kelvin in [233.15, 300.15, 373.15] {
                            let h = 0.003;
                            let middle = nominal.prepare(Temperature::new(kelvin)?, 0.0)?;
                            let lower = nominal.prepare(Temperature::new(kelvin - h)?, 0.0)?;
                            let upper = nominal.prepare(Temperature::new(kelvin + h)?, 0.0)?;
                            let d_forward = 0.0015;
                            for vbici in [-2.2, -0.8, -0.15, 0.2, 0.78] {
                                let (center, sens) =
                                    source_thermal_avalanche(&middle, vbici, 0.018, d_forward)?;
                                let (below, _) = source_thermal_avalanche(
                                    &lower,
                                    vbici,
                                    h.mul_add(-d_forward, 0.018),
                                    d_forward,
                                )?;
                                let (above, _) = source_thermal_avalanche(
                                    &upper,
                                    vbici,
                                    h.mul_add(d_forward, 0.018),
                                    d_forward,
                                )?;
                                close(
                                    sens.gain,
                                    (above.gain - below.gain) / (2.0 * h),
                                    3e-5,
                                    2e-12,
                                );
                                close(
                                    sens.generation,
                                    (above.generation.value - below.generation.value) / (2.0 * h),
                                    3e-5,
                                    2e-14,
                                );
                                if kavl > 0.0 && center.gain > 0.0 {
                                    strong_cases += 1;
                                }
                                checked += 2;
                            }
                        }
                    }
                }
            }
        }
    }
    assert_eq!(checked, 3240);
    assert!(strong_cases > 200);
    Ok(())
}

#[test]
fn avalanche_temperature_jacobian_rejects_missing_transport_sensitivity_and_disabled_cap()
-> TestResult {
    use model_kernel_bipolar::hicum2::temperature;
    let mut nominal = temperature::Nominal::source_defaults();
    nominal.auxiliary.favl = 0.2;
    nominal.auxiliary.qavl = 2e-16;
    nominal.auxiliary.kavl = 0.5;
    let prepared = nominal.prepare(Temperature::new(300.15)?, 0.0)?;
    let junction = prepared.junction_kernel()?;
    let collector = junction.intrinsic_for_transport(0.74, -0.5)?.collector;
    let bias = avalanche::Bias {
        vbici: -0.5,
        forward: transport::Value2 {
            value: 0.03,
            d_vbiei: 0.3,
            d_vbici: 0.0,
        },
        intrinsic_collector: collector,
    };
    let avalanche = prepared.avalanche_kernel()?;
    let op = avalanche.evaluate(bias)?;
    let slopes = junction
        .collector_temperature_sensitivity(junction::Bias {
            vbiei: 0.74,
            vbpei: 0.72,
            vbici: -0.5,
            vbci: -0.5,
            vbpci: -0.5,
            vsici: -0.3,
            vsc: -0.2,
        })?
        .intrinsic;
    assert!(matches!(
        avalanche.temperature_sensitivity(
            bias,
            op,
            slopes,
            prepared.junction_parameter_slopes.intrinsic_collector,
            f64::NAN,
        ),
        Err(EvaluationError::NonFiniteBias)
    ));
    nominal
        .transport
        .junction
        .intrinsic_collector
        .zero_bias_capacitance = 0.0;
    let missing = nominal.prepare(Temperature::new(300.15)?, 0.0)?;
    let result = missing.avalanche_kernel()?.temperature_sensitivity(
        bias,
        missing.avalanche_kernel()?.evaluate(bias)?,
        junction::CollectorSensitivity::default(),
        missing.junction_parameter_slopes.intrinsic_collector,
        0.001,
    )?;
    assert_eq!(result, avalanche::TemperatureSensitivity::default());
    Ok(())
}

#[test]
fn device_avalanche_gain_thermal_sensitivity_matches_standalone_both_polarities() -> TestResult {
    use model_kernel_bipolar::hicum2::{Polarity, device, nqs, temperature};
    for polarity in [Polarity::Npn, Polarity::Pnp] {
        for punch in [2.4, 100.0] {
            let mut nominal = temperature::Nominal::source_defaults();
            nominal.polarity = polarity;
            nominal.auxiliary.favl = 0.12;
            nominal.auxiliary.qavl = 4e-16;
            nominal.auxiliary.kavl = 0.7;
            nominal.coefficients.alfav = 0.002;
            nominal.coefficients.alqav = 0.004;
            nominal.coefficients.alkav = -0.001;
            nominal.transport.junction.intrinsic_collector.punch_through = punch;
            let prepared = nominal.prepare(Temperature::new(332.15)?, 0.0)?;
            let evaluator = prepared.device_kernel()?;
            let sign = if matches!(polarity, Polarity::Npn) {
                1.0
            } else {
                -1.0
            };
            for collector in [0.2, 0.9, 1.3] {
                let op = evaluator.evaluate(&device::Bias {
                    nodes: device::Nodes {
                        b: sign * 0.78,
                        bp: sign * 0.76,
                        bi: sign * 0.75,
                        c: sign * collector,
                        ci: sign * (collector - 0.01),
                        e: 0.0,
                        ei: sign * 0.015,
                        s: 0.0,
                        si: 0.0,
                    },
                    nqs: nqs::State::default(),
                })?;
                let junction = prepared.junction_kernel()?;
                let slope = junction
                    .collector_temperature_sensitivity(junction::Bias {
                        vbiei: op.voltage.vbiei,
                        vbpei: op.voltage.vbpei,
                        vbici: op.voltage.vbici,
                        vbci: op.voltage.vbci,
                        vbpci: op.voltage.vbpci,
                        vsici: op.voltage.vsici,
                        vsc: op.voltage.vsc,
                    })?
                    .intrinsic;
                let direct = prepared.avalanche_kernel()?.gain_temperature_sensitivity(
                    avalanche::Bias {
                        vbici: op.voltage.vbici,
                        forward: op.transfer.forward_current,
                        intrinsic_collector: op.junction.intrinsic_collector,
                    },
                    slope,
                    prepared.junction_parameter_slopes.intrinsic_collector,
                )?;
                close(
                    evaluator.avalanche_gain_temperature_sensitivity(&op)?,
                    direct,
                    1e-13,
                    1e-17,
                );
            }
        }
    }
    Ok(())
}
