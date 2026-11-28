use model_kernel_bipolar::{
    EvaluationError, ModelError, Temperature,
    hicum2::{junction, tunneling},
};
#[path = "fixtures/hicum2_baseline.rs"]
mod baseline;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn close(actual: f64, expected: f64, relative: f64, absolute: f64) {
    let tolerance = relative.mul_add(actual.abs().max(expected.abs()), absolute);
    assert!(
        (actual - expected).abs() <= tolerance,
        "actual={actual:.15e},expected={expected:.15e},tolerance={tolerance:.6e}"
    );
}

fn physical(
    tunnel: &tunneling::Prepared,
    junction_kernel: &junction::Prepared,
    emitter_voltage: f64,
    perimeter_voltage: f64,
) -> Result<tunneling::Output, EvaluationError> {
    let output = junction_kernel.evaluate(junction::Bias {
        vbiei: emitter_voltage,
        vbpei: perimeter_voltage,
        vbici: -0.8,
        vbci: -0.8,
        vbpci: -0.8,
        vsici: -1.0,
        vsc: -1.0,
    })?;
    tunnel.evaluate(tunneling::Bias {
        vbiei: emitter_voltage,
        vbpei: perimeter_voltage,
        intrinsic: output.intrinsic_emitter,
        peripheral: output.peripheral_emitter,
    })
}

