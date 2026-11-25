use model_kernel_bipolar::{EvaluationError, ModelError, Temperature, vbic};
use vbic::{depletion, parasitic, resistance, transport};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn close(actual: f64, expected: f64, relative: f64, absolute: f64) {
    let tolerance = relative.mul_add(actual.abs().max(expected.abs()), absolute);
    assert!(
        (actual - expected).abs() <= tolerance,
        "actual={actual:.15e}; expected={expected:.15e}; tolerance={tolerance:.3e}"
    );
}

const fn resistor_params() -> resistance::Parameters {
    resistance::Parameters {
        rcx: 8.0,
        rci: 5.0,
        vo: 0.5,
        gamm: 0.13,
        hrcf: 0.45,
        rbx: 3.0,
        rbi: 2.0,
        re: 1.0,
        rbp: 12.0,
        rs: 5.0,
    }
}

const fn charge_model() -> depletion::Parameters {
    depletion::Parameters {
        fc: 0.9,
        pe: 0.75,
        me: 0.33,
        aje: -0.5,
        pc: 0.75,
        mc: 0.33,
        ajc: -0.5,
        vrt: 0.0,
        art: 0.1,
    }
}

const fn transport_model() -> transport::Parameters {
    transport::Parameters {
        is: 4.81e-17,
        nf: 1.018,
        nr: 1.01,
        isrr: 1.0,
        vef: 189.0,
        ver: 5.3,
        ikf: 0.009,
        ikr: 0.01,
        nkf: 0.5,
        base_charge_form: transport::BaseChargeForm::Coupled,
    }
}

const fn peripheral_model() -> parasitic::Parameters {
    parasitic::Parameters {
        isp: 4e-16,
        wsp: 0.3,
        nfp: 1.04,
        ikp: 1e-4,
    }
}

#[derive(Clone, Copy, Debug)]
struct Voltages {
    vbei: f64,
    vbep: f64,
    vbcp: f64,
    vrcx: f64,
    vrci: f64,
    vbci: f64,
    vbcx: f64,
    vrbx: f64,
    vrbi: f64,
    vre: f64,
    vrbp: f64,
    vrs: f64,
}

impl Voltages {
    const fn at(emitter: f64, collector: f64, resistance_bias: f64) -> Self {
        Self {
            vbei: emitter,
            vbep: emitter - 0.1,
            vbcp: collector - 0.3,
            vrcx: resistance_bias * 0.8,
            vrci: resistance_bias,
            vbci: collector,
            vbcx: collector + 0.1,
            vrbx: resistance_bias * 0.1,
            vrbi: resistance_bias * 0.4,
            vre: resistance_bias * 0.2,
            vrbp: resistance_bias * 0.15,
            vrs: resistance_bias * 0.06,
        }
    }
}

fn evaluate(
    resistors: &resistance::Prepared,
    transport: &transport::Prepared,
    peripheral: &parasitic::Prepared,
    depletion: &depletion::Prepared,
    bias: Voltages,
) -> Result<resistance::Output, EvaluationError> {
    let charges = depletion.evaluate(bias.vbei, bias.vbci)?;
    let transport = transport.evaluate(transport::Bias {
        vbei: bias.vbei,
        vbci: bias.vbci,
        depletion: charges,
    })?;
    let peripheral = peripheral.evaluate(parasitic::Bias {
        vbep: bias.vbep,
        vbci: bias.vbci,
        vbcp: bias.vbcp,
    })?;
    resistors.evaluate(&resistance::Bias {
        vrcx: bias.vrcx,
        vrci: bias.vrci,
        vbci: bias.vbci,
        vbcx: bias.vbcx,
        vrbx: bias.vrbx,
        vrbi: bias.vrbi,
        vre: bias.vre,
        vrbp: bias.vrbp,
        vrs: bias.vrs,
        transport,
        parasitic: peripheral,
    })
}

type Axis = (fn(&mut Voltages, f64), fn(resistance::Output) -> f64, f64);

fn verify_local_derivatives(
    resistors: &resistance::Prepared,
    transport: &transport::Prepared,
    peripheral: &parasitic::Prepared,
    depletion: &depletion::Prepared,
    bias: Voltages,
) -> TestResult {
    let output = evaluate(resistors, transport, peripheral, depletion, bias)?;
    let axis: &[Axis] = &[
        (
            |v, d| v.vrci += d,
            |o| o.irci.amperes,
            output.irci.d_resistor,
        ),
        (|v, d| v.vbci += d, |o| o.irci.amperes, output.irci.d_first),
        (|v, d| v.vbcx += d, |o| o.irci.amperes, output.irci.d_second),
        (
            |v, d| v.vrbi += d,
            |o| o.irbi.amperes,
            output.irbi.d_resistor,
        ),
        (|v, d| v.vbei += d, |o| o.irbi.amperes, output.irbi.d_first),
        (|v, d| v.vbci += d, |o| o.irbi.amperes, output.irbi.d_second),
        (
            |v, d| v.vrbp += d,
            |o| o.irbp.amperes,
            output.irbp.d_resistor,
        ),
        (|v, d| v.vbep += d, |o| o.irbp.amperes, output.irbp.d_first),
        (|v, d| v.vbci += d, |o| o.irbp.amperes, output.irbp.d_second),
    ];
    for (update, component, expected) in axis {
        let h = 1e-6;
        let mut above = bias;
        let mut below = bias;
        update(&mut above, h);
        update(&mut below, -h);
        let high = component(evaluate(
            resistors, transport, peripheral, depletion, above,
        )?);
        let low = component(evaluate(
            resistors, transport, peripheral, depletion, below,
        )?);
        close(*expected, (high - low) / (2.0 * h), 3e-5, 1e-10);
    }
    close(
        output.ircx.amperes,
        bias.vrcx / resistor_params().rcx,
        1e-14,
        1e-17,
    );
    close(
        output.irbx.amperes,
        bias.vrbx / resistor_params().rbx,
        1e-14,
        1e-17,
    );
    close(
        output.ire.amperes,
        bias.vre / resistor_params().re,
        1e-14,
        1e-17,
    );
    close(
        output.irs.amperes,
        bias.vrs / resistor_params().rs,
        1e-14,
        1e-17,
    );
    Ok(())
}

