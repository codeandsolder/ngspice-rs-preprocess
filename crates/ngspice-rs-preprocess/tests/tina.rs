//! Independent first-order circuit oracle for TINA plaintext exports.
use anyhow::{Context, Result};
use ngspice_rs_preprocess::{
    Cancellation, InputFormat, Options, Provenance, Request, SourceInput, prepare,
};
use std::{fs, path::Path, process::Command};

fn prepared(format: InputFormat, path: &Path) -> Result<ngspice_rs_preprocess::PreparedBundle> {
    prepare(
        &Request {
            format,
            input: SourceInput::File(path.to_owned()),
            libraries: Vec::new(),
            search_paths: Vec::new(),
        },
        &Options::default(),
        &Cancellation::default(),
    )
}

const LIBRARY: &str = include_str!("fixtures/tina/filter.lib");
const DECK: &str = include_str!("fixtures/tina/deck.cir");

#[test]
fn tina_deck_and_library_keep_formals_local_parameters_and_provenance() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let library = directory.path().join("filter.lib");
    let deck = directory.path().join("deck.cir");
    fs::write(&library, LIBRARY)?;
    fs::write(&deck, DECK)?;
    let include = prepared(InputFormat::TinaLibrary, &library)?;
    assert_eq!(include.primary.filename, "model.lib");
    let text = std::str::from_utf8(&include.primary.bytes)?;
    assert!(!text.lines().any(|line| line.eq_ignore_ascii_case(".end")));
    let result = prepared(InputFormat::TinaDeck, &deck)?;
    let source = std::str::from_utf8(&result.primary.bytes)?;
    assert!(source.starts_with("TINA export style"));
    assert!(source.ends_with(".end\n"));
    assert!(source.lines().any(|line| line.split_ascii_whitespace().eq([
        ".subckt",
        "filter",
        "input",
        "output",
        "PARAMS:",
        "rbase=500"
    ])));
    let Provenance::Native { lines } = &result.primary.provenance else {
        anyhow::bail!("missing source map");
    };
    assert_eq!(lines.len(), source.lines().count());
    let library = library.canonicalize()?;
    for (text, origin) in source.lines().zip(lines) {
        if text.starts_with(".param scale={2}") || text.starts_with(".subckt filter") {
            assert_eq!(origin.source, library);
            assert_eq!(origin.line, 2);
        }
    }
    assert!(source.contains(".param scale={2}"));
    assert_eq!(result.format, InputFormat::TinaDeck);
    Ok(())
}

fn rows(path: &Path) -> Result<Vec<Vec<f64>>> {
    fs::read_to_string(path)?
        .lines()
        .map(|line| {
            line.split_whitespace()
                .map(|value| value.parse::<f64>().map_err(Into::into))
                .collect()
        })
        .collect()
}

