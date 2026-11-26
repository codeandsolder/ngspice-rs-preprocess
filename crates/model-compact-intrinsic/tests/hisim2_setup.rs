use model_compact_intrinsic::{
    BoundParameters, ModelError, ModelVariant, MosPolarity, ParameterScope, ParameterValue,
    hisim2::{
        BodyNetwork, ChargeDynamics, PreparedInstance, PreparedModel, PrimeNode, prepare_instance,
        prepare_model,
    },
};

const FAMILY: ModelVariant = ModelVariant::HiSim2;

fn bound(
    scope: ParameterScope,
    fields: &[(&str, ParameterValue)],
) -> Result<BoundParameters, ModelError> {
    BoundParameters::bind(FAMILY, scope, fields.iter().map(|(n, v)| (*n, v.clone())))
}

fn model(fields: &[(&str, ParameterValue)]) -> Result<PreparedModel, ModelError> {
    prepare_model(&bound(ParameterScope::Model, fields)?)
}

fn instance(
    m: &PreparedModel,
    fields: &[(&str, ParameterValue)],
    t: f64,
) -> Result<PreparedInstance, ModelError> {
    prepare_instance(m, &bound(ParameterScope::Instance, fields)?, t)
}

fn slot(scope: ParameterScope, name: &str) -> Result<u16, ModelError> {
    FAMILY
        .parameter(scope, name)
        .map(|spec| spec.slot)
        .ok_or_else(|| ModelError::UnknownParameter(name.to_owned()))
}

fn get<'a>(p: &'a BoundParameters, name: &str) -> Result<&'a ParameterValue, ModelError> {
    let index = slot(p.scope(), name)?;
    p.get_slot(index)
        .ok_or(ModelError::MissingDefaultDependency(index))
}

#[test]
fn full_model_defaults_preserve_family_specific_modes() -> Result<(), ModelError> {
    use ParameterValue::{Flag, Integer, Real};
    for pmos in [false, true] {
        let m = model(&[("pmos", Flag(pmos))])?;
        assert_eq!(
            m.polarity(),
            if pmos {
                MosPolarity::Pmos
            } else {
                MosPolarity::Nmos
            }
        );
        for spec in FAMILY.parameters(ParameterScope::Model) {
            if !matches!(spec.name, "nmos" | "pmos") {
                assert!(
                    m.parameters().get_slot(spec.slot).is_some(),
                    "{}",
                    spec.name
                );
            }
        }
        assert_eq!(get(m.parameters(), "version")?, &Integer(280));
        assert_eq!(
            get(m.parameters(), "mueph1")?,
            &Real(if pmos { 9e3 } else { 25e3 })
        );
        assert_eq!(
            get(m.parameters(), "bb")?,
            &Real(if pmos { 1.0 } else { 2.0 })
        );
        assert_eq!(
            get(m.parameters(), "vgsmin")?,
            &Real(if pmos { 5.0 } else { -5.0 })
        );
    }
    let m = model(&[
        ("codep", Integer(1)),
        ("coddlt", Integer(0)),
        ("corecip", Integer(1)),
        ("coisti", Integer(1)),
        ("corsrd", Integer(1)),
        ("noise", Integer(0)),
        ("ndepm", Real(1e19)),
        ("tndep", Real(2e-6)),
        ("nsubpfac", Real(2.0)),
        ("nsubpdlt", Real(0.2)),
        ("vfbc", Real(0.35)),
    ])?;
    for (name, expected) in [
        ("codep", 1),
        ("corecip", 0),
        ("coisti", 0),
        ("copprv", 0),
        ("corsrd", -1),
        ("noise", 1),
    ] {
        assert_eq!(get(m.parameters(), name)?, &Integer(expected), "{name}");
    }
    for (name, expected) in [
        ("ndepm", 2e17),
        ("tndep", 1e-6),
        ("nsubpfac", 1.0),
        ("nsubpdlt", 0.1),
        ("vfbc", 0.35),
        ("nsubc", 5e16),
        ("lp", 0.0),
        ("nsubp", 1e17),
        ("muesr1", 5e15),
        ("ddltslp", 0.0),
        ("ddltict", 10.0),
    ] {
        assert_eq!(get(m.parameters(), name)?, &Real(expected), "{name}");
    }
    assert!(m.explicitly_given(slot(ParameterScope::Model, "corecip")?));
    assert!(!m.explicitly_given(slot(ParameterScope::Model, "nsubc")?));
    let m = model(&[
        ("sc2", Real(1.0)),
        ("psc4", Real(2.0)),
        ("coqy", Integer(1)),
    ])?;
    assert_eq!(get(m.parameters(), "sc2")?, &Real(0.0));
    assert_eq!(get(m.parameters(), "psc4")?, &Real(0.0));
    assert_eq!(get(m.parameters(), "coqy")?, &Integer(0));
    Ok(())
}

