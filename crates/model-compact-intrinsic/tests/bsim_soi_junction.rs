use ParameterValue::{Flag, Integer, Real};
use model_compact_intrinsic::{
    BoundParameters, BsimSoiInstanceInput, BsimSoiJunctionSide, ModelVariant, ParameterScope,
    ParameterValue, PreparedBsimSoiSetup, prepare_bsim_soi_model, prepare_bsim_soi_setup,
};
type TestResult = Result<(), Box<dyn std::error::Error>>;

const CARD: &str = "isbjt=0 idbjt=0 isdif=100 iddif=200 isrec=20 idrec=30 istun=40 idtun=50 ndiode=1.1 ndioded=1.4 nrecf0=2.3 nrecf0d=2.5 nrecr0=10 nrecr0d=8 vrec0=0.8 vrec0d=0.9 vtun0=0.5 vtun0d=0.6 ntun=10 ntund=12 xbjt=1 xdif=1.1 xdifd=0.9 xrec=0.5 xrecd=0.4 xtun=0.7 xtund=0.8 ntrecf=0.2 ntrecr=0.1";

fn setup(
    pmos: bool,
    heat: bool,
    mode: i32,
) -> Result<PreparedBsimSoiSetup, Box<dyn std::error::Error>> {
    let model_values = CARD
        .split_whitespace()
        .map(|token| {
            let (name, value) = token.split_once('=').ok_or("bad fixture")?;
            Ok((name, Real(value.parse()?)))
        })
        .collect::<Result<Vec<_>, Box<dyn std::error::Error>>>()?;
    let model = BoundParameters::bind(
        ModelVariant::BsimSoi,
        ParameterScope::Model,
        model_values.into_iter().chain([
            ("pmos", Flag(pmos)),
            ("soimod", Integer(mode)),
            ("shmod", Integer(i32::from(heat))),
            ("rth0", Real(0.005)),
            ("cth0", Real(1e-6)),
            ("tsi", Real(1e-7)),
        ]),
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
            ("pdbcp", Real(1e-7)),
            ("psbcp", Real(2e-7)),
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

fn check_derivative(actual: f64, numeric: f64) {
    let tolerance = 1e-5_f64.mul_add(actual.abs().max(numeric.abs()), 1e-19);
    assert!(
        (actual - numeric).abs() <= tolerance,
        "analytic {actual:e} numerical {numeric:e} tol {tolerance:e}"
    );
}

#[test]
fn independent_junction_partials_all_modes_and_polarities() -> TestResult {
    for pmos in [false, true] {
        let sign = if pmos { -1. } else { 1. };
        for mode in [0, 1, 2] {
            for heat in [false, true] {
                let setup = setup(pmos, heat, mode)?;
                let law = setup.prepare_junctions(0, 300.15)?;
                for temp in [240., 300.15, 380.] {
                    for (bs, bd) in [
                        (-0.3, -0.2),
                        (-0.05, 0.1),
                        (0.2, -0.1),
                        (0.4995, 0.5995),
                        (0.7995, 0.8995),
                    ] {
                        let (bs, bd) = (bs * sign, bd * sign);
                        let value = law.evaluate(temp, bs, bd)?;
                        let step = 1e-6;
                        let high = law.evaluate(temp, bs + step, bd + step)?;
                        let low = law.evaluate(temp, bs - step, bd - step)?;
                        let thermal_step = 1e-3;
                        let hot = law.evaluate(temp + thermal_step, bs, bd)?;
                        let cold = law.evaluate(temp - thermal_step, bs, bd)?;
                        let branches = |side: BsimSoiJunctionSide| {
                            [side.diffusion, side.recombination, side.tunneling]
                        };
                        for (point, upper, lower, hot, cold) in [
                            (
                                value.source,
                                high.source,
                                low.source,
                                hot.source,
                                cold.source,
                            ),
                            (value.drain, high.drain, low.drain, hot.drain, cold.drain),
                        ] {
                            for ((((current, a), b), c), d) in branches(point)
                                .into_iter()
                                .zip(branches(upper))
                                .zip(branches(lower))
                                .zip(branches(hot))
                                .zip(branches(cold))
                            {
                                check_derivative(
                                    current.conductance,
                                    (a.current - b.current) / (2. * step),
                                );
                                check_derivative(
                                    current.d_temperature,
                                    (c.current - d.current) / (2. * thermal_step),
                                );
                                if mode == 2 {
                                    assert_eq!(current.current, 0.);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires local ngspice 47 binary"]
fn native_asymmetric_junction_currents() -> TestResult {
    use std::{fs, process::Command};
    let mut count = 0;
    for pmos in [false, true] {
        let sign = if pmos { -1. } else { 1. };
        let polarity = if pmos { "pmos" } else { "nmos" };
        for heat in [false, true] {
            let setup = setup(pmos, heat, 0)?;
            for ambient_c in [-40., 85.] {
                let ambient = ambient_c + 273.15;
                let law = setup.prepare_junctions(0, ambient)?;
                for drain in [-0.2, 0.2] {
                    for body in [-0.1, 0.12] {
                        let vd = sign * drain;
                        let vb = sign * body;
                        let vg = sign * -0.5;
                        let rise = if heat { 10. } else { 0. };
                        let shmod = i32::from(heat);
                        let dir = tempfile::tempdir()?;
                        let deck = format!(
                            "BSIMSOI independent junction reference\nVd d 0 {vd}\nVg g 0 {vg}\nVe e 0 0\nVb b 0 {vb}\nVt t 0 {rise}\nM1 d g 0 e b t soi tnodeout l=0.5u w=4u nf=2 nseg=2 m=2.3 pd=4u ps=4u pdbcp=0.1u psbcp=0.2u\n.model soi {polarity} level=10 soimod=0 shmod={shmod} rth0=0.005 cth0=1e-6 tsi=0.1u\n+ {CARD}\n.temp {ambient_c}\n.options gmin=1e-18 reltol=1e-10 abstol=1e-17 vntol=1e-12\n.control\nset numdgt=17\nset wr_singlescale\nop\nwrdata out.dat @m1[ibs] @m1[ibd]\nquit\n.endc\n.end\n"
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
                        assert_eq!(native.len(), 3);
                        let actual = law.evaluate(ambient + rise, vb, vb - vd)?;
                        for (side, expected) in [
                            (actual.source, native[1] * sign),
                            (actual.drain, native[2] * sign),
                        ] {
                            let current = side.independent_current().current;
                            let tolerance =
                                3e-7_f64.mul_add(current.abs().max(expected.abs()), 1e-16);
                            assert!(
                                (current - expected).abs() <= tolerance,
                                "{polarity} heat={heat} T={ambient} Vd={vd} Vb={vb}: {current:e} vs {expected:e}; {side:?}"
                            );
                        }
                        count += 1;
                    }
                }
            }
        }
    }
    assert_eq!(count, 32);
    Ok(())
}
