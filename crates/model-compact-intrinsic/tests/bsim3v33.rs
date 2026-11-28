use ParameterValue::{Integer, Real, RealVector};
use model_compact_intrinsic::{
    BoundParameters, ModelVariant, MosPolarity, ParameterScope, ParameterValue,
    bsim3v33::{Bias, ModelDefaults, ModelSetup, bind_instance},
};
use std::collections::BTreeMap;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn defaults(
    input: &[(&str, ParameterValue)],
    polarity: MosPolarity,
) -> Result<ModelDefaults, model_compact_intrinsic::bsim3v33::Error> {
    let source = BoundParameters::bind(
        ModelVariant::Bsim3V33,
        ParameterScope::Model,
        input.iter().cloned(),
    )?;
    ModelDefaults::prepare(&source, polarity, 300.15)
}

fn close(name: &str, actual: f64, expected: f64, relative: f64, absolute: f64) {
    let tolerance = relative.mul_add(actual.abs().max(expected.abs()), absolute);
    assert!(
        actual.is_finite() && expected.is_finite() && (actual - expected).abs() <= tolerance,
        "{name}: actual {actual:.17e}, expected {expected:.17e}"
    );
}

#[test]
fn defaulted_slots_preserve_aliases_and_given_bits() -> TestResult {
    for polarity in [MosPolarity::Nmos, MosPolarity::Pmos] {
        let model = defaults(&[], polarity)?;
        let signed = if polarity == MosPolarity::Nmos {
            1.
        } else {
            -1.
        };
        assert_eq!(model.get("vth0"), Some(0.7 * signed));
        assert_eq!(model.get("capmod"), Some(3.));
        assert_eq!(model.get("tnom"), Some(300.15));
        assert!(!model.is_given("vth0"));
        for spec in ModelVariant::Bsim3V33.parameters(ParameterScope::Model) {
            if matches!(
                spec.kind,
                model_compact_intrinsic::ParameterType::Real
                    | model_compact_intrinsic::ParameterType::Integer
            ) {
                assert!(
                    model.get(spec.name).is_some(),
                    "unrepresented {}",
                    spec.name
                );
                assert!(!model.is_given(spec.name), "{}", spec.name);
            }
        }
    }
    let model = defaults(
        &[
            ("vth0", Real(0.5)),
            ("vtho", Real(0.6)),
            ("tox", Real(8e-9)),
            ("nch", Real(2e23)),
            ("tnom", Real(50.)),
            ("nqsmod", Integer(8)),
            ("cjsw", Real(3e-10)),
            ("mobmod", Integer(3)),
        ],
        MosPolarity::Nmos,
    )?;
    assert_eq!(model.get("vth0"), Some(0.6));
    assert!(model.is_given("vtho"));
    assert_eq!(model.get("toxm"), Some(8e-9));
    assert!(!model.is_given("toxm"));
    close("doping", model.get("nch").ok_or("nch")?, 2e17, 1e-14, 0.);
    assert_eq!(model.get("tnom"), Some(323.15));
    assert_eq!(model.get("nqsmod"), Some(0.));
    assert_eq!(model.warnings.len(), 1);
    assert_eq!(model.get("cjswg"), Some(3e-10));
    assert_eq!(model.get("uc"), Some(-0.0465));
    Ok(())
}

