use model_kernel_bipolar::{
    EvaluationError, Temperature,
    hicum2::{self, device, nqs, temperature},
};
#[path = "fixtures/hicum2_baseline.rs"]
mod baseline;

type TestResult = Result<(), Box<dyn std::error::Error>>;
fn close(actual: f64, expected: f64, relative: f64, absolute: f64) {
    let limit = relative.mul_add(actual.abs().max(expected.abs()), absolute);
    assert!(
        (actual - expected).abs() <= limit,
        "actual={actual:.15e},expected={expected:.15e},limit={limit:.4e}"
    );
}

const fn terminal_nodes(be: f64, vce: f64) -> device::Nodes {
    device::Nodes {
        b: be,
        bp: be,
        bi: be,
        c: vce,
        ci: vce,
        ei: 0.0,
        e: 0.0,
        si: 0.0,
        s: 0.0,
    }
}

const fn mixed_model() -> temperature::Nominal {
    let mut nominal = temperature::Nominal::source_defaults();
    nominal.transport = baseline::parameters();
    nominal.transport.flcomp = 2.3;
    nominal.diodes.ibeps = 4e-19;
    nominal.diodes.ireis = 4e-18;
    nominal.diodes.ibcxs = 1e-18;
    nominal.diodes.iscs = 2e-18;
    nominal.auxiliary.rbi0 = 18.0;
    nominal.auxiliary.rbx = 12.0;
    nominal.auxiliary.rcx = 5.0;
    nominal.auxiliary.re = 3.0;
    nominal.auxiliary.rsu = 30.0;
    nominal.auxiliary.csu = 2e-15;
    nominal.auxiliary.cbepar = 4e-15;
    nominal.auxiliary.cbcpar = 5e-15;
    nominal.auxiliary.fbcpar = 0.4;
    nominal.auxiliary.fbepar = 0.7;
    nominal.auxiliary.fcrbi = 0.3;
    nominal.auxiliary.favl = 0.1;
    nominal.auxiliary.qavl = 2e-15;
    nominal.auxiliary.ibets = 1e-4;
    nominal.auxiliary.abet = 0.8;
    nominal.auxiliary.itss = 2e-17;
    nominal.auxiliary.tsf = 1e-12;
    nominal.auxiliary.flsh = 2;
    nominal.auxiliary.rth = 250.0;
    nominal.auxiliary.cth = 1e-9;
    nominal.auxiliary.flnqs = true;
    nominal.noise.kf = 1e-19;
    nominal.noise.kfre = 3e-19;
    nominal.noise.correlated = true;
    nominal
}

const fn mixed_nodes() -> device::Nodes {
    device::Nodes {
        b: 0.78,
        bp: 0.76,
        bi: 0.75,
        c: 1.22,
        ci: 1.12,
        ei: 0.03,
        e: 0.0,
        si: -0.07,
        s: -0.10,
    }
}

#[test]
fn composite_default_model_matches_subkernels_and_native_oracle() -> TestResult {
    let fixture = include_str!("fixtures/hicum2_ngspice47_source_defaults.csv");
    let nominal = temperature::Nominal::source_defaults();
    let mut n = 0;
    for row in fixture.lines().filter(|line| !line.starts_with('#')) {
        let [ambient, be, vce, _, _, ic, ibei, _ibci, ick, tf] =
            <[&str; 10]>::try_from(row.split(',').collect::<Vec<_>>())
                .map_err(|_| "malformed HICUM default composite oracle")?;
        let temp = Temperature::new(ambient.parse::<f64>()? + 273.15)?;
        let prepared = nominal.prepare(temp, 0.0)?;
        let evaluator = prepared.device_kernel()?;
        let be: f64 = be.parse()?;
        let vce: f64 = vce.parse()?;
        let input = device::Bias {
            nodes: terminal_nodes(be, vce),
            nqs: nqs::State::default(),
        };
        let result = evaluator.evaluate(&input)?;
        close(result.voltage.vbiei, be, 0.0, 0.0);
        close(result.voltage.vbici, be - vce, 0.0, 0.0);
        assert!(result.internal_base.shorted);
        assert!(result.external.external_base.shorted);
        assert!(result.external.external_collector.shorted);
        assert!(result.external.emitter.shorted);
        assert!(result.external.substrate.shorted);
        assert!(result.thermal.shorted);
        assert!(!result.nqs.active);
        close(result.diode.ibei.amperes, ibei.parse()?, 1e-4, 1e-22);
        close(
            result.transfer.critical_current.value,
            ick.parse()?,
            1e-4,
            0.0,
        );
        close(result.transfer.forward_transit_time, tf.parse()?, 0.0, 0.0);
        let terminal_collector = 1e-12f64.mul_add(
            3.0f64.mul_add(vce, -2.0 * be),
            result.transfer.net_transfer_current.value - result.diode.ibci.amperes,
        );
        close(terminal_collector, ic.parse()?, 1e-4, 1e-20);
        let noise = evaluator.evaluate_noise(&result, 1e7)?;
        assert_eq!(noise.flicker.be, 0.0);
        assert_eq!(noise.thermal.rbx, 0.0);
        assert_eq!(noise.thermal.re, 0.0);
        n += 1;
    }
    assert_eq!(n, 36);
    Ok(())
}

