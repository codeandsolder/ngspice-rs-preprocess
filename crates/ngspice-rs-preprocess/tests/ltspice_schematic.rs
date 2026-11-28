//! Headless schematic import through the ordinary source preparation API.
use anyhow::{Context, Result, ensure};
use model_normalize_ngspice::linked_source::{LinkLimits, RootKind, SourceDialect, SourceLinker};
use model_preprocess_command::{AdmissionConfig, AdmittedSource, SourceSession};
use ngspice_rs_preprocess::{
    Cancellation, InputFormat, Options, PreparedBundle, Provenance, Request, SourceInput, prepare,
};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

const RES: &str = "Version 4\nSymbolType CELL\nSYMATTR Prefix R\nSYMATTR Value 1k\nPIN 0 16 NONE 0\nPINATTR SpiceOrder 2\nPIN 0 0 NONE 0\nPINATTR SpiceOrder 1\n";
const CHILD: &str = "Version 4\nSymbolType BLOCK\nPIN 0 0 NONE 0\nPINATTR PinName in\nPINATTR SpiceOrder 1\nPIN 0 16 NONE 0\nPINATTR PinName out\nPINATTR SpiceOrder 2\n";

fn prepared(path: &Path) -> Result<PreparedBundle> {
    prepared_with_search(path, Vec::new())
}

fn prepared_with_search(path: &Path, search_paths: Vec<PathBuf>) -> Result<PreparedBundle> {
    prepare(
        &Request {
            format: InputFormat::LtspiceSchematic,
            input: SourceInput::File(path.to_owned()),
            libraries: Vec::new(),
            search_paths,
        },
        &Options::default(),
        &Cancellation::default(),
    )
}

#[test]
fn source_maps_retain_child_and_library_lines() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("top.asc");
    let child = dir.path().join("child.asc");
    let lib = dir.path().join("mod.lib");
    fs::write(dir.path().join("RES.ASY"), RES)?;
    fs::write(dir.path().join("child.asy"), CHILD)?;
    fs::write(&lib, ".subckt ext a b\nRext a b 2k\n.ends ext\n")?;
    fs::write(
        dir.path().join("mod.asy"),
        RES.replace("Prefix R", "Prefix X")
            .replace("Value 1k", "Value ext\nSYMATTR ModelFile mod.lib"),
    )?;
    fs::write(
        &child,
        "Version 4\nSHEET 1 100 100\nFLAG 0 0 in\nIOPIN 0 0 In\nFLAG 0 16 out\nIOPIN 0 16 Out\nSYMBOL res 0 0 R0\nSYMATTR InstName R1\nSYMATTR Value 2k\n",
    )?;
    fs::write(
        &root,
        "Version 4\nSHEET 1 100 100\nFLAG 0 0 a\nFLAG 0 16 b\nFLAG 16 0 b\nFLAG 16 16 0\nSYMBOL child 0 0 R0\nSYMATTR InstName X1\nSYMBOL mod 16 0 R0\nSYMATTR InstName X2\nTEXT 0 0 Left 2 !.op\n",
    )?;
    let bundle = prepared(&root)?;
    let output = std::str::from_utf8(&bundle.primary.bytes)?;
    let Provenance::Native { lines, .. } = &bundle.primary.provenance else {
        anyhow::bail!("missing provenance")
    };
    assert_eq!(lines.len(), output.lines().count());
    for (needle, source, expected) in [
        ("XX1 a b __lt_sheet_1", &root, 7),
        ("XX2 b 0 ext", &root, 9),
        ("R1 in out 2k", &child, 7),
        ("Rext a b 2k", &lib, 2),
    ] {
        let index = output
            .lines()
            .position(|line| line == needle)
            .with_context(|| format!("missing {needle} in {output}"))?;
        assert_eq!(lines[index].source, source.canonicalize()?);
        assert_eq!(lines[index].line, expected);
    }
    let plan = bundle
        .analysis_plan
        .as_ref()
        .context("missing schematic analysis plan")?;
    let request = plan
        .native_requests()
        .next()
        .context("missing schematic operating point")?;
    assert_eq!(request.command, ".OP");
    assert_eq!(
        request.origin,
        ngspice_rs_preprocess::analysis_plan::DirectiveOrigin::SourceOccurrence {
            path: root.canonicalize()?,
            first_line: 11,
            last_line: 11,
            visit: 0,
            scope: 0,
        }
    );
    assert!(!output.lines().any(|line| line == ".op"));
    Ok(())
}

