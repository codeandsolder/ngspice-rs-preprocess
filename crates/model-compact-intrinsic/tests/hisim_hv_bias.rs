//! Independent HiSIM-HV clamp/symmetry value and gradient comparisons.
use model_compact_intrinsic::{
    HiSimHvBiasConditioningInputs, HiSimHvIntrinsicBias, HiSimHvIntrinsicScalar,
    evaluate_hisim_hv_bias_conditioning,
};
type TestResult = Result<(), Box<dyn std::error::Error>>;
fn close(actual: f64, expected: f64, rel: f64, abs: f64, context: impl std::fmt::Debug) {
    assert!(
        (actual - expected).abs() <= rel.mul_add(actual.abs().max(expected.abs()), abs),
        "{context:?}: {actual:e} vs {expected:e}"
    );
}
#[test]
fn native_body_conditioning_and_mixed_gradients() -> TestResult {
    let mut count = 0;
    for (index, line) in include_str!("fixtures/hisim-hv-bias-ngspice47.txt")
        .lines()
        .filter(|line| !line.starts_with('#'))
        .enumerate()
    {
        let fields: [f64; 43] = line
            .split_whitespace()
            .map(str::parse::<f64>)
            .collect::<Result<Vec<_>, _>>()?
            .try_into()
            .map_err(|_| "bias fixture record width")?;
        let input = HiSimHvBiasConditioningInputs {
            bias: HiSimHvIntrinsicBias {
                vgs: fields[1],
                vds: fields[2],
                vbs: fields[3],
            },
            twice_bulk_potential: HiSimHvIntrinsicScalar {
                value: fields[4],
                gradient: [0., 0., 0., -0.0017],
            },
            reference_channel_potential: fields[5],
            reference_bulk_potential: fields[6],
            symmetry_voltage: fields[7],
        };
        let point = evaluate_hisim_hv_bias_conditioning(&input)?;
        let outputs = [
            point.confined_body,
            point.confinement_slope,
            point.maximum_body,
            point.symmetry_offset,
            point.modified_body,
            point.modified_drain,
            point.modified_gate,
        ];
        for (branch, actual) in outputs.into_iter().enumerate() {
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
                    fields[15 + 4 * branch + axis],
                    2e-5,
                    1e-8,
                    (index, branch, axis),
                );
            }
        }
        count += 1;
    }
    assert_eq!(count, 2688);
    Ok(())
}
