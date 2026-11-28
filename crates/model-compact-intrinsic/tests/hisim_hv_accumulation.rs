//! Independent source-extracted normal-mode accumulation values and derivatives.
use model_compact_intrinsic::{
    HiSimHvAccumulationInputs, HiSimHvIntrinsicScalar, ModelVariant, evaluate_hisim_hv_accumulation,
};
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
fn near(actual: f64, expected: f64, rel: f64, abs: f64, context: impl std::fmt::Debug) {
    assert!(
        (actual - expected).abs() <= rel.mul_add(actual.abs().max(expected.abs()), abs),
        "{context:?}: {actual:e} vs {expected:e}"
    );
}
const GRADIENTS: [[f64; 4]; 7] = [
    [1., 0.04, -0.03, 0.0002],
    [0., 0.015, 0.91, -0.00015],
    [0., 0., 0., -0.13],
    [0.0001, 0.00008, 0.00012, 3e-6],
    [0.0002, 0.00003, 0.00002, 0.],
    [0., 0., 0., -0.00018],
    [0., 0., 0., 0.0004],
];
#[test]
fn native_accumulation_confinement_and_charge() -> TestResult {
    let mut count = 0;
    for (index, line) in include_str!("fixtures/hisim-hv-accumulation-ngspice47.txt")
        .lines()
        .filter(|line| !line.starts_with('#'))
        .enumerate()
    {
        let values: [f64; 19] = line
            .split_whitespace()
            .map(str::parse::<f64>)
            .collect::<Result<Vec<_>, _>>()?
            .try_into()
            .map_err(|_| "accumulation fixture record width")?;
        let variant = match values[0] {
            1. => ModelVariant::HiSimHv1,
            2. => ModelVariant::HiSimHv2,
            _ => return Err("accumulation family".into()),
        };
        let fields = std::array::from_fn::<_, 7, _>(|i| HiSimHvIntrinsicScalar {
            value: values[i + 1],
            gradient: GRADIENTS[i],
        });
        let state = HiSimHvAccumulationInputs {
            effective_gate: fields[0],
            confined_body: fields[1],
            inverse_thermal_voltage: fields[2],
            poisson_coefficient: fields[3],
            oxide_capacitance: fields[4],
            bandgap: fields[5],
            twice_bulk_potential: fields[6],
            minimum_gate_voltage: values[8],
        };
        let point = evaluate_hisim_hv_accumulation(variant, &state)?;
        for (field, scalar) in [point.surface_potential, point.bulk_charge_density]
            .into_iter()
            .enumerate()
        {
            near(
                scalar.value,
                values[9 + field],
                2e-10,
                1e-14,
                (index, field, "value"),
            );
            for (axis, gradient) in scalar.gradient.into_iter().enumerate() {
                near(
                    gradient,
                    values[11 + axis * 2 + field],
                    3e-5,
                    1e-10,
                    (index, field, axis),
                );
            }
        }
        count += 1;
    }
    assert_eq!(count, 320);
    Ok(())
}
