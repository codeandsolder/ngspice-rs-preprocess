//! Explicit LTspice source admission and native numerical lowering.
use anyhow::{Context, Result, ensure};
use ngspice_rs_preprocess::{
    Cancellation, InputFormat, Options, Provenance, Request, SourceInput, prepare,
};
use std::{fs, path::Path, process::Command};

fn prepared(path: &Path, format: InputFormat) -> Result<ngspice_rs_preprocess::PreparedBundle> {
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

#[test]
fn links_whole_files_and_named_sections_with_original_line_mapping() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("source.cir");
    let libs = dir.path().join("sub dir");
    fs::create_dir(&libs)?;
    let library = libs.join("part.lib");
    let component = libs.join("res.inc");
    fs::write(
        &library,
        b"* library\nVignored junk 0 7\n.subckt cell p n\n.include res.inc\n.ends cell\n",
    )?;
    fs::write(&component, b"Rquiet p n 1k\n+ Noiseless ; flag\n")?;
    fs::write(dir.path().join("sections.lib"), b".lib unused\n.model wrong D\n.endl unused\n.lib chosen\n.model right D\nRignored skip 0 1k\n.endl chosen\n")?;
    fs::write(dir.path().join("literal.inc"), b"Rliteral a 0 2k\n")?;
    fs::write(&root, b"ordinary title\n.lib \"sub dir\\part.lib\"\n.lib sections.lib chosen\n.include literal.inc\nX1 a 0 cell\nV1 a 0 1\n.backanno\n.end\n")?;
    let bundle = prepared(&root, InputFormat::LtspiceDeck)?;
    let output = std::str::from_utf8(&bundle.primary.bytes)?;
    assert!(output.starts_with("ordinary title\n"));
    assert!(output.contains(".subckt cell p n"));
    assert!(output.contains("+ noisy=0 ; flag"));
    assert!(output.contains(".model right D"));
    assert!(output.contains("Rliteral a 0 2k"));
    assert!(output.contains("*backanno"));
    assert!(!output.contains("Vignored"));
    assert!(!output.contains("Rignored"));
    assert!(!output.contains(".model wrong"));
    let Provenance::Native { lines } = &bundle.primary.provenance else {
        anyhow::bail!("missing source mapping");
    };
    assert_eq!(lines.len(), output.lines().count());
    for (line, origin) in output.lines().zip(lines) {
        if line.contains("noisy=0") {
            assert_eq!(origin.source, component.canonicalize()?);
            assert_eq!(origin.line, 2);
        }
    }
    Ok(())
}

#[test]
fn source_specific_edits_preserve_names_control_and_native_admission() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("source.cir");
    fs::write(&root, b"limit(x,y,z) noiseless title\nR1 noiseless 0 1k\nR2 p 0 {noiseless}\nR3 p 0 1k noisy=1\n.param noiseless=10\n.func __lt_intrinsic_if(c,a,b) {c+a+b}\nB1 n 0 V=if(0.4,1,limit(5,3,-1))\n* limit(x,y,z)\n.control\necho limit(x,y,z)\n.endc\n.backanno\n.end\n")?;
    let native = prepared(&root, InputFormat::NativeDeck)?;
    assert_eq!(native.primary.bytes, fs::read(&root)?);
    let translated = prepared(&root, InputFormat::LtspiceDeck)?;
    let text = std::str::from_utf8(&translated.primary.bytes)?;
    assert!(text.starts_with("limit(x,y,z) noiseless title\n"));
    assert!(text.contains("R1 noiseless 0 1k"));
    assert!(text.contains("R2 p 0 {noiseless}"));
    assert!(text.contains("R3 p 0 1k noisy=1"));
    assert!(text.contains("__lt_intrinsic_if_1(0.4,1,__lt_intrinsic_limit("));
    assert!(text.contains("echo limit(x,y,z)"));
    assert!(text.contains("* limit(x,y,z)"));
    let Provenance::Native { lines } = &translated.primary.provenance else {
        anyhow::bail!("missing source mapping");
    };
    assert_eq!(lines.len(), text.lines().count());
    assert_eq!(lines[1].line, 7);
    assert_eq!(lines[2].line, 7);
    Ok(())
}

#[test]
fn rejects_ambiguous_flags_and_shadowed_intrinsics() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("bad.cir");
    for body in [
        "R1 a 0 1k noiseless noiseless",
        "R1 a 0 1k noiseless=1",
        "R1 a 0 1k noisy=1 noiseless",
        ".func limit(x,y,z) {x}",
        ".backanno unexpected",
    ] {
        fs::write(&root, format!("title\n{body}\n.end\n"))?;
        assert!(prepared(&root, InputFormat::LtspiceDeck).is_err(), "{body}");
    }
    Ok(())
}

