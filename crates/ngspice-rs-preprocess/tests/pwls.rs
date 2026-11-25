//! Synthetic waveform regressions against measured `SIMetrix` 9.40d behavior.
use anyhow::{Context, Result, ensure};
use ngspice_rs_preprocess::{Cancellation, InputFormat, Options, Request, SourceInput, prepare};
use std::{fs, process::Command};

const DECK: &str = "* PWLS independent sources
Vbase a 0 PWLS PUL V1=2 V2=6 FREQ=1k END
Vgated b 0 PWLS PUL V1=2 V2=6 FREQ=1k DELAY=750u END
Vphase c 0 PWLS PUL V1=2 V2=6 FREQ=1k DELAY=750u OFF_UNTIL_DELAY=0 END
Vtriangle d 0 PWLS PUL V1=2 V2=6 PERIOD=1m RISE=100u FALL=200u WIDTH=0 END
Ishape 0 e PWLS PUL V1=2m V2=6m FREQ=1k RISE=100u FALL=200u WIDTH=300u END
R1 a 0 1k
R2 b 0 1k
R3 c 0 1k
R4 d 0 1k
R5 e 0 1k
X1 f 0 GEN params: freq=1k angle=-90 usephase=1
X2 g 0 GEN params: freq=2k angle=0 usephase=0
R6 f 0 1k
R7 g 0 1k
.subckt GEN p n params: freq=100 angle=0 usephase=0
Vsrc p n PWLS PUL V1={1+1} V2=6 FREQ={freq} DELAY={IFF(usephase,angle/360/freq,0)} END
.ends
.end
";

fn read_data(path: &std::path::Path) -> Result<Vec<Vec<f64>>> {
    fs::read_to_string(path)?
        .lines()
        .map(|line| {
            line.split_whitespace()
                .map(|value| value.parse::<f64>().map_err(Into::into))
                .collect()
        })
        .collect()
}

fn verify(actual: f64, expected: f64) -> Result<()> {
    ensure!(
        (actual - expected).abs() <= 2e-6 * expected.abs().max(1.),
        "{actual} != {expected}"
    );
    Ok(())
}

fn shape(time: f64, period: f64, rise: f64, width: f64, fall: f64, shift: f64) -> f64 {
    if time < shift {
        return 2.;
    }
    let phase = (time - shift).rem_euclid(period);
    if phase < rise {
        (4. * phase / rise) + 2.
    } else if phase <= rise + width {
        6.
    } else if phase < rise + width + fall {
        6. - 4. * (phase - rise - width) / fall
    } else {
        2.
    }
}

#[test]
fn pwls_linked_deck_preserves_vendor_voltage_current_and_phase() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let source = dir.path().join("pulse.net");
    fs::write(&source, DECK)?;
    let request = Request {
        format: InputFormat::SimetrixDeck,
        input: SourceInput::File(source),
        libraries: Vec::new(),
        search_paths: Vec::new(),
    };
    let bundle = prepare(&request, &Options::default(), &Cancellation::default())?;
    for artifact in &bundle.companions {
        fs::write(dir.path().join(&artifact.filename), &artifact.bytes)?;
    }
    let mut deck = bundle
        .primary
        .bytes
        .strip_suffix(b".end\n")
        .context("missing deck terminator")?
        .to_vec();
    deck.extend_from_slice(b".control\nset numdgt=17\nset wr_singlescale\nop\nwrdata op.dat v(a) v(b) v(c) v(d) v(e) v(f) v(g) i(vbase)\ntran 0.1u 2m 0 0.1u\nwrdata tran.dat v(a) v(b) v(c) v(d) v(e) v(f) v(g) i(vbase)\nquit\n.endc\n.end\n");
    fs::write(dir.path().join("pulse.cir"), deck)?;
    let result = Command::new("ngspice")
        .current_dir(dir.path())
        .args(["-b", "-n", "pulse.cir"])
        .output()?;
    let log = format!(
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    ensure!(
        result.status.success() && !log.to_ascii_lowercase().contains("error:"),
        "{log}"
    );
    let op = read_data(&dir.path().join("op.dat"))?;
    let row = op.first().context("missing OP row")?;
    ensure!(row.len() == 9, "unexpected OP shape");
    for (&actual, expected) in row[1..].iter().zip([2., 2., 6., 2., 2., 6., 2., -0.002]) {
        verify(actual, expected)?;
    }
    let rows = read_data(&dir.path().join("tran.dat"))?;
    ensure!(rows.len() > 10_000, "missing transient samples");
    for row in rows {
        let [
            time,
            base,
            gated,
            phase,
            triangle,
            current,
            nested_phase,
            nested_freq,
            branch,
        ] = row.as_slice()
        else {
            anyhow::bail!("unexpected transient row");
        };
        let default = |period, shift| {
            shape(
                *time,
                period,
                period / 1000.,
                period * 0.499,
                period / 1000.,
                shift,
            )
        };
        for (actual, expected) in [
            (*base, default(1e-3, 0.)),
            (*gated, default(1e-3, 750e-6)),
            (*phase, default(1e-3, -250e-6)),
            (*triangle, shape(*time, 1e-3, 100e-6, 0., 200e-6, 0.)),
            (*current, shape(*time, 1e-3, 100e-6, 300e-6, 200e-6, 0.)),
            (*nested_phase, default(1e-3, -250e-6)),
            (*nested_freq, default(0.5e-3, 0.)),
            (*branch, -*base / 1000.),
        ] {
            verify(actual, expected).with_context(|| format!("time={time}"))?;
        }
    }
    Ok(())
}
