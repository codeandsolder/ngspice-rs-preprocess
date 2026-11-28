use model_kernel_bipolar::{
    EvaluationError, ModelError, Temperature,
    hicum2::{nqs, transport},
};
#[path = "fixtures/hicum2_baseline.rs"]
mod baseline;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn close(actual: f64, expected: f64, rel: f64, abs: f64) {
    let limit = rel.mul_add(actual.abs().max(expected.abs()), abs);
    assert!(
        (actual - expected).abs() <= limit,
        "actual={actual:.15e}, expected={expected:.15e}, limit={limit:.5e}"
    );
}

const fn params() -> nqs::Parameters {
    nqs::Parameters {
        flnqs: true,
        flcomp: 2.3,
        alit: 0.333,
        alqf: 0.167,
        t0: 2e-12,
    }
}

#[test]
fn source_nqs_enable_gate_and_short_algebraic_auxiliary_nodes() -> TestResult {
    let mut p = params();
    for (flcomp, flnqs, enabled) in [
        (0.0, false, true),
        (2.1, false, true),
        (2.2, false, false),
        (2.3, false, false),
        (2.3, true, true),
        (2.2, true, true),
    ] {
        p.flcomp = flcomp;
        p.flnqs = flnqs;
        let kernel = nqs::Prepared::new(p)?;
        assert_eq!(kernel.is_active(4e-12), enabled);
        assert!(!kernel.is_active(0.0));
        let temp = Temperature::new(300.15)?;
        let original =
            transport::Prepared::new(&baseline::parameters(), temp)?.evaluate(transport::Bias {
                vbiei: 0.78,
                vbici: -0.45,
            })?;
        let state = nqs::State {
            current_first: 0.003,
            current_second: 0.002,
            stored_charge: 7e-15,
        };
        let mut inactive = original;
        inactive.forward_transit_time = 0.0;
        let output = kernel.evaluate(&nqs::Bias {
            transport: inactive,
            state,
        })?;
        assert!(!output.active);
        assert_eq!(output.delayed_current, original.forward_current);
        assert_eq!(output.delayed_charge, original.forward_diffusion_charge);
        assert_eq!(output.first_current.current.value, state.current_first);
        assert_eq!(output.second_current.current.value, state.current_second);
        assert_eq!(output.charge_current.current.value, state.stored_charge);
        assert_eq!(output.first_charge, nqs::DynamicCharge::default());
        assert_eq!(output.second_charge, nqs::DynamicCharge::default());
        assert_eq!(output.charge_filter_charge, nqs::DynamicCharge::default());
        assert_eq!(output.first_current.d_state.current_first, 1.0);
        assert_eq!(output.second_current.d_state.current_second, 1.0);
        assert_eq!(output.charge_current.d_state.stored_charge, 1.0);
        if !enabled {
            let disabled = kernel.evaluate(&nqs::Bias {
                transport: original,
                state,
            })?;
            assert_eq!(disabled.delayed_current, original.forward_current);
            assert_eq!(disabled.delayed_charge, original.forward_diffusion_charge);
        }
    }
    for (alit, alqf) in [(0.0, 0.0), (0.1, 0.0), (0.0, 0.1)] {
        let kernel = nqs::Prepared::new(nqs::Parameters {
            alit,
            alqf,
            ..params()
        })?;
        assert_eq!(kernel.is_active(3e-12), alit > 0.0 || alqf > 0.0);
    }
    let wide_source_mode = nqs::Prepared::new(nqs::Parameters {
        flcomp: 1e5,
        flnqs: true,
        ..params()
    })?;
    assert!(wide_source_mode.is_active(5e-12));
    Ok(())
}