#[test]
fn geometry_binning_topology_and_temperature_follow_source() -> Result<(), ModelError> {
    use ParameterValue::{Integer, Real, RealVector};
    let m = model(&[
        ("xl", Real(1e-6)),
        ("xw", Real(2e-6)),
        ("lbinn", Real(2.0)),
        ("wbinn", Real(1.0)),
        ("vmax", Real(10.0)),
        ("lvmax", Real(8.0)),
        ("wvmax", Real(12.0)),
        ("pvmax", Real(24.0)),
        ("vfbc", Real(-0.5)),
        ("vfbcl", Real(2.0)),
        ("vfbclp", Real(2.0)),
        ("corsrd", Integer(-1)),
        ("rd", Real(1.0)),
        ("corg", Integer(1)),
        ("rshg", Real(2.0)),
        ("corbnet", Integer(1)),
        ("rbdb", Real(80.0)),
        ("conqs", Integer(1)),
    ])?;
    let i = instance(
        &m,
        &[
            ("l", Real(1e-6)),
            ("w", Real(8e-6)),
            ("nf", Real(2.0)),
            ("dtemp", Real(5.0)),
            ("ic", RealVector(vec![0.1, 0.2])),
        ],
        325.0,
    )?;
    assert_eq!(i.geometry().length_bin, 4.0);
    assert_eq!(i.geometry().width_bin, 6.0);
    assert_eq!(
        i.binned_model_slot(slot(ParameterScope::Model, "vmax")?),
        Some(15.0)
    );
    assert_eq!(
        i.binned_model_slot(slot(ParameterScope::Model, "vfbc")?),
        Some(-0.75)
    );
    assert_eq!(
        FAMILY
            .parameters(ParameterScope::Model)
            .iter()
            .filter(|s| i.binned_model_slot(s.slot).is_some())
            .count(),
        72
    );
    assert_eq!(i.initial_conditions(), [0.1, 0.2, 0.0]);
    assert_eq!(i.temperature_kelvin(), 330.0);
    assert_eq!(i.topology().drain_prime, PrimeNode::Internal);
    assert_eq!(i.topology().gate_prime, PrimeNode::Internal);
    assert_eq!(i.topology().source_prime, PrimeNode::External);
    assert_eq!(i.topology().body_network, BodyNetwork::Distributed);
    assert_eq!(i.topology().charge_dynamics, ChargeDynamics::NonQuasistatic);
    assert_eq!(get(i.parameters(), "rbdb")?, &Real(80.0));
    assert_eq!(get(i.parameters(), "nrd")?, &Real(0.0));
    let i = instance(
        &m,
        &[
            ("temp", Real(50.0)),
            ("dtemp", Real(-5.0)),
            ("corbnet", Integer(0)),
        ],
        400.0,
    )?;
    assert_eq!(i.temperature_kelvin(), 318.15);
    assert_eq!(i.topology().body_network, BodyNetwork::Collapsed);
    assert!(i.explicitly_given(slot(ParameterScope::Instance, "temp")?));
    Ok(())
}

#[test]
fn invalid_modes_and_instance_vectors_are_not_silently_rebound() -> Result<(), ModelError> {
    use ParameterValue::{Integer, Real, RealVector};
    for (name, value) in [
        ("version", 270),
        ("level", 73),
        ("codep", 2),
        ("coddlt", -1),
        ("corbnet", 2),
    ] {
        assert!(
            matches!(
                model(&[(name, Integer(value))]),
                Err(ModelError::InvalidSelector { .. })
            ),
            "{name}"
        );
    }
    let m = model(&[])?;
    for fields in [
        vec![("nf", Real(0.0))],
        vec![("l", Real(-1.0))],
        vec![("corg", Integer(3))],
        vec![("ic", RealVector(vec![1.0; 4]))],
        vec![("ic", RealVector(Vec::new()))],
        vec![("temp", Real(-300.0))],
    ] {
        assert!(instance(&m, &fields, 300.0).is_err(), "{fields:?}");
    }
    Ok(())
}

#[cfg(target_os = "linux")]
mod native {
    use super::*;
    use std::{collections::BTreeMap, error::Error, fmt::Write as _, fs, process::Command};

