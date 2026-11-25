use model_kernel_bipolar::{Branch, EvaluationError, ModelError, Temperature, vbic};
use vbic::{avalanche, depletion, transport};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn close(actual: f64, expected: f64, relative: f64, absolute: f64) {
    let tolerance = relative.mul_add(actual.abs().max(expected.abs()), absolute);
    assert!(
        (actual - expected).abs() <= tolerance,
        "actual={actual:.15e}; expected={expected:.15e}; tolerance={tolerance:.3e}"
    );
}

const fn charge_params() -> depletion::Parameters {
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

const fn source_params(form: transport::BaseChargeForm) -> transport::Parameters {
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

const fn junction_params() -> vbic::Parameters {
    vbic::Parameters {
        ibei: 0.0,
        iben: 0.0,
        nei: 1.0,
        nen: 2.0,
        wbe: 1.0,
        ibci: 1e-16,
        ibcn: 0.0,
        nci: 1.01,
        ncn: 2.0,
        ibeip: 0.0,
        ibenp: 0.0,
        ibcip: 0.0,
        ibcnp: 0.0,
        ncip: 1.0,
        ncnp: 2.0,
        vbbe: 0.0,
        nbbe: 1.0,
        ibbe: 0.0,
    }
}

fn evaluate(
    prepared: &avalanche::Prepared,
    transport: &transport::Prepared,
    depletion: &depletion::Prepared,
    junction: &vbic::Prepared,
    emitter: f64,
    collector: f64,
) -> Result<avalanche::Output, EvaluationError> {
    let charges = depletion.evaluate(emitter, collector)?;
    let transfer = transport.evaluate(transport::Bias {
        vbei: emitter,
        vbci: collector,
        depletion: charges,
    })?;
    let diode = junction.evaluate(vbic::Bias {
        vbei: emitter,
        vbex: emitter,
        vbci: collector,
        vbep: 0.0,
        vbcp: 0.0,
    })?;
    prepared.evaluate(avalanche::Bias {
        vbci: collector,
        transport: transfer,
        ibcj: diode.ibcj,
    })
}

#[test]
fn avalanche_currents_and_gmu_match_ngspice_47() -> TestResult {
    // Native ngspice-47 isolated base-current oracle; see adjacent script.
    // No intrinsic B-E or parasitic diode current, so @q1[ib] is Ibc.
    // The model reports gmu = dIbc/dVbci. Internal biases (not source
    // voltages) are required to isolate intrinsic junction equations.
    // ngspice also injects an auxiliary gmin current into reported IB;
    // the oracle absolute floor of 5e-18 A covers that independently
    // modeled conductance when the physical diode is reverse saturated.
    let csv = include_str!("fixtures/vbic_ngspice47_avalanche.csv");
    let temperature = Temperature::new(300.15)?;
    let charges = depletion::Prepared::new(charge_params())?;
    let junction = vbic::Prepared::new(junction_params(), temperature)?;
    let mut count = 0;
    for line in csv.lines().filter(|line| !line.starts_with('#')) {
        let [
            variant,
            qbm,
            _external_emitter,
            _external_collector,
            emitter_text,
            collector_text,
            ib_text,
            gmu_text,
        ] = <[&str; 8]>::try_from(line.split(',').collect::<Vec<_>>())
            .map_err(|_| "invalid avalanche oracle row")?;
        let form = match qbm {
            "0" => transport::BaseChargeForm::Coupled,
            "1" => transport::BaseChargeForm::Factorized,
            _ => return Err("unrecognized QBM selector".into()),
        };
        let avc1 = match variant {
            "enabled" => 0.08,
            "disabled" => 0.0,
            _ => return Err("unrecognized impact-ionization option".into()),
        };
        let prepared = avalanche::Prepared::new(avalanche::Parameters {
            avc1,
            avc2: 0.15,
            pc: 0.75,
            mc: 0.33,
        })?;
        let source = transport::Prepared::new(source_params(form), temperature)?;
        let emitter: f64 = emitter_text.parse()?;
        let collector: f64 = collector_text.parse()?;
        let output = evaluate(&prepared, &source, &charges, &junction, emitter, collector)?;
        close(output.ibc, ib_text.parse()?, 2e-5, 5e-18);
        close(output.d_vbci, gmu_text.parse()?, 2e-5, 3e-18);
        count += 1;
    }
    assert_eq!(count, 24);
    Ok(())
}

#[test]
fn avalanche_two_axis_analytic_derivatives_match_finite_difference() -> TestResult {
    let temperature = Temperature::new(300.15)?;
    let source = transport::Prepared::new(
        source_params(transport::BaseChargeForm::Coupled),
        temperature,
    )?;
    let junction = vbic::Prepared::new(junction_params(), temperature)?;
    let charges = depletion::Prepared::new(charge_params())?;
    let prepared = avalanche::Prepared::new(avalanche::Parameters {
        avc1: 0.08,
        avc2: 0.15,
        pc: 0.75,
        mc: 0.33,
    })?;
    for emitter in [-0.3, 0.0, 0.5, 0.65, 0.85] {
        for collector in [-2.5, -1.5, -0.6, 0.1, 0.7] {
            let output = evaluate(&prepared, &source, &charges, &junction, emitter, collector)?;
            let step = 1e-6;
            let emitter_slope = (evaluate(
                &prepared,
                &source,
                &charges,
                &junction,
                emitter + step,
                collector,
            )?
            .ibc - evaluate(
                &prepared,
                &source,
                &charges,
                &junction,
                emitter - step,
                collector,
            )?
            .ibc)
                / (2.0 * step);
            let collector_slope = (evaluate(
                &prepared,
                &source,
                &charges,
                &junction,
                emitter,
                collector + step,
            )?
            .ibc - evaluate(
                &prepared,
                &source,
                &charges,
                &junction,
                emitter,
                collector - step,
            )?
            .ibc)
                / (2.0 * step);
            close(output.d_vbei, emitter_slope, 2e-5, 5e-13);
            close(output.d_vbci, collector_slope, 2e-5, 5e-13);
        }
    }
    Ok(())
}

#[test]
fn avalanche_disabled_is_exact_original_diode_and_batch_validates() -> TestResult {
    let prepared = avalanche::Prepared::new(avalanche::Parameters {
        avc1: 0.0,
        avc2: 0.15,
        pc: 0.75,
        mc: 0.33,
    })?;
    let diode = Branch {
        amperes: -1e-16,
        siemens: 3e-7,
    };
    let mut transport = transport::Output::default();
    transport.net.value = 3e-5;
    transport.net.d_vbei = 2e-4;
    let bias = avalanche::Bias {
        vbci: -0.5,
        transport,
        ibcj: diode,
    };
    assert_eq!(
        prepared.evaluate(bias)?,
        avalanche::Output {
            ibc: diode.amperes,
            d_vbci: diode.siemens,
            ..avalanche::Output::default()
        }
    );
    let input = [bias; 3];
    let mut outputs = [avalanche::Output::default(); 3];
    prepared.evaluate_batch(&input, &mut outputs)?;
    assert!(outputs.iter().all(|output| *output == outputs[0]));
    assert!(matches!(
        prepared.evaluate_batch(&input, &mut outputs[..2]),
        Err(EvaluationError::LengthMismatch)
    ));
    assert!(matches!(
        avalanche::Prepared::new(avalanche::Parameters {
            avc1: -0.2,
            avc2: 0.1,
            pc: 0.75,
            mc: 0.33,
        }),
        Err(ModelError::InvalidParameter("avc1"))
    ));
    Ok(())
}
