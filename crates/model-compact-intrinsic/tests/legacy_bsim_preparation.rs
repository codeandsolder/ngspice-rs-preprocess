use std::collections::BTreeMap;

use model_compact_intrinsic::{
    BoundParameters, LegacyBsimChannel, ModelError, ModelVariant, MosPolarity, ParameterScope,
    ParameterType, ParameterValue, PreparedLegacyBsimInstance, prepare_legacy_bsim_model,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn close(name: &str, actual: f64, expected: f64) {
    let tolerance = 2e-13 * expected.abs().max(actual.abs()).max(1e-30);
    assert!(
        actual.is_finite() && (actual - expected).abs() <= tolerance,
        "{name}: actual {actual:.17e}, C {expected:.17e}"
    );
}

fn series<const N: usize>(values: &mut BTreeMap<String, f64>, name: &str, input: [f64; N]) {
    for (index, value) in input.into_iter().enumerate() {
        values.insert(format!("{name}{index}"), value);
    }
}

fn common(device: &PreparedLegacyBsimInstance) -> BTreeMap<String, f64> {
    [
        ("oxide_density", device.oxide_capacitance_density),
        ("oxide_charge", device.oxide_capacitance),
        ("threshold", device.threshold),
        ("drain_conductance", device.drain.series_conductance),
        ("source_conductance", device.source.series_conductance),
        ("overlap_drain", device.overlap.drain),
        ("overlap_source", device.overlap.source),
        ("overlap_bulk", device.overlap.bulk),
        ("bottom_potential", device.junction_law.bottom_potential),
        ("sidewall_potential", device.junction_law.sidewall_potential),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), value))
    .collect()
}

fn coefficients(device: &PreparedLegacyBsimInstance) -> BTreeMap<String, f64> {
    let mut values = common(device);
    match &device.channel {
        LegacyBsimChannel::Bsim1(channel) => {
            values.extend(
                [
                    ("vfb", channel.vfb),
                    ("phi", channel.phi),
                    ("k1", channel.k1),
                    ("k2", channel.k2),
                    ("vdd", channel.vdd),
                    (
                        "partition",
                        f64::from(u8::from(channel.partition_zero_drain)),
                    ),
                ]
                .map(|(key, value)| (key.to_owned(), value)),
            );
            series(&mut values, "eta", channel.eta);
            series(&mut values, "beta_zero", channel.beta_zero);
            series(&mut values, "beta_vdd", channel.beta_vdd);
            series(&mut values, "ugs", channel.ugs);
            series(&mut values, "uds", channel.uds);
            series(&mut values, "subthreshold", channel.subthreshold);
        }
        LegacyBsimChannel::Bsim2(channel) => {
            let [vdd, vgg, vbb] = channel.voltage_anchors;
            values.extend(
                [
                    ("vfb", channel.vfb),
                    ("phi", channel.phi),
                    ("k1", channel.k1),
                    ("k2", channel.k2),
                    ("gate_high", channel.gate_high),
                    ("gate_low", channel.gate_low),
                    ("thermal_voltage", channel.thermal_voltage),
                    ("vdd", vdd),
                    ("vgg", vgg),
                    ("vbb", vbb),
                    ("sqrt_phi", channel.sqrt_phi),
                    ("phi_three_halves", channel.phi_three_halves),
                    ("beta_body_argument", channel.beta_body_argument),
                ]
                .map(|(key, value)| (key.to_owned(), value)),
            );
            series(&mut values, "eta", channel.eta);
            series(&mut values, "beta_zero", channel.beta_zero);
            series(&mut values, "beta_saturation", channel.beta_saturation);
            series(&mut values, "beta2", channel.beta2);
            series(&mut values, "beta3", channel.beta3);
            series(&mut values, "beta4", channel.beta4);
            series(&mut values, "ua", channel.ua);
            series(&mut values, "ub", channel.ub);
            series(&mut values, "u1", channel.u1);
            series(&mut values, "subthreshold", channel.subthreshold);
            series(&mut values, "offset", channel.offset);
            series(&mut values, "impact_a", channel.impact_a);
            series(&mut values, "impact_b", channel.impact_b);
        }
    }
    values
}

