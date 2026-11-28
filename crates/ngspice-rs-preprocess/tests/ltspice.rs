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
    assert!(output.contains(".model right ltspice_diode"));
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
    assert!(output.contains(".model dtest ltspice_diode(Is=1n)"));
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
fn series_parasitics_preserve_coefficients_and_coupling_scope() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("source.cir");
    fs::write(&root, b"title\n.options reltol=1e-10 abstol=1e-14 vntol=1e-12\nV0 s0 0 AC 1\nRsrc0 s0 a0 .1\nC0 a0 0 2u Rser=5\nV1 s1 0 AC 1\nRsrc1 s1 a1 .1\nC1 a1 0 2u Rser=5 m=3\nV2 s2 0 AC 1\nRsrc2 s2 a2 .1\nL2 a2 0 3m\nV3 s3 0 AC 1\nRsrc3 s3 a3 .1\nL3 a3 0 3m Rser=0 Rpar=0\nV4 s4 0 AC 1\nRsrc4 s4 a4 .1\nL4 a4 0 3m Rser=5 m=3\nV5 s5 0 AC 1\nRsrc5 s5 a5 .1\nV6 s6 0 AC 1\nRsrc6 s6 a6 .1\nX1 a5 a6 coupled\n.subckt coupled a b\nL2 a 0 3m\nL3 b 0 3m\nK1 L2 L3 .5\n.ends coupled\n.end\n")?;
    let bundle = prepared(&root, InputFormat::LtspiceDeck)?;
    let text = std::str::from_utf8(&bundle.primary.bytes)?;
    assert!(!text.contains("R__L"));
    assert!(!text.contains("R__C"));
    assert!(text.contains("L2 a2 0 3m pn_rseries_uncoupled=1m"));
    assert!(text.contains("L3 a3 0 3m pn_rseries=0 pn_rparallel=0"));
    let coupled = text
        .split_once(".subckt coupled a b\n")
        .context("missing coupled scope")?
        .1;
    let coupled = coupled
        .split_once(".ends coupled")
        .context("missing scope end")?
        .0;
    for name in ["L2 a 0 3m", "L3 b 0 3m"] {
        let line = coupled
            .lines()
            .find(|line| line.starts_with(name))
            .context("missing coupled inductor")?;
        assert!(line.contains("pn_rseries_uncoupled=1m"));
        assert!(!line.contains("pn_rseries="));
        assert!(line.contains("pn_gterminal_dc="));
    }
    assert!(coupled.contains("K1 L2 L3 .5"));
    let Provenance::Native { lines } = &bundle.primary.provenance else {
        anyhow::bail!("missing source mapping");
    };
    assert_eq!(lines.len(), text.lines().count());
    for (line, origin) in text.lines().zip(lines) {
        if line.starts_with("L2 a2 ") {
            assert_eq!(origin.line, 11);
        }
    }
    let library = model_normalize_ngspice::parameter_scope::ModelLibrary::parse_source(
        &bundle.primary.bytes,
        model_normalize_ngspice::linked_source::RootKind::Deck,
    )?;
    for (device, resistance, copies) in [
        ("C0", 5., 1.),
        ("C1", 5., 3.),
        ("L2", 1e-3, 1.),
        ("L3", 0., 1.),
        ("L4", 5., 3.),
    ] {
        let line = text
            .lines()
            .find(|line| line.starts_with(&format!("{device} a")))
            .context("missing original device")?;
        let tail = line
            .splitn(5, ' ')
            .nth(4)
            .context("missing network parameters")?;
        let fields = model_normalize_ngspice::model_card::parse_assignments(tail)?;
        let value = |key: &str| -> Result<Option<f64>> {
            fields
                .iter()
                .find(|field| field.name.eq_ignore_ascii_case(key))
                .map(|field| {
                    library.evaluate_scalar(
                        0,
                        field.expression.as_deref().context("missing value")?,
                        &[],
                    )
                })
                .transpose()
        };
        near(
            value(if device == "L2" {
                "pn_rseries_uncoupled"
            } else {
                "pn_rseries"
            })?
            .context("missing series resistance")?,
            resistance,
        )?;
        near(value("m")?.unwrap_or(1.), copies)?;
    }
    Ok(())
}

