use super::*;
use model_compact_intrinsic::bsim3soi::{
    DepletedBias, DepletedChannelEvaluation, DepletedValue, PreparedDepletedChannel,
    prepare_temperature,
};

const ISOLATED: &str = "isbjt=0 isdif=0 isrec=0 istun=0 agidl=0 bgidl=0 aii=0";

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

fn observables(p: DepletedChannelEvaluation) -> [DepletedValue; 17] {
    let e = p.electrostatics;
    [
        e.gate_effective,
        e.depleted_threshold,
        e.equilibrium_body,
        e.diode_body,
        e.effective_body,
        e.body_contact,
        e.coupling,
        e.threshold,
        e.overdrive,
        e.depletion_depth,
        p.current,
        p.width,
        p.mobility,
        p.saturation_voltage,
        p.effective_drain,
        p.resistance,
        p.bulk_factor,
    ]
}

#[test]
fn depleted_channel_derivatives() -> Result<(), Box<dyn Error>> {
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
                                        (point.electrostatics.diode_body.value
                                            - point.electrostatics.equilibrium_body.value)
                                            .abs()
                                            < 1e-15
                                    );
                                    assert!(point.electrostatics.coupling.value.abs() < 1e-15);
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
                                        let absolute = match index {
                                            9 | 11 => 1e-14,
                                            10 => 1e-15,
                                            12 => 1e-9,
                                            15 => 1e-5,
                                            _ => 1e-8,
                                        };
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
    println!("{comparisons} FD/DD electrostatic/transport derivative comparisons");
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

    fn compare(case: Case<'_>) -> Result<(usize, [f64; 11]), Box<dyn Error>> {
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
        // Independent native scalar witnesses at +/- h on each electrical axis.
        // The old DD hand Jacobian has discrepancies, so the current law,
        // not its separately maintained derivative, is the oracle.
        let h = 1e-4;
        let mut witnesses = String::new();
        let mut columns = String::new();
        for (axis, node) in ["g", "d", "b", "e"].into_iter().enumerate() {
            for (side, delta) in [-2.0 * h, -h, h, 2.0 * h].into_iter().enumerate() {
                use std::fmt::Write as _;
                let id = 2 + axis * 4 + side;
                let shifted = format!("p{id}");
                writeln!(witnesses, "Vp{id} {shifted} {node} {}", delta * sign)?;
                let nodes = ["d", "g", "0", "e", "b"]
                    .map(|n| if n == node { shifted.as_str() } else { n })
                    .join(" ");
                writeln!(witnesses, "M{id} {nodes} m {} debug=2", case.instance)?;
                write!(columns, " v(m{id}#ids)")?;
            }
        }
        let deck = format!(
            "FD/DD electrostatic witness\n.temp {}\n.options reltol=1e-9 abstol=1e-19 vntol=1e-11\n.model m {} level={} {card}\nVd d 0 {}\nVg g 0 0\nVb b 0 0\nVe e 0 0\nM1 d g 0 e b m {} debug=2\n{witnesses}.control\nset num_threads=1\nset numdgt=17\nset wr_vecnames\nset wr_singlescale\nset appendwrite\nsave all @m1[vth] @m1[vdsat]\nforeach d {} {} {}\nalter Vd $d\nforeach e {} 0 {}\nalter Ve $e\nforeach b {} {} {}\nalter Vb $b\ndc Vg {} {} {}\nwrdata witness.tsv v(d) v(g) v(b) v(e) v(m1#vbs0eff) v(m1#vbs) v(m1#vbseff) @m1[vth] v(m1#abeff) v(m1#ids) @m1[vdsat] v(m1#gm) v(m1#gds) v(m1#gmbs) v(m1#gme){columns}\nend\nend\nend\nquit\n.endc\n.end\n",
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
        let mut hand_error = [0.0_f64; 4];
        let mut max = [0.0_f64; 11];
        for row in table
            .lines()
            .filter(|line| !line.trim_start().starts_with("v-sweep"))
        {
            let row = row
                .split_whitespace()
                .map(str::parse::<f64>)
                .collect::<Result<Vec<_>, _>>()?;
            let [
                _,
                vd,
                vg,
                vb,
                ve,
                eq,
                diode,
                effective,
                vth,
                bulk,
                ids,
                vdsat,
                gm,
                gds,
                gmbs,
                gme,
                differences @ ..,
            ] = row.as_slice()
            else {
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
                a.electrostatics.equilibrium_body.value,
                a.electrostatics.diode_body.value,
                a.electrostatics.effective_body.value,
                a.electrostatics.threshold.value,
                a.bulk_factor.value,
                a.current.value,
                a.saturation_voltage.value,
                a.current.gradient[0],
                a.current.gradient[1],
                a.current.gradient[2],
                a.current.gradient[3],
            ];
            let mut expected = [
                *eq, *diode, *effective, *vth, *bulk, *ids, *vdsat, *gm, *gds, *gmbs, *gme,
            ];
            assert_eq!(differences.len(), 16);
            for (axis, pair) in differences.chunks_exact(4).enumerate() {
                expected[7 + axis] = (8.0 * (pair[2] - pair[1]) - (pair[3] - pair[0])) / (12.0 * h);
            }
            for (axis, hand) in [*gm, *gds, *gmbs, *gme].into_iter().enumerate() {
                hand_error[axis] = hand_error[axis].max((hand - a.current.gradient[axis]).abs());
            }
            for (index, (a, e)) in actual.into_iter().zip(expected).enumerate() {
                let error = (a - e).abs();
                max[index] = max[index].max(error);
                assert!(
                    error
                        < if index == 5 || index >= 7 {
                            if index >= 7 {
                                let roundoff = differences[(index - 7) * 4..(index - 6) * 4]
                                    .iter()
                                    .map(|v| v.abs())
                                    .fold(0.0_f64, f64::max)
                                    * 64.0
                                    * f64::EPSILON
                                    / h;
                                2e-16 + roundoff + 2e-6 * e.abs()
                            } else {
                                2e-16 + 1e-7 * e.abs()
                            }
                        } else {
                            3e-10 + 1e-8 * e.abs()
                        },
                    "{:?} {} T={} {b:?} field={index}: {a:e} != {e:e}",
                    case.family,
                    case.polarity,
                    case.ambient
                );
                count += 1;
            }
        }
        println!(
            "{:?} {} T={} native hand Jacobian max errors {hand_error:?}",
            case.family, case.polarity, case.ambient
        );
        assert_eq!(count, 13 * 27 * 11);
        Ok((count, max))
    }

    #[test]
    #[ignore = "requires independent ngspice 47 executable"]
    fn native_depleted_channel() -> Result<(), Box<dyn Error>> {
        let mut total = 0;
        let mut max = [0.0_f64; 11];
        for (family, level) in [
            (ModelVariant::Bsim3SoiFd, 55),
            (ModelVariant::Bsim3SoiDd, 56),
        ] {
            for polarity in ["nmos", "pmos"] {
                for (model, instance, ambient) in [
                    ("mobmod=1", "l=1e-6 w=3e-6", 27.0),
                    (
                        "mobmod=2 rdsw=150 prwg=0.2 prwb=0.15 dwg=1e-8 dwb=2e-8 a1=0.1 a2=0.8 ngate=2e20 tsi=8e-8 tbox=2e-7 vbsa=0.04 kb1=0.85 kb3=1.2 dvbd0=0.08 dvbd1=0.3 cdscb=3e-5 cdscd=1e-5 kt2=-0.01",
                        "l=0.7e-6 w=2e-6",
                        85.0,
                    ),
                    (
                        "mobmod=3 a0=0 a1=-0.1 a2=0.8 pdiblcb=0.1 pvag=0.3 nsub=-6e16 nch=2e17 k1=0.5 k2=-0.0186 tsi=1.2e-7 tbox=1e-7 kb1=0.95 kb3=0.7 tnom=32 lkb1=0.04 wkb1=-0.03 lvbsa=0.02",
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
                    for i in 0..11 {
                        max[i] = max[i].max(m[i]);
                    }
                }
            }
        }
        println!(
            "{total} native FD/DD channel comparisons; max [Vbs0eff,Vbsdio,Vbseff,Vth,Abeff,Ids,Vdsat,dIds/dVgs,dIds/dVds,dIds/dVbs,dIds/dVes] {max:?}"
        );
        Ok(())
    }
}
