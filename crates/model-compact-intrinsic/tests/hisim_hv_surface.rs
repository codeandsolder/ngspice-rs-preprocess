//! Independent source-extracted Poisson residual and finite-difference oracle.
use model_compact_intrinsic::{
    HiSimHvIntrinsicScalar, HiSimHvSurfaceInputs, evaluate_hisim_hv_surface,
};
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn near(actual: f64, expected: f64, rel: f64, abs: f64, context: impl std::fmt::Debug) {
    assert!(
        (actual - expected).abs() <= rel.mul_add(actual.abs().max(expected.abs()), abs),
        "{context:?}: {actual:e} vs {expected:e}"
    );
}
fn inputs(values: [f64; 6], drain: bool) -> HiSimHvSurfaceInputs {
    let [gate, body, qf, beta, fac1, carrier] = values;
    HiSimHvSurfaceInputs {
        effective_gate: HiSimHvIntrinsicScalar {
            value: gate,
            gradient: [1., 0.04, -0.03, 0.0002],
        },
        confined_body: HiSimHvIntrinsicScalar {
            value: body,
            gradient: [0., 0.015, 0.91, -0.00015],
        },
        quasi_fermi: HiSimHvIntrinsicScalar {
            value: qf,
            gradient: if drain { [0., 0.8, 0.02, 0.] } else { [0.; 4] },
        },
        inverse_thermal_voltage: HiSimHvIntrinsicScalar {
            value: beta,
            gradient: [0., 0., 0., -0.13],
        },
        charge_factor: HiSimHvIntrinsicScalar {
            value: fac1,
            gradient: [0.001, 0.0008, 0.0012, 0.00003],
        },
        minority_coefficient: HiSimHvIntrinsicScalar {
            value: carrier,
            gradient: [0., 0., 0., carrier * 0.025],
        },
    }
}
#[test]
fn native_source_and_drain_poisson_root_and_jacobian() -> TestResult {
    let fixture = include_str!("fixtures/hisim-hv-surface-ngspice47.txt");
    let mut count = 0;
    for (index, line) in fixture
        .lines()
        .filter(|line| !line.starts_with('#'))
        .enumerate()
    {
        let fields: [f64; 34] = line
            .split_whitespace()
            .map(str::parse::<f64>)
            .collect::<Result<Vec<_>, _>>()?
            .try_into()
            .map_err(|_| "surface reference record width")?;
        let state = inputs(
            [
                fields[8], fields[3], fields[4], fields[5], fields[6], fields[7],
            ],
            fields[1] != 0.,
        );
        let point = evaluate_hisim_hv_surface(&state)
            .map_err(|error| format!("case {index} {state:?}: {error:?}"))?;
        let actual = [
            point.potential,
            point.chi,
            point.depletion_root,
            point.inversion_squared,
            point.total_root,
        ];
        for (field, scalar) in actual.into_iter().enumerate() {
            near(
                scalar.value,
                fields[9 + field],
                3e-10,
                1e-14,
                (index, field, "value"),
            );
            for (axis, gradient) in scalar.gradient.into_iter().enumerate() {
                near(
                    gradient,
                    fields[14 + axis * 5 + field],
                    4e-5,
                    2e-8,
                    (index, field, axis),
                );
            }
        }
        // Independent root residual, including the correct polynomial region.
        near(
            state.effective_gate.value,
            state
                .charge_factor
                .value
                .mul_add(point.total_root.value, point.potential.value),
            1e-12,
            1e-13,
            (index, "residual"),
        );
        count += 1;
    }
    assert_eq!(count, 960);
    Ok(())
}
#[test]
fn flatband_limit_and_invalid_inputs() -> TestResult {
    let state = inputs([-0.5, -0.5, 0., 40., 0.2, 1e-13], false);
    let point = evaluate_hisim_hv_surface(&state)?;
    assert_eq!(point.potential.value, -0.5);
    assert_eq!(point.chi.value, 0.);
    assert_eq!(point.total_root.value, 0.);
    assert_eq!(point.inversion_squared.value, 0.);
    // At flatband the exact one-sided derivative of the native polynomial is finite.
    let expected = (state.charge_factor.value * state.inverse_thermal_voltage.value)
        .mul_add(std::f64::consts::FRAC_1_SQRT_2, 1.)
        .recip();
    near(
        point.potential.gradient[0],
        expected,
        1e-14,
        0.,
        "flatband tangent",
    );
    let mut invalid = state;
    invalid.effective_gate.value -= 1e-6;
    assert!(evaluate_hisim_hv_surface(&invalid).is_err());
    invalid = state;
    invalid.minority_coefficient.value = f64::NAN;
    assert!(evaluate_hisim_hv_surface(&invalid).is_err());
    Ok(())
}