fn numerical_data(dir: &Path, deck: &[u8], name: &str) -> Result<Vec<Vec<f64>>> {
    let mut deck = deck
        .strip_suffix(b".end\n")
        .context("missing deck terminator")?
        .to_vec();
    deck.extend_from_slice(format!(".control\nset numdgt=17\nset wr_singlescale\nop\nwrdata {name}-op.dat v(out)\ndc V1 0 2 0.125\nwrdata {name}-dc.dat v(out)\nac lin 3 100 300\nwrdata {name}-ac.dat v(out)\ntran 1u 4m 0 10u\nwrdata {name}-tran.dat v(out)\nquit\n.endc\n.end\n").as_bytes());
    let filename = format!("{name}.cir");
    fs::write(dir.join(&filename), deck)?;
    let result = Command::new("ngspice")
        .current_dir(dir)
        .args(["-b", "-n", &filename])
        .output()?;
    let log = format!(
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    ensure!(
        result.status.success() && !log.to_ascii_lowercase().contains("error"),
        "{log}"
    );
    ["op", "dc", "ac", "tran"]
        .into_iter()
        .map(|analysis| {
            let text = fs::read_to_string(dir.join(format!("{name}-{analysis}.dat")))?;
            text.split_whitespace()
                .map(|value| Ok(value.parse::<f64>()?))
                .collect()
        })
        .collect()
}

#[test]
fn imported_hierarchical_rc_matches_independent_native_deck() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("filter.asc");
    fs::write(dir.path().join("res.asy"), RES)?;
    fs::write(
        dir.path().join("cap.asy"),
        RES.replace("Prefix R", "Prefix C")
            .replace("Value 1k", "Value 1u"),
    )?;
    fs::write(
        dir.path().join("voltage.asy"),
        RES.replace("Prefix R", "Prefix V")
            .replace("Value 1k", "Value PULSE(0 1 1u 1u 1u 2m 4m) AC 1"),
    )?;
    fs::write(dir.path().join("child.asy"), CHILD)?;
    fs::write(
        dir.path().join("child.asc"),
        "Version 4\nSHEET 1 100 100\nFLAG 0 0 in\nIOPIN 0 0 In\nFLAG 0 16 out\nIOPIN 0 16 Out\nSYMBOL res 0 0 R0\nSYMATTR InstName R1\n",
    )?;
    fs::write(
        &root,
        "Version 4\nSHEET 1 100 100\nFLAG 0 0 in\nFLAG 0 16 0\nFLAG 32 0 in\nFLAG 32 16 out\nFLAG 64 0 out\nFLAG 64 16 0\nSYMBOL voltage 0 0 R0\nSYMATTR InstName V1\nSYMBOL child 32 0 R0\nSYMATTR InstName X1\nSYMBOL cap 64 0 M0\nSYMATTR InstName C1\nTEXT 0 0 Left 2 !.tran 1u 4m 0 10u\n",
    )?;
    let bundle = prepared(&root)?;
    let reference = b"reference\nV1 in 0 PULSE(0 1 1u 1u 1u 2m 4m) AC 1\nX1 in out cell\nC1 out 0 1u\n.subckt cell in out\nR1 in out 1k\n.ends cell\n.end\n";
    let measured = numerical_data(dir.path(), &bundle.primary.bytes, "imported")?;
    let expected = numerical_data(dir.path(), reference, "reference")?;
    for (observed, expected) in measured.iter().zip(&expected) {
        ensure!(!observed.is_empty(), "empty numerical trace");
        assert_eq!(observed.len(), expected.len());
        for (&a, &b) in observed.iter().zip(expected) {
            ensure!((a - b).abs() <= 1e-10 * b.abs().max(1.), "{a} != {b}");
        }
    }
    Ok(())
}

