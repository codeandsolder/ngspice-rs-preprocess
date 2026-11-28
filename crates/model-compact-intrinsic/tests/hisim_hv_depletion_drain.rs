use model_compact_intrinsic::{
    HiSimHvDepletionDrainInputs, HiSimHvDepletionDrainPoint, HiSimHvIntrinsicScalar, ModelError,
    evaluate_hisim_hv_depletion_drain,
};
type Jet = HiSimHvIntrinsicScalar;
type TestResult = Result<(), Box<dyn std::error::Error>>;
fn eval(row: &[f64; 10], offset: [f64; 4]) -> Result<HiSimHvDepletionDrainPoint, ModelError> {
    let state = |value: f64, gradient: [f64; 4]| Jet {
        value: value + gradient.iter().zip(offset).map(|(g, x)| g * x).sum::<f64>(),
        gradient,
    };
    evaluate_hisim_hv_depletion_drain(HiSimHvDepletionDrainInputs {
        gate: state(row[0], [1., 0.07, -0.05, 0.0002]),
        symmetric_body: state(row[1], [0., 0.01, 0.94, -0.0001]),
        drain: state(row[2], [0., 0.8, 0.02, 0.]),
        thermal_voltage: state(row[3], [0., 0., 0., 0.00008]),
        oxide_capacitance: state(row[4], [1e-5, 2e-5, 3e-5, -1e-6]),
        depletion_doping: row[5],
        smoothing_exponent: row[6],
        drift_gate_offset: row[7],
        drift_scale: row[8],
        drift_floor: row[9],
    })
}
#[test]
fn native_depletion_drain_biases() -> TestResult {
    let mut count = 0;
    let mut failures = [0; 2];
    let mut max_error = [0_f64; 2];
    for line in include_str!("fixtures/hisim-hv-depletion-drain-ngspice47.txt").lines() {
        if line.starts_with('#') {
            continue;
        }
        let row: Vec<f64> = line
            .split_whitespace()
            .map(str::parse)
            .collect::<Result<_, _>>()?;
        let p = eval(&row[..10].try_into()?, [0.; 4])?;
        for (i, p) in [p.channel, p.drift].into_iter().enumerate() {
            let expected = row[10 + i];
            let error = (p.value - expected).abs();
            if error > 1e-9 * expected.abs().max(p.value.abs()) + 2e-13 {
                failures[i] += 1;
                if failures[i] <= 5 || error > max_error[i] {
                    eprintln!(
                        "field {i}: native={expected:e} Rust={:e} inputs={:?}",
                        p.value,
                        &row[..10]
                    );
                }
                max_error[i] = max_error[i].max(error);
            }
        }
        count += 1;
    }
    assert_eq!(count, 11664);
    assert_eq!(failures, [0; 2], "maximum errors {max_error:?}");
    Ok(())
}
#[test]
fn independent_depletion_drain_jacobian() -> TestResult {
    for gate in [-8., -2., -0.1, 0., 0.3, 1., 5.] {
        for drain in [1e-5, 0.0009, 0.0011, 0.02, 0.5, 5., 15.] {
            for exponent in [1.2, 3., 8.] {
                let row = [
                    gate,
                    -0.7,
                    drain,
                    1. / 38.,
                    0.003,
                    1e23,
                    exponent,
                    2.,
                    0.5,
                    0.5,
                ];
                let p = eval(&row, [0.; 4])?;
                for axis in 0..4 {
                    let step = if axis == 3 { 1e-3 } else { 1e-6 };
                    let ev = |n: f64| {
                        let mut offset = [0.; 4];
                        offset[axis] = n * step;
                        eval(&row, offset)
                    };
                    let a = ev(-2.)?;
                    let b = ev(-1.)?;
                    let c = ev(1.)?;
                    let d = ev(2.)?;
                    for (i, (p, a, b, c, d)) in [
                        (p.channel, a.channel, b.channel, c.channel, d.channel),
                        (p.drift, a.drift, b.drift, c.drift, d.drift),
                    ]
                    .into_iter()
                    .enumerate()
                    {
                        let fd = ((a.value - d.value) + 8. * (c.value - b.value)) / (12. * step);
                        let exact = p.gradient[axis];
                        let budget = 2e-5 * fd.abs().max(exact.abs())
                            + 64. * f64::EPSILON * p.value.abs() / step
                            + 1e-9;
                        assert!(
                            (fd - exact).abs() <= budget,
                            "gate {gate} drain {drain} exponent {exponent} axis {axis} output {i}: FD {fd:e}, AD {exact:e}"
                        );
                    }
                }
            }
        }
    }
    Ok(())
}