#[test]
fn ordered_instance_ic_and_model_check_state_are_transactional() -> TestResult {
    let model = defaults(
        &[
            ("paramchk", Integer(1)),
            ("cgdo", Real(-1e-12)),
            ("calcacm", Integer(3)),
        ],
        MosPolarity::Nmos,
    )?;
    let mut setup = ModelSetup::new(model, 300.15)?;
    let invalid = bind_instance([("l", Real(0.)), ("w", Real(2e-6))])?;
    assert!(setup.prepare_instance(&invalid).is_err());
    let first = bind_instance([
        ("l", Real(1e-6)),
        ("w", Real(2e-6)),
        ("icvds", Real(9.)),
        ("ic", RealVector(vec![1., 2., 3.])),
        ("icvgs", Real(4.)),
        ("ic", RealVector(vec![5.])),
    ])?;
    let a = setup.prepare_instance(&first)?;
    assert_eq!(
        [a.setup().ic_vds, a.setup().ic_vgs, a.setup().ic_vbs],
        [5., 4., 3.]
    );
    assert!(
        a.setup()
            .warnings
            .iter()
            .any(|d| d.message.contains("CALCACM"))
    );
    let second = bind_instance([("l", Real(2e-6)), ("w", Real(2e-6))])?;
    let b = setup.prepare_instance(&second)?;
    assert!(
        !b.setup()
            .warnings
            .iter()
            .any(|d| d.message.contains("CALCACM"))
    );
    let bias = Bias {
        vbs: -0.2,
        vgs: 1.5,
        vds: 0.6,
    };
    let before = a.evaluate(bias, 1e-12)?;
    let _ = b.evaluate(bias, 1e-12)?;
    assert_eq!(a.evaluate(bias, 1e-12)?, before);
    assert!(bind_instance([("ic", RealVector(vec![]))]).is_err());
    assert!(bind_instance([("ic", RealVector(vec![f64::NAN]))]).is_err());
    Ok(())
}

#[test]
fn independent_channel_derivatives_and_charge_conservation() -> TestResult {
    let mut setup = ModelSetup::new(defaults(&[], MosPolarity::Nmos)?, 350.)?;
    let instance =
        setup.prepare_instance(&bind_instance([("l", Real(1e-6)), ("w", Real(5e-6))])?)?;
    for vgs in [0.2, 0.8, 1.2, 2.] {
        for vds in [0.03, 0.5, 1.5] {
            let bias = Bias {
                vbs: -0.2,
                vgs,
                vds,
            };
            let point = instance.evaluate(bias, 0.)?;
            let h = 1e-5;
            let central = |plus: Bias,
                           minus: Bias|
             -> Result<f64, model_compact_intrinsic::bsim3v33::Error> {
                Ok(
                    (instance.evaluate(plus, 0.)?.dc.ids - instance.evaluate(minus, 0.)?.dc.ids)
                        / (2. * h),
                )
            };
            close(
                "gm",
                point.dc.gm,
                central(
                    Bias {
                        vgs: vgs + h,
                        ..bias
                    },
                    Bias {
                        vgs: vgs - h,
                        ..bias
                    },
                )?,
                1e-5,
                1e-14,
            );
            close(
                "gds",
                point.dc.gds,
                central(
                    Bias {
                        vds: vds + h,
                        ..bias
                    },
                    Bias {
                        vds: vds - h,
                        ..bias
                    },
                )?,
                1e-5,
                1e-14,
            );
            close(
                "gmbs",
                point.dc.gmbs,
                central(
                    Bias {
                        vbs: bias.vbs + h,
                        ..bias
                    },
                    Bias {
                        vbs: bias.vbs - h,
                        ..bias
                    },
                )?,
                1e-5,
                1e-14,
            );
            let q = point.charge.with_overlap(point.dc.mode, vgs - bias.vbs);
            close(
                "KCL charge",
                q.qgate + q.qbulk + q.qdrn + q.qsrc,
                0.,
                0.,
                1e-28,
            );
            let noise = instance.noise(bias, 0.)?;
            assert!(noise.channel.is_finite() && noise.channel >= 0.);
        }
    }
    Ok(())
}

fn render(values: &[(&str, ParameterValue)]) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    for (name, value) in values {
        match value {
            Real(value) => {
                let _ = write!(out, " {name}={value:e}");
            }
            Integer(value) => {
                let _ = write!(out, " {name}={value}");
            }
            _ => {}
        }
    }
    out
}