#[test]
fn source_nqs_gyrator_dc_current_charge_and_full_state_jacobians() -> TestResult {
    let t = transport::Prepared::new(&baseline::parameters(), Temperature::new(300.15)?)?;
    let kernel = nqs::Prepared::new(params())?;
    for be in [0.6, 0.75, 0.85] {
        for bc in [-1.0, -0.3, 0.1] {
            let tr = t.evaluate(transport::Bias {
                vbiei: be,
                vbici: bc,
            })?;
            let state = nqs::State {
                current_first: tr.forward_current.value * 0.7,
                current_second: tr.forward_current.value * 1.2,
                stored_charge: tr.forward_diffusion_charge.value * 0.6,
            };
            let output = kernel.evaluate(&nqs::Bias {
                transport: tr,
                state,
            })?;
            assert!(output.active);
            let ratio = params().t0 / tr.forward_transit_time;
            close(
                output.first_current.current.value,
                (state.current_second - tr.forward_current.value) * ratio,
                1e-13,
                1e-20,
            );
            close(
                output.second_current.current.value,
                (state.current_second - state.current_first) * ratio,
                1e-13,
                1e-20,
            );
            close(
                output.charge_current.current.value,
                (state.stored_charge - tr.forward_diffusion_charge.value) * ratio,
                1e-13,
                1e-30,
            );
            close(
                output.first_charge.coulombs,
                params().alit * params().t0 * state.current_first,
                1e-13,
                1e-29,
            );
            close(
                output.second_charge.coulombs,
                params().alit * params().t0 / 3.0 * state.current_second,
                1e-13,
                1e-29,
            );
            close(
                output.charge_filter_charge.coulombs,
                params().alqf * params().t0 * state.stored_charge,
                1e-13,
                1e-29,
            );
            assert_eq!(output.delayed_current.value, state.current_second);
            assert_eq!(output.delayed_charge.value, state.stored_charge);
            assert_eq!(output.delayed_current_d_state.current_second, 1.0);
            assert_eq!(output.delayed_charge_d_state.stored_charge, 1.0);
            assert_eq!(output.first_current.d_state.current_second, ratio);
            assert_eq!(output.second_current.d_state.current_first, -ratio);
            assert_eq!(output.second_current.d_state.current_second, ratio);
            assert_eq!(output.charge_current.d_state.stored_charge, ratio);
            close(
                output.first_current.d_tf,
                -output.first_current.current.value / tr.forward_transit_time,
                1e-14,
                0.0,
            );
            close(
                output.second_current.d_tf,
                -output.second_current.current.value / tr.forward_transit_time,
                1e-14,
                0.0,
            );
            close(
                output.charge_current.d_tf,
                -output.charge_current.current.value / tr.forward_transit_time,
                1e-14,
                0.0,
            );
        }
    }
    Ok(())
}

fn verify_source_nqs_partials(
    kernel: &nqs::Prepared,
    trans: &transport::Prepared,
    bias: transport::Bias,
) -> TestResult {
    let t = trans.evaluate(bias)?;
    let state = nqs::State {
        current_first: t.forward_current.value * 0.7,
        current_second: t.forward_current.value * 1.1,
        stored_charge: t.forward_diffusion_charge.value * 0.8,
    };
    let out = kernel.evaluate(&nqs::Bias {
        transport: t,
        state,
    })?;
    let h = 1e-6;
    for (positive, negative, be_axis) in [
        (
            transport::Bias {
                vbiei: bias.vbiei + h,
                ..bias
            },
            transport::Bias {
                vbiei: bias.vbiei - h,
                ..bias
            },
            true,
        ),
        (
            transport::Bias {
                vbici: bias.vbici + h,
                ..bias
            },
            transport::Bias {
                vbici: bias.vbici - h,
                ..bias
            },
            false,
        ),
    ] {
        // Explicit partial with TF held fixed; dI/dTf is carried
        // separately and never silently assumed to vanish.
        let mut positive = trans.evaluate(positive)?;
        let mut negative = trans.evaluate(negative)?;
        positive.forward_transit_time = t.forward_transit_time;
        negative.forward_transit_time = t.forward_transit_time;
        let upper = kernel.evaluate(&nqs::Bias {
            transport: positive,
            state,
        })?;
        let lower = kernel.evaluate(&nqs::Bias {
            transport: negative,
            state,
        })?;
        let first = if be_axis {
            out.first_current.current.d_vbiei
        } else {
            out.first_current.current.d_vbici
        };
        let charge = if be_axis {
            out.charge_current.current.d_vbiei
        } else {
            out.charge_current.current.d_vbici
        };
        close(
            first,
            (upper.first_current.current.value - lower.first_current.current.value) / (2.0 * h),
            4e-4,
            1e-16,
        );
        close(
            charge,
            (upper.charge_current.current.value - lower.charge_current.current.value) / (2.0 * h),
            4e-4,
            1e-24,
        );
    }
    verify_source_nqs_tf_axis(kernel, &t, state, &out)?;
    Ok(())
}

