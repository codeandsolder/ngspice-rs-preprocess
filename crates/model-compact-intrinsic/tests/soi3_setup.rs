use std::error::Error;

use model_compact_intrinsic::{
    BoundParameters, ModelVariant, ParameterScope, ParameterType, ParameterValue,
    soi3::{
        ChannelLengthModulation, CircuitDefaults, ThermalNetwork, prepare_instance, prepare_model,
    },
};

fn bind(
    scope: ParameterScope,
    fields: &str,
    polarity: Option<&str>,
) -> Result<BoundParameters, Box<dyn Error>> {
    let family = ModelVariant::Soi3;
    let mut values = Vec::new();
    for field in fields.split_whitespace() {
        let (name, value) = field.split_once('=').ok_or("expected assignment")?;
        let spec = family
            .parameter(scope, name)
            .ok_or_else(|| format!("unknown {name}"))?;
        let value = match spec.kind {
            ParameterType::Real => ParameterValue::Real(value.parse()?),
            ParameterType::Integer => ParameterValue::Integer(value.parse()?),
            _ => return Err("unexpected fixture type".into()),
        };
        values.push((name, value));
    }
    if let Some(polarity) = polarity {
        values.push((polarity, ParameterValue::Flag(true)));
    }
    Ok(BoundParameters::bind(family, scope, values)?)
}

fn real(p: &BoundParameters, name: &str) -> Result<f64, Box<dyn Error>> {
    let slot = p
        .variant()
        .parameter(p.scope(), name)
        .ok_or("missing slot")?
        .slot;
    match p.get_slot(slot) {
        Some(ParameterValue::Real(value)) => Ok(*value),
        _ => Err(format!("{name} has no real value").into()),
    }
}

const CIRCUIT: CircuitDefaults = CircuitDefaults {
    temperature_kelvin: 300.15,
    length_m: 5e-6,
    width_m: 5e-6,
};
const CARD: &str = "tof=1e-8 tob=2e-7 tb=3e-7 nsub=1e17";

fn close(actual: f64, expected: f64, label: &str, tolerance: f64) {
    assert!(
        (actual - expected).abs() <= tolerance * expected.abs().max(1e-18),
        "{label}: {actual:e} != {expected:e}"
    );
}

