use model_kernel_bipolar::{
    EvaluationError, ModelError, Temperature,
    hicum2::thermal::{self, Mode},
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn close(actual: f64, expected: f64, relative: f64, absolute: f64) {
    let tolerance = relative.mul_add(actual.abs().max(expected.abs()), absolute);
    assert!(
        (actual - expected).abs() <= tolerance,
        "actual={actual:.15e},expected={expected:.15e},tolerance={tolerance:.5e}"
    );
}

fn prepared(
    mode: Mode,
    nominal_rth: f64,
    effective_kelvin: f64,
    rise: f64,
) -> Result<thermal::Prepared, ModelError> {
    let tnom = Temperature::new(300.15)?;
    let tdev = Temperature::new(effective_kelvin)?;
    let exponent = 0.6;
    let alpha: f64 = 0.0004;
    let ratio = effective_kelvin / tnom.kelvin();
    let rth =
        nominal_rth * ratio.powf(exponent) * alpha.mul_add(effective_kelvin - tnom.kelvin(), 1.0);
    thermal::Prepared::new(thermal::Parameters {
        mode,
        nominal_resistance: nominal_rth,
        effective_resistance: rth,
        heat_capacity: 7e-9,
        effective_temperature: tdev,
        nominal_temperature: tnom,
        zetarth: exponent,
        alrth: alpha,
        thermal_rise: rise,
        temperature_slope: 1.0,
    })
}

const fn source_bias() -> thermal::ElectricalBias {
    thermal::ElectricalBias {
        transport: thermal::Terminal {
            voltage: 1.3,
            current: 0.002,
        },
        avalanche: thermal::Avalanche {
            collector_potential: 0.7,
            collector_voltage: -0.5,
            current: 1e-4,
        },
        intrinsic_emitter: thermal::Terminal {
            voltage: 0.8,
            current: 3e-5,
        },
        intrinsic_collector: thermal::Terminal {
            voltage: -0.5,
            current: -3e-6,
        },
        peripheral_emitter: thermal::Terminal {
            voltage: 0.75,
            current: 5e-6,
        },
        peripheral_collector: thermal::Terminal {
            voltage: -0.55,
            current: -1e-6,
        },
        substrate: thermal::Terminal {
            voltage: -0.7,
            current: -4e-7,
        },
        intrinsic_base: thermal::Series {
            voltage: 0.011,
            resistance: 12.0,
        },
        external_emitter: thermal::Series {
            voltage: 0.025,
            resistance: 4.0,
        },
        external_collector: thermal::Series {
            voltage: 0.052,
            resistance: 8.0,
        },
        external_base: thermal::Series {
            voltage: 0.020,
            resistance: 3.0,
        },
    }
}

#[test]
fn source_flsh_modes_power_terms_and_temperature_rc_equilibrium() -> TestResult {
    let bias = source_bias();
    for kelvin in [233.15, 300.15, 373.15] {
        let rise = kelvin - 300.15;
        let rth = 500.0 * (kelvin / 300.15f64).powf(0.6) * 0.0004f64.mul_add(rise, 1.0);
        for mode in [
            Mode::Disabled,
            Mode::TransferAvalanche,
            Mode::FullElectrical,
        ] {
            let kernel = prepared(mode, 500.0, kelvin, rise)?;
            let output = kernel.evaluate(bias)?;
            if mode == Mode::Disabled {
                assert!(output.shorted);
                assert_eq!(output.thermal_current, 0.0);
                assert_eq!(output.heat_charge, 0.0);
                continue;
            }
            assert!(!output.shorted);
            let p_transfer = 1.3 * 0.002;
            let p_avalanche = (0.7 + 0.5) * 1e-4;
            let p_base = p_transfer + p_avalanche;
            close(output.electrical.transport.watts, p_transfer, 1e-14, 0.0);
            close(output.electrical.avalanche.watts, p_avalanche, 1e-14, 0.0);
            if mode == Mode::TransferAvalanche {
                close(output.electrical.watts, p_base, 1e-14, 0.0);
                assert_eq!(output.electrical.intrinsic_emitter.watts, 0.0);
                assert_eq!(output.electrical.intrinsic_base.watts, 0.0);
            } else {
                let p_diodes = [
                    0.8 * 3e-5,
                    (-0.5) * (-3e-6),
                    0.75 * 5e-6,
                    (-0.55) * (-1e-6),
                    (-0.7) * (-4e-7),
                ]
                .into_iter()
                .sum::<f64>();
                let p_series = 0.011f64.powi(2) / 12.0
                    + 0.025f64.powi(2) / 4.0
                    + 0.052f64.powi(2) / 8.0
                    + 0.020f64.powi(2) / 3.0;
                close(
                    output.electrical.watts,
                    p_base + p_diodes + p_series,
                    3e-14,
                    0.0,
                );
            }
            close(
                output.thermal_current,
                rise / rth - output.electrical.watts,
                2e-14,
                1e-20,
            );
            close(output.heat_charge, 7e-9 * rise, 1e-14, 0.0);
            assert_eq!(output.heat_capacitance, 7e-9);
        }
    }
    Ok(())
}

#[test]
fn thermal_source_temperature_derivative_matches_finite_difference() -> TestResult {
    let electrical = source_bias();
    for mode in [Mode::TransferAvalanche, Mode::FullElectrical] {
        for rise in [-55.0, -5.0, 0.0, 15.0, 95.0] {
            let center = prepared(mode, 500.0, 300.15 + rise, rise)?.evaluate(electrical)?;
            let h = 1e-5;
            let high = prepared(mode, 500.0, 300.15 + rise + h, rise + h)?.evaluate(electrical)?;
            let low = prepared(mode, 500.0, 300.15 + rise - h, rise - h)?.evaluate(electrical)?;
            close(
                center.d_thermal_current_d_rise_at_fixed_electrical,
                (high.thermal_current - low.thermal_current) / (2.0 * h),
                2e-8,
                1e-12,
            );
            close(
                center.heat_capacitance,
                (high.heat_charge - low.heat_charge) / (2.0 * h),
                2e-8,
                1e-15,
            );
        }
    }
    Ok(())
}

#[test]
fn source_resistive_and_diode_power_derivatives_include_native_signs() -> TestResult {
    let bias = source_bias();
    let kernel = prepared(Mode::FullElectrical, 650.0, 315.15, 15.0)?;
    let output = kernel.evaluate(bias)?;
    let h = 2e-6;
    for idx in 0..4 {
        let branch = match idx {
            0 => bias.intrinsic_base,
            1 => bias.external_emitter,
            2 => bias.external_collector,
            3 => bias.external_base,
            _ => unreachable!(),
        };
        let expected = match idx {
            0 => output.electrical.intrinsic_base,
            1 => output.electrical.external_emitter,
            2 => output.electrical.external_collector,
            3 => output.electrical.external_base,
            _ => unreachable!(),
        };
        let shifted = |change: f64, change_resistance: bool| {
            let mut next = branch;
            if change_resistance {
                next.resistance += change;
            } else {
                next.voltage += change;
            }
            let mut altered = bias;
            match idx {
                0 => altered.intrinsic_base = next,
                1 => altered.external_emitter = next,
                2 => altered.external_collector = next,
                _ => altered.external_base = next,
            }
            kernel.evaluate(altered)
        };
        let voltage_high = shifted(h, false)?;
        let voltage_low = shifted(-h, false)?;
        let resistance_high = shifted(h, true)?;
        let resistance_low = shifted(-h, true)?;
        close(
            expected.d_voltage,
            (voltage_high.electrical.watts - voltage_low.electrical.watts) / (2.0 * h),
            4e-9,
            1e-14,
        );
        close(
            expected.d_resistance,
            (resistance_high.electrical.watts - resistance_low.electrical.watts) / (2.0 * h),
            4e-7,
            1e-15,
        );
    }
    let mut altered = bias;
    altered.intrinsic_emitter.current += h;
    let forward = kernel.evaluate(altered)?;
    altered = bias;
    altered.intrinsic_emitter.current -= h;
    let reverse = kernel.evaluate(altered)?;
    close(
        output.electrical.intrinsic_emitter.d_current,
        (forward.electrical.watts - reverse.electrical.watts) / (2.0 * h),
        1e-12,
        1e-15,
    );
    altered = bias;
    altered.intrinsic_collector.voltage += h;
    let forward = kernel.evaluate(altered)?;
    altered = bias;
    altered.intrinsic_collector.voltage -= h;
    let reverse = kernel.evaluate(altered)?;
    close(
        output.electrical.intrinsic_collector.d_voltage,
        (forward.electrical.watts - reverse.electrical.watts) / (2.0 * h),
        1e-10,
        3e-14,
    );
    altered = bias;
    altered.avalanche.collector_voltage += h;
    let forward = kernel.evaluate(altered)?;
    altered = bias;
    altered.avalanche.collector_voltage -= h;
    let reverse = kernel.evaluate(altered)?;
    close(
        -output.electrical.avalanche.d_voltage,
        (forward.electrical.watts - reverse.electrical.watts) / (2.0 * h),
        1e-10,
        3e-14,
    );
    Ok(())
}

#[test]
fn source_thermal_nominal_cutoff_and_invalid_bias_contracts() -> TestResult {
    let source = source_bias();
    for mode in [
        Mode::Disabled,
        Mode::TransferAvalanche,
        Mode::FullElectrical,
    ] {
        for rth in [0.0, 0.0009, 0.001, 300.0] {
            let kernel = prepared(mode, rth, 300.15, 0.0)?;
            let out = kernel.evaluate(source)?;
            assert_eq!(out.shorted, mode == Mode::Disabled || rth < 0.001);
        }
    }
    let kernel = prepared(Mode::FullElectrical, 100.0, 300.15, 0.0)?;
    let mut bad = source;
    bad.intrinsic_base.resistance = -1.0;
    assert!(matches!(
        kernel.evaluate(bad),
        Err(EvaluationError::InvalidPhysicalDomain)
    ));
    bad = source;
    bad.avalanche.current = -0.01;
    assert!(matches!(
        kernel.evaluate(bad),
        Err(EvaluationError::InvalidPhysicalDomain)
    ));
    bad = source;
    bad.transport.current = f64::NAN;
    assert!(matches!(
        kernel.evaluate(bad),
        Err(EvaluationError::NonFiniteBias)
    ));
    let mut buffers = [thermal::Output::default(); 2];
    kernel.evaluate_batch(&[source, source], &mut buffers)?;
    assert_eq!(buffers[0], buffers[1]);
    assert!(matches!(
        kernel.evaluate_batch(&[source], &mut buffers),
        Err(EvaluationError::LengthMismatch)
    ));
    let nominal = Temperature::new(300.15)?;
    let mut parameter = thermal::Parameters {
        mode: Mode::FullElectrical,
        nominal_resistance: 100.0,
        effective_resistance: 100.0,
        heat_capacity: 0.0,
        effective_temperature: nominal,
        nominal_temperature: nominal,
        zetarth: 0.0,
        alrth: 0.0,
        thermal_rise: 0.0,
        temperature_slope: 1.0,
    };
    parameter.heat_capacity = -1.0;
    assert!(matches!(
        thermal::Prepared::new(parameter),
        Err(ModelError::InvalidParameter("cth"))
    ));
    parameter.heat_capacity = 0.0;
    parameter.temperature_slope = 2.0;
    assert!(matches!(
        thermal::Prepared::new(parameter),
        Err(ModelError::InvalidParameter("temperature_slope"))
    ));
    parameter.temperature_slope = 1.0;
    parameter.nominal_resistance = -1.0;
    assert!(matches!(
        thermal::Prepared::new(parameter),
        Err(ModelError::InvalidParameter("rth"))
    ));
    parameter.nominal_resistance = 100.0;
    parameter.effective_resistance = 0.0;
    assert!(matches!(
        thermal::Prepared::new(parameter),
        Err(ModelError::InvalidParameter("rth_t"))
    ));
    Ok(())
}

#[test]
fn source_nominal_series_resistors_excluded_below_one_milliohm() -> TestResult {
    let kernel = prepared(Mode::FullElectrical, 250.0, 300.15, 0.0)?;
    let mut bias = source_bias();
    bias.intrinsic_base.resistance = 0.0009;
    bias.external_emitter.resistance = 0.0009;
    bias.external_collector.resistance = 0.0009;
    bias.external_base.resistance = 0.0009;
    let zero = kernel.evaluate(bias)?;
    for branch in [
        zero.electrical.intrinsic_base,
        zero.electrical.external_emitter,
        zero.electrical.external_collector,
        zero.electrical.external_base,
    ] {
        assert_eq!(branch, thermal::ResistorPower::default());
    }
    bias.intrinsic_base.resistance = 0.001;
    let active = kernel.evaluate(bias)?;
    assert!(active.electrical.intrinsic_base.watts > 0.0);
    assert_eq!(active.electrical.external_emitter.watts, 0.0);
    close(
        active.electrical.watts - zero.electrical.watts,
        source_bias().intrinsic_base.voltage.powi(2) / 0.001,
        1e-14,
        0.0,
    );
    Ok(())
}

fn native_reference_thermal(
    mode: Mode,
    nominal_rth: f64,
    exponent: f64,
    alpha: f64,
    rise: f64,
    raw_kelvin: f64,
) -> Result<thermal::Prepared, ModelError> {
    let kelvin = raw_kelvin.clamp(173.15, 600.0);
    let operating = Temperature::new(kelvin)?;
    let nominal = Temperature::new(300.15)?;
    let rth_t =
        nominal_rth * (kelvin / 300.15).powf(exponent) * alpha.mul_add(kelvin - 300.15, 1.0);
    thermal::Prepared::new(thermal::Parameters {
        mode,
        nominal_resistance: nominal_rth,
        effective_resistance: rth_t,
        heat_capacity: 1e-9,
        effective_temperature: operating,
        nominal_temperature: nominal,
        zetarth: exponent,
        alrth: alpha,
        thermal_rise: rise,
        temperature_slope: if mode != Mode::Disabled && raw_kelvin > 173.15 && raw_kelvin < 600.0 {
            1.0
        } else {
            0.0
        },
    })
}

#[test]
fn native_ngspice47_thermal_equilibrium_and_temperature_clamp_oracle() -> TestResult {
    let fixture = include_str!("fixtures/hicum2_ngspice47_thermal.csv");
    let mut all = 0;
    let mut active = 0;
    let mut conforming = 0;
    let mut clamped_non_equilibrium = 0;
    for row in fixture.lines().filter(|row| !row.starts_with('#')) {
        let [
            mode,
            ambient,
            _vbe,
            _vce,
            rth,
            zeta,
            alpha,
            rise,
            ic,
            emitter_diode,
            collector_diode,
            emitter_voltage,
            collector_voltage,
        ] = <[&str; 13]>::try_from(row.split(',').collect::<Vec<_>>())
            .map_err(|_| "malformed HICUM thermal reference row")?;
        let mode = match mode {
            "0" => Mode::Disabled,
            "1" => Mode::TransferAvalanche,
            "2" => Mode::FullElectrical,
            _ => return Err("unknown HICUM self-heating mode".into()),
        };
        let ambient: f64 = ambient.parse()?;
        let rise: f64 = rise.parse()?;
        let nominal_rth: f64 = rth.parse()?;
        let exponent: f64 = zeta.parse()?;
        let alpha: f64 = alpha.parse()?;
        let raw_kelvin = ambient + 273.15 + rise;
        let prepared =
            native_reference_thermal(mode, nominal_rth, exponent, alpha, rise, raw_kelvin)?;
        let emitter: f64 = emitter_voltage.parse()?;
        let collector: f64 = collector_voltage.parse()?;
        let collector_current: f64 = ic.parse()?;
        let input = thermal::ElectricalBias {
            transport: thermal::Terminal {
                voltage: emitter - collector,
                current: collector_current,
            },
            intrinsic_emitter: thermal::Terminal {
                voltage: emitter,
                current: emitter_diode.parse()?,
            },
            intrinsic_collector: thermal::Terminal {
                voltage: collector,
                current: collector_diode.parse()?,
            },
            ..thermal::ElectricalBias::default()
        };
        let output = prepared.evaluate(input)?;
        if mode == Mode::Disabled {
            assert!(output.shorted);
            assert_eq!(rise, 0.0);
        } else {
            active += 1;
            assert!(!output.shorted);
            let relative = output.thermal_current.abs() / output.electrical.watts.abs().max(1e-25);
            if raw_kelvin >= 600.0 {
                // Native ngspice clamps the thermal unknown exactly at the
                // source TMAX and nevertheless reports OP as converged,
                // even though the electrical dissipation remains much
                // higher than the cooling flow. Do not misclassify this
                // as a physically valid thermal DC solution.
                clamped_non_equilibrium += 1;
                assert!(
                    relative > 0.9,
                    "unexpected saturated KCL residual: {relative}"
                );
                assert_eq!(output.d_tdev_d_rise, 0.0);
            } else {
                conforming += 1;
                assert!(
                    relative < 2e-7,
                    "HICUM native power mismatch {relative} at ambient {ambient} K"
                );
                assert_eq!(output.d_tdev_d_rise, 1.0);
            }
        }
        all += 1;
    }
    assert_eq!(all, 54);
    assert_eq!(active, 36);
    assert_eq!(conforming, 30);
    assert_eq!(clamped_non_equilibrium, 6);
    Ok(())
}
