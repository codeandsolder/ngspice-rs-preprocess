use super::*;
use model_compact_intrinsic::bsim3soi::{PreparedPdChannel, prepare_temperature};

const ISOLATED: &str = "isbjt=0 isdif=0 isrec=0 istun=0 alpha0=0 agidl=0 bgidl=0 igmod=0";

fn prepared(
    fields: &str,
    instance: &str,
    polarity: &str,
    ambient: f64,
) -> Result<PreparedPdChannel, Box<dyn Error>> {
    let family = ModelVariant::Bsim3SoiPd;
    let m = model(family, fields, polarity)?;
    let i = prepare_instance(&m, &bind(family, ParameterScope::Instance, instance, None)?)?;
    Ok(prepare_temperature(&m, &i, ambient)?
        .pd_channel
        .ok_or("missing PD channel")?)
}

#[test]
fn pd_channel_analytic_derivatives() -> Result<(), Box<dyn Error>> {
    let mut checked = 0;
    for mode in [1, 2, 3] {
        for polarity in ["nmos", "pmos"] {
            let card = format!(
                "{ISOLATED} mobmod={mode} shmod=1 rth0=0.02 ngate=2e20 a0=0.8 ags=0.1 keta=-0.02 rdsw=150 prwg=0.2 prwb=0.15 dwg=1e-8 dwb=2e-8 a1=0.1 a2=0.8 pdiblcb=0.1 pvag=0.3"
            );
            let j = prepared(&card, "l=1e-6 w=3e-6 nseg=2 nbc=1", polarity, 300.15)?;
            let phi = j.initial_surface_potential();
            for gate in [-0.3, 0.1, 0.5, 0.9, 1.4] {
                for drain in [0.03, 0.3, 1.1] {
                    for body in [-0.6, -0.1, 0.3] {
                        for temp in [265.0, 300.15, 350.0] {
                            let a = j.evaluate(gate, drain, body, temp, phi)?;
                            for (axis, h) in [1e-6, 1e-6, 1e-6, 1e-3].into_iter().enumerate() {
                                let mut lo = [gate, drain, body, temp];
                                let mut hi = lo;
                                lo[axis] -= h;
                                hi[axis] += h;
                                let l = j.evaluate(lo[0], lo[1], lo[2], lo[3], phi)?.current_a;
                                let r = j.evaluate(hi[0], hi[1], hi[2], hi[3], phi)?.current_a;
                                let numerical = (r - l) / (2.0 * h);
                                let expected = a.current_gradient[axis];
                                let roundoff = 16.0 * f64::EPSILON * l.abs().max(r.abs()) / h;
                                assert!(
                                    (numerical - expected).abs()
                                        < roundoff + 2e-17 + 1e-5 * expected.abs(),
                                    "{mode} {polarity} {gate} {drain} {body} {temp} axis={axis}: {numerical:e} != {expected:e}"
                                );
                                checked += 1;
                            }
                        }
                    }
                }
            }
        }
    }
    println!("{checked} PD channel analytic derivative comparisons");
    Ok(())
}

#[cfg(target_os = "linux")]
mod native {
    use super::*;
    use std::{fs, process::Command};

