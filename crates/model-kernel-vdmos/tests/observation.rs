//! Observation laws must never modify the electrical or thermal residual.
use model_kernel_vdmos::{
    Bias, Instance, Model, ParameterError, Polarity, PreparedModel, compile_ngspice,
    observation::{ObservationTemperature, SoaQuantity, SoaSample},
};
use model_normalize_ngspice::{model_card::parse_model_cards, vdmos_card::vdmos_literals};
use std::{
    error::Error,
    io::Write,
    path::Path,
    process::{Command, Stdio},
    sync::Arc,
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn sample(polarity: Polarity, temperature: ObservationTemperature) -> SoaSample {
    let sign = polarity.sign();
    SoaSample {
        drain: 9.0 * sign,
        gate: 5.0 * sign,
        source: 0.0,
        channel: 2.0 * sign,
        diode: -0.5,
        i_cgs: 0.1,
        i_cgd: -0.25,
        temperature,
    }
}

#[test]
fn source_metadata_has_exact_precedence_and_no_physical_effect() -> Result<()> {
    let source = b".model test vdmos (pchan kp=2 phi=.7 PHI=.9 vgs_max=2 vgsr_max=3 vgd_max=2 vgdr_max=3 vds_max=8 id_max=-1 idr_max=-2 pd_max=20 te_max=100 rth_ext=90 derating=.2 tnom=27)";
    let cards = parse_model_cards(source)?;
    let parsed = vdmos_literals(cards.first().ok_or("no model")?)?.ok_or("not VDMOS")?;
    let model = compile_ngspice(Polarity::P, parsed.parameters)?;
    let baseline = compile_ngspice(Polarity::P, [("KP", 2.), ("TNOM", 27.)])?;
    assert_eq!(model.native_type(), "vdmosp");
    assert_eq!(model.parameters().phi, 0.9);
    assert_eq!(model.parameters().soa.te_max, Some(373.15));
    assert_eq!(model.external_thermal_resistance(), 90.0);
    assert_eq!(
        baseline.external_thermal_resistance(),
        baseline.parameters().thermal.rthca
    );
    assert!((model.distortion_inputs(300.15)?.surface_potential - 0.9).abs() < 1e-14);
    let bias = Bias {
        vgs: -5.,
        vds: -1.,
        vds_external: -1.,
        vdiode: -1.,
        temperature: 350.,
        ..Bias::default()
    };
    let device = model.bind(Instance::default())?;
    let base = baseline.bind(Instance::default())?;
    let a = device.evaluate(bias)?;
    let b = base.evaluate(bias)?;
    // Debug covers every current, derivative, charge, heat and series field.
    assert_eq!(format!("{a:?}"), format!("{b:?}"));
    assert_eq!(
        format!("{:?}", device.noise_density(&a, 100.)?),
        format!("{:?}", base.noise_density(&b, 100.)?)
    );
    assert!(matches!(
        compile_ngspice(Polarity::N, [("TYPE", 1.)]),
        Err(ParameterError::ReadOnly(_))
    ));
    let mut typed = Model::defaults(Polarity::N);
    typed.soa.pd_max = Some(f64::NAN);
    assert!(typed.prepare().is_err());
    Ok(())
}

#[test]
fn asymmetric_gate_limits_and_derating_use_native_conventions() -> Result<()> {
    for polarity in [Polarity::N, Polarity::P] {
        let model = compile_ngspice(
            polarity,
            [
                ("vgs_max", 2.),
                ("vgsr_max", 3.),
                ("vgd_max", 2.),
                ("vgdr_max", 3.),
                ("vds_max", 8.),
                ("id_max", -1.),
                ("idr_max", -2.),
                ("pd_max", 20.),
                ("te_max", 100.),
                ("derating", 0.2),
                ("tnom", 27.),
            ],
        )?;
        let result = model.observe_soa(sample(polarity, ObservationTemperature::Case(400.15)))?;
        assert!((result.power - 42.).abs() < 1e-12);
        assert!(result.power_limit.abs() < 1e-12);
        let actual = result.violations().map(|v| v.quantity).collect::<Vec<_>>();
        assert_eq!(
            actual,
            [
                SoaQuantity::Vgs,
                SoaQuantity::VgdReverse,
                SoaQuantity::Vds,
                SoaQuantity::DrainCurrent,
                SoaQuantity::ReverseCurrent,
                SoaQuantity::Power,
                SoaQuantity::Temperature
            ]
        );
        let fixed = model.observe_soa(sample(polarity, ObservationTemperature::Fixed(400.15)))?;
        assert!(
            !fixed
                .violations()
                .any(|v| v.quantity == SoaQuantity::Temperature)
        );
        let mut shifted = sample(polarity, ObservationTemperature::Case(400.15));
        shifted.drain += 20.;
        shifted.gate += 20.;
        shifted.source += 20.;
        let shifted = model.observe_soa(shifted)?;
        assert_eq!(shifted.power, result.power);
        assert_eq!(
            shifted.violations().collect::<Vec<_>>(),
            result.violations().collect::<Vec<_>>()
        );
        let cold = model.observe_soa(sample(polarity, ObservationTemperature::Case(290.15)))?;
        assert_eq!(cold.power_limit, 20.);
    }
    Ok(())
}

#[test]
fn omission_and_native_power_default_are_not_conflated_with_zero() -> Result<()> {
    let model = compile_ngspice(
        Polarity::N,
        [
            ("pd_max", 1.),
            ("vgs_max", 2.),
            ("vgd_max", 2.),
            ("derating", 2.),
            ("tnom", 27.),
        ],
    )?;
    assert_eq!(model.parameters().soa.pd_max, Some(1.));
    let result = model.observe_soa(sample(Polarity::N, ObservationTemperature::Fixed(350.15)))?;
    assert!(result.power_limit > 1e90);
    assert_eq!(
        result.violations().map(|v| v.quantity).collect::<Vec<_>>(),
        [SoaQuantity::Vgs, SoaQuantity::Vgd]
    );
    let model = compile_ngspice(
        Polarity::N,
        [
            ("pd_max", 1.),
            ("id_max", 100.),
            ("derating", 2.),
            ("te_max", 30.),
        ],
    )?;
    let result = model.observe_soa(sample(Polarity::N, ObservationTemperature::Case(400.15)))?;
    assert_eq!(result.power_limit, 1.);
    assert!(
        !result
            .violations()
            .any(|v| v.quantity == SoaQuantity::Temperature)
    );
    let mut invalid = sample(Polarity::N, ObservationTemperature::Fixed(300.15));
    invalid.channel = f64::MAX;
    assert!(model.observe_soa(invalid).is_err());
    Ok(())
}

fn oracle(fixture: &str) -> Result<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut child = Command::new("ngspice")
        .args(["-b", "-n"])
        .current_dir(path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().ok_or("missing oracle input")?;
    write!(
        stdin,
        "* diagnostic harness\n.control\nset warn=1\nset maxwarns=100\nsource {fixture}\nquit\n.endc\n.end\n"
    )?;
    drop(stdin);
    let result = child.wait_with_output()?;
    if !result.status.success() {
        return Err(String::from_utf8_lossy(&result.stderr).into_owned().into());
    }
    let output = format!(
        "{}\n{}",
        String::from_utf8(result.stdout)?,
        String::from_utf8(result.stderr)?
    );
    if !output.contains("ngspice-47 done") {
        return Err(format!("not ngspice 47: {output}").into());
    }
    Ok(output)
}

fn value(output: &str, field: &str) -> Result<f64> {
    output
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once('=')?;
            (key.trim() == field).then_some(value.trim())
        })
        .ok_or_else(|| format!("missing {field}: {output}").into())
        .and_then(|text| Ok(text.parse()?))
}

