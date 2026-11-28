use anyhow::{Context, Result};
use model_import_ltspice::import;
use model_preprocess_command::{AdmissionConfig, Cancellation, SourceSession};
use std::{
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
};

fn symbol(path: &Path, attributes: &str, pins: &[(usize, i32, i32)]) -> Result<()> {
    let mut text = format!("Version 4\nSymbolType CELL\n{attributes}");
    for &(order, x, y) in pins {
        writeln!(text, "PIN {x} {y} NONE 0\nPINATTR SpiceOrder {order}")?;
    }
    fs::create_dir_all(path.parent().context("missing symbol directory")?)?;
    fs::write(path, text)?;
    Ok(())
}

fn imported(path: &Path, search: &[PathBuf]) -> Result<String> {
    let mut session = SourceSession::new(AdmissionConfig::default(), Cancellation::default());
    let root = session.load(path)?;
    Ok(String::from_utf8(
        import(&root, search, &mut session, 100_000)?.bytes,
    )?)
}

fn records(text: &str) -> Result<Vec<String>> {
    text.lines()
        .filter(|line| {
            !line.is_empty()
                && !line.starts_with('*')
                && !line.starts_with("LTspice schematic")
                && !line.eq_ignore_ascii_case(".backanno")
        })
        .map(|line| {
            if let Some(path) = line.strip_prefix(".lib ") {
                let name = Path::new(path.trim_matches('"'))
                    .file_name()
                    .context("missing library basename")?
                    .to_str()
                    .context("non-UTF8 library basename")?;
                Ok(format!(".lib {name}"))
            } else {
                Ok(line.to_owned())
            }
        })
        .collect()
}

#[test]
fn standard_primitive_families_match_vendor_exports() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let lib = dir.path().join("lib");
    let sym = lib.join("sym");
    for (name, prefix, value, pins) in [
        ("diode", "D", "D", &[(1, 16, 0), (2, 16, 64)][..]),
        (
            "npn",
            "QN",
            "NPN",
            &[(1, 64, 0), (2, 0, 48), (3, 64, 96)][..],
        ),
        (
            "pnp",
            "QP",
            "PNP",
            &[(1, 64, 0), (2, 0, 48), (3, 64, 96)][..],
        ),
        (
            "nmos",
            "MN",
            "NMOS",
            &[(1, 48, 0), (2, 0, 80), (3, 48, 96)][..],
        ),
        (
            "pmos",
            "MP",
            "PMOS",
            &[(1, 48, 0), (2, 0, 80), (3, 48, 96)][..],
        ),
        (
            "nmos4",
            "MN",
            "NMOS",
            &[(1, 48, 0), (2, 0, 80), (3, 48, 96), (4, 48, 48)][..],
        ),
        (
            "njf",
            "JN",
            "NJF",
            &[(1, 48, 0), (2, 0, 64), (3, 48, 96)][..],
        ),
        (
            "pjf",
            "JP",
            "PJF",
            &[(1, 48, 0), (2, 0, 64), (3, 48, 96)][..],
        ),
        (
            "sw",
            "S",
            "SW",
            &[(1, 0, 16), (2, 0, 96), (3, -48, 80), (4, -48, 32)][..],
        ),
    ] {
        symbol(
            &sym.join(format!("{name}.asy")),
            &format!("SYMATTR Prefix {prefix}\nSYMATTR Value {value}\n"),
            pins,
        )?;
    }
    fs::create_dir_all(lib.join("cmp"))?;
    // Authored empty libraries suffice for topology export; no vendor model
    // data is necessary or embedded in this fixture.
    for family in ["dio", "bjt", "jft", "mos"] {
        fs::write(
            lib.join("cmp").join(format!("standard.{family}")),
            "* authored empty model library\n",
        )?;
    }
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vendor/standard");
    for name in ["default", "selected"] {
        let path = fixtures.join(format!("{name}.asc"));
        let actual = imported(&path, std::slice::from_ref(&lib))?;
        let reference = fs::read_to_string(fixtures.join(format!("{name}.net")))?;
        assert_eq!(records(&actual)?, records(&reference)?, "{name}");
    }
    // A symbol library can itself establish the sibling cmp search root.
    let path = fixtures.join("default.asc");
    assert_eq!(
        records(&imported(&path, &[sym])?)?,
        records(&fs::read_to_string(fixtures.join("default.net"))?)?
    );
    fs::remove_file(lib.join("cmp/standard.dio"))?;
    assert!(imported(&path, &[lib]).is_err());
    Ok(())
}