fn prepare_fixture(
    variant: ModelVariant,
    fixture: &str,
) -> Result<PreparedLegacyBsimInstance, Box<dyn std::error::Error>> {
    let (input, _) = fixture
        .split_once("\n--\n")
        .ok_or("missing fixture separator")?;
    let mut model = Vec::new();
    let mut instance = Vec::new();
    for line in input.lines().filter(|line| !line.starts_with('#')) {
        let mut fields = line.split_whitespace();
        let (scope, destination) = match fields.next() {
            Some("m") => (ParameterScope::Model, &mut model),
            Some("i") => (ParameterScope::Instance, &mut instance),
            _ => return Err("unknown fixture scope".into()),
        };
        let name = fields.next().ok_or("missing parameter")?;
        let value: f64 = fields.next().ok_or("missing value")?.parse()?;
        assert!(fields.next().is_none());
        let spec = variant
            .parameter(scope, name)
            .ok_or("unknown fixture parameter")?;
        let value = match spec.kind {
            ParameterType::Real => ParameterValue::Real(value),
            ParameterType::Flag => ParameterValue::Flag(value != 0.),
            _ => return Err("unsupported fixture input".into()),
        };
        destination.push((name, value));
    }
    let model = prepare_legacy_bsim_model(&BoundParameters::bind(
        variant,
        ParameterScope::Model,
        model,
    )?)?;
    let source = BoundParameters::bind(variant, ParameterScope::Instance, instance)?;
    let prepared = model.prepare_instance(&source)?;
    assert_eq!(prepared, model.prepare_instance(&source)?);
    Ok(prepared)
}

fn check_fixture(variant: ModelVariant, fixture: &str) -> TestResult {
    let (_, reference) = fixture
        .split_once("\n--\n")
        .ok_or("missing fixture separator")?;
    let prepared = prepare_fixture(variant, fixture)?;
    let mut actual = coefficients(&prepared);
    for line in reference.lines() {
        let (name, expected) = line.split_once(' ').ok_or("missing reference value")?;
        let expected: f64 = expected.parse()?;
        let observed = actual
            .remove(name)
            .ok_or_else(|| format!("missing field {name}"))?;
        close(name, observed, expected);
    }
    assert!(actual.is_empty(), "uncompared prepared fields: {actual:?}");
    Ok(())
}

#[test]
fn bsim1_size_preparation_matches_original_c() -> TestResult {
    for fixture in [
        include_str!("fixtures/legacy-bsim/bsim1-ordinary.txt"),
        include_str!("fixtures/legacy-bsim/bsim1-clamps.txt"),
        include_str!("fixtures/legacy-bsim/bsim1-wide.txt"),
    ] {
        check_fixture(ModelVariant::Bsim1, fixture)?;
    }
    Ok(())
}

#[test]
fn bsim2_size_preparation_matches_original_c() -> TestResult {
    for fixture in [
        include_str!("fixtures/legacy-bsim/bsim2-ordinary.txt"),
        include_str!("fixtures/legacy-bsim/bsim2-clamps.txt"),
        include_str!("fixtures/legacy-bsim/bsim2-wide.txt"),
    ] {
        check_fixture(ModelVariant::Bsim2, fixture)?;
    }
    Ok(())
}

#[test]
fn defaults_preserve_given_values_and_family_identity() -> TestResult {
    for (variant, af) in [(ModelVariant::Bsim1, 1.), (ModelVariant::Bsim2, 0.)] {
        let source = BoundParameters::bind(
            variant,
            ParameterScope::Model,
            [
                ("pb", ParameterValue::Real(0.04)),
                ("pmos", ParameterValue::Flag(true)),
            ],
        )?;
        let model = prepare_legacy_bsim_model(&source)?;
        assert_eq!(model.polarity(), MosPolarity::Pmos);
        for spec in variant.parameters(ParameterScope::Model) {
            assert!(model.get_slot(spec.slot).is_some(), "{}", spec.name);
        }
        let pb = variant
            .parameter(ParameterScope::Model, "pb")
            .ok_or("pb")?
            .slot;
        let af_slot = variant
            .parameter(ParameterScope::Model, "af")
            .ok_or("af")?
            .slot;
        assert_eq!(model.given(pb), Some(&ParameterValue::Real(0.04)));
        assert_eq!(model.get_slot(pb), Some(&ParameterValue::Real(0.1)));
        assert_eq!(model.given(af_slot), None);
        assert_eq!(model.get_slot(af_slot), Some(&ParameterValue::Real(af)));
    }
    let source = BoundParameters::bind(ModelVariant::Bsim3V33, ParameterScope::Model, [])?;
    assert_eq!(
        prepare_legacy_bsim_model(&source),
        Err(ModelError::WrongModelScope)
    );
    Ok(())
}