#[test]
fn nonlinear_collector_and_both_base_resistors_have_correct_cross_derivatives() -> TestResult {
    let temperature = Temperature::new(300.15)?;
    for form in [
        transport::BaseChargeForm::Coupled,
        transport::BaseChargeForm::Factorized,
    ] {
        let mut model = transport_model();
        model.base_charge_form = form;
        let transport = transport::Prepared::new(model, temperature)?;
        let parasitic = parasitic::Prepared::new(peripheral_model(), temperature)?;
        let charges = depletion::Prepared::new(charge_model())?;
        for emitter in [-0.2, 0.1, 0.55, 0.78, 0.9] {
            for collector in [-0.9, -0.35, -0.02, 0.2] {
                for vres in [-0.8, -0.05, 0.0, 0.02, 0.6] {
                    let resistors = resistance::Prepared::new(resistor_params(), temperature)?;
                    verify_local_derivatives(
                        &resistors,
                        &transport,
                        &parasitic,
                        &charges,
                        Voltages::at(emitter, collector, vres),
                    )?;
                }
            }
        }
    }
    Ok(())
}

#[test]
fn collector_factor_gamm_and_vo_hrcf_limits_are_exact() -> TestResult {
    let temperature = Temperature::new(300.15)?;
    let transport = transport::Prepared::new(transport_model(), temperature)?;
    let parasitic = parasitic::Prepared::new(peripheral_model(), temperature)?;
    let charges = depletion::Prepared::new(charge_model())?;
    for (gamm, vo, hrcf) in [
        (0.0, 0.5, 0.45),
        (0.13, 0.0, 0.45),
        (0.13, 0.5, 0.0),
        (0.0, 0.0, 0.0),
    ] {
        let mut p = resistor_params();
        p.gamm = gamm;
        p.vo = vo;
        p.hrcf = hrcf;
        let prepared = resistance::Prepared::new(p, temperature)?;
        let bias = Voltages::at(0.8, -0.1, 0.15);
        let result = evaluate(&prepared, &transport, &parasitic, &charges, bias)?;
        assert!(result.irci.amperes.is_finite());
        if gamm == 0.0 && vo == 0.0 {
            close(result.irci.amperes, bias.vrci / p.rci, 1e-14, 1e-16);
            assert_eq!(result.irci.d_first, 0.0);
            assert_eq!(result.irci.d_second, 0.0);
        }
        let second = Voltages { vrci: -0.3, ..bias };
        let reverse = evaluate(&prepared, &transport, &parasitic, &charges, second)?;
        assert!(reverse.irci.amperes.is_finite());
    }
    Ok(())
}

#[test]
fn disabled_branches_and_invalid_inputs_fail_closed() -> TestResult {
    let temperature = Temperature::new(300.15)?;
    let mut p = resistor_params();
    p.rci = 0.0;
    p.rbi = 0.0;
    p.rbp = 0.0;
    let resistors = resistance::Prepared::new(p, temperature)?;
    let transport = transport::Prepared::new(transport_model(), temperature)?;
    let parasitic = parasitic::Prepared::new(peripheral_model(), temperature)?;
    let charges = depletion::Prepared::new(charge_model())?;
    let bias = Voltages::at(0.8, -0.5, 0.2);
    let actual = evaluate(&resistors, &transport, &parasitic, &charges, bias)?;
    assert_eq!(actual.irci, resistance::Current3::default());
    assert_eq!(actual.irbi, resistance::Current3::default());
    assert_eq!(actual.irbp, resistance::Current3::default());

    let mut bad = resistor_params();
    bad.rci = -1.0;
    assert!(matches!(
        resistance::Prepared::new(bad, temperature),
        Err(ModelError::InvalidParameter("rci"))
    ));
    bad = resistor_params();
    bad.hrcf = f64::NAN;
    assert!(matches!(
        resistance::Prepared::new(bad, temperature),
        Err(ModelError::InvalidParameter("hrcf"))
    ));
    let resistors = resistance::Prepared::new(resistor_params(), temperature)?;
    let mut nan = bias;
    nan.vrcx = f64::NAN;
    let failure = evaluate(&resistors, &transport, &parasitic, &charges, nan);
    assert!(matches!(failure, Err(EvaluationError::NonFiniteBias)));
    Ok(())
}