#[test]
fn library_topology_is_deferred_but_unsupported_setters_are_diagnostic() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("source.cir");
    for card in [
        "L1 a 0 1m Lser=1m",
        "L1 a 0 1m RLshunt=20",
        "C1 a 0 Q={v(a)*v(a)}",
    ] {
        fs::write(&root, format!("title\n{card}\n.end\n"))?;
        assert!(prepared(&root, InputFormat::LtspiceDeck).is_err(), "{card}");
    }
    fs::write(&root, "L1 a b 1m\n")?;
    let bundle = prepared(&root, InputFormat::LtspiceLibrary)?;
    assert!(
        std::str::from_utf8(&bundle.primary.bytes)?.contains("L1 a b 1m pn_rseries_uncoupled=1m")
    );
    fs::write(&root, "L1 a b 1m Rser=0\n")?;
    let bundle = prepared(&root, InputFormat::LtspiceLibrary)?;
    assert!(std::str::from_utf8(&bundle.primary.bytes)?.contains("L1 a b 1m pn_rseries=0"));
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

#[test]
fn vendor_transient_timing_is_explicit_in_native_decks() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("source.cir");
    for (source, expected) in [
        (".tran 1", [1. / 1024., 1., 0., 1. / 1024.]),
        (".tran 0 1 .2", [1. / 1024., 1., 0.2, 1. / 1024.]),
        (".tran .03 1", [0.03, 1., 0., 1. / 1024.]),
        (".tran 0 1 0 .02 uic", [0.02, 1., 0., 0.02]),
        (".tran 0 1 -.2 0", [1. / 1024., 1., 0., 1. / 1024.]),
    ] {
        fs::write(
            &root,
            format!("title\nV1 a 0 1\nR1 a 0 1k\n{source}\n.print tran v(a)\n.end\n"),
        )?;
        let bundle = prepared(&root, InputFormat::LtspiceDeck)?;
        let output = std::str::from_utf8(&bundle.primary.bytes)?;
        let tran = output
            .lines()
            .find(|line| line.starts_with(".tran"))
            .context("missing transient analysis")?;
        let actual = tran
            .split_whitespace()
            .skip(1)
            .take(4)
            .map(str::parse::<f64>)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        assert_eq!(actual.len(), 4);
        for (&actual, expected) in actual.iter().zip(expected) {
            near(actual, expected)?;
        }
        assert_eq!(source.ends_with("uic"), tran.ends_with("uic"));
        native(dir.path(), &bundle.primary.bytes)?;
    }
    for source in [
        ".tran 0 1 startup",
        ".tran 0 1 steady",
        ".tran 0 1 1",
        ".tran 0 1 0 -1",
    ] {
        fs::write(&root, format!("title\n{source}\n.end\n"))?;
        assert!(
            prepared(&root, InputFormat::LtspiceDeck).is_err(),
            "{source}"
        );
    }
    Ok(())
}

#[test]
fn comma_delimited_pwl_duplicates_do_not_leave_empty_fields() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("source.cir");
    fs::write(&root, b"title\nV1 a 0 PWL(0,0,1u,1,1u,3,1u,8,2u,0)\n.end\n")?;
    let bundle = prepared(&root, InputFormat::LtspiceDeck)?;
    let output = std::str::from_utf8(&bundle.primary.bytes)?;
    assert!(
        !output.contains(",,") && !output.contains("  ,"),
        "{output}"
    );
    let mut deck = bundle
        .primary
        .bytes
        .strip_suffix(b".end\n")
        .context("missing terminator")?
        .to_vec();
    deck.extend_from_slice(b".control\nset wr_singlescale\ntran 10n 2u 0 10n\nwrdata comma.dat v(a)\nquit\n.endc\n.end\n");
    native(dir.path(), &deck)?;
    for row in data(&dir.path().join("comma.dat"))? {
        let expected = (4e6 * row[0]).min((4e6 * (2e-6 - row[0])).max(0.));
        near(row[1], expected)?;
    }
    Ok(())
}

