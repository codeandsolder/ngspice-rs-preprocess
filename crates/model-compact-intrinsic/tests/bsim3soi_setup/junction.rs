use super::*;
use model_compact_intrinsic::bsim3soi::{
    JunctionCurrent, JunctionEvaluation, PreparedJunctions, prepare_temperature,
};

const COMMON: &str = "isdif=1e-3 isbjt=2e-3 isrec=3e-3 istun=1e-3 ndiode=1.2 ntun=3";
const PD: &str = "vrec0=0.9 vtun0=0.8 ahli=0.01 nrecf0=1.8 nrecr0=4 ntrecf=0.1 ntrecr=-0.1 lbjt0=3e-7 nbjt=1.1 vabjt=8 aely=1e5";
const DD: &str = "edl=2e-6 kbjt1=1e-6";

fn fields(family: ModelVariant, extra: &str) -> String {
    let family_fields = match family {
        ModelVariant::Bsim3SoiFd => "",
        ModelVariant::Bsim3SoiDd => DD,
        ModelVariant::Bsim3SoiPd => PD,
        _ => "",
    };
    format!("{COMMON} {family_fields} {extra}")
}

fn evaluator(
    family: ModelVariant,
    card: &str,
    instance: &str,
    polarity: &str,
    ambient: f64,
) -> Result<PreparedJunctions, Box<dyn Error>> {
    let m = model(family, card, polarity)?;
    let i = prepare_instance(&m, &bind(family, ParameterScope::Instance, instance, None)?)?;
    Ok(prepare_temperature(&m, &i, ambient)?.junction_currents)
}

fn components(value: JunctionEvaluation) -> [JunctionCurrent; 13] {
    let s = value.source;
    let d = value.drain;
    [
        s.diffusion,
        s.recombination,
        s.bipolar,
        s.tunneling,
        s.total,
        s.stored_charge_diffusion,
        d.diffusion,
        d.recombination,
        d.bipolar,
        d.tunneling,
        d.total,
        d.stored_charge_diffusion,
        value.collector,
    ]
}

#[test]
fn junction_voltage_and_thermal_derivatives() -> Result<(), Box<dyn Error>> {
    let mut count = 0;
    for family in FAMILIES {
        for polarity in ["nmos", "pmos"] {
            let card = fields(family, "shmod=1 rth0=0.02 tnom=32 xbjt=1.5 xrec=2 xtun=1");
            let j = evaluator(family, &card, "l=1e-6 w=3e-6", polarity, 300.15)?;
            for vbs in [-0.5, -0.1, 0.02, 0.2, 0.45, 0.65] {
                for vds in [0.1, 0.3, 0.99, 1.5] {
                    for temp in [260.0, 300.15, 360.0] {
                        let a = components(j.evaluate(vbs, vds, temp)?);
                        for (axis, h) in [1e-6, 1e-6, 1e-3].into_iter().enumerate() {
                            let mut lo = [vbs, vds, temp];
                            let mut hi = lo;
                            lo[axis] -= h;
                            hi[axis] += h;
                            let low = components(j.evaluate(lo[0], lo[1], lo[2])?);
                            let high = components(j.evaluate(hi[0], hi[1], hi[2])?);
                            for (index, (middle, (l, r))) in
                                a.iter().zip(low.into_iter().zip(high)).enumerate()
                            {
                                let d = (r.current_a - l.current_a) / (2.0 * h);
                                let expected = [
                                    middle.body_conductance_s,
                                    middle.drain_conductance_s,
                                    middle.temperature_derivative_a_per_k,
                                ][axis];
                                assert!(
                                    // Finite-difference roundoff is proportional to the differenced currents.
                                    (d - expected).abs()
                                        < 8.0
                                            * f64::EPSILON
                                            * l.current_a.abs().max(r.current_a.abs())
                                            / h
                                            + 1e-20
                                            + 3e-6 * expected.abs(),
                                    "{family:?} {polarity} {vbs} {vds} {temp} axis {axis} component {index}: {d:e} != {expected:e}"
                                );
                                count += 1;
                            }
                        }
                    }
                }
            }
        }
    }
    println!("{count} junction analytic derivative checks");
    Ok(())
}

