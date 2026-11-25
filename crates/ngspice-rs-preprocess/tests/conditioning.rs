//! Compare exact coordinate conditioning against unconditioned native
//! integration and an independent linear small-signal circuit law.
use anyhow::{Context, Result, ensure};
use model_normalize_ngspice::conditioning::{
    ConditionedArtifact, ConditioningInput, condition_private_differentiators,
};
use ngspice_rs_preprocess::{Cancellation, InputFormat, Options, Request, SourceInput, prepare};
use std::{fs, path::Path, process::Command};

const CELL: &str = r".subckt cell a b
.param cnom=470n
Edrive n1 b a b 1
Vmeasure n1 n2 0
Vreverse n3 n2 DC 0
Cstorage n3 b 2
Gcap a b VALUE {cnom/2*cosh(V(a,b)/3)**(-1)*I(Vmeasure)}
Bother a b I={cnom/8*(I(Vmeasure)+I(Vreverse))}
.ends cell
";

fn condition(library: &str, root: &str) -> Result<ConditionedArtifact> {
    let mut outputs = condition_private_differentiators(
        &[
            ConditioningInput {
                filename: "deck.cir",
                bytes: root.as_bytes(),
            },
            ConditioningInput {
                filename: "model.lib",
                bytes: library.as_bytes(),
            },
        ],
        None,
    )?;
    ensure!(outputs.len() == 2);
    Ok(outputs.remove(1))
}

#[test]
fn private_loop_proof_and_coordinate_mapping() -> Result<()> {
    let result = condition(CELL, "* caller\nX1 a 0 cell\n.end\n")?;
    ensure!(result.scalings.len() == 1, "{:?}", result.scalings);
    let scale = &result.scalings[0];
    assert_eq!(scale.scope, ["cell"]);
    assert_eq!(scale.capacitor, "Cstorage");
    assert_eq!(scale.voltage_branches.len(), 3);
    // The second feedback expression cancels its opposite-signed probes;
    // the scale therefore comes from the nominal first expression.
    assert!((scale.factor - 235e-9).abs() < 1e-18);
    let text = std::str::from_utf8(&result.bytes)?;
    let cap = text
        .lines()
        .find(|line| line.starts_with("Cstorage "))
        .context("missing capacitor")?;
    let value = cap
        .split_ascii_whitespace()
        .nth(3)
        .context("missing cap value")?
        .parse::<f64>()?;
    assert!((value - 470e-9).abs() < 1e-18);
    assert!(text.contains("I(Vmeasure)/"));
    assert!(text.contains("I(Vreverse)/"));
    assert_eq!(text.lines().count(), CELL.lines().count());
    Ok(())
}

#[test]
fn exposed_state_or_untracked_current_reference_is_not_changed() -> Result<()> {
    let variants = [
        CELL.replace(".ends cell", "Rleak n2 b 1G\n.ends cell"),
        CELL.replace(".subckt cell a b", ".subckt cell a b n2"),
        CELL.replace(".param cnom=470n", ".global n2\n.param cnom=470n"),
        CELL.replace(".ends cell", "Fother a b Vmeasure 1m\n.ends cell"),
        CELL.replace("I(Vmeasure)+I(Vreverse)", "@Vmeasure[i]+I(Vreverse)"),
        CELL.replace("Cstorage n3 b 2", "Cstorage n3 b 2 Rser=0.1"),
        CELL.replace("Vmeasure n1 n2 0", "Vmeasure n1 n2 0 AC 1"),
        CELL.replace("cnom=470n", "cnom=unknown"),
    ];
    for source in variants {
        let output = condition(&source, "* caller\nX1 a 0 cell\n.end\n")?;
        ensure!(output.scalings.is_empty(), "{source}");
        assert_eq!(output.bytes, source.as_bytes());
    }
    for directive in [
        ".save i(v.x1.vmeasure)",
        ".save @c.x1.cstorage[i]",
        ".global n3",
        ".func hidden_feedback(x) {x*I(Vmeasure)}",
        ".options rshunt=1e12",
        ".options cshunt=1p",
    ] {
        let root = format!("* caller\nX1 a 0 cell\n{directive}\n.end\n");
        let output = condition(CELL, &root)?;
        assert!(output.scalings.is_empty(), "{directive}");
        assert_eq!(output.bytes, CELL.as_bytes());
    }
    Ok(())
}

fn native_run(dir: &Path, model: &[u8], source: &str) -> Result<Vec<Vec<f64>>> {
    fs::write(dir.join("model.lib"), model)?;
    fs::write(dir.join("test.cir"), source)?;
    let output = Command::new("ngspice")
        .current_dir(dir)
        .args(["-b", "-n", "test.cir"])
        .output()?;
    let log = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    ensure!(
        output.status.success() && !log.to_ascii_lowercase().contains("error:"),
        "{log}"
    );
    fs::read_to_string(dir.join("wave.dat"))?
        .lines()
        .map(|line| {
            line.split_ascii_whitespace()
                .map(str::parse::<f64>)
                .collect::<std::result::Result<Vec<_>, _>>()
                .context("bad native trace")
        })
        .collect()
}

