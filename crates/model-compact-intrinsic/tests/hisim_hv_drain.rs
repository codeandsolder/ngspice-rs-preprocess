//! Independent source-extracted effective drain saturation reference.
use model_compact_intrinsic::{
    HiSimHvDrainSmoothingInputs, HiSimHvIntrinsicScalar, ModelVariant,
    evaluate_hisim_hv_drain_smoothing,
};
type TestResult = Result<(), Box<dyn std::error::Error>>;
const GRADIENTS: [[f64; 4]; 5] = [
    [0., 1., 0., 0.],
    [1., 0.04, -0.03, 0.0002],
    [0., 0.015, 0.91, -0.00015],
    [0., 0., 0., 8.617e-5],
    [0.0002, 0.00003, 0.00002, 0.],
];
fn close(actual: f64, expected: f64, rel: f64, abs: f64, context: impl std::fmt::Debug) {
    assert!(
        (actual - expected).abs() <= rel.mul_add(actual.abs().max(expected.abs()), abs),
        "{context:?}: {actual:e} vs {expected:e}"
    );
}
#[test]
fn native_drain_smoothing_values_and_gradients() -> TestResult {
    let mut count = 0;
    for (index, line) in include_str!("fixtures/hisim-hv-drain-ngspice47.txt")
        .lines()
        .filter(|line| !line.starts_with('#'))
        .enumerate()
    {
        let fields: [f64; 18] = line
            .split_whitespace()
            .map(str::parse::<f64>)
            .collect::<Result<Vec<_>, _>>()?
            .try_into()
            .map_err(|_| "drain fixture record width")?;
        let variant = match fields[0] {
            1. => ModelVariant::HiSimHv1,
            2. => ModelVariant::HiSimHv2,
            _ => return Err("drain reference family".into()),
        };
        let scalars: [HiSimHvIntrinsicScalar; 5] =
            std::array::from_fn(|i| HiSimHvIntrinsicScalar {
                value: fields[i + 1],
                gradient: GRADIENTS[i],
            });
        let input = HiSimHvDrainSmoothingInputs {
            drain: scalars[0],
            effective_gate: scalars[1],
            modified_body: scalars[2],
            thermal_voltage: scalars[3],
            oxide_capacitance: scalars[4],
            charge_density_permittivity: fields[6],
            exponent: fields[7],
        };
        let point = evaluate_hisim_hv_drain_smoothing(variant, &input)?;
        for (branch, actual) in [point.effective_drain, point.saturation_limit]
            .into_iter()
            .enumerate()
        {
            close(
                actual.value,
                fields[8 + branch],
                2e-10,
                1e-14,
                (index, branch),
            );
            for (axis, slope) in actual.gradient.into_iter().enumerate() {
                close(
                    slope,
                    fields[10 + 4 * branch + axis],
                    2e-5,
                    1e-8,
                    (index, branch, axis),
                );
            }
        }
        if count % 32 == 0 {
            let zero = HiSimHvDrainSmoothingInputs {
                drain: HiSimHvIntrinsicScalar {
                    value: 0.,
                    gradient: GRADIENTS[0],
                },
                ..input
            };
            let limit = evaluate_hisim_hv_drain_smoothing(variant, &zero)?;
            assert_eq!(limit.effective_drain, zero.drain);
        }
        count += 1;
    }
    assert_eq!(count, 1280);
    Ok(())
}