    struct Case<'a> {
        model: &'a str,
        instance: &'a str,
        polarity: &'a str,
        ambient: f64,
    }

    fn compare(case: Case<'_>) -> Result<(usize, [f64; 6]), Box<dyn Error>> {
        let card = format!("{ISOLATED} {}", case.model);
        let j = prepared(&card, case.instance, case.polarity, case.ambient + 273.15)?;
        let phi = j.initial_surface_potential();
        let sign = if case.polarity == "nmos" { 1.0 } else { -1.0 };
        let tmp = tempfile::tempdir()?;
        let deck = format!(
            "BSIM3SOI PD channel witness\n.temp {}\n.options reltol=1e-9 abstol=1e-19 vntol=1e-11\n.model m {} level=57 {card}\nVd d 0 {}\nVg g 0 0\nVb b 0 0\nVe e 0 0\nM1 d g 0 e b 0 b m {} debug=1\n.control\nset num_threads=1\nset numdgt=17\nset wr_vecnames\nset wr_singlescale\nset appendwrite\nsave all @m1[vbs] @m1[vds] @m1[vgs] @m1[gm] @m1[gds] @m1[gmbs] @m1[vth] @m1[vdsat]\nforeach v {} {} {}\nalter Vd $v\nforeach b {} {} {}\nalter Vb $b\ndc Vg {} {} {}\nwrdata witness.tsv @m1[vgs] @m1[vds] @m1[vbs] v(m1#ids) @m1[gm] @m1[gds] @m1[gmbs] @m1[vth] @m1[vdsat]\nend\nend\nquit\n.endc\n.end\n",
            case.ambient,
            case.polarity,
            0.1 * sign,
            case.instance,
            0.03 * sign,
            0.3 * sign,
            1.1 * sign,
            -0.6 * sign,
            -0.1 * sign,
            0.3 * sign,
            -0.3 * sign,
            1.5 * sign,
            0.05 * sign
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
        let mut n = 0;
        let mut max = [0.0_f64; 6];
        for line in table
            .lines()
            .filter(|line| !line.trim_start().starts_with("v-sweep"))
        {
            let values = line
                .split_whitespace()
                .map(str::parse::<f64>)
                .collect::<Result<Vec<_>, _>>()?;
            let [_, vgs, vds, vbs, ids, gm, gds, gmb, vth, vdsat] = values.as_slice() else {
                return Err("bad PD channel reference row".into());
            };
            let a = j.evaluate(*vgs, *vds, *vbs, case.ambient + 273.15, phi)?;
            let actual = [
                a.current_a.abs() + 1e-14,
                a.current_gradient[0],
                a.current_gradient[1],
                a.current_gradient[2],
                a.threshold_v,
                a.saturation_voltage_v,
            ];
            let expected = [*ids, *gm, *gds, *gmb, *vth, *vdsat];
            for (index, (actual, expected)) in actual.into_iter().zip(expected).enumerate() {
                let error = (actual - expected).abs();
                max[index] = max[index].max(error);
                let absolute = if index < 4 { 2e-16 } else { 2e-11 };
                assert!(
                    error < absolute + 2e-6 * expected.abs(),
                    "{} {} T={} VGS={vgs} VDS={vds} VBS={vbs} index={index}: {actual:e} != {expected:e}",
                    case.model,
                    case.polarity,
                    case.ambient
                );
                n += 1;
            }
        }
        assert_eq!(n, 37 * 9 * 6);
        Ok((n, max))
    }

    #[test]
    #[ignore = "requires independent ngspice 47 executable"]
    fn native_pd_channel_matrix() -> Result<(), Box<dyn Error>> {
        let mut total = 0;
        let mut max = [0.0_f64; 6];
        for (model, instance, ambient) in [
            ("mobmod=1", "l=1e-6 w=3e-6", 27.0),
            (
                "mobmod=2 ngate=2e20 rdsw=150 prwg=0.2 prwb=0.15 a1=0.1 a2=0.8 dwg=1e-8 dwb=2e-8",
                "l=0.7e-6 w=2e-6 nseg=2 nbc=1",
                85.0,
            ),
            (
                "mobmod=3 a0=0.8 ags=0.1 keta=-0.02 k1w1=2e-7 k1w2=1e-7 a1=-0.1 a2=0.8 pdiblcb=0.1 pvag=0.3",
                "l=0.8e-6 w=4e-6",
                -10.0,
            ),
        ] {
            for polarity in ["nmos", "pmos"] {
                let (n, m) = compare(Case {
                    model,
                    instance,
                    polarity,
                    ambient,
                })?;
                total += n;
                for i in 0..6 {
                    max[i] = max[i].max(m[i]);
                }
            }
        }
        println!(
            "{total} native PD channel comparisons; max errors [Ids, gm, gds, gmb, Vth, Vdsat] {max:?}"
        );
        Ok(())
    }
}
