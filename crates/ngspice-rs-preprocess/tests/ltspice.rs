//! Explicit `LTspice` source admission and native numerical lowering.
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
        ".model ideal D(Ron=0.1 Roff=1Meg)",
        ".model smooth SW(Ron=1 Roff=1Meg Vh=-1)",
        ".model smooth SW(Vh={width})",
        ".model smooth SW(level=2)",
        ".model limited SW(Ilimit=1)",
        ".model reactive C(Rser=1)",
        ".func min(x,y) {x}",
        "B1 y 0 V=table(v(x),0,0,1)",
        "B1 y 0 V=limit(random(time),0,1)",
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

fn parse_data(text: &str) -> Result<Vec<Vec<f64>>> {
    text.lines()
        .map(|line| {
            line.split_whitespace()
                .map(|value| value.parse::<f64>().map_err(Into::into))
                .collect()
        })
        .collect()
}

fn data(path: &Path) -> Result<Vec<Vec<f64>>> {
    parse_data(&fs::read_to_string(path)?)
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

#[test]
fn vendor_operator_precedence_and_table_limiter_dc_sweep() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("source.cir");
    // Independently run with LTspice 24.0.12 on 2026-11-25.
    // The vendor DC sweep uses exact quarter-volt steps and tight tolerances.
    fs::write(&root, b"title\n.options reltol=1e-10 abstol=1e-14 vntol=1e-12\nVx x 0 0\nB1 a 0 V=table(V(x),-1,2,0,5,2,1)\nB2 b 0 V=uplim(V(x),1,.5)\nB3 c 0 V=dnlim(V(x),-1,.5)\nB4 d 0 V=V(x)^0.4\nB5 e 0 V=V(x)&0.6\nB6 f 0 V=V(x)|0.4\nB7 g 0 V=!V(x)\nB8 h 0 V=~V(x)\nB9 j 0 V=1|1&0\nB10 k 0 V=1^1|1\nB11 l 0 V=1|1^1\nB12 m 0 V=-2**2\nB13 n 0 V=2**3**2\nB14 p 0 V=pow(-2,0.5)\nB15 q 0 V=pow(-2,1.5)\nB16 r 0 V=pow(-2,2.5)\nB17 s 0 V=pow(-2,1.25)\nB18 t 0 V=pow(-2,3)\nB19 u 0 V=0**0\nB20 w 0 V=table(V(x),0,10)\nB21 zz 0 V=table(V(x),-1,2,0,5,0,8,2,1)\nB22 z0 0 V=pow(0,-2)\nB23 z1 0 V=pwr(0,0)\nB24 z2 0 V=pwrs(0,0)\nB25 z3 0 V=uplim(V(x),1,0)\nB26 z4 0 V=dnlim(V(x),-1,0)\nB27 z5 0 V=uplim(V(x),1,-.5)\nB28 z6 0 V=dnlim(V(x),-1,-.5)\n.end\n")?;
    let bundle = prepared(&root, InputFormat::LtspiceDeck)?;
    let mut deck = bundle
        .primary
        .bytes
        .strip_suffix(b".end\n")
        .context("missing terminator")?
        .to_vec();
    deck.extend_from_slice(b".control\nset numdgt=17\nset wr_singlescale\ndc Vx -3 3 .25\nwrdata dc.dat v(x) v(a) v(b) v(c) v(d) v(e) v(f) v(g) v(h) v(j) v(k) v(l) v(m) v(n) v(p) v(q) v(r) v(s) v(t) v(u) v(w) v(zz) v(z0) v(z1) v(z2) v(z3) v(z4) v(z5) v(z6)\nquit\n.endc\n.end\n");
    native(dir.path(), &deck)?;
    let rows = data(&dir.path().join("dc.dat"))?;
    assert_eq!(rows.len(), 25);
    let vendor = parse_data(include_str!("fixtures/ltspice/scalar-dc.tsv"))?;
    assert_eq!(vendor.len(), rows.len());
    for (row, reference) in rows.iter().zip(vendor) {
        assert_eq!(reference.len(), 22);
        for (index, (&actual, expected)) in row.iter().zip(reference).enumerate() {
            if (row[1] - 0.5).abs() <= 8. * f64::EPSILON && (5..10).contains(&index) {
                continue;
            }
            near(actual, expected)
                .with_context(|| format!("vendor sweep {}, column={index}", row[0]))?;
        }
    }
    for row in rows {
        let x = row[1];
        let value = if x <= -1. {
            2.
        } else if x <= 0. {
            3.0_f64.mul_add(x, 5.)
        } else {
            2.0_f64.mul_add(-x.min(2.), 5.)
        };
        let upper = |input: f64, width: f64| {
            if width == 0. {
                return input.min(1.);
            }
            if input <= 1. - width {
                input
            } else {
                (-width).mul_add(((1. - width - input) / width).exp(), 1.)
            }
        };
        let duplicate = if x <= 0. {
            value
        } else {
            (-3.5_f64).mul_add(x.min(2.), 8.)
        };
        let boolean = f64::from(x > 0.5);
        let expected = [
            value,
            upper(x, 0.5),
            -upper(-x, 0.5),
            boolean,
            boolean,
            boolean,
            1. - boolean,
            1. - boolean,
            0.,
            1.,
            0.,
            4.,
            512.,
            0.,
            0.,
            0.,
            0.,
            -8.,
            1.,
            10.,
            duplicate,
            0.,
            1.,
            1.,
            upper(x, 0.),
            -upper(-x, 0.),
            upper(x, -0.5),
            -upper(-x, -0.5),
        ];
        assert_eq!(row.len(), expected.len() + 2);
        for (index, (&actual, expected)) in row[2..].iter().zip(expected).enumerate() {
            // Native Newton trial values can land one ULP across a jump.
            if (x - 0.5).abs() <= 8. * f64::EPSILON && (3..8).contains(&index) {
                assert!((0. ..=1.).contains(&actual));
            } else if x.abs() <= 8. * f64::EPSILON && index == 20 {
                assert!(actual == 5. || actual == 8.);
            } else {
                near(actual, expected).with_context(|| format!("x={x}, column={index}"))?;
            }
        }
    }
    Ok(())
}

