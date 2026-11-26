use std::error::Error;

use model_compact_intrinsic::{
    BoundParameters, ModelError, ModelVariant, ParameterScope, ParameterType, ParameterValue,
    bsim3soi::{PreparedModel, prepare_instance, prepare_model},
};

const FAMILIES: [ModelVariant; 3] = [
    ModelVariant::Bsim3SoiFd,
    ModelVariant::Bsim3SoiDd,
    ModelVariant::Bsim3SoiPd,
];

fn bind(
    family: ModelVariant,
    scope: ParameterScope,
    fields: &str,
    polarity: Option<&str>,
) -> Result<BoundParameters, Box<dyn Error>> {
    let mut values = Vec::new();
    for field in fields.split_whitespace() {
        let (name, value) = field.split_once('=').ok_or("expected assignment")?;
        let spec = family
            .parameter(scope, name)
            .ok_or_else(|| ModelError::UnknownParameter(name.to_owned()))?;
        let value = match spec.kind {
            ParameterType::Real => ParameterValue::Real(value.parse()?),
            ParameterType::Integer => ParameterValue::Integer(value.parse()?),
            ParameterType::Flag => ParameterValue::Flag(value.parse::<i32>()? != 0),
            _ => return Err("unexpected fixture type".into()),
        };
        values.push((name, value));
    }
    if let Some(polarity) = polarity {
        values.push((polarity, ParameterValue::Flag(true)));
    }
    Ok(BoundParameters::bind(family, scope, values)?)
}

fn model(
    family: ModelVariant,
    fields: &str,
    polarity: &str,
) -> Result<PreparedModel, Box<dyn Error>> {
    Ok(prepare_model(
        &bind(family, ParameterScope::Model, fields, Some(polarity))?,
        300.15,
    )?)
}

fn slot(family: ModelVariant, scope: ParameterScope, name: &str) -> Result<u16, ModelError> {
    family
        .parameter(scope, name)
        .map(|s| s.slot)
        .ok_or_else(|| ModelError::UnknownParameter(name.to_owned()))
}

fn real(p: &BoundParameters, name: &str) -> Result<f64, ModelError> {
    let id = slot(p.variant(), p.scope(), name)?;
    match p.get_slot(id) {
        Some(ParameterValue::Real(v)) => Ok(*v),
        _ => Err(ModelError::MissingDefaultDependency(id)),
    }
}

fn close(a: f64, b: f64, label: &str) {
    assert!(
        (a - b).abs() <= 1e-12 * b.abs().max(1e-30),
        "{label}: {a:e} != {b:e}"
    );
}

#[test]
fn ordered_defaults_aliases_and_given_flags() -> Result<(), Box<dyn Error>> {
    for family in FAMILIES {
        let m = model(
            family,
            "tnom=30 tox=2e-8 tsi=2e-7 nch=2e23 ngate=2e25 mobmod=3 drout=0.75 lint=1e-8 dlc=2e-8 cgdl=1e-12 cgsl=2e-12 xdif=3.2",
            "pmos",
        )?;
        let p = m.parameters();
        for (name, expected) in [
            ("tnom", 303.15),
            ("nch", 2e17),
            ("ngate", 2e19),
            ("vth0", -0.7),
            ("u0", 0.025),
            ("uc", -0.0465),
            ("uc1", -0.056),
            ("dsub", 0.75),
            ("xj", 2e-7),
            ("noia", 9.9e18),
            ("noib", 2.4e3),
            ("noic", 1.4e-12),
        ] {
            close(real(p, name)?, expected, name);
        }
        let cox = 3.453_133e-11 / 2e-8;
        close(m.oxide_capacitance_f_per_m2(), cox, "cox");
        close(real(p, "cgdo")?, 2e-8 * cox - 1e-12, "cgdo");
        close(real(p, "cgso")?, 2e-8 * cox - 2e-12, "cgso");
        assert!(!m.explicitly_given(slot(family, ParameterScope::Model, "vth0")?));
        assert!(m.explicitly_given(slot(family, ParameterScope::Model, "dlc")?));
        assert!(
            p.get_slot(slot(family, ParameterScope::Model, "k1")?)
                .is_none()
        );
        if family == ModelVariant::Bsim3SoiPd {
            close(real(p, "xbjt")?, 1.0, "PD xbjt");
            close(m.diffusion_temperature_exponent(), 3.2, "PD xdif");
        } else {
            close(real(p, "xbjt")?, 3.2, "FD/DD xbjt alias");
            for name in ["ldvbd0", "ldvbd1", "wdvbd0", "wdvbd1", "pdvbd0", "pdvbd1"] {
                close(real(p, name)?, 1.0, name);
            }
            close(
                m.diffusion_temperature_exponent(),
                2.0,
                "FD/DD internal xdif",
            );
        }
    }
    assert!(model(ModelVariant::Bsim3SoiPd, "ddmod=1", "nmos").is_err());
    assert!(model(ModelVariant::Bsim3SoiFd, "capmod=0", "nmos").is_err());
    Ok(())
}

