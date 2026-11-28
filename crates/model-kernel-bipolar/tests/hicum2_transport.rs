use hicum2::transport;
use model_kernel_bipolar::{EvaluationError, ModelError, Temperature, hicum2};
type TestResult = Result<(), Box<dyn std::error::Error>>;

#[path = "fixtures/hicum2_baseline.rs"]
mod hicum2_baseline;
use hicum2_baseline::parameters;

fn close(actual: f64, expected: f64, relative: f64, absolute: f64) {
    let limit = relative.mul_add(actual.abs().max(expected.abs()), absolute);
    assert!(
        (actual - expected).abs() <= limit,
        "actual={actual:.15e}, expected={expected:.15e}, tolerance={limit:.3e}"
    );
}

fn check_axis(
    actual: transport::Value2,
    emitter_plus: transport::Value2,
    emitter_minus: transport::Value2,
    collector_plus: transport::Value2,
    collector_minus: transport::Value2,
    step: f64,
) {
    let denominator = 2.0 * step;
    close(
        actual.d_vbiei,
        (emitter_plus.value - emitter_minus.value) / denominator,
        8e-4,
        1e-16,
    );
    close(
        actual.d_vbici,
        (collector_plus.value - collector_minus.value) / denominator,
        8e-4,
        1e-16,
    );
}

fn check_gradients(kernel: &transport::Prepared, bias: transport::Bias) -> TestResult {
    let center = kernel.evaluate(bias)?;
    let step = 1e-6;
    let emitter_plus = kernel.evaluate(transport::Bias {
        vbiei: bias.vbiei + step,
        ..bias
    })?;
    let emitter_minus = kernel.evaluate(transport::Bias {
        vbiei: bias.vbiei - step,
        ..bias
    })?;
    let collector_plus = kernel.evaluate(transport::Bias {
        vbici: bias.vbici + step,
        ..bias
    })?;
    let collector_minus = kernel.evaluate(transport::Bias {
        vbici: bias.vbici - step,
        ..bias
    })?;
    macro_rules! compare {
        ($field:ident) => {
            check_axis(
                center.$field,
                emitter_plus.$field,
                emitter_minus.$field,
                collector_plus.$field,
                collector_minus.$field,
                step,
            );
        };
    }
    compare!(forward_current);
    compare!(reverse_current);
    compare!(net_transfer_current);
    compare!(equilibrium_hole_charge);
    compare!(total_hole_charge);
    compare!(forward_diffusion_charge);
    compare!(weighted_forward_charge);
    compare!(reverse_diffusion_charge);
    compare!(critical_current);
    compare!(low_current_transit_time);
    compare!(excess_base_charge);
    compare!(barrier_base_current);
    close(
        center.net_transfer_current.value,
        center.forward_current.value - center.reverse_current.value,
        1e-13,
        1e-18,
    );
    assert!(center.total_hole_charge.value > 0.0);
    assert!(center.critical_current.value > 0.0);
    if center.iterations > 0 {
        close(
            center.total_hole_charge.value,
            center.equilibrium_hole_charge.value
                + center.weighted_forward_charge.value
                + center.reverse_diffusion_charge.value,
            2e-8,
            1e-21,
        );
    }
    Ok(())
}

#[test]
fn hiccr_implicit_transport_charge_and_cross_derivatives() -> TestResult {
    let temperature = Temperature::new(300.15)?;
    let mut points = 0;
    for (width, length) in [
        (0.0, 0.0),
        (0.2, 0.1),
        (0.2, 0.2),
        (0.0, 0.2),
        (0.005, 0.002),
    ] {
        for compatibility in [0.0, 2.3] {
            for high_current in [0.0, 0.04] {
                for hjei_slope in [0.0, 0.5] {
                    let mut model = parameters();
                    model.latb = width;
                    model.latl = length;
                    model.flcomp = compatibility;
                    model.ich = high_current;
                    model.ahjei_nominal = hjei_slope;
                    model.ahjei = hjei_slope;
                    let kernel = transport::Prepared::new(&model, temperature)?;
                    for emitter in [0.55, 0.68, 0.8, 0.9] {
                        for collector in [-1.0, -0.3, 0.15] {
                            check_gradients(
                                &kernel,
                                transport::Bias {
                                    vbiei: emitter,
                                    vbici: collector,
                                },
                            )?;
                            points += 1;
                        }
                    }
                }
            }
        }
    }
    assert_eq!(points, 480);
    Ok(())
}

