use super::{Control, ControlledTransfer, OutputKind, ResolvedTransfer, SpiceExpression, parse};
use crate::{linked_source::RootKind, normalize_mapped};
use anyhow::{Context, Result, bail, ensure};
use model_preprocess_command::WorkBudget;
use std::{fs, process::Command};

fn card(text: &str) -> Result<ControlledTransfer> {
    parse(text)?.context("expected rational transfer card")
}

fn resolve(source: &ControlledTransfer, pole: f64, gain: f64) -> Result<(Vec<f64>, Vec<f64>)> {
    let ResolvedTransfer::Rational {
        numerator,
        denominator,
    } = source.resolve(|text| {
        SpiceExpression::parse(text)?.evaluate(|name| match name {
            "p" => Ok(pole),
            "g" => Ok(gain),
            _ => bail!("undefined coefficient symbol {name}"),
        })
    })?
    else {
        bail!("expected rational coefficients");
    };
    Ok((numerator, denominator))
}

#[test]
fn structural_polynomial_arithmetic_preserves_scoped_coefficients() -> Result<()> {
    let source = card("Efilter out ref LAPLACE {V(in,ref)*2} = {g*(s+p)/((s+p)*(s+2*p))}")?;
    assert_eq!(source.output, OutputKind::Voltage);
    assert_eq!(source.control, Control::Expression("V(in,ref)*2".into()));
    for (pole, gain) in [(2., 3.), (5., -4.), (2., 3.)] {
        let (num, den) = resolve(&source, pole, gain)?;
        assert_eq!(num, [gain, gain * pole]);
        assert_eq!(den, [1., 3. * pole, 2. * pole * pole]);
    }
    let source = card("Gfilter out ref in n LaPlAcE = 1 / (s+1) + 2 / (s+2)")?;
    assert_eq!(source.output, OutputKind::Current);
    assert_eq!(
        source.control,
        Control::Differential {
            positive: "in".into(),
            negative: "n".into()
        }
    );
    assert_eq!(resolve(&source, 1., 1.)?, (vec![3., 4.], vec![1., 3., 2.]));
    let source = card("Efilter out 0 LAPLACE {v(in)} {(1+s)^(-2)}")?;
    assert_eq!(resolve(&source, 1., 1.)?, (vec![1.], vec![1., 2., 1.]));
    let source = card("Efilter out 0 in 0 Laplace={(s^2-s^2+2)/(s+1)}")?;
    assert_eq!(resolve(&source, 1., 1.)?, (vec![2.], vec![1., 1.]));
    let source = card("Efilter out 0 in 0 Laplace={(s^2+3*s+2)/(s^2+5*s+6)}")?;
    assert_eq!(
        resolve(&source, 1., 1.)?,
        (vec![1., 3., 2.], vec![1., 5., 6.])
    );
    let source = card("Efilter out 0 in 0 Laplace={g/(1+s/p)}")?;
    assert_eq!(resolve(&source, 2., 3.)?, (vec![3.], vec![0.5, 1.]));
    assert!(resolve(&source, 0., 3.).is_err());
    let source = card("Efilter out 0 laplace ref LAPLACE={1/(s+1)}")?;
    assert_eq!(
        source.control,
        Control::Differential {
            positive: "laplace".into(),
            negative: "ref".into()
        }
    );
    Ok(())
}

#[test]
fn malformed_or_nonrational_forms_fail_without_changing_ordinary_sources() -> Result<()> {
    for text in [
        "E1 out 0 LAPLACE {v(in)} = {exp(-s)}",
        "E1 out 0 in 0 Laplace={sqrt(s)}",
        "E1 out 0 in 0 Laplace={s^0.5}",
        "E1 out 0 in 0 Laplace={1/(s-s)}",
        "E1 out 0 in 0 Laplace={s^3/(s+1)}",
        "E1 out 0 in 0 Laplace={1/(s+1)} window=1m",
        "E1 out 0 LAPLACE {v(in)} =",
        "E1 out 0 in 0 Laplace={1/(s^257+1)}",
        "E1 out 0 in 0 Laplace={1/(s+1)+1/0}",
        "E1 out 0 in 0 Laplace={0*(1/0)*s}",
        "E1 out 0 in 0 Laplace={1/(1e-200*s*1e-200+1)}",
        "E1 out 0 in 0 Laplace={1/(s+1}",
    ] {
        assert!(parse(text).is_err(), "{text}");
    }
    for text in [
        "E1 out 0 laplace ref 2",
        "G1 out 0 laplace ref {gain}",
        "E1 out laplace in ref 2",
        "E1 out 0 in ref laplace",
    ] {
        assert!(parse(text)?.is_none(), "{text}");
    }
    let source = card("E1 out 0 in 0 Laplace={(s+1)/(p*s+g)}")?;
    assert!(resolve(&source, 0., 1.).is_err());
    assert!(resolve(&source, 0., 0.).is_err());
    let source = card("E1 out 0 in 0 Laplace={missing/(s+1)}")?;
    assert!(resolve(&source, 1., 1.).is_err());
    Ok(())
}