fn verify_source_nqs_tf_axis(
    kernel: &nqs::Prepared,
    t: &transport::Output,
    state: nqs::State,
    out: &nqs::Output,
) -> TestResult {
    let delta_tf = t.forward_transit_time * 1e-4;
    let mut plus = *t;
    let mut minus = *t;
    plus.forward_transit_time += delta_tf;
    minus.forward_transit_time -= delta_tf;
    let upper = kernel.evaluate(&nqs::Bias {
        transport: plus,
        state,
    })?;
    let lower = kernel.evaluate(&nqs::Bias {
        transport: minus,
        state,
    })?;
    for (analytic, positive, negative) in [
        (
            out.first_current.d_tf,
            upper.first_current.current.value,
            lower.first_current.current.value,
        ),
        (
            out.second_current.d_tf,
            upper.second_current.current.value,
            lower.second_current.current.value,
        ),
        (
            out.charge_current.d_tf,
            upper.charge_current.current.value,
            lower.charge_current.current.value,
        ),
    ] {
        close(
            analytic,
            (positive - negative) / (2.0 * delta_tf),
            2e-7,
            1e-10,
        );
    }
    Ok(())
}

#[test]
fn nqs_source_partials_include_tf_and_all_state_derivatives() -> TestResult {
    let tr = transport::Prepared::new(&baseline::parameters(), Temperature::new(300.15)?)?;
    let kernel = nqs::Prepared::new(params())?;
    for emitter in [0.62, 0.76, 0.84] {
        for collector in [-0.8, -0.25, 0.2] {
            let bias = transport::Bias {
                vbiei: emitter,
                vbici: collector,
            };
            verify_source_nqs_partials(&kernel, &tr, bias)?;
            let t = tr.evaluate(bias)?;
            let state = nqs::State {
                current_first: t.forward_current.value * 0.6,
                current_second: t.forward_current.value * 1.1,
                stored_charge: t.forward_diffusion_charge.value * 0.9,
            };
            let output = kernel.evaluate(&nqs::Bias {
                transport: t,
                state,
            })?;
            let h_current = t.forward_current.value.abs().mul_add(1e-5, 1e-11);
            let upper_state = nqs::State {
                current_second: state.current_second + h_current,
                ..state
            };
            let lower_state = nqs::State {
                current_second: state.current_second - h_current,
                ..state
            };
            let upper = kernel.evaluate(&nqs::Bias {
                transport: t,
                state: upper_state,
            })?;
            let lower = kernel.evaluate(&nqs::Bias {
                transport: t,
                state: lower_state,
            })?;
            close(
                output.first_current.d_state.current_second,
                (upper.first_current.current.value - lower.first_current.current.value)
                    / (2.0 * h_current),
                1e-6,
                0.0,
            );
            close(
                output.second_current.d_state.current_second,
                (upper.second_current.current.value - lower.second_current.current.value)
                    / (2.0 * h_current),
                1e-6,
                0.0,
            );
            close(
                output.second_charge.d_state.current_second,
                (upper.second_charge.coulombs - lower.second_charge.coulombs) / (2.0 * h_current),
                1e-6,
                0.0,
            );
        }
    }
    Ok(())
}

#[test]
fn source_ac_transfer_solves_full_complex_gyrator_kcl() -> TestResult {
    for flcomp in [0.0, 2.1, 2.2, 2.3] {
        for flnqs in [false, true] {
            let p = nqs::Parameters {
                flcomp,
                flnqs,
                ..params()
            };
            let kernel = nqs::Prepared::new(p)?;
            for tf in [0.0, 0.3e-12, 2.5e-12, 25e-12] {
                for hz in [0.0, 1e6, 1e9, 1e11] {
                    let response = kernel.frequency_response(tf, hz)?;
                    if !kernel.is_active(tf) {
                        assert_eq!(response.current.real, 1.0);
                        assert_eq!(response.current.imaginary, 0.0);
                        assert_eq!(response.charge.real, 1.0);
                        assert_eq!(response.charge.imaginary, 0.0);
                        continue;
                    }
                    let omega = 2.0 * std::f64::consts::PI * hz;
                    let a = omega * tf * p.alit;
                    let b = omega * tf * p.alqf;
                    // Exact source complex KCL:
                    // xf1=(1+j*a/3)*xf2, xf2+j*a*xf1=Itf,
                    // xf+j*b*xf=Qdei. Test equations, not only formula.
                    let x2 = response.current;
                    let x1_real = (a / 3.0).mul_add(-x2.imaginary, x2.real);
                    let x1_imag = (a / 3.0).mul_add(x2.real, x2.imaginary);
                    close(a.mul_add(-x1_imag, x2.real), 1.0, 1e-10, 1e-11);
                    close(a.mul_add(x1_real, x2.imaginary), 0.0, 0.0, 1e-11);
                    let xq = response.charge;
                    close(b.mul_add(-xq.imaginary, xq.real), 1.0, 1e-10, 1e-11);
                    close(b.mul_add(xq.real, xq.imaginary), 0.0, 0.0, 1e-11);
                }
            }
        }
    }
    Ok(())
}

