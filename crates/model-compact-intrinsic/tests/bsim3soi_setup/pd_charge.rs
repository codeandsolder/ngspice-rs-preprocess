use super::*;
use model_compact_intrinsic::bsim3soi::{PdCharge, PreparedPdChannel, prepare_temperature};

const ISOLATED: &str = "isbjt=0 isdif=0 isrec=0 istun=0 agidl=0 bgidl=0 alpha0=0 cgso=0 cgdo=0 cgsl=0 cgdl=0 cgeo=0 cf=0 cjswg=0 csdmin=0";
const INSTANCE: &str = "l=0.8e-6 w=3e-6 nseg=2 nbc=1 agbcp=0.1e-12 aebcp=0.2e-12";
const CV: &str = "delvt=0.04 dtoxcv=1e-9 ngate=2e20 acde=1.2 moin=18 fbody=0.9";

fn prepared(card: &str, polarity: &str) -> Result<PreparedPdChannel, Box<dyn Error>> {
    let family = ModelVariant::Bsim3SoiPd;
    let m = model(family, card, polarity)?;
    let i = prepare_instance(&m, &bind(family, ParameterScope::Instance, INSTANCE, None)?)?;
    Ok(prepare_temperature(&m, &i, 300.15)?
        .pd_channel
        .ok_or("missing PD evaluator")?)
}
fn evaluate(p: &PreparedPdChannel, b: [f64; 5], phi: f64) -> Result<[PdCharge; 5], ModelError> {
    let q = p.evaluate_charge(b[0], b[1], b[2], b[3], b[4], phi)?;
    Ok([q.gate, q.drain, q.source, q.body, q.backgate])
}

