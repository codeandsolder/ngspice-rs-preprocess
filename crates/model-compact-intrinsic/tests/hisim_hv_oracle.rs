//! Full C setup-model conformance against the installed ngspice-47 executable.

#[cfg(target_os = "linux")]
mod linux {
    use std::io::Write;
    use std::process::{Command, Stdio};

    use model_compact_intrinsic::{
        BoundParameters, ModelVariant, ParameterScope, ParameterValue, prepare_hisim_hv_instance,
        prepare_hisim_hv_model,
    };

    fn run_native(
        card: &str,
        instance: &str,
        show: &str,
    ) -> Result<(String, String), Box<dyn std::error::Error>> {
        let circuit = format!(
            "* ngspice stdin wrapper title\n* HiSIM-HV native setup oracle\n.model xhv {card} level=73\nVd d 0 1\nVg g 0 1.5\nM1 d g 0 0 xhv l=10u w=50u {instance}\n.control\nop\n{show} m1\nquit\n.endc\n.end\n"
        );
        let directory = tempfile::tempdir()?;
        let mut child = Command::new("ngspice")
            .args(["-b", "/dev/stdin"])
            .current_dir(directory.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(circuit.as_bytes())?;
        }
        let output = child.wait_with_output()?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok((
            String::from_utf8(output.stdout)?,
            String::from_utf8(output.stderr)?,
        ))
    }

    fn reference(
        card: &str,
        instance: &str,
        show: &str,
    ) -> Result<Vec<(String, f64)>, Box<dyn std::error::Error>> {
        let (stdout, stderr) = run_native(card, instance, show)?;
        assert!(stderr.is_empty(), "{stderr}");
        assert!(
            stdout.contains("HiSIMHV") && stdout.contains("No. of Data Rows : 1"),
            "{stdout}"
        );
        Ok(stdout
            .lines()
            .filter_map(|line| {
                let mut fields = line.split_whitespace();
                let name = fields.next()?;
                let value = fields.next()?.parse::<f64>().ok()?;
                if fields.next().is_some() {
                    return None;
                }
                Some((name.to_ascii_lowercase(), value))
            })
            .collect())
    }

    fn compare(
        variant: ModelVariant,
        card: &str,
        parameters: &[(&str, ParameterValue)],
    ) -> Result<(), Box<dyn std::error::Error>> {
        let input = BoundParameters::bind(
            variant,
            ParameterScope::Model,
            parameters
                .iter()
                .map(|(name, value)| (*name, value.clone())),
        )?;
        let model = prepare_hisim_hv_model(&input)?;
        let mut compared = 0;
        let mut missing = Vec::new();
        let mut mismatches = Vec::new();
        for (name, expected) in reference(card, "", "showmod")? {
            let Some(spec) = variant.parameter(ParameterScope::Model, &name) else {
                continue;
            };
            let result = match model.get_slot(spec.slot) {
                Some(ParameterValue::Real(value)) => *value,
                Some(ParameterValue::Integer(value)) => f64::from(*value),
                Some(ParameterValue::Text(_) | ParameterValue::Flag(_)) => continue,
                _ => {
                    missing.push(name);
                    continue;
                }
            };
            compared += 1;
            let tolerance = expected.abs().max(1e-30) * 1.0e-5;
            if !result.is_finite() || (result - expected).abs() > tolerance {
                mismatches.push(format!("{name}: prepared {result:e}, ngspice {expected:e}"));
            }
        }
        eprintln!(
            "{variant:?} ({card}): matched {compared}, unprepared {missing_count} {missing:?}",
            missing_count = missing.len()
        );
        assert!(
            compared
                >= if variant == ModelVariant::HiSimHv1 {
                    550
                } else {
                    660
                },
            "prepared only {compared} values; missing: {missing:?}"
        );
        assert!(
            mismatches.is_empty(),
            "{} mismatches:\n{}",
            mismatches.len(),
            mismatches.join("\n")
        );
        Ok(())
    }

