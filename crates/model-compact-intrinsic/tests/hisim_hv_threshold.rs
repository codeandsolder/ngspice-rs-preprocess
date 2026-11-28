//! Native source-extracted HV1/HV2 threshold component oracle.
use model_compact_intrinsic::{
    BoundParameters, HiSimHvBiasConditioningPoint, HiSimHvIntrinsicScalar as Jet,
    HiSimHvQuantumOxidePoint, ModelVariant, ParameterScope,
    ParameterValue::{Integer, Real},
    prepare_hisim_hv_electrostatic_material, prepare_hisim_hv_instance, prepare_hisim_hv_model,
    prepare_hisim_hv_temperature, prepare_hisim_hv_threshold,
};
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
fn close(actual: f64, expected: f64, rel: f64, abs: f64, context: impl std::fmt::Debug) {
    assert!(
        (actual - expected).abs() <= rel.mul_add(actual.abs().max(expected.abs()), abs),
        "{context:?}: {actual:e} vs {expected:e}"
    );
}
fn bias_and_oxide(fields: &[f64; 48]) -> (HiSimHvBiasConditioningPoint, HiSimHvQuantumOxidePoint) {
    let bias = HiSimHvBiasConditioningPoint {
        confined_body: Jet {
            value: 0.,
            gradient: [0.; 4],
        },
        confinement_slope: Jet {
            value: 0.,
            gradient: [0.; 4],
        },
        maximum_body: Jet {
            value: 0.,
            gradient: [0.; 4],
        },
        symmetry_offset: Jet {
            value: 0.,
            gradient: [0.; 4],
        },
        modified_gate: Jet {
            value: fields[1],
            gradient: [1., 0.02, 0., 0.001],
        },
        modified_drain: Jet {
            value: fields[2],
            gradient: [0., 1., 0.04, 0.0001],
        },
        modified_body: Jet {
            value: fields[3],
            gradient: [0., 0.03, 0.8, -0.0002],
        },
    };
    let cox = Jet {
        value: fields[4],
        gradient: [0.0001, 0.00002, -0.00003, 0.000_002],
    };
    // These are independent intermediate source inputs, not an approximation of the coupled solution.
    let inverse = Jet {
        value: cox.value.recip(),
        gradient: cox.gradient.map(|v| -v / cox.value.powi(2)),
    };
    let oxide = HiSimHvQuantumOxidePoint {
        active: true,
        thickness: Jet {
            value: 10e-9,
            gradient: [0.; 4],
        },
        capacitance: cox,
        inverse_capacitance: inverse,
        poisson_capacitance_factor: Jet {
            value: (0.0005 * inverse.value).powi(2),
            gradient: inverse
                .gradient
                .map(|v| 2. * 0.0005_f64.powi(2) * inverse.value * v),
        },
    };
    (bias, oxide)
}
fn evaluate(fields: &[f64; 48], variant: ModelVariant) -> TestResult<[Jet; 7]> {
    let mut values = vec![
        ("tox", Real(10e-9)),
        ("kappa", Real(3.9)),
        ("vfbc", Real(-0.7)),
        ("nsubc", Real(1e17)),
        ("nsubp", Real(5e17)),
        ("npext", Real(1e17)),
        ("nsubp0", Real(0.)),
        ("nsubcw", Real(0.)),
        ("xl", Real(0.)),
        ("xw", Real(0.)),
        ("xld", Real(0.)),
        ("xldld", Real(0.)),
        ("xwd", Real(0.)),
        ("ll", Real(0.)),
        ("wl", Real(0.)),
        ("lp", Real(fields[10])),
        ("bs1", Real(0.02)),
        ("bs2", Real(0.4)),
        ("vbi", Real(1.1)),
        ("scp21", Real(0.6)),
        ("scp22", Real(0.0007)),
        ("parl2", Real(0.05e-6)),
        ("sc4", Real(0.12)),
        ("scp1", Real(0.1)),
        ("scp2", Real(0.03)),
        ("scp3", Real(0.04e-6)),
        ("sc1", Real(0.1)),
        ("sc2", Real(0.02)),
        ("sc3", Real(0.03e-6)),
        ("wfc", Real(0.3e-13)),
        ("wvth0", Real(0.03)),
        ("wl2", Real(0.005)),
        ("wl2p", Real(1.)),
    ];
    if variant == ModelVariant::HiSimHv1 {
        values.push(("pthrou", Real(fields[11])));
    } else {
        values.extend([
            ("codep", Integer(i32::from(fields[12] != 0.))),
            ("depeta", Real(0.07)),
        ]);
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
            [("l", Real(fields[8])), ("w", Real(fields[9]))],
        )?,
    )?;
    let material = prepare_hisim_hv_electrostatic_material(&model, &instance)?;
    let mut temperature = prepare_hisim_hv_temperature(&model, &instance, 300., 0.)?;
    temperature.inverse_thermal_voltage = fields[5];
    temperature.inverse_thermal_voltage_derivative = -0.12;
    let mut mp = material.evaluate(&temperature)?;
    mp.twice_bulk_potential = Jet {
        value: fields[6],
        gradient: [0., 0., 0., -0.001],
    };
    mp.pocket_overlap_potential = Jet {
        value: fields[7],
        gradient: [0., 0., 0., 0.0001],
    };
    let (bias, oxide) = bias_and_oxide(fields);
    let law = prepare_hisim_hv_threshold(&model, &instance)?;
    let result = law.evaluate(
        &bias,
        &oxide,
        &mp,
        &temperature,
        8.854_187_8e-12 * 3.9 / 10e-9,
    )?;
    Ok([
        result.pocket_shift,
        result.short_channel_shift,
        result.narrow_channel_shift,
        result.total_shift,
        result.reference_bulk_potential,
        result.depletion_potential_sum,
        result.threshold,
    ])
}
#[test]
fn native_threshold_components_and_gradients() -> TestResult {
    let mut count = 0;
    for (index, line) in include_str!("fixtures/hisim-hv-threshold-ngspice47.txt")
        .lines()
        .filter(|l| !l.starts_with('#'))
        .enumerate()
    {
        let fields: [f64; 48] = line
            .split_whitespace()
            .map(str::parse::<f64>)
            .collect::<Result<Vec<_>, _>>()?
            .try_into()
            .map_err(|_| "threshold record width")?;
        let variant = match fields[0] {
            1. => ModelVariant::HiSimHv1,
            2. => ModelVariant::HiSimHv2,
            _ => return Err("threshold reference family".into()),
        };
        let actual = evaluate(&fields, variant).map_err(|e| format!("case {index}: {e}"))?;
        for (branch, jet) in actual.into_iter().enumerate() {
            close(
                jet.value,
                fields[13 + branch],
                3e-10,
                1e-13,
                (index, branch),
            );
            for (axis, slope) in jet.gradient.into_iter().enumerate() {
                close(
                    slope,
                    fields[20 + 4 * branch + axis],
                    5e-5,
                    1e-9,
                    (index, branch, axis),
                );
            }
        }
        count += 1;
    }
    assert_eq!(count, 3456);
    Ok(())
}
