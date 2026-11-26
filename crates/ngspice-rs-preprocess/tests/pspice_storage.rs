//! Independent integrated-charge witness, not a PSpice dialect oracle.
use anyhow::{Context, Result, ensure};
use ngspice_rs_preprocess::passive_storage::PolynomialStorage;
use std::{fs, path::Path, process::Command};

fn table<const N: usize>(path: &Path) -> Result<Vec<[f64; N]>> {
    fs::read_to_string(path)?
        .lines()
        .skip(1)
        .map(|line| {
            line.split_whitespace()
                .map(str::parse)
                .collect::<std::result::Result<Vec<f64>, _>>()?
                .try_into()
                .map_err(|_| anyhow::anyhow!("unexpected witness table shape"))
        })
        .collect()
}

fn close(actual: f64, expected: f64, relative: f64, absolute: f64) {
    assert!(
        (actual - expected).abs() <= relative * expected.abs() + absolute,
        "{actual} != {expected}"
    );
}

fn integral(rows: &[[f64; 5]], ordinate: impl Fn(&[f64; 5]) -> f64) -> f64 {
    rows.windows(2)
        .map(|pair| {
            let [a, b] = pair else { unreachable!() };
            (b[0] - a[0]) * (ordinate(a) + ordinate(b)) * 0.5
        })
        .sum()
}

#[test]
fn independent_charge_flux_circuit_matches_prepared_polynomial() -> Result<()> {
    // Explicit mathematical primitives, independent of model normalization
    // and Rust's polynomial kernel. The flux witness uses the dual
    // charge circuit: its voltage represents signed inductor current.
    let deck = r"Independent polynomial storage witness
Vdrive v 0 DC -2 AC 1 PWL(0 -2 1m 2 2m 2)
Cq v 0 Q='2n*(v(v)+0.1*v(v)*v(v)+0.01*v(v)*v(v)*v(v))'
Iin 0 i DC -0.5 AC 1 PWL(0 -0.5 1m 1.5 2m 1.5)
Vsense i n 0
Bphi aux 0 V='i(vsense)'
Cphi aux 0 Q='3m*(v(aux)-0.05*v(aux)*v(aux)+0.02*v(aux)*v(aux)*v(aux)/3)'
Bvoltage n 0 V='-i(Bphi)'
.control
set wr_singlescale
set wr_vecnames
op
wrdata op.dat v(v) i(vdrive) i(vsense) v(n)
ac lin 3 1k 3k
wrdata ac.dat i(vdrive) v(n)
tran 1u 1m 0 1u
wrdata tran.dat v(v) i(vdrive) i(vsense) v(n)
quit
.endc
.end
";
    let dir = tempfile::tempdir()?;
    fs::write(dir.path().join("deck.cir"), deck)?;
    let output = match Command::new("ngspice")
        .args(["-n", "-b", "deck.cir"])
        .current_dir(dir.path())
        .output()
    {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    ensure!(
        output.status.success(),
        "ngspice: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let capacitor = PolynomialStorage::new(2e-9, 0.2, 0.03)?;
    let inductor = PolynomialStorage::new(3e-3, -0.1, 0.02)?;
    let op = table::<5>(&dir.path().join("op.dat"))?;
    assert_eq!(op.len(), 1);
    let [_, v, c_current, i, l_voltage] = op[0];
    close(v, -2., 0., 1e-12);
    close(i, -0.5, 0., 1e-12);
    close(c_current, 0., 0., 1e-12);
    close(l_voltage, 0., 0., 1e-12);
    let ac = table::<5>(&dir.path().join("ac.dat"))?;
    assert_eq!(ac.len(), 3);
    for [f, re_c, im_c, re_l, im_l] in ac {
        let omega = std::f64::consts::TAU * f;
        close(re_c, 0., 0., 1e-12);
        close(re_l, 0., 0., 1e-12);
        close(
            im_c,
            -omega * capacitor.evaluate(v)?.differential,
            1e-7,
            1e-12,
        );
        close(
            im_l,
            omega * inductor.evaluate(i)?.differential,
            1e-7,
            1e-10,
        );
    }
    let tr = table::<5>(&dir.path().join("tran.dat"))?;
    assert!(tr.len() >= 1000);
    let first = tr.first().context("missing initial state")?;
    let last = tr.last().context("missing final state")?;
    close(first[1], -2., 0., 1e-12);
    close(first[3], -0.5, 0., 1e-12);
    close(last[1], 2., 0., 1e-9);
    close(last[3], 1.5, 0., 1e-9);
    // The initial OP row precedes the ramp, and the external simulator
    // reports finite-step samples. Integrate those samples independently.
    close(
        integral(&tr, |r| -r[2]),
        capacitor.evaluate(last[1])?.stored - capacitor.evaluate(first[1])?.stored,
        1e-5,
        1e-15,
    );
    close(
        integral(&tr, |r| r[4]),
        inductor.evaluate(last[3])?.stored - inductor.evaluate(first[3])?.stored,
        1e-5,
        1e-12,
    );
    close(
        integral(&tr, |r| -r[1] * r[2]),
        capacitor.energy(last[1])? - capacitor.energy(first[1])?,
        5e-5,
        1e-15,
    );
    close(
        integral(&tr, |r| r[3] * r[4]),
        inductor.energy(last[3])? - inductor.energy(first[3])?,
        1e-5,
        1e-12,
    );
    Ok(())
}
