use model_kernel_bipolar::{
    ModelError, Temperature,
    hicum2::{
        self,
        instance::{self, Bound, InitialVoltages, Options},
        parameters::{Id, Kind, SPECIFICATIONS},
        temperature,
        topology::{Branch, Node, Resistor},
    },
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn close(actual: f64, expected: f64, relative: f64, absolute: f64) {
    let tolerance = relative.mul_add(actual.abs().max(expected.abs()), absolute);
    assert!(
        (actual - expected).abs() <= tolerance,
        "actual={actual:.15e} expected={expected:.15e} tol={tolerance:.7e}"
    );
}

fn field(name: &str) -> Result<Id, Box<dyn std::error::Error>> {
    Id::from_name(name).ok_or_else(|| format!("missing HICUM field {name}").into())
}

fn magnitude_from_spec(name: &str) -> Result<f64, Box<dyn std::error::Error>> {
    let id = field(name)?;
    let spec = id.specification();
    if spec.default != 0.0 {
        return Ok(spec.default);
    }
    if name == "icbar" {
        return Ok(0.2);
    }
    if name == "ibets" {
        return Ok(1e-3);
    }
    if name == "qavl" {
        return Ok(1e-15);
    }
    if name == "kf" || name == "kfre" {
        return Ok(1e-18);
    }
    if name == "c10" {
        return Ok(2e-30);
    }
    if name.starts_with("cj") || name.ends_with("par") || name == "cth" {
        return Ok(1e-15);
    }
    if name == "ireis" || name == "ibeps" || name == "ireps" || name == "ibcxs" {
        return Ok(1e-18);
    }
    Ok(2.0)
}

#[test]
fn default_instance_scaling_is_identity_for_all_136_source_parameters() -> TestResult {
    let model = temperature::Nominal::source_defaults();
    let original = model;
    let default = Options::default();
    let bound = Bound::new(&model, Temperature::new(300.15)?, default)?;
    assert_eq!(bound.ambient, Temperature::new(300.15)?);
    assert_eq!(bound.topology, model.physical_topology()?);
    assert!(!bound.off);
    assert_eq!(bound.initial, InitialVoltages::default());
    for spec in SPECIFICATIONS {
        let id = field(spec.name)?;
        assert_eq!(
            bound.nominal.parameter(id),
            model.parameter(id),
            "default area/m changed {}",
            spec.name
        );
        assert_eq!(model.parameter(id), original.parameter(id));
    }
    let direct = model.prepare(Temperature::new(300.15)?, 0.0)?;
    let prepared = bound.prepare(0.0)?;
    assert_eq!(direct.effective_temperature, prepared.effective_temperature);
    let bias = hicum2::device::Bias {
        nodes: hicum2::device::Nodes {
            b: 0.72,
            bp: 0.72,
            bi: 0.72,
            c: 1.1,
            ci: 1.1,
            ei: 0.0,
            e: 0.0,
            si: 0.0,
            s: 0.0,
        },
        nqs: hicum2::nqs::State::default(),
    };
    let a = direct.device_kernel()?.evaluate(&bias)?;
    let b = prepared.device_kernel()?.evaluate(&bias)?;
    assert_eq!(a.transfer.forward_current, b.transfer.forward_current);
    assert_eq!(a.junction.intrinsic_emitter, b.junction.intrinsic_emitter);
    Ok(())
}

#[test]
fn all_136_model_fields_follow_exact_native_area_times_m_scaling_table() -> TestResult {
    let area_m_names = [
        "qp0", "icbar", "cjei0", "ibeis", "ireis", "ibets", "ibcis", "cjci0", "qavl", "cth",
    ];
    let perimeter = [
        "ibeps", "ireps", "cjep0", "cbepar", "cjcx0", "cbcpar", "ibcxs",
    ];
    let inverse = ["rth", "re", "rci0", "rbx", "rcx", "rbi0"];
    let source = temperature::Nominal::source_defaults();
    let opt = Options {
        area: 2.0,
        multiplicity: 3.0,
        ..Options::default()
    };
    for spec in SPECIFICATIONS {
        let id = field(spec.name)?;
        let mut model = source;
        if spec.kind == Kind::Real
            && (area_m_names.contains(&spec.name)
                || perimeter.contains(&spec.name)
                || inverse.contains(&spec.name)
                || spec.name == "c10"
                || spec.name == "kf"
                || spec.name == "kfre")
        {
            model.assign_parameter(id, magnitude_from_spec(spec.name)?)?;
        }
        let scaled = instance::scale(&model, opt)?;
        let expected_factor = if area_m_names.contains(&spec.name) {
            6.0
        } else if perimeter.contains(&spec.name) {
            3.0
        } else if inverse.contains(&spec.name) {
            1.0 / 6.0
        } else if spec.name == "c10" {
            36.0
        } else if spec.name == "kf" {
            3.0f64.powf(1.0 - model.noise.af)
        } else if spec.name == "kfre" {
            3.0f64.powf(1.0 - model.noise.afre)
        } else {
            1.0
        };
        let unscaled = model.parameter(id);
        close(
            scaled.parameter(id),
            unscaled * expected_factor,
            4e-15,
            1e-35,
        );
        assert_eq!(
            source.parameter(id),
            spec.default,
            "an instance changed the source model {}",
            spec.name
        );
    }
    Ok(())
}

#[test]
fn area_and_m_have_physically_distinct_perimeter_and_noise_dependence() -> TestResult {
    let mut p = temperature::Nominal::source_defaults();
    for (name, value) in [
        ("qp0", 8e-14),
        ("rbi0", 12.0),
        ("cjei0", 5e-15),
        ("cjep0", 2e-15),
        ("cjei0", 6e-15),
        ("cjcx0", 9e-15),
        ("cbepar", 6e-15),
        ("ibeps", 1e-17),
        ("ibcxs", 2e-17),
        ("ibets", 1e-3),
        ("kf", 1e-18),
        ("kfre", 2e-18),
        ("af", 1.7),
        ("afre", 2.1),
        ("flcomp", 2.3),
    ] {
        p.assign_parameter(field(name)?, value)?;
    }
    let area = instance::scale(
        &p,
        Options {
            area: 6.0,
            ..Options::default()
        },
    )?;
    let mult = instance::scale(
        &p,
        Options {
            multiplicity: 6.0,
            ..Options::default()
        },
    )?;
    for name in ["qp0", "cjei0", "ibets", "rbi0", "cjci0"] {
        close(
            area.parameter(field(name)?),
            mult.parameter(field(name)?),
            1e-14,
            0.0,
        );
    }
    for name in ["cjep0", "cjcx0", "cbepar", "ibeps", "ibcxs"] {
        close(
            area.parameter(field(name)?),
            p.parameter(field(name)?),
            1e-14,
            0.0,
        );
        close(
            mult.parameter(field(name)?),
            6.0 * p.parameter(field(name)?),
            1e-14,
            0.0,
        );
    }
    close(area.noise.kf, p.noise.kf, 1e-14, 0.0);
    close(
        mult.noise.kf,
        p.noise.kf * 6.0f64.powf(1.0 - p.noise.af),
        1e-14,
        0.0,
    );
    close(
        mult.noise.kfre,
        p.noise.kfre * 6.0f64.powf(1.0 - p.noise.afre),
        1e-14,
        0.0,
    );
    for name in ["csu", "cjs0", "cscp0", "rsu", "iscs", "itss", "tsf"] {
        assert_eq!(area.parameter(field(name)?), p.parameter(field(name)?));
        assert_eq!(mult.parameter(field(name)?), p.parameter(field(name)?));
    }
    Ok(())
}

#[test]
fn area_scaled_resistor_topology_reduces_only_after_instance_binding() -> TestResult {
    let mut nominal = temperature::Nominal::source_defaults();
    for name in ["rbi0", "rbx", "rcx", "re", "rsu"] {
        nominal.assign_parameter(field(name)?, 0.004)?;
    }
    nominal.assign_parameter(field("flsh")?, 2.0)?;
    nominal.assign_parameter(field("rth")?, 0.004)?;
    let nominal_map = nominal.physical_topology()?;
    assert_eq!(nominal_map.count(), 9);
    let scaled = Bound::new(
        &nominal,
        Temperature::new(300.15)?,
        Options {
            area: 2.0,
            multiplicity: 3.0,
            ..Options::default()
        },
    )?;
    // Each AREA*M-scaled resistor is now below the 1mOhm source gate.
    for resistor in [Resistor::Rbx, Resistor::Rbi, Resistor::Rcx, Resistor::Re] {
        assert!(nominal_map.present(resistor));
        assert!(!scaled.topology.present(resistor));
    }
    assert!(scaled.topology.present(Resistor::Rsu));
    assert!(scaled.topology.thermal_shorted());
    assert_eq!(scaled.topology.count(), 5);
    let (from, to) = scaled.topology.ends(Branch::Rcx);
    assert_eq!(from, to);
    let (from, to) = scaled.topology.ends(Branch::Rsu);
    assert_ne!(from, to);
    assert_eq!(scaled.topology.index(Node::C), 0);
    assert_eq!(scaled.topology.index(Node::B), 1);
    assert_eq!(scaled.topology.index(Node::E), 2);
    assert_eq!(scaled.topology.index(Node::S), 3);
    assert_eq!(nominal.physical_topology()?, nominal_map);
    Ok(())
}

#[test]
fn instance_temperature_override_dt_and_model_dt_are_separate() -> TestResult {
    let mut model = temperature::Nominal::source_defaults();
    model.assign_parameter(field("dt")?, 6.0)?;
    let ambient = Temperature::new(310.15)?;
    let bound = Bound::new(
        &model,
        ambient,
        Options {
            temp_celsius: Some(35.0),
            dt_kelvin: -7.0,
            initial: InitialVoltages {
                vbe: Some(0.7),
                vce: Some(1.4),
                vcs: Some(-0.1),
            },
            off: true,
            ..Options::default()
        },
    )?;
    assert_eq!(bound.ambient, Temperature::new(301.15)?);
    assert_eq!(bound.prepare(0.0)?.effective_temperature.kelvin(), 307.15);
    assert!(bound.off);
    assert_eq!(bound.initial.vbe, Some(0.7));
    assert_eq!(bound.initial.vce, Some(1.4));
    assert_eq!(bound.initial.vcs, Some(-0.1));
    assert_eq!(model.parameter(field("dt")?), 6.0);
    let fallback = Bound::new(
        &model,
        ambient,
        Options {
            dt_kelvin: 3.0,
            ..Options::default()
        },
    )?;
    assert_eq!(fallback.ambient, Temperature::new(313.15)?);
    assert_eq!(
        fallback.prepare(0.0)?.effective_temperature.kelvin(),
        319.15
    );
    Ok(())
}

#[test]
fn invalid_instance_multipliers_temperatures_and_ics_fail_before_scaling() -> TestResult {
    let model = temperature::Nominal::source_defaults();
    let ambient = Temperature::new(300.15)?;
    let mut options = Options::default();
    for invalid in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        options.area = invalid;
        assert!(matches!(
            Bound::new(&model, ambient, options),
            Err(ModelError::InvalidParameter("area"))
        ));
        options = Options {
            multiplicity: invalid,
            ..Options::default()
        };
        assert!(matches!(
            Bound::new(&model, ambient, options),
            Err(ModelError::InvalidParameter("m"))
        ));
    }
    for wrong in [
        Options {
            area: 1e308,
            multiplicity: 1e308,
            ..Options::default()
        },
        Options {
            area: 1e-300,
            multiplicity: 1e-300,
            ..Options::default()
        },
    ] {
        assert!(matches!(
            Bound::new(&model, ambient, wrong),
            Err(ModelError::InvalidParameter("area*m"))
        ));
    }
    Ok(())
}