const fn warning_name(quantity: SoaQuantity) -> &'static str {
    match quantity {
        SoaQuantity::Vgs => "Vgs_max",
        SoaQuantity::VgsReverse => "Vgsr_max",
        SoaQuantity::Vgd => "Vgd_max",
        SoaQuantity::VgdReverse => "Vgdr_max",
        SoaQuantity::Vds => "Vds_max",
        SoaQuantity::DrainCurrent => "Id_max",
        SoaQuantity::ReverseCurrent => "Idr_max",
        SoaQuantity::Power => "Pd_max",
        SoaQuantity::Temperature => "te_max",
    }
}

// Use the kernel's constitutive value, not the diagnostic data, to
// independently check that explicit diagnostic fields leave DC intact.
fn check_independent_channel(
    model: &PreparedModel,
    fixture: &str,
    polarity: Polarity,
    vgs: f64,
    vds: f64,
    temp: f64,
    output: &str,
) -> Result<()> {
    let point = model.bind(Instance::default())?.evaluate(Bias {
        vgs,
        vds,
        vds_external: vds,
        vdiode: -vds * polarity.sign(),
        temperature: temp,
        ..Bias::default()
    })?;
    let expected = value(output, "@m1[id]")?;
    assert!(
        point
            .channel
            .value
            .mul_add(polarity.sign(), -expected)
            .abs()
            < 2e-5 * expected.abs().max(1.),
        "{fixture}"
    );
    Ok(())
}

