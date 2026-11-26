use ParameterValue::{Flag, Integer, Real};
use model_compact_intrinsic::{
    BoundParameters, BsimSoiInstanceInput, ModelVariant, ParameterScope, ParameterValue,
    PreparedBsimSoiSetup, prepare_bsim_soi_model, prepare_bsim_soi_setup,
};
type TestResult = Result<(), Box<dyn std::error::Error>>;

const CARD: &str = "isbjt=100 idbjt=150 isdif=0 iddif=0 isrec=0 idrec=0 istun=0 idtun=0 ndiode=1.1 ndioded=1.4 ahli=0.001 ahlid=0.002 xbjt=1.3 ln=0.7u lbjt0=0.3u nbjt=0.8 ndif=0.6 ldif0=0.2 vabjt=2 aely=1e6";

fn setup(
    pmos: bool,
    heat: bool,
    off: bool,
) -> Result<PreparedBsimSoiSetup, Box<dyn std::error::Error>> {
    setup_with(pmos, heat, off, &[])
}

fn setup_with(
    pmos: bool,
    heat: bool,
    off: bool,
    extra: &[(&str, ParameterValue)],
) -> Result<PreparedBsimSoiSetup, Box<dyn std::error::Error>> {
    // Native SPICE units for these explicit geometry terms are SI.
    let model_values = CARD
        .split_whitespace()
        .map(|token| {
            let (name, value) = token.split_once('=').ok_or("bad card")?;
            let number = if let Some(value) = value.strip_suffix('u') {
                value.parse::<f64>()? * 1e-6
            } else {
                value.parse()?
            };
            Ok((name, Real(number)))
        })
        .collect::<Result<Vec<_>, Box<dyn std::error::Error>>>()?;
    let model = BoundParameters::bind(
        ModelVariant::BsimSoi,
        ParameterScope::Model,
        model_values
            .into_iter()
            .chain([
                ("pmos", Flag(pmos)),
                ("shmod", Integer(i32::from(heat))),
                ("rth0", Real(0.005)),
                ("cth0", Real(1e-6)),
                ("tsi", Real(1e-7)),
            ])
            .chain(extra.iter().cloned()),
    )?;
    let model = prepare_bsim_soi_model(&model, 300.15)?;
    let raw = BoundParameters::bind(
        ModelVariant::BsimSoi,
        ParameterScope::Instance,
        [
            ("tnodeout", Flag(true)),
            ("l", Real(5e-7)),
            ("w", Real(4e-6)),
            ("nf", Real(2.)),
            ("nseg", Real(2.)),
            ("m", Real(2.3)),
            ("bjtoff", Integer(i32::from(off))),
        ],
    )?;
    Ok(prepare_bsim_soi_setup(
        &model,
        &[BsimSoiInstanceInput {
            parameters: &raw,
            terminal_count: 6,
            geometry_scale: 1.,
        }],
    )?)
}

fn close(actual: f64, expected: f64, tol: f64) {
    assert!(
        (actual - expected).abs() <= tol * actual.abs().max(expected.abs()) + 1e-18,
        "{actual:e} vs {expected:e}"
    );
}