fn native(deck: &str) -> Result<BTreeMap<String, f64>, Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    std::fs::write(dir.path().join("witness.cir"), deck)?;
    let output = std::process::Command::new("ngspice")
        .current_dir(dir.path())
        .args(["-b", "witness.cir"])
        .output()?;
    let log = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && !log.contains("Fatal error") && !log.contains("aborted"),
        "{log}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(log
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once(" = ")?;
            let name = key.trim().strip_prefix("@m1[")?.strip_suffix(']')?;
            Some((name.into(), value.trim().parse().ok()?))
        })
        .collect())
}

const ASKS: [&str; 25] = [
    "vth", "id", "gm", "gds", "gmbs", "vdsat", "vbs", "vgs", "vds", "ibs", "ibd", "gbs", "gbd",
    "qg", "qb", "qd", "cgg", "cgd", "cgs", "cdg", "cdd", "cds", "cbg", "cbd", "cbs",
];

fn compare_native(
    model: &[(&str, ParameterValue)],
    instance: &[(&str, ParameterValue)],
    polarity: MosPolarity,
    temp_c: f64,
    bias: Bias,
) -> TestResult {
    let (family, sign) = if polarity == MosPolarity::Nmos {
        ("nmos", 1.)
    } else {
        ("pmos", -1.)
    };
    let source = format!(
        "BSIM3 v3.3 independent witness\nvd d 0 {:e}\nvg g 0 {:e}\nvb b 0 {:e}\nm1 d g 0 b n{}\n.model n {family} level=8 version=3.3.0{}\n.options temp={temp_c} tnom=27 bypass=0\n.control\nset numdgt=17\nop\nprint {}\nquit\n.endc\n.end\n",
        sign * bias.vds,
        sign * bias.vgs,
        sign * bias.vbs,
        render(instance),
        render(model),
        ASKS.map(|name| format!("@m1[{name}]")).join(" ")
    );
    let asks = native(&source)?;
    assert_eq!(asks.len(), ASKS.len(), "{source}");
    let mut setup = ModelSetup::new(defaults(model, polarity)?, temp_c + 273.15)?;
    let prepared = setup.prepare_instance(&bind_instance(instance.iter().cloned())?)?;
    let actual_bias = Bias {
        vbs: asks["vbs"],
        vgs: asks["vgs"],
        vds: asks["vds"],
    };
    if prepared.setup().drain_conductance == 0. && prepared.setup().source_conductance == 0. {
        close("applied VDS", actual_bias.vds, bias.vds, 1e-9, 1e-12);
        close("applied VGS", actual_bias.vgs, bias.vgs, 1e-9, 1e-12);
        close("applied VBS", actual_bias.vbs, bias.vbs, 1e-9, 1e-12);
    }
    let result = prepared.evaluate(actual_bias, 1e-12)?;
    let dc = &result.dc;
    for name in [
        "vth", "id", "gm", "gds", "vdsat", "ibs", "ibd", "gbs", "gbd",
    ] {
        close(
            &format!("{name} {bias:?}"),
            dc.ask(name).ok_or("missing DC ask")?,
            asks[name],
            1e-9,
            1e-16,
        );
    }
    // C differentiates the discarded smoothing law even when the
    // body-potential clamp is active. The correct branch derivative is
    // separately verified by terminal finite differences.
    if dc.vbs < 0. {
        close("gmbs", dc.gmbs, asks["gmbs"], 1e-9, 1e-16);
    }
    let charge = result.charge;
    let q = charge.with_overlap(dc.mode, actual_bias.vgs - actual_bias.vbs);
    for (name, value) in [
        ("qg", q.qg),
        ("qb", q.qb),
        ("qd", q.qd),
        ("cgg", charge.cggb),
        ("cgd", charge.cgdb),
        ("cdg", charge.cdgb),
        ("cdd", charge.cddb),
        ("cbg", charge.cbgb),
        ("cbd", charge.cbdb),
    ] {
        close(name, value, asks[name], 1e-9, 1e-22);
    }
    // The remaining three source columns include upstream's erroneous
    // CTM body derivative via gauge invariance. The complete physical
    // Jacobian is independently tested in terminal_jacobians_cover_both_polarities_and_modes.
    Ok(())
}