#[test]
fn vendor_scaled_pwl_signed_coordinates_and_closed_waveform_expressions() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("source.cir");
    fs::write(&root, b"title\nV1 a 0 PWL TIME_SCALE_FACTOR=2 VALUE_SCALE_FACTOR=3 (0 0 +1u 1 +0 3 +1u 0)\nV2 b 0 PWL VALUE_SCALE_FACTOR=-2 TIME_SCALE_FACTOR=.5 (1u 1 +1u 2)\nV3 c 0 PWL(0 0 1u 1 2u 0) TIME_SCALE_FACTOR=2 VALUE_SCALE_FACTOR=3\nV4 d 0 PWL TIME_SCALE_FACTOR=2 TIME_SCALE_FACTOR={2+1} VALUE_SCALE_FACTOR={2*3} (0 0 1u 1 2u 0)\nV5 e 0 PWL(-1u 1 1u 3)\nV6 f 0 PWL TIME_SCALE_FACTOR=-1 (0 1 -1u 3 -2u 8)\nV7 g 0 PWL TIME_SCALE_FACTOR=-1 (2u 1 1u 3 0 8)\nV8 h 0 PWL VALUE_SCALE_FACTOR=0 (0 1 1u 3)\nV9 j 0 PWL(0 1 0 3)\nV10 k 0 PULSE({-2**2} {2**3**2} {1u/2} {1u/10} {1u/10} {1u*2} {1u*10})\nV11 l 0 PWL TIME_SCALE_FACTOR=3%2 (0 0 1u 1 2u 0)\nI1 0 m PWL TIME_SCALE_FACTOR=2 VALUE_SCALE_FACTOR=3\n+ (0 0 +1u 1 +0 3 +1u 0)\nR1 m 0 1\n.end\n")?;
    let bundle = prepared(&root, InputFormat::LtspiceDeck)?;
    let text = std::str::from_utf8(&bundle.primary.bytes)?;
    assert!(!text.to_ascii_lowercase().contains("scale_factor"));
    assert!(text.contains("PULSE(4 512"));
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
    deck.extend_from_slice(b".control\nset numdgt=17\nset wr_singlescale\ntran 10n 8u 0 10n\nwrdata tran.dat v(a) v(b) v(c) v(d) v(e) v(f) v(g) v(h) v(j) v(k) v(l) v(m)\nquit\n.endc\n.end\n");
    native(dir.path(), &deck)?;
    let rows = data(&dir.path().join("tran.dat"))?;
    assert!(rows.len() > 500);
    for row in rows {
        let t = row[0];
        let triangle = |half: f64, peak: f64| {
            if t <= half {
                peak * t / half
            } else {
                peak * (2. - t / half).max(0.)
            }
        };
        let negative_scale = if t < 1e-6 {
            2e6_f64.mul_add(t, 1.)
        } else {
            5e6_f64.mul_add(t.min(2e-6) - 1e-6, 3.)
        };
        let pulse = if t <= 0.5e-6 {
            4.
        } else if t < 0.6e-6 {
            508e7_f64.mul_add(t - 0.5e-6, 4.)
        } else if t < 2.6e-6 {
            512.
        } else if t < 2.7e-6 {
            508e7_f64.mul_add(2.7e-6 - t, 4.)
        } else {
            4.
        };
        let expected = [
            triangle(2e-6, 6.),
            -2. * (2e6 * t).clamp(1., 2.),
            triangle(1e-6, 1.),
            triangle(3e-6, 6.),
            1e6_f64.mul_add(t.min(1e-6), 2.),
            negative_scale,
            8.,
            0.,
            2.,
            pulse,
            triangle(32e-9, 1.),
            triangle(2e-6, 6.),
        ];
        for (index, (&actual, expected)) in row[1..].iter().zip(expected).enumerate() {
            near(actual, expected).with_context(|| format!("scaled waveform {index}, time {t}"))?;
        }
    }
    Ok(())
}

#[test]
fn vendor_pwl_scaling_rejects_only_unrepresentable_coordinates() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("source.cir");
    for card in [
        "V1 a 0 PWL TIME_SCALE_FACTOR=0 (0 0 1u 1)",
        "V1 a 0 PWL TIME_SCALE_FACTOR=-1 (0 0 1u 1)",
        "V1 a 0 PWL (1u 0 0 1)",
    ] {
        fs::write(&root, format!("title\n{card}\n.end\n"))?;
        assert!(prepared(&root, InputFormat::LtspiceDeck).is_err(), "{card}");
    }
    Ok(())
}