#[test]
fn junction_disabled_and_guarded_branches() -> Result<(), Box<dyn Error>> {
    for family in FAMILIES {
        let card = fields(family, "");
        let active = evaluator(family, &card, "l=1e-6 w=3e-6", "nmos", 300.15)?;
        let off = evaluator(family, &card, "l=1e-6 w=3e-6 bjtoff=1", "nmos", 300.15)?;
        let a = active.evaluate(0.4, 0.2, 340.0)?;
        let b = off.evaluate(0.4, 0.2, 340.0)?;
        assert_eq!(b.collector.current_a, 0.0);
        assert_eq!(active.evaluate(0.4, 0.0, 340.0)?.collector.current_a, 0.0);
        assert_eq!(a.collector.temperature_derivative_a_per_k, 0.0);
        assert_eq!(a, active.evaluate(0.4, 0.2, 300.15)?);
        assert!(active.evaluate(0.4, -0.1, 300.15).is_err());
        match family {
            ModelVariant::Bsim3SoiFd => assert_eq!(a, JunctionEvaluation::default()),
            ModelVariant::Bsim3SoiDd => {
                assert!(a.source.bipolar.current_a > 0.0);
                assert_eq!(b.source.bipolar.current_a, 0.0);
            }
            ModelVariant::Bsim3SoiPd => {
                assert!(a.source.bipolar.current_a > 0.0);
                assert_eq!(a.source.bipolar, b.source.bipolar);
                assert_eq!(
                    a.source.stored_charge_diffusion,
                    b.source.stored_charge_diffusion
                );
            }
            _ => return Err("unexpected family".into()),
        }
    }
    Ok(())
}

#[test]
fn junction_charge_derivatives_and_continuations() -> Result<(), Box<dyn Error>> {
    let mut count = 0;
    for family in FAMILIES {
        for grading in [0.4, 0.5, 1.0] {
            let extra = if family == ModelVariant::Bsim3SoiPd {
                "tpbswg=1e-3 tcjswg=0.002"
            } else {
                ""
            };
            let card = fields(
                family,
                &format!(
                    "shmod=1 rth0=0.02 tnom=32 cjswg=3e-10 pbswg=0.5 mjswg={grading} tt=1e-9 {extra}"
                ),
            );
            let j = evaluator(family, &card, "l=1e-6 w=3e-6", "nmos", 300.15)?;
            for temp in [265.0, 305.15, 360.0] {
                for vbs in [-0.5, -0.01, 0.0, 0.1, 0.45, 0.65] {
                    let vds = 0.17;
                    let a = j.evaluate(vbs, vds, temp)?;
                    for (axis, h) in [1e-6, 1e-6, 1e-3].into_iter().enumerate() {
                        let mut lo = [vbs, vds, temp];
                        let mut hi = lo;
                        lo[axis] -= h;
                        hi[axis] += h;
                        let l = j.evaluate(lo[0], lo[1], lo[2])?;
                        let r = j.evaluate(hi[0], hi[1], hi[2])?;
                        for (mid, left, right) in [
                            (a.source_charge, l.source_charge, r.source_charge),
                            (a.drain_charge, l.drain_charge, r.drain_charge),
                        ] {
                            let numerical = (right.charge_c - left.charge_c) / (2.0 * h);
                            let expected = [
                                mid.body_capacitance_f,
                                mid.drain_capacitance_f,
                                mid.temperature_derivative_c_per_k,
                            ][axis];
                            let roundoff =
                                8.0 * f64::EPSILON * left.charge_c.abs().max(right.charge_c.abs())
                                    / h;
                            assert!(
                                (numerical - expected).abs()
                                    < roundoff + 1e-27 + 5e-6 * expected.abs(),
                                "{family:?} grading={grading} T={temp} VBS={vbs} axis={axis}: {numerical:e} != {expected:e}"
                            );
                            count += 1;
                        }
                    }
                }
            }
        }
    }
    println!("{count} analytic junction charge derivative checks");
    Ok(())
}

