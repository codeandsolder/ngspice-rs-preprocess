use model_kernel_bipolar::{
    ModelError, Temperature,
    hicum2::{
        self,
        binding::{self, Assignment, Error, Value},
        instance::InitialVoltages,
        parameters::{Id, Kind, SPECIFICATIONS},
        temperature,
    },
};
type TestResult = Result<(), Box<dyn std::error::Error>>;
const fn scalar(name: &str, value: f64) -> Assignment<'_> {
    Assignment {
        name,
        value: Value::Scalar(value),
    }
}
const fn flag(name: &str) -> Assignment<'_> {
    Assignment {
        name,
        value: Value::Flag,
    }
}

fn id(name: &str) -> Result<Id, Box<dyn std::error::Error>> {
    Id::from_name(name).ok_or_else(|| format!("unknown source HICUM parameter {name}").into())
}

#[test]
fn scalar_model_card_directly_binds_all_136_source_coefficient_defaults() -> TestResult {
    let mut assignments = vec![scalar("LEVEL", 8.0)];
    assignments.extend(
        SPECIFICATIONS
            .iter()
            .map(|spec| scalar(spec.name, spec.default)),
    );
    let model = binding::model("NpN", Some(8.0), &assignments)?;
    let source = temperature::Nominal::source_defaults();
    for spec in SPECIFICATIONS {
        let slot = id(spec.name)?;
        assert_eq!(
            model.parameter(slot),
            source.parameter(slot),
            "{}",
            spec.name
        );
    }
    assert_eq!(model.polarity, hicum2::Polarity::Npn);
    let prepared = model.prepare(Temperature::new(300.15)?, 0.0)?;
    assert_eq!(prepared.polarity, hicum2::Polarity::Npn);
    Ok(())
}

#[test]
fn every_source_slot_bound_by_name_has_identical_typed_semantics() -> TestResult {
    for spec in SPECIFICATIONS {
        let slot = id(spec.name)?;
        let different = match (spec.kind, spec.name) {
            (Kind::Integer, "type") => -1.0,
            (Kind::Integer, "cfbe") => -2.0,
            (Kind::Integer, "flsh") => 2.0,
            (Kind::Integer, _) => 1.0,
            (Kind::Real, _) if spec.accepts(0.0) && spec.default != 0.0 => 0.0,
            (Kind::Real, _) if spec.accepts(0.125) => 0.125,
            _ => spec.default,
        };
        assert!(spec.accepts(different), "{}", spec.name);
        let card = binding::model(
            "NPN",
            Some(8.0),
            &[scalar("LeVeL", 8.0), scalar(spec.name, different)],
        )?;
        let mut direct = temperature::Nominal::source_defaults();
        direct.assign_parameter(slot, different)?;
        assert_eq!(
            card.parameter(slot),
            direct.parameter(slot),
            "{}",
            spec.name
        );
    }
    Ok(())
}

#[test]
fn model_type_flags_source_type_and_duplicate_setters_apply_in_card_order() -> TestResult {
    let card = binding::model(
        "PNP",
        Some(8.0),
        &[
            scalar("level", 8.0),
            flag("npn"),
            scalar("type", -1.0),
            flag("npn"),
            flag("pnp"),
            scalar("type", 1.0),
            scalar("rbx", 3.0),
            scalar("RBX", 5.0),
            scalar("tref", 42.0),
        ],
    )?;
    assert_eq!(card.polarity, hicum2::Polarity::Npn);
    assert_eq!(card.parameter(id("rbx")?), 5.0);
    assert_eq!(card.parameter(id("tnom")?), 42.0);
    assert_eq!(
        binding::model("pnp", Some(8.0), &[])?.polarity,
        hicum2::Polarity::Pnp
    );
    Ok(())
}