fn assert_mixed_noise(
    evaluator: &device::Prepared,
    prepared: &temperature::Prepared,
    output: &device::Output,
) -> TestResult {
    let spectra = evaluator.evaluate_noise(output, 1e10)?;
    let four_kt = 4.0 * 1.380_650_3e-23 * prepared.effective_temperature.kelvin();
    close(
        spectra.thermal.rbx,
        four_kt * output.external.external_base.siemens,
        1e-14,
        0.0,
    );
    close(
        spectra.thermal.rbi,
        four_kt * output.internal_base.branch_conductance,
        1e-14,
        0.0,
    );
    close(
        spectra.thermal.re,
        four_kt * output.external.emitter.siemens,
        1e-14,
        0.0,
    );
    assert!(spectra.be_ce_shot.correlated);
    assert!(spectra.be_ce_shot.cross_imaginary > 0.0);
    Ok(())
}

#[test]
fn composite_mixed_hicum_all_physical_families_share_nodal_bias_and_thermal_state() -> TestResult {
    let nominal = mixed_model();
    for kelvin in [245.0, 300.15, 375.0] {
        let prepared = nominal.prepare(Temperature::new(kelvin)?, 2.0)?;
        let evaluator = prepared.device_kernel()?;
        let bias = device::Bias {
            nodes: mixed_nodes(),
            nqs: nqs::State {
                current_first: 0.003,
                current_second: 0.005,
                stored_charge: 3e-15,
            },
        };
        let output = evaluator.evaluate(&bias)?;
        let v = output.voltage;
        close(v.vbiei, 0.72, 1e-15, 0.0);
        close(v.vbici, -0.37, 1e-15, 0.0);
        close(v.vbpci, -0.36, 1e-15, 0.0);
        close(v.vbpbi_raw, 0.01, 1e-15, 0.0);
        assert!(!output.internal_base.shorted);
        assert!(!output.external.external_base.shorted);
        assert!(!output.external.emitter.shorted);
        assert!(!output.external.substrate.shorted);
        assert!(!output.thermal.shorted);
        assert!(output.nqs.active);
        close(
            output.nqs.delayed_current.value,
            bias.nqs.current_second,
            0.0,
            0.0,
        );
        close(
            output.nqs.delayed_charge.value,
            bias.nqs.stored_charge,
            0.0,
            0.0,
        );
        let native_current = prepared.diode_kernel()?.evaluate(hicum2::Bias {
            vbiei: v.vbiei,
            vbpei: v.vbpei,
            vbici: v.vbici,
            vbpci: v.vbpci,
            vsici: v.vsici,
        })?;
        close(
            output.diode.ibei.amperes,
            native_current.ibei.amperes,
            0.0,
            0.0,
        );
        close(
            output.diode.ibci.amperes,
            native_current.ibci.amperes,
            0.0,
            0.0,
        );
        let native_transport = prepared
            .transport_kernel()?
            .evaluate(hicum2::transport::Bias {
                vbiei: v.vbiei,
                vbici: v.vbici,
            })?;
        close(
            output.transfer.net_transfer_current.value,
            native_transport.net_transfer_current.value,
            0.0,
            0.0,
        );
        close(
            output.thermal.electrical.transport.watts,
            v.vciei * output.transfer.net_transfer_current.value,
            2e-15,
            0.0,
        );
        close(
            output.thermal.electrical.intrinsic_emitter.watts,
            v.vbiei * output.diode.ibei.amperes,
            2e-15,
            0.0,
        );
        close(
            output.thermal.electrical.intrinsic_base.watts,
            v.vbpbi_raw.powi(2) / output.internal_base.resistance.value,
            2e-15,
            0.0,
        );
        assert_mixed_noise(&evaluator, &prepared, &output)?;
    }
    Ok(())
}

