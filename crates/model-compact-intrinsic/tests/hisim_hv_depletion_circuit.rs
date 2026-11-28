//! Native isolated HV2 CODEP=1 circuit operating points and independent Jacobians.
use model_compact_intrinsic::{
    BoundParameters, HiSimHvDepletionChannelEvaluation, HiSimHvIntrinsicBias,
    HiSimHvIntrinsicScalar, ModelError, ModelVariant, ParameterScope, ParameterValue,
    PreparedHiSimHvDepletionChannel, PreparedHiSimHvInstance, PreparedHiSimHvModel,
    prepare_hisim_hv_depletion_channel, prepare_hisim_hv_instance, prepare_hisim_hv_model,
    prepare_hisim_hv_transport,
};
type TestResult = Result<(), Box<dyn std::error::Error>>;
type Jet = HiSimHvIntrinsicScalar;

fn prepare(
    row: &[f64; 18],
    heating: bool,
) -> Result<
    (
        PreparedHiSimHvModel,
        PreparedHiSimHvInstance,
        PreparedHiSimHvDepletionChannel,
    ),
    ModelError,
> {
    let variant = ModelVariant::HiSimHv2;
    let mut fields = vec![
        (
            if row[1] < 0. { "pmos" } else { "nmos" },
            ParameterValue::Flag(true),
        ),
        ("codep", ParameterValue::Integer(1)),
    ];
    for name in [
        "corsrd", "coisti", "coisub", "coadov", "coovlp", "conqs", "cordrift",
    ] {
        fields.push((name, ParameterValue::Integer(0)));
    }
    if row[16] == 1. {
        fields.extend(
            [
                ("ndepm", 3e17),
                ("tndep", 0.1e-6),
                ("depeta", 0.2),
                ("depvdsef1", 1.5),
                ("depvdsef2", 0.8),
                ("depleak", 0.08),
                ("depmue0", 600.),
                ("depmue1", 40.),
                ("depmueback0", 220.),
                ("depmueback1", 12.),
                ("depmueph0", 0.6),
                ("depbb", 1.7),
            ]
            .map(|(name, value)| (name, ParameterValue::Real(value))),
        );
    }
    let model = prepare_hisim_hv_model(&BoundParameters::bind(
        variant,
        ParameterScope::Model,
        fields,
    )?)?;
    let instance = prepare_hisim_hv_instance(
        &model,
        &BoundParameters::bind(
            variant,
            ParameterScope::Instance,
            [
                ("l", ParameterValue::Real(row[3])),
                ("w", ParameterValue::Real(row[4])),
                ("m", ParameterValue::Real(row[5])),
                ("nf", ParameterValue::Real(row[17])),
                ("coselfheat", ParameterValue::Integer(i32::from(heating))),
            ],
        )?,
    )?;
    let channel = prepare_hisim_hv_depletion_channel(&model, &instance)?;
    Ok((model, instance, channel))
}
fn outputs(point: &HiSimHvDepletionChannelEvaluation) -> [Jet; 5] {
    [
        point.drain_current_a,
        point.terminal_charges.bulk,
        point.terminal_charges.gate,
        point.terminal_charges.drain,
        point.terminal_charges.source,
    ]
}
fn rows() -> impl Iterator<Item = Result<[f64; 18], Box<dyn std::error::Error>>> {
    include_str!("fixtures/hisim-hv-depletion-circuit-ngspice47.txt")
        .lines()
        .filter(|line| !line.starts_with('#'))
        .map(|line| {
            let values = line
                .split_whitespace()
                .map(str::parse::<f64>)
                .collect::<Result<Vec<_>, _>>()?;
            values.try_into().map_err(|_| "native fixture width".into())
        })
}