#[test]
fn unsupported_levels_types_and_unknown_setters_fail_at_binding() {
    for level in [
        None,
        Some(0.0),
        Some(1.0),
        Some(7.0),
        Some(8.0001),
        Some(f64::NAN),
    ] {
        assert!(matches!(
            binding::model("npn", level, &[]),
            Err(Error::Level(_))
        ));
    }
    for base in ["hicum", "d", "nmos", "", "hIcUm2"] {
        assert!(matches!(
            binding::model(base, Some(8.0), &[]),
            Err(Error::ModelType(_))
        ));
    }
    assert!(matches!(
        binding::model(
            "npn",
            Some(8.0),
            &[scalar("level", 8.0), scalar("level", 7.0)]
        ),
        Err(Error::Level(Some(7.0)))
    ));
    assert!(matches!(
        binding::model(
            "npn",
            Some(8.0),
            &[
                scalar("level", 8.0),
                scalar("qp0", 2e-14),
                scalar("nonsense", 1.0)
            ]
        ),
        Err(Error::UnknownParameter { assignment: 2, .. })
    ));
    for wrong in [flag("qp0"), scalar("npn", 1.0), flag("type"), flag("level")] {
        assert!(matches!(
            binding::model("npn", Some(8.0), &[wrong]),
            Err(Error::WrongKind { assignment: 0, .. })
        ));
    }
    for (name, value) in [
        ("qp0", 0.0),
        ("type", 0.0),
        ("flsh", 3.0),
        ("t0", -1.0),
        ("flnqs", 1.5),
        ("vlim", f64::NAN),
        ("qp0", f64::INFINITY),
    ] {
        assert!(matches!(
            binding::model(
                "npn",
                Some(8.0),
                &[scalar("level", 8.0), scalar(name, value)]
            ),
            Err(Error::InvalidValue { assignment: 1, .. })
        ));
    }
}

#[test]
fn instance_card_resolves_all_scalar_vector_flag_and_temperature_settings() -> TestResult {
    let model = binding::model(
        "PNP",
        Some(8.0),
        &[
            scalar("level", 8.0),
            scalar("rbx", 9.0),
            scalar("rbi0", 12.0),
            scalar("rcx", 15.0),
            scalar("re", 6.0),
            scalar("rsu", 3.0),
            scalar("qp0", 4e-14),
            scalar("dt", 5.0),
            scalar("flcomp", 2.3),
        ],
    )?;
    let initial = InitialVoltages {
        vbe: Some(-0.82),
        vce: Some(-1.5),
        vcs: None,
    };
    let bound = binding::instance(
        &model,
        Temperature::new(290.15)?,
        &[
            scalar("AREA", 2.0),
            scalar("M", 3.0),
            scalar("TEMP", 40.0),
            scalar("DT", -7.0),
            flag("OFF"),
            Assignment {
                name: "IC",
                value: Value::InitialVoltages(initial),
            },
        ],
    )?;
    assert_eq!(bound.nominal.polarity, hicum2::Polarity::Pnp);
    assert_eq!(bound.ambient, Temperature::new(306.15)?);
    assert_eq!(bound.prepare(0.0)?.effective_temperature.kelvin(), 311.15);
    assert_eq!(bound.nominal.auxiliary.rbx, 9.0 / 6.0);
    assert_eq!(bound.nominal.auxiliary.rbi0, 12.0 / 6.0);
    assert_eq!(bound.nominal.auxiliary.rcx, 15.0 / 6.0);
    assert_eq!(bound.nominal.auxiliary.re, 6.0 / 6.0);
    assert_eq!(bound.nominal.auxiliary.rsu, 3.0);
    assert_eq!(bound.nominal.transport.qp0, 4e-14 * 6.0);
    assert_eq!(model.auxiliary.rbx, 9.0);
    assert_eq!(model.transport.qp0, 4e-14);
    assert_eq!(bound.initial, initial);
    assert!(bound.off);
    assert_eq!(bound.topology.count(), 9);
    let no_instance = binding::instance(&model, Temperature::new(290.15)?, &[])?;
    assert_eq!(no_instance.nominal.transport.qp0, model.transport.qp0);
    assert_eq!(no_instance.ambient, Temperature::new(290.15)?);
    Ok(())
}

