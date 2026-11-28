use anyhow::Result;
use model_import_ltspice::{ImportedDeck, import};
use model_preprocess_command::{AdmissionConfig, Cancellation, SourceSession};
use std::{fs, path::Path};

const RES: &str = "Version 4\nSymbolType CELL\nSYMATTR Prefix R\nSYMATTR Value 1k\nPIN 0 16 NONE 0\nPINATTR SpiceOrder 2\nPIN 0 0 NONE 0\nPINATTR SpiceOrder 1\n";
const CHILD: &str = "Version 4\nSymbolType BLOCK\nPIN 0 0 NONE 0\nPINATTR PinName in\nPINATTR SpiceOrder 1\nPIN 0 16 NONE 0\nPINATTR PinName out\nPINATTR SpiceOrder 2\n";

fn read(path: &Path, ceiling: usize) -> Result<ImportedDeck> {
    let mut session = SourceSession::new(AdmissionConfig::default(), Cancellation::default());
    let root = session.load(path)?;
    import(&root, &[], &mut session, ceiling)
}

#[test]
fn repeated_hierarchy_and_source_origins() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("top.asc");
    let child = dir.path().join("child.asc");
    fs::write(dir.path().join("res.asy"), RES)?;
    fs::write(dir.path().join("child.asy"), CHILD)?;
    fs::write(
        &child,
        "Version 4\nSHEET 1 100 100\nFLAG 0 0 in\nIOPIN 0 0 In\nFLAG 0 16 out\nIOPIN 0 16 Out\nSYMBOL res 0 0 R0\nSYMATTR InstName R1\nSYMATTR Value 2k\n",
    )?;
    fs::write(
        &root,
        "Version 4\nSHEET 1 100 100\nFLAG 0 0 a\nFLAG 0 16 b\nFLAG 16 0 b\nFLAG 16 16 0\nSYMBOL child 0 0 R0\nSYMATTR InstName X1\nSYMBOL child 16 0 R0\nSYMATTR InstName X2\nTEXT 0 0 Left 2 !.op\n",
    )?;
    let deck = read(&root, 100_000)?;
    let source = std::str::from_utf8(&deck.bytes)?;
    assert!(source.contains("XX1 a b __lt_sheet_1\n"));
    assert!(source.contains("XX2 b 0 __lt_sheet_1\n"));
    assert_eq!(source.matches(".subckt").count(), 1);
    assert!(source.contains(".subckt __lt_sheet_1 in out\nR1 in out 2k\n"));
    assert!(source.contains(".op\n"));
    assert_eq!(deck.origins.len(), source.lines().count());
    let index = source
        .lines()
        .position(|line| line.starts_with("R1 "))
        .ok_or_else(|| anyhow::anyhow!("missing resistor"))?;
    assert_eq!(deck.origins[index].path, child.canonicalize()?);
    assert_eq!(deck.origins[index].line, 7);
    assert!(read(&root, 8).is_err());
    fs::write(
        &child,
        "Version 4\nSHEET 1 100 100\nFLAG 0 0 in\nIOPIN 0 0 In\nFLAG 0 16 out\nIOPIN 0 16 Out\nSYMBOL child 0 0 R0\nSYMATTR InstName X1\n",
    )?;
    assert!(read(&root, 100_000).is_err());
    Ok(())
}

#[test]
fn explicit_model_file_and_rotated_cell() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("top.asc");
    fs::write(
        dir.path().join("custom.asy"),
        RES.replace("Prefix R", "Prefix X").replace(
            "Value 1k",
            "Value cell\nSYMATTR ModelFile \"models file.lib\"",
        ),
    )?;
    fs::write(
        dir.path().join("models file.lib"),
        ".subckt cell a b\nR1 a b 1k\n.ends\n",
    )?;
    fs::write(
        &path,
        "Version 4\nSHEET 1 100 100\nFLAG 0 0 input\nFLAG -16 0 0\nSYMBOL custom 0 0 R90\nSYMATTR InstName X1\nTEXT 0 0 Left 2 !.op\n",
    )?;
    let deck = read(&path, 100_000)?;
    let text = std::str::from_utf8(&deck.bytes)?;
    assert!(text.contains("XX1 input 0 cell\n"));
    assert!(text.contains(".lib \""));
    assert!(text.contains("models file.lib\""));
    Ok(())
}

#[test]
fn public_rc_schematic_matches_supplied_ltspice_netlist() -> Result<()> {
    let dir = tempfile::tempdir()?;
    for (name, bytes) in [
        (
            "filter.asc",
            include_bytes!("fixtures/rc-filter/filter.asc").as_slice(),
        ),
        (
            "voltage.asy",
            include_bytes!("fixtures/rc-filter/voltage.asy").as_slice(),
        ),
        (
            "res.asy",
            include_bytes!("fixtures/rc-filter/res.asy").as_slice(),
        ),
        (
            "cap.asy",
            include_bytes!("fixtures/rc-filter/cap.asy").as_slice(),
        ),
    ] {
        fs::write(dir.path().join(name), bytes)?;
    }
    let deck = read(&dir.path().join("filter.asc"), 100_000)?;
    let actual = std::str::from_utf8(&deck.bytes)?;
    let reference = include_str!("fixtures/rc-filter/reference.net");
    let normalize = |text: &str| {
        text.lines()
            .filter(|line| {
                !line.starts_with('*')
                    && !line.starts_with("LTspice schematic")
                    && !line.eq_ignore_ascii_case(".backanno")
            })
            .map(|line| line.replace('µ', "u").to_ascii_lowercase())
            .collect::<Vec<_>>()
    };
    assert_eq!(normalize(actual), normalize(reference));
    Ok(())
}

#[test]
fn synthetic_schematics_match_vendor_exports() -> Result<()> {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vendor");
    let normalize = |text: &str| {
        let mut lines = text
            .lines()
            .map(str::trim)
            .filter(|line| {
                !line.is_empty()
                    && !line.starts_with('*')
                    && !line.starts_with("LTspice schematic")
                    && !line.eq_ignore_ascii_case(".backanno")
            })
            .map(|line| {
                if line.starts_with(".lib ") {
                    ".lib ext.lib".to_owned()
                } else {
                    line.split_whitespace()
                        .map(|part| {
                            if part == "child" {
                                "__lt_sheet_1"
                            } else {
                                part
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(" ")
                }
            })
            .collect::<Vec<_>>();
        lines.sort_unstable();
        lines
    };
    for name in [
        "orient",
        "cross",
        "junction",
        "hierarchy",
        "custom",
        "naming",
    ] {
        let deck = read(&fixtures.join(format!("{name}.asc")), 100_000)?;
        let actual = std::str::from_utf8(&deck.bytes)?;
        let reference = fs::read_to_string(fixtures.join(format!("{name}.net")))?;
        if name == "custom" {
            assert!(
                actual
                    .lines()
                    .any(|line| line.starts_with(".lib ") && line.ends_with("ext.lib\""))
            );
        }
        assert_eq!(normalize(actual), normalize(&reference), "{name}");
    }
    Ok(())
}