#[test]
fn normalized_tina_export_matches_op_dc_ac_and_transient_laws() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let library = directory.path().join("filter.lib");
    let deck = directory.path().join("deck.cir");
    fs::write(&library, LIBRARY)?;
    fs::write(&deck, DECK)?;
    let result = prepared(InputFormat::TinaDeck, &deck)?;
    let native = std::str::from_utf8(&result.primary.bytes)?;
    let native = native.strip_suffix(".end\n").context("missing .end")?;
    let execution = format!(
        "{native}.control\nset numdgt=17\nset wr_singlescale\nop\nwrdata op.dat v(out) v(other)\ndc Vinput 0 1 0.25\nwrdata dc.dat v(out) v(other)\nac lin 3 1000 3000\nwrdata ac.dat v(out) v(other)\ntran 1u 200u 0 1u uic\nwrdata tran.dat v(out) v(other)\nquit\n.endc\n.end\n"
    );
    fs::write(directory.path().join("native.cir"), execution)?;
    let simulation = Command::new("ngspice")
        .current_dir(directory.path())
        .args(["-b", "-n", "native.cir"])
        .output()?;
    assert!(simulation.status.success(), "{simulation:?}");
    let op = rows(&directory.path().join("op.dat"))?;
    let [_, out, other] = op.first().context("missing OP")?.as_slice() else {
        anyhow::bail!("unexpected OP columns");
    };
    assert!((out - 2.0).abs() < 1e-10 && (other - 2.0).abs() < 1e-10);
    let dc = rows(&directory.path().join("dc.dat"))?;
    assert_eq!(dc.len(), 5);
    for row in dc {
        let [input, out, other] = row.as_slice() else {
            anyhow::bail!("unexpected DC columns");
        };
        assert!((out - 2.0 * input).abs() < 1e-10);
        assert!((other - 2.0 * input).abs() < 1e-10);
    }
    let ac = rows(&directory.path().join("ac.dat"))?;
    assert_eq!(ac.len(), 3);
    for row in ac {
        let [frequency, real1, imag1, real2, imag2] = row.as_slice() else {
            anyhow::bail!("unexpected AC columns");
        };
        for (tau, real, imag) in [(1e-4, real1, imag1), (2e-4, real2, imag2)] {
            let x = std::f64::consts::TAU * frequency * tau;
            let den = x.mul_add(x, 1.0);
            assert!((real - 2.0 / den).abs() < 1e-10);
            assert!((imag + 2.0 * x / den).abs() < 1e-10);
        }
    }
    let tran = rows(&directory.path().join("tran.dat"))?;
    let [time, out, other] = tran.last().context("missing transient rows")?.as_slice() else {
        anyhow::bail!("unexpected transient columns");
    };
    assert!((time - 2e-4).abs() < 1e-12);
    for (tau, actual) in [(1e-4_f64, out), (2e-4_f64, other)] {
        // Source has a finite 1 ns ramp; this is the exact response after it.
        let rise = 1e-9;
        let ramp_factor = (rise / tau).exp_m1() / (rise / tau);
        let expected = 2.0 * (1.0 - (-time / tau).exp() * ramp_factor);
        assert!((actual - expected).abs() < 2e-4, "{actual} vs {expected}");
    }
    Ok(())
}

#[test]
fn tina_container_is_not_a_netlist() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("container.tsm");
    fs::write(&path, b"PK\x03\x04archive\x00")?;
    let error = prepared(InputFormat::TinaLibrary, &path)
        .err()
        .context("accepted binary")?;
    assert!(format!("{error:#}").contains("plaintext SPICE netlist"));
    Ok(())
}

#[test]
fn tina_parameterized_table_preserves_instance_scope() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let deck = directory.path().join("table.cir");
    fs::write(
        &deck,
        "TINA table scope reference\n.param scale=10 gain=100\nVinput in 0 0\n.subckt mapper a b PARAMS : gain=2 .param scale=500m\nEtab b 0 TABLE {V(a)*scale} = (0,0)(1,{gain})(2,{gain+1})\n.ends mapper\nX1 in out mapper PARAMS : gain=4\nX2 in other mapper\n.control\nset numdgt=17\nset wr_singlescale\ndc Vinput 0.5 3.5 1\nwrdata dc.dat v(out) v(other)\nquit\n.endc\n.end\n",
    )?;
    let result = prepared(InputFormat::TinaDeck, &deck)?;
    let native = directory.path().join("native.cir");
    fs::write(&native, &result.primary.bytes)?;
    let simulation = Command::new("ngspice")
        .current_dir(directory.path())
        .args(["-b", "-n", "native.cir"])
        .output()?;
    assert!(simulation.status.success(), "{simulation:?}");
    let dc = rows(&directory.path().join("dc.dat"))?;
    assert_eq!(dc.len(), 4);
    for row in dc {
        let [input, out, other] = row.as_slice() else {
            anyhow::bail!("unexpected DC columns");
        };
        let x = input * 0.5;
        for (gain, actual) in [(4.0, out), (2.0, other)] {
            let expected = if x < 1.0 { x * gain } else { gain + x - 1.0 };
            assert!((actual - expected).abs() < 1e-10, "{actual} vs {expected}");
        }
    }
    Ok(())
}
