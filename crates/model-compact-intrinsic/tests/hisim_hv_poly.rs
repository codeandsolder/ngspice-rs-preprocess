//! Native source-extracted poly depletion values and gradients.
use model_compact_intrinsic::{
    BoundParameters, HiSimHvBiasConditioningPoint, HiSimHvIntrinsicBias,
    HiSimHvIntrinsicScalar as Jet, ModelVariant, ParameterScope, ParameterValue::Real,
    prepare_hisim_hv_instance, prepare_hisim_hv_model, prepare_hisim_hv_poly_depletion,
    prepare_hisim_hv_temperature,
};
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
fn close(actual: f64, expected: f64, rel: f64, abs: f64, context: impl std::fmt::Debug) {
    assert!(
        (actual - expected).abs() <= rel.mul_add(actual.abs().max(expected.abs()), abs),
        "{context:?}: {actual:e} vs {expected:e}"
    );
}
fn evaluate(fields: &[f64; 37], variant: ModelVariant) -> TestResult<[Jet; 5]> {
    let mut values = vec![
        ("tox", Real(10e-9)),
        ("xl", Real(0.)),
        ("xw", Real(0.)),
        ("xld", Real(0.)),
        ("xldld", Real(0.)),
        ("xwd", Real(0.)),
        ("ll", Real(0.)),
        ("wl", Real(0.)),
        ("kappa", Real(3.9)),
        ("vfbc", Real(-0.7)),
        ("nsubc", Real(1e17)),
        ("nsubp", Real(1e17)),
        ("npext", Real(1e17)),
        ("nsubp0", Real(0.)),
        ("nsubcw", Real(0.)),
        ("pgd1", Real(fields[7])),
        ("pgd2", Real(fields[8])),
        ("pgd4", Real(0.7)),
    ];
    if variant == ModelVariant::HiSimHv1 {
        values.push(("pgd3", Real(fields[9])));
    }
    let model = prepare_hisim_hv_model(&BoundParameters::bind(
        variant,
        ParameterScope::Model,
        values,
    )?)?;
    let instance = prepare_hisim_hv_instance(
        &model,
        &BoundParameters::bind(
            variant,
            ParameterScope::Instance,
            [("l", Real(fields[10])), ("w", Real(10e-6))],
        )?,
    )?;
    let mut temperature = prepare_hisim_hv_temperature(&model, &instance, 300., 0.)?;
    temperature.nominal_inverse_thermal_voltage = fields[11];
    let bias = HiSimHvIntrinsicBias {
        vgs: fields[1],
        vds: fields[2],
        vbs: fields[3],
    };
    let zero = Jet {
        value: 0.,
        gradient: [0.; 4],
    };
    let conditioned = HiSimHvBiasConditioningPoint {
        confined_body: Jet {
            value: fields[3],
            gradient: [0., 0., 0.8, -0.0002],
        },
        modified_gate: Jet {
            value: fields[4],
            gradient: [1., 0.02, 0.01, 0.001],
        },
        modified_body: Jet {
            value: fields[5],
            gradient: [0., 0.03, 0.81, 0.0008],
        },
        confinement_slope: zero,
        maximum_body: zero,
        symmetry_offset: zero,
        modified_drain: zero,
    };
    let point = prepare_hisim_hv_poly_depletion(&model, &instance)?.evaluate(
        bias,
        &conditioned,
        Jet {
            value: fields[6],
            gradient: [0., 0.03, 0.02, 0.0001],
        },
        &temperature,
    )?;
    Ok([
        point.depletion,
        point.effective_gate,
        point.flatband_voltage,
        point.symmetry_factor,
        point.accumulation_damping,
    ])
}
#[test]
fn native_poly_depletion_and_gradients() -> TestResult {
    let mut count = 0;
    for (index, line) in include_str!("fixtures/hisim-hv-poly-ngspice47.txt")
        .lines()
        .filter(|l| !l.starts_with('#'))
        .enumerate()
    {
        let fields: [f64; 37] = line
            .split_whitespace()
            .map(str::parse::<f64>)
            .collect::<Result<Vec<_>, _>>()?
            .try_into()
            .map_err(|_| "poly record width")?;
        let variant = match fields[0] {
            1. => ModelVariant::HiSimHv1,
            2. => ModelVariant::HiSimHv2,
            _ => return Err("poly reference family".into()),
        };
        let actual = evaluate(&fields, variant).map_err(|e| format!("case {index}: {e}"))?;
        for (branch, jet) in actual.into_iter().enumerate() {
            close(
                jet.value,
                fields[12 + branch],
                3e-10,
                1e-13,
                (index, branch),
            );
            for (axis, slope) in jet.gradient.into_iter().enumerate() {
                close(
                    slope,
                    fields[17 + 4 * branch + axis],
                    5e-5,
                    1e-9,
                    (index, branch, axis),
                );
            }
        }
        count += 1;
    }
    assert_eq!(count, 4320);
    Ok(())
}
