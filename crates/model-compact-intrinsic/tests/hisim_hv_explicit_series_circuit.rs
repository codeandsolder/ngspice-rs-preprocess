//! Native physical explicit series resistance with measured prime-node drops.
use model_compact_intrinsic::{
    BoundParameters, HiSimHvExplicitSeriesBias, ModelError, ModelVariant, ParameterScope,
    ParameterValue, PreparedHiSimHvInstance, PreparedHiSimHvModel,
    prepare_hisim_hv_explicit_series, prepare_hisim_hv_instance, prepare_hisim_hv_model,
    prepare_hisim_hv_temperature,
};
type TestResult = Result<(), Box<dyn std::error::Error>>;

fn prepare(
    row: &[f64],
    cotemp: Option<i32>,
) -> Result<(PreparedHiSimHvModel, PreparedHiSimHvInstance), ModelError> {
    let variant = if row[0] == 1. {
        ModelVariant::HiSimHv1
    } else {
        ModelVariant::HiSimHv2
    };
    let mut fields = vec![
        (
            if row[2] < 0. { "pmos" } else { "nmos" },
            ParameterValue::Flag(true),
        ),
        (
            "corsrd",
            ParameterValue::Integer(if row[1] == 1. { 1 } else { 3 }),
        ),
        (
            "cosubnode",
            ParameterValue::Integer(i32::from(row[9] == 1.)),
        ),
        ("cosym", ParameterValue::Integer(i32::from(row[8] == 2.))),
    ];
    if variant == ModelVariant::HiSimHv2 {
        fields.push(("cordrift", ParameterValue::Integer(0)));
        fields.extend(
            ["js0d", "js0s", "js0swd", "js0sws"].map(|name| (name, ParameterValue::Real(0.))),
        );
    } else {
        fields.extend(["js0", "js0sw"].map(|name| (name, ParameterValue::Real(0.))));
    }
    for name in [
        "coovlp", "coadov", "coisti", "coisub", "coiigs", "cogidl", "conqs",
    ] {
        fields.push((name, ParameterValue::Integer(0)));
    }
    fields.extend(
        [
            ("nover", 1e17),
            ("loverld", 5e-7),
            ("lovers", 3e-7),
            ("rsh", 10.),
            ("rd", 0.005),
            ("rs", if row[8] == 2. { 0.005 } else { 0.003 }),
        ]
        .map(|(name, value)| (name, ParameterValue::Real(value))),
    );
    let (rdvd, gate, reference, body) = match row[8] {
        0. => (0., 0., 100., 0.),
        1. => (0.015, 0.4, 5., 0.2),
        2. => (0.02, 0.8, 7., 0.15),
        _ => return Err(ModelError::InvalidPhysicalParameter("series fixture card")),
    };
    fields.extend(
        [
            ("rdvd", rdvd),
            ("rdvg11", gate),
            ("rdvg12", reference),
            ("rdvb", body),
        ]
        .map(|(name, value)| (name, ParameterValue::Real(value))),
    );
    if row[8] == 2. {
        fields.extend(
            [
                ("rdtemp1", 0.00002),
                ("rdtemp2", 1e-8),
                ("rdvdtemp1", 0.00003),
                ("rdvdtemp2", 1e-8),
                ("rds", 1.5),
                ("rdsp", 0.7),
                ("rdvds", 2.),
                ("rdvdsp", 0.8),
                ("rdvdl", 0.02),
                ("rdvdlp", 1.1),
                ("rdslp1", 0.3),
                ("rdict1", 0.7),
                ("rdslp2", 0.8),
                ("rdict2", 0.2),
                ("rdov11", 0.4),
                ("rdov12", 1.2),
                ("rdov13", 0.6),
            ]
            .map(|(name, value)| (name, ParameterValue::Real(value))),
        );
    }
    if let Some(mode) = cotemp {
        fields.push(("cotemp", ParameterValue::Integer(mode)));
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
                ("l", row[4]),
                ("w", row[5]),
                ("m", row[6]),
                ("nf", row[7]),
                ("nrd", 2.),
                ("nrs", 1.),
                ("ldrift1", 1.2e-6),
                ("ldrift2", 1.5e-6),
                ("ldrift1s", 0.8e-6),
                ("ldrift2s", 1.1e-6),
            ]
            .map(|(name, value)| (name, ParameterValue::Real(value))),
        )?,
    )?;
    Ok((model, instance))
}

