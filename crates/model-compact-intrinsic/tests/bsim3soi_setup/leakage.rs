use super::*;
use model_compact_intrinsic::bsim3soi::{PdCurrent, PhysicalTemperature, prepare_temperature};

const ACTIVE: &str = "igmod=1 tox=3e-9 toxqm=2e-9 ngate=2e20 agidl=1e-8 bgidl=1e8 ngidl=0.1 alpha0=0.02 beta0=0.1 beta1=0.2 beta2=0.5 sii0=0.5 sii1=0.1 sii2=0.2 siid=0.1 fbjtii=0.7 isbjt=2e-3 isdif=1e-3 isrec=2e-3 istun=1e-4 vrec0=0.9 vtun0=0.8 ahli=0.01 dwg=1e-8 dwb=2e-8";
const INSTANCE: &str = "l=1e-6 w=3e-6 nseg=2 nbc=1 pdbcp=1e-7 psbcp=3e-7";

fn prepared(
    card: &str,
    polarity: &str,
    ambient: f64,
) -> Result<PhysicalTemperature, Box<dyn Error>> {
    let family = ModelVariant::Bsim3SoiPd;
    let model = model(family, card, polarity)?;
    let instance = prepare_instance(
        &model,
        &bind(family, ParameterScope::Instance, INSTANCE, None)?,
    )?;
    Ok(prepare_temperature(&model, &instance, ambient)?)
}

fn currents(
    t: &PhysicalTemperature,
    bias: [f64; 4],
    phi: f64,
) -> Result<[PdCurrent; 4], Box<dyn Error>> {
    let channel = t
        .pd_channel
        .as_ref()
        .ok_or("missing PD channel")?
        .evaluate(bias[0], bias[1], bias[2], bias[3], phi)?;
    let junction = t.junction_currents.evaluate(bias[2], bias[1], bias[3])?;
    let channel_current = PdCurrent {
        current_a: channel.current_a,
        gradient: channel.current_gradient,
    };
    let leakage = channel.leakage;
    Ok([
        leakage.drain_gidl,
        leakage.source_gidl,
        leakage.gate_body,
        leakage.impact_ionization(channel_current, junction.collector)?,
    ])
}

#[test]
fn pd_leakage_analytic_derivatives() -> Result<(), Box<dyn Error>> {
    let mut checked = 0;
    for polarity in ["nmos", "pmos"] {
        let card = format!("{ACTIVE} shmod=1 rth0=0.02 tnom=32 tii=0.1");
        let t = prepared(&card, polarity, 300.15)?;
        let phi = t.electrostatics.phi;
        for gate in [-1.2, -0.3, 0.15, 0.8, 1.4, 2.0] {
            for drain in [0.03, 0.3, 1.1] {
                for body in [-0.6, -0.1, 0.3] {
                    for temp in [265.0, 300.15, 350.0] {
                        let bias = [gate, drain, body, temp];
                        let a = currents(&t, bias, phi)?;
                        for (axis, h) in [1e-6, 1e-6, 1e-6, 1e-3].into_iter().enumerate() {
                            let mut lo = bias;
                            let mut hi = bias;
                            lo[axis] -= h;
                            hi[axis] += h;
                            let l = currents(&t, lo, phi)?;
                            let r = currents(&t, hi, phi)?;
                            for (index, (middle, (left, right))) in
                                a.iter().zip(l.into_iter().zip(r)).enumerate()
                            {
                                // At zero drain-body bias, the collector's high-injection
                                // cutoff can lie inside the usual voltage perturbation.
                                // Use a local step for that contribution only.
                                let (left, right, h) = if index == 3
                                    && (axis == 1 || axis == 2)
                                    && (body - drain).abs() < 1e-12
                                {
                                    let h = 1e-8;
                                    let mut lo = bias;
                                    let mut hi = bias;
                                    lo[axis] -= h;
                                    hi[axis] += h;
                                    (
                                        currents(&t, lo, phi)?[index],
                                        currents(&t, hi, phi)?[index],
                                        h,
                                    )
                                } else if index == 2 && axis == 1 {
                                    // Gate tunneling has a very weak drain dependence;
                                    // a larger drain-only step resolves the subtraction.
                                    let h = 1e-4;
                                    let mut lo = bias;
                                    let mut hi = bias;
                                    lo[axis] -= h;
                                    hi[axis] += h;
                                    (
                                        currents(&t, lo, phi)?[index],
                                        currents(&t, hi, phi)?[index],
                                        h,
                                    )
                                } else {
                                    (left, right, h)
                                };
                                let numerical = (right.current_a - left.current_a) / (2.0 * h);
                                let expected = middle.gradient[axis];
                                let roundoff = 16.0
                                    * f64::EPSILON
                                    * left.current_a.abs().max(right.current_a.abs())
                                    / h;
                                assert!(
                                    (numerical - expected).abs()
                                        < roundoff + 1e-18 + 1e-5 * expected.abs(),
                                    "{polarity} {bias:?} axis={axis} component={index}: {numerical:e} != {expected:e}"
                                );
                                checked += 1;
                            }
                        }
                    }
                }
            }
        }
    }
    println!("{checked} PD leakage analytic derivative checks");
    Ok(())
}