#[test]
fn film_threshold_clm_and_required_inputs() -> Result<(), Box<dyn Error>> {
    let m = prepare_model(&bind(ParameterScope::Model, CARD, Some("nsoi"))?, 300.15)?;
    close(
        m.film().front_oxide,
        3.453_143_799_69e-3,
        "front capacitance",
        1e-10,
    );
    close(real(m.parameters(), "phi")?, 0.814565, "phi", 1e-6);
    close(real(m.parameters(), "gamma")?, 0.527623, "gamma", 1e-6);
    close(real(m.parameters(), "kp")?, 0.000207189, "kp", 1e-5);
    assert!((2e-7..2.1e-7).contains(&m.minimum_body_thickness()));
    for (extra, expected) in [
        ("lx=1e-6 vp=2 mexp=2", ChannelLengthModulation::LxVp),
        (
            "lx=1e-6 vp=2 mexp=2 lambda=0",
            ChannelLengthModulation::Lambda,
        ),
        ("lx=1e-6 vp=2 mexp=0", ChannelLengthModulation::Lambda),
    ] {
        let m = prepare_model(
            &bind(
                ParameterScope::Model,
                &format!("{CARD} {extra}"),
                Some("nsoi"),
            )?,
            300.15,
        )?;
        assert_eq!(m.modulation(), expected);
    }
    assert!(
        prepare_model(
            &bind(ParameterScope::Model, "tof=1e-8 nsub=1e17", None)?,
            300.15
        )
        .is_err()
    );
    assert!(
        prepare_model(
            &bind(
                ParameterScope::Model,
                "tof=1e-8 tob=2e-7 tb=3e-7 nsub=1e9",
                None
            )?,
            300.15
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn thermal_ladder_multiplicity_and_source_resistance_precedence() -> Result<(), Box<dyn Error>> {
    let m = prepare_model(
        &bind(
            ParameterScope::Model,
            &format!("{CARD} fmin=0.5 rta=2e-7 cta=0.3 rd=50 rsh=25 rsw=10"),
            Some("nsoi"),
        )?,
        300.15,
    )?;
    let source = bind(
        ParameterScope::Instance,
        "l=2e-6 w=3e-6 m=4 nrd=0 nrs=2 temp=80 rt1=500 ct1=2e-12 rt3=1000",
        None,
    )?;
    let i = prepare_instance(&m, &source, CIRCUIT)?;
    let ThermalNetwork::Ladder(sections) = i.thermal() else {
        return Err("missing thermal ladder".into());
    };
    assert_eq!(sections.len(), 2);
    let area = 4e-6 * 4e-6;
    close(
        sections[0].resistance_k_per_w,
        2e-7 / area / 4.0,
        "RT",
        1e-12,
    );
    close(
        sections[0].capacitance_j_per_k,
        0.3 * area * 4.0,
        "CT",
        1e-12,
    );
    close(sections[1].resistance_k_per_w, 125.0, "RT1", 1e-12);
    close(sections[1].capacitance_j_per_k, 8e-12, "CT1", 1e-12);
    close(i.temperature().kelvin, 300.15, "thermal ambient", 1e-12);
    close(i.series()[0].conductance_s, 4.0 / 50.0, "RD", 1e-12);
    close(i.series()[1].conductance_s, 4.0 / 50.0, "RS", 1e-12);
    let off = prepare_instance(
        &m,
        &bind(ParameterScope::Instance, "rt=0 temp=80", None)?,
        CIRCUIT,
    )?;
    assert_eq!(off.thermal(), &ThermalNetwork::Isothermal);
    close(
        off.temperature().kelvin,
        353.15,
        "explicit temperature",
        1e-12,
    );
    Ok(())
}

#[cfg(target_os = "linux")]
mod native {
    use super::*;
    use std::{fs, process::Command};

    fn numeric(input: &str) -> impl Iterator<Item = (&str, f64)> {
        input.lines().filter_map(|line| {
            let mut fields = line.split_whitespace();
            let name = fields.next()?;
            let value = fields.next()?.parse().ok()?;
            fields.next().is_none().then_some((name, value))
        })
    }

    fn run(
        model_fields: &str,
        instance_fields: &str,
        polarity: &str,
        ambient: f64,
    ) -> Result<(usize, usize), Box<dyn Error>> {
        let model = prepare_model(
            &bind(ParameterScope::Model, model_fields, Some(polarity))?,
            300.15,
        )?;
        let circuit = CircuitDefaults {
            temperature_kelvin: ambient + 273.15,
            ..CIRCUIT
        };
        let instance = prepare_instance(
            &model,
            &bind(ParameterScope::Instance, instance_fields, None)?,
            circuit,
        )?;
        let sign = if polarity == "psoi" { "-" } else { "" };
        let deck = format!(
            "STAG native setup witness\n.temp {ambient}\n.model m nmos level=60 {polarity} {model_fields}\nVd d 0 {sign}0.1\nVg g 0 {sign}0.8\nM1 d g 0 0 0 th m {instance_fields}\n.control\nset num_threads=1\nop\nshowmod m1\nshow m1\nquit\n.endc\n.end\n"
        );
        let tmp = tempfile::tempdir()?;
        fs::write(tmp.path().join("witness.cir"), &deck)?;
        let output = Command::new("ngspice")
            .args(["-b", "-n", "witness.cir"])
            .current_dir(tmp.path())
            .output()?;
        let report = String::from_utf8(output.stdout)?;
        assert!(
            output.status.success() && report.contains("No. of Data Rows : 1"),
            "{deck}\n{report}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let (_, report) = report
            .split_once(" models (")
            .ok_or("missing model report")?;
        let (model_report, instance_report) = report
            .split_once(" SOI3:")
            .ok_or("missing instance report")?;
        let mut model_count = 0;
        let mut instance_count = 0;
        let mut errors = Vec::new();
        let mut check = |name: &str, actual: f64, expected: f64| {
            if (actual - expected).abs() > 1e-5 * expected.abs().max(1e-18) {
                errors.push(format!("{name}: {actual:e} != {expected:e}"));
            }
        };
        for (name, expected) in numeric(model_report) {
            let Some(spec) = ModelVariant::Soi3.parameter(ParameterScope::Model, name) else {
                continue;
            };
            let actual = match name {
                "chid" => instance.temperature().junction_activation[0],
                "chid1" => instance.temperature().junction_activation[1],
                "tnom" => real(model.parameters(), name)? - 273.15,
                _ => match model.parameters().get_slot(spec.slot) {
                    Some(ParameterValue::Real(value)) => *value,
                    Some(ParameterValue::Integer(value)) => f64::from(*value),
                    _ => return Err(format!("no model value {name}").into()),
                },
            };
            check(name, actual, expected);
            model_count += 1;
        }
        let state = instance.temperature();
        let mult = real(instance.parameters(), "m")?;
        for (name, expected) in numeric(instance_report) {
            let actual = match name {
                "sourcecondu" => instance.series()[1].conductance_s,
                "drainconduc" => instance.series()[0].conductance_s,
                "sourcevcrit" | "drainvcrit" => state.critical_voltage,
                "von" => state.threshold,
                "vfbf" => state.front_flatband,
                "vfbb" => state.back_flatband,
                "cbd0" => state.zero_bias_capacitance[0],
                "cbs0" => state.zero_bias_capacitance[1],
                "temp" => state.kelvin - 273.15,
                _ => {
                    if ModelVariant::Soi3
                        .parameter(ParameterScope::Instance, name)
                        .is_none()
                    {
                        continue;
                    }
                    let value = real(instance.parameters(), name)?;
                    if name.starts_with("rt") {
                        value / mult
                    } else if name.starts_with("ct") {
                        value * mult
                    } else {
                        value
                    }
                }
            };
            check(name, actual, expected);
            instance_count += 1;
        }
        assert!(errors.is_empty(), "{polarity}: {}", errors.join("\n"));
        println!("{polarity}: {model_count} model, {instance_count} instance values");
        Ok((model_count, instance_count))
    }

    #[test]
    #[ignore = "requires independent ngspice 47 executable"]
    fn native_setup_matrix() -> Result<(), Box<dyn Error>> {
        let mut total = (0, 0);
        let cases = [
            (CARD.to_owned(), "l=5e-6 w=5e-6", "nsoi", 27.0),
            (
                format!(
                    "{CARD} tnom=30 u0=450 nqff=1e10 nqfb=2e10 tpg=-1 rd=80 rsw=200 cj=0.002 fmin=0.5 rta=1e-7 cta=0.3"
                ),
                "l=4e-6 w=5e-6 m=2 rt1=500 ct1=1e-12 temp=90",
                "psoi",
                60.0,
            ),
            (
                format!(
                    "{CARD} vtex=0.6 vdex=-0.1 delta0=0.1 etad=0 etad1=0 cbd=2e-15 cbs=3e-15 rsh=50"
                ),
                "l=3e-6 w=4e-6 nrd=2 nrs=3 m=3 rt=0 temp=45",
                "nsoi",
                15.0,
            ),
            (
                "tof=2e-8 tob=2e-7 tb=6e-7 phi=0.7 gamma=0.5 vt0=-0.65 kp=2e-5 kox=0 rta=0"
                    .to_owned(),
                "l=3e-6 w=5e-6",
                "psoi",
                27.0,
            ),
        ];
        for (model, instance, polarity, temp) in cases {
            let (mc, ic) = run(&model, instance, polarity, temp)?;
            total.0 += mc;
            total.1 += ic;
        }
        println!("STAG: {} model and {} instance values", total.0, total.1);
        Ok(())
    }
}