#[test]
fn symbolic_table_knots_and_operators_in_parameter_scope() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("source.cir");
    fs::write(&root, b"title\n.param knot=2\n.param a=table(1,0,3,knot,5) b=table(2,0,3)\n.param e=1|1&0 f=2**3**2 g=-2**2 h=0**0\n.func nested(x) {table(x,0,table(0,0,3),knot,5)}\n.param k={nested(1)}\nVa a 0 {a}\nVb b 0 {b}\nVe e 0 {e}\nVf f 0 {f}\nVg g 0 {g}\nVh h 0 {h}\nVk k 0 {k}\n.end\n")?;
    let bundle = prepared(&root, InputFormat::LtspiceDeck)?;
    let mut deck = bundle
        .primary
        .bytes
        .strip_suffix(b".end\n")
        .context("missing terminator")?
        .to_vec();
    deck.extend_from_slice(b".control\nset numdgt=17\nset wr_singlescale\nop\nwrdata op.dat v(a) v(b) v(e) v(f) v(g) v(h) v(k)\nquit\n.endc\n.end\n");
    native(dir.path(), &deck)?;
    let rows = data(&dir.path().join("op.dat"))?;
    let row = rows.first().context("missing OP")?;
    assert_eq!(row.len(), 8);
    for (&actual, expected) in row[1..].iter().zip([4., 3., 0., 512., 4., 1., 4.]) {
        near(actual, expected)?;
    }
    Ok(())
}

#[test]
fn vendor_ac_slopes_preserve_interpolation_and_exponential_limiting() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("source.cir");
    fs::write(&root, b"title\n.options reltol=1e-10 abstol=1e-14 vntol=1e-12\nVx x 0 DC 0.75 AC 1\nB1 a 0 V=table(V(x),-1,2,0,5,2,1)\nB2 b 0 V=uplim(V(x),1,.5)\nB3 c 0 V=dnlim(-V(x),-1,.5)\nB4 d 0 V=table(V(x),-2,0,-1,1)\nB5 e 0 V=table(V(x),0,10)\nB6 f 0 V=table(V(x),0,0,1,2,2,4)\n.end\n")?;
    let bundle = prepared(&root, InputFormat::LtspiceDeck)?;
    let mut deck = bundle
        .primary
        .bytes
        .strip_suffix(b".end\n")
        .context("missing terminator")?
        .to_vec();
    deck.extend_from_slice(b".control\nset numdgt=17\nset wr_singlescale\nac lin 1 1000 1000\nwrdata ac.dat v(x) v(a) v(b) v(c) v(d) v(e) v(f)\nquit\n.endc\n.end\n");
    native(dir.path(), &deck)?;
    let rows = data(&dir.path().join("ac.dat"))?;
    let row = rows.first().context("missing AC row")?;
    assert_eq!(row.len(), 15);
    near(row[0], 1000.)?;
    for (actual, expected) in row[1..].chunks_exact(2).zip([
        1.,
        -2.,
        0.606_530_659_712_633_4,
        -0.606_530_659_712_633_4,
        0.,
        0.,
        2.,
    ]) {
        near(actual[0], expected)?;
        near(actual[1], 0.)?;
    }
    Ok(())
}

