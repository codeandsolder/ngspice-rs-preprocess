use model_kernel_bipolar::{EvaluationError, ModelError, vbic};

type TestResult = Result<(), Box<dyn std::error::Error>>;
use vbic::{depletion, transport};

fn close(actual: f64, expected: f64, relative: f64, absolute: f64) {
    let tolerance = relative.mul_add(actual.abs().max(expected.abs()), absolute);
    assert!(
        (actual - expected).abs() <= tolerance,
        "expected {expected:.16e}, got {actual:.16e}; tolerance {tolerance:.5e}"
    );
}

const fn transport_params(form: transport::BaseChargeForm) -> transport::Parameters {
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
        base_charge_form: form,
    }
}

const fn default_depletion() -> depletion::Parameters {
    // ngspice 47 VBIC default P/ M/ A/ FC, zero punch-through.
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

const fn ihp_depletion() -> depletion::Parameters {
    // IHP SG13G2 public npn13G2_NX_vbic LEVEL=9, Nx=4 nominal.
    depletion::Parameters {
        fc: 0.8,
        pe: 0.92,
        me: 0.12,
        aje: -0.5,
        pc: 0.558,
        mc: 0.12,
        ajc: -0.5,
        vrt: 0.0,
        art: 0.1,
    }
}

fn coupled(
    kernel: &transport::Prepared,
    depletion: &depletion::Prepared,
    emitter: f64,
    collector: f64,
) -> Result<transport::Output, EvaluationError> {
    kernel.evaluate(transport::Bias {
        vbei: emitter,
        vbci: collector,
        depletion: depletion.evaluate(emitter, collector)?,
    })
}

#[test]
fn vbic_level9_op_current_and_nodal_derivatives_match_ngspice_47() -> TestResult {
    // Reference: ngspice 47 with Q1 c b 0 0 0 TRANSPORT, VBE and VCE
    // independent, VBC=VBE-VCE, .options gmin=1e-18, TNOM=TEMP=27.
    // IS=4.81e-17, NF=1.018, NR=1.01, ISRR=1, VEF=189, VER=5.3,
    // IKF=.009, IKR=.01, NKF=.5; QBM=0 or QBM=1 as marked.
    // All other currents and physical charges are disabled in the card,
    // but normalized qdbe/qdbc are NOT zero. Use the intrinsic @q1[vbe]
    // and @q1[vbc] reported by ngspice: its zero-resistance devices still
    // introduce tiny internal voltage differences under high current.
    // Values: ngspice .control op, set numdgt=15,
    // print @q1[ic] @q1[gm] @q1[go].
    let reference = include_str!("fixtures/vbic_ngspice47_transport.csv");
    let charges = depletion::Prepared::new(default_depletion())?;
    let temperature = model_kernel_bipolar::Temperature::new(300.15)?;
    for line in reference.lines().filter(|line| !line.starts_with('#')) {
        let cells: Vec<&str> = line.split(',').collect();
        let [
            form_text,
            _external_emitter,
            _external_collector,
            emitter_text,
            collector_text,
            current_text,
            transconductance_text,
            collector_conductance_text,
        ] = <[&str; 8]>::try_from(cells).map_err(|_| "invalid reference CSV row")?;
        let form_id: u8 = form_text.parse()?;
        let emitter: f64 = emitter_text.parse()?;
        let collector: f64 = collector_text.parse()?;
        let current: f64 = current_text.parse()?;
        let transconductance: f64 = transconductance_text.parse()?;
        let conductance: f64 = collector_conductance_text.parse()?;
        let form = if form_id == 0 {
            transport::BaseChargeForm::Coupled
        } else {
            transport::BaseChargeForm::Factorized
        };
        let kernel = transport::Prepared::new(transport_params(form), temperature)?;
        let out = coupled(&kernel, &charges, emitter, collector)?;
        close(out.net.value, current, 2e-5, 3e-16);
        // ngspice gm is the intrinsic derivative with respect to Vbei at fixed Vbci;
        // ngspice go reports the native intrinsic Vbci derivative of the transfer law.
        close(out.net.d_vbei, transconductance, 2e-5, 3e-13);
        close(out.net.d_vbci, conductance, 2e-5, 3e-13);
    }
    Ok(())
}

#[test]
fn vbic_transport_charge_and_cross_derivatives_match_finite_differences() -> TestResult {
    let temperature = model_kernel_bipolar::Temperature::new(300.15)?;
    for form in [
        transport::BaseChargeForm::Coupled,
        transport::BaseChargeForm::Factorized,
    ] {
        let transport = transport::Prepared::new(transport_params(form), temperature)?;
        for params in [default_depletion(), ihp_depletion()] {
            let charges = depletion::Prepared::new(params)?;
            for (emitter, collector) in [
                (-0.2, -0.5),
                (0.0, 0.0),
                (0.50, -0.30),
                (0.80, -0.40),
                (0.90, 0.20),
                (0.70, 0.50),
                (0.55, 0.70),
                (0.85, 0.80),
            ] {
                let output = coupled(&transport, &charges, emitter, collector)?;
                for (get, derivative_emitter, derivative_collector) in [
                    (0u8, output.forward.d_vbei, output.forward.d_vbci),
                    (1u8, output.reverse.d_vbei, output.reverse.d_vbci),
                    (2u8, output.net.d_vbei, output.net.d_vbci),
                    (3u8, output.qb.d_vbei, output.qb.d_vbci),
                ] {
                    let f = |x: f64, y: f64| -> Result<f64, EvaluationError> {
                        let c = coupled(&transport, &charges, x, y)?;
                        Ok(match get {
                            0 => c.forward.value,
                            1 => c.reverse.value,
                            2 => c.net.value,
                            _ => c.qb.value,
                        })
                    };
                    let step = 1e-6;
                    let measured_emitter = (f(emitter + step, collector)?
                        - f(emitter - step, collector)?)
                        / (2.0 * step);
                    let measured_collector = (f(emitter, collector + step)?
                        - f(emitter, collector - step)?)
                        / (2.0 * step);
                    close(derivative_emitter, measured_emitter, 2e-5, 1e-12);
                    close(derivative_collector, measured_collector, 2e-5, 1e-12);
                }
            }
        }
    }
    Ok(())
}

#[test]
fn normalized_depletion_charges_validate_all_smoothing_modes() -> TestResult {
    let params = default_depletion();
    let variants = [
        params,
        depletion::Parameters {
            aje: 0.015,
            ajc: 0.015,
            ..params
        },
        depletion::Parameters { vrt: 0.6, ..params },
        depletion::Parameters {
            ajc: 0.02,
            vrt: 0.6,
            art: 0.03,
            ..params
        },
        depletion::Parameters {
            me: 1.0,
            mc: 1.0,
            ..params
        },
    ];
    for variant in variants {
        let prepared = depletion::Prepared::new(variant)?;
        let zero = prepared.evaluate(0.0, 0.0)?;
        close(zero.qdbe, 0.0, 0.0, 1e-15);
        close(zero.qdbc, 0.0, 0.0, 1e-15);
        for voltage in [-1.0, -0.65, -0.2, 0.0, 0.3, 0.5, 0.8, 1.0] {
            let d = prepared.evaluate(voltage, voltage)?;
            let step = 1e-6;
            let measured_emitter = (prepared.evaluate(voltage + step, voltage)?.qdbe
                - prepared.evaluate(voltage - step, voltage)?.qdbe)
                / (2.0 * step);
            let measured_collector = (prepared.evaluate(voltage, voltage + step)?.qdbc
                - prepared.evaluate(voltage, voltage - step)?.qdbc)
                / (2.0 * step);
            close(d.cbe, measured_emitter, 3e-5, 2e-9);
            close(d.cbc, measured_collector, 3e-5, 2e-9);
        }
    }
    Ok(())
}

#[test]
fn transport_rejects_invalid_parameters_and_batch_mismatch() -> TestResult {
    let temperature = model_kernel_bipolar::Temperature::new(300.15)?;
    let mut parameters = transport_params(transport::BaseChargeForm::Coupled);
    parameters.ikr = f64::NAN;
    assert!(matches!(
        transport::Prepared::new(parameters, temperature),
        Err(ModelError::InvalidParameter("ikr"))
    ));
    let kernel = transport::Prepared::new(
        transport_params(transport::BaseChargeForm::Coupled),
        temperature,
    )?;
    let charges = depletion::Prepared::new(default_depletion())?;
    let inputs = [
        transport::Bias {
            vbei: 0.8,
            vbci: -0.3,
            depletion: charges.evaluate(0.8, -0.3)?,
        },
        transport::Bias {
            vbei: 0.9,
            vbci: 0.2,
            depletion: charges.evaluate(0.9, 0.2)?,
        },
    ];
    let mut outputs = [transport::Output::default(); 2];
    kernel.evaluate_batch(&inputs, &mut outputs)?;
    for (input, output) in inputs.iter().zip(outputs) {
        assert_eq!(kernel.evaluate(*input)?, output);
    }
    assert!(matches!(
        kernel.evaluate_batch(&inputs, &mut outputs[..1]),
        Err(EvaluationError::LengthMismatch)
    ));
    let mut nan_bias = inputs[0];
    nan_bias.depletion.cbc = f64::NAN;
    assert!(matches!(
        kernel.evaluate(nan_bias),
        Err(EvaluationError::NonFiniteBias)
    ));
    Ok(())
}

#[test]
fn vbic_nondefault_junction_smoothing_matches_ngspice_47() -> TestResult {
    // Generated with the adjacent, version-pinned native ngspice fixture
    // generator. Uses the *reported intrinsic* device biases, not source
    // voltages, and retains the exact same transport parameters as the
    // default oracle. This specifically exercises the AJE/AJC/VPT/ART laws.
    let reference = include_str!("fixtures/vbic_ngspice47_extended.csv");
    let temperature = model_kernel_bipolar::Temperature::new(300.15)?;
    for line in reference.lines().filter(|line| !line.starts_with('#')) {
        let cells: Vec<&str> = line.split(',').collect();
        let [
            variant,
            form_text,
            _external_emitter,
            _external_collector,
            emitter_text,
            collector_text,
            current_text,
            transconductance_text,
            collector_conductance_text,
        ] = <[&str; 9]>::try_from(cells).map_err(|_| "invalid extended reference row")?;
        let mut params = default_depletion();
        match variant {
            "smooth" => {
                params.aje = 0.02;
                params.ajc = 0.03;
            }
            "punchthrough" => {
                params.vrt = 0.6;
            }
            "double_smooth" => {
                params.aje = 0.02;
                params.ajc = 0.03;
                params.vrt = 0.6;
                params.art = 0.04;
            }
            _ => return Err("unrecognized reference model family variant".into()),
        }
        let form = if form_text == "0" {
            transport::BaseChargeForm::Coupled
        } else {
            transport::BaseChargeForm::Factorized
        };
        let kernel = transport::Prepared::new(transport_params(form), temperature)?;
        let charges = depletion::Prepared::new(params)?;
        let emitter: f64 = emitter_text.parse()?;
        let collector: f64 = collector_text.parse()?;
        let current: f64 = current_text.parse()?;
        let transconductance: f64 = transconductance_text.parse()?;
        let collector_conductance: f64 = collector_conductance_text.parse()?;
        let out = coupled(&kernel, &charges, emitter, collector)?;
        close(out.net.value, current, 2e-5, 3e-16);
        close(out.net.d_vbei, transconductance, 2e-5, 3e-13);
        close(out.net.d_vbci, collector_conductance, 2e-5, 3e-13);
    }
    Ok(())
}