#[test]
fn zero_giccr_carriers_preserve_nonzero_equilibrium_hole_charge() -> TestResult {
    let mut model = parameters();
    model.c10 = 0.0;
    let kernel = transport::Prepared::new(&model, Temperature::new(300.15)?)?;
    for bias in [
        transport::Bias {
            vbiei: 0.0,
            vbici: -0.9,
        },
        transport::Bias {
            vbiei: 0.8,
            vbici: 0.2,
        },
    ] {
        let output = kernel.evaluate(bias)?;
        assert_eq!(output.forward_current, transport::Value2::default());
        assert_eq!(output.reverse_current, transport::Value2::default());
        assert_eq!(
            output.forward_diffusion_charge,
            transport::Value2::default()
        );
        assert_eq!(
            output.reverse_diffusion_charge,
            transport::Value2::default()
        );
        assert_eq!(output.net_transfer_current, transport::Value2::default());
        assert_eq!(output.iterations, 0);
        close(
            output.total_hole_charge.value,
            output.equilibrium_hole_charge.value,
            0.0,
            0.0,
        );
    }
    Ok(())
}

#[test]
fn hiccr_native_vs_historical_weighted_spreading_branches() -> TestResult {
    let mut p = parameters();
    p.latb = 0.4;
    p.latl = 0.06;
    p.hfc = 0.6;
    p.hfe = 1.4;
    p.hf0 = 0.85;
    let temperature = Temperature::new(300.15)?;
    p.flcomp = 0.0;
    let historical = transport::Prepared::new(&p, temperature)?;
    p.flcomp = 2.3;
    let native = transport::Prepared::new(&p, temperature)?;
    let bias = transport::Bias {
        vbiei: 0.91,
        vbici: -0.2,
    };
    let a = historical.evaluate(bias)?;
    let b = native.evaluate(bias)?;
    assert!(a.iterations > 0 && b.iterations > 0);
    assert_ne!(
        a.weighted_forward_charge.value,
        b.weighted_forward_charge.value
    );
    assert_ne!(a.forward_current.value, b.forward_current.value);
    Ok(())
}

#[test]
fn hiccr_source_temperature_and_zero_slope_hjei_limit() -> TestResult {
    let mut p = parameters();
    p.ahjei_nominal = 0.5;
    p.ahjei = 0.0;
    p.hjei_zero_bias = 1.2;
    for kelvin in [233.15, 300.15, 398.15] {
        let kernel = transport::Prepared::new(&p, Temperature::new(kelvin)?)?;
        let output = kernel.evaluate(transport::Bias {
            vbiei: 0.7,
            vbici: -0.5,
        })?;
        assert!(output.total_hole_charge.value > 0.0);
        assert!(output.forward_current.value > 0.0);
        assert!(output.forward_transit_time >= 0.0);
        assert!(output.weighted_transit_time >= 0.0);
    }
    Ok(())
}

#[test]
fn hiccr_prepared_validation_and_borrowed_batch_contract() -> TestResult {
    let temperature = Temperature::new(300.15)?;
    let mut p = parameters();
    p.rci0 = 0.0;
    assert!(matches!(
        transport::Prepared::new(&p, temperature),
        Err(ModelError::InvalidParameter("rci0"))
    ));
    p = parameters();
    p.fthc = 1.2;
    assert!(matches!(
        transport::Prepared::new(&p, temperature),
        Err(ModelError::InvalidParameter("fthc"))
    ));
    p = parameters();
    p.c10 = f64::NAN;
    assert!(matches!(
        transport::Prepared::new(&p, temperature),
        Err(ModelError::InvalidParameter("c10"))
    ));
    let kernel = transport::Prepared::new(&parameters(), temperature)?;
    let input = [transport::Bias {
        vbiei: 0.75,
        vbici: -0.4,
    }; 3];
    let mut outputs = [transport::Output::default(); 3];
    kernel.evaluate_batch(&input, &mut outputs)?;
    assert!(outputs.iter().all(|value| *value == outputs[0]));
    assert!(matches!(
        kernel.evaluate_batch(&input, &mut outputs[..2]),
        Err(EvaluationError::LengthMismatch)
    ));
    assert!(matches!(
        kernel.evaluate(transport::Bias {
            vbiei: f64::NAN,
            vbici: 0.0
        }),
        Err(EvaluationError::NonFiniteBias)
    ));
    Ok(())
}