#[test]
fn lowering_keeps_scope_branch_names_source_lines_and_collision_free_ids() -> Result<()> {
    let input = b"rational transfer\r\n.subckt filter in ref out p=2 g=3\r\nE1 out ref LAPLACE {V(in,ref)} =\r\n+ {g/(s+p)} ; source\r\nR1 __laplace_2_out 0 1k\r\n.ends filter\r\n.control\r\necho laplace\r\n.endc\r\n.end\r\n";
    let result = normalize_mapped(
        input,
        RootKind::Deck,
        crate::linked_source::SourceDialect::Native,
        &WorkBudget::default(),
    )?;
    let text = std::str::from_utf8(&result.model.bytes)?;
    assert!(
        text.contains("E1 out ref __laplace_2_out_1 0 1\r\n"),
        "{text}"
    );
    assert!(
        text.contains("num_coeff=[{g}] den_coeff=[1e0 {p}] denormalized_freq=1"),
        "{text}"
    );
    assert!(text.contains("; source"));
    assert!(text.contains("echo laplace"));
    assert_eq!(result.input_lines, [1, 2, 3, 3, 3, 3, 4, 5, 6, 7, 8, 9, 10]);
    let repeated = normalize_mapped(
        &result.model.bytes,
        RootKind::Deck,
        crate::linked_source::SourceDialect::Native,
        &WorkBudget::default(),
    )?;
    assert_eq!(repeated.model.bytes, result.model.bytes);
    let bad = normalize_mapped(
        b"title\nL1 a 0 1m Rser=1\nE1 b 0 a 0 Laplace={exp(-s)}\n.end\n",
        RootKind::Deck,
        crate::linked_source::SourceDialect::Native,
        &WorkBudget::default(),
    );
    assert!(format!("{:#}", bad.err().context("expected positioned failure")?).contains("line 3"));
    Ok(())
}

fn rows(data: &str) -> impl Iterator<Item = Result<Vec<f64>>> + '_ {
    data.lines().skip(1).map(|line| {
        line.split_whitespace()
            .map(|word| Ok(word.parse::<f64>()?))
            .collect()
    })
}

fn near(actual: f64, expected: f64, tolerance: f64) {
    assert!(
        (actual - expected).abs() <= tolerance,
        "{actual} != {expected} (tolerance {tolerance})"
    );
}