#[test]
fn neutral_bipolar_analytic_derivatives() -> TestResult {
    for pmos in [false, true] {
        let sign = if pmos { -1. } else { 1. };
        for heat in [false, true] {
            for off in [false, true] {
                let setup = setup(pmos, heat, off)?;
                let law = setup.prepare_junctions(0, 300.15)?;
                for temp in [240., 300.15, 380.] {
                    for (bs, bd) in [
                        (-0.3, -0.2),
                        (0.1, -0.2),
                        (-0.2, 0.1),
                        (0.4, 0.25),
                        (0.25, 0.4),
                    ] {
                        let (bs, bd) = (sign * bs, sign * bd);
                        let eval = |t, b, d| law.evaluate(t, b, d).map(|x| x.neutral_body);
                        let point = eval(temp, bs, bd)?;
                        let h = 1e-6;
                        let ht = 1e-3;
                        let high_bs = eval(temp, bs + h, bd)?;
                        let low_bs = eval(temp, bs - h, bd)?;
                        let high_bd = eval(temp, bs, bd + h)?;
                        let low_bd = eval(temp, bs, bd - h)?;
                        let hot = eval(temp + ht, bs, bd)?;
                        let cold = eval(temp - ht, bs, bd)?;
                        for (a, b, c, d, e) in [
                            (
                                point.source_recombination,
                                high_bs.source_recombination,
                                low_bs.source_recombination,
                                hot.source_recombination,
                                cold.source_recombination,
                            ),
                            (
                                point.drain_recombination,
                                high_bd.drain_recombination,
                                low_bd.drain_recombination,
                                hot.drain_recombination,
                                cold.drain_recombination,
                            ),
                            (
                                point.source_diffusion,
                                high_bs.source_diffusion,
                                low_bs.source_diffusion,
                                hot.source_diffusion,
                                cold.source_diffusion,
                            ),
                            (
                                point.drain_diffusion,
                                high_bd.drain_diffusion,
                                low_bd.drain_diffusion,
                                hot.drain_diffusion,
                                cold.drain_diffusion,
                            ),
                        ] {
                            close(a.conductance, (b.current - c.current) / (2. * h), 1e-5);
                            close(a.d_temperature, (d.current - e.current) / (2. * ht), 1e-5);
                        }
                        let collector = point.collector;
                        close(
                            collector.d_body_source,
                            (high_bs.collector.current - low_bs.collector.current) / (2. * h),
                            1e-5,
                        );
                        close(
                            collector.d_body_drain,
                            (high_bd.collector.current - low_bd.collector.current) / (2. * h),
                            1e-5,
                        );
                        close(
                            collector.d_temperature,
                            (hot.collector.current - cold.collector.current) / (2. * ht),
                            1e-5,
                        );
                        if off {
                            assert_eq!(collector.current, 0.);
                        }
                    }
                }
                assert_eq!(
                    law.evaluate(300.15, 0.1, 0.1)?
                        .neutral_body
                        .collector
                        .current,
                    0.
                );
            }
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires local ngspice 47 binary"]
fn native_neutral_body_reference() -> TestResult {
    use std::{fs, process::Command};
    let mut count = 0;
    for pmos in [false, true] {
        let sign = if pmos { -1. } else { 1. };
        let polarity = if pmos { "pmos" } else { "nmos" };
        let on = setup(pmos, true, false)?;
        let off = setup(pmos, true, true)?;
        for ambient_c in [-40., 85.] {
            let ambient = ambient_c + 273.15;
            let on_law = on.prepare_junctions(0, ambient)?;
            let off_law = off.prepare_junctions(0, ambient)?;
            for (drain, body) in [(0.2, -0.1), (-0.2, -0.1), (0.2, 0.25), (-0.2, 0.15)] {
                let vd = sign * drain;
                let vb = sign * body;
                let vg = sign * -0.5;
                let actual = on_law.evaluate(ambient + 10., vb, vb - vd)?.neutral_body;
                let disabled = off_law.evaluate(ambient + 10., vb, vb - vd)?.neutral_body;
                assert_eq!(actual.source_recombination, disabled.source_recombination);
                assert_eq!(actual.drain_recombination, disabled.drain_recombination);
                let mut external = [0.; 2];
                for bjtoff in [0, 1] {
                    let dir = tempfile::tempdir()?;
                    let deck = format!(
                        "BSIMSOI neutral body reference\nVd d 0 {vd}\nVg g 0 {vg}\nVe e 0 0\nVb b 0 {vb}\nVt t 0 10\nM1 d g 0 e b t soi tnodeout l=.5u w=4u nf=2 nseg=2 m=2.3 pd=4u ps=4u bjtoff={bjtoff}\n.model soi {polarity} level=10 soimod=0 shmod=1 rth0=.005 cth0=1e-6 tsi=.1u\n+ {CARD}\n.temp {ambient_c}\n.options gmin=1e-18 reltol=1e-10 abstol=1e-18 vntol=1e-12\n.control\nset numdgt=17\nset wr_singlescale\nop\nwrdata out.dat @m1[ibs] @m1[ibd] i(vd)\nquit\n.endc\n.end\n"
                    );
                    fs::write(dir.path().join("deck.cir"), deck)?;
                    let output = Command::new("ngspice")
                        .current_dir(dir.path())
                        .args(["-b", "-n", "deck.cir"])
                        .output()?;
                    assert!(
                        output.status.success(),
                        "{}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                    let native: Vec<f64> = fs::read_to_string(dir.path().join("out.dat"))?
                        .split_whitespace()
                        .map(str::parse)
                        .collect::<Result<_, _>>()?;
                    assert_eq!(native.len(), 4);
                    close(actual.source_recombination.current, sign * native[1], 1e-6);
                    close(actual.drain_recombination.current, sign * native[2], 1e-6);
                    external[bjtoff] = -native[3];
                }
                // All other inputs are identical; with the heat port fixed
                // only the bipolar collector transfer is removed by BJTOFF.
                close(actual.collector.current, external[0] - external[1], 1e-5);
                count += 1;
            }
        }
    }
    assert_eq!(count, 16);
    Ok(())
}

const CHARGE_CARD: &str = "cjswg=2e-10 cjswgd=3e-10 pbswg=.65 pbswgd=.75 mjswg=.5 mjswgd=.33 tcjswg=.001 tcjswgd=.0015 tpbswg=.0002 tpbswgd=.0003 tt=2e-9";

fn charge_parameters() -> Result<Vec<(&'static str, ParameterValue)>, Box<dyn std::error::Error>> {
    CHARGE_CARD
        .split_whitespace()
        .map(|token| {
            let (name, value) = token.split_once('=').ok_or("bad charge card")?;
            Ok((name, Real(value.parse()?)))
        })
        .collect()
}

fn close_charge(actual: f64, expected: f64, tolerance: f64) {
    assert!(
        (actual - expected).abs() <= tolerance * actual.abs().max(expected.abs()) + 1e-26,
        "{actual:e} vs {expected:e}"
    );
}

#[test]
fn junction_storage_analytic_derivatives() -> TestResult {
    for pmos in [false, true] {
        let sign = if pmos { -1. } else { 1. };
        for heat in [false, true] {
            for grading in [0.33, 0.5, 1.] {
                let mut parameters = charge_parameters()?;
                parameters.retain(|(name, _)| *name != "mjswg" && *name != "mjswgd");
                parameters.extend([("mjswg", Real(grading)), ("mjswgd", Real(grading))]);
                let setup = setup_with(pmos, heat, false, &parameters)?;
                let law = setup.prepare_junctions(0, 300.15)?;
                for temp in [240., 300.15, 380.] {
                    for (bs, bd) in [(-0.4, -0.2), (0.1, -0.3), (0.4, 0.2), (0.8, 0.9)] {
                        let (bs, bd) = (sign * bs, sign * bd);
                        let eval = |t, b, d| law.evaluate(t, b, d).map(|x| x.charges);
                        let point = eval(temp, bs, bd)?;
                        let h = 1e-6;
                        let ht = 1e-3;
                        let plus = eval(temp, bs + h, bd + h)?;
                        let minus = eval(temp, bs - h, bd - h)?;
                        let hot = eval(temp + ht, bs, bd)?;
                        let cold = eval(temp - ht, bs, bd)?;
                        for (a, b, c, d, e) in [
                            (
                                point.source,
                                plus.source,
                                minus.source,
                                hot.source,
                                cold.source,
                            ),
                            (point.drain, plus.drain, minus.drain, hot.drain, cold.drain),
                        ] {
                            for (a, b, c, d, e) in [
                                (
                                    a.depletion,
                                    b.depletion,
                                    c.depletion,
                                    d.depletion,
                                    e.depletion,
                                ),
                                (
                                    a.diffusion,
                                    b.diffusion,
                                    c.diffusion,
                                    d.diffusion,
                                    e.diffusion,
                                ),
                                (a.total(), b.total(), c.total(), d.total(), e.total()),
                            ] {
                                close_charge(a.capacitance, (b.charge - c.charge) / (2. * h), 2e-5);
                                close_charge(
                                    a.d_temperature,
                                    (d.charge - e.charge) / (2. * ht),
                                    2e-5,
                                );
                            }
                        }
                    }
                }
            }
        }
    }
    let mut parameters = charge_parameters()?;
    parameters.push(("soimod", Integer(2)));
    let setup = setup_with(false, true, false, &parameters)?;
    let point = setup
        .prepare_junctions(0, 300.15)?
        .evaluate(325., 0.4, 0.2)?;
    assert_eq!(point.charges, Default::default());
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires local ngspice 47 binary"]
fn native_junction_storage_reference() -> TestResult {
    use std::{fs, process::Command};
    let parameters = charge_parameters()?;
    for pmos in [false, true] {
        let sign = if pmos { -1. } else { 1. };
        let polarity = if pmos { "pmos" } else { "nmos" };
        let setup = setup_with(pmos, true, false, &parameters)?;
        for ambient_c in [-40., 85.] {
            let ambient = ambient_c + 273.15;
            let law = setup.prepare_junctions(0, ambient)?;
            for (drain, body) in [(0.2, -0.1), (-0.2, -0.1), (0.2, 0.25), (-0.2, 0.15)] {
                let vd = sign * drain;
                let vb = sign * body;
                let vg = sign * -0.5;
                let point = law.evaluate(ambient + 10., vb, vb - vd)?.charges;
                for thermal in [false, true] {
                    let mut currents = [[0.; 3]; 2];
                    let bac = u8::from(!thermal);
                    let tac = u8::from(thermal);
                    for (index, card) in ["cjswg=0 cjswgd=0 tt=0", CHARGE_CARD]
                        .into_iter()
                        .enumerate()
                    {
                        let dir = tempfile::tempdir()?;
                        let deck = format!(
                            "BSIMSOI junction storage reference\nVs s 0 0\nVd d 0 {vd}\nVg g 0 {vg}\nVe e 0 0\nVb b 0 {vb} AC {bac}\nVt t 0 10 AC {tac}\nM1 d g s e b t soi tnodeout l=.5u w=4u nf=2 nseg=2 m=2.3 pd=4u ps=4u\n.model soi {polarity} level=10 soimod=0 shmod=1 rth0=.005 cth0=1e-6 tsi=.1u\n+ {CARD}\n+ {card}\n.temp {ambient_c}\n.options gmin=1e-18 reltol=1e-10 abstol=1e-18 vntol=1e-12\n.control\nset numdgt=17\nset wr_singlescale\nop\nac lin 1 1e9 1e9\nwrdata out.dat i(vs) i(vd) i(vb)\nquit\n.endc\n.end\n"
                        );
                        fs::write(dir.path().join("deck.cir"), deck)?;
                        let output = Command::new("ngspice")
                            .current_dir(dir.path())
                            .args(["-b", "-n", "deck.cir"])
                            .output()?;
                        assert!(
                            output.status.success(),
                            "{}",
                            String::from_utf8_lossy(&output.stderr)
                        );
                        let values: Vec<f64> = fs::read_to_string(dir.path().join("out.dat"))?
                            .split_whitespace()
                            .map(str::parse)
                            .collect::<Result<_, _>>()?;
                        assert_eq!(values.len(), 7);
                        for (i, j) in [2, 4, 6].into_iter().enumerate() {
                            currents[index][i] = values[j] / (std::f64::consts::TAU * values[0]);
                        }
                    }
                    let src = point.source.total();
                    let drn = point.drain.total();
                    let (qs, qd) = if thermal {
                        (src.d_temperature, drn.d_temperature)
                    } else {
                        (src.capacitance, drn.capacitance)
                    };
                    for (i, expected) in [qs, qd, -qs - qd].into_iter().enumerate() {
                        close_charge(currents[1][i] - currents[0][i], expected, 2e-5);
                    }
                }
            }
        }
    }
    Ok(())
}
