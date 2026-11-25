use model_kernel_bipolar::{EvaluationError, ModelError, Temperature, vbic};
use vbic::{charge, depletion, parasitic, transport};

type TestResult = Result<(), Box<dyn std::error::Error>>;
type ChargeAxis = (fn(&mut charge::Bias, f64), fn(charge::Output) -> f64, f64);

fn close(actual: f64, expected: f64, relative: f64, absolute: f64) {
    let tolerance = relative.mul_add(actual.abs().max(expected.abs()), absolute);
    assert!(
        (actual - expected).abs() <= tolerance,
        "actual={actual:.15e}; expected={expected:.15e}; tolerance={tolerance:.3e}"
    );
}

const fn parameters(form: transport::BaseChargeForm) -> charge::Parameters {
    charge::Parameters {
        transport: transport::Parameters {
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
        },
        parasitic: parasitic::Parameters {
            isp: 0.0,
            wsp: 0.5,
            nfp: 1.0,
            ikp: 0.0,
        },
        depletion: depletion::Parameters {
            fc: 0.8,
            pe: 0.92,
            me: 0.12,
            aje: 0.014,
            pc: 0.558,
            mc: 0.12,
            ajc: 0.009,
            vrt: 0.0,
            art: 0.1,
        },
        substrate: depletion::SubstrateParameters {
            ps: 0.75,
            ms: 0.33,
            ajs: -0.5,
        },
        cje: 3e-15,
        cjc: 2e-15,
        cjep: 0.0,
        cjcp: 0.0,
        ccso: 0.0,
        cbeo: 0.0,
        cbco: 0.0,
        qco: 8e-15,
        wbe: 1.0,
        tf: 8e-12,
        qtf: 0.25,
        xtf: 0.45,
        vtf: 0.6,
        itf: 0.02,
        tr: 1.3e-12,
        gamm: 0.13,
    }
}

const fn bias(emitter: f64, collector: f64) -> charge::Bias {
    charge::Bias {
        vbei: emitter,
        vbex: emitter,
        vbci: collector,
        vbcx: collector,
        vbep: 0.0,
        vbcp: 0.0,
        vbe: emitter,
        vbc: collector,
    }
}

#[test]
fn all_vbic_physical_charge_partials_are_conservative() -> TestResult {
    let temperature = Temperature::new(300.15)?;
    for selector in [
        transport::BaseChargeForm::Coupled,
        transport::BaseChargeForm::Factorized,
    ] {
        let mut p = parameters(selector);
        p.wbe = 0.3;
        p.cjep = 4e-15;
        p.cjcp = 5e-15;
        p.ccso = 2e-15;
        p.cbeo = 1e-15;
        p.cbco = 0.4e-15;
        p.parasitic.isp = 2e-16;
        p.parasitic.wsp = 0.25;
        p.parasitic.ikp = 0.003;
        p.substrate.ajs = 0.013;
        p.depletion.vrt = 0.5;
        p.depletion.art = 0.02;
        let kernel = charge::Prepared::new(&p, temperature)?;
        for emitter in [-0.4, 0.0, 0.4, 0.65, 0.85, 0.95] {
            for collector in [-0.8, -0.3, 0.0, 0.4] {
                let mut input = bias(emitter, collector);
                input.vbex = emitter - 0.11;
                input.vbcx = collector - 0.07;
                input.vbep = emitter - 0.17;
                input.vbcp = collector - 0.21;
                let q = kernel.evaluate(input)?;
                let h = 1e-6;
                let approximate = |modify: fn(&mut charge::Bias, f64),
                                   take: fn(charge::Output) -> f64|
                 -> Result<f64, EvaluationError> {
                    let mut above = input;
                    let mut below = input;
                    modify(&mut above, h);
                    modify(&mut below, -h);
                    Ok((take(kernel.evaluate(above)?) - take(kernel.evaluate(below)?)) / (2.0 * h))
                };
                let cases: &[ChargeAxis] = &[
                    (|b, v| b.vbei += v, |q| q.qbe.coulombs, q.qbe.primary_farads),
                    (|b, v| b.vbci += v, |q| q.qbe.coulombs, q.qbe.coupled_farads),
                    (|b, v| b.vbex += v, |q| q.qbex.coulombs, q.qbex.farads),
                    (|b, v| b.vbci += v, |q| q.qbc.coulombs, q.qbc.farads),
                    (|b, v| b.vbcx += v, |q| q.qbcx.coulombs, q.qbcx.farads),
                    (
                        |b, v| b.vbep += v,
                        |q| q.qbep.coulombs,
                        q.qbep.primary_farads,
                    ),
                    (
                        |b, v| b.vbci += v,
                        |q| q.qbep.coulombs,
                        q.qbep.coupled_farads,
                    ),
                    (|b, v| b.vbcp += v, |q| q.qbcp.coulombs, q.qbcp.farads),
                    (|b, v| b.vbe += v, |q| q.qbeo.coulombs, q.qbeo.farads),
                    (|b, v| b.vbc += v, |q| q.qbco.coulombs, q.qbco.farads),
                ];
                for (modify, take, expected) in cases {
                    close(approximate(*modify, *take)?, *expected, 8e-6, 3e-18);
                }
            }
        }
    }
    Ok(())
}