#[test]
fn device_polarity_maps_all_physical_voltages_with_raw_resistor_signs() -> TestResult {
    let mut nominal = mixed_model();
    let temperature = Temperature::new(300.15)?;
    let npn = nominal.prepare(temperature, 0.0)?.device_kernel()?;
    let bias = device::Bias {
        nodes: mixed_nodes(),
        nqs: nqs::State::default(),
    };
    let out = npn.evaluate(&bias)?;
    nominal.polarity = hicum2::Polarity::Pnp;
    let pnp = nominal.prepare(temperature, 0.0)?.device_kernel()?;
    let nodes = bias.nodes;
    let mirrored = device::Nodes {
        b: -nodes.b,
        bp: -nodes.bp,
        bi: -nodes.bi,
        c: -nodes.c,
        ci: -nodes.ci,
        ei: -nodes.ei,
        e: -nodes.e,
        si: -nodes.si,
        s: -nodes.s,
    };
    let pnp_output = pnp.evaluate(&device::Bias {
        nodes: mirrored,
        nqs: bias.nqs,
    })?;
    let mut expected_voltages = out.voltage;
    expected_voltages.vbpbi_raw = -expected_voltages.vbpbi_raw;
    assert_eq!(expected_voltages, pnp_output.voltage);
    close(
        out.transfer.net_transfer_current.value,
        pnp_output.transfer.net_transfer_current.value,
        1e-14,
        0.0,
    );
    close(
        out.diode.ibei.amperes,
        pnp_output.diode.ibei.amperes,
        1e-14,
        0.0,
    );
    close(
        out.junction.intrinsic_emitter.coulombs,
        pnp_output.junction.intrinsic_emitter.coulombs,
        1e-14,
        0.0,
    );
    close(
        out.external.external_base.amperes,
        -pnp_output.external.external_base.amperes,
        1e-14,
        0.0,
    );
    close(
        out.external.emitter.amperes,
        -pnp_output.external.emitter.amperes,
        1e-14,
        0.0,
    );
    close(
        out.external.external_emitter_isolation.coulombs,
        -pnp_output.external.external_emitter_isolation.coulombs,
        1e-14,
        0.0,
    );
    close(
        out.thermal.electrical.watts,
        pnp_output.thermal.electrical.watts,
        1e-14,
        0.0,
    );
    Ok(())
}

#[test]
fn device_borrowed_batch_and_nonfinite_voltage_contract() -> TestResult {
    let nominal = mixed_model();
    let prepared = nominal.prepare(Temperature::new(300.15)?, 0.0)?;
    let device = prepared.device_kernel()?;
    let input = device::Bias {
        nodes: mixed_nodes(),
        nqs: nqs::State::default(),
    };
    let mut output = [device::Output::default(); 3];
    device.evaluate_batch(&[input; 3], &mut output)?;
    assert!(output.iter().all(|row| *row == output[0]));
    assert!(matches!(
        device.evaluate_batch(&[input; 2], &mut output),
        Err(EvaluationError::LengthMismatch)
    ));
    let mut invalid = input;
    invalid.nodes.si = f64::NAN;
    assert!(matches!(
        device.evaluate(&invalid),
        Err(EvaluationError::NonFiniteBias)
    ));
    let mut invalid = input;
    invalid.nodes.b = f64::MAX;
    invalid.nodes.bi = f64::MAX;
    assert!(matches!(
        device.evaluate(&invalid),
        Err(EvaluationError::NonFiniteResult
            | EvaluationError::InvalidPhysicalDomain
            | EvaluationError::NonFiniteBias)
    ));
    assert!(matches!(
        device.evaluate_noise(&output[0], 0.0),
        Err(EvaluationError::InvalidPhysicalDomain)
    ));
    Ok(())
}