#[test]
fn homogeneous_resistance_batch_preserves_outputs() -> TestResult {
    let temperature = Temperature::new(300.15)?;
    let resistors = resistance::Prepared::new(resistor_params(), temperature)?;
    let transport = transport::Prepared::new(transport_model(), temperature)?;
    let peripheral = parasitic::Prepared::new(peripheral_model(), temperature)?;
    let depletion = depletion::Prepared::new(charge_model())?;
    let bias = Voltages::at(0.76, -0.3, 0.1);
    let source = transport.evaluate(transport::Bias {
        vbei: bias.vbei,
        vbci: bias.vbci,
        depletion: depletion.evaluate(bias.vbei, bias.vbci)?,
    })?;
    let parasite = peripheral.evaluate(parasitic::Bias {
        vbep: bias.vbep,
        vbci: bias.vbci,
        vbcp: bias.vbcp,
    })?;
    let one = resistance::Bias {
        vrcx: bias.vrcx,
        vrci: bias.vrci,
        vbci: bias.vbci,
        vbcx: bias.vbcx,
        vrbx: bias.vrbx,
        vrbi: bias.vrbi,
        vre: bias.vre,
        vrbp: bias.vrbp,
        vrs: bias.vrs,
        transport: source,
        parasitic: parasite,
    };
    let inputs = [one; 3];
    let mut outputs = [resistance::Output::default(); 3];
    resistors.evaluate_batch(&inputs, &mut outputs)?;
    assert!(outputs.iter().all(|output| *output == outputs[0]));
    assert!(matches!(
        resistors.evaluate_batch(&inputs, &mut outputs[..2]),
        Err(EvaluationError::LengthMismatch)
    ));
    Ok(())
}

#[test]
fn native_vbic_collector_internal_resistor_matches_ngspice_47() -> TestResult {
    // Independent ngspice-47 .op oracle using exactly one internal collector
    // resistance, no other parasitic resistances or diode currents. The
    // internal CI voltage is reported by v(q1#collCI), not inferred from
    // terminal voltages. Here Vrci = Vce - Vci, Vbcx = Vbei - Vce.
    // KCL: Irci should match the negative external VCE supply current.
    let csv = include_str!("fixtures/vbic_ngspice47_resistance.csv");
    let temperature = Temperature::new(300.15)?;
    let mut count = 0usize;
    for row in csv.lines().filter(|line| !line.starts_with('#')) {
        let [
            variant,
            _selector,
            external_emitter,
            external_collector,
            _intrinsic_emitter,
            intrinsic_collector,
            collector_internal,
            supply_current,
        ] = <[&str; 8]>::try_from(row.split(',').collect::<Vec<_>>())
            .map_err(|_| "invalid VBIC resistor reference row")?;
        let (gamm, vo, hrcf) = match variant {
            "linear" => (0.0, 0.0, 0.0),
            "field" => (0.1, 0.0, 0.0),
            "velocity" => (0.1, 0.5, 0.0),
            "highcurrent" => (0.1, 0.5, 0.5),
            _ => return Err("invalid collector resistance variant".into()),
        };
        let mut model = resistor_params();
        model.rci = 5.0;
        model.rcx = 0.0;
        model.rbx = 0.0;
        model.rbi = 0.0;
        model.re = 0.0;
        model.rbp = 0.0;
        model.rs = 0.0;
        model.gamm = gamm;
        model.vo = vo;
        model.hrcf = hrcf;
        let prepared = resistance::Prepared::new(model, temperature)?;
        let source_emitter_voltage: f64 = external_emitter.parse()?;
        let supply_voltage: f64 = external_collector.parse()?;
        let internal_collector_bias: f64 = intrinsic_collector.parse()?;
        let internal_collector_voltage: f64 = collector_internal.parse()?;
        let supply_current: f64 = supply_current.parse()?;
        // ngspice 47 clamps an explicitly specified RCX=0 to 0.01 ohm.
        // The extra collector node is collapsed, so recover the actual CX
        // bias from its source current and source series-resistor law.
        // This is an input reconstruction, not a relaxed numerical oracle.
        let collector_external_voltage = supply_current.mul_add(0.01, supply_voltage);
        let result = prepared.evaluate(&resistance::Bias {
            vrcx: 0.0,
            vrci: collector_external_voltage - internal_collector_voltage,
            vbci: internal_collector_bias,
            vbcx: source_emitter_voltage - collector_external_voltage,
            vrbx: 0.0,
            vrbi: 0.0,
            vre: 0.0,
            vrbp: 0.0,
            vrs: 0.0,
            transport: transport::Output::default(),
            parasitic: parasitic::Output::default(),
        })?;
        close(result.irci.amperes, -supply_current, 3e-5, 8e-15);
        count += 1;
    }
    assert_eq!(count, 40);
    Ok(())
}
