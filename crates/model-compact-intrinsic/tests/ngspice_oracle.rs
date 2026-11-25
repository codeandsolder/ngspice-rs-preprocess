//! Independent ngspice-47 oracle checks for source-derived model preparation.

#[cfg(target_os = "linux")]
mod linux {
    use std::io::Write;
    use std::process::{Command, Stdio};

    use model_compact_intrinsic::{
        BoundParameters, ModelVariant, ParameterScope, ParameterValue, prepare_bsim_soi_model,
    };

    fn show_model(card: &str) -> Result<Vec<(String, f64)>, Box<dyn std::error::Error>> {
        let source = format!(
            "* ngspice stdin wrapper title\n* ngspice-47 independent BSIMSOI parameter oracle\n.model xsoi {card} level=10\nVd d 0 1.0\nVg g 0 1.5\nVe e 0 0\nM1 d g 0 e xsoi l=0.5u w=1u\n.control\nop\nshowmod m1\nquit\n.endc\n.end\n"
        );
        let workdir = tempfile::tempdir()?;
        let mut child = Command::new("ngspice")
            .current_dir(workdir.path())
            .args(["-b", "/dev/stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(source.as_bytes())?;
        }
        let output = child.wait_with_output()?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout)?;
        assert!(stdout.contains("B4SOI models"));
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

    fn assert_matches_ngspice(
        card: &str,
        input: &[(&str, ParameterValue)],
    ) -> Result<(), Box<dyn std::error::Error>> {
        let source = BoundParameters::bind(
            ModelVariant::BsimSoi,
            ParameterScope::Model,
            input.iter().map(|(name, value)| (*name, value.clone())),
        )?;
        let prepared = prepare_bsim_soi_model(&source, 300.15)?;
        let mut checked = 0;
        let mut mismatches = Vec::new();
        for (name, reference) in show_model(card)? {
            if name == "tnom" {
                continue;
            } // ngspice external parameter uses Celsius.
            let Some(spec) = ModelVariant::BsimSoi.parameter(ParameterScope::Model, &name) else {
                continue;
            };
            let value = match prepared.get_slot(spec.slot) {
                Some(ParameterValue::Real(value)) => *value,
                Some(ParameterValue::Integer(value)) => f64::from(*value),
                _ => continue,
            };
            checked += 1;
            let epsilon = reference.abs().max(1e-35) * 1.0e-5;
            if !value.is_finite() || (value - reference).abs() > epsilon {
                mismatches.push(format!("{name}: prepared {value:e}, ngspice {reference:e}"));
            }
        }
        assert!(
            checked >= 650,
            "only {checked} ngspice values checked for {card}"
        );
        eprintln!("Validated {checked} ngspice-47 numeric model values for {card}");
        assert!(
            mismatches.is_empty(),
            "{count} of {checked} ngspice-47 defaults differ for {card}:\n{details}",
            count = mismatches.len(),
            details = mismatches.join("\n")
        );
        Ok(())
    }

    #[test]
    #[ignore = "requires local ngspice 47 binary"]
    fn native_bsimsoi_model_defaults_nmos() -> Result<(), Box<dyn std::error::Error>> {
        assert_matches_ngspice("nmos", &[])
    }

    #[test]
    #[ignore = "requires local ngspice 47 binary"]
    fn native_bsimsoi_model_defaults_pmos() -> Result<(), Box<dyn std::error::Error>> {
        assert_matches_ngspice("pmos", &[("pmos", ParameterValue::Flag(true))])
    }

    #[test]
    #[ignore = "requires local ngspice 47 binary"]
    fn native_bsimsoi_custom_model_parameters() -> Result<(), Box<dyn std::error::Error>> {
        assert_matches_ngspice(
            "nmos mobmod=3 tox=2.5n drout=0.91 nlx=75n ngidl=4.25",
            &[
                ("mobmod", ParameterValue::Integer(3)),
                ("tox", ParameterValue::Real(2.5e-9)),
                ("drout", ParameterValue::Real(0.91)),
                ("nlx", ParameterValue::Real(75e-9)),
                ("ngidl", ParameterValue::Real(4.25)),
            ],
        )
    }
}