    fn compare_instance(
        variant: ModelVariant,
        card: &str,
        model_inputs: &[(&str, ParameterValue)],
        instance_card: &str,
        instance_inputs: &[(&str, ParameterValue)],
    ) -> Result<(), Box<dyn std::error::Error>> {
        let model_input = BoundParameters::bind(
            variant,
            ParameterScope::Model,
            model_inputs
                .iter()
                .map(|(name, value)| (*name, value.clone())),
        )?;
        let model = prepare_hisim_hv_model(&model_input)?;
        let inputs = BoundParameters::bind(
            variant,
            ParameterScope::Instance,
            [
                ("l", ParameterValue::Real(10e-6)),
                ("w", ParameterValue::Real(50e-6)),
            ]
            .into_iter()
            .chain(
                instance_inputs
                    .iter()
                    .map(|(name, value)| (*name, value.clone())),
            ),
        )?;
        let instance = prepare_hisim_hv_instance(&model, &inputs)?;
        let mut compared = 0;
        let mut skipped = Vec::new();
        let mut mismatches = Vec::new();
        for (name, expected) in reference(card, instance_card, "show")? {
            let Some(spec) = variant.parameter(ParameterScope::Instance, &name) else {
                continue;
            };
            let actual = match instance.parameters().get_slot(spec.slot) {
                Some(ParameterValue::Real(value)) => *value,
                Some(ParameterValue::Integer(value)) => f64::from(*value),
                Some(ParameterValue::Flag(value)) => f64::from(u8::from(*value)),
                Some(ParameterValue::Text(_) | ParameterValue::RealVector(_)) => continue,
                None => {
                    skipped.push(name);
                    continue;
                }
            };
            compared += 1;
            let tolerance = expected.abs().max(1e-30) * 1.0e-5;
            if !actual.is_finite() || (actual - expected).abs() > tolerance {
                mismatches.push(format!("{name}: prepared {actual:e}, native {expected:e}"));
            }
        }
        eprintln!(
            "native HV instance {variant:?} {instance_card}: compared {compared}; missing {skipped:?}"
        );
        assert_eq!(
            compared, 37,
            "every scalar C show-field must be prepared; missing {skipped:?}"
        );
        assert!(
            skipped.is_empty(),
            "unprepared C instance fields: {skipped:?}"
        );
        assert!(
            mismatches.is_empty(),
            "{} model differences: {}",
            mismatches.len(),
            mismatches.join("; ")
        );
        Ok(())
    }

    #[test]
    #[ignore = "requires ngspice 47"]
    fn hv1_default_instance() -> Result<(), Box<dyn std::error::Error>> {
        compare_instance(ModelVariant::HiSimHv1, "nmos version=1.24", &[], "", &[])
    }

    #[test]
    #[ignore = "requires ngspice 47"]
    fn hv2_default_instance() -> Result<(), Box<dyn std::error::Error>> {
        compare_instance(ModelVariant::HiSimHv2, "nmos version=2.20", &[], "", &[])
    }

    #[test]
    #[ignore = "requires ngspice 47"]
    fn hv1_symmetric_instance() -> Result<(), Box<dyn std::error::Error>> {
        compare_instance(
            ModelVariant::HiSimHv1,
            "nmos version=1.24 cosym=1",
            &[("cosym", ParameterValue::Integer(1))],
            "lover=20n ldrift1=3u ldrift2=4u nf=2 m=3",
            &[
                ("lover", ParameterValue::Real(20e-9)),
                ("ldrift1", ParameterValue::Real(3e-6)),
                ("ldrift2", ParameterValue::Real(4e-6)),
                ("nf", ParameterValue::Real(2.0)),
                ("m", ParameterValue::Real(3.0)),
            ],
        )
    }

    #[test]
    #[ignore = "requires ngspice 47"]
    fn hv2_symmetric_instance() -> Result<(), Box<dyn std::error::Error>> {
        compare_instance(
            ModelVariant::HiSimHv2,
            "nmos version=2.20 cosym=1",
            &[("cosym", ParameterValue::Integer(1))],
            "lover=20n ldrift1=3u ldrift2=4u nf=2 m=3",
            &[
                ("lover", ParameterValue::Real(20e-9)),
                ("ldrift1", ParameterValue::Real(3e-6)),
                ("ldrift2", ParameterValue::Real(4e-6)),
                ("nf", ParameterValue::Real(2.0)),
                ("m", ParameterValue::Real(3.0)),
            ],
        )
    }

    #[test]
    #[ignore = "requires ngspice 47"]
    fn hv1_model_given_blocks_symmetric_instance_drift_override()
    -> Result<(), Box<dyn std::error::Error>> {
        compare_instance(
            ModelVariant::HiSimHv1,
            "pmos version=1.24 cosym=1 ldrift1s=0.8u lovers=30n",
            &[
                ("pmos", ParameterValue::Flag(true)),
                ("cosym", ParameterValue::Integer(1)),
                ("ldrift1s", ParameterValue::Real(0.8e-6)),
                ("lovers", ParameterValue::Real(30e-9)),
            ],
            "ldrift1=3u lover=10n",
            &[
                ("ldrift1", ParameterValue::Real(3e-6)),
                ("lover", ParameterValue::Real(10e-9)),
            ],
        )
    }

    #[test]
    #[ignore = "requires ngspice 47"]
    fn hv2_model_given_blocks_symmetric_instance_drift_override()
    -> Result<(), Box<dyn std::error::Error>> {
        compare_instance(
            ModelVariant::HiSimHv2,
            "nmos version=2.20 cosym=1 ldrift1s=0.8u lovers=30n",
            &[
                ("cosym", ParameterValue::Integer(1)),
                ("ldrift1s", ParameterValue::Real(0.8e-6)),
                ("lovers", ParameterValue::Real(30e-9)),
            ],
            "ldrift1=3u lover=10n",
            &[
                ("ldrift1", ParameterValue::Real(3e-6)),
                ("lover", ParameterValue::Real(10e-9)),
            ],
        )
    }