#[test]
fn native_rational_reference_ac_transient_scope_and_current_polarity() -> Result<()> {
    let directory = tempfile::tempdir()?;
    // Independent first-order oracle, nonzero common mode and distinct
    // lexical overrides of one physical subcircuit.
    let source = b"Rational scoped reference\n.subckt filter p n out gout params: tau=1m gainv=2\nE1 out 0 LAPLACE {v(p,n)}={gainv/(1+tau*s)}\nG1 gout 0 p n Laplace={gainv/(1+tau*s)}\n.ends filter\nV1 in ref dc 0 ac 1 PWL(0 0 1u 1 5m 1)\nVref ref 0 2\nX1 in ref out1 gout1 filter tau=1m gainv=2\nX2 in ref out2 gout2 filter tau=2m gainv=3\nR1 out1 0 1k\nR2 out2 0 1k\nRg1 gout1 0 1\nRg2 gout2 0 1\n.options reltol=1e-7 abstol=1e-12 vntol=1e-9\n.control\nset wr_singlescale\nset wr_vecnames\nac lin 5 100 500\nwrdata ac.dat v(out1) v(gout1) v(out2) v(gout2)\ntran 0.1u 5m 0 0.1u\nwrdata tran.dat v(out1) v(gout1) v(out2) v(gout2)\nquit\n.endc\n.end\n";
    let normalized = normalize_mapped(
        source,
        RootKind::Deck,
        crate::linked_source::SourceDialect::Native,
        &WorkBudget::default(),
    )?;
    fs::write(
        directory.path().join("reference.cir"),
        normalized.model.bytes,
    )?;
    let output = match Command::new("ngspice")
        .args(["-b", "reference.cir"])
        .current_dir(directory.path())
        .output()
    {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    ensure!(
        output.status.success(),
        "ngspice: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let data = fs::read_to_string(directory.path().join("ac.dat"))?;
    let mut count = 0;
    for row in rows(&data) {
        let row = row?;
        let [frequency, r1, i1, gr1, gi1, r2, i2, gr2, gi2] = row.as_slice() else {
            bail!("invalid AC reference row");
        };
        for (tau, gain, re, im, gre, gim) in [
            (0.001, 2., *r1, *i1, *gr1, *gi1),
            (0.002, 3., *r2, *i2, *gr2, *gi2),
        ] {
            let omega = std::f64::consts::TAU * frequency * tau;
            let scale = gain / omega.mul_add(omega, 1.);
            near(re, scale, 1e-8);
            near(im, -scale * omega, 1e-8);
            near(gre, -re, 1e-8);
            near(gim, -im, 1e-8);
        }
        count += 1;
    }
    assert_eq!(count, 5);
    let data = fs::read_to_string(directory.path().join("tran.dat"))?;
    let mut last = 0.;
    for row in rows(&data) {
        let row = row?;
        let [time, out1, gout1, out2, gout2] = row.as_slice() else {
            bail!("invalid transient reference row");
        };
        if *time >= 1e-6 {
            for (tau, gain, voltage, current) in [
                (0.001_f64, 2., *out1, *gout1),
                (0.002_f64, 3., *out2, *gout2),
            ] {
                let ramp = 1e-6;
                let fraction = -(-ramp / tau).exp_m1() * tau / ramp;
                let expected = gain * (fraction.mul_add(-(-(time - ramp) / tau).exp(), 1.));
                near(voltage, expected, 8e-5);
                near(current, -voltage, 1e-8);
            }
        }
        last = *time;
    }
    near(last, 0.005, 1e-10);
    Ok(())
}

#[test]
fn rational_coefficients_consume_cumulative_intermediate_work() -> Result<()> {
    use model_preprocess_command::{Cancellation, Resource, WorkLimits};

    let source = "E1 out 0 in 0 Laplace={1/(s+1)^32}";
    let normal = super::parse_with_budget(source, &WorkBudget::default())?
        .context("missing rational transfer")?;
    let (numerator, denominator) = resolve(&normal, 1., 1.)?;
    assert_eq!(numerator, [1.]);
    assert_eq!(denominator.len(), 33);
    assert_eq!(denominator[1], 32.);
    for (limits, dimension) in [
        (
            WorkLimits {
                evaluation_steps: 200,
                ..WorkLimits::default()
            },
            "EvaluationSteps",
        ),
        (
            WorkLimits {
                generated_nodes: 100,
                ..WorkLimits::default()
            },
            "GeneratedNodes",
        ),
        (
            WorkLimits {
                emitted_bytes: 4096,
                ..WorkLimits::default()
            },
            "EmittedBytes",
        ),
    ] {
        let budget = WorkBudget::new(limits, Cancellation::default());
        let error = super::parse_with_budget(source, &budget)
            .err()
            .context("rational work escaped the caller's limit")?;
        assert!(
            format!("{error:#}").contains(&format!("{dimension} budget exceeded")),
            "{error:#}"
        );
        assert!(budget.check().is_err());
    }

    // Exact allowance for a single symbolic transfer, then an identical
    // second call against the same ledger. Neither call changes coefficients.
    let symbolic = "E1 out 0 in 0 Laplace={g/((s+p)*(s+2*p))}";
    let limits = WorkLimits::default();
    let measured = WorkBudget::new(limits, Cancellation::default());
    let expected = super::parse_with_budget(symbolic, &measured)?;
    let bytes = limits.emitted_bytes - measured.remaining(Resource::EmittedBytes)?;
    let budget = WorkBudget::new(
        WorkLimits {
            emitted_bytes: bytes,
            ..limits
        },
        Cancellation::default(),
    );
    assert_eq!(super::parse_with_budget(symbolic, &budget)?, expected);
    assert_eq!(budget.remaining(Resource::EmittedBytes)?, 0);
    let error = super::parse_with_budget(symbolic, &budget)
        .err()
        .context("transfer parser reset cumulative allowance")?;
    assert!(
        format!("{error:#}").contains("EmittedBytes budget exceeded"),
        "{error:#}"
    );
    Ok(())
}

#[test]
fn hspice_positional_coefficients_use_shared_rational_binding() -> Result<()> {
    let budget = WorkBudget::default();
    let source = super::hspice_laplace("G1 p n LAPLACE a b g, 2,0 / 1, p, 3 SCALE=2 M=4", &budget)?
        .context("missing HSPICE transfer")?;
    assert_eq!(source.output, OutputKind::Current);
    assert_eq!(
        source.control,
        Control::Differential {
            positive: "a".into(),
            negative: "b".into()
        }
    );
    assert_eq!(
        resolve(&source, 5., 7.)?,
        (vec![16., 56.], vec![3., 5., 1.])
    );
    for malformed in [
        "E1 p n LAPLACE a b 1 / 0",
        "E1 p n LAPLACE a b 1 2 / 1",
        "E1 p n LAPLACE a b 1 / 1 / 2",
        "E1 p n LAPLACE a b / 1",
        "E1 p n LAPLACE a b 1 / 1 TC1=0.1",
        "E1 p n LAPLACE a b 1 / 1 M=2",
        "G1 p n LAPLACE a b 1 / 1 SCALE=1 SCALE=2",
    ] {
        assert!(
            super::hspice_laplace(malformed, &budget).is_err(),
            "{malformed}"
        );
    }
    Ok(())
}
