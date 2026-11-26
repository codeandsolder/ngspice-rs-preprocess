use super::*;
use model_compact_intrinsic::bsim3soi::{PreparedSubstrateCharge, prepare_temperature};

const INSTANCE: &str = "l=2e-6 w=4e-6 ad=1e-9 as=2e-9 pd=10e-6 ps=12e-6 m=3";

fn prepared(
    family: ModelVariant,
    fields: &str,
    polarity: &str,
) -> Result<PreparedSubstrateCharge, Box<dyn Error>> {
    let m = model(family, fields, polarity)?;
    let instance = prepare_instance(&m, &bind(family, ParameterScope::Instance, INSTANCE, None)?)?;
    Ok(prepare_temperature(&m, &instance, 300.15)?.substrate_charge)
}

#[test]
fn substrate_charge_derivative_and_transition_boundaries() -> Result<(), Box<dyn Error>> {
    for family in FAMILIES {
        for (polarity, flatband, threshold) in [("nmos", -0.2, 0.6), ("pmos", 0.6, -0.2)] {
            for asd in [0.0_f64, 0.3, 0.7, 1.0] {
                let fields = format!(
                    "vsdfb={flatband} vsdth={threshold} asd={asd} csdmin=5e-5 csdesw=2e-10"
                );
                let j = prepared(family, &fields, polarity)?;
                let split = asd.mul_add(0.8, -0.2);
                for v in [-1.0, -0.2, -0.1, split, 0.3, 0.6, 1.0] {
                    let lo = j.evaluate(v - 1e-6, v - 1e-6)?;
                    let hi = j.evaluate(v + 1e-6, v + 1e-6)?;
                    let mid = j.evaluate(v, v)?;
                    for i in 0..2 {
                        let actual = (hi[i].charge_c - lo[i].charge_c) / 2e-6;
                        let expected = mid[i].capacitance_f;
                        // The exact zero-length subinterval limit has a kink
                        // in its slope at an end, but charge is continuous.
                        assert!(
                            (actual - expected).abs() < 2e-6 * expected.abs() + 1e-23,
                            "{family:?} {polarity} {asd} {v} {actual:e} != {expected:e}"
                        );
                    }
                }
            }
        }
    }
    let pd = prepared(ModelVariant::Bsim3SoiPd, "csdmin=0 csdesw=2e-10", "nmos")?;
    let zero = pd.evaluate(0.0, 0.0)?;
    assert_eq!(zero[0].charge_c, 0.0);
    let value = pd.evaluate(0.5, -0.7)?;
    close(
        value[0].charge_c,
        value[0].capacitance_f * 0.5,
        "PD linear drain",
    );
    close(
        value[1].charge_c,
        value[1].capacitance_f * -0.7,
        "PD linear source",
    );
    Ok(())
}

#[cfg(target_os = "linux")]
mod native {
    use super::*;
    use std::{fs, process::Command};