#[test]
fn device_external_collector_charge_and_rcx_current_follow_source_branch_voltages() -> TestResult {
    let mut nominal = mixed_model();
    // At FBCpar=0, the published split assigns nonzero Cjcx01 to b-ci.
    // At FBCpar=0.4 with this example CBCpar, Cjcx01 is exactly zero.
    nominal.auxiliary.fbcpar = 0.0;
    let prepared = nominal.prepare(Temperature::new(300.15)?, 0.0)?;
    let kernel = prepared.device_kernel()?;
    let input = device::Bias {
        nodes: mixed_nodes(),
        nqs: nqs::State::default(),
    };
    let original = kernel.evaluate(&input)?;
    close(
        original.external.external_collector.amperes,
        (input.nodes.ci - input.nodes.c) / prepared.auxiliary.rcx,
        1e-14,
        0.0,
    );
    let mut other = input;
    other.nodes.b += 2e-6;
    let varied = kernel.evaluate(&other)?;
    assert_eq!(
        original.junction.intrinsic_collector,
        varied.junction.intrinsic_collector,
    );
    assert_ne!(
        original.junction.external_collector.coulombs,
        varied.junction.external_collector.coulombs,
    );
    assert_eq!(
        original.junction.peripheral_collector,
        varied.junction.peripheral_collector,
    );
    close(
        (varied.junction.external_collector.coulombs
            - original.junction.external_collector.coulombs)
            / 2e-6,
        original
            .junction
            .external_collector
            .farads
            .midpoint(varied.junction.external_collector.farads),
        0.1,
        1e-20,
    );
    Ok(())
}

#[test]
fn prepared_device_exposes_identical_seven_junction_thermal_slopes_for_both_polarities()
-> TestResult {
    for polarity in [hicum2::Polarity::Npn, hicum2::Polarity::Pnp] {
        for kelvin in [233.15, 300.15, 373.15] {
            let mut nominal = temperature::Nominal::source_defaults();
            nominal.polarity = polarity;
            nominal.transport.junction.intrinsic_collector.punch_through = 2.0;
            nominal.transport.junction.external_collector.punch_through = 2.5;
            nominal.transport.junction.substrate.punch_through = 1.8;
            nominal
                .transport
                .junction
                .peripheral_substrate
                .punch_through = 1.4;
            let prepared = nominal.prepare(Temperature::new(kelvin)?, 0.0)?;
            let device_kernel = prepared.device_kernel()?;
            let sign = if matches!(polarity, hicum2::Polarity::Npn) {
                1.0
            } else {
                -1.0
            };
            let op = device_kernel.evaluate(&device::Bias {
                nodes: device::Nodes {
                    b: sign * 0.72,
                    bp: sign * 0.67,
                    bi: sign * 0.66,
                    c: sign * 1.2,
                    ci: sign * 1.18,
                    e: 0.0,
                    ei: sign * 0.02,
                    s: sign * 0.03,
                    si: sign * 0.06,
                },
                nqs: nqs::State::default(),
            })?;
            let direct =
                prepared
                    .junction_kernel()?
                    .temperature_sensitivity(hicum2::junction::Bias {
                        vbiei: op.voltage.vbiei,
                        vbpei: op.voltage.vbpei,
                        vbici: op.voltage.vbici,
                        vbci: op.voltage.vbci,
                        vbpci: op.voltage.vbpci,
                        vsici: op.voltage.vsici,
                        vsc: op.voltage.vsc,
                    })?;
            assert_eq!(device_kernel.junction_temperature_sensitivity(&op)?, direct);
        }
    }
    Ok(())
}

#[test]
fn complete_hiccr_knee_and_delay_device_temperature_slopes_match_both_polarities() -> TestResult {
    for polarity in [hicum2::Polarity::Npn, hicum2::Polarity::Pnp] {
        for mode in [0.0, 2.3] {
            for kelvin in [233.15, 300.15, 373.15] {
                let mut nominal = mixed_model();
                nominal.polarity = polarity;
                nominal.transport.flcomp = mode;
                nominal.transport.dt0h = 2e-12;
                nominal.transport.tbvl = -1e-12;
                nominal.coefficients.alvs = 0.0007;
                nominal.coefficients.zetaci = 1.3;
                let prepared = nominal.prepare(Temperature::new(kelvin)?, 0.0)?;
                let kernel = prepared.device_kernel()?;
                let sign = if matches!(polarity, hicum2::Polarity::Npn) {
                    1.0
                } else {
                    -1.0
                };
                let mut nodes = mixed_nodes();
                for v in [
                    &mut nodes.b,
                    &mut nodes.bp,
                    &mut nodes.bi,
                    &mut nodes.c,
                    &mut nodes.ci,
                    &mut nodes.ei,
                    &mut nodes.e,
                    &mut nodes.s,
                    &mut nodes.si,
                ] {
                    *v *= sign;
                }
                let op = kernel.evaluate(&device::Bias {
                    nodes,
                    nqs: nqs::State::default(),
                })?;
                let actual = kernel.transport_knee_temperature_sensitivity(&op)?;
                let transport = prepared.transport_kernel()?;
                close(
                    actual.0,
                    transport.critical_current_temperature_sensitivity(op.voltage.vciei)?,
                    1e-13,
                    1e-16,
                );
                close(
                    actual.1,
                    transport.low_current_transit_time_temperature_sensitivity(op.voltage.vbici)?,
                    1e-13,
                    1e-21,
                );
            }
        }
    }
    Ok(())
}