#[test]
fn instance_geometry_junctions_and_candidate_failures_are_transactional() -> TestResult {
    let variant = ModelVariant::Bsim1;
    let model = prepare_legacy_bsim_model(&BoundParameters::bind(
        variant,
        ParameterScope::Model,
        [
            ("tox", ParameterValue::Real(0.02)),
            ("vdd", ParameterValue::Real(5.)),
            ("muz", ParameterValue::Real(400.)),
            ("dl", ParameterValue::Real(0.2)),
            ("dw", ParameterValue::Real(0.5)),
            ("rsh", ParameterValue::Real(100.)),
            ("js", ParameterValue::Real(2e-5)),
            ("cj", ParameterValue::Real(1e-3)),
            ("cjsw", ParameterValue::Real(1e-10)),
        ],
    )?)?;
    let inputs = [
        ("l", ParameterValue::Real(2e-6)),
        ("w", ParameterValue::Real(4e-6)),
        ("m", ParameterValue::Real(3.)),
        ("ad", ParameterValue::Real(2e-10)),
        ("pd", ParameterValue::Real(3e-6)),
        ("nrd", ParameterValue::Real(2.)),
        ("nrs", ParameterValue::Real(0.)),
    ];
    let bind = |items| BoundParameters::bind(variant, ParameterScope::Instance, items);
    let input = bind(inputs.clone())?;
    let first = model.prepare_instance(&input)?;
    close("effective length", first.geometry.effective_length, 1.8e-6);
    close("effective width", first.geometry.effective_width, 3.5e-6);
    close("multiplicity", first.geometry.multiplier, 3.);
    close("drain saturation", first.drain.saturation_current, 4e-15);
    close(
        "source saturation floor",
        first.source.saturation_current,
        1e-15,
    );
    close("bottom", first.drain.bottom_capacitance, 2e-13);
    close("sidewall", first.drain.sidewall_capacitance, 3e-16);
    close("drain conductance", first.drain.series_conductance, 0.005);
    close("source short", first.source.series_conductance, 0.);
    let changed = BoundParameters::bind(
        variant,
        ParameterScope::Instance,
        inputs
            .clone()
            .into_iter()
            .chain([("w", ParameterValue::Real(8e-6))]),
    )?;
    assert_ne!(model.prepare_instance(&changed)?, first);
    let bad = BoundParameters::bind(
        variant,
        ParameterScope::Instance,
        inputs
            .into_iter()
            .chain([("l", ParameterValue::Real(0.1e-6))]),
    )?;
    assert!(model.prepare_instance(&bad).is_err());
    assert_eq!(model.prepare_instance(&input)?, first);
    let other = BoundParameters::bind(ModelVariant::Bsim2, ParameterScope::Instance, [])?;
    assert_eq!(
        model.prepare_instance(&other),
        Err(ModelError::WrongModelScope)
    );
    Ok(())
}

#[test]
fn default_geometry_and_undefined_oxide_or_model_temperature() -> TestResult {
    let input = BoundParameters::bind(ModelVariant::Bsim1, ParameterScope::Instance, [])?;
    let model = prepare_legacy_bsim_model(&BoundParameters::bind(
        ModelVariant::Bsim1,
        ParameterScope::Model,
        [],
    )?)?;
    assert!(model.prepare_instance(&input).is_err());
    let bsim2 = ModelVariant::Bsim2;
    let source = BoundParameters::bind(bsim2, ParameterScope::Model, [])?;
    let instance = BoundParameters::bind(bsim2, ParameterScope::Instance, [])?;
    let prepared = prepare_legacy_bsim_model(&source)?.prepare_instance(&instance)?;
    close("default L", prepared.geometry.length, 5e-6);
    close("default W", prepared.geometry.width, 5e-6);
    close("default M", prepared.geometry.multiplier, 1.);
    let invalid = BoundParameters::bind(
        bsim2,
        ParameterScope::Model,
        [("temp", ParameterValue::Real(-273.))],
    )?;
    assert!(
        prepare_legacy_bsim_model(&invalid)?
            .prepare_instance(&instance)
            .is_err()
    );
    Ok(())
}