#[test]
fn vendor_parameter_and_device_numeric_phases() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("source.cir");
    // LTspice 24.0.12 OP: unbraced engineering percent, braced remainder,
    // subcircuit argument remainder, and static intrinsic boundary values.
    fs::write(&root, b"title\n.param p=3%2 q=-3%2 r=3.5%2\nVx x 0 1\nB1 a 0 V=3%2\nB2 b 0 V={3%2}\nB3 c 0 V={3%2}+3%2\nB4 d 0 V=3%25\nB5 e 0 V=3%\nB6 f 0 V=if(1,3%2,0)\nB7 g 0 V=sqrt(-4)+round(2.5)+round(-2.5)+int(-2.5)+u(0)\nB8 h 0 V=1==2<3\nB9 i 0 V=1<2==1\nVp pnode 0 {p}\nVq qnode 0 {q}\nVr rnode 0 {r}\n.subckt sub a b params: p=3%2\nR1 a b {p}\n.ends sub\nVdefault vd 0 1\nVexplicit ve 0 1\nX1 vd 0 sub\nX2 ve 0 sub p=3%2\n.end\n")?;
    let bundle = prepared(&root, InputFormat::LtspiceDeck)?;
    let mut deck = bundle
        .primary
        .bytes
        .strip_suffix(b".end\n")
        .context("missing terminator")?
        .to_vec();
    deck.extend_from_slice(b".control\nset numdgt=17\nset wr_singlescale\nop\nwrdata op.dat v(a) v(b) v(c) v(d) v(e) v(f) v(g) v(h) v(i) v(pnode) v(qnode) v(rnode) i(vdefault) i(vexplicit)\nquit\n.endc\n.end\n");
    native(dir.path(), &deck)?;
    let rows = data(&dir.path().join("op.dat"))?;
    let row = rows.first().context("missing OP")?;
    let expected = [
        0.032, 1., 1.032, 0.0325, 0.03, 0.032, -2., 1., 1., 1., -1., 1.5, -1., -1.,
    ];
    assert_eq!(row.len(), expected.len() + 1);
    for (&actual, expected) in row[1..].iter().zip(expected) {
        near(actual, expected)?;
    }
    Ok(())
}

#[test]
fn vendor_duplicate_table_knots_preserve_both_macro_evaluation_phases() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("source.cir");
    // The same user function evaluates a leading duplicate knot differently
    // in .param and behavioral contexts in LTspice 24.0.12.
    fs::write(&root, b"title\n.options reltol=1e-10 abstol=1e-14 vntol=1e-12\n.func inner(x) {table(x,0,1,0,2,1,3)}\n.func outer(x) {inner(x)}\n.param p=outer(0) q=table(1,1,1,1,2,1,3)\nVx x 0 DC 0 AC 1\nVp pp 0 {p}\nVq qq 0 {q}\nB1 a 0 V=outer(v(x))\nB2 b 0 V={outer(0)}\nB3 c 0 V=table(v(x),0,1,0,2,1,3)\n.end\n")?;
    let bundle = prepared(&root, InputFormat::LtspiceDeck)?;
    let text = std::str::from_utf8(&bundle.primary.bytes)?;
    assert!(text.contains(".func __lt_param_INNER"));
    assert!(text.contains(".func __lt_param_OUTER"));
    let Provenance::Native { lines } = &bundle.primary.provenance else {
        anyhow::bail!("missing source mapping");
    };
    assert_eq!(lines.len(), text.lines().count());
    let mut deck = bundle
        .primary
        .bytes
        .strip_suffix(b".end\n")
        .context("missing terminator")?
        .to_vec();
    deck.extend_from_slice(b".control\nset numdgt=17\nset wr_singlescale\nop\nwrdata op.dat v(pp) v(qq) v(a) v(b) v(c)\nac lin 1 1 1\nwrdata ac.dat v(a) v(c)\nquit\n.endc\n.end\n");
    native(dir.path(), &deck)?;
    let rows = data(&dir.path().join("op.dat"))?;
    let row = rows.first().context("missing OP")?;
    assert_eq!(row.len(), 6);
    for (&actual, expected) in row[1..].iter().zip([1., 1., 0., 1., 0.]) {
        near(actual, expected)?;
    }
    let rows = data(&dir.path().join("ac.dat"))?;
    let row = rows.first().context("missing AC")?;
    assert_eq!(row.len(), 5);
    for (&actual, expected) in row[1..].iter().zip([1e200, 0., 1e200, 0.]) {
        near(actual, expected)?;
    }
    Ok(())
}