#[test]
fn parameterized_source_waveforms_bind_in_actual_scope_and_rebind_candidates() -> Result<()> {
    use model_normalize_ngspice::{linked_source::RootKind, parameter_scope::ModelLibrary};
    use ngspice_rs_preprocess::waveform::Forcing;
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("source.cir");
    fs::write(&root,b"title\n.param ltprep_waveform=99\n.subckt unit p n params: width=2u gain=2 shift=-1u scale=1 cycles=1.5\n.func twice(x) {x+x}\nVdrive p 0 DC {gain*10} PULSE(0 {gain} 0 0 0 {width} {width*5} {cycles}) AC 1\nIwave 0 n PWL TIME_SCALE_FACTOR={scale} VALUE_SCALE_FACTOR={gain} ({shift} 1 {shift+width} 3 {shift+width} 5 {shift+twice(width)} 0)\n.ends unit\nX1 a b unit\nX2 c d unit width=3u gain=3\n.end\n")?;
    let bundle = prepared(&root, InputFormat::LtspiceDeck)?;
    assert_eq!(bundle.waveforms.len(), 2);
    let text = std::str::from_utf8(&bundle.primary.bytes)?;
    assert!(text.contains("* mandatory prepared waveform binding for Vdrive"));
    assert!(text.contains("* mandatory prepared waveform binding for Iwave"));
    assert!(text.contains("AC 1"));
    assert!(!text.contains("PULSE("));
    let library = ModelLibrary::parse_source(&bundle.primary.bytes, RootKind::Deck)?;
    let scope = library
        .scopes
        .iter()
        .position(|item| {
            item.name
                .as_deref()
                .is_some_and(|name| name.eq_ignore_ascii_case("unit"))
        })
        .context("missing unit scope")?;
    let [pulse, pwl] = bundle.waveforms.as_slice() else {
        anyhow::bail!("missing source metadata");
    };
    assert_eq!(pulse.scope, ["unit"]);
    assert_eq!(pwl.scope, ["unit"]);
    for (width, gain) in [(2e-6, 2.), (3e-6, 3.), (2e-6, 2.)] {
        let overrides = [(scope, "width", width), (scope, "gain", gain)];
        let resolve = |expression: &str| library.evaluate_scalar(scope, expression, &overrides);
        let pulse = pulse.resolve(resolve)?;
        near(pulse.dc, gain * 10.)?;
        near(pulse.startup, 0.)?;
        let Forcing::PiecewiseLinear {
            knots,
            repeat: Some(repeat),
        } = pulse.forcing
        else {
            anyhow::bail!("missing pulse forcing");
        };
        near(repeat.period, width * 5.)?;
        near(
            repeat.limit.context("missing finite pulse count")?.time,
            width * 7.5,
        )?;
        near(knots[1].time, width * 0.1)?;
        near(knots[1].right, gain)?;
        let pwl = pwl.resolve(resolve)?;
        let Forcing::PiecewiseLinear {
            knots,
            repeat: None,
        } = pwl.forcing
        else {
            anyhow::bail!("missing PWL forcing");
        };
        assert_eq!(knots.len(), 3);
        near(knots[0].time, -1e-6)?;
        near(knots[1].time, width - 1e-6)?;
        near(knots[1].right, gain * 4.)?;
        near(pwl.dc, gain * (1. + 3e-6 / width))?;
        near(pwl.startup, pwl.dc)?;
    }
    let Provenance::Native { lines } = &bundle.primary.provenance else {
        anyhow::bail!("missing provenance");
    };
    assert_eq!(lines.len(), text.lines().count());
    for (line, origin) in text.lines().zip(lines) {
        if line.starts_with(".param ltprep_waveform_")
            || line.contains("mandatory prepared waveform")
        {
            assert!(
                [5, 6].contains(&origin.line),
                "{line} origin {}",
                origin.line
            );
        }
    }
    for source in &bundle.waveforms {
        let decoded: ngspice_rs_preprocess::waveform::PreparedWaveform =
            serde_json::from_slice(&serde_json::to_vec(source)?)?;
        assert_eq!(&decoded, source);
    }
    Ok(())
}