#[test]
#[ignore = "requires independent ngspice 47 binary on PATH"]
fn native_v33_dc_charge_reference_matrix() -> TestResult {
    let geometry = [
        ("l", Real(0.5e-6)),
        ("w", Real(4e-6)),
        ("ad", Real(2e-12)),
        ("as", Real(1e-12)),
        ("pd", Real(6e-6)),
        ("ps", Real(3e-6)),
    ];
    let cards = [
        vec![],
        vec![
            ("mobmod", Integer(2)),
            ("xpart", Real(1.)),
            ("vth0", Real(0.5)),
            ("tnom", Real(50.)),
        ],
        vec![
            ("mobmod", Integer(3)),
            ("xpart", Real(0.5)),
            ("cjsw", Real(1e-10)),
            ("cjswg", Real(2e-10)),
            ("rsh", Real(10.)),
        ],
    ];
    for polarity in [MosPolarity::Nmos, MosPolarity::Pmos] {
        for model in &cards {
            for temp in [0., 27., 85.] {
                for (vgs, vds) in [(0.3, 0.1f64), (1.2, 0.05), (1.5, 0.8), (1.2, -0.5)] {
                    for body in [-0.3, 0., 0.1] {
                        compare_native(
                            model,
                            &geometry,
                            polarity,
                            temp,
                            Bias {
                                vbs: vds.min(0.) + body,
                                vgs,
                                vds,
                            },
                        )?;
                    }
                }
            }
        }
    }
    Ok(())
}

#[test]
fn terminal_jacobians_cover_both_polarities_and_modes() -> TestResult {
    for polarity in [MosPolarity::Nmos, MosPolarity::Pmos] {
        let sign = if polarity == MosPolarity::Nmos {
            1.
        } else {
            -1.
        };
        let mut model = ModelSetup::new(
            defaults(
                &[
                    ("alpha0", Real(1e-8)),
                    ("beta0", Real(0.1)),
                    ("cgdo", Real(2e-10)),
                    ("cgso", Real(3e-10)),
                ],
                polarity,
            )?,
            330.,
        )?;
        let instance = model.prepare_instance(&bind_instance([
            ("l", Real(0.5e-6)),
            ("w", Real(3e-6)),
            ("m", Real(3.)),
            ("ad", Real(1e-12)),
            ("as", Real(2e-12)),
            ("pd", Real(3e-6)),
            ("ps", Real(4e-6)),
        ])?)?;
        for drain in [0.07f64, 0.9, -0.07, -0.9] {
            for (gate, body) in [0.3, 0.9, 1.8]
                .into_iter()
                .flat_map(|gate| [-1.1, -0.02, 0.02, 0.2].map(|body| (gate, body)))
            {
                let voltage =
                    [drain, gate, 0., drain.min(0.) + body].map(|v| f64::mul_add(sign, v, 0.2));
                let point = instance.terminal_point(voltage, 1e-12)?;
                close("KCL", point.current.iter().sum(), 0., 0., 1e-15);
                close(
                    "charge conservation",
                    point.charge.iter().sum(),
                    0.,
                    0.,
                    1e-27,
                );
                for column in 0..4 {
                    let mut plus = voltage;
                    let mut minus = voltage;
                    let h = 1e-5;
                    plus[column] += h;
                    minus[column] -= h;
                    let plus = instance.terminal_point(plus, 1e-12)?;
                    let minus = instance.terminal_point(minus, 1e-12)?;
                    for row in 0..4 {
                        close(
                            &format!(
                                "terminal conductance {polarity:?} {voltage:?} [{row},{column}]"
                            ),
                            point.conductance[row][column],
                            (plus.current[row] - minus.current[row]) / (2. * h),
                            2e-5,
                            1e-13,
                        );
                        close(
                            &format!(
                                "terminal capacitance {polarity:?} {voltage:?} [{row},{column}]"
                            ),
                            point.capacitance[row][column],
                            (plus.charge[row] - minus.charge[row]) / (2. * h),
                            2e-5,
                            1e-22,
                        );
                    }
                }
            }
        }
    }
    Ok(())
}
