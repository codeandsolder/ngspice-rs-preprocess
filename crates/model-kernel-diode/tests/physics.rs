//! Independent analytic derivative and source-parameter contracts.
use model_kernel_diode::{Instance, Model, compile_simetrix, evaluate_batch};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn close(actual: f64, expected: f64, absolute: f64, relative: f64) -> Result<()> {
    if (actual - expected).abs() <= relative.mul_add(expected.abs(), absolute) {
        Ok(())
    } else {
        Err(format!("{actual:e} != {expected:e}").into())
    }
}

#[test]
fn measured_temperature_precedence_and_aliases() -> Result<()> {
    let mut count = 0;
    for row in include_str!("fixtures/temperature-policy.tsv")
        .lines()
        .skip(1)
    {
        let fields: [&str; 9] = row
            .split('\t')
            .collect::<Vec<_>>()
            .try_into()
            .map_err(|_| "temperature fixture width")?;
        let [
            name,
            tnom,
            model_temp,
            model_delta,
            analysis,
            instance_temp,
            instance_delta,
            effective,
            current,
        ] = fields;
        let mut params = vec![
            ("IS", 1e-12),
            ("N", 1.0),
            ("T_REL_GLOBAL", model_delta.parse()?),
            (
                if name == "measured" {
                    "T_MEASURED"
                } else {
                    "TNOM"
                },
                tnom.parse()?,
            ),
        ];
        if model_temp != "-" {
            params.push(("T_ABS", model_temp.parse()?));
        }
        let model = compile_simetrix(params)?;
        let instance = Instance {
            analysis_temperature: analysis.parse::<f64>()? + 273.15,
            absolute_temperature: if instance_temp == "-" {
                None
            } else {
                Some(instance_temp.parse::<f64>()? + 273.15)
            },
            relative_temperature: instance_delta.parse()?,
            gmin: 1e-15,
            ..Instance::default()
        };
        let device = model.bind(instance)?;
        close(
            device.temperature(),
            effective.parse::<f64>()? + 273.15,
            1e-12,
            0.0,
        )?;
        close(device.evaluate(0.4)?.current, current.parse()?, 2e-18, 1e-9)?;
        count += 1;
    }
    assert_eq!(count, 20);
    Ok(())
}

#[test]
fn exact_junction_derivatives_and_continuity() -> Result<()> {
    // Both sides of FC and V=0, reverse avalanche, high injection,
    // recombination and the logarithmic depletion charge limit.
    for grading in [0.0, 0.4, 1.0 - 1e-12, 1.0, 1.0 + 1e-12, 1.2] {
        let model = compile_simetrix([
            ("IS", 1e-12),
            ("N", 1.3),
            ("ISR", 2e-8),
            ("NR", 2.0),
            ("IKF", 1e-4),
            ("BV", 4.0),
            ("NBV", 1.4),
            ("IBV", 1e-5),
            ("IBVL", 1e-7),
            ("NBVL", 2.2),
            ("CJO", 1e-9),
            ("VJ", 0.7),
            ("M", grading),
            ("FC", 0.5),
            ("TT", 20e-9),
        ])?;
        let device = model.bind(Instance {
            area: 2.0,
            multiplier: 3.0,
            gmin: 1e-12,
            ..Instance::default()
        })?;
        for voltage in [
            -4.1, -4.0, -3.6, -1.0, -0.1, 0.0, 0.1, 0.3499, 0.35, 0.3501, 0.6, 0.8,
        ] {
            let step = 1e-6;
            let a = device.evaluate(voltage - step)?;
            let b = device.evaluate(voltage + step)?;
            let c = device.evaluate(voltage)?;
            close(
                (b.current - a.current) / (2.0 * step),
                c.conductance,
                1e-12,
                2e-6,
            )?;
            close(
                (b.charge - a.charge) / (2.0 * step),
                c.capacitance,
                1e-15,
                2e-6,
            )?;
        }
    }
    Ok(())
}

#[test]
fn stable_extreme_bias_and_small_voltage() -> Result<()> {
    // Raw exp(800) overflows, but the scaled and injection-corrected
    // transport law is finite. No overflow-driven current clipping.
    let model = compile_simetrix([("IS", 1e-300), ("IKF", 1e-100), ("N", 1.0)])?;
    let device = model.bind(Instance {
        gmin: 0.0,
        ..Instance::default()
    })?;
    let v = 800.0 * model_kernel_diode::K_OVER_Q * 300.15;
    let output = device.evaluate(v)?;
    let expected = (0.5 * (800.0 + 1e-300_f64.ln() + 1e-100_f64.ln())).exp();
    close(output.current, expected, 0.0, 1e-12)?;
    close(
        output.conductance,
        expected / (2.0 * model_kernel_diode::K_OVER_Q * 300.15),
        0.0,
        1e-12,
    )?;
    let model = compile_simetrix([("IS", 1e-12)])?;
    let device = model.bind(Instance {
        gmin: 0.0,
        ..Instance::default()
    })?;
    let output = device.evaluate(1e-20)?;
    close(output.current, output.conductance * 1e-20, 0.0, 1e-12)?;
    Ok(())
}

#[test]
fn source_defaults_and_disabled_groups() -> Result<()> {
    let base = Model::default().prepare()?.bind(Instance::default())?;
    let disabled = compile_simetrix([("IKF", 0.0), ("BV", 0.0)])?.bind(Instance::default())?;
    for bias in [-5.0, -0.5, 0.0, 0.5] {
        close(
            disabled.evaluate(bias)?.current,
            base.evaluate(bias)?.current,
            0.0,
            0.0,
        )?;
    }
    let zero = compile_simetrix([("IS", 0.0)])?.bind(Instance {
        gmin: 1e-12,
        multiplier: 2.0,
        ..Instance::default()
    })?;
    close(zero.evaluate(0.5)?.current, 1e-12, 0.0, 0.0)?;
    close(zero.evaluate(0.5)?.capacitance, 0.0, 0.0, 0.0)?;
    let series = compile_simetrix([("RS", -1.0)])?.bind(Instance {
        area: 2.0,
        multiplier: 3.0,
        ..Instance::default()
    })?;
    close(series.series_resistance(), -1.0 / 6.0, 0.0, 0.0)?;
    assert!(compile_simetrix([("LEVEL", 3.0)]).is_err());
    assert!(compile_simetrix([("TLEV", 1.0)]).is_err());
    assert!(compile_simetrix([("NR", 0.0)]).is_err());
    assert!(compile_simetrix([("FC", 1.0)]).is_err());
    assert!(base.evaluate(f64::NAN).is_err());
    Ok(())
}

#[test]
fn homogeneous_batch_matches_scalar_and_rejects_shape_mismatch() -> Result<()> {
    let instance = compile_simetrix([
        ("IS", 1e-12),
        ("ISR", 1e-8),
        ("CJO", 100e-12),
        ("TT", 20e-9),
    ])?
    .bind(Instance::default())?;
    let biases = [-1.0, 0.0, 0.4, 0.7];
    let mut output = [model_kernel_diode::Evaluation::default(); 4];
    evaluate_batch(&instance, &biases, &mut output)?;
    for (bias, actual) in biases.into_iter().zip(output) {
        let expected = instance.evaluate(bias)?;
        for (a, b) in [
            (actual.current, expected.current),
            (actual.charge, expected.charge),
            (actual.conductance, expected.conductance),
            (actual.capacitance, expected.capacitance),
        ] {
            close(a, b, 0.0, 0.0)?;
        }
    }
    assert!(evaluate_batch(&instance, &biases, &mut output[..3]).is_err());
    Ok(())
}