#[test]
fn invalid_instance_temperatures_and_initial_voltages_fail_at_binding() -> TestResult {
    let model = temperature::Nominal::source_defaults();
    let ambient = Temperature::new(300.15)?;
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(matches!(
            Bound::new(
                &model,
                ambient,
                Options {
                    temp_celsius: Some(bad),
                    ..Options::default()
                }
            ),
            Err(ModelError::InvalidParameter("temp"))
        ));
        assert!(matches!(
            Bound::new(
                &model,
                ambient,
                Options {
                    dt_kelvin: bad,
                    ..Options::default()
                }
            ),
            Err(ModelError::InvalidParameter("dt"))
        ));
        assert!(matches!(
            Bound::new(
                &model,
                ambient,
                Options {
                    initial: InitialVoltages {
                        vbe: Some(bad),
                        ..InitialVoltages::default()
                    },
                    ..Options::default()
                }
            ),
            Err(ModelError::InvalidParameter("ic_vbe"))
        ));
        assert!(matches!(
            Bound::new(
                &model,
                ambient,
                Options {
                    initial: InitialVoltages {
                        vce: Some(bad),
                        ..InitialVoltages::default()
                    },
                    ..Options::default()
                }
            ),
            Err(ModelError::InvalidParameter("ic_vce"))
        ));
        assert!(matches!(
            Bound::new(
                &model,
                ambient,
                Options {
                    initial: InitialVoltages {
                        vcs: Some(bad),
                        ..InitialVoltages::default()
                    },
                    ..Options::default()
                }
            ),
            Err(ModelError::InvalidParameter("ic_vcs"))
        ));
    }
    assert!(matches!(
        Bound::new(
            &model,
            ambient,
            Options {
                temp_celsius: Some(-500.0),
                ..Options::default()
            }
        ),
        Err(ModelError::InvalidTemperature)
    ));
    Ok(())
}

