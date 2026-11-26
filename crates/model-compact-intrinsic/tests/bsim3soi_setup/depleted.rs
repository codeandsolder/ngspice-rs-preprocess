use super::*;
use model_compact_intrinsic::bsim3soi::{
    DepletedBias, DepletedElectrostaticPoint, DepletedValue, PreparedDepletedElectrostatics,
    prepare_temperature,
};

const ISOLATED: &str = "isbjt=0 isdif=0 isrec=0 istun=0 agidl=0 bgidl=0 aii=0";

fn prepared(
    family: ModelVariant,
    card: &str,
    instance: &str,
    polarity: &str,
    ambient: f64,
) -> Result<PreparedDepletedElectrostatics, Box<dyn Error>> {
    let m = model(family, card, polarity)?;
    let i = prepare_instance(&m, &bind(family, ParameterScope::Instance, instance, None)?)?;
    Ok(prepare_temperature(&m, &i, ambient)?
        .depleted_electrostatics
        .ok_or("depleted electrostatics missing")?)
}

fn from_array(b: [f64; 6]) -> DepletedBias {
    DepletedBias {
        vgs: b[0],
        vds: b[1],
        vbs: b[2],
        ves: b[3],
        vps: b[4],
        kelvin: b[5],
    }
}

fn observables(p: DepletedElectrostaticPoint) -> [DepletedValue; 10] {
    [
        p.gate_effective,
        p.depleted_threshold,
        p.equilibrium_body,
        p.diode_body,
        p.effective_body,
        p.body_contact,
        p.coupling,
        p.threshold,
        p.overdrive,
        p.depletion_depth,
    ]
}

