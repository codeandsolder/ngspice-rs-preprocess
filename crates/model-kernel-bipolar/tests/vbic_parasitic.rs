use model_kernel_bipolar::{EvaluationError, ModelError, Temperature, vbic::parasitic};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn close(actual: f64, expected: f64, relative: f64, absolute: f64) {
    assert!(
        (actual - expected).abs() <= relative.mul_add(actual.abs().max(expected.abs()), absolute),
        "actual {actual:.15e}; expected {expected:.15e}"
    );
}

const fn parameters(isp: f64, wsp: f64, ikp: f64) -> parasitic::Parameters {
    parasitic::Parameters {
        isp,
        wsp,
        nfp: 1.04,
        ikp,
    }
}

#[test]
fn parasitic_transfer_matches_three_axis_differentials() -> TestResult {
    let temperature = Temperature::new(300.15)?;
    for weight in [0.0, 0.3, 0.75, 1.0] {
        for knee in [0.0, 1e-4] {
            let kernel = parasitic::Prepared::new(parameters(4e-16, weight, knee), temperature)?;
            for (peripheral, intrinsic, substrate) in [
                (-0.5, -0.6, -0.3),
                (0.0, 0.0, 0.0),
                (0.3, -0.4, -0.3),
                (0.5, -0.2, 0.1),
                (0.8, 0.2, -0.15),
                (0.8, 0.8, -0.15),
            ] {
                let bias = parasitic::Bias {
                    vbep: peripheral,
                    vbci: intrinsic,
                    vbcp: substrate,
                };
                let actual = kernel.evaluate(bias)?;
                let delta = 1e-7;
                let finite_diff =
                    |change: fn(&mut parasitic::Bias, f64)| -> Result<f64, EvaluationError> {
                        let mut positive = bias;
                        let mut negative = bias;
                        change(&mut positive, delta);
                        change(&mut negative, -delta);
                        Ok(
                            (kernel.evaluate(positive)?.iccp - kernel.evaluate(negative)?.iccp)
                                / (2.0 * delta),
                        )
                    };
                let emitter = finite_diff(|input, change| input.vbep += change)?;
                let collector = finite_diff(|input, change| input.vbci += change)?;
                let reverse = finite_diff(|input, change| input.vbcp += change)?;
                close(actual.d_vbep, emitter, 3e-7, 1e-12);
                close(actual.d_vbci, collector, 3e-7, 1e-12);
                close(actual.d_vbcp, reverse, 3e-7, 1e-12);
                if knee == 0.0 {
                    assert_eq!(actual.qb, 1.0);
                } else {
                    assert!(actual.qb > 0.0);
                }
            }
        }
    }
    Ok(())
}

#[test]
fn parasitic_three_junction_oracle_matches_ngspice_47() -> TestResult {
    // Full 5-terminal native ngspice 47 .op with the same model:
    // Q1 c b 0 0 0 PARASITIC, VBE=0.95, VCE=0.15;
    // IS=0, ISP=4e-16, NFP=1.04, WSP=.3, IKP=1e-4,
    // IB* parasitic and intrinsic diode currents disabled, no CJE/CJC.
    // Reference @q1[is]=5.262917397920385e-04 A.
    // Local Vbep/Vbcp are inferred from the terminal bias: ngspice has
    // residual 0.01-ohm series resistances so only a bounded comparison is
    // justified here. Intrinsic @q1[vbc] is measured independently.
    let kernel = parasitic::Prepared::new(parameters(4e-16, 0.3, 1e-4), Temperature::new(300.15)?)?;
    let result = kernel.evaluate(parasitic::Bias {
        vbep: 0.8,
        vbci: 0.799_994_737_082_601_7,
        vbcp: -0.15,
    })?;
    close(result.iccp, 5.262_917_397_920_385e-4, 5e-4, 1e-12);
    Ok(())
}

#[test]
fn parasitic_input_validation_and_batch_are_stable() -> TestResult {
    let temperature = Temperature::new(300.15)?;
    for value in [f64::NAN, f64::INFINITY, -0.1, 1.1] {
        assert!(matches!(
            parasitic::Prepared::new(parameters(1e-16, value, 0.0), temperature),
            Err(ModelError::InvalidParameter("wsp"))
        ));
    }
    let disabled = parasitic::Prepared::new(parameters(0.0, 0.3, 0.0), temperature)?;
    let input = parasitic::Bias {
        vbep: 0.6,
        vbci: -0.4,
        vbcp: -0.2,
    };
    assert_eq!(
        disabled.evaluate(input)?,
        parasitic::Output {
            qb: 1.0,
            ..parasitic::Output::default()
        }
    );
    let kernel = parasitic::Prepared::new(parameters(4e-16, 0.5, 1e-4), temperature)?;
    let inputs = [input; 3];
    let mut outputs = [parasitic::Output::default(); 3];
    kernel.evaluate_batch(&inputs, &mut outputs)?;
    for out in outputs {
        assert_eq!(out, kernel.evaluate(input)?);
    }
    assert!(matches!(
        kernel.evaluate_batch(&inputs, &mut outputs[..1]),
        Err(EvaluationError::LengthMismatch)
    ));
    Ok(())
}
