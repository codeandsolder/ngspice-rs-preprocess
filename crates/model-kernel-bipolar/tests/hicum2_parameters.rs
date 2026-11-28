use model_kernel_bipolar::{
    ModelError, Temperature,
    hicum2::{
        self,
        parameters::{Id, Kind, SPECIFICATIONS},
        temperature,
    },
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn slot(name: &str) -> Result<Id, Box<dyn std::error::Error>> {
    Id::from_name(name).ok_or_else(|| format!("missing 2.4.0 source slot: {name}").into())
}

#[test]
fn all_136_source_declarations_have_unique_typed_id_matching_nominal_defaults() -> TestResult {
    let mut native = temperature::Nominal::source_defaults();
    assert_eq!(SPECIFICATIONS.len(), 136);
    assert_eq!(
        SPECIFICATIONS
            .iter()
            .filter(|x| x.kind == Kind::Integer)
            .count(),
        6
    );
    let mut covered = [false; 136];
    for (index, spec) in SPECIFICATIONS.iter().copied().enumerate() {
        let id = slot(spec.name)?;
        assert_eq!(id.specification(), spec);
        assert!(
            spec.accepts(spec.default),
            "published default outside source range: {}",
            spec.name
        );
        assert_eq!(
            native.parameter(id),
            spec.default,
            "source-default factory mismatch for {}",
            spec.name
        );
        // Assignment of every source default must be idempotent and
        // prepare successfully without needing cross-field stubs.
        native.assign_parameter(id, spec.default)?;
        assert_eq!(native.parameter(id), spec.default);
        assert!(!covered[index]);
        covered[index] = true;
        let upper = spec.name.to_ascii_uppercase();
        assert_eq!(Id::from_name(&upper), Some(id));
        for other in SPECIFICATIONS.iter().take(index) {
            assert_ne!(other.name, spec.name);
        }
    }
    assert!(covered.iter().all(|x| *x));
    assert!(Id::from_name("certainly_not_hicum24").is_none());
    assert!(Id::from_name("").is_none());
    assert!(Id::from_name("version").is_none());
    assert_eq!(
        native.prepare(Temperature::new(300.15)?, 0.0)?.polarity,
        hicum2::Polarity::Npn
    );
    Ok(())
}

#[test]
fn source_numeric_ranges_apply_open_closed_integer_and_excluded_zero_boundaries() -> TestResult {
    for spec in SPECIFICATIONS {
        let id = slot(spec.name)?;
        let mut nominal = temperature::Nominal::source_defaults();
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(matches!(nominal.assign_parameter(id,bad),
                Err(ModelError::InvalidParameter(name)) if name==spec.name));
        }
        if spec.minimum.is_finite() {
            let below = spec.minimum - 1.0f64.max(spec.minimum.abs() * 1e-5);
            assert!(!spec.accepts(below));
            assert!(matches!(nominal.assign_parameter(id,below),
                Err(ModelError::InvalidParameter(name)) if name==spec.name));
            if !spec.include_minimum {
                assert!(!spec.accepts(spec.minimum));
            }
        }
        if spec.maximum.is_finite() {
            let above = spec.maximum + 1.0f64.max(spec.maximum.abs() * 1e-5);
            assert!(!spec.accepts(above));
            assert!(matches!(nominal.assign_parameter(id,above),
                Err(ModelError::InvalidParameter(name)) if name==spec.name));
            if !spec.include_maximum {
                assert!(!spec.accepts(spec.maximum));
            }
        }
        if spec.exclude_zero {
            assert!(!spec.accepts(0.0));
            assert!(matches!(nominal.assign_parameter(id,0.0),
                Err(ModelError::InvalidParameter(name)) if name==spec.name));
        }
        if spec.kind == Kind::Integer {
            assert!(!spec.accepts(0.25));
        }
        assert_eq!(
            nominal.parameter(id),
            spec.default,
            "invalid assignment mutated source model: {}",
            spec.name
        );
    }
    Ok(())
}