#[test]
fn physical_voltage_current_and_small_signal_derivative_match() -> Result<()> {
    let original = tempfile::tempdir()?;
    let scaled = tempfile::tempdir()?;
    let source = r"* differentiator
Vdrive supply 0 PULSE(0 2 10u 5u 5u 50u 100u) AC 1
Rsource supply a 100
X1 a 0 cell
.include model.lib
.control
set numdgt=17
set wr_singlescale
tran 0.1u 200u 0 0.05u
linearize v(a) i(vdrive) i(v.x1.vmeasure)
wrdata wave.dat v(a) i(vdrive) i(v.x1.vmeasure)
quit
.endc
.end
";
    // The source program explicitly reads a scaled current *for verification*.
    // It is not part of the admitted electrical artifact; the mapping lets a
    // caller recover physical current when requesting such a diagnostic.
    let result = condition(CELL, "* differentiator\nX1 a 0 cell\n.end\n")?;
    let factor = result.scalings.first().context("no scaling")?.factor;
    let a = native_run(original.path(), CELL.as_bytes(), source)?;
    let b = native_run(scaled.path(), &result.bytes, source)?;
    ensure!(
        a.len() == b.len() && a.len() > 1000,
        "shape {} {}",
        a.len(),
        b.len()
    );
    for (a, b) in a.iter().zip(&b) {
        ensure!(a.len() == 4 && b.len() == 4);
        ensure!((a[0] - b[0]).abs() < 1e-15, "time grid mismatch");
        ensure!(
            (a[1] - b[1]).abs() <= 1e-4_f64.mul_add(a[1].abs(), 1e-5),
            "voltage {} {}",
            a[1],
            b[1]
        );
        ensure!(
            (a[2] - b[2]).abs() <= 1e-4_f64.mul_add(a[2].abs(), 1e-7),
            "terminal current {} {}",
            a[2],
            b[2]
        );
        ensure!(
            (a[3] - b[3] / factor).abs() <= 1e-3_f64.mul_add(a[3].abs(), 0.1),
            "internal coordinate {} {}",
            a[3],
            b[3] / factor
        );
    }
    // At zero bias the differential capacitance is exactly 470 nF.
    let ac = r"* small signal
Vdrive supply 0 0 AC 1
Rsource supply a 100
X1 a 0 cell
.include model.lib
.control
set numdgt=17
set wr_singlescale
ac lin 9 1k 9k
wrdata wave.dat real(v(a)) imag(v(a)) real(i(vdrive)) imag(i(vdrive))
quit
.endc
.end
";
    for (dir, bytes) in [
        (original.path(), CELL.as_bytes()),
        (scaled.path(), result.bytes.as_slice()),
    ] {
        let rows = native_run(dir, bytes, ac)?;
        assert_eq!(rows.len(), 9);
        for row in rows {
            let x = 2. * std::f64::consts::PI * row[0] * 100. * 470e-9;
            let re = 1. / x.mul_add(x, 1.);
            let im = -x * re;
            for (actual, expected) in row[1..].iter().zip([re, im, (re - 1.) / 100., im / 100.]) {
                ensure!(
                    (actual - expected).abs() < 1e-9,
                    "AC: {actual} != {expected}"
                );
            }
        }
    }
    Ok(())
}

#[test]
fn facade_conditions_closed_translated_bundle_and_retains_input() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("root.net");
    let lib = dir.path().join("cell.lb");
    let source = b"* closed graph\nV1 p 0 1\nR1 p a 100\nX1 a 0 cell\n.end\n";
    fs::write(&root, source)?;
    fs::write(&lib, CELL)?;
    let request = Request {
        format: InputFormat::SimetrixDeck,
        input: SourceInput::File(root.clone()),
        libraries: vec![ngspice_rs_preprocess::LibraryInput::Source(
            SourceInput::File(lib.clone()),
        )],
        search_paths: Vec::new(),
    };
    let bundle = prepare(&request, &Options::default(), &Cancellation::default())?;
    let count = bundle
        .artifacts()
        .map(|artifact| artifact.current_scalings.len())
        .sum::<usize>();
    assert_eq!(count, 1);
    assert!(
        bundle.normalization.contains(
            &model_normalize_ngspice::NormalizationPass::PrivateDifferentiatorConditioning
        )
    );
    let unconditioned = prepare(
        &request,
        &Options {
            conditioning: ngspice_rs_preprocess::ConditioningPolicy::Disabled,
            ..Options::default()
        },
        &Cancellation::default(),
    )?;
    assert!(
        unconditioned
            .artifacts()
            .all(|artifact| artifact.current_scalings.is_empty())
    );
    assert!(
        !unconditioned.normalization.contains(
            &model_normalize_ngspice::NormalizationPass::PrivateDifferentiatorConditioning
        )
    );
    assert_eq!(fs::read(root)?, source);
    assert_eq!(fs::read(lib)?, CELL.as_bytes());
    Ok(())
}

#[test]
fn grounded_loops_ignore_numeric_literals_outside_their_scope() -> Result<()> {
    let model = CELL
        .replace("Edrive n1 b a b 1", "Edrive n1 0 a b 1")
        .replace("Cstorage n3 b 2", "Cstorage n3 0 2");
    let root = "* grounded\nV1 a 0 0.25\nX1 a 0 cell\n.end\n";
    let result = condition(&model, root)?;
    assert_eq!(result.scalings.len(), 1);
    Ok(())
}

#[test]
fn driver_current_can_be_used_without_a_separate_sensor() -> Result<()> {
    let source = ".subckt cell a b\nEdriver aux b a b 1\nCstate aux b 1 IC=0\nBcap a b I={-1u*I(Edriver)}\n.ends cell\n";
    let output = condition(source, "* caller\nX1 a 0 cell\n.end\n")?;
    assert_eq!(output.scalings.len(), 1);
    assert!((output.scalings[0].factor - 1e-6).abs() < 1e-18);
    let text = std::str::from_utf8(&output.bytes)?;
    assert!(text.contains("IC=0"));
    assert!(text.contains("I(Edriver)/"));
    Ok(())
}