#[test]
fn native_ngspice47_level8_area_and_m_physical_operating_points() -> TestResult {
    let reference = include_str!("fixtures/hicum2_ngspice47_instance_scaling.csv");
    let model = temperature::Nominal::source_defaults();
    let mut count = 0;
    let mut maximum_relative = [0.0_f64; 4];
    for row in reference.lines().filter(|row| !row.starts_with('#')) {
        let [
            area,
            m,
            ambient,
            vbe,
            vce,
            ic,
            emitter_report,
            collector_report,
            ick,
            tf,
        ] = <[f64; 10]>::try_from(
            row.split(',')
                .map(str::parse::<f64>)
                .collect::<Result<Vec<_>, _>>()?,
        )
        .map_err(|_| "invalid ngspice47 HICUM instance fixture row")?;
        let bound = Bound::new(
            &model,
            Temperature::new(ambient + 273.15)?,
            Options {
                area,
                multiplicity: m,
                ..Options::default()
            },
        )?;
        let prepared = bound.prepare(0.0)?;
        let be = vbe;
        let bc = vbe - vce;
        let transfer = prepared
            .transport_kernel()?
            .evaluate(hicum2::transport::Bias {
                vbiei: be,
                vbici: bc,
            })?;
        let diode = prepared.diode_kernel()?.evaluate(hicum2::Bias {
            vbiei: be,
            vbpei: be,
            vbici: bc,
            vbpci: bc,
            vsici: -1.0,
        })?;
        // The native ngspice model adds solver-owned Gmin currents
        // that are not part of HICUM compact-model physics.
        let terminal_collector = 1e-12f64.mul_add(
            3.0f64.mul_add(-bc, be),
            transfer.net_transfer_current.value - diode.ibci.amperes,
        );
        let actual = [
            terminal_collector,
            diode.ibei.amperes,
            1e-12f64.mul_add(bc, diode.ibci.amperes),
            transfer.critical_current.value,
        ];
        for (index, (predicted, observed)) in actual
            .into_iter()
            .zip([ic, emitter_report, collector_report, ick])
            .enumerate()
        {
            let error =
                (predicted - observed).abs() / predicted.abs().max(observed.abs()).max(1e-20);
            maximum_relative[index] = maximum_relative[index].max(error);
            close(predicted, observed, 1e-4, 1e-20);
        }
        // Tf is zero in the unmodified source defaults.
        assert_eq!(tf, 0.0);
        count += 1;
    }
    assert_eq!(count, 120);
    assert!(maximum_relative[0] < 1e-4);
    Ok(())
}