#[test]
fn complete_model_defaults_match_ngspice_showmod() -> TestResult {
    for (variant, reference, count) in [
        (
            ModelVariant::Bsim1,
            include_str!("fixtures/legacy-bsim/bsim1-defaults.txt"),
            79,
        ),
        (
            ModelVariant::Bsim2,
            include_str!("fixtures/legacy-bsim/bsim2-defaults.txt"),
            135,
        ),
    ] {
        let model = prepare_legacy_bsim_model(&BoundParameters::bind(
            variant,
            ParameterScope::Model,
            [
                ("tox", ParameterValue::Real(0.03)),
                ("vdd", ParameterValue::Real(5.)),
            ],
        )?)?;
        let mut checked = 0;
        for line in reference.lines() {
            let (name, expected) = line.split_once(' ').ok_or("reference row")?;
            let expected: f64 = expected.parse()?;
            let spec = variant
                .parameter(ParameterScope::Model, name)
                .ok_or("model schema")?;
            let observed = match model.get_slot(spec.slot) {
                Some(ParameterValue::Real(value)) => *value,
                Some(ParameterValue::Flag(value)) => f64::from(u8::from(*value)),
                _ => return Err(format!("missing scalar {name}").into()),
            };
            close(name, observed, expected);
            checked += 1;
        }
        assert_eq!(checked, count);
    }
    Ok(())
}

fn check_legacy_channel(
    variant: ModelVariant,
    fixture: &str,
    points: &str,
    expected_count: usize,
) -> TestResult {
    use model_compact_intrinsic::{LegacyBsimBias, evaluate_bsim1_intrinsic};
    let prepared = prepare_fixture(variant, fixture)?;
    let mut count = 0;
    for line in points.lines().filter(|line| !line.starts_with('#')) {
        let values = line
            .split_whitespace()
            .map(str::parse::<f64>)
            .collect::<Result<Vec<_>, _>>()?;
        let [
            vds,
            vgs,
            vbs,
            drain_current,
            drain_charge,
            gate_charge,
            bulk_charge,
            von,
            sat,
        ]: [f64; 9] = values
            .try_into()
            .map_err(|_| "wrong reference row length")?;
        let bias = LegacyBsimBias { vds, vgs, vbs };
        let point = if variant == ModelVariant::Bsim1 {
            evaluate_bsim1_intrinsic(&prepared, bias)?
        } else {
            model_compact_intrinsic::evaluate_bsim2_intrinsic(&prepared, bias)?
        };
        for (label, actual, expected, absolute) in [
            ("Id", point.drain_current, drain_current, 1e-22),
            ("Qd", point.charge[0], drain_charge, 1e-28),
            ("Qg", point.charge[1], gate_charge, 1e-28),
            ("Qb", point.charge[3], bulk_charge, 1e-28),
            ("Von", point.threshold, von, 1e-14),
            ("Vdsat", point.saturation_voltage, sat, 1e-14),
        ] {
            let tolerance = 3e-12f64.mul_add(expected.abs(), absolute);
            assert!(
                (actual - expected).abs() <= tolerance,
                "{bias:?} {label}: {actual:.17e} != {expected:.17e}"
            );
        }
        let norm: f64 = point.charge.iter().map(|q| q.abs()).sum();
        assert!(point.charge.iter().sum::<f64>().abs() <= 2e-15 * norm);
        for column in 0..3 {
            let sum = point.capacitance.iter().map(|row| row[column]).sum::<f64>();
            let norm = point
                .capacitance
                .iter()
                .map(|row| row[column].abs())
                .sum::<f64>();
            assert!(sum.abs() <= 2e-15 * norm);
        }
        count += 1;
    }
    assert_eq!(count, expected_count);
    Ok(())
}

#[test]
fn bsim1_channel_and_both_charge_partitions_match_original_c() -> TestResult {
    check_legacy_channel(
        ModelVariant::Bsim1,
        include_str!("fixtures/legacy-bsim/bsim1-ordinary.txt"),
        include_str!("fixtures/legacy-bsim/bsim1-channel-ordinary.txt"),
        108,
    )?;
    check_legacy_channel(
        ModelVariant::Bsim1,
        include_str!("fixtures/legacy-bsim/bsim1-wide.txt"),
        include_str!("fixtures/legacy-bsim/bsim1-channel-wide.txt"),
        108,
    )
}