#[test]
fn explicit_library_format_preserves_first_model_and_include_safety() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("library.lib");
    fs::write(
        &path,
        b".param x={limit(5,2,-1)}\n.model dtest D(Is=1n)\n.end\n",
    )?;
    let bundle = prepared(&path, InputFormat::LtspiceLibrary)?;
    assert_eq!(bundle.primary.filename, "model.lib");
    let output = std::str::from_utf8(&bundle.primary.bytes)?;
    assert!(output.contains(".param x={__lt_intrinsic_limit(5,2,-1)}"));
    assert!(output.contains(".model dtest D(Is=1n)"));
    assert!(!output.lines().any(|line| line.eq_ignore_ascii_case(".end")));
    Ok(())
}

fn native(dir: &Path, deck: &[u8]) -> Result<()> {
    fs::write(dir.join("native.cir"), deck)?;
    let result = Command::new("ngspice")
        .current_dir(dir)
        .args(["-b", "-n", "native.cir"])
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
    Ok(())
}

fn data(path: &Path) -> Result<Vec<Vec<f64>>> {
    fs::read_to_string(path)?
        .lines()
        .map(|line| {
            line.split_whitespace()
                .map(|value| value.parse::<f64>().map_err(Into::into))
                .collect()
        })
        .collect()
}

fn near(actual: f64, expected: f64) -> Result<()> {
    ensure!(
        (actual - expected).abs() <= 1e-9 * expected.abs().max(1.),
        "{actual} != {expected}"
    );
    Ok(())
}

#[test]
fn scalar_intrinsics_match_documented_laws_in_native_dc_and_transient() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("source.cir");
    fs::write(&root, b"title\n.options reltol=1e-10 abstol=1e-14 vntol=1e-12\n.param base={if(0.5,10,20)+limit(3,2,-2)+pwr(-2,2)+pwrs(-2,2)+inv(0.4)+square(-2)}\nVp p 0 {base}\nVin x 0 PWL(0 -3 6u 3)\nB1 y 0 V=if(v(x),10,20)\nB2 z 0 V=limit(v(x),2,-2)\nB3 u 0 V=pwr(v(x),2)\nB4 s 0 V=pwrs(v(x),2)\nB5 w 0 V=inv(v(x))\nB6 q 0 V=square(v(x))\n.backanno\n.end\n")?;
    let bundle = prepared(&root, InputFormat::LtspiceDeck)?;
    let mut deck = bundle
        .primary
        .bytes
        .strip_suffix(b".end\n")
        .context("missing terminator")?
        .to_vec();
    deck.extend_from_slice(b".control\nset numdgt=17\nset wr_singlescale\ndc Vin -3 3 0.125\nwrdata dc.dat v(x) v(p) v(y) v(z) v(u) v(s) v(w) v(q)\ntran 10n 6u 0 10n\nwrdata tran.dat v(x) v(p) v(y) v(z) v(u) v(s) v(w) v(q)\nquit\n.endc\n.end\n");
    native(dir.path(), &deck)?;
    for (filename, minimum) in [("dc.dat", 49), ("tran.dat", 500)] {
        let rows = data(&dir.path().join(filename))?;
        ensure!(rows.len() >= minimum, "missing {filename} samples");
        for row in rows {
            let [_, x, p, y, z, u, s, w, q] = row.as_slice() else {
                anyhow::bail!("unexpected {filename} row");
            };
            near(*p, 27.)?;
            // Use the actual solved x, not the nominal sweep coordinate.
            // At the discontinuity C's Newton trial can lie one ULP on
            // either side of the final printed voltage. Check both output
            // states there, and exact strict threshold for the static base.
            if (*x - 0.5).abs() > 8. * f64::EPSILON {
                near(*y, if *x > 0.5 { 10. } else { 20. })
                    .with_context(|| format!("{filename}: x={x:.17e}"))?;
                near(*w, if *x > 0.5 { 0. } else { 1. })?;
            } else {
                near(*w, w.round())?;
                ensure!((0. ..=1.).contains(w), "invalid Boolean value {w}");
                near(*y, 10.0_f64.mul_add(*w, 10.))?;
            }
            near(*z, x.clamp(-2., 2.))?;
            near(*u, x * x)?;
            near(*s, x.signum() * x * x)?;
            near(*q, x * x)?;
        }
    }
    Ok(())
}