    fn run(
        family: ModelVariant,
        fields: &str,
        polarity: &str,
        level: i32,
    ) -> Result<(usize, f64), Box<dyn Error>> {
        let j = prepared(family, fields, polarity)?;
        let temp = tempfile::tempdir()?;
        let vds = if polarity == "nmos" { 0.1 } else { -0.1 };
        // Separate native model records intentionally avoid the legacy
        // size-cache bug in which area-dependent coefficients are shared.
        let transient = level != 57;
        let substrate_source = if transient {
            "PWL(0 -1 1u 1)"
        } else {
            "0 ac 1"
        };
        let analysis = if transient {
            "tran 0.1n 1u 0 0.1n\nlet cd=(vd#branch-vd0#branch)/2e6\nlet cs=(vs#branch-vs0#branch)/2e6\nwrdata witness.tsv v(e) cd cs"
        } else {
            "foreach v -1 -0.7 -0.5 -0.35 -0.2 -0.1 0 0.1 0.2 0.35 0.5 0.7 1\nalter Ve $v\nac lin 1 1e6 1e6\nlet cd=imag(vd#branch-vd0#branch)/(2*pi*frequency)\nlet cs=imag(vs#branch-vs0#branch)/(2*pi*frequency)\nwrdata witness.tsv cd cs\nend"
        };
        let deck = format!(
            "BSIM3SOI substrate capacitance witness\n.model m {polarity} level={level} {fields}\n.model m0 {polarity} level={level} {fields}\nVd d 0 {vds}\nVs s 0 0\nVg g 0 0\nVe e 0 {substrate_source}\nVd0 d0 0 {vds}\nVs0 s0 0 0\nM1 d g s e 0 m {INSTANCE}\nM0 d0 g s0 e 0 m0 l=2e-6 w=4e-6 ad=0 as=0 pd=0 ps=0 m=3\n.options method=gear maxord=2 reltol=1e-8 abstol=1e-16 vntol=1e-10\n.control\nset num_threads=1\nset numdgt=17\nset wr_singlescale\nset wr_vecnames\nset appendwrite\n{analysis}\nquit\n.endc\n.end\n"
        );
        fs::write(temp.path().join("witness.cir"), &deck)?;
        let result = Command::new("ngspice")
            .args(["-b", "-n", "witness.cir"])
            .current_dir(temp.path())
            .output()?;
        assert!(
            result.status.success(),
            "{deck}\n{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        let table = fs::read_to_string(temp.path().join("witness.tsv"))?;
        let rows = table
            .lines()
            .filter(|line| {
                !line.trim_start().starts_with("frequency")
                    && !line.trim_start().starts_with("time")
            })
            .map(|line| {
                line.split_whitespace()
                    .map(str::parse::<f64>)
                    .collect::<Result<Vec<_>, _>>()
            })
            .collect::<Result<Vec<_>, _>>()?;
        let bias = [
            -1.0, -0.7, -0.5, -0.35, -0.2, -0.1, 0.0, 0.1, 0.2, 0.35, 0.5, 0.7, 1.0,
        ];
        let mut max = 0.0_f64;
        if transient {
            assert!(rows.len() > 10000);
            let initial = j.evaluate(vds + 1.0, 1.0)?;
            let mut previous_v = -1.0;
            let mut previous_cap = [initial[0].capacitance_f, initial[1].capacitance_f];
            let mut charge_delta = [0.0; 2];
            for row in rows.iter().skip(1) {
                let [_, v, cd, cs] = row.as_slice() else {
                    return Err("bad transient row".into());
                };
                let a = j.evaluate(vds - v, -v)?;
                for (index, expected) in [*cd, *cs].into_iter().enumerate() {
                    let actual = a[index].capacitance_f;
                    let error = (actual - expected).abs();
                    max = max.max(error);
                    assert!(
                        error < 1e-22 + 2e-7 * expected.abs(),
                        "{family:?} {polarity} transient e={v}: {actual:e} != {expected:e}"
                    );
                    // Integrate the measured displacement current along the
                    // linear substrate ramp, with the physical sign.
                    charge_delta[index] -=
                        (previous_cap[index] + expected) * 0.5 * (v - previous_v);
                    previous_cap[index] = expected;
                    let expected_charge = a[index].charge_c - initial[index].charge_c;
                    assert!(
                        (charge_delta[index] - expected_charge).abs()
                            < 2e-20 + 2e-7 * expected_charge.abs(),
                        "{family:?} {polarity} integrated charge at e={v}"
                    );
                }
                previous_v = *v;
            }
            Ok(((rows.len() - 1) * 2, max))
        } else {
            assert_eq!(rows.len(), bias.len());
            for (row, v) in rows.iter().zip(bias) {
                let [_, cd, _, cs, _] = row.as_slice() else {
                    return Err("bad AC row".into());
                };
                let a = j.evaluate(vds - v, -v)?;
                for (actual, expected) in [(a[0].capacitance_f, *cd), (a[1].capacitance_f, *cs)] {
                    let error = (actual - expected).abs();
                    max = max.max(error);
                    assert!(
                        error < 1e-22 + 1e-9 * expected.abs(),
                        "{family:?} {polarity} AC e={v} {actual:e} != {expected:e}"
                    );
                }
            }
            Ok((rows.len() * 2, max))
        }
    }

    #[test]
    #[ignore = "requires independent ngspice 47 executable"]
    fn native_substrate_capacitance_matrix() -> Result<(), Box<dyn Error>> {
        let mut n = 0;
        let mut max = 0.0_f64;
        for (family, level) in FAMILIES.into_iter().zip([55, 56, 57]) {
            for (polarity, fields) in [
                (
                    "nmos",
                    "csdesw=2e-10 csdmin=5e-5 vsdfb=-0.2 vsdth=0.6 asd=0.35 wint=1e-7",
                ),
                (
                    "pmos",
                    "csdesw=2e-10 csdmin=5e-5 vsdfb=0.6 vsdth=-0.2 asd=0.65 wint=1e-7",
                ),
            ] {
                let (count, error) = run(family, fields, polarity, level)?;
                n += count;
                max = max.max(error);
            }
            if level == 57 {
                let (count, error) = run(family, "csdesw=2e-10 csdmin=0", "nmos", level)?;
                n += count;
                max = max.max(error);
            }
        }
        assert!(n > 80000);
        println!(
            "{n} native differential transient/AC capacitance values, maximum absolute deviation {max:e} F"
        );
        Ok(())
    }
}