fn parameters_for_native_oracle(variant: &str) -> Result<transport::Parameters, &'static str> {
    let mut model = parameters();
    match variant {
        "native" => {}
        "historical" => model.flcomp = 0.0,
        "symmetric" => model.latl = 0.2,
        "elongated" => {
            model.latb = 0.0;
            model.latl = 0.2;
        }
        "high_correction" => model.ich = 0.04,
        "hjei_slope" => {
            model.ahjei_nominal = 0.5;
            model.ahjei = 0.5;
        }
        "no_lateral" => {
            model.latb = 0.0;
            model.latl = 0.0;
        }
        "no_storage" => {
            model.thcs = 0.0;
            model.tef0 = 0.0;
        }
        "no_collector_storage" => model.thcs = 0.0,
        "no_emitter_storage" => model.tef0 = 0.0,
        "no_barrier" => model.vcbar = 0.0,
        "historical_no_barrier" => {
            model.vcbar = 0.0;
            model.flcomp = 0.0;
        }
        "correction_no_barrier" => {
            model.vcbar = 0.0;
            model.ich = 0.04;
        }
        "symmetric_no_barrier" => {
            model.vcbar = 0.0;
            model.latl = 0.2;
        }
        "elongated_no_barrier" => {
            model.vcbar = 0.0;
            model.latb = 0.0;
            model.latl = 0.2;
        }
        "tiny_no_barrier" => {
            model.vcbar = 0.0;
            model.latb = 0.005;
            model.latl = 0.002;
        }
        _ => return Err("unknown HICUM source variant"),
    }
    Ok(model)
}

#[test]
fn hiccr_shared_source_physics_matches_native_ngspice_47() -> TestResult {
    // The ngspice 47 built-in level=8 device reports version 2.4.0, but its
    // combined lateral-spreading/collector-barrier charge differs from the
    // published VA-Models 2.4.0 HICQFC source. Test exact shared physical
    // branches rather than changing the target equations to mimic it.
    // All 144 ICK observations and 72 shared-subspace It/TF values are checked.
    let csv = include_str!("fixtures/hicum2_ngspice47_transport.csv");
    let temperature = Temperature::new(300.15)?;
    let mut count = 0;
    let mut common = 0;
    let mut different_barrier_cases = 0;
    for row in csv.lines().filter(|line| !line.starts_with('#')) {
        let [
            variant,
            _source_emitter,
            _source_collector,
            emitter_voltage_text,
            collector_voltage_text,
            current_text,
            critical_text,
            transit_text,
        ] = <[&str; 8]>::try_from(row.split(',').collect::<Vec<_>>())
            .map_err(|_| "invalid native HICUM level 8 oracle row")?;
        let model = parameters_for_native_oracle(variant)?;
        let kernel = transport::Prepared::new(&model, temperature)?;
        let bias = transport::Bias {
            vbiei: emitter_voltage_text.parse()?,
            vbici: collector_voltage_text.parse()?,
        };
        let output = kernel.evaluate(bias)?;
        let reference_current: f64 = current_text.parse()?;
        let reference_critical: f64 = critical_text.parse()?;
        let reference_time: f64 = transit_text.parse()?;
        close(
            output.critical_current.value,
            reference_critical,
            2e-8,
            2e-13,
        );
        if matches!(
            variant,
            "no_lateral"
                | "no_storage"
                | "no_collector_storage"
                | "no_barrier"
                | "correction_no_barrier"
                | "symmetric_no_barrier"
                | "elongated_no_barrier"
                | "tiny_no_barrier"
        ) {
            // The native implicit charge solver terminates at ~1e-5 relative
            // step, so the exported It/TF values need a small relative
            // tolerance even with source-identical branch expressions.
            close(
                output.net_transfer_current.value,
                reference_current,
                1e-4,
                2e-16,
            );
            // Native HICQFC documents an incomplete dQf_CT/dItf derivative;
            // the tiny-geometry historical limit differs by ~2.5e-4.
            let time_tolerance = if variant == "tiny_no_barrier" {
                4e-4
            } else {
                1e-4
            };
            close(
                output.forward_transit_time,
                reference_time,
                time_tolerance,
                1e-17,
            );
            common += 1;
        } else {
            // Source-version discriminator only, not a conformance check.
            // Cases where both VCbar and lateral spreading contribute belong
            // to the published VA 2.4.0 implementation, not this ngspice
            // binary's differently weighted HICQFC branch.
            if (output.net_transfer_current.value / reference_current - 1.0).abs() > 1e-4 {
                different_barrier_cases += 1;
            }
        }
        count += 1;
    }
    assert_eq!(count, 144);
    assert_eq!(common, 72);
    assert!(different_barrier_cases > 0);
    Ok(())
}