    type Fields<'a> = &'a [(&'a str, ParameterValue)];

    struct Case<'a> {
        family: &'a str,
        model: Fields<'a>,
        instance: Fields<'a>,
    }

    fn card(fields: Fields<'_>) -> Result<String, Box<dyn Error>> {
        let mut result = String::new();
        for (name, value) in fields {
            match value {
                ParameterValue::Real(v) => write!(result, " {name}={v:e}")?,
                ParameterValue::Integer(v) => write!(result, " {name}={v}")?,
                ParameterValue::Flag(true) => write!(result, " {name}")?,
                ParameterValue::Flag(false) => (),
                _ => return Err(format!("unexpected native fixture field {name}").into()),
            }
        }
        Ok(result)
    }

    fn native_output(case: &Case<'_>) -> Result<String, Box<dyn Error>> {
        let temp = tempfile::tempdir()?;
        let polarity = if case.family == "pmos" { "-" } else { "" };
        let deck = format!(
            "HiSIM2 native setup oracle\n.model ref {} level=68{}\nVd d 0 {polarity}0.5\nVg g 0 {polarity}1.2\nM1 d g 0 0 ref{}\n.control\nset num_threads=1\nop\nshowmod m1\nshow m1\nquit\n.endc\n.end\n",
            case.family,
            card(case.model)?,
            card(case.instance)?,
        );
        fs::write(temp.path().join("reference.cir"), &deck)?;
        let result = Command::new("ngspice")
            .args(["-b", "-n", "reference.cir"])
            .current_dir(temp.path())
            .output()?;
        let output = String::from_utf8(result.stdout)?;
        assert!(
            result.status.success(),
            "{deck}\n{output}\n{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(output.contains("No. of Data Rows : 1"), "{deck}\n{output}");
        assert!(output.contains("HiSIM2 models"), "{deck}\n{output}");
        assert!(output.contains("HiSIM2:"), "{deck}\n{output}");
        Ok(output)
    }

    fn numeric_fields(input: &str) -> impl Iterator<Item = (&str, f64)> {
        input.lines().filter_map(|line| {
            let mut fields = line.split_whitespace();
            let name = fields.next()?;
            let value = fields.next()?.parse::<f64>().ok()?;
            fields.next().is_none().then_some((name, value))
        })
    }

    fn compare(p: &BoundParameters, reference: &str) -> Result<usize, Box<dyn Error>> {
        let mut counts = BTreeMap::new();
        let mut mismatches = Vec::new();
        for (raw_name, expected) in numeric_fields(reference) {
            // ngspice showmod prints at most eleven characters in the
            // name column. The source table orders each base, L and LP.
            let name = if matches!(raw_name, "depmueback0" | "depmueback1") {
                let count = counts.entry(raw_name.to_owned()).or_insert(0);
                let suffix = ["", "l", "lp"]
                    .get(*count)
                    .ok_or("ambiguous truncated name")?;
                *count += 1;
                format!("{raw_name}{suffix}")
            } else {
                raw_name.to_owned()
            };
            let Some(spec) = FAMILY.parameter(p.scope(), &name) else {
                continue;
            };
            let actual = match p.get_slot(spec.slot) {
                Some(ParameterValue::Real(v)) => *v,
                Some(ParameterValue::Integer(v)) => f64::from(*v),
                Some(ParameterValue::Flag(v)) => f64::from(u8::from(*v)),
                other => return Err(format!("missing {name}: {other:?}").into()),
            };
            // Default showmod uses six significant digits.
            if (actual - expected).abs() > expected.abs().max(1e-99) * 1e-5 {
                mismatches.push(format!("{name}: {actual:e} != {expected:e}"));
            }
            *counts.entry(String::new()).or_insert(0) += 1;
        }
        assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
        Ok(counts.get("").copied().unwrap_or_default())
    }

    fn run_case(case: &Case<'_>) -> Result<(usize, usize), Box<dyn Error>> {
        let mut fields = case.model.to_vec();
        fields.push((case.family, ParameterValue::Flag(true)));
        let m = model(&fields)?;
        let i = instance(&m, case.instance, 300.15)?;
        let output = native_output(case)?;
        let (_, report) = output
            .split_once("HiSIM2 models")
            .ok_or("missing native model report")?;
        let (models, instances) = report
            .split_once("HiSIM2:")
            .ok_or("missing native instance report")?;
        let nmodel = compare(m.parameters(), models)?;
        let ninstance = compare(i.parameters(), instances)?;
        assert_eq!(nmodel, 544);
        assert_eq!(ninstance, 31);
        Ok((nmodel, ninstance))
    }

    #[test]
    #[ignore = "requires independent ngspice 47 executable"]
    fn native_model_and_instance_setup_matrix() -> Result<(), Box<dyn Error>> {
        use ParameterValue::{Flag, Integer, Real};
        let cases = [
            Case {
                family: "nmos",
                model: &[],
                instance: &[],
            },
            Case {
                family: "pmos",
                model: &[],
                instance: &[],
            },
            Case {
                family: "nmos",
                model: &[
                    ("version", Integer(280)),
                    ("coddlt", Integer(0)),
                    ("corecip", Integer(0)),
                    ("sc2", Real(0.1)),
                    ("psc4", Real(0.1)),
                    ("tnom", Real(30.0)),
                    ("xl", Real(0.1e-6)),
                    ("xw", Real(0.2e-6)),
                    ("lbinn", Real(1.2)),
                    ("wbinn", Real(0.8)),
                    ("lvmax", Real(1e5)),
                    ("wvmax", Real(2e5)),
                    ("pvmax", Real(3e5)),
                    ("rbdb", Real(120.0)),
                    ("corbnet", Integer(1)),
                    ("corg", Integer(1)),
                    ("rshg", Real(10.0)),
                ],
                instance: &[
                    ("l", Real(5e-6)),
                    ("w", Real(10e-6)),
                    ("nf", Real(2.0)),
                    ("m", Real(2.0)),
                    ("temp", Real(40.0)),
                    ("dtemp", Real(5.0)),
                    ("rbdb", Real(90.0)),
                    ("corbnet", Integer(0)),
                    ("off", Flag(true)),
                ],
            },
            Case {
                family: "nmos",
                model: &[
                    ("corecip", Integer(1)),
                    ("sc2", Real(0.3)),
                    ("scp2", Real(0.2)),
                    ("sc4", Real(0.1)),
                    ("coqy", Integer(1)),
                    ("noise", Integer(0)),
                    ("nsubpfac", Real(2.0)),
                    ("nsubpdlt", Real(0.2)),
                ],
                instance: &[],
            },
            Case {
                family: "nmos",
                model: &[
                    ("codep", Integer(1)),
                    ("corecip", Integer(1)),
                    ("copprv", Integer(1)),
                    ("corsrd", Integer(1)),
                    ("coisti", Integer(1)),
                    ("cothrml", Integer(1)),
                    ("coign", Integer(1)),
                    ("ndepm", Real(3e17)),
                    ("tndep", Real(2e-6)),
                ],
                instance: &[],
            },
        ];
        let mut counts = (0, 0);
        for case in cases {
            let (m, i) = run_case(&case)?;
            counts.0 += m;
            counts.1 += i;
        }
        println!(
            "HiSIM2: {} independent model values and {} instance values",
            counts.0, counts.1
        );
        Ok(())
    }
}

#[test]
fn physical_geometry_series_and_context() -> Result<(), ModelError> {
    use ParameterValue::{Integer, Real};
    let m = model(&[
        ("corsrd", Integer(-1)),
        ("rsh", Real(12.0)),
        ("rd", Real(20e-6)),
        ("rs", Real(10e-6)),
        ("xl", Real(0.1e-6)),
        ("xw", Real(0.2e-6)),
        ("xld", Real(0.1e-6)),
        ("xwd", Real(0.15e-6)),
        ("ll", Real(0.05e-6)),
        ("wl", Real(0.025e-6)),
        ("corg", Integer(1)),
        ("rshg", Real(10.0)),
        ("corbnet", Integer(1)),
        ("rbdb", Real(80.0)),
    ])?;
    let i = instance(
        &m,
        &[
            ("l", Real(3e-6)),
            ("w", Real(10e-6)),
            ("nf", Real(2.0)),
            ("nrd", Real(2.0)),
            ("nrs", Real(3.0)),
            ("m", Real(4.0)),
        ],
        300.15,
    )?;
    let p = model_compact_intrinsic::hisim2::prepare_physical(&m, &i)?;
    assert!((p.geometry.length_m - 2.8e-6).abs() < 1e-18);
    assert!((p.geometry.total_width_m - 9.7e-6).abs() < 1e-18);
    // Independent ngspice 47 DC node-drop/current measurements, level 68.
    assert!((p.drain.external_s - 0.15348101265822761).abs() < 1e-12);
    assert!((p.source.external_s - 0.10801781737193764).abs() < 1e-12);
    assert_eq!(p.drain.internal_noise_s, 0.0);
    assert!(
        (p.kernel_binned_slot(slot(ParameterScope::Model, "vmax")?)
            .unwrap()
            - 1e5)
            .abs()
            < 1e-9
    );
    assert!(p.gate_s.unwrap() > 0.0);
    assert!(p.body.unwrap().drain_bulk_s > 0.05);
    let other = model(&[])?;
    assert_eq!(
        model_compact_intrinsic::hisim2::prepare_physical(&other, &i),
        Err(ModelError::MismatchedPreparedContext)
    );
    let short = instance(&m, &[("l", Real(1e-9))], 300.15)?;
    assert!(model_compact_intrinsic::hisim2::prepare_physical(&m, &short).is_err());
    Ok(())
}