#[test]
fn hicum_hictun_intrinsic_and_peripheral_source_law_and_jacobian() -> TestResult {
    let p = baseline::parameters();
    for kelvin in [233.15, 273.15, 300.15, 398.15] {
        let j = junction::Prepared::new(p.junction, Temperature::new(kelvin)?)?;
        for location in [
            tunneling::Location::Intrinsic,
            tunneling::Location::Peripheral,
        ] {
            for attenuation in [0.0, 1.5, 8.0, 40.0] {
                let saturation = 2.0e-8;
                let t = tunneling::Prepared::new(tunneling::Parameters {
                    saturation,
                    attenuation,
                    location,
                    intrinsic: p.junction.intrinsic_emitter,
                    peripheral: p.junction.peripheral_emitter,
                })?;
                for emitter in [-1.2, -0.7, -0.15, 0.2] {
                    for perimeter in [-1.1, -0.5, -0.07, 0.3] {
                        let output = physical(&t, &j, emitter, perimeter)?;
                        let active = emitter < 0.0 || perimeter < 0.0;
                        let (voltage, junction_param, capacitance) = match location {
                            tunneling::Location::Intrinsic => (
                                emitter,
                                p.junction.intrinsic_emitter,
                                j.evaluate(junction::Bias {
                                    vbiei: emitter,
                                    vbpei: perimeter,
                                    vbici: -0.8,
                                    vbci: -0.8,
                                    vbpci: -0.8,
                                    vsici: -1.0,
                                    vsc: -1.0,
                                })?
                                .intrinsic_emitter,
                            ),
                            tunneling::Location::Peripheral => (
                                perimeter,
                                p.junction.peripheral_emitter,
                                j.evaluate(junction::Bias {
                                    vbiei: emitter,
                                    vbpei: perimeter,
                                    vbici: -0.8,
                                    vbci: -0.8,
                                    vbpci: -0.8,
                                    vsici: -1.0,
                                    vsc: -1.0,
                                })?
                                .peripheral_emitter,
                            ),
                        };
                        let ratio =
                            capacitance.source_capacitance / junction_param.zero_bias_capacitance;
                        let field = ratio.powf(1.0 - 1.0 / junction_param.grading);
                        let expected = if active {
                            -voltage / junction_param.potential
                                * saturation
                                * field
                                * (-attenuation / field).exp()
                        } else {
                            0.0
                        };
                        close(output.amperes, expected, 1e-13, 1e-24);
                        let step = 2e-6;
                        if active {
                            let emitter_plus = physical(&t, &j, emitter + step, perimeter)?;
                            let emitter_minus = physical(&t, &j, emitter - step, perimeter)?;
                            let perimeter_plus = physical(&t, &j, emitter, perimeter + step)?;
                            let perimeter_minus = physical(&t, &j, emitter, perimeter - step)?;
                            close(
                                output.d_vbiei,
                                (emitter_plus.amperes - emitter_minus.amperes) / (2.0 * step),
                                4e-5,
                                2e-15,
                            );
                            close(
                                output.d_vbpei,
                                (perimeter_plus.amperes - perimeter_minus.amperes) / (2.0 * step),
                                4e-5,
                                2e-15,
                            );
                        } else {
                            assert_eq!(output, tunneling::Output::default());
                        }
                        if location == tunneling::Location::Intrinsic {
                            assert_eq!(output.d_vbpei, 0.0);
                        } else {
                            assert_eq!(output.d_vbiei, 0.0);
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

#[test]
fn hicum_hictun_outer_gate_is_reverse_on_either_junction() -> TestResult {
    let p = baseline::parameters();
    let j = junction::Prepared::new(p.junction, Temperature::new(300.15)?)?;
    let intrinsic = tunneling::Prepared::new(tunneling::Parameters {
        saturation: 1e-7,
        attenuation: 1.0,
        location: tunneling::Location::Intrinsic,
        intrinsic: p.junction.intrinsic_emitter,
        peripheral: p.junction.peripheral_emitter,
    })?;
    let peripheral = tunneling::Prepared::new(tunneling::Parameters {
        saturation: 1e-7,
        attenuation: 1.0,
        location: tunneling::Location::Peripheral,
        intrinsic: p.junction.intrinsic_emitter,
        peripheral: p.junction.peripheral_emitter,
    })?;
    assert_eq!(
        physical(&intrinsic, &j, 0.2, 0.5)?,
        tunneling::Output::default()
    );
    assert_eq!(
        physical(&peripheral, &j, 0.2, 0.5)?,
        tunneling::Output::default()
    );
    // The source gate is an OR over *both* junctions, even though only
    // the selected branch voltage enters czz. A positive selected branch
    // can thus contribute a negative source tunneling current.
    assert!(physical(&intrinsic, &j, 0.2, -0.5)?.amperes < 0.0);
    assert!(physical(&peripheral, &j, -0.5, 0.2)?.amperes < 0.0);
    assert!(physical(&intrinsic, &j, -0.5, 0.2)?.amperes > 0.0);
    assert!(physical(&peripheral, &j, 0.2, -0.5)?.amperes > 0.0);
    Ok(())
}

#[test]
fn hicum_hictun_parameter_disabled_junction_and_buffer_contracts() -> TestResult {
    let p = baseline::parameters();
    let j = junction::Prepared::new(p.junction, Temperature::new(300.15)?)?;
    let make = |saturation, capacitance| {
        let mut emitter = p.junction.intrinsic_emitter;
        emitter.zero_bias_capacitance = capacitance;
        tunneling::Prepared::new(tunneling::Parameters {
            saturation,
            attenuation: 5.0,
            location: tunneling::Location::Intrinsic,
            intrinsic: emitter,
            peripheral: p.junction.peripheral_emitter,
        })
    };
    let off = make(0.0, 2e-15)?;
    assert_eq!(
        physical(&off, &j, -0.5, -0.5)?,
        tunneling::Output::default()
    );
    let capoff = make(1e-8, 0.0)?;
    assert_eq!(
        physical(&capoff, &j, -0.5, -0.5)?,
        tunneling::Output::default()
    );
    assert!(matches!(
        make(-0.1, 2e-15),
        Err(ModelError::InvalidParameter("ibets"))
    ));

    let active = make(1e-8, 2e-15)?;
    let stored = j.evaluate(junction::Bias {
        vbiei: -0.4,
        vbpei: -0.7,
        vbici: -0.8,
        vbci: -0.8,
        vbpci: -0.8,
        vsici: -1.0,
        vsc: -1.0,
    })?;
    let input = tunneling::Bias {
        vbiei: -0.4,
        vbpei: -0.7,
        intrinsic: stored.intrinsic_emitter,
        peripheral: stored.peripheral_emitter,
    };
    let mut outputs = [tunneling::Output::default(); 2];
    active.evaluate_batch(&[input, input], &mut outputs)?;
    assert_eq!(outputs[0], outputs[1]);
    assert!(matches!(
        active.evaluate_batch(&[input], &mut outputs),
        Err(EvaluationError::LengthMismatch)
    ));
    let mut invalid = input;
    invalid.peripheral.source_capacitance = f64::NAN;
    assert!(matches!(
        active.evaluate(invalid),
        Err(EvaluationError::NonFiniteBias)
    ));
    invalid = input;
    invalid.intrinsic.source_capacitance = 0.0;
    assert!(matches!(
        active.evaluate(invalid),
        Err(EvaluationError::InvalidPhysicalDomain)
    ));
    Ok(())
}

const fn thermal_model(
    mode: f64,
    location: tunneling::Location,
    saturation: f64,
    attenuation: f64,
    zero_selected_capacitance: bool,
) -> model_kernel_bipolar::hicum2::temperature::Nominal {
    use model_kernel_bipolar::hicum2::temperature;
    let mut nominal = temperature::Nominal::source_defaults();
    nominal.transport.flcomp = mode;
    nominal.auxiliary.tunode = (!matches!(location, tunneling::Location::Intrinsic)) as u8;
    nominal.auxiliary.ibets = saturation;
    nominal.auxiliary.abet = attenuation;
    nominal.coefficients.vge = 1.08;
    nominal.coefficients.f1vg = -0.00009;
    nominal.coefficients.f2vg = 0.00032;
    nominal
        .transport
        .junction
        .intrinsic_emitter
        .zero_bias_capacitance = if zero_selected_capacitance && nominal.auxiliary.tunode == 0 {
        0.0
    } else {
        2e-15
    };
    nominal
        .transport
        .junction
        .peripheral_emitter
        .zero_bias_capacitance = if zero_selected_capacitance && nominal.auxiliary.tunode == 1 {
        0.0
    } else {
        1e-15
    };
    nominal
}

fn tunneling_at(
    prepared: &model_kernel_bipolar::hicum2::temperature::Prepared,
    emitter: f64,
    peripheral: f64,
) -> Result<(tunneling::Output, f64), Box<dyn std::error::Error>> {
    let bias = junction::Bias {
        vbiei: emitter,
        vbpei: peripheral,
        vbici: -0.5,
        vbci: -0.5,
        vbpci: -0.5,
        vsici: -0.4,
        vsc: -0.4,
    };
    let source = prepared.junction_kernel()?;
    let p = source.evaluate(bias)?;
    let tunneling_bias = tunneling::Bias {
        vbiei: emitter,
        vbpei: peripheral,
        intrinsic: p.intrinsic_emitter,
        peripheral: p.peripheral_emitter,
    };
    let model = prepared.tunneling_kernel()?;
    let op = model.evaluate(tunneling_bias)?;
    let slope = model.temperature_sensitivity(
        tunneling_bias,
        op,
        prepared.junction_parameter_slopes,
        source.emitter_temperature_sensitivity(bias)?,
    )?;
    Ok((op, slope))
}

#[test]
fn exact_hictun_thermal_current_slope_matches_independent_junction_model_repreparation()
-> TestResult {
    let mut checked = 0;
    let mut nonzero = 0;
    for mode in [0.0, 2.3] {
        for location in [
            tunneling::Location::Intrinsic,
            tunneling::Location::Peripheral,
        ] {
            for saturation in [0.0, 2e-8] {
                for attenuation in [0.0, 2.5] {
                    for cap_zero in [false, true] {
                        let nominal =
                            thermal_model(mode, location, saturation, attenuation, cap_zero);
                        for kelvin in [233.15, 300.15, 373.15, 523.15] {
                            let step = 0.003;
                            let middle = nominal.prepare(Temperature::new(kelvin)?, 0.0)?;
                            let lower = nominal.prepare(Temperature::new(kelvin - step)?, 0.0)?;
                            let upper = nominal.prepare(Temperature::new(kelvin + step)?, 0.0)?;
                            for (emitter, peripheral) in [
                                (-1.5, -0.8),
                                (-0.8, -0.3),
                                (-0.25, -0.15),
                                (-0.05, 0.25),
                                (0.15, -0.4),
                                (0.25, 0.12),
                                (0.65, -0.08),
                                (-0.3, 0.9),
                            ] {
                                let (_, exact) = tunneling_at(&middle, emitter, peripheral)?;
                                let (low, _) = tunneling_at(&lower, emitter, peripheral)?;
                                let (high, _) = tunneling_at(&upper, emitter, peripheral)?;
                                let estimated = (high.amperes - low.amperes) / (2.0 * step);
                                close(exact, estimated, 2e-5, 2e-20);
                                nonzero += usize::from(exact.abs() > 1e-13);
                                if saturation == 0.0
                                    || cap_zero
                                    || (emitter >= 0.0 && peripheral >= 0.0)
                                {
                                    assert_eq!(exact, 0.0);
                                }
                                checked += 1;
                            }
                        }
                    }
                }
            }
        }
    }
    assert_eq!(checked, 1024);
    assert!(nonzero > 40);
    Ok(())
}

#[test]
fn source_device_tunneling_thermal_derivative_matches_standalone_for_both_polarities() -> TestResult
{
    use model_kernel_bipolar::hicum2::{Polarity, device, nqs};
    for polarity in [Polarity::Npn, Polarity::Pnp] {
        for location in [
            tunneling::Location::Intrinsic,
            tunneling::Location::Peripheral,
        ] {
            let mut nominal = thermal_model(2.3, location, 4e-8, 1.5, false);
            nominal.polarity = polarity;
            let prepared = nominal.prepare(Temperature::new(300.15)?, 0.0)?;
            let kernel = prepared.device_kernel()?;
            let sign = if matches!(polarity, Polarity::Npn) {
                1.0
            } else {
                -1.0
            };
            // The tunneling source enables when either B-E junction
            // is reverse biased, including a forward selected branch.
            for (emitter, perimeter) in [(-0.45, -0.3), (-0.25, 0.05), (0.1, -0.4)] {
                let bias = device::Bias {
                    nodes: device::Nodes {
                        b: sign * emitter,
                        bp: sign * perimeter,
                        bi: sign * emitter,
                        c: sign * 0.2,
                        ci: sign * 0.2,
                        e: 0.0,
                        ei: 0.0,
                        s: 0.0,
                        si: 0.0,
                    },
                    nqs: nqs::State::default(),
                };
                let output = kernel.evaluate(&bias)?;
                let (standalone, direct) =
                    tunneling_at(&prepared, output.voltage.vbiei, output.voltage.vbpei)?;
                close(output.tunneling.amperes, standalone.amperes, 1e-13, 1e-25);
                close(
                    kernel.tunneling_temperature_sensitivity(&output)?,
                    direct,
                    1e-13,
                    1e-25,
                );
            }
        }
    }
    Ok(())
}