#[test]
fn series_parasitics_preserve_terminal_admittance_and_coupling_scope() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("source.cir");
    fs::write(&root, b"title\n.options reltol=1e-10 abstol=1e-14 vntol=1e-12\nV0 s0 0 AC 1\nRsrc0 s0 a0 .1\nC0 a0 0 2u Rser=5\nV1 s1 0 AC 1\nRsrc1 s1 a1 .1\nC1 a1 0 2u Rser=5 m=3\nV2 s2 0 AC 1\nRsrc2 s2 a2 .1\nL2 a2 0 3m\nV3 s3 0 AC 1\nRsrc3 s3 a3 .1\nL3 a3 0 3m Rser=0 Rpar=0\nV4 s4 0 AC 1\nRsrc4 s4 a4 .1\nL4 a4 0 3m Rser=5 m=3\nV5 s5 0 AC 1\nRsrc5 s5 a5 .1\nV6 s6 0 AC 1\nRsrc6 s6 a6 .1\nX1 a5 a6 coupled\n.subckt coupled a b\nL2 a 0 3m\nL3 b 0 3m\nK1 L2 L3 .5\n.ends coupled\n.end\n")?;
    let bundle = prepared(&root, InputFormat::LtspiceDeck)?;
    let text = std::str::from_utf8(&bundle.primary.bytes)?;
    assert!(text.contains("R__L2_RSER a2"));
    assert!(text.contains(" 1m\n"));
    assert!(text.contains("L3 a3 0 3m\n"));
    assert!(text.contains(".subckt coupled a b\nL2 a 0 3m\nL3 b 0 3m\nK1"));
    let Provenance::Native { lines } = &bundle.primary.provenance else {
        anyhow::bail!("missing source mapping");
    };
    assert_eq!(lines.len(), text.lines().count());
    for (line, origin) in text.lines().zip(lines) {
        if line.starts_with("R__L2_RSER") {
            assert_eq!(origin.line, 11);
        }
    }
    let mut deck = bundle
        .primary
        .bytes
        .strip_suffix(b".end\n")
        .context("missing terminator")?
        .to_vec();
    deck.extend_from_slice(b".control\nset numdgt=17\nset wr_singlescale\nac lin 3 100 10000\nwrdata ac.dat v(a0) i(v0) v(a1) i(v1) v(a2) i(v2) v(a3) i(v3) v(a4) i(v4) v(a5) i(v5) v(a6) i(v6)\nquit\n.endc\n.end\n");
    native(dir.path(), &deck)?;
    for row in data(&dir.path().join("ac.dat"))? {
        let omega = std::f64::consts::TAU * row[0];
        let c = -1. / (omega * 2e-6);
        let l = omega * 3e-3;
        for (values, (resistance, reactance, m)) in row[1..].chunks_exact(4).zip([
            (5_f64, c, 1.),
            (5., c, 3.),
            (1e-3, l, 1.),
            (0., l, 1.),
            (5., l, 3.),
            (0., 1.5 * l, 1.),
            (0., 1.5 * l, 1.),
        ]) {
            let [vr, vi, ir, ii] = [values[0], values[1], values[2], values[3]];
            let voltage_sq = vr.mul_add(vr, vi * vi);
            let real = -ir.mul_add(vr, ii * vi) / voltage_sq;
            let imag = -ii.mul_add(vr, -ir * vi) / voltage_sq;
            let z_sq = resistance.mul_add(resistance, reactance * reactance);
            near(real, m * resistance / z_sq)?;
            near(imag, -m * reactance / z_sq)?;
        }
    }
    Ok(())
}