#[test]
fn nqs_prepared_validates_underdetermined_networks_and_batch_buffers() -> TestResult {
    for (name, value) in [
        ("flcomp", -0.01),
        ("flcomp", f64::NAN),
        ("alit", -0.01),
        ("alit", 1.01),
        ("alqf", -0.01),
        ("alqf", 1.01),
        ("t0", -1e-12),
    ] {
        let mut p = params();
        match name {
            "flcomp" => p.flcomp = value,
            "alit" => p.alit = value,
            "alqf" => p.alqf = value,
            "t0" => p.t0 = value,
            _ => return Err("unexpected NQS parameter".into()),
        }
        assert!(matches!(nqs::Prepared::new(p),
            Err(ModelError::InvalidParameter(key)) if key==name));
    }
    let original = transport::Prepared::new(&baseline::parameters(), Temperature::new(300.15)?)?
        .evaluate(transport::Bias {
            vbiei: 0.79,
            vbici: -0.45,
        })?;
    let state = nqs::State {
        current_first: 0.02,
        current_second: 0.01,
        stored_charge: 1e-14,
    };
    let kernel = nqs::Prepared::new(params())?;
    let input = nqs::Bias {
        transport: original,
        state,
    };
    let mut output = [nqs::Output::default(); 3];
    kernel.evaluate_batch(&[input; 3], &mut output)?;
    assert!(output.iter().all(|o| *o == output[0]));
    assert!(matches!(
        kernel.evaluate_batch(&[input; 2], &mut output),
        Err(EvaluationError::LengthMismatch)
    ));
    let mut nonfinite = input;
    nonfinite.state.current_first = f64::NAN;
    assert!(matches!(
        kernel.evaluate(&nonfinite),
        Err(EvaluationError::NonFiniteBias)
    ));
    nonfinite = input;
    nonfinite.transport.forward_transit_time = f64::NAN;
    assert!(matches!(
        kernel.evaluate(&nonfinite),
        Err(EvaluationError::NonFiniteBias)
    ));
    let singular = nqs::Prepared::new(nqs::Parameters {
        t0: 0.0,
        ..params()
    })?;
    assert!(singular.is_active(original.forward_transit_time));
    assert!(matches!(
        singular.evaluate(&input),
        Err(EvaluationError::InvalidPhysicalDomain)
    ));
    assert!(matches!(
        singular.frequency_response(original.forward_transit_time, 1e9),
        Err(EvaluationError::InvalidPhysicalDomain)
    ));
    for hz in [-1.0, -1e3] {
        assert!(matches!(
            kernel.frequency_response(original.forward_transit_time, hz),
            Err(EvaluationError::InvalidPhysicalDomain)
        ));
    }
    for tf in [f64::NAN, f64::INFINITY] {
        assert!(matches!(
            kernel.frequency_response(tf, 1e9),
            Err(EvaluationError::NonFiniteBias)
        ));
    }
    assert!(matches!(
        kernel.frequency_response(1e-12, f64::NAN),
        Err(EvaluationError::NonFiniteBias)
    ));
    assert!(matches!(
        kernel.frequency_response(1e-12, 1e308),
        Err(EvaluationError::NonFiniteResult)
    ));
    let mut overflowing = input;
    overflowing.transport.forward_transit_time = f64::MIN_POSITIVE;
    assert!(matches!(
        kernel.evaluate(&overflowing),
        Err(EvaluationError::NonFiniteResult)
    ));
    Ok(())
}