#[test]
fn vendor_sine_phase_and_finite_count_preserve_dc_and_startup() -> Result<()> {
    use model_normalize_ngspice::model_card::parse_spice_constant;
    use ngspice_rs_preprocess::waveform::Forcing;
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("source.cir");
    fs::write(&root,b"title\nV1 a 0 DC 7 SINE(1 2 1Meg -1u 1e5 30)\nV2 b 0 SIN(1 2 -1Meg 1u -1e5 30)\nV3 c 0 SINE(1 2 0 0 0 90)\nV4 d 0 DC 7 SINE(1 2 1Meg 1u 1e5 30 1.5) AC 2\nV5 e 0 SINE(1 2 -1Meg 0 1e5 30 1.5)\nV6 f 0 SINE(1 2 1Meg -2u 1e5 30 1.5)\nV7 g 0 SINE(1 2 0 1u 1e5 30)\n.end\n")?;
    let bundle = prepared(&root, InputFormat::LtspiceDeck)?;
    assert_eq!(bundle.waveforms.len(), 4);
    for (item, (dc, startup, frequency, stop_after)) in bundle.waveforms.iter().zip([
        (7., 2., 1e6, Some(1.5e-6)),
        (2., 2., -1e6, Some(-1.5e-6)),
        (0.139292023575, 0.139292023575, 1e6, Some(1.5e-6)),
        (2., 2., 0., None),
    ]) {
        let wave = item.resolve(parse_spice_constant)?;
        near(wave.dc, dc)?;
        near(wave.startup, startup)?;
        let Forcing::Sine {
            frequency: actual,
            phase,
            stop_after: cutoff,
            ..
        } = wave.forcing
        else {
            anyhow::bail!("missing sine forcing");
        };
        near(actual, frequency)?;
        near(phase, 30.)?;
        assert_eq!(cutoff.map(f64::to_bits), stop_after.map(f64::to_bits));
    }
    let mut deck = bundle
        .primary
        .bytes
        .strip_suffix(b".end\n")
        .context("missing terminator")?
        .to_vec();
    deck.extend_from_slice(b".control\nset numdgt=17\nset wr_singlescale\nop\nwrdata op.dat v(a) v(b) v(c)\ntran 5n 4u 0 5n\nwrdata tran.dat v(a) v(b) v(c)\nquit\n.endc\n.end\n");
    native(dir.path(), &deck)?;
    let rows = data(&dir.path().join("op.dat"))?;
    let row = rows.first().context("missing OP")?;
    for (&actual, expected) in row[1..].iter().zip([7., 2., 3.]) {
        near(actual, expected)?;
    }
    for row in data(&dir.path().join("tran.dat"))? {
        let time = row[0];
        let a_time = time + 1e-6;
        let b_time = (time - 1e-6).max(0.);
        let a = (2. * (-1e5 * a_time).exp()).mul_add(
            (std::f64::consts::TAU * 1e6 * a_time + 30_f64.to_radians()).sin(),
            1.,
        );
        let b = (2. * (1e5 * b_time).exp()).mul_add(
            (-std::f64::consts::TAU * 1e6 * b_time + 30_f64.to_radians()).sin(),
            1.,
        );
        for (&actual, expected) in row[1..].iter().zip([a, b, 3.]) {
            near(actual, expected)?;
        }
    }
    Ok(())
}

#[test]
fn voltage_source_series_resistance_preserves_identity_and_coefficients() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("source.cir");
    fs::write(&root, b"title\n.temp 27\nV1 a 0 DC 1 AC 1 Rser=2 Rser={500*2}\nRload a 0 1k noiseless\nV2 b 0 PULSE(0 1 0 1u 1u 2u 5u) Rser=2\nR2 b 0 3\nV3 c 0 1 Rser={2-2} Cpar=0\nR3 c 0 3\n.end\n")?;
    let bundle = prepared(&root, InputFormat::LtspiceDeck)?;
    let text = std::str::from_utf8(&bundle.primary.bytes)?;
    assert!(!text.contains("R__V"));
    assert!(text.contains("V1 a 0 DC 1 AC 1 pn_rseries={500*2}"));
    assert!(text.contains("V2 b 0 PULSE(") || text.contains("V2 b 0 pulse("));
    assert!(text.contains("V3 c 0 1 pn_rseries={2-2}"));
    let Provenance::Native { lines } = &bundle.primary.provenance else {
        anyhow::bail!("missing source map");
    };
    assert_eq!(lines.len(), text.lines().count());
    for (line, origin) in text.lines().zip(lines) {
        if line.starts_with("V1 a ") {
            assert_eq!(origin.line, 3);
        }
        if line.starts_with("V2 b ") {
            assert_eq!(origin.line, 5);
        }
    }
    Ok(())
}