#[test]
fn legacy_channel_jacobians_differentiate_the_actual_constitutive_law() -> TestResult {
    use model_compact_intrinsic::{LegacyBsimBias, evaluate_legacy_bsim_intrinsic};
    let step = 1e-5;
    for (variant, fixture) in [
        (
            ModelVariant::Bsim1,
            include_str!("fixtures/legacy-bsim/bsim1-ordinary.txt"),
        ),
        (
            ModelVariant::Bsim1,
            include_str!("fixtures/legacy-bsim/bsim1-wide.txt"),
        ),
        (
            ModelVariant::Bsim2,
            include_str!("fixtures/legacy-bsim/bsim2-channel-model-ordinary.txt"),
        ),
        (
            ModelVariant::Bsim2,
            include_str!("fixtures/legacy-bsim/bsim2-channel-model-wide.txt"),
        ),
    ] {
        let prepared = prepare_fixture(variant, fixture)?;
        for vds in [0.03, 0.23, 0.91, 4.2, 6.2] {
            for vgs in [-1.7, -0.33, 0.27, 0.67, 1.29, 2.57] {
                for vbs in [-1.9, -0.37, 0.07] {
                    let bias = LegacyBsimBias { vds, vgs, vbs };
                    let point = evaluate_legacy_bsim_intrinsic(&prepared, bias)?;
                    for column in 0..3 {
                        let sample = |delta: f64| {
                            let mut shifted = [vds, vgs, vbs];
                            shifted[column] += delta;
                            evaluate_legacy_bsim_intrinsic(
                                &prepared,
                                LegacyBsimBias {
                                    vds: shifted[0],
                                    vgs: shifted[1],
                                    vbs: shifted[2],
                                },
                            )
                        };
                        let [a, b, c, d] = [
                            sample(-2. * step)?,
                            sample(-step)?,
                            sample(step)?,
                            sample(2. * step)?,
                        ];
                        let derivative = |a: f64, b: f64, c: f64, d: f64| {
                            (c - b).mul_add(8., a - d) / (12. * step)
                        };
                        let expected = derivative(
                            a.drain_current,
                            b.drain_current,
                            c.drain_current,
                            d.drain_current,
                        );
                        let actual = point.conductance[column];
                        assert!(
                            (actual - expected).abs() <= 3e-7f64.mul_add(expected.abs(), 1e-12),
                            "{bias:?} column {column} dI: {actual:e} != {expected:e}"
                        );
                        for row in 0..4 {
                            let expected = derivative(
                                a.charge[row],
                                b.charge[row],
                                c.charge[row],
                                d.charge[row],
                            );
                            let actual = point.capacitance[row][column];
                            assert!(
                                (actual - expected).abs() <= 3e-7f64.mul_add(expected.abs(), 1e-20),
                                "{bias:?} column {column} row {row} dQ: {actual:e} != {expected:e}"
                            );
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

#[test]
fn bsim1_large_inversion_and_zero_mobility_limits_are_finite() -> TestResult {
    use model_compact_intrinsic::{LegacyBsimBias, evaluate_bsim1_intrinsic};
    let fixture = include_str!("fixtures/legacy-bsim/bsim1-ordinary.txt");
    let prepared = prepare_fixture(ModelVariant::Bsim1, fixture)?;
    let high = evaluate_bsim1_intrinsic(
        &prepared,
        LegacyBsimBias {
            vds: 1.,
            vgs: 100.,
            vbs: -0.2,
        },
    )?;
    assert!(high.drain_current.is_finite());
    let mut zero = prepared.clone();
    if let LegacyBsimChannel::Bsim1(channel) = &mut zero.channel {
        channel.beta_zero = [0.; 2];
        channel.beta_vdd = [0.; 3];
        channel.k1 = 0.;
    }
    for vgs in [-2., -0.5, 0.2, 100.] {
        let point = evaluate_bsim1_intrinsic(
            &zero,
            LegacyBsimBias {
                vds: 1.,
                vgs,
                vbs: -0.2,
            },
        )?;
        assert!(point.drain_current.abs() < f64::MIN_POSITIVE);
        assert!(
            point
                .conductance
                .iter()
                .all(|x| x.abs() < f64::MIN_POSITIVE)
        );
    }
    assert!(
        evaluate_bsim1_intrinsic(
            &prepared,
            LegacyBsimBias {
                vds: -1.,
                vgs: 0.,
                vbs: 0.
            }
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn bsim2_intrinsic_matches_original_c() -> TestResult {
    check_legacy_channel(
        ModelVariant::Bsim2,
        include_str!("fixtures/legacy-bsim/bsim2-channel-model-ordinary.txt"),
        include_str!("fixtures/legacy-bsim/bsim2-channel-ordinary.txt"),
        224,
    )?;
    check_legacy_channel(
        ModelVariant::Bsim2,
        include_str!("fixtures/legacy-bsim/bsim2-channel-model-wide.txt"),
        include_str!("fixtures/legacy-bsim/bsim2-channel-wide.txt"),
        224,
    )
}