#[test]
fn depleted_electrostatic_derivatives() -> Result<(), Box<dyn Error>> {
    let mut comparisons = 0;
    let card = format!(
        "{ISOLATED} shmod=1 rth0=0.02 tnom=32 ngate=2e20 vbsa=0.04 kb1=0.85 kb3=1.2 dvbd0=0.08 dvbd1=0.3 cdscb=3e-5 cdscd=1e-5 kt2=-0.01"
    );
    for family in [ModelVariant::Bsim3SoiFd, ModelVariant::Bsim3SoiDd] {
        for polarity in ["nmos", "pmos"] {
            let p = prepared(family, &card, "l=0.7e-6 w=2e-6", polarity, 300.15)?;
            let phi = p.initial_surface_potential();
            for gate in [-0.3, 0.4, 0.9, 1.5] {
                for drain in [0.03, 0.3, 1.1] {
                    for body in [-0.6, -0.1, 0.3] {
                        for backgate in [-2.0, 0.2, 2.0] {
                            for temp in [270.0, 300.15, 350.0] {
                                let b = [gate, drain, body, backgate, 0.1, temp];
                                let point = p.evaluate(from_array(b), phi)?;
                                if family == ModelVariant::Bsim3SoiFd {
                                    assert!(
                                        (point.diode_body.value - point.equilibrium_body.value)
                                            .abs()
                                            < 1e-15
                                    );
                                    assert!(point.coupling.value.abs() < 1e-15);
                                }
                                let middle = observables(point);
                                for (axis, h) in
                                    [1e-5, 1e-5, 1e-5, 1e-5, 1e-5, 1e-3].into_iter().enumerate()
                                {
                                    let mut lo = b;
                                    let mut hi = b;
                                    lo[axis] -= h;
                                    hi[axis] += h;
                                    let left = observables(p.evaluate(from_array(lo), phi)?);
                                    let right = observables(p.evaluate(from_array(hi), phi)?);
                                    for (index, (l, r)) in left.into_iter().zip(right).enumerate() {
                                        let expected = middle[index].gradient[axis];
                                        let numerical = (r.value - l.value) / (2.0 * h);
                                        let roundoff =
                                            32.0 * f64::EPSILON * l.value.abs().max(r.value.abs())
                                                / h;
                                        let absolute = if index == 9 { 1e-14 } else { 1e-8 };
                                        assert!(
                                            (numerical - expected).abs()
                                                < absolute + roundoff + 1e-5 * expected.abs(),
                                            "{family:?} {polarity} {b:?} axis={axis} field={index}: {numerical:e} != {expected:e}"
                                        );
                                        comparisons += 1;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    println!("{comparisons} FD/DD electrostatic derivative comparisons");
    Ok(())
}

#[cfg(target_os = "linux")]
mod native {
    use super::*;
    use std::{fs, process::Command};

    struct Case<'a> {
        family: ModelVariant,
        level: i32,
        polarity: &'a str,
        model: &'a str,
        instance: &'a str,
        ambient: f64,
    }

    fn compare(case: Case<'_>) -> Result<(usize, [f64; 4]), Box<dyn Error>> {
        let card = format!("{ISOLATED} {}", case.model);
        let p = prepared(
            case.family,
            &card,
            case.instance,
            case.polarity,
            case.ambient + 273.15,
        )?;
        let phi = p.initial_surface_potential();
        let sign = if case.polarity == "nmos" { 1.0 } else { -1.0 };
        let tmp = tempfile::tempdir()?;
        let deck = format!(
            "FD/DD electrostatic witness\n.temp {}\n.options reltol=1e-9 abstol=1e-19 vntol=1e-11\n.model m {} level={} {card}\nVd d 0 {}\nVg g 0 0\nVb b 0 0\nVe e 0 0\nM1 d g 0 e b m {} debug=2\n.control\nset num_threads=1\nset numdgt=17\nset wr_vecnames\nset wr_singlescale\nset appendwrite\nsave all @m1[vth]\nforeach d {} {} {}\nalter Vd $d\nforeach e {} 0 {}\nalter Ve $e\nforeach b {} {} {}\nalter Vb $b\ndc Vg {} {} {}\nwrdata witness.tsv v(d) v(g) v(b) v(e) v(m1#vbs0eff) v(m1#vbs) v(m1#vbseff) @m1[vth]\nend\nend\nend\nquit\n.endc\n.end\n",
            case.ambient,
            case.polarity,
            case.level,
            0.3 * sign,
            case.instance,
            0.03 * sign,
            0.3 * sign,
            1.1 * sign,
            -2.0 * sign,
            2.0 * sign,
            -0.6 * sign,
            -0.1 * sign,
            0.3 * sign,
            -0.3 * sign,
            1.5 * sign,
            0.15 * sign
        );
        fs::write(tmp.path().join("witness.cir"), &deck)?;
        let output = Command::new("ngspice")
            .args(["-b", "-n", "witness.cir"])
            .current_dir(tmp.path())
            .output()?;
        assert!(
            output.status.success(),
            "{deck}\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let table = fs::read_to_string(tmp.path().join("witness.tsv"))?;
        let mut count = 0;
        let mut max = [0.0_f64; 4];
        for row in table
            .lines()
            .filter(|line| !line.trim_start().starts_with("v-sweep"))
        {
            let row = row
                .split_whitespace()
                .map(str::parse::<f64>)
                .collect::<Result<Vec<_>, _>>()?;
            let [_, vd, vg, vb, ve, eq, diode, effective, vth] = row.as_slice() else {
                return Err("bad native depleted electrostatic row".into());
            };
            let b = DepletedBias {
                vgs: sign * vg,
                vds: sign * vd,
                vbs: sign * vb,
                ves: sign * ve,
                vps: sign * vb,
                kelvin: case.ambient + 273.15,
            };
            let a = p.evaluate(b, phi)?;
            let actual = [
                a.equilibrium_body.value,
                a.diode_body.value,
                a.effective_body.value,
                a.threshold.value,
            ];
            let expected = [*eq, *diode, *effective, *vth];
            for (index, (a, e)) in actual.into_iter().zip(expected).enumerate() {
                let error = (a - e).abs();
                max[index] = max[index].max(error);
                assert!(
                    error < 3e-10 + 1e-8 * e.abs(),
                    "{:?} {} T={} {b:?} field={index}: {a:e} != {e:e}",
                    case.family,
                    case.polarity,
                    case.ambient
                );
                count += 1;
            }
        }
        assert_eq!(count, 13 * 27 * 4);
        Ok((count, max))
    }

    #[test]
    #[ignore = "requires independent ngspice 47 executable"]
    fn native_depleted_electrostatics() -> Result<(), Box<dyn Error>> {
        let mut total = 0;
        let mut max = [0.0_f64; 4];
        for (family, level) in [
            (ModelVariant::Bsim3SoiFd, 55),
            (ModelVariant::Bsim3SoiDd, 56),
        ] {
            for polarity in ["nmos", "pmos"] {
                for (model, instance, ambient) in [
                    ("", "l=1e-6 w=3e-6", 27.0),
                    (
                        "ngate=2e20 tsi=8e-8 tbox=2e-7 vbsa=0.04 kb1=0.85 kb3=1.2 dvbd0=0.08 dvbd1=0.3 cdscb=3e-5 cdscd=1e-5 kt2=-0.01",
                        "l=0.7e-6 w=2e-6",
                        85.0,
                    ),
                    (
                        "nsub=-6e16 nch=2e17 k1=0.5 k2=-0.0186 tsi=1.2e-7 tbox=1e-7 kb1=0.95 kb3=0.7 tnom=32 lkb1=0.04 wkb1=-0.03 lvbsa=0.02",
                        "l=0.8e-6 w=4e-6",
                        -10.0,
                    ),
                ] {
                    let (n, m) = compare(Case {
                        family,
                        level,
                        polarity,
                        model,
                        instance,
                        ambient,
                    })?;
                    total += n;
                    for i in 0..4 {
                        max[i] = max[i].max(m[i]);
                    }
                }
            }
        }
        println!(
            "{total} native FD/DD electrostatic comparisons; max [Vbs0eff,Vbsdio,Vbseff,Vth] {max:?} V"
        );
        Ok(())
    }
}