#[test]
fn source_parasitics_without_equivalent_observations_are_diagnostic() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("source.cir");
    for card in [
        "V1 a 0 1 Rpar=1k",
        "I1 a 0 1 Rser=1k",
        "V1 a 0 1 temp=127",
        "V1 a 0 1 Rser=1 temp=127",
        "C1 a 0 1u dtemp=10",
        "C1 a 0 1u tc1=.01",
        "C1 a 0 1u tc2=.01",
        "C1 a 0 1u tnom=27",
        "L1 a 0 1m dtemp=10",
        "L1 a 0 1m tc=.01",
        "L1 a 0 1m tc3=.01",
    ] {
        fs::write(&root, format!("title\n{card}\n.end\n"))?;
        assert!(prepared(&root, InputFormat::LtspiceDeck).is_err(), "{card}");
    }
    Ok(())
}

#[test]
fn exponential_sources_preserve_independent_edges_and_source_bias() -> Result<()> {
    use model_normalize_ngspice::model_card::parse_spice_constant;
    use ngspice_rs_preprocess::waveform::Forcing;
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("source.cir");
    fs::write(&root, b"title\nV1 a 0 EXP(1 3 1u 2u 5u 3u)\nI1 0 b EXP(1 3 1u 2u 5u 3u)\nR1 b 0 1\nV2 c 0 DC 7 EXP(1 3 -1u 2u 5u 3u)\nV3 d 0 EXP(1 3 5u 2u 1u 3u)\nV4 e 0 EXP(1 3 0 2u 5u 3u)\nV5 f 0 EXP(1 3 -2u 2u -1u 3u)\n.end\n")?;
    let bundle = prepared(&root, InputFormat::LtspiceDeck)?;
    assert_eq!(bundle.waveforms.len(), 4);
    for (declaration, expected) in bundle.waveforms.iter().zip([
        (7., 1.786938680574733, -1e-6, 5e-6),
        (1., 1., 5e-6, 1e-6),
        (1., 1., 0., 5e-6),
        (1.6973037388046939, 1.6973037388046939, -2e-6, -1e-6),
    ]) {
        let waveform = declaration.resolve(parse_spice_constant)?;
        let Forcing::Exponential(exp) = waveform.forcing else {
            anyhow::bail!("expected exponential forcing");
        };
        let (dc, startup, rise, fall) = expected;
        near(waveform.dc, dc)?;
        near(waveform.startup, startup)?;
        near(exp.rise_delay, rise)?;
        near(exp.fall_delay, fall)?;
        near(exp.rise_tau, 2e-6)?;
        near(exp.fall_tau, 3e-6)?;
        let decoded: ngspice_rs_preprocess::waveform::PreparedWaveform =
            serde_json::from_slice(&serde_json::to_vec(declaration)?)?;
        assert_eq!(&decoded, declaration);
    }
    let mut deck = bundle
        .primary
        .bytes
        .strip_suffix(b".end\n")
        .context("missing terminator")?
        .to_vec();
    deck.extend_from_slice(b".control\nset numdgt=17\nset wr_singlescale\ntran 20n 10u 0 20n\nwrdata exp.dat v(a) v(b)\nquit\n.endc\n.end\n");
    native(dir.path(), &deck)?;
    for row in data(&dir.path().join("exp.dat"))? {
        let t = row[0];
        let rise = -(-(t - 1e-6).max(0.) / 2e-6).exp_m1();
        let fall = -(-(t - 5e-6).max(0.) / 3e-6).exp_m1();
        let expected = 2_f64.mul_add(rise - fall, 1.);
        near(row[1], expected)?;
        near(row[2], expected)?;
    }
    Ok(())
}

#[test]
fn exp_analysis_dependent_defaults_require_binding_instead_of_native_substitution() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("source.cir");
    for card in [
        "V1 a 0 EXP(1 3)",
        "V1 a 0 EXP(1 3 1u 0 5u 3u)",
        "V1 a 0 EXP(1 3 1u 2u 0 3u)",
        "V1 a 0 EXP(1 3 1u 2u 5u 0)",
    ] {
        fs::write(&root, format!("title\n{card}\n.end\n"))?;
        assert!(prepared(&root, InputFormat::LtspiceDeck).is_err(), "{card}");
    }
    Ok(())
}