#[test]
fn per_family_geometry_raw_binning_and_initial_conditions() -> Result<(), Box<dyn Error>> {
    for family in FAMILIES {
        let pd = family == ModelVariant::Bsim3SoiPd;
        let mut fields="lint=1e-7 wint=2e-7 dlc=1.5e-7 dwc=2.5e-7 vth0=0.4 lvth0=0.2 wvth0=0.3 pvth0=0.4 lk1=0.1 rth0=500 cth0=2e-12".to_owned();
        if pd {
            fields.push_str(" dwbc=3e-7 dlcb=1e-7 dlbg=2e-7");
        }
        let m = model(family, &fields, "nmos")?;
        let mut source = bind(
            family,
            ParameterScope::Instance,
            if pd {
                "l=2e-6 w=3e-6 nbc=1 nseg=2 pdbcp=1e-7 psbcp=2e-7"
            } else {
                "l=2e-6 w=3e-6"
            },
            None,
        )?;
        // Exercise all five explicit vector components, including a supplied zero.
        let ic = family
            .parameter(ParameterScope::Instance, "ic")
            .ok_or("no IC")?;
        let mut values: Vec<_> = family
            .parameters(ParameterScope::Instance)
            .iter()
            .filter_map(|s| source.get_slot(s.slot).map(|v| (s.name, v.clone())))
            .collect();
        values.push((
            ic.name,
            ParameterValue::RealVector(vec![0.1, 0.0, 0.3, 0.4, 0.5]),
        ));
        source = BoundParameters::bind(family, ParameterScope::Instance, values)?;
        let i = prepare_instance(&m, &source)?;
        let g = i.geometry();
        close(g.length_m, 1.8e-6, "length");
        close(g.cv_length_m, 1.7e-6, "cv length");
        close(g.width_m, if pd { 2.5e-6 } else { 2.6e-6 }, "width");
        close(g.cv_width_m, if pd { 2.45e-6 } else { 2.5e-6 }, "cv width");
        close(
            g.abulk_cv_factor,
            if pd { 2.0 } else { 1.0 },
            "abulk factor",
        );
        assert_eq!(
            i.initial_conditions(),
            [Some(0.1), Some(0.0), Some(0.3), Some(0.4), Some(0.5)]
        );
        close(real(i.parameters(), "rth0")?, 500.0, "rth0");
        close(real(i.parameters(), "cth0")?, 2e-12, "cth0");
        let v = i
            .raw_binned_slot(slot(family, ParameterScope::Model, "vth0")?)
            .ok_or("missing bin")?;
        close(
            v.prelude_value(),
            0.4 + 0.2e-6 / g.length_m + 0.3e-6 / g.width_m + 0.4e-12 / (g.length_m * g.width_m),
            "vth bin",
        );
        let k1 = i
            .raw_binned_slot(slot(family, ParameterScope::Model, "k1")?)
            .ok_or("missing k1 bin")?;
        assert_eq!(k1.base, None);
        close(k1.correction, 0.1e-6 / g.length_m, "k1 bin");
        assert_eq!(
            family
                .parameters(ParameterScope::Model)
                .iter()
                .filter(|s| i.raw_binned_slot(s.slot).is_some())
                .count(),
            if pd { 90 } else { 80 }
        ); // vth0/vtho aliases
        if let Some(g) = g.pd {
            close(g.body_cv_length, 1.6e-6, "body CV");
            close(g.backgate_cv_length, 2e-6, "backgate CV");
            close(g.drain_diode_width, 1.35e-6, "drain segment");
            close(g.source_diode_width, 1.45e-6, "source segment");
        } else {
            assert!(!pd);
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
mod native {
    use super::*;
    use std::{fs, process::Command};

    const DEFERRED: &[&str] = &[
        "gamma1", "gamma2", "vbx", "k1", "k2", "vsdfb", "vsdth", "csdmin",
    ];

    fn numeric(input: &str) -> impl Iterator<Item = (&str, f64)> {
        input.lines().filter_map(|line| {
            let mut f = line.split_whitespace();
            let name = f.next()?;
            let value = f.next()?.parse::<f64>().ok()?;
            f.next().is_none().then_some((name, value))
        })
    }

    fn run(
        family: ModelVariant,
        fields: &str,
        polarity: &str,
    ) -> Result<(usize, usize), Box<dyn Error>> {
        let m = model(family, fields, polarity)?;
        let level = match family {
            ModelVariant::Bsim3SoiFd => 55,
            ModelVariant::Bsim3SoiDd => 56,
            ModelVariant::Bsim3SoiPd => 57,
            _ => return Err("not BSIM3SOI".into()),
        };
        let sign = if polarity == "pmos" { "-" } else { "" };
        let instance = "l=5e-6 w=5e-6 pd=8e-6 ps=8e-6 m=2 nrd=2 nrs=3 nrb=4 ad=2e-12 as=3e-12";
        let i = prepare_instance(&m, &bind(family, ParameterScope::Instance, instance, None)?)?;
        let deck = format!(
            "BSIM3SOI setup witness\n.model m {polarity} level={level} {fields}\nVd d 0 {sign}0.1\nVg g 0 {sign}0.8\nM1 d g 0 0 m {instance}\n.control\nset num_threads=1\nop\nshowmod m1\nshow m1\nquit\n.endc\n.end\n"
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
        let (_, model_report) = report
            .split_once(" models (")
            .ok_or("missing model report")?;
        let (model_report, instance_report) = model_report
            .split_once(" B3SOI")
            .ok_or("missing instance report")?;
        let mut model_count = 0;
        let mut instance_count = 0;
        let mut errors = Vec::new();
        for (report, p, count) in [
            (model_report, m.parameters(), &mut model_count),
            (instance_report, i.parameters(), &mut instance_count),
        ] {
            for (name, e) in numeric(report) {
                // FD/DD native mask getters for six DVBD bin inputs return
                // the unbinned base instead of the actual stored coefficient.
                // Compare what the getter actually exposes; independently
                // exercise the real coefficients below.
                let lookup = if level != 57
                    && p.scope() == ParameterScope::Model
                    && matches!(
                        name,
                        "ldvbd0" | "ldvbd1" | "wdvbd0" | "wdvbd1" | "pdvbd0" | "pdvbd1"
                    ) {
                    &name[1..]
                } else {
                    name
                };
                let Some(spec) = family.parameter(p.scope(), lookup) else {
                    continue;
                };
                let a = match p.get_slot(spec.slot) {
                    Some(ParameterValue::Real(v)) => *v,
                    Some(ParameterValue::Integer(v)) => f64::from(*v),
                    Some(ParameterValue::Flag(v)) => f64::from(u8::from(*v)),
                    None if p.scope() == ParameterScope::Model && DEFERRED.contains(&name) => {
                        continue;
                    }
                    other => return Err(format!("{name} missing: {other:?}").into()),
                };
                if (a - e).abs() > 1e-5 * e.abs().max(1e-99) {
                    errors.push(format!("{name}: {a:e} != {e:e}"));
                }
                *count += 1;
            }
        }
        assert!(
            errors.is_empty(),
            "{family:?} {polarity}: {}",
            errors.join("\n")
        );
        assert_eq!(model_count, if level == 57 { 457 } else { 377 });
        println!("{family:?} {polarity}: {model_count} model, {instance_count} instance");
        Ok((model_count, instance_count))
    }

    #[test]
    #[ignore = "requires independent ngspice 47 executable"]
    fn native_setup_matrix() -> Result<(), Box<dyn Error>> {
        let mut total = (0, 0);
        for family in FAMILIES {
            for (fields, polarity) in [
                ("", "nmos"),
                (
                    "tnom=30 tox=2e-8 tsi=2e-7 nch=2e23 ngate=2e25 mobmod=3 drout=0.75 lint=1e-8 dlc=2e-8 cgdl=1e-12 cgsl=2e-12 xdif=3.2",
                    "pmos",
                ),
            ] {
                let (m, i) = run(family, fields, polarity)?;
                total.0 += m;
                total.1 += i;
            }
        }
        println!(
            "BSIM3SOI: {} independent model and {} instance values",
            total.0, total.1
        );
        Ok(())
    }
}

#[test]
fn native_internal_temperature_coefficients() -> Result<(), Box<dyn Error>> {
    use model_compact_intrinsic::bsim3soi::prepare_temperature;
    let mut checked = 0;
    let mut cases = 0;
    let mut max_rel = 0.0_f64;
    let mut failures = Vec::new();
    let mut lines = include_str!("fixtures/bsim3soi-thermal/thermal.tsv")
        .lines()
        .peekable();
    while let Some(header) = lines.next() {
        let columns = header.split('\t').collect::<Vec<_>>();
        let ["case", family, name, temp, model, instance] = columns.as_slice() else {
            return Err("invalid oracle header".into());
        };
        let family = match *family {
            "fd" => ModelVariant::Bsim3SoiFd,
            "dd" => ModelVariant::Bsim3SoiDd,
            "pd" => ModelVariant::Bsim3SoiPd,
            _ => return Err("unknown oracle family".into()),
        };
        let model = prepare_model(&bind(family, ParameterScope::Model, model, None)?, 300.15)?;
        let instance = prepare_instance(
            &model,
            &bind(family, ParameterScope::Instance, instance, None)?,
        )?;
        let t = prepare_temperature(&model, &instance, temp.parse()?)?;
        let g = instance.geometry();
        let e = t.electrostatics;
        let j = t.junctions;
        let tr = t.transport;
        let mat = t.material;
        let mut values = std::collections::BTreeMap::from([
            ("leff", g.length_m),
            ("weff", g.width_m),
            ("leffCV", g.cv_length_m),
            ("weffCV", g.cv_width_m),
            ("abulkCVfactor", g.abulk_cv_factor),
            ("ua", tr.mobility_coefficients[0]),
            ("ub", tr.mobility_coefficients[1]),
            ("uc", tr.mobility_coefficients[2]),
            ("uatemp", tr.nominal_mobility_coefficients[0]),
            ("ubtemp", tr.nominal_mobility_coefficients[1]),
            ("uctemp", tr.nominal_mobility_coefficients[2]),
            ("u0", tr.nominal_mobility),
            ("u0temp", tr.mobility),
            ("vsattemp", tr.saturation_velocity),
            ("rds0denom", tr.resistance_denominator),
            ("rds0", tr.series_resistance),
            ("rth", tr.thermal_resistance),
            ("cth", tr.thermal_capacitance),
            ("rbody", tr.body_resistance),
            ("cgdo", t.overlaps[0]),
            ("cgso", t.overlaps[1]),
            ("cgeo", t.overlaps[2]),
            ("npeak", e.channel_doping),
            ("jbjt", j.bipolar),
            ("jdif", j.diffusion),
            ("jrec", j.recombination),
            ("jtun", j.tunneling),
            ("vfbb", e.back_flatband),
            ("vsdfb", e.substrate_flatband),
            ("vsdth", e.substrate_threshold),
            ("phi", e.phi),
            ("sqrtPhi", e.phi.sqrt()),
            ("phis3", e.phi * e.phi.sqrt()),
            ("Xdep0", e.depletion_depth),
            ("sqrtXdep0", e.depletion_depth.sqrt()),
            ("litl", e.lateral_length),
            ("vbi", e.built_in_potential),
            ("cdep0", e.depletion_capacitance),
            ("k1", e.k1),
            ("k2", e.k2),
            ("vbsc", e.body_bias_limit),
            ("vfb", e.flatband),
            ("vth0", e.threshold),
            ("theta0vb0", e.short_channel_factor),
            ("thetaRout", e.output_resistance_factor),
            ("eg0", mat.nominal_gap),
            ("vtm", mat.thermal_voltage),
            ("csdmin", e.substrate_minimum_capacitance),
            ("cbox", e.buried_oxide_capacitance),
            ("rbodyext", t.external_body_resistance),
            ("drainConductance", t.series_conductances[0]),
            ("sourceConductance", t.series_conductances[1]),
            ("csesw", t.substrate_sidewall[1]),
            ("cdesw", t.substrate_sidewall[0]),
        ]);
        if let Some(pd) = g.pd {
            values.extend([
                ("k1eff", e.effective_k1),
                (
                    "oxideRatio",
                    tr.oxide_ratio.ok_or("PD oxide ratio missing")?,
                ),
                (
                    "ahli0",
                    j.high_level_injection.ok_or("PD injection missing")?,
                ),
                ("wdiod", pd.drain_diode_width),
                ("wdios", pd.source_diode_width),
                ("wdiodCV", pd.drain_diode_cv_width),
                ("wdiosCV", pd.source_diode_cv_width),
                ("leffCVb", pd.body_cv_length),
                ("leffCVbg", pd.backgate_cv_length),
            ]);
        }
        while lines.peek().is_some_and(|line| !line.starts_with("case\t")) {
            let line = lines.next().ok_or("missing value")?;
            let (key, value) = line.split_once('\t').ok_or("invalid oracle value")?;
            let expected = value.parse::<f64>()?;
            let actual = values
                .remove(key)
                .ok_or_else(|| format!("unknown coefficient {key}"))?;
            let residual = (actual - expected).abs();
            let relative = residual / expected.abs().max(1e-100);
            max_rel = max_rel.max(relative);
            if !relative.is_finite() || relative >= 2e-10 {
                failures.push(format!(
                    "{family:?} {name} {key}: {actual:e} != {expected:e}, relative {relative:e}"
                ));
            }
            checked += 1;
        }
        assert!(
            values.is_empty(),
            "{family:?} {name}: missing reference {values:?}"
        );
        cases += 1;
    }
    assert_eq!(cases, 18);
    assert_eq!(checked, 1026);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    println!(
        "{cases} native C setup/temp cases, {checked} scalar coefficients, maximum relative deviation {max_rel:e}"
    );
    Ok(())
}

#[test]
fn physical_temperature_rejects_mismatched_preparation() -> Result<(), Box<dyn Error>> {
    use model_compact_intrinsic::bsim3soi::prepare_temperature;
    let family = ModelVariant::Bsim3SoiPd;
    let a = model(family, "", "nmos")?;
    let b = model(family, "", "nmos")?;
    let i = prepare_instance(
        &a,
        &bind(family, ParameterScope::Instance, "l=5e-6 w=3e-6", None)?,
    )?;
    assert!(matches!(
        prepare_temperature(&b, &i, 300.15),
        Err(ModelError::MismatchedPreparedContext)
    ));
    assert!(prepare_temperature(&a.clone(), &i, 300.15).is_ok());
    assert!(prepare_temperature(&a, &i, 0.0).is_err());
    Ok(())
}

#[path = "bsim3soi_setup/substrate.rs"]
mod substrate;

#[path = "bsim3soi_setup/junction.rs"]
mod junction;