#[test]
fn extreme_scale_factors_avoid_intermediate_square_and_zero_noise_overflow() -> TestResult {
    let mut model = temperature::Nominal::source_defaults();
    model.assign_parameter(field("c10")?, 1e-35)?;
    model.assign_parameter(field("kf")?, 0.0)?;
    model.assign_parameter(field("kfre")?, 0.0)?;
    // AREA*M squared is greater than f64::MAX, but C10*(AREA*M)^2
    // remains representable. The intermediate multiplication must
    // not make a physically representable coefficient fail.
    let scaled = instance::scale(
        &model,
        Options {
            area: 1e160,
            multiplicity: 1.0,
            ..Options::default()
        },
    )?;
    close(scaled.transport.c10, 1e285, 5e-15, 0.0);
    // With AREA*M close to one, an extremely small M must not cause
    // the disabled KF terms to multiply by an infinite M^(1-AF).
    let scaled = instance::scale(
        &model,
        Options {
            area: 1e200,
            multiplicity: 1e-200,
            ..Options::default()
        },
    )?;
    assert_eq!(scaled.noise.kf, 0.0);
    assert_eq!(scaled.noise.kfre, 0.0);
    close(scaled.transport.c10, model.transport.c10, 1e-14, 0.0);
    Ok(())
}