#[cfg(target_os = "linux")]
mod native {
    use super::*;
    use std::{fs, process::Command};

    fn compare(
        family: ModelVariant,
        level: i32,
        polarity: &str,
        ambient: f64,
        off: i32,
    ) -> Result<(usize, f64), Box<dyn Error>> {
        let card = fields(family, "tnom=32 xbjt=1.5 xrec=2 xtun=1");
        let instance = format!(
            "l=1e-6 w=3e-6 bjtoff={off}{}",
            if level == 57 {
                " nseg=2 nbc=1 pdbcp=1e-7 psbcp=3e-7"
            } else {
                ""
            }
        );
        let j = evaluator(family, &card, &instance, polarity, ambient + 273.15)?;
        let sign = if polarity == "nmos" { 1.0 } else { -1.0 };
        let tmp = tempfile::tempdir()?;
        let debug = if level == 57 { 1 } else { 2 };
        let deck = format!(
            "BSIM3SOI junction witness\n.temp {ambient}\n.options reltol=1e-9 abstol=1e-19 vntol=1e-11\n.model m {polarity} level={level} {card}\nVd d 0 {}\nVg g 0 {}\nVb b 0 0\nVe e 0 0\nM1 d g 0 e b m {instance} debug={debug}\n.control\nset num_threads=1\nset numdgt=17\nset wr_vecnames\nset wr_singlescale\nsave all @m1[vbs] @m1[vds]\ndc Vb {} {} {}\nwrdata witness.tsv @m1[vbs] @m1[vds] v(m1#ibs) v(m1#ibd) v(m1#ic)\nquit\n.endc\n.end\n",
            0.3 * sign,
            -0.5 * sign,
            -0.5 * sign,
            0.65 * sign,
            0.025 * sign
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
        let mut max = 0.0_f64;
        for line in table.lines().skip(1) {
            let values = line
                .split_whitespace()
                .map(str::parse::<f64>)
                .collect::<Result<Vec<_>, _>>()?;
            let [_, vbs, vds, ibs, ibd, ic] = values.as_slice() else {
                return Err("bad native junction row".into());
            };
            let a = j.evaluate(*vbs, *vds, ambient + 273.15)?;
            // PD's debug node FLOG is abs(current)+1e-14, not the
            // signed current. DD exposes the actual signed value.
            for (current, expected) in [
                (a.source.total.current_a, *ibs),
                (a.drain.total.current_a, *ibd),
                (a.collector.current_a, *ic),
            ] {
                let actual = if level == 57 {
                    current.abs() + 1e-14
                } else {
                    current
                };
                let error = (actual - expected).abs();
                max = max.max(error);
                assert!(
                    error < 1e-19 + 1e-7 * expected.abs(),
                    "{family:?} {polarity} {ambient} off={off} VBS={vbs} VDS={vds}: {actual:e} != {expected:e}"
                );
                n += 1;
            }
        }
        assert_eq!(n, 141);
        Ok((n, max))
    }

    #[test]
    #[ignore = "requires independent ngspice 47 executable"]
    fn native_junction_matrix() -> Result<(), Box<dyn Error>> {
        let mut n = 0;
        let mut max = 0.0_f64;
        // FD is checked separately through its zero-current source branch.
        for (family, level) in [
            (ModelVariant::Bsim3SoiDd, 56),
            (ModelVariant::Bsim3SoiPd, 57),
        ] {
            for polarity in ["nmos", "pmos"] {
                for temp in [-10.0, 27.0, 85.0] {
                    for off in [0, 1] {
                        let (count, error) = compare(family, level, polarity, temp, off)?;
                        n += count;
                        max = max.max(error);
                    }
                }
            }
        }
        assert_eq!(n, 3384);
        println!(
            "{n} native junction/collector current comparisons, max absolute deviation {max:e} A"
        );
        Ok(())
    }
    fn charge_case(
        family: ModelVariant,
        level: i32,
        polarity: &str,
        ambient: f64,
        grading: f64,
    ) -> Result<(usize, f64), Box<dyn Error>> {
        let extra = if level == 57 {
            "tpbswg=1e-3 tcjswg=0.002"
        } else {
            ""
        };
        let card = fields(
            family,
            &format!("tnom=32 cjswg=3e-10 pbswg=0.5 mjswg={grading} tt=1e-9 {extra}"),
        );
        let instance = if level == 57 {
            "l=1e-6 w=3e-6 nseg=2 nbc=1 pdbcp=1e-7 psbcp=3e-7"
        } else {
            "l=1e-6 w=3e-6"
        };
        let j = evaluator(family, &card, instance, polarity, ambient + 273.15)?;
        let sign = if polarity == "nmos" { 1.0 } else { -1.0 };
        let debug = if level == 57 { 1 } else { 2 };
        let tmp = tempfile::tempdir()?;
        let deck = format!(
            "BSIM3SOI intrinsic charge witness\n.temp {ambient}\n.options reltol=1e-9 abstol=1e-19 vntol=1e-11\n.model m {polarity} level={level} {card}\nVd d 0 {}\nVg g 0 {}\nVb b 0 PWL(0 {} 1u {})\nVe e 0 0\nM1 d g 0 e b m {instance} debug={debug}\n.control\nset num_threads=1\nset numdgt=17\nset wr_vecnames\nset wr_singlescale\nsave all @m1[vbs] @m1[vds]\ntran 2n 1u 0 2n\nwrdata witness.tsv @m1[vbs] @m1[vds] v(m1#qjs) v(m1#qjd)\nquit\n.endc\n.end\n",
            0.17 * sign,
            -0.5 * sign,
            -0.5 * sign,
            0.65 * sign
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
        let mut max = 0.0_f64;
        // The zero-time DC initialization does not evaluate junction charge.
        for line in table.lines().skip(2) {
            let values = line
                .split_whitespace()
                .map(str::parse::<f64>)
                .collect::<Result<Vec<_>, _>>()?;
            let [_, vbs, vds, qs, qd] = values.as_slice() else {
                return Err("bad native charge row".into());
            };
            let a = j.evaluate(*vbs, *vds, ambient + 273.15)?;
            for (actual, expected) in [
                (a.source_charge.charge_c, *qs),
                (a.drain_charge.charge_c, *qd),
            ] {
                let error = (actual - expected).abs();
                max = max.max(error);
                assert!(
                    error < 1e-25 + 1e-8 * expected.abs(),
                    "{family:?} {polarity} {ambient} grading={grading} VBS={vbs} VDS={vds}: {actual:e} != {expected:e}"
                );
                n += 1;
            }
        }
        assert!(n >= 1000);
        Ok((n, max))
    }

    #[test]
    #[ignore = "requires independent ngspice 47 executable"]
    fn native_junction_charge_matrix() -> Result<(), Box<dyn Error>> {
        let mut n = 0;
        let mut max = 0.0_f64;
        for (family, level) in [
            (ModelVariant::Bsim3SoiDd, 56),
            (ModelVariant::Bsim3SoiPd, 57),
        ] {
            for polarity in ["nmos", "pmos"] {
                for (temp, grading) in [(-10.0, 0.4), (27.0, 0.5), (85.0, 0.65)] {
                    let (count, error) = charge_case(family, level, polarity, temp, grading)?;
                    n += count;
                    max = max.max(error);
                }
            }
        }
        println!("{n} native junction charge comparisons, maximum absolute deviation {max:e} C");
        Ok(())
    }
}
