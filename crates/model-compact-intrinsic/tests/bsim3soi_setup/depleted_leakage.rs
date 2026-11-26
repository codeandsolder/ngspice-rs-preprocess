use super::*;
use model_compact_intrinsic::bsim3soi::{
    DepletedBias, DepletedValue, PreparedDepletedChannel, prepare_temperature,
};

const ACTIVE: &str = "tox=4e-9 ngate=2e20 alpha0=1e-8 alpha1=0.2 beta0=0.1 aii=0.5 bii=1e-7 cii=0.2 dii=0.1 agidl=1e-8 bgidl=1e8 ngidl=0.1 isbjt=0 isdif=0 isrec=0 istun=0";
const INSTANCE: &str = "l=1e-6 w=3e-6";

fn prepared(
    family: ModelVariant,
    card: &str,
    instance: &str,
    polarity: &str,
    ambient: f64,
) -> Result<PreparedDepletedChannel, Box<dyn Error>> {
    let m = model(family, card, polarity)?;
    let i = prepare_instance(&m, &bind(family, ParameterScope::Instance, instance, None)?)?;
    Ok(prepare_temperature(&m, &i, ambient)?
        .depleted_channel
        .ok_or("missing depleted channel")?)
}

fn bias(b: [f64; 6]) -> DepletedBias {
    DepletedBias {
        vgs: b[0],
        vds: b[1],
        vbs: b[2],
        ves: b[3],
        vps: b[4],
        kelvin: b[5],
    }
}

fn values(p: &PreparedDepletedChannel, b: [f64; 6]) -> Result<[DepletedValue; 4], Box<dyn Error>> {
    let q = p.evaluate(bias(b), p.initial_surface_potential())?.leakage;
    Ok([q.impact, q.drain_gidl, q.source_gidl, q.body_contact])
}

