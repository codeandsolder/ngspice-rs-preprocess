//! Independent source-extracted HiSIM-HV non-edge gate currents and derivatives.
use model_compact_intrinsic::{
    BoundParameters, HiSimHvGateChannelState, HiSimHvIntrinsicBias, HiSimHvIntrinsicScalar,
    ModelVariant, ParameterScope,
    ParameterValue::{Integer, Real},
    prepare_hisim_hv_gate_tunneling, prepare_hisim_hv_instance, prepare_hisim_hv_model,
    prepare_hisim_hv_temperature,
};
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
const GRADIENTS: [[f64; 4]; 10] = [
    [0.94, 0.01, 0.03, -0.00015],
    [0., 0.86, 0.02, 0.],
    [0., -0.02, 0.93, 0.],
    [0.08, 0.02, 0.1, -0.0001],
    [0.025, 0.013, -0.04, 0.00025],
    [0.01, 0.001, 0.005, 0.00005],
    [0.04, 0.015, -0.07, 0.0002],
    [0.05, 1., 0.04, -0.001],
    [1e-4, 2e-5, -3e-5, 1e-7],
    [0., 0., 0., 1e-8],
];
fn channel_state(trial: [f64; 4], has_inversion: bool) -> HiSimHvGateChannelState {
    let [gate, drain, body, kelvin] = trial;
    let inputs = [gate, drain, body, kelvin - 300.15];
    let baseline = [0.02, 0.04, -0.02, 0.35, 0.10, 0.03, 0.20, 2., 0.004, 0.003];
    let fields = std::array::from_fn::<_, 10, _>(|index| {
        let gradient = GRADIENTS[index];
        let value = gradient
            .into_iter()
            .zip(inputs)
            .fold(baseline[index], |acc, (slope, input)| {
                slope.mul_add(input, acc)
            });
        HiSimHvIntrinsicScalar { value, gradient }
    });
    HiSimHvGateChannelState {
        modified_gate: fields[0],
        modified_drain: fields[1],
        modified_body: fields[2],
        source_surface: fields[3],
        threshold_shift: fields[4],
        poly_depletion: fields[5],
        short_long_shift: fields[6],
        lateral_field: fields[7],
        inversion_density: fields[8],
        poisson_coefficient: fields[9],
        zero_field_oxide_capacitance: 3.453_133e-11 / 8e-9,
        has_inversion,
    }
}
fn close(actual: f64, expected: f64, relative: f64, absolute: f64, context: impl std::fmt::Debug) {
    assert!(
        (actual - expected).abs() <= relative.mul_add(actual.abs().max(expected.abs()), absolute),
        "{context:?}: {actual:e} vs {expected:e}"
    );
}
fn prepared_gate_context(
    variant: ModelVariant,
    enabled: i32,
    fields: &[f64; 26],
) -> TestResult<(
    model_compact_intrinsic::PreparedHiSimHvModel,
    model_compact_intrinsic::PreparedHiSimHvInstance,
)> {
    let model = prepare_hisim_hv_model(&BoundParameters::bind(
        variant,
        ParameterScope::Model,
        [
            ("coiigs", Integer(enabled)),
            ("gleak1", Real(50.)),
            ("gleak2", Real(1e6)),
            ("gleak3", Real(0.06)),
            ("gleak4", Real(4.)),
            ("gleak5", Real(7500.)),
            ("gleak6", Real(0.25)),
            ("gleak7", Real(1e-6)),
            ("glkb1", Real(5e-16)),
            ("glkb2", Real(1.)),
            ("glkb3", Real(0.1)),
            ("fn1", Real(50.)),
            ("fn2", Real(1e6)),
            ("fn3", Real(0.)),
            ("fvbs", Real(0.012)),
            ("vzadd0", Real(0.01)),
            ("tox", Real(8e-9)),
            ("vfbc", Real(-0.8)),
            ("eg0", Real(1.2)),
            ("egig", Real(0.06)),
            ("bgtmp1", Real(9.025e-5)),
            ("bgtmp2", Real(1e-7)),
            ("igtemp2", Real(110.)),
            ("igtemp3", Real(1300.)),
            ("xl", Real(0.)),
            ("xw", Real(0.)),
            ("xld", Real(0.)),
            ("xldld", Real(0.)),
            ("ll", Real(0.)),
            ("xwd", Real(0.)),
            ("wl", Real(0.)),
        ],
    )?)?;
    let instance = prepare_hisim_hv_instance(
        &model,
        &BoundParameters::bind(
            variant,
            ParameterScope::Instance,
            [
                ("l", Real(fields[3] * 1e-6)),
                ("w", Real(fields[4] * 1e-6)),
                ("nf", Real(fields[5])),
                ("m", Real(fields[6])),
            ],
        )?,
    )?;
    Ok((model, instance))
}
#[test]
fn native_gate_values_and_independent_gradients() -> TestResult {
    let fixture = include_str!("fixtures/hisim-hv-gate-ngspice47.txt");
    let mut count = 0;
    for (index, line) in fixture
        .lines()
        .filter(|line| !line.starts_with('#'))
        .enumerate()
    {
        let fields: [f64; 26] = line
            .split_whitespace()
            .map(str::parse::<f64>)
            .collect::<Result<Vec<_>, _>>()?
            .try_into()
            .map_err(|_| "gate fixture record width")?;
        let variant = match fields[0] {
            1. => ModelVariant::HiSimHv1,
            2. => ModelVariant::HiSimHv2,
            _ => return Err("gate reference family".into()),
        };
        let enabled = match fields[1] {
            0. => 0,
            1. => 1,
            _ => return Err("gate selector".into()),
        };
        let (model, instance) = prepared_gate_context(variant, enabled, &fields)?;
        let law = prepare_hisim_hv_gate_tunneling(&model, &instance)?;
        let trial: [f64; 4] = fields[7..11].try_into()?;
        let point = law.evaluate(
            HiSimHvIntrinsicBias {
                vgs: trial[0],
                vds: trial[1],
                vbs: trial[2],
            },
            &channel_state(trial, fields[2] == 0.),
            &prepare_hisim_hv_temperature(&model, &instance, trial[3], 0.)?,
        )?;
        for (branch, (actual, expected)) in
            [point.inversion, point.direct_body, point.fowler_nordheim]
                .into_iter()
                .zip(fields[11..].as_chunks::<5>().0)
                .enumerate()
        {
            close(
                actual.value,
                expected[0],
                2e-10,
                1e-30,
                (index, branch, "value"),
            );
            for (axis, (slope, reference)) in actual
                .gradient
                .into_iter()
                .zip(expected[1..].iter().copied())
                .enumerate()
            {
                close(slope, reference, 3e-5, 1e-22, (index, branch, axis));
            }
        }
        count += 1;
    }
    assert_eq!(count, 80);
    Ok(())
}