#[test]
fn native_explicit_series_branches() -> TestResult {
    let mut count = 0;
    let mut failures = [0; 2];
    let mut worst = [0_f64; 2];
    for (index, line) in include_str!("fixtures/hisim-hv-explicit-series-ngspice47.txt")
        .lines()
        .enumerate()
    {
        if line.starts_with('#') {
            continue;
        }
        let row: [f64; 20] = line
            .split_whitespace()
            .map(str::parse)
            .collect::<Result<Vec<_>, _>>()?
            .try_into()
            .map_err(|_| "native explicit series fixture width")?;
        let (model, instance) = prepare(&row, None)?;
        let series =
            prepare_hisim_hv_explicit_series(&model, &instance)?.ok_or("series law disabled")?;
        assert_eq!(series.prime_nodes(), [true, true]);
        let temp = prepare_hisim_hv_temperature(&model, &instance, row[3] + 273.15, 0.)?;
        let reversed = row[10] == 1.;
        let (gate, drain, body) = if reversed {
            (row[11] - row[12], -row[12], row[13] - row[12])
        } else {
            (row[11], row[12], row[13])
        };
        let bias = HiSimHvExplicitSeriesBias {
            gate_source: gate,
            drain_source: drain,
            body_source: body,
            drain_drop: drain - row[15],
            source_drop: -row[16],
            substrate_voltage: if row[9] == 1. { Some(row[14]) } else { None },
        };
        let point = series.evaluate(bias, &temp)?;
        let d = point.drain.ok_or("drain prime absent")?;
        let s = point.source.ok_or("source prime absent")?;
        for (i, (actual, expected)) in [(d.current_a.value, row[17]), (s.current_a.value, row[18])]
            .into_iter()
            .enumerate()
        {
            let error = (actual - expected).abs();
            let relative = error / expected.abs().max(actual.abs()).max(1e-15);
            if error > 5e-6 * expected.abs().max(actual.abs()) + 1e-14 {
                failures[i] += 1;
                if relative > worst[i] {
                    worst[i] = relative;
                    eprintln!(
                        "row {} branch {i}: expected={expected:e}, actual={actual:e}, R={:?}, inputs={:?}",
                        index + 1,
                        [d.resistance_ohm.value, s.resistance_ohm.value],
                        &row[..17]
                    );
                }
            }
        }
        count += 1;
    }
    assert_eq!(count, 6912);
    assert_eq!(failures, [0; 2], "worst relative {worst:?}");
    Ok(())
}

#[test]
fn independent_series_jacobians() -> TestResult {
    for family in [1., 2.] {
        for card in 0..3 {
            for cotemp in 0..4 {
                for substrate in [false, true] {
                    let mut row = [0.; 20];
                    row[..10].copy_from_slice(&[
                        family,
                        3.,
                        1.,
                        27.,
                        4e-6,
                        12e-6,
                        2.,
                        2.,
                        f64::from(card),
                        if substrate { 1. } else { 0. },
                    ]);
                    let (model, instance) = prepare(&row, Some(cotemp))?;
                    let series = prepare_hisim_hv_explicit_series(&model, &instance)?
                        .ok_or("series law disabled")?;
                    let evaluate = |x: [f64; 7]| -> Result<_, Box<dyn std::error::Error>> {
                        let temp = prepare_hisim_hv_temperature(&model, &instance, 300.15, x[3])?;
                        let p = series.evaluate(
                            HiSimHvExplicitSeriesBias {
                                gate_source: x[0],
                                drain_source: x[1],
                                body_source: x[2],
                                drain_drop: x[4],
                                source_drop: x[5],
                                substrate_voltage: substrate.then_some(x[6]),
                            },
                            &temp,
                        )?;
                        let d = p.drain.ok_or("drain absent")?;
                        let s = p.source.ok_or("source absent")?;
                        Ok([d.resistance_ohm, s.resistance_ohm, d.current_a, s.current_a])
                    };
                    for d in [-3.2, 0.03, 3.2] {
                        let x = [4.1, d, -0.5, 40., 0.7, -0.05, -0.3];
                        let analytical = evaluate(x)?;
                        for axis in 0..7 {
                            let h = if axis == 3 { 1e-2 } else { 1e-5 };
                            let perturb = |offset: f64| {
                                let mut y = x;
                                y[axis] += offset * h;
                                evaluate(y)
                            };
                            let a = perturb(-2.)?;
                            let b = perturb(-1.)?;
                            let c = perturb(1.)?;
                            let d = perturb(2.)?;
                            for i in 0..4 {
                                let numerical = ((a[i].value - d[i].value)
                                    + 8. * (c[i].value - b[i].value))
                                    / (12. * h);
                                let exact = analytical[i].gradient[axis];
                                let bound =
                                    32. * f64::EPSILON * analytical[i].value.abs() / h + 1e-12;
                                assert!(
                                    (numerical - exact).abs()
                                        <= 2e-5 * exact.abs().max(numerical.abs()) + bound,
                                    "family={family} card={card} cotemp={cotemp} substrate={substrate} axis={axis} output={i}, analytic={exact:e}, FD={numerical:e}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(())
}