#[test]
fn all_named_assignments_roundtrip_without_truncation_or_runtime_lookup() -> TestResult {
    for spec in SPECIFICATIONS {
        let id = slot(spec.name)?;
        let mut nominal = temperature::Nominal::source_defaults();
        let legal_value = if spec.kind == Kind::Integer {
            match spec.name {
                "type" => -1.0,
                "cfbe" => -2.0,
                "tunode" | "flnqs" | "flcono" => 1.0,
                "flsh" => 2.0,
                other => return Err(format!("unexpected source integer selector {other}").into()),
            }
        } else if spec.name == "flcomp" {
            4.0 // The source's real FLcomp domain is [0,+inf), not [0,3].
        } else if spec.default != 0.0 && spec.accepts(0.0) {
            0.0
        } else {
            spec.default
        };
        assert!(spec.accepts(legal_value));
        nominal.assign_parameter(id, legal_value)?;
        assert_eq!(
            nominal.parameter(id),
            legal_value,
            "typed field binding lost precision for {}",
            spec.name
        );
    }
    Ok(())
}

#[test]
fn coherent_temperature_and_charge_model_card_assignments_reprepare_all_families() -> TestResult {
    let mut nominal = temperature::Nominal::source_defaults();
    let updates = [
        ("TYPE", -1.0),
        ("FLcomp", 2.3),
        ("C10", 3e-30),
        ("Qp0", 5e-14),
        ("IBEIS", 5e-18),
        ("IBcis", 3e-16),
        ("RBI0", 12.0),
        ("RBX", 4.0),
        ("RCX", 6.0),
        ("RE", 3.0),
        ("RSU", 7.0),
        ("CSU", 4e-15),
        ("CJCX0", 8e-15),
        ("CBCPAR", 6e-15),
        ("FBCPAR", 0.5),
        ("CBEpar", 2e-15),
        ("FBEpar", 0.4),
        ("CJCI0", 3e-15),
        ("CJEP0", 4e-15),
        ("CJEI0", 6e-15),
        ("TNOM", 27.0),
        ("DT", 6.0),
        ("ZETARBI", 0.7),
        ("FLSH", 2.0),
        ("RTH", 250.0),
        ("CTH", 2e-9),
        ("FLNQS", 1.0),
        ("ALIT", 0.333),
        ("ALQF", 0.167),
        ("T0", 5e-12),
        ("KF", 1e-20),
        ("FLcono", 1.0),
        ("TUNODE", 0.0),
        ("AF", 1.7),
    ];
    let values = updates
        .into_iter()
        .map(|(name, value)| Ok((slot(name)?, value)))
        .collect::<Result<Vec<_>, Box<dyn std::error::Error>>>()?;
    nominal.assign_parameters(&values)?;
    for (id, value) in &values {
        assert_eq!(nominal.parameter(*id), *value);
    }
    assert_eq!(nominal.polarity, hicum2::Polarity::Pnp);
    let p = nominal.prepare(Temperature::new(330.15)?, 8.0)?;
    assert_eq!(p.effective_temperature.kelvin(), 344.15);
    assert!(p.nqs_kernel()?.is_active(5e-12));
    assert!(p.external_kernel().split().external_collector_junction > 0.0);
    assert!(!nominal.physical_topology()?.thermal_shorted());
    assert_eq!(nominal.physical_topology()?.count(), 9);
    let evaluator = p.device_kernel()?;
    let physical = evaluator.evaluate(&hicum2::device::Bias {
        nodes: hicum2::device::Nodes {
            b: -0.78,
            bp: -0.77,
            bi: -0.76,
            c: -1.15,
            ci: -1.10,
            ei: -0.02,
            e: 0.0,
            si: 0.1,
            s: 0.11,
        },
        nqs: hicum2::nqs::State::default(),
    })?;
    assert!(!physical.thermal.shorted);
    assert!(physical.transfer.net_transfer_current.value.is_finite());
    assert!(
        !evaluator
            .source_stamps(&physical)?
            .thermal
            .current
            .value
            .is_nan()
    );
    Ok(())
}

#[test]
fn invalid_multi_parameter_model_cards_leave_original_nominal_model_unchanged() -> TestResult {
    let mut nominal = temperature::Nominal::source_defaults();
    let original = nominal;
    let values = [
        (slot("RBX")?, 10.0),
        (slot("C10")?, 1e-30),
        (slot("FBCPAR")?, 2.0),
        (slot("RTH")?, 500.0),
    ];
    assert!(matches!(
        nominal.assign_parameters(&values),
        Err(ModelError::InvalidParameter("fbcpar"))
    ));
    for spec in SPECIFICATIONS {
        let id = slot(spec.name)?;
        assert_eq!(
            nominal.parameter(id),
            original.parameter(id),
            "partial model card update leaked into {}",
            spec.name
        );
    }
    Ok(())
}
