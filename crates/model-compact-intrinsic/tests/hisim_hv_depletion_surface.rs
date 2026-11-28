use model_compact_intrinsic::{
    HiSimHvDepletionSurfaceInputs, HiSimHvDepletionSurfacePoint, HiSimHvIntrinsicScalar,
    ModelError, evaluate_hisim_hv_depletion_surface,
};
type TestResult = Result<(), Box<dyn std::error::Error>>;
type Jet = HiSimHvIntrinsicScalar;
fn evaluate(row: &[f64; 8], offset: [f64; 4]) -> Result<HiSimHvDepletionSurfacePoint, ModelError> {
    let state = |value: f64, gradient: [f64; 4]| Jet {
        value: value + gradient.iter().zip(offset).map(|(g, x)| g * x).sum::<f64>(),
        gradient,
    };
    evaluate_hisim_hv_depletion_surface(HiSimHvDepletionSurfaceInputs {
        effective_gate: state(row[0], [1., 0.07, -0.05, 0.0002]),
        confined_body: state(row[1], [0., 0.01, 0.94, -0.0001]),
        quasi_fermi: state(0., [0.; 4]),
        inverse_thermal_voltage: state(row[2], [0., 0., 0., -0.1]),
        oxide_capacitance: state(row[3], [0.00001, 0.00002, 0.00003, -0.000001]),
        intrinsic_carrier_density: state(row[6], [0., 0., 0., row[6] * 0.03]),
        depletion_doping: row[4],
        substrate_doping: row[5],
        layer_thickness_m: row[7],
    })
}
fn outputs(p: HiSimHvDepletionSurfacePoint) -> [Jet; 6] {
    [
        p.surface,
        p.buried,
        p.junction,
        p.surface_charge_density,
        p.buried_charge_density,
        p.substrate_charge_density,
    ]
}
#[test]
fn native_depletion_surface() -> TestResult {
    let mut failures = [0; 6];
    let mut errors = 0;
    let mut count = 0;
    for (index, line) in include_str!("fixtures/hisim-hv-depletion-surface-ngspice47.txt")
        .lines()
        .enumerate()
    {
        if line.starts_with('#') {
            continue;
        }
        let row: Vec<f64> = line
            .split_whitespace()
            .map(str::parse)
            .collect::<Result<_, _>>()?;
        let inputs: [f64; 8] = row[..8].try_into()?;
        count += 1;
        let p = match evaluate(&inputs, [0.; 4]) {
            Ok(p) => p,
            Err(e) => {
                errors += 1;
                if errors <= 20 {
                    eprintln!("row {index}: {e}: {inputs:?}");
                }
                continue;
            }
        };
        for (i, p) in outputs(p).into_iter().enumerate() {
            let expected = row[9 + i];
            let tol = 2e-8 * expected.abs().max(p.value.abs()) + if i < 3 { 2e-10 } else { 2e-12 };
            if (p.value - expected).abs() > tol {
                failures[i] += 1;
                if failures[i] <= 15 {
                    eprintln!(
                        "row {index} field {i} native {expected:e} Rust {:e} inputs {inputs:?}",
                        p.value
                    );
                }
            }
        }
    }
    assert_eq!(count, 3238);
    assert_eq!(errors, 0);
    assert_eq!(failures, [0; 6]);
    Ok(())
}
#[test]
fn independent_depletion_surface_jacobian() -> TestResult {
    for gate in [-6., -2., -0.4, 0.3, 1.5, 5.] {
        for thickness in [1e-7, 2e-7, 5e-7] {
            for body in [-0.2, -1.2] {
                let row = [gate, body, 38., 0.003, 1e23, 5e22, 1e16, thickness];
                let p = outputs(evaluate(&row, [0.; 4])?);
                for axis in 0..4 {
                    let step = if axis == 3 { 1e-3 } else { 1e-5 };
                    let ev = |n: f64| {
                        let mut offset = [0.; 4];
                        offset[axis] = n * step;
                        evaluate(&row, offset).map(outputs)
                    };
                    let a = ev(-2.)?;
                    let b = ev(-1.)?;
                    let c = ev(1.)?;
                    let d = ev(2.)?;
                    for i in 0..6 {
                        let fd = ((a[i].value - d[i].value) + 8. * (c[i].value - b[i].value))
                            / (12. * step);
                        let exact = p[i].gradient[axis];
                        let budget = 2e-5 * fd.abs().max(exact.abs())
                            + 64. * f64::EPSILON * p[i].value.abs() / step
                            + if i < 3 { 1e-8 } else { 1e-10 };
                        assert!(
                            (fd - exact).abs() <= budget,
                            "gate {gate} body {body} thickness {thickness:e} axis {axis} output {i}: FD {fd:e}, AD {exact:e}"
                        );
                    }
                }
            }
        }
    }
    Ok(())
}