#[test]
fn hiccr_barrier_base_recombination_is_source_charge_divided_by_tbhrec() -> TestResult {
    let ambient = Temperature::new(300.15)?;
    for tbhrec in [0.0, 1e-12, 4e-12, 1e-10] {
        for flcomp in [0.0, 2.3] {
            for latb in [0.0, 0.2] {
                let mut p = parameters();
                p.tbhrec = tbhrec;
                p.flcomp = flcomp;
                p.vcbar = 0.0;
                p.latb = latb;
                p.latl = latb / 2.0;
                let kernel = transport::Prepared::new(&p, ambient)?;
                for be in [0.7, 0.75, 0.8, 0.85, 0.9] {
                    let bias = transport::Bias {
                        vbiei: be,
                        vbici: be - 1.3,
                    };
                    let output = kernel.evaluate(bias)?;
                    let current = output.barrier_base_current;
                    if tbhrec == 0.0 {
                        assert_eq!(current, transport::Value2::default());
                    } else {
                        close(
                            current.value,
                            output.excess_base_charge.value / tbhrec,
                            1e-14,
                            1e-28,
                        );
                        close(
                            current.d_vbiei,
                            output.excess_base_charge.d_vbiei / tbhrec,
                            1e-14,
                            1e-28,
                        );
                        close(
                            current.d_vbici,
                            output.excess_base_charge.d_vbici / tbhrec,
                            1e-14,
                            1e-28,
                        );
                        let delta = 1e-6;
                        let plus_emitter = kernel.evaluate(transport::Bias {
                            vbiei: be + delta,
                            ..bias
                        })?;
                        let minus_emitter = kernel.evaluate(transport::Bias {
                            vbiei: be - delta,
                            ..bias
                        })?;
                        close(
                            current.d_vbiei,
                            (plus_emitter.barrier_base_current.value
                                - minus_emitter.barrier_base_current.value)
                                / (2.0 * delta),
                            3e-4,
                            1e-15,
                        );
                        let plus_collector = kernel.evaluate(transport::Bias {
                            vbici: bias.vbici + delta,
                            ..bias
                        })?;
                        let minus_collector = kernel.evaluate(transport::Bias {
                            vbici: bias.vbici - delta,
                            ..bias
                        })?;
                        close(
                            current.d_vbici,
                            (plus_collector.barrier_base_current.value
                                - minus_collector.barrier_base_current.value)
                                / (2.0 * delta),
                            3e-4,
                            1e-15,
                        );
                    }
                }
            }
        }
    }
    let mut p = parameters();
    p.tbhrec = -1.0;
    assert!(matches!(
        transport::Prepared::new(&p, ambient),
        Err(ModelError::InvalidParameter("tbhrec"))
    ));
    Ok(())
}

#[test]
fn analytic_mixed_forward_transit_time_gradient_matches_independent_bias_differences() -> TestResult
{
    let mut points = 0;
    let mut emitter_sensitivity = 0;
    let mut collector_sensitivity = 0;
    for variant in [
        "native",
        "historical",
        "symmetric",
        "elongated",
        "high_correction",
        "hjei_slope",
        "no_lateral",
        "no_collector_storage",
        "no_emitter_storage",
        "no_barrier",
        "tiny_no_barrier",
    ] {
        let model = parameters_for_native_oracle(variant)?;
        for kelvin in [233.15, 300.15, 373.15] {
            let kernel = transport::Prepared::new(&model, Temperature::new(kelvin)?)?;
            for vbe in [0.71, 0.78, 0.87, 1.04] {
                for vbc in [-0.95, -0.45, 0.04, 0.2] {
                    let bias = transport::Bias {
                        vbiei: vbe,
                        vbici: vbc,
                    };
                    let center = kernel.evaluate(bias)?;
                    let h = 2e-6;
                    let emitter_high = kernel
                        .evaluate(transport::Bias {
                            vbiei: vbe + h,
                            ..bias
                        })?
                        .forward_transit_time;
                    let emitter_low = kernel
                        .evaluate(transport::Bias {
                            vbiei: vbe - h,
                            ..bias
                        })?
                        .forward_transit_time;
                    let collector_high = kernel
                        .evaluate(transport::Bias {
                            vbici: vbc + h,
                            ..bias
                        })?
                        .forward_transit_time;
                    let collector_low = kernel
                        .evaluate(transport::Bias {
                            vbici: vbc - h,
                            ..bias
                        })?
                        .forward_transit_time;
                    let expected = [
                        (emitter_high - emitter_low) / (2.0 * h),
                        (collector_high - collector_low) / (2.0 * h),
                    ];
                    for (predicted, expected) in center
                        .forward_transit_time_gradient
                        .into_iter()
                        .zip(expected)
                    {
                        close(predicted, expected, 3e-3, 1e-17);
                    }
                    emitter_sensitivity += usize::from(expected[0].abs() > 1e-16);
                    collector_sensitivity += usize::from(expected[1].abs() > 1e-16);
                    points += 1;
                }
            }
        }
    }
    assert_eq!(points, 528);
    assert!(emitter_sensitivity > 50);
    assert!(collector_sensitivity > 50);
    Ok(())
}