#[test]
fn vbic_charge_zero_and_disabled_paths_preserve_physics() -> TestResult {
    let temperature = Temperature::new(300.15)?;
    let mut p = parameters(transport::BaseChargeForm::Coupled);
    p.cje = 0.0;
    p.cjc = 0.0;
    p.cjep = 0.0;
    p.cjcp = 0.0;
    p.ccso = 0.0;
    p.cbeo = 0.0;
    p.cbco = 0.0;
    p.qco = 0.0;
    p.tf = 0.0;
    p.tr = 0.0;
    let kernel = charge::Prepared::new(&p, temperature)?;
    assert_eq!(kernel.evaluate(bias(0.8, -0.5))?, charge::Output::default());

    p.tf = 3e-12;
    p.qtf = -0.5;
    p.xtf = 0.0;
    let kernel = charge::Prepared::new(&p, temperature)?;
    assert!(kernel.evaluate(bias(0.8, -0.5))?.qbe.coulombs > 0.0);

    p.wbe = 1.1;
    assert!(matches!(
        charge::Prepared::new(&p, temperature),
        Err(ModelError::InvalidParameter("wbe"))
    ));
    p.wbe = 0.5;
    p.qco = f64::NAN;
    assert!(matches!(
        charge::Prepared::new(&p, temperature),
        Err(ModelError::InvalidParameter("qco"))
    ));
    Ok(())
}

#[test]
fn vbic_charge_homogeneous_batch_and_invalid_bias() -> TestResult {
    let kernel = charge::Prepared::new(
        &parameters(transport::BaseChargeForm::Factorized),
        Temperature::new(300.15)?,
    )?;
    let inputs = [bias(0.75, -0.4), bias(0.8, 0.15), bias(-0.2, -0.5)];
    let mut output = [charge::Output::default(); 3];
    kernel.evaluate_batch(&inputs, &mut output)?;
    for (input, actual) in inputs.iter().zip(output) {
        assert_eq!(kernel.evaluate(*input)?, actual);
    }
    assert!(matches!(
        kernel.evaluate_batch(&inputs, &mut output[..2]),
        Err(EvaluationError::LengthMismatch)
    ));
    let mut invalid = inputs[0];
    invalid.vbcx = f64::NAN;
    assert!(matches!(
        kernel.evaluate(invalid),
        Err(EvaluationError::NonFiniteBias)
    ));
    Ok(())
}

#[test]
fn vbic_physical_charge_and_incremental_capacitance_match_ngspice_47() -> TestResult {
    // Independent ngspice-47 .op oracle: see neighboring reproducible script.
    // Device VBE/VBC from @q1 intrinsic states differ from terminal voltage at
    // high current, so use them rather than source inputs. The output charge
    // and incremental capacitance are reported by @q1[qbe/qbc/cbe/cbc].
    let csv = include_str!("fixtures/vbic_ngspice47_charge.csv");
    let temperature = Temperature::new(300.15)?;
    let mut count = 0usize;
    for line in csv.lines().filter(|line| !line.starts_with('#')) {
        let [
            variant,
            qbm_text,
            _external_emitter,
            _external_collector,
            emitter_text,
            collector_text,
            emitter_charge_reference,
            collector_charge_reference,
            capacitance_emitter_text,
            capacitance_collector_text,
        ] = <[&str; 10]>::try_from(line.split(',').collect::<Vec<_>>())
            .map_err(|_| "invalid VBIC charge oracle CSV row")?;
        let form = match qbm_text {
            "0" => transport::BaseChargeForm::Coupled,
            "1" => transport::BaseChargeForm::Factorized,
            _ => return Err("unrecognized QBM selector in oracle".into()),
        };
        let mut p = parameters(form);
        match variant {
            "full" => {}
            "knee_disabled" => p.itf = 0.0,
            "no_modulation" => {
                p.qtf = 0.0;
                p.xtf = 0.0;
                p.vtf = 0.0;
                p.itf = 0.0;
                p.qco = 0.0;
            }
            _ => return Err("unrecognized charge oracle variant".into()),
        }
        let prepared = charge::Prepared::new(&p, temperature)?;
        let emitter: f64 = emitter_text.parse()?;
        let collector: f64 = collector_text.parse()?;
        let measured = prepared.evaluate(bias(emitter, collector))?;
        close(
            measured.qbe.coulombs,
            emitter_charge_reference.parse()?,
            3e-5,
            1e-20,
        );
        close(
            measured.qbc.coulombs,
            collector_charge_reference.parse()?,
            3e-5,
            1e-20,
        );
        close(
            measured.qbe.primary_farads,
            capacitance_emitter_text.parse()?,
            3e-5,
            1e-20,
        );
        close(
            measured.qbc.farads,
            capacitance_collector_text.parse()?,
            3e-5,
            1e-20,
        );
        count += 1;
    }
    assert_eq!(count, 36);
    Ok(())
}

#[test]
fn epitaxial_zero_gamma_has_finite_exact_constant_charge() -> TestResult {
    let temperature = Temperature::new(300.15)?;
    let mut model = parameters(transport::BaseChargeForm::Coupled);
    model.gamm = 0.0;
    model.cjc = 0.0;
    model.tr = 0.0;
    model.tf = 0.0;
    let prepared = charge::Prepared::new(&model, temperature)?;
    let actual = prepared.evaluate(bias(0.2, -30.0))?;
    assert_eq!(actual.qbc.coulombs, model.qco);
    assert_eq!(actual.qbc.farads, 0.0);
    assert_eq!(actual.qbcx.coulombs, model.qco);
    assert_eq!(actual.qbcx.farads, 0.0);
    Ok(())
}