#[test]
fn invalid_instance_names_kinds_and_physical_ranges_fail_before_kernel_creation() -> TestResult {
    let model = temperature::Nominal::source_defaults();
    let ambient = Temperature::new(300.15)?;
    for assignment in [
        scalar("kf", 1e-16),
        scalar("flsh", 2.0),
        scalar("t0", 3e-12),
        scalar("temperature", 300.0),
    ] {
        assert!(matches!(
            binding::instance(&model, ambient, &[scalar("area", 2.0), assignment]),
            Err(Error::UnknownParameter { assignment: 1, .. })
        ));
    }
    for assignment in [
        flag("area"),
        flag("dt"),
        flag("m"),
        scalar("ic", 0.0),
        scalar("off", 1.0),
        Assignment {
            name: "temp",
            value: Value::InitialVoltages(InitialVoltages::default()),
        },
    ] {
        assert!(matches!(
            binding::instance(&model, ambient, &[assignment]),
            Err(Error::WrongKind { assignment: 0, .. })
        ));
    }
    for invalid in [0.0, -1.0, 1e-300, f64::NAN, f64::INFINITY] {
        let result = binding::instance(
            &model,
            ambient,
            &[scalar("area", invalid), scalar("m", 1e-300)],
        );
        assert!(result.is_err());
    }
    assert!(matches!(
        binding::instance(&model, ambient, &[scalar("area", 0.0)]),
        Err(Error::Instance(ModelError::InvalidParameter("area")))
    ));
    assert!(matches!(
        binding::instance(&model, ambient, &[scalar("temp", f64::NAN)]),
        Err(Error::InvalidValue { assignment: 0, .. })
    ));
    assert!(matches!(
        binding::instance(
            &model,
            ambient,
            &[Assignment {
                name: "ic",
                value: Value::InitialVoltages(InitialVoltages {
                    vbe: Some(f64::NAN),
                    ..InitialVoltages::default()
                })
            }]
        ),
        Err(Error::Instance(ModelError::InvalidParameter("ic_vbe")))
    ));
    Ok(())
}

#[test]
fn native_textual_type_and_exact_compact_model_version_are_validated() -> TestResult {
    let card = binding::model(
        "npn",
        Some(8.0),
        &[
            scalar("level", 8.0),
            Assignment {
                name: "version",
                value: Value::Text("2.4.0"),
            },
            Assignment {
                name: "type",
                value: Value::Text("PnP"),
            },
            scalar("rbx", 5.0),
        ],
    )?;
    assert_eq!(card.polarity, hicum2::Polarity::Pnp);
    assert_eq!(card.parameter(id("rbx")?), 5.0);
    let card = binding::model(
        "pnp",
        Some(8.0),
        &[
            Assignment {
                name: "TYPE",
                value: Value::Text("NPN"),
            },
            Assignment {
                name: "type",
                value: Value::Scalar(-1.0),
            },
        ],
    )?;
    assert_eq!(card.polarity, hicum2::Polarity::Pnp);
    for version in ["2.1", "2.3", "2.4", "2.5.0", "2.4.1", "invalid"] {
        assert!(matches!(
            binding::model(
                "npn",
                Some(8.0),
                &[Assignment {
                    name: "version",
                    value: Value::Text(version)
                }]
            ),
            Err(Error::Version(_))
        ));
    }
    for kind in ["NJF", "bjt", "positive", "1", ""] {
        assert!(matches!(
            binding::model(
                "npn",
                Some(8.0),
                &[Assignment {
                    name: "type",
                    value: Value::Text(kind)
                }]
            ),
            Err(Error::ModelType(_))
        ));
    }
    assert!(matches!(
        binding::model("npn", Some(8.0), &[scalar("version", 2.4)]),
        Err(Error::WrongKind { assignment: 0, .. })
    ));
    assert!(matches!(
        binding::model(
            "npn",
            Some(8.0),
            &[Assignment {
                name: "type",
                value: Value::InitialVoltages(InitialVoltages::default())
            }]
        ),
        Err(Error::WrongKind { assignment: 0, .. })
    ));
    Ok(())
}