    fn native_geometry_conformance(
        variant: ModelVariant,
        version: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        for (parameter, c_name, kind) in [
            (("xldld", 20e-6), "xldld=20u", "length"),
            (("xwd", 30e-6), "xwd=30u", "width"),
        ] {
            let source = BoundParameters::bind(
                variant,
                ParameterScope::Model,
                [(parameter.0, ParameterValue::Real(parameter.1))],
            )?;
            let model = prepare_hisim_hv_model(&source)?;
            let inputs = BoundParameters::bind(
                variant,
                ParameterScope::Instance,
                [
                    ("l", ParameterValue::Real(10e-6)),
                    ("w", ParameterValue::Real(50e-6)),
                ],
            )?;
            let error = prepare_hisim_hv_instance(&model, &inputs);
            assert!(
                error.as_ref().is_err_and(|failure| failure
                    .to_string()
                    .contains(&format!("effective channel {kind}"))),
                "expected geometry rejection for {c_name}, got {error:?}"
            );
            let (_, stderr) = run_native(&format!("nmos version={version} {c_name}"), "", "show")?;
            assert!(
                stderr.contains(&format!("effective channel {kind} is negative or 0")),
                "native {variant:?}: {stderr}"
            );
        }
        let (stdout, stderr) = run_native(
            &format!("nmos version={version} xldld=0.5u xwd=1u"),
            "",
            "show",
        )?;
        assert!(stderr.is_empty(), "{stderr}");
        assert!(stdout.contains("No. of Data Rows : 1"), "{stdout}");
        Ok(())
    }

    #[test]
    #[ignore = "requires ngspice 47"]
    fn hv1_native_effective_geometry_acceptance_and_fatal_rejections()
    -> Result<(), Box<dyn std::error::Error>> {
        native_geometry_conformance(ModelVariant::HiSimHv1, "1.24")
    }

    #[test]
    #[ignore = "requires ngspice 47"]
    fn hv2_native_effective_geometry_acceptance_and_fatal_rejections()
    -> Result<(), Box<dyn std::error::Error>> {
        native_geometry_conformance(ModelVariant::HiSimHv2, "2.20")
    }

    #[test]
    #[ignore = "requires ngspice 47"]
    fn hv1_nmos_defaults() -> Result<(), Box<dyn std::error::Error>> {
        compare(ModelVariant::HiSimHv1, "nmos version=1.24", &[])
    }
    #[test]
    #[ignore = "requires ngspice 47"]
    fn hv1_pmos_defaults() -> Result<(), Box<dyn std::error::Error>> {
        compare(
            ModelVariant::HiSimHv1,
            "pmos version=1.24",
            &[("pmos", ParameterValue::Flag(true))],
        )
    }
    #[test]
    #[ignore = "requires ngspice 47"]
    fn hv2_nmos_defaults() -> Result<(), Box<dyn std::error::Error>> {
        compare(ModelVariant::HiSimHv2, "nmos version=2.20", &[])
    }
    #[test]
    #[ignore = "requires ngspice 47"]
    fn hv2_pmos_defaults() -> Result<(), Box<dyn std::error::Error>> {
        compare(
            ModelVariant::HiSimHv2,
            "pmos version=2.20",
            &[("pmos", ParameterValue::Flag(true))],
        )
    }
    #[test]
    #[ignore = "requires ngspice 47"]
    fn hv2_symmetry_and_depletion() -> Result<(), Box<dyn std::error::Error>> {
        compare(
            ModelVariant::HiSimHv2,
            "nmos version=2.20 cosym=1 codep=1 rd=0.12 cgdo=0.4n",
            &[
                ("cosym", ParameterValue::Integer(1)),
                ("codep", ParameterValue::Integer(1)),
                ("rd", ParameterValue::Real(0.12)),
                ("cgdo", ParameterValue::Real(0.4e-9)),
            ],
        )
    }
    #[test]
    #[ignore = "requires ngspice 47"]
    fn hv1_symmetry_and_legacy() -> Result<(), Box<dyn std::error::Error>> {
        compare(
            ModelVariant::HiSimHv1,
            "nmos version=1.24 cosym=1 rd=0.12 cgdo=0.4n lover=20n",
            &[
                ("cosym", ParameterValue::Integer(1)),
                ("rd", ParameterValue::Real(0.12)),
                ("cgdo", ParameterValue::Real(0.4e-9)),
                ("lover", ParameterValue::Real(20e-9)),
            ],
        )
    }
}