#[test]
fn unresolved_passive_topology_is_diagnostic_not_silent() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("source.cir");
    for card in [
        "C1 a 0 1u Rpar=100",
        "C1 a 0 1u Lser=1m",
        "L1 a 0 1m Cpar=1p",
        "C1 a 0 1u m=0",
        "C1 a 0 1u IC=.75",
        "L1 a 0 1m IC=.1",
        "C1 a 0 Q={v(a)*v(a)}",
    ] {
        fs::write(&root, format!("title\n{card}\n.end\n"))?;
        assert!(prepared(&root, InputFormat::LtspiceDeck).is_err(), "{card}");
    }
    fs::write(&root, "L1 a b 1m\n")?;
    assert!(prepared(&root, InputFormat::LtspiceLibrary).is_err());
    fs::write(&root, "L1 a b 1m Rser=0\n")?;
    let bundle = prepared(&root, InputFormat::LtspiceLibrary)?;
    assert_eq!(bundle.primary.bytes, b"L1 a b 1m\n");
    Ok(())
}

#[test]
fn vendor_pulse_timing_defaults_and_relative_duplicate_pwl() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("source.cir");
    fs::write(&root, b"title\nV1 a 0 PULSE(0 1 1u 0 0 2u 10u)\nV2 b 0 PULSE(0 1 1u 0 1u 8u 10u)\nV3 c 0 PULSE(0 1 -1u 0 0 -2u 10u)\nV4 d 0 PULSE(0 1 1u 0 0)\nV5 e 0 PULSE(0 1 1u .1u .1u 8u 10u 1.5)\nV6 f 0 PWL(0 0 +1u 1\n+ +0 3 +0 8\n+ +1u 0)\nV7 g 0 PWL(+1u 1 +1u 2 +2u 0)\nI1 0 h PULSE(0 1 1u 0 0 2u 10u)\nR1 h 0 1\n.end\n")?;
    let bundle = prepared(&root, InputFormat::LtspiceDeck)?;
    let text = std::str::from_utf8(&bundle.primary.bytes)?;
    let Provenance::Native { lines } = &bundle.primary.provenance else {
        anyhow::bail!("missing source mapping");
    };
    assert_eq!(lines.len(), text.lines().count());
    assert!(
        text.lines()
            .any(|line| line.starts_with('*') && !line.contains("+0"))
    );
    let mut deck = bundle
        .primary
        .bytes
        .strip_suffix(b".end\n")
        .context("missing terminator")?
        .to_vec();
    deck.extend_from_slice(b".control\nset numdgt=17\nset wr_singlescale\ntran 5n 25u 0 5n\nwrdata tran.dat v(a) v(b) v(c) v(d) v(e) v(f) v(g) v(h)\nquit\n.endc\n.end\n");
    native(dir.path(), &deck)?;
    let pulse =
        |time: f64, delay: f64, rise: f64, fall: f64, width: f64, period: f64, cycles: f64| {
            let elapsed = time - delay;
            if elapsed <= 0. || (cycles > 0. && elapsed >= cycles * period) {
                return 0.;
            }
            let phase = elapsed % period;
            if phase < rise {
                phase / rise
            } else if phase < rise + width {
                1.
            } else if phase < rise + width + fall {
                (rise + width + fall - phase) / fall
            } else {
                0.
            }
        };
    for row in data(&dir.path().join("tran.dat"))? {
        let t = row[0];
        // Exact left/right choice at a count cutoff is simulator-dependent.
        if (t - 16e-6).abs() < 1e-16 {
            continue;
        }
        let f = if t < 1e-6 {
            4e6 * t
        } else {
            (4e6 * (2e-6 - t)).max(0.)
        };
        let g = if t < 1e-6 {
            1.
        } else if t < 2e-6 {
            1e6 * t
        } else {
            (1e6 * (4e-6 - t)).max(0.)
        };
        let expected = [
            pulse(t, 1e-6, 2e-7, 2e-7, 2e-6, 1e-5, 0.),
            pulse(t, 1e-6, 1e-7, 1e-6, 8e-6, 1e-5, 0.),
            pulse(t, 0., 1e-6, 1e-6, 0., 1e-5, 0.),
            pulse(t, 1e-6, 1e-8, 1e-8, 0., 2e-8 * (1. + 2e-15), 1.),
            pulse(t, 1e-6, 1e-7, 1e-7, 8e-6, 1e-5, 1.5),
            f,
            g,
            pulse(t, 1e-6, 2e-7, 2e-7, 2e-6, 1e-5, 0.),
        ];
        for (&actual, expected) in row[1..].iter().zip(expected) {
            near(actual, expected).with_context(|| format!("time {t}"))?;
        }
    }
    Ok(())
}