#[test]
fn diode_dialect_retains_model_identity_and_ordered_setters() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("ideal.cir");
    let source = b"ideal diode\nD1 a 0 dm m=3 n=2\n.model dm D(Ron={r}\n+ Roff=100 Vfwd=1 Ilimit=0.25 Epsilon=0.4)\n.param r=2\n.subckt nested a b\nD2 a b dm\n.model dm D(IS=1p)\n.ends\n.end\n";
    fs::write(&root, source)?;
    let native = prepared(&root, InputFormat::NativeDeck)?;
    assert_eq!(native.primary.bytes, source);
    let translated = prepared(&root, InputFormat::LtspiceDeck)?;
    let text = std::str::from_utf8(&translated.primary.bytes)?;
    assert!(text.contains("D1 a 0 dm m=3 n=2"));
    assert!(text.contains(".model dm ltspice_diode(Ron={r}"));
    assert!(text.contains("+ Roff=100 Vfwd=1 Ilimit=0.25 Epsilon=0.4)"));
    assert!(text.contains(".model dm ltspice_diode(IS=1p)"));
    let Provenance::Native { lines } = &translated.primary.provenance else {
        anyhow::bail!("missing source mapping");
    };
    assert_eq!(lines.len(), text.lines().count());
    assert_eq!(lines[2].line, 3);
    assert_eq!(lines[3].line, 4);
    Ok(())
}

#[test]
fn switch_selection_retains_ordered_setters_flags_and_native_isolation() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("switch.cir");
    let source = b"title\nS1 a b c 0 swm ON\n.model swm SW(Ron=1 Roff=1Meg Vh={width} level=2\n+ level=1 level=2 Ilimit=1 Vser=0.1 Lser=0 Epsilon=0.2 ONEWAY)\n.param width=-1\n.model native_csw CSW(Ron=2 Roff=1Meg)\n.end\n";
    fs::write(&root, source)?;
    assert_eq!(
        prepared(&root, InputFormat::NativeDeck)?.primary.bytes,
        source
    );
    let translated = prepared(&root, InputFormat::LtspiceDeck)?;
    let expected = std::str::from_utf8(source)?.replace(" SW(", " ltspice_switch(");
    assert_eq!(translated.primary.bytes, expected.as_bytes());
    let Provenance::Native { lines } = translated.primary.provenance else {
        anyhow::bail!("missing source mapping");
    };
    assert_eq!(lines.len(), expected.lines().count());
    assert_eq!(lines[2].line, 3);
    assert_eq!(lines[3].line, 4);
    Ok(())
}

#[test]
fn diode_dispatch_is_deferred_to_each_resolved_occurrence() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("source.cir");
    let source = b"title\n.model plain D\n.model fn D(Level=2 Tox=50 L=2 W=3)\n.model limit_only D(Ilimit=1 Epsilon=0.2)\n.model ordered d(Level=1 level=2 Ron=2)\n.model reversed D(level=2 LEVEL=1 Ron=2)\n.subckt cell p n sel=1\n.model dm D(Level={sel} Ron=2\n+ Roff=100 Vfwd=1)\nD1 p n dm m=2 n=3\n.ends cell\nX1 a 0 cell sel=1\nX2 b 0 cell sel=2\nX3 c 0 cell sel=3\n.end\n";
    fs::write(&root, source)?;
    assert_eq!(
        prepared(&root, InputFormat::NativeDeck)?.primary.bytes,
        source
    );
    let translated = prepared(&root, InputFormat::LtspiceDeck)?;
    let expected = std::str::from_utf8(source)?
        .replace(" D(", " ltspice_diode(")
        .replace(" d(", " ltspice_diode(")
        .replace("plain D\n", "plain ltspice_diode\n");
    assert_eq!(translated.primary.bytes, expected.as_bytes());
    let Provenance::Native { lines } = &translated.primary.provenance else {
        anyhow::bail!("missing source mapping");
    };
    assert_eq!(lines.len(), expected.lines().count());
    for (index, line) in lines.iter().enumerate() {
        assert_eq!(line.line, index + 1);
    }
    Ok(())
}