#[test]
#[ignore = "requires installed ngspice 47 C reference"]
fn independent_native_diagnostic_warning_oracle() -> Result<()> {
    let fixtures = [
        (
            "soa_n.cir",
            Polarity::N,
            5.,
            1.,
            ObservationTemperature::Fixed(350.15),
        ),
        (
            "soa_p.cir",
            Polarity::P,
            -5.,
            -1.,
            ObservationTemperature::Fixed(350.15),
        ),
        (
            "soa_power_default.cir",
            Polarity::N,
            5.,
            1.,
            ObservationTemperature::Fixed(350.15),
        ),
        (
            "soa_thermal.cir",
            Polarity::N,
            5.,
            1.,
            ObservationTemperature::Case(400.15),
        ),
    ];
    for (fixture, polarity, vgs, vds, temperature) in fixtures {
        let output = oracle(fixture)?;
        let file = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(fixture);
        let cards = parse_model_cards(&std::fs::read(file)?)?;
        let parsed = vdmos_literals(cards.first().ok_or("no model")?)?.ok_or("not VDMOS")?;
        let model = compile_ngspice(polarity, parsed.parameters)?;
        let observed = model.observe_soa(SoaSample {
            drain: vds,
            gate: vgs,
            source: 0.,
            channel: polarity.sign() * value(&output, "@m1[id]")?,
            diode: value(&output, "@m1[idio]")?,
            i_cgs: 0.,
            i_cgd: 0.,
            temperature,
        })?;
        let actual = observed
            .violations()
            .map(|v| warning_name(v.quantity))
            .collect::<Vec<_>>();
        let reference = output
            .lines()
            .filter_map(|line| {
                line.split_once("has exceeded ")
                    .map(|(_, tail)| tail.split('=').next().unwrap_or_default())
            })
            .collect::<Vec<_>>();
        assert_eq!(actual, reference, "{fixture}: {output}");
        assert!((model.parameters().phi - value(&output, "@test[phi]")?).abs() < 1e-14);
        let raw_effective = if model.parameters().soa.id_max.is_some() {
            model.parameters().soa.pd_max.unwrap_or(1e99)
        } else {
            1e99
        };
        assert_eq!(raw_effective, value(&output, "@test[pd_max]")?);
        if let Some(warning) = output.lines().find_map(|line| {
            line.split_once("has exceeded Pd_max=")
                .map(|(_, tail)| tail)
        }) {
            let reference_limit: f64 = warning
                .split_ascii_whitespace()
                .next()
                .ok_or("missing power limit")?
                .parse()?;
            assert!(
                (observed.power_limit - reference_limit).abs()
                    <= 5e-4 * reference_limit.abs().max(1.)
            );
        }
        if let ObservationTemperature::Fixed(temp) = temperature {
            check_independent_channel(&model, fixture, polarity, vgs, vds, temp, &output)?;
        }
    }
    Ok(())
}

#[test]
fn observation_metadata_preserves_complete_artifact_residual_and_storage() -> Result<()> {
    use model_kernel_api::PointBuffer;
    use model_kernel_vdmos::artifact::artifact;
    for polarity in [Polarity::N, Polarity::P] {
        let common = [
            ("rd", 0.05),
            ("rs", 0.02),
            ("rg", 1.),
            ("rb", 0.03),
            ("tt", 60e-9),
            ("vp", 0.4),
            ("cgs", 1e-9),
            ("cgdmin", 1e-11),
            ("cgdmax", 3e-10),
            ("cjo", 100e-12),
        ];
        let annotations = [
            ("phi", 0.9),
            ("vgs_max", 1.),
            ("vgsr_max", 2.),
            ("vgd_max", 2.),
            ("vgdr_max", 3.),
            ("vds_max", 0.1),
            ("id_max", 0.01),
            ("idr_max", 0.02),
            ("pd_max", 0.001),
            ("rth_ext", 99.),
            ("te_max", 25.),
            ("derating", 0.1),
            ("tnom", 27.),
        ];
        let a = Arc::new(compile_ngspice(
            polarity,
            common.into_iter().chain(annotations),
        )?);
        let b = Arc::new(compile_ngspice(
            polarity,
            common.into_iter().chain([("tnom", 27.)]),
        )?);
        for heating in [false, true] {
            let annotated = artifact(Arc::clone(&a), heating, [1; 16])?;
            let ordinary = artifact(Arc::clone(&b), heating, [2; 16])?;
            assert_eq!(
                annotated.descriptor().equations,
                ordinary.descriptor().equations
            );
            let aa = annotated.prepare(&[2.5], 320.)?;
            let bb = ordinary.prepare(&[2.5], 320.)?;
            let sign = polarity.sign();
            let coordinates = annotated
                .descriptor()
                .equations
                .iter()
                .map(|row| {
                    Ok(match row.name.as_str() {
                        "drain" => 2. * sign,
                        "gate" => 5.1 * sign,
                        "source" => 0.,
                        "drain_prime" => 1.9 * sign,
                        "gate_prime" => 5. * sign,
                        "source_prime" => 0.1 * sign,
                        "body_prime" => 0.02 * sign,
                        "junction" => 340.,
                        "case" => 320.,
                        "recovery" => 1e-8,
                        name => return Err(format!("unexpected row {name}")),
                    })
                })
                .collect::<std::result::Result<Vec<_>, String>>()?;
            let mut left = PointBuffer::new(coordinates.len())?;
            let mut right = PointBuffer::new(coordinates.len())?;
            annotated.evaluate(&aa, &coordinates, &mut left.point())?;
            ordinary.evaluate(&bb, &coordinates, &mut right.point())?;
            let l = left.point();
            let r = right.point();
            assert_eq!(l.current, r.current);
            assert_eq!(l.conductance, r.conductance);
            assert_eq!(l.charge, r.charge);
            assert_eq!(l.capacitance, r.capacitance);
        }
    }
    Ok(())
}