#[test]
fn depleted_leakage_derivatives() -> Result<(), Box<dyn Error>> {
    let mut checked = 0;
    for family in [ModelVariant::Bsim3SoiFd, ModelVariant::Bsim3SoiDd] {
        for polarity in ["nmos", "pmos"] {
            for extra in [
                "aii=0",
                "rbody=10 rbsh=200 shmod=1 rth0=0.02 tnom=32",
                "rbody=0 rbsh=200",
            ] {
                let p = prepared(
                    family,
                    &format!("{ACTIVE} {extra}"),
                    "l=1e-6 w=3e-6 nrb=2",
                    polarity,
                    300.15,
                )?;
                for vg in [-0.8, 0.4, 1.5, 2.0] {
                    for vd in [0.03, 0.4, 1.5] {
                        for (vb, vp) in [(-0.4, -0.8), (-0.1, 0.3), (0.2, -0.3)] {
                            for ve in [-1.0, 0.5] {
                                let b = [vg, vd, vb, ve, vp, 335.0];
                                let mid = values(&p, b)?;
                                if family == ModelVariant::Bsim3SoiFd {
                                    assert!(
                                        mid.iter()
                                            .all(|v| v.value == 0.0 && v.gradient == [0.0; 6])
                                    );
                                }
                                for (axis, h) in
                                    [1e-6, 1e-6, 1e-6, 1e-6, 1e-6, 1e-3].into_iter().enumerate()
                                {
                                    let mut lo = b;
                                    let mut hi = b;
                                    lo[axis] -= h;
                                    hi[axis] += h;
                                    for (index, (m, (l, r))) in mid
                                        .iter()
                                        .zip(values(&p, lo)?.into_iter().zip(values(&p, hi)?))
                                        .enumerate()
                                    {
                                        let numerical = (r.value - l.value) / (2.0 * h);
                                        let roundoff =
                                            32.0 * f64::EPSILON * l.value.abs().max(r.value.abs())
                                                / h;
                                        assert!(
                                            (numerical - m.gradient[axis]).abs()
                                                < 2e-15 + roundoff + 2e-5 * m.gradient[axis].abs(),
                                            "{family:?} {polarity} {extra} {b:?} {axis} {index}: {numerical:e} != {:e}",
                                            m.gradient[axis]
                                        );
                                        checked += 1;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    println!("{checked} FD/DD leakage and contact derivative comparisons");
    Ok(())
}

#[cfg(target_os = "linux")]
mod native {
    use super::*;
    use std::{fs, process::Command};

    #[test]
    #[ignore = "requires independent ngspice 47 executable"]
    fn native_depleted_leakage() -> Result<(), Box<dyn Error>> {
        let mut count = 0;
        let mut maximum = [0.0_f64; 4];
        for family in [ModelVariant::Bsim3SoiFd, ModelVariant::Bsim3SoiDd] {
            for polarity in ["nmos", "pmos"] {
                let sign = if polarity == "nmos" { 1.0 } else { -1.0 };
                let level = if family == ModelVariant::Bsim3SoiFd {
                    55
                } else {
                    56
                };
                for ambient in [-10.0, 85.0] {
                    for extra in ["aii=0", ""] {
                        let card = format!("{ACTIVE} {extra}");
                        let p = prepared(family, &card, INSTANCE, polarity, ambient + 273.15)?;
                        let tmp = tempfile::tempdir()?;
                        let deck = format!(
                            "FD/DD leakage witness\n.temp {ambient}\n.options reltol=1e-10 abstol=1e-20 vntol=1e-12 gmin=1e-18 bypass=0\n.model m {polarity} level={level} {card}\nVd d 0 {}\nVg g 0 0\nVb b 0 0\nVe e 0 0\nM1 d g 0 e b m {INSTANCE} debug=2\n.control\nset num_threads=1\nset numdgt=17\nset wr_vecnames\nset wr_singlescale\nset appendwrite\nsave all\nforeach d {} {} {}\nalter Vd $d\nforeach e {} 0 {}\nalter Ve $e\nforeach b {} {} {}\nalter Vb $b\ndc Vg {} {} {}\nwrdata witness.tsv v(d) v(g) v(b) v(e) v(m1#iii) v(m1#igidl) i(Vb) v(m1#ibp)\nend\nend\nend\nquit\n.endc\n.end\n",
                            0.3 * sign,
                            0.05 * sign,
                            0.4 * sign,
                            1.5 * sign,
                            -1.0 * sign,
                            1.0 * sign,
                            -0.6 * sign,
                            -0.1 * sign,
                            0.2 * sign,
                            -1.0 * sign,
                            2.0 * sign,
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
                        for line in table
                            .lines()
                            .filter(|l| !l.trim_start().starts_with("v-sweep"))
                        {
                            let r = line
                                .split_whitespace()
                                .map(str::parse::<f64>)
                                .collect::<Result<Vec<_>, _>>()?;
                            let [_, d, g, b, e, impact, gidl, body, contact] = r.as_slice() else {
                                return Err("bad native row".into());
                            };
                            let a = values(
                                &p,
                                [
                                    sign * g,
                                    sign * d,
                                    sign * b,
                                    sign * e,
                                    sign * b,
                                    ambient + 273.15,
                                ],
                            )?;
                            let expected = [*impact, *gidl, sign * body - impact - gidl, *contact];
                            for (index, (a, expected)) in a.into_iter().zip(expected).enumerate() {
                                let err = (a.value - expected).abs();
                                maximum[index] = maximum[index].max(err);
                                assert!(
                                    err < 5e-17 + 1e-7 * expected.abs(),
                                    "{family:?} {polarity} {ambient} {extra} {g} {d} {b} {e} index={index}: {:e} != {expected:e}",
                                    a.value
                                );
                                count += 1;
                            }
                        }
                    }
                }
            }
        }
        println!("{count} native FD/DD leakage comparisons, max {maximum:?}");
        Ok(())
    }
    #[test]
    #[ignore = "requires independent ngspice 47 executable"]
    fn native_depleted_resistive_contact() -> Result<(), Box<dyn Error>> {
        let mut maximum = 0.0_f64;
        let mut count = 0;
        for polarity in ["nmos", "pmos"] {
            let sign = if polarity == "nmos" { 1.0 } else { -1.0 };
            for resistance in ["rbody=10 rbsh=200", "rbody=0 rbsh=200", "rbody=10 rbsh=0"] {
                let card = format!("{ACTIVE} {resistance} isdif=1e-3 isrec=1e-3");
                let instance = "l=1e-6 w=3e-6 nrb=2";
                let p = prepared(ModelVariant::Bsim3SoiDd, &card, instance, polarity, 300.15)?;
                for (d, g, e, contact) in [
                    (0.4, 1.0, -0.5, -0.1),
                    (1.5, 1.5, 0.0, -0.1),
                    (0.4, 1.5, 0.5, 0.15),
                    (1.5, 1.0, 0.0, 0.15),
                ] {
                    let tmp = tempfile::tempdir()?;
                    let deck = format!(
                        "DD independent body contact witness\n.temp 27\n.options reltol=1e-10 abstol=1e-20 vntol=1e-12 gmin=1e-18\n.model m {polarity} level=56 {card}\nVd d 0 {}\nVg g 0 {}\nVb b 0 {}\nVe e 0 {}\nM1 d g 0 e b m {instance} debug=2\n.control\nset num_threads=1\nset numdgt=17\nset wr_vecnames\nset wr_singlescale\nop\nwrdata witness.tsv v(m1#body) v(m1#ibp)\nquit\n.endc\n.end\n",
                        d * sign,
                        g * sign,
                        contact * sign,
                        e * sign
                    );
                    fs::write(tmp.path().join("witness.cir"), &deck)?;
                    let out = Command::new("ngspice")
                        .args(["-b", "-n", "witness.cir"])
                        .current_dir(tmp.path())
                        .output()?;
                    assert!(
                        out.status.success(),
                        "{deck}\n{}\n{}",
                        String::from_utf8_lossy(&out.stdout),
                        String::from_utf8_lossy(&out.stderr)
                    );
                    let table = fs::read_to_string(tmp.path().join("witness.tsv"))?;
                    let row = table
                        .lines()
                        .nth(1)
                        .ok_or("no contact result")?
                        .split_whitespace()
                        .map(str::parse::<f64>)
                        .collect::<Result<Vec<_>, _>>()?;
                    let [_, body, ibp] = row.as_slice() else {
                        return Err("bad contact row".into());
                    };
                    let actual = values(&p, [g, d, sign * body, e, contact, 300.15])?[3].value;
                    let err = (actual - ibp).abs();
                    maximum = maximum.max(err);
                    assert!(
                        err < 5e-17 + 1e-7 * ibp.abs(),
                        "{polarity} {resistance}: d={d} g={g} b={body} e={e} contact={contact}: {actual:e} != {ibp:e}"
                    );
                    count += 1;
                }
            }
        }
        assert_eq!(count, 24);
        println!("{count} native DD resistive body-contact comparisons, max {maximum:e} A");
        Ok(())
    }
}
