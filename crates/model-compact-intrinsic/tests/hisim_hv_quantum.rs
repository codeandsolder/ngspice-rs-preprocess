//! Independent source-extracted HV1/HV2 quantum oxide laws.
use model_compact_intrinsic::{
    BoundParameters, HiSimHvIntrinsicBias, HiSimHvIntrinsicScalar, ModelVariant, ParameterScope,
    ParameterValue::Real, PreparedHiSimHvQuantumOxide, prepare_hisim_hv_instance,
    prepare_hisim_hv_model, prepare_hisim_hv_quantum_oxide,
};
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
fn close(actual: f64, expected: f64, rel: f64, abs: f64, context: impl std::fmt::Debug) {
    assert!(
        (actual - expected).abs() <= rel.mul_add(actual.abs().max(expected.abs()), abs),
        "{context:?}: {actual:e} vs {expected:e}"
    );
}
fn prepare(variant: ModelVariant, fields: &[f64; 33]) -> TestResult<PreparedHiSimHvQuantumOxide> {
    let model = prepare_hisim_hv_model(&BoundParameters::bind(
        variant,
        ParameterScope::Model,
        [
            ("tox", Real(fields[5])),
            ("kappa", Real(fields[6] / 8.854_187_8e-12)),
            ("vfbc", Real(fields[7])),
            ("qme1", Real(fields[9])),
            ("qme2", Real(fields[10])),
            ("qme3", Real(fields[11])),
            ("nsubc", Real(1e17)),
            ("nsubp", Real(1e17)),
            ("npext", Real(1e17)),
            ("nsubp0", Real(0.)),
            ("nsubcw", Real(0.)),
        ],
    )?)?;
    let instance = prepare_hisim_hv_instance(
        &model,
        &BoundParameters::bind(
            variant,
            ParameterScope::Instance,
            [("l", Real(2e-6)), ("w", Real(10e-6))],
        )?,
    )?;
    Ok(prepare_hisim_hv_quantum_oxide(&model, &instance)?)
}
#[test]
fn native_quantum_oxide_value_and_gradient() -> TestResult {
    let mut count = 0;
    for (index, line) in include_str!("fixtures/hisim-hv-quantum-ngspice47.txt")
        .lines()
        .filter(|line| !line.starts_with('#'))
        .enumerate()
    {
        let fields: [f64; 33] = line
            .split_whitespace()
            .map(str::parse::<f64>)
            .collect::<Result<Vec<_>, _>>()?
            .try_into()
            .map_err(|_| "quantum fixture record width")?;
        let variant = match fields[0] {
            1. => ModelVariant::HiSimHv1,
            2. => ModelVariant::HiSimHv2,
            _ => return Err("quantum reference family".into()),
        };
        let law = prepare(variant, &fields)?;
        let point = law.evaluate(
            HiSimHvIntrinsicBias {
                vgs: fields[1],
                vds: 0.1,
                vbs: fields[2],
            },
            HiSimHvIntrinsicScalar {
                value: fields[3],
                gradient: [1., 0.2, -0.08, 0.0004],
            },
            HiSimHvIntrinsicScalar {
                value: fields[4],
                gradient: [0., 0., 0., 3e-6],
            },
        )?;
        assert_eq!(point.active, fields[16] != 0., "case {index}");
        let outputs = [
            point.thickness,
            point.capacitance,
            point.inverse_capacitance,
            point.poisson_capacitance_factor,
        ];
        for (branch, actual) in outputs.into_iter().enumerate() {
            close(
                actual.value,
                fields[12 + branch],
                2e-9,
                1e-21,
                (index, branch),
            );
            for (axis, slope) in actual.gradient.into_iter().enumerate() {
                close(
                    slope,
                    fields[17 + 4 * branch + axis],
                    5e-5,
                    1e-10 * actual.value.abs(),
                    (index, branch, axis),
                );
            }
        }
        count += 1;
    }
    assert_eq!(count, 5120);
    Ok(())
}