#[test]
fn native_ngspice47_hicum_vertical_nqs_complex_ac_ratio_oracle() -> TestResult {
    let csv = include_str!("fixtures/hicum2_ngspice47_nqs_ac.csv");
    let mut count = 0;
    let mut matching = 0;
    let mut source_version_divergences = 0;
    let mut worst_matching = (0.0, String::new());
    for row in csv.lines().filter(|line| !line.starts_with('#')) {
        let [
            variant,
            flcomp,
            flnqs,
            alit,
            alqf,
            hz,
            tf,
            off_re,
            off_im,
            on_re,
            on_im,
        ] = <[&str; 11]>::try_from(row.split(',').collect::<Vec<_>>())
            .map_err(|_| "bad native HICUM NQS AC fixture row")?;
        let kernel = nqs::Prepared::new(nqs::Parameters {
            flnqs: flnqs == "1",
            flcomp: flcomp.parse()?,
            alit: alit.parse()?,
            alqf: alqf.parse()?,
            t0: 5e-12,
        })?;
        let frequency: f64 = hz.parse()?;
        let native_tf: f64 = tf.parse()?;
        let expected = kernel.frequency_response(native_tf, frequency)?;
        let reference_real: f64 = off_re.parse()?;
        let reference_imaginary: f64 = off_im.parse()?;
        let observed_real: f64 = on_re.parse()?;
        let observed_imaginary: f64 = on_im.parse()?;
        let denominator =
            reference_real.mul_add(reference_real, reference_imaginary * reference_imaginary);
        assert!(denominator > 0.0);
        let actual_real = reference_real
            .mul_add(observed_real, reference_imaginary * observed_imaginary)
            / denominator;
        let actual_imaginary = reference_real
            .mul_add(observed_imaginary, -reference_imaginary * observed_real)
            / denominator;
        let relative = (actual_real - expected.current.real)
            .hypot(actual_imaginary - expected.current.imaginary)
            / expected
                .current
                .real
                .hypot(expected.current.imaginary)
                .max(1e-16);
        if variant == "intermediate_disabled" {
            // Installed ngspice 47 treats FLcomp=2.2 as legacy and
            // activates vertical NQS even with FLNQS=0. Published
            // 2.4.0 VA restricts this legacy exception to 0 and 2.1.
            // Preserve the source law and keep this difference visible.
            assert!(
                relative > 1e-5,
                "native FLcomp2.2 NQS discrepancy disappeared at f={frequency}"
            );
            source_version_divergences += 1;
        } else {
            if relative > worst_matching.0 {
                worst_matching = (relative, format!("{variant}: {frequency} Hz"));
            }
            assert!(
                relative < 3e-8,
                "native HICUM NQS source difference {variant} f={frequency}: {relative:.6e}"
            );
            matching += 1;
        }
        count += 1;
    }
    assert_eq!(count, 176);
    assert_eq!(matching, 160);
    assert_eq!(source_version_divergences, 16);
    assert!(
        worst_matching.0 < 3e-8,
        "worst conforming NQS AC reference case: {worst_matching:?}"
    );
    Ok(())
}

#[test]
fn active_vertical_nqs_with_zero_t0_is_an_explicit_singular_network_error() -> TestResult {
    let parameters = nqs::Parameters {
        t0: 0.0,
        flcomp: 2.3,
        flnqs: true,
        alit: 0.333,
        alqf: 0.167,
    };
    let kernel = nqs::Prepared::new(parameters)?;
    let mut transport = transport::Output {
        forward_transit_time: 3e-12,
        forward_current: transport::Value2 {
            value: 2e-5,
            ..transport::Value2::default()
        },
        forward_diffusion_charge: transport::Value2 {
            value: 6e-17,
            ..transport::Value2::default()
        },
        ..transport::Output::default()
    };
    assert!(kernel.is_active(transport.forward_transit_time));
    assert!(matches!(
        kernel.evaluate(&nqs::Bias {
            transport,
            state: nqs::State::default()
        }),
        Err(model_kernel_bipolar::EvaluationError::InvalidPhysicalDomain)
    ));
    assert!(matches!(
        kernel.frequency_response(transport.forward_transit_time, 1e9),
        Err(model_kernel_bipolar::EvaluationError::InvalidPhysicalDomain)
    ));
    transport.forward_transit_time = 0.0;
    assert!(!kernel.is_active(transport.forward_transit_time));
    assert!(
        !kernel
            .evaluate(&nqs::Bias {
                transport,
                state: nqs::State::default()
            })?
            .active
    );
    Ok(())
}
