use model_kernel_bipolar::{
    EvaluationError, ModelError,
    hicum2::external::{self, Bias, Capacitances, Parameters, Resistances},
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn close(actual: f64, expected: f64, relative: f64, absolute: f64) {
    let limit = relative.mul_add(actual.abs().max(expected.abs()), absolute);
    assert!(
        (actual - expected).abs() <= limit,
        "actual={actual:.15e}, expected={expected:.15e}, tolerance={limit:.4e}"
    );
}

const fn parameters() -> Parameters {
    Parameters {
        nominal: Resistances {
            external_base: 12.0,
            external_collector: 1.8,
            emitter: 4.0,
            substrate: 90.0,
        },
        effective: Resistances {
            external_base: 13.0,
            external_collector: 2.4,
            emitter: 5.0,
            substrate: 90.0,
        },
        capacitances: Capacitances {
            substrate_shunt: 2e-15,
            base_emitter_parasitic: 8e-15,
            base_emitter_partition: 0.25,
            base_collector_parasitic: 6e-15,
            base_collector_partition: 0.35,
            external_collector_zero_bias: 9e-15,
        },
    }
}
const fn bias() -> Bias {
    Bias {
        vbbp: 0.12,
        vcci: -0.08,
        veie: 0.04,
        vsis: -0.5,
        vbe: 0.7,
        vbpe: 0.67,
        vbci: -0.4,
        vbpci: -0.43,
    }
}

#[test]
fn external_source_resistor_current_and_parasitic_charge_derivatives() -> TestResult {
    let p = parameters();
    let prepared = external::Prepared::new(p)?;
    let output = prepared.evaluate(bias())?;
    for (value, resistance, voltage) in [
        (output.external_base, p.effective.external_base, bias().vbbp),
        (
            output.external_collector,
            p.effective.external_collector,
            bias().vcci,
        ),
        (output.emitter, p.effective.emitter, bias().veie),
        (output.substrate, p.effective.substrate, bias().vsis),
    ] {
        assert!(!value.shorted);
        close(value.amperes, voltage / resistance, 1e-14, 0.0);
        close(value.siemens, 1.0 / resistance, 1e-14, 0.0);
    }
    let split = prepared.split();
    for (value, capacitance, voltage) in [
        (
            output.substrate_shunt,
            p.capacitances.substrate_shunt,
            bias().vsis,
        ),
        (
            output.external_emitter_isolation,
            split.external_emitter_isolation,
            bias().vbe,
        ),
        (
            output.peripheral_emitter_isolation,
            split.peripheral_emitter_isolation,
            bias().vbpe,
        ),
        (
            output.external_collector_isolation,
            split.external_collector_isolation,
            bias().vbci,
        ),
        (
            output.peripheral_collector_isolation,
            split.peripheral_collector_isolation,
            bias().vbpci,
        ),
    ] {
        close(value.farads, capacitance, 1e-14, 0.0);
        close(value.coulombs, voltage * capacitance, 1e-14, 0.0);
    }
    assert!(split.external_collector_junction > 0.0);
    assert!(split.peripheral_collector_junction > 0.0);
    Ok(())
}

#[test]
fn source_external_cap_partition_conserves_charge_and_handles_branch_switch() -> TestResult {
    let mut samples = 0;
    for cjcx in [0.0, 1e-15, 9e-15, 90e-15] {
        for cbc in [0.0, 2e-15, 100e-15] {
            for cbe in [0.0, 8e-15] {
                for fbc in [0.0, 0.1, 0.3, 0.5, 0.9, 1.0] {
                    for fbe in [0.0, 0.2, 0.7, 1.0] {
                        let mut p = parameters();
                        p.capacitances.external_collector_zero_bias = cjcx;
                        p.capacitances.base_collector_parasitic = cbc;
                        p.capacitances.base_emitter_parasitic = cbe;
                        p.capacitances.base_collector_partition = fbc;
                        p.capacitances.base_emitter_partition = fbe;
                        let s = external::Prepared::new(p)?.split();
                        close(
                            s.external_collector_junction + s.peripheral_collector_junction,
                            cjcx,
                            2e-14,
                            1e-28,
                        );
                        close(
                            s.external_collector_isolation + s.peripheral_collector_isolation,
                            cbc,
                            2e-14,
                            1e-28,
                        );
                        close(
                            s.external_emitter_isolation + s.peripheral_emitter_isolation,
                            cbe,
                            2e-14,
                            1e-28,
                        );
                        let source_first = (1.0 - fbc) * (cjcx + cbc);
                        close(
                            s.external_collector_junction + s.external_collector_isolation,
                            source_first,
                            2e-14,
                            1e-28,
                        );
                        if source_first >= cbc {
                            close(s.external_collector_isolation, cbc, 2e-14, 1e-28);
                            assert_eq!(s.peripheral_collector_isolation, 0.0);
                        } else {
                            assert_eq!(s.external_collector_junction, 0.0);
                            close(s.external_collector_isolation, source_first, 2e-14, 1e-28);
                        }
                        close(s.peripheral_emitter_isolation, fbe * cbe, 2e-14, 1e-28);
                        samples += 1;
                    }
                }
            }
        }
    }
    assert_eq!(samples, 576);
    Ok(())
}

#[test]
fn nominal_source_threshold_controls_short_and_substrate_shunt() -> TestResult {
    for nominal in [0.0, 0.0009, 0.001, 0.5, 10.0] {
        let mut p = parameters();
        p.nominal = Resistances {
            external_base: nominal,
            external_collector: nominal,
            emitter: nominal,
            substrate: nominal,
        };
        p.effective = Resistances {
            external_base: 0.0001,
            external_collector: 0.0001,
            emitter: 0.0001,
            substrate: 0.0001,
        };
        let output = external::Prepared::new(p)?.evaluate(bias())?;
        let short = nominal < 0.001;
        for r in [
            output.external_base,
            output.external_collector,
            output.emitter,
            output.substrate,
        ] {
            assert_eq!(r.shorted, short);
            close(r.siemens, if short { 0.0 } else { 10000.0 }, 0.0, 0.0);
            if short {
                assert_eq!(r.amperes, 0.0);
            }
        }
        close(
            output.substrate_shunt.farads,
            if short { 0.0 } else { 2e-15 },
            0.0,
            0.0,
        );
    }
    Ok(())
}

#[test]
fn external_source_derivative_jacobians_are_exact_at_all_independent_voltages() -> TestResult {
    let prepared = external::Prepared::new(parameters())?;
    let center = prepared.evaluate(bias())?;
    let h = 1e-6;
    for branch in 0..8 {
        let shifted = |delta: f64| {
            let mut v = bias();
            match branch {
                0 => v.vbbp += delta,
                1 => v.vcci += delta,
                2 => v.veie += delta,
                3 => v.vsis += delta,
                4 => v.vbe += delta,
                5 => v.vbpe += delta,
                6 => v.vbci += delta,
                _ => v.vbpci += delta,
            }
            prepared.evaluate(v)
        };
        let high = shifted(h)?;
        let low = shifted(-h)?;
        let expected_current = [
            center.external_base.siemens,
            center.external_collector.siemens,
            center.emitter.siemens,
            center.substrate.siemens,
        ];
        let high_current = [
            high.external_base.amperes,
            high.external_collector.amperes,
            high.emitter.amperes,
            high.substrate.amperes,
        ];
        let low_current = [
            low.external_base.amperes,
            low.external_collector.amperes,
            low.emitter.amperes,
            low.substrate.amperes,
        ];
        for idx in 0..4 {
            close(
                (high_current[idx] - low_current[idx]) / (2.0 * h),
                if branch == idx {
                    expected_current[idx]
                } else {
                    0.0
                },
                2e-10,
                1e-12,
            );
        }
        let expected_capacitors = [
            center.substrate_shunt,
            center.external_emitter_isolation,
            center.peripheral_emitter_isolation,
            center.external_collector_isolation,
            center.peripheral_collector_isolation,
        ];
        let high_cap = [
            high.substrate_shunt,
            high.external_emitter_isolation,
            high.peripheral_emitter_isolation,
            high.external_collector_isolation,
            high.peripheral_collector_isolation,
        ];
        let low_cap = [
            low.substrate_shunt,
            low.external_emitter_isolation,
            low.peripheral_emitter_isolation,
            low.external_collector_isolation,
            low.peripheral_collector_isolation,
        ];
        for index in 0..5 {
            let responsible = match index {
                0 => 3,
                1 => 4,
                2 => 5,
                3 => 6,
                _ => 7,
            };
            close(
                (high_cap[index].coulombs - low_cap[index].coulombs) / (2.0 * h),
                if branch == responsible {
                    expected_capacitors[index].farads
                } else {
                    0.0
                },
                2e-10,
                1e-26,
            );
        }
    }
    Ok(())
}

#[test]
fn external_source_validates_parameters_bias_and_borrowed_batches() -> TestResult {
    for (name, value) in [
        ("rbx", -1.0),
        ("rcx", -1.0),
        ("re", -1.0),
        ("rsu", -1.0),
        ("rbx_t", -1.0),
        ("rcx_t", -1.0),
        ("re_t", -1.0),
        ("rsu_t", -1.0),
        ("csu", -1.0),
        ("cbepar", -1.0),
        ("cbcpar", -1.0),
        ("cjcx0", -1.0),
        ("fbepar", -0.1),
        ("fbcpar", 1.1),
    ] {
        let mut p = parameters();
        match name {
            "rbx" => p.nominal.external_base = value,
            "rcx" => p.nominal.external_collector = value,
            "re" => p.nominal.emitter = value,
            "rsu" => p.nominal.substrate = value,
            "rbx_t" => p.effective.external_base = value,
            "rcx_t" => p.effective.external_collector = value,
            "re_t" => p.effective.emitter = value,
            "rsu_t" => p.effective.substrate = value,
            "csu" => p.capacitances.substrate_shunt = value,
            "cbepar" => p.capacitances.base_emitter_parasitic = value,
            "cbcpar" => p.capacitances.base_collector_parasitic = value,
            "cjcx0" => p.capacitances.external_collector_zero_bias = value,
            "fbepar" => p.capacitances.base_emitter_partition = value,
            "fbcpar" => p.capacitances.base_collector_partition = value,
            _ => return Err("bad test parameter label".into()),
        }
        assert!(matches!(external::Prepared::new(p),
            Err(ModelError::InvalidParameter(label)) if label==name));
    }
    for (name, index) in [("rbx_t", 0), ("rcx_t", 1), ("re_t", 2), ("rsu_t", 3)] {
        let mut p = parameters();
        match index {
            0 => p.effective.external_base = 0.0,
            1 => p.effective.external_collector = 0.0,
            2 => p.effective.emitter = 0.0,
            _ => p.effective.substrate = 0.0,
        }
        assert!(matches!(external::Prepared::new(p),
            Err(ModelError::InvalidParameter(label)) if label==name));
    }
    let prepared = external::Prepared::new(parameters())?;
    let mut result = [external::Output::default(); 3];
    prepared.evaluate_batch(&[bias(); 3], &mut result)?;
    assert!(result.iter().all(|value| *value == result[0]));
    assert!(matches!(
        prepared.evaluate_batch(&[bias()], &mut result),
        Err(EvaluationError::LengthMismatch)
    ));
    let mut invalid = bias();
    invalid.vbe = f64::NAN;
    assert!(matches!(
        prepared.evaluate(invalid),
        Err(EvaluationError::NonFiniteBias)
    ));
    invalid = bias();
    invalid.vbbp = f64::MAX;
    let mut high_conductance = parameters();
    high_conductance.effective.external_base = 0.0001;
    let high_conductance = external::Prepared::new(high_conductance)?;
    assert!(matches!(
        high_conductance.evaluate(invalid),
        Err(EvaluationError::NonFiniteResult)
    ));
    Ok(())
}

fn check_external_power_slope(
    bias: Bias,
    slope: external::CurrentSensitivity,
    low: external::Output,
    high: external::Output,
    h: f64,
) {
    // Source FLSH=2: P_ext=Vbbp^2/RBX+Vcci^2/RCX+Veie^2/RE.
    // RSU intentionally does not contribute to source thermal power.
    let watts = |out: external::Output| {
        bias.vbbp.mul_add(
            out.external_base.amperes,
            bias.vcci.mul_add(
                out.external_collector.amperes,
                bias.veie * out.emitter.amperes,
            ),
        )
    };
    close(
        slope.external_dissipation_slope(bias),
        (watts(high) - watts(low)) / (2.0 * h),
        2e-8,
        1e-13,
    );
}

#[test]
fn exact_external_resistor_temperature_jacobian_matches_independent_full_preparation() -> TestResult
{
    use model_kernel_bipolar::{
        Temperature,
        hicum2::{device, nqs, temperature},
    };

    let mut cases = 0;
    for resistances in [
        (12.0, 7.0, 5.0, 8.0),
        (0.0009, 7.0, 0.0, 8.0),
        (12.0, 0.0009, 5.0, 0.0),
    ] {
        for coefficients in [(0.0, 0.0, 0.0), (1.4, -0.9, 2.6), (-2.5, 0.7, -1.5)] {
            let mut model = temperature::Nominal::source_defaults();
            model.auxiliary.rbx = resistances.0;
            model.auxiliary.rcx = resistances.1;
            model.auxiliary.re = resistances.2;
            model.auxiliary.rsu = resistances.3;
            model.coefficients.zetarbx = coefficients.0;
            model.coefficients.zetarcx = coefficients.1;
            model.coefficients.zetare = coefficients.2;
            for kelvin in [233.15, 300.15, 373.15] {
                let h = 0.008;
                let kernel = model
                    .prepare(Temperature::new(kelvin)?, 0.0)?
                    .external_kernel();
                for multiplier in [-2.0, 0.8, 3.0] {
                    let bias = Bias {
                        vbbp: bias().vbbp * multiplier,
                        vcci: bias().vcci * multiplier,
                        veie: bias().veie * multiplier,
                        vsis: bias().vsis * multiplier,
                        ..bias()
                    };
                    let operating = kernel.evaluate(bias)?;
                    let slope = kernel.temperature_sensitivity(operating)?;
                    let low = model
                        .prepare(Temperature::new(kelvin - h)?, 0.0)?
                        .external_kernel()
                        .evaluate(bias)?;
                    let high = model
                        .prepare(Temperature::new(kelvin + h)?, 0.0)?
                        .external_kernel()
                        .evaluate(bias)?;
                    for (actual, l, r) in [
                        (
                            slope.external_base,
                            low.external_base.amperes,
                            high.external_base.amperes,
                        ),
                        (
                            slope.external_collector,
                            low.external_collector.amperes,
                            high.external_collector.amperes,
                        ),
                        (slope.emitter, low.emitter.amperes, high.emitter.amperes),
                        (
                            slope.substrate,
                            low.substrate.amperes,
                            high.substrate.amperes,
                        ),
                    ] {
                        close(actual, (r - l) / (2.0 * h), 2e-8, 1e-13);
                    }
                    check_external_power_slope(bias, slope, low, high, h);
                    assert_eq!(slope.substrate, 0.0);
                    if model.auxiliary.rbx < 0.001 {
                        assert_eq!(slope.external_base, 0.0);
                    }
                    if model.auxiliary.rcx < 0.001 {
                        assert_eq!(slope.external_collector, 0.0);
                    }
                    if model.auxiliary.re < 0.001 {
                        assert_eq!(slope.emitter, 0.0);
                    }

                    let device_kernel = model
                        .prepare(Temperature::new(kelvin)?, 0.0)?
                        .device_kernel()?;
                    let physical = device_kernel.evaluate(&device::Bias {
                        nodes: device::Nodes {
                            b: 0.78,
                            bp: 0.7,
                            bi: 0.69,
                            c: 1.2,
                            ci: 1.17,
                            e: 0.0,
                            ei: 0.02,
                            s: -0.1,
                            si: -0.11,
                        },
                        nqs: nqs::State::default(),
                    })?;
                    let from_device = device_kernel.external_temperature_sensitivity(&physical)?;
                    let expected = kernel.temperature_sensitivity(physical.external)?;
                    assert_eq!(from_device, expected);
                    cases += 1;
                }
            }
        }
    }
    assert_eq!(cases, 81);
    Ok(())
}

#[test]
fn exact_external_temperature_slope_constructor_rejects_nonfinite_coefficients() -> TestResult {
    let original = parameters();
    let unconfigured = external::Prepared::new(original)?;
    let sensitivity = unconfigured.temperature_sensitivity(unconfigured.evaluate(bias())?)?;
    assert_eq!(sensitivity, external::CurrentSensitivity::default());
    for nonfinite in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let invalid = Resistances {
            external_base: nonfinite,
            ..Resistances::default()
        };
        assert!(matches!(
            external::Prepared::with_temperature_slope(original, invalid),
            Err(ModelError::InvalidParameter("rbx_temperature_slope"))
        ));
    }
    Ok(())
}