#[test]
fn imported_semiconductor_terminal_and_library_bindings_match_native_deck() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("devices.asc");
    let lib = dir.path().join("lib");
    let cmp = lib.join("cmp");
    fs::create_dir_all(&cmp)?;
    fs::write(dir.path().join("res.asy"), RES)?;
    fs::write(
        dir.path().join("voltage.asy"),
        RES.replace("Prefix R", "Prefix V")
            .replace("Value 1k", "Value 0"),
    )?;
    fs::write(
        dir.path().join("diode.asy"),
        RES.replace("Prefix R", "Prefix D")
            .replace("Value 1k", "Value D"),
    )?;
    let three = format!("{RES}PIN 16 0 NONE 0\nPINATTR SpiceOrder 3\n");
    fs::write(
        dir.path().join("npn.asy"),
        three
            .replace("Prefix R", "Prefix QN")
            .replace("Value 1k", "Value NPN"),
    )?;
    fs::write(
        dir.path().join("nmos.asy"),
        three
            .replace("Prefix R", "Prefix MN")
            .replace("Value 1k", "Value NMOS"),
    )?;
    // Deliberately authored compact models, not copied installation data.
    fs::write(cmp.join("standard.dio"), ".model dm D(IS=2e-14 N=1.1)\n")?;
    fs::write(
        cmp.join("standard.bjt"),
        ".model qm NPN(IS=3e-15 BF=120 VAF=100)\n",
    )?;
    fs::write(
        cmp.join("standard.mos"),
        ".model mm NMOS(LEVEL=1 VTO=.65 KP=.001)\n",
    )?;
    fs::write(
        &root,
        concat!(
            "Version 4\nSHEET 1 800 600\n",
            "FLAG 0 0 in\nFLAG 0 16 0\n",
            "FLAG 32 0 vcc\nFLAG 32 16 0\n",
            "FLAG 64 0 vcc\nFLAG 64 16 out\n",
            "FLAG 96 0 in\nFLAG 96 16 base\n",
            "FLAG 128 0 out\nFLAG 128 16 base\nFLAG 144 0 0\n",
            "FLAG 160 0 out\nFLAG 160 16 in\nFLAG 176 0 0\n",
            "FLAG 192 0 0\nFLAG 192 16 out\n",
            "SYMBOL voltage 0 0 R0\nSYMATTR InstName V1\nSYMATTR Value PULSE(0 1.2 1u 1u 1u 2m 4m) AC 1\n",
            "SYMBOL voltage 32 0 R0\nSYMATTR InstName V2\nSYMATTR Value 5\n",
            "SYMBOL res 64 0 R0\nSYMATTR InstName R1\nSYMATTR Value 2k\n",
            "SYMBOL res 96 0 R0\nSYMATTR InstName R2\nSYMATTR Value 10k\n",
            "SYMBOL npn 128 0 R0\nSYMATTR InstName Q1\nSYMATTR Value qm\n",
            "SYMBOL nmos 160 0 R0\nSYMATTR InstName M1\nSYMATTR Value mm\n",
            "SYMBOL diode 192 0 R0\nSYMATTR InstName D1\nSYMATTR Value dm\n",
            "TEXT 0 32 Left 2 !.op\n",
        ),
    )?;
    let bundle = prepared_with_search(&root, vec![lib.clone()])?;
    let output = std::str::from_utf8(&bundle.primary.bytes)?;
    assert!(output.lines().any(|line| line == "Q1 out base 0 0 qm"));
    assert!(output.lines().any(|line| line == "M1 out in 0 0 mm"));
    assert!(output.lines().any(|line| line == "D1 0 out dm"));
    assert!(
        output
            .lines()
            .any(|line| line == ".model dm ltspice_diode(IS=2e-14 N=1.1)")
    );
    assert!(output.lines().any(|line| line == ".model D ltspice_diode"));
    let Provenance::Native { lines, .. } = &bundle.primary.provenance else {
        anyhow::bail!("missing provenance")
    };
    let qm = output
        .lines()
        .position(|line| line.starts_with(".model qm "))
        .context("missing library model")?;
    assert_eq!(lines[qm].source, cmp.join("standard.bjt").canonicalize()?);
    assert_eq!(lines[qm].line, 1);
    let reference = concat!(
        "reference\n",
        "V1 in 0 PULSE(0 1.2 1u 1u 1u 2m 4m) AC 1\n",
        "V2 vcc 0 5\nR1 vcc out 2k\nR2 in base 10k\n",
        "Q1 out base 0 0 qm\nM1 out in 0 0 mm\nD1 0 out dm\n",
        ".model D D\n.model dm D(IS=2e-14 N=1.1)\n",
        ".model NPN NPN\n.model PNP PNP\n.model qm NPN(IS=3e-15 BF=120 VAF=100)\n",
        ".model NMOS NMOS\n.model PMOS PMOS\n.model mm NMOS(LEVEL=1 VTO=.65 KP=.001)\n.end\n",
    );
    // Stock C ngspice cannot execute solver-specific dialect markers. Use
    // the importer's original LTspice source for this independent topology
    // and binding oracle, on the ordinary shared semiconductor subset. The
    // prepared output and provenance above separately check dialect lowering.
    let mut session = SourceSession::new(AdmissionConfig::default(), Cancellation::default());
    let admitted = session.load(&root)?;
    let imported = model_import_ltspice::import(
        &admitted,
        std::slice::from_ref(&lib),
        &mut session,
        Options::default().output_bytes,
    )?;
    let linked = SourceLinker {
        search_paths: vec![lib],
        dialect: SourceDialect::Ltspice,
    }
    .link_admitted(
        &AdmittedSource {
            path: admitted.path.clone(),
            bytes: imported.bytes.into(),
        },
        RootKind::Deck,
        LinkLimits {
            output_bytes: Options::default().output_bytes,
        },
        &mut session,
    )?;
    let observed = numerical_data(dir.path(), &linked.bytes, "device-import")?;
    let expected = numerical_data(dir.path(), reference.as_bytes(), "device-reference")?;
    for (observed, expected) in observed.iter().zip(&expected) {
        ensure!(!observed.is_empty(), "empty numerical trace");
        assert_eq!(observed.len(), expected.len());
        for (&a, &b) in observed.iter().zip(expected) {
            ensure!((a - b).abs() <= 1e-10 * b.abs().max(1.0), "{a} != {b}");
        }
    }
    Ok(())
}