#[test]
fn sparse_special_function_terminals_match_vendor_export() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let symbols = dir.path().join("Digital");
    symbol(
        &symbols.join("inv.asy"),
        "SYMATTR Prefix A\nSYMATTR SpiceModel BUF\n",
        &[(1, 0, 64), (6, 64, 64), (8, 0, 80)],
    )?;
    symbol(
        &symbols.join("and.asy"),
        "SYMATTR Prefix A\nSYMATTR SpiceModel AND\n",
        &[
            (1, -32, 32),
            (2, -32, 48),
            (3, -32, 64),
            (4, -32, 80),
            (5, -32, 96),
            (6, 32, 80),
            (7, 32, 48),
            (8, -16, 96),
        ],
    )?;
    symbol(
        &symbols.join("schmtbuf.asy"),
        "SYMATTR Prefix A\nSYMATTR SpiceModel SCHMITT\n",
        &[(1, 0, 64), (7, 64, 64), (8, 0, 80)],
    )?;
    symbol(
        &dir.path().join("res.asy"),
        "SYMATTR Prefix R\nSYMATTR Value 1k\n",
        &[(1, 16, 16), (2, 16, 96)],
    )?;
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vendor/digital");
    let path = fixtures.join("logic.asc");
    let actual = imported(&path, &[dir.path().to_owned()])?;
    assert_eq!(
        records(&actual)?,
        records(&fs::read_to_string(fixtures.join("logic.net"))?)?
    );
    let floating = imported(&fixtures.join("floating.asc"), &[dir.path().to_owned()])?;
    let line = floating
        .lines()
        .find(|line| line.starts_with("A1 "))
        .context("missing inverter")?;
    let tokens = line.split_whitespace().collect::<Vec<_>>();
    let reference = fs::read_to_string(fixtures.join("floating.net"))?;
    let expected = reference
        .lines()
        .find(|line| line.starts_with("A1 "))
        .context("missing vendor inverter")?
        .split_whitespace()
        .collect::<Vec<_>>();
    assert_eq!(&tokens[..6], &expected[..6]);
    assert_eq!(&tokens[7..], &expected[7..]);
    // The wire stub is a genuine connection and does not collapse to common;
    // generated anonymous node names need not match the vendor's numbering.
    assert_ne!(tokens[6], tokens[8]);
    assert_ne!(tokens[6], "0");
    Ok(())
}

#[test]
fn fixed_subcircuit_binding_keeps_only_the_call_tail() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("fixed.asc");
    fs::write(
        dir.path().join("models.lib"),
        ".subckt cell a b\nR1 a b 2k\n.ends cell\n",
    )?;
    symbol(
        &dir.path().join("fixed.asy"),
        "SYMATTR Prefix X\nSYMATTR Value display-only\nSYMATTR Value2 cell\nSYMATTR SpiceModel models.lib\nSYMATTR SpiceLine ignored_default\n",
        &[(1, 0, 0), (2, 0, 16)],
    )?;
    fs::write(
        &path,
        "Version 4\nSHEET 1 800 600\nFLAG 128 128 a\nFLAG 128 144 0\nSYMBOL fixed 128 128 R0\nSYMATTR InstName X1\nSYMATTR SpiceLine ignored_instance\nSYMATTR SpiceLine2 ignored_second\nTEXT 16 32 Left 2 !.op\n",
    )?;
    assert_eq!(
        records(&imported(&path, &[])?)?,
        ["XX1 a 0 cell", ".op", ".lib models.lib", ".end"]
    );
    symbol(
        &dir.path().join("fixed.asy"),
        "SYMATTR Prefix X\nSYMATTR Value cell\nSYMATTR Value2 params: P=2\nSYMATTR ModelFile models.lib\n",
        &[(1, 0, 0), (2, 0, 16)],
    )?;
    assert_eq!(
        records(&imported(&path, &[])?)?,
        [
            "XX1 a 0 cell params: P=2 ignored_instance ignored_second",
            ".op",
            ".lib models.lib",
            ".end"
        ]
    );
    Ok(())
}
