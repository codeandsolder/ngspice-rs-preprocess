use super::*;
use model_compact_intrinsic::bsim3soi::prepare_temperature;

#[test]
#[ignore = "requires independent ngspice 47 executable"]
fn native_pd_forced_temperature() -> Result<(), Box<dyn Error>> {
    use std::{fs, process::Command};
    let family = ModelVariant::Bsim3SoiPd;
    let card = "shmod=1 rth0=0.02 tnom=32 isbjt=2e-3 isdif=1e-3 isrec=3e-3 istun=1e-3 ndiode=1.2 ntun=3 vrec0=0.9 vtun0=0.8 ahli=0.01 nrecf0=1.8 nrecr0=4 ntrecf=0.1 ntrecr=-0.1 lbjt0=3e-7 nbjt=1.1 vabjt=8 aely=1e5";
    let instance = "l=1e-6 w=3e-6";
    let mut count = 0;
    let mut max = [0.0_f64; 6];
    for polarity in ["nmos", "pmos"] {
        let sign = if polarity == "nmos" { 1.0 } else { -1.0 };
        let m = model(family, card, polarity)?;
        let i = prepare_instance(&m, &bind(family, ParameterScope::Instance, instance, None)?)?;
        let t = prepare_temperature(&m, &i, 300.15)?;
        let channel = t.pd_channel.as_ref().ok_or("PD channel missing")?;
        let phi = channel.initial_surface_potential();
        for rise in [-20.0, 0.0, 20.0, 40.0, 60.0] {
            for (gate, drain, body) in [(0.4, 0.1, -0.3), (1.5, 0.3, 0.25), (1.1, 0.7, 0.1)] {
                // Fresh operating point, no previous accepted predictor phi.
                let tmp = tempfile::tempdir()?;
                let deck = format!(
                    "PD externally imposed temperature witness\n.temp 27\n.options reltol=1e-9 abstol=1e-19 vntol=1e-11\n.model m {polarity} level=57 {card}\nVd d 0 {}\nVg g 0 {}\nVb b 0 {}\nVe e 0 0\nVt th 0 {rise}\nM1 d g 0 e b th b m {instance} debug=1 tnodeout\n.control\nset num_threads=1\nset numdgt=17\nset wr_vecnames\nset wr_singlescale\nsave all @m1[vgs] @m1[vds] @m1[vbs] @m1[vth] @m1[vdsat]\nop\nwrdata witness.tsv v(th) @m1[vgs] @m1[vds] @m1[vbs] @m1[vth] @m1[vdsat] v(m1#ids) v(m1#ibs) v(m1#ibd) v(m1#ic)\nquit\n.endc\n.end\n",
                    drain * sign,
                    gate * sign,
                    body * sign
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
                let row = table.lines().nth(1).ok_or("missing thermal witness")?;
                let values = row
                    .split_whitespace()
                    .map(str::parse::<f64>)
                    .collect::<Result<Vec<_>, _>>()?;
                let [_, delta, vgs, vds, vbs, vth, vdsat, ids, ibs, ibd, ic] = values.as_slice()
                else {
                    return Err("bad native thermal witness".into());
                };
                assert!((delta - rise).abs() < 1e-9 && (vbs - body).abs() < 1e-9);
                let kelvin = 300.15 + delta;
                let c = channel.evaluate(*vgs, *vds, *vbs, kelvin, phi)?;
                let j = t.junction_currents.evaluate(*vbs, *vds, kelvin)?;
                let actual = [
                    c.threshold_v,
                    c.saturation_voltage_v,
                    c.current_a.abs() + 1e-14,
                    j.source.total.current_a.abs() + 1e-14,
                    j.drain.total.current_a.abs() + 1e-14,
                    j.collector.current_a.abs() + 1e-14,
                ];
                let expected = [*vth, *vdsat, *ids, *ibs, *ibd, *ic];
                for (index, (a, b)) in actual.into_iter().zip(expected).enumerate() {
                    let error = (a - b).abs();
                    max[index] = max[index].max(error);
                    let abs = if index < 2 { 2e-10 } else { 1e-19 };
                    assert!(
                        error < abs + 1e-7 * b.abs(),
                        "{polarity} T={kelvin} VGS={vgs} VDS={vds} VBS={vbs} field={index}: {a:e} != {b:e}"
                    );
                    count += 1;
                }
                // The native predictor's next accepted phi can be derived
                // from the last solved temperature without mutating the trial.
                assert!(channel.accepted_surface_potential(kelvin)?.is_finite());
            }
        }
    }
    assert_eq!(count, 180);
    println!(
        "{count} native forced-temperature comparisons, max [Vth,Vdsat,Ids,Ibs,Ibd,Ic] {max:?}"
    );
    Ok(())
}