fn assert_full_device_thermal_source(
    physical: &device::Output,
    before: &device::Output,
    after: &device::Output,
    aval: hicum2::avalanche::TemperatureSensitivity,
    step: f64,
) {
    let s = physical.transfer.temperature_sensitivity;
    for (name, predicted, lower, upper, absolute) in [
        (
            "Itf",
            s.forward_current,
            before.transfer.forward_current.value,
            after.transfer.forward_current.value,
            1e-15,
        ),
        (
            "Itr",
            s.reverse_current,
            before.transfer.reverse_current.value,
            after.transfer.reverse_current.value,
            1e-15,
        ),
        (
            "Qp",
            s.total_hole_charge,
            before.transfer.total_hole_charge.value,
            after.transfer.total_hole_charge.value,
            1e-20,
        ),
        (
            "Qf",
            s.forward_diffusion_charge,
            before.transfer.forward_diffusion_charge.value,
            after.transfer.forward_diffusion_charge.value,
            1e-20,
        ),
        (
            "QfT",
            s.weighted_forward_charge,
            before.transfer.weighted_forward_charge.value,
            after.transfer.weighted_forward_charge.value,
            1e-20,
        ),
        (
            "Iavl",
            aval.generation,
            before.avalanche.generation.value,
            after.avalanche.generation.value,
            1e-15,
        ),
        (
            "Gain",
            aval.gain,
            before.avalanche.gain,
            after.avalanche.gain,
            1e-12,
        ),
    ] {
        let numerical = (upper - lower) / (2.0 * step);
        let allowed = 0.01_f64.mul_add(predicted.abs().max(numerical.abs()), absolute);
        assert!(
            (predicted - numerical).abs() <= allowed,
            "{name} thermal: exact={predicted:.14e} numerical={numerical:.14e}, tol={allowed:.4e}"
        );
    }
}

#[test]
fn device_composed_implicit_transport_and_avalanche_thermal_jacobian_matches_repreparation()
-> TestResult {
    let mut cases = 0;
    for polarity in [hicum2::Polarity::Npn, hicum2::Polarity::Pnp] {
        for mode in [0.0, 2.3] {
            for kelvin in [233.15, 300.15, 373.15] {
                let mut nominal = mixed_model();
                nominal.polarity = polarity;
                nominal.transport.flcomp = mode;
                nominal.coefficients.alfav = 0.002;
                nominal.coefficients.alqav = -0.003;
                nominal.coefficients.alkav = 0.001;
                nominal.coefficients.alt0 = 0.0003;
                let step = 0.005;
                let prepared = nominal.prepare(Temperature::new(kelvin)?, 0.0)?;
                let before = nominal
                    .prepare(Temperature::new(kelvin - step)?, 0.0)?
                    .device_kernel()?;
                let after = nominal
                    .prepare(Temperature::new(kelvin + step)?, 0.0)?
                    .device_kernel()?;
                let current = prepared.device_kernel()?;
                let sign = if matches!(polarity, hicum2::Polarity::Npn) {
                    1.0
                } else {
                    -1.0
                };
                for scale in [0.93, 1.05] {
                    let mut nodes = mixed_nodes();
                    nodes.b *= scale;
                    nodes.bp *= scale;
                    nodes.bi *= scale;
                    for value in [
                        &mut nodes.b,
                        &mut nodes.bp,
                        &mut nodes.bi,
                        &mut nodes.c,
                        &mut nodes.ci,
                        &mut nodes.ei,
                        &mut nodes.e,
                        &mut nodes.si,
                        &mut nodes.s,
                    ] {
                        *value *= sign;
                    }
                    let input = device::Bias {
                        nodes,
                        nqs: nqs::State::default(),
                    };
                    let op = current.evaluate(&input)?;
                    let lower = before.evaluate(&input)?;
                    let upper = after.evaluate(&input)?;
                    let avalanche = current.avalanche_temperature_sensitivity(&op)?;
                    assert_full_device_thermal_source(&op, &lower, &upper, avalanche, step);
                    cases += 1;
                }
            }
        }
    }
    assert_eq!(cases, 24);
    Ok(())
}