#[cfg(target_os = "linux")]
mod native {
    use super::*;
    use std::{fs, process::Command};

    fn compare(polarity: &str, ambient: f64) -> Result<(usize, [f64; 4]), Box<dyn Error>> {
        let t = prepared(ACTIVE, polarity, ambient + 273.15)?;
        let sign = if polarity == "nmos" { 1.0 } else { -1.0 };
        let tmp = tempfile::tempdir()?;
        let deck = format!(
            "BSIM3SOI PD leakage witness\n.temp {ambient}\n.options reltol=1e-9 abstol=1e-19 vntol=1e-11\n.model m {polarity} level=57 {ACTIVE}\nVd d 0 {}\nVg g 0 0\nVb b 0 0\nVe e 0 0\nM1 d g 0 e b 0 b m {INSTANCE} debug=1\n.control\nset num_threads=1\nset numdgt=17\nset wr_vecnames\nset wr_singlescale\nset appendwrite\nsave all @m1[vbs] @m1[vds] @m1[vgs]\nforeach v {} {} {}\nalter Vd $v\nforeach b {} {} {}\nalter Vb $b\ndc Vg {} {} {}\nwrdata witness.tsv @m1[vgs] @m1[vds] @m1[vbs] v(m1#igidl) v(m1#ig) v(m1#iii) vb#branch\nend\nend\nquit\n.endc\n.end\n",
            0.1 * sign,
            0.03 * sign,
            0.3 * sign,
            1.1 * sign,
            -0.6 * sign,
            -0.1 * sign,
            0.3 * sign,
            -1.2 * sign,
            2.0 * sign,
            0.1 * sign
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
        let mut max = [0.0_f64; 4];
        for line in table
            .lines()
            .filter(|line| !line.trim_start().starts_with("v-sweep"))
        {
            let values = line
                .split_whitespace()
                .map(str::parse::<f64>)
                .collect::<Result<Vec<_>, _>>()?;
            let [_, vgs, vds, vbs, gidl, ig, iii, body_current] = values.as_slice() else {
                return Err("bad PD leakage reference row".into());
            };
            let bias = [*vgs, *vds, *vbs, ambient + 273.15];
            let a = currents(&t, bias, t.electrostatics.phi)?;
            let junction = t.junction_currents.evaluate(*vbs, *vds, bias[3])?;
            let normalized_body = a.iter().map(|v| v.current_a).sum::<f64>()
                - junction.source.total.current_a
                - junction.drain.total.current_a;
            let actual = [
                a[0].current_a,
                a[2].current_a,
                a[3].current_a.abs() + 1e-14,
                normalized_body,
            ];
            let expected = [*gidl, *ig, *iii, sign * body_current];
            for (index, (actual, expected)) in actual.into_iter().zip(expected).enumerate() {
                let error = (actual - expected).abs();
                max[index] = max[index].max(error);
                assert!(
                    error < 2e-16 + 1e-7 * expected.abs(),
                    "{polarity} {ambient} {bias:?} index={index}: {actual:e} != {expected:e}"
                );
                n += 1;
            }
        }
        assert_eq!(n, 33 * 9 * 4);
        Ok((n, max))
    }

    #[test]
    #[ignore = "requires independent ngspice 47 executable"]
    fn native_pd_leakage_matrix() -> Result<(), Box<dyn Error>> {
        let mut total = 0;
        let mut max = [0.0_f64; 4];
        for polarity in ["nmos", "pmos"] {
            for ambient in [-10.0, 27.0, 85.0] {
                let (n, m) = compare(polarity, ambient)?;
                total += n;
                for i in 0..4 {
                    max[i] = max[i].max(m[i]);
                }
            }
        }
        println!(
            "{total} native PD leakage comparisons, max deviations [drain GIDL, gate, impact, body KCL] {max:?} A"
        );
        Ok(())
    }
}