fn check_circuit(smoke: bool) -> TestResult {
    let mut failures = [0; 4];
    let mut errors = 0;
    let mut count = 0;
    let mut maximum = [0_f64; 4];
    for (index, row) in rows().enumerate() {
        if smoke && !index.is_multiple_of(71) {
            continue;
        }
        let row = row?;
        let (model, instance, channel) = prepare(&row, false)?;
        let temperature = prepare_hisim_hv_transport(&model, &instance, row[2] + 273.15, 0.)?;
        let bias = HiSimHvIntrinsicBias {
            vgs: row[6],
            vds: row[7],
            vbs: row[8],
        };
        count += 1;
        let point = match channel.evaluate(bias, &temperature) {
            Ok(p) => p,
            Err(error) => {
                errors += 1;
                if errors <= 20 {
                    eprintln!("case {index}: {error}: {:?}", &row[..9]);
                }
                continue;
            }
        };
        let [current, bulk, gate, drain, source] = outputs(&point);
        assert!(
            (gate.value + bulk.value + drain.value + source.value).abs()
                <= 32.
                    * f64::EPSILON
                    * gate
                        .value
                        .abs()
                        .max(bulk.value.abs())
                        .max(drain.value.abs())
                        .max(source.value.abs())
        );
        for (i, (p, expected)) in [
            (current, row[9]),
            (bulk, row[13]),
            (gate, row[14]),
            (drain, row[15]),
        ]
        .into_iter()
        .enumerate()
        {
            let error = (p.value - expected).abs();
            let tolerance = if i == 0 {
                1e-6 * p.value.abs().max(expected.abs()) + 1e-18
            } else {
                2e-7 * p.value.abs().max(expected.abs()) + 1e-25
            };
            if error > tolerance {
                failures[i] += 1;
                if failures[i] <= 5 || error > maximum[i] {
                    if i == 0 {
                        eprintln!(
                            "components: surface={:e}, residual={:e}, back={:e}, carrier source={:e}, drain={:e}, Idd={:e}, Pds={:e}, Mu={:e}",
                            point.channel.surface_current_a.value,
                            point.channel.residual_current_a.value,
                            point.channel.back_current_a.value,
                            point.source.transport_carrier.value,
                            point.drain.transport_carrier.value,
                            point.channel.drift_diffusion_integral.value,
                            point.channel.surface_difference.value,
                            point.channel.surface_mobility.value
                        );
                    }
                    eprintln!(
                        "case {index}, field {i}: native={expected:e}, Rust={:e}, input {:?}, card {}, NF {}",
                        p.value,
                        &row[..9],
                        row[16],
                        row[17]
                    );
                }
                maximum[i] = maximum[i].max(error);
            }
        }
    }
    assert_eq!(count, if smoke { 33 } else { 2304 });
    assert_eq!(errors, 0);
    assert_eq!(failures, [0; 4], "maximum errors {maximum:?}");
    Ok(())
}
#[test]
fn native_depletion_circuit_smoke() -> TestResult {
    check_circuit(true)
}
#[test]
fn native_depletion_circuit() -> TestResult {
    check_circuit(false)
}
#[test]
fn independent_depletion_circuit_jacobian() -> TestResult {
    let mut count = 0;
    for (index, row) in rows().enumerate() {
        if !index.is_multiple_of(97) {
            continue;
        }
        let row = row?;
        let (model, instance, channel) = prepare(&row, true)?;
        let evaluate = |offset: [f64; 4]| {
            let temperature =
                prepare_hisim_hv_transport(&model, &instance, row[2] + 273.15, 5. + offset[3])?;
            channel
                .evaluate(
                    HiSimHvIntrinsicBias {
                        vgs: row[6] + offset[0],
                        vds: row[7] + offset[1],
                        vbs: row[8] + offset[2],
                    },
                    &temperature,
                )
                .map(|p| outputs(&p))
        };
        let actual = evaluate([0.; 4])?;
        for axis in 0..4 {
            let step = if axis == 3 { 1e-3 } else { 1e-5 };
            let ev = |n: f64| {
                let mut offset = [0.; 4];
                offset[axis] = n * step;
                evaluate(offset)
            };
            let minus_two = ev(-2.)?;
            let minus_one = ev(-1.)?;
            let plus_one = ev(1.)?;
            let plus_two = ev(2.)?;
            for i in 0..5 {
                let fd = ((minus_two[i].value - plus_two[i].value)
                    + 8. * (plus_one[i].value - minus_one[i].value))
                    / (12. * step);
                let exact = actual[i].gradient[axis];
                let tolerance = 2e-5 * fd.abs().max(exact.abs())
                    + 64. * f64::EPSILON * actual[i].value.abs() / step
                    + if i == 0 { 1e-13 } else { 1e-22 };
                assert!(
                    (fd - exact).abs() <= tolerance,
                    "case {index}, field {i}, axis {axis}: AD={exact:e}, FD={fd:e}"
                );
                count += 1;
            }
        }
    }
    assert_eq!(count, 480);
    Ok(())
}