#[test]
fn pd_intrinsic_charge_conservation_and_derivatives() -> Result<(), Box<dyn Error>> {
    let mut count = 0;
    for polarity in ["nmos", "pmos"] {
        for mode in [2, 3] {
            for partition in [0.0, 0.5, 1.0] {
                let card = format!(
                    "{ISOLATED} {CV} capmod={mode} xpart={partition} shmod=1 rth0=0.01 tnom=32 nsub=6e16"
                );
                let p = prepared(&card, polarity)?;
                let phi = p.initial_surface_potential();
                for gate in [-0.8, 0.1, 0.7, 1.5, 2.5] {
                    for drain in [0.01, 0.3, 1.2] {
                        for body in [-0.6, 0.2] {
                            for temp in [270.0, 350.0] {
                                let b = [gate, drain, body, 0.4, temp];
                                let middle = evaluate(&p, b, phi)?;
                                let scale = middle.iter().map(|q| q.charge_c.abs()).sum::<f64>();
                                assert!(
                                    middle.iter().map(|q| q.charge_c).sum::<f64>().abs()
                                        < 1e-29 + scale * 1e-14
                                );
                                for (axis, h) in
                                    [1e-5, 1e-5, 1e-5, 1e-5, 1e-3].into_iter().enumerate()
                                {
                                    let mut lo = b;
                                    let mut hi = b;
                                    lo[axis] -= h;
                                    hi[axis] += h;
                                    let left = evaluate(&p, lo, phi)?;
                                    let right = evaluate(&p, hi, phi)?;
                                    let sum = middle.iter().map(|q| q.gradient[axis]).sum::<f64>();
                                    let mag =
                                        middle.iter().map(|q| q.gradient[axis].abs()).sum::<f64>();
                                    assert!(sum.abs() < 1e-29 + mag * 1e-14);
                                    for k in 0..5 {
                                        let numerical =
                                            (right[k].charge_c - left[k].charge_c) / (2.0 * h);
                                        let exact = middle[k].gradient[axis];
                                        let roundoff = 64.0
                                            * f64::EPSILON
                                            * right[k].charge_c.abs().max(left[k].charge_c.abs())
                                            / h;
                                        assert!(
                                            (numerical - exact).abs()
                                                < 2e-23 + roundoff + 1e-5 * exact.abs(),
                                            "{polarity} capmod={mode} xpart={partition} {b:?} axis={axis} terminal={k}: {numerical:e} != {exact:e}"
                                        );
                                        count += 1;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    println!("{count} PD charge derivative and conservation checks");
    Ok(())
}

#[cfg(target_os = "linux")]
mod native {
    use super::*;
    use std::{fmt::Write as _, fs, process::Command};

    #[test]
    #[ignore = "requires independent ngspice 47 executable"]
    fn native_pd_intrinsic_charge_ramps() -> Result<(), Box<dyn Error>> {
        let mut max = 0.0_f64;
        let mut comparisons = 0;
        for polarity in ["nmos", "pmos"] {
            let sign = if polarity == "nmos" { 1.0 } else { -1.0 };
            for mode in [2, 3] {
                for partition in [0.0, 0.5, 1.0] {
                    let card = format!("{ISOLATED} {CV} capmod={mode} xpart={partition}");
                    let p = prepared(&card, polarity)?;
                    let phi = p.initial_surface_potential();
                    for (axis, first, last) in [
                        (0, -0.5, 1.8),
                        (1, 0.01, 1.2),
                        (2, -0.6, 0.3),
                        (3, -1.0, 1.0),
                    ] {
                        let mut b = [1.2, 0.3, -0.1, 0.2, 300.15];
                        b[axis] = first;
                        let mut sources = String::new();
                        for (j, node) in ["g", "d", "b", "e"].into_iter().enumerate() {
                            if j == axis {
                                writeln!(
                                    sources,
                                    "V{node} {node} 0 PWL(0 {} 1n {} 11n {} 12n {})",
                                    sign * first,
                                    sign * first,
                                    sign * last,
                                    sign * last
                                )?;
                            } else {
                                writeln!(sources, "V{node} {node} 0 {}", sign * b[j])?;
                            }
                        }
                        let deck = format!(
                            "PD intrinsic charge witness\n.options reltol=1e-10 abstol=1e-19 vntol=1e-12 gmin=1e-18\n.temp 27\n.model m {polarity} level=57 {card}\n{sources}Vs s 0 0\nM1 d g s e b 0 b m {INSTANCE} debug=1\n.control\nset num_threads=1\nset numdgt=17\nset wr_vecnames\nset wr_singlescale\nsave all\ntran 0.01n 12n 0 0.005n\nwrdata witness.tsv v(g) v(d) v(b) v(e) i(Vg) i(Vd) i(Vs) i(Vb) i(Ve) v(m1#ids)\nquit\n.endc\n.end\n"
                        );
                        let tmp = tempfile::tempdir()?;
                        fs::write(tmp.path().join("witness.cir"), deck)?;
                        let output = Command::new("ngspice")
                            .args(["-b", "witness.cir"])
                            .current_dir(tmp.path())
                            .output()?;
                        assert!(
                            output.status.success(),
                            "{}",
                            String::from_utf8_lossy(&output.stderr)
                        );
                        let table = fs::read_to_string(tmp.path().join("witness.tsv"))?;
                        let initial = evaluate(&p, b, phi)?;
                        let mut last_t = 0.0;
                        let mut last_i = [0.0; 5];
                        let mut integrated = [0.0; 5];
                        let mut rows = 0;
                        for line in table.lines().skip(1) {
                            let row = line
                                .split_whitespace()
                                .map(str::parse::<f64>)
                                .collect::<Result<Vec<_>, _>>()?;
                            let [t, g, d, body, e, ig, id, is, ib, ie, ids] = row.as_slice() else {
                                return Err("invalid native charge row".into());
                            };
                            let conduction = ids - 1e-14;
                            let currents = [
                                -sign * ig,
                                -sign * id - conduction,
                                -sign * is + conduction,
                                -sign * ib,
                                -sign * ie,
                            ];
                            if rows > 0 {
                                for k in 0..5 {
                                    integrated[k] += (currents[k] + last_i[k]) * 0.5 * (t - last_t);
                                }
                            }
                            last_t = *t;
                            last_i = currents;
                            rows += 1;
                            if rows % 50 == 0 || *t >= 12e-9 {
                                let actual = evaluate(
                                    &p,
                                    [sign * g, sign * d, sign * body, sign * e, 300.15],
                                    phi,
                                )?;
                                for k in 0..5 {
                                    let expected = actual[k].charge_c - initial[k].charge_c;
                                    let error = (integrated[k] - expected).abs();
                                    max = max.max(error);
                                    assert!(
                                        error < 2e-22 + 1e-5 * expected.abs(),
                                        "{polarity} capmod={mode} partition={partition} axis={axis} t={t:e} terminal={k}: integrated {:.6e}, charge delta {expected:e}, error {error:e}",
                                        integrated[k]
                                    );
                                    comparisons += 1;
                                }
                            }
                        }
                        assert!(rows > 2000);
                    }
                }
            }
        }
        println!("{comparisons} native PD terminal charge ramp comparisons; max error {max:e} C");
        Ok(())
    }
}
