use anyhow::{Context, Result, ensure};
use ngspice_rs_preprocess::{
    Cancellation, InputFormat, Options, PreparedBundle, Provenance, Request, SourceInput, prepare,
};
use std::{fs, path::Path, process::Command};

fn prepare_source(path: &Path, format: InputFormat) -> Result<PreparedBundle> {
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

fn rows(directory: &Path, source: &[u8], oracle: bool) -> Result<Option<Vec<Vec<f64>>>> {
    fs::create_dir_all(directory)?;
    fs::write(directory.join("deck.cir"), source)?;
    let mut command = Command::new("ngspice");
    if oracle {
        fs::write(directory.join(".spiceinit"), b"set ngbehavior=psa\n")?;
    } else {
        command.arg("-n");
    }
    let result = command
        .args(["-b", "deck.cir"])
        .current_dir(directory)
        .output();
    let output = match result {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    ensure!(
        output.status.success(),
        "ngspice: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let data = fs::read_to_string(directory.join("response.dat"))?;
    let values = data
        .lines()
        .skip(1)
        .map(|line| {
            line.split_whitespace()
                .map(str::parse::<f64>)
                .collect::<std::result::Result<Vec<_>, _>>()
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(Some(values))
}

fn compare(source: &[u8], expected: impl Fn(&[f64])) -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("source.cir");
    fs::write(&path, source)?;
    let prepared = prepare_source(&path, InputFormat::PspiceDeck)?;
    let native = rows(
        &directory.path().join("native"),
        &prepared.primary.bytes,
        false,
    )?;
    let oracle = rows(&directory.path().join("oracle"), source, true)?;
    if let (Some(native), Some(oracle)) = (native, oracle) {
        assert_eq!(native.len(), oracle.len());
        assert!(!native.is_empty());
        for (actual, reference) in native.iter().zip(oracle) {
            assert_eq!(actual.len(), reference.len());
            for (&value, expected) in actual.iter().zip(reference) {
                close(value, expected);
            }
            expected(actual);
        }
    }
    Ok(())
}

fn close(actual: f64, expected: f64) {
    let tolerance = 2e-7 * expected.abs().max(1.0);
    assert!(
        (actual - expected).abs() <= tolerance,
        "{actual} != {expected}"
    );
}

#[test]
fn behavioral_functions_and_table_match_psa_and_analytic_dc_values() -> Result<()> {
    let source = br"PSpice functions
V1 in 0 -2
E1 out 0 VALUE={LIMIT(V(in),3,-1)+PWR(V(in),2)+PWRS(V(in),2)+STP(V(in))+INT(-2.8)+IF((V(in)<0)&~(V(in)>0),10,20)}
E2 tab 0 VALUE={TABLE(V(in),-1,2,0,5,1,10)*2}
R1 out 0 1k
R2 tab 0 1k
.control
set wr_singlescale
set wr_vecnames
dc V1 -2 2 0.5
wrdata response.dat v(out) v(tab)
quit
.endc
.end
";
    compare(source, |row| {
        let x = row[0];
        let step = if x < 0. {
            0.
        } else if x > 0. {
            1.
        } else {
            0.5
        };
        let condition = if x < 0. { 10. } else { 20. };
        close(
            row[1],
            x.clamp(-1., 3.) + x * x + x.signum() * x * x + step - 2. + condition,
        );
        close(row[2], 2. * if x <= 0. { 5. + 3. * x } else { 5. + 5. * x });
    })
}

#[test]
fn ground_identity_and_temperature_parameters_preserve_physics() -> Result<()> {
    let source = br"PSpice local ground and temperature
V1 in 0 3
X1 in out CELL
.SUBCKT CELL GND out
R1 GND out 1k
.ENDS CELL
R2 out 0 1k
Etemp t 0 VALUE={TEMP+VT+1e12*GMIN}
Rtemp t 0 1k
X2 subtemp 0 TEMP_CELL
.SUBCKT TEMP_CELL o n
E1 o n VALUE={TEMP+VT+1e12*GMIN}
.ENDS TEMP_CELL
Rsub subtemp 0 1k
.temp 65
.control
set wr_singlescale
set wr_vecnames
op
wrdata response.dat v(out) v(t) v(subtemp)
quit
.endc
.end
";
    compare(source, |row| {
        close(row[1], 1.5);
        close(row[2], 66. + (65. + 273.15) * 8.6173303e-5);
        close(row[3], row[2]);
    })
}

#[test]
fn user_functions_shadow_compatibility_helpers_in_their_own_scope() -> Result<()> {
    let source = br"PSpice lexical function definitions
V1 in 0 3
E1 out 0 VALUE={PWR(V(in),2)}
X1 in sub CELL
.SUBCKT CELL a b
.func pwr(x,a) {2*x}
E1 b 0 VALUE={PWR(V(a),2)}
.ENDS CELL
R1 out 0 1k
R2 sub 0 1k
.control
set wr_singlescale
set wr_vecnames
op
wrdata response.dat v(out) v(sub)
quit
.endc
.end
";
    compare(source, |row| {
        close(row[1], 9.);
        close(row[2], 6.);
    })
}

#[test]
fn model_inheritance_and_positional_area_tc_and_subcircuit_parameters() -> Result<()> {
    let source = br"PSpice model and instance mappings
V1 in 0 0.6
D1 in 0 local 2
.model base D(IS=1e-12 N=1)
.model local AKO:base D(IS=2e-12)
V2 res 0 1
R1 res 0 1k TC=0.01,0.0001
X1 out 0 CELL PARAMS: gain=3, offset=4
.SUBCKT CELL a b PARAMS: gain=1, offset=2
E1 a b VALUE={gain+offset}
.ENDS CELL
R2 out 0 1k
.temp 65
.control
set wr_singlescale
set wr_vecnames
op
wrdata response.dat i(V1) i(V2) v(out)
quit
.endc
.end
";
    compare(source, |row| {
        assert!(row[1] < 0.);
        close(
            row[2],
            -1. / (1000. * (1. + 0.01 * 38. + 0.0001 * 38. * 38.)),
        );
        close(row[3], 7.);
    })
}

#[test]
fn library_imports_are_mapped_and_native_mode_does_not_change() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let root = directory.path().join("root.cir");
    let library = directory.path().join("part.lb");
    fs::write(
        &root,
        b"Title\n.lib \"part.lb\"\nX1 out 0 CELL PARAMS: a=2, b=4\n.end\n",
    )?;
    fs::write(&library, b"*#PSPICE\n.subckt CELL o n PARAMS: a=1, b=2\nE1 o n VALUE={IF(a>0,a+b,0)}\n.ends CELL\n.end\n")?;
    let prepared = prepare_source(&root, InputFormat::PspiceDeck)?;
    let text = std::str::from_utf8(&prepared.primary.bytes)?;
    assert!(text.starts_with("Title\n.func "));
    assert!(text.contains("* #PSPICE"));
    assert!(text.contains("X1 out 0 CELL PARAMS: a=2  b=4"));
    assert!(text.ends_with(".end\n"));
    let Provenance::Native { lines } = &prepared.primary.provenance else {
        anyhow::bail!("missing PSpice source map");
    };
    assert_eq!(lines.len(), text.lines().count());
    let (_, origin) = text
        .lines()
        .zip(lines)
        .find(|(line, _)| line.starts_with("E1"))
        .context("missing source")?;
    assert_eq!(origin.source, library.canonicalize()?);
    assert_eq!(origin.line, 3);
    assert!(
        prepare_source(&root, InputFormat::NativeDeck).is_err()
            || !prepare_source(&root, InputFormat::NativeDeck)?
                .primary
                .bytes
                .windows(5)
                .any(|w| w == b".func")
    );
    let include = prepare_source(&library, InputFormat::PspiceLibrary)?;
    assert!(
        !std::str::from_utf8(&include.primary.bytes)?
            .lines()
            .any(|line| line.eq_ignore_ascii_case(".end"))
    );
    fs::write(&library, b"*#HSPICE\nR1 in 0 1k\n")?;
    assert!(prepare_source(&root, InputFormat::PspiceDeck).is_err());
    Ok(())
}

#[test]
fn model_scope_resolution_and_unsupported_physics_are_explicit() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("test.lib");
    for body in [
        ".model A AKO:B D(IS=1e-12)\n.model B AKO:A D(IS=1e-12)\n",
        ".model A AKO:MISSING D(IS=1e-12)\n",
        ".model A D(IS=1e-12)\n.model B AKO:A NPN(BF=80)\n",
        ".model A NMOS(LEVEL=5)\n",
        ".model A NPN(LEVEL=2)\n",
        ".model A D(IS=1e-12 DEV=5%)\n",
        ".distribution foo\n",
        "U1 INV(1) 0 a b D0\n",
    ] {
        fs::write(&path, body)?;
        assert!(
            prepare_source(&path, InputFormat::PspiceLibrary).is_err(),
            "{body}"
        );
    }
    fs::write(&path,b".model base D(IS=1e-12)\n.subckt CELL a b\n.model base D(IS=2e-12)\n.model child AKO:base D(N=2)\nD1 a b child\n.ends CELL\n.model global_child AKO:base D(N=3)\n")?;
    let prepared = prepare_source(&path, InputFormat::PspiceLibrary)?;
    let text = std::str::from_utf8(&prepared.primary.bytes)?;
    assert!(text.contains(".model child D(IS=2e-12 N=2)"), "{text}");
    assert!(
        text.contains(".model global_child D(IS=1e-12 N=3)"),
        "{text}"
    );
    Ok(())
}

#[test]
fn continuous_voltage_switch_matches_cubic_logarithmic_law() -> Result<()> {
    let source = br"PSpice voltage switch
V1 control 0 -1
V2 supply 0 1
S1 supply out control 0 MOD1
R1 out 0 100
.model MOD1 VSWITCH(VON=1 VOFF=0 RON=10 ROFF=1000)
.control
set wr_singlescale
set wr_vecnames
dc V1 -0.5 1.5 0.25
wrdata response.dat v(out) i(V1)
quit
.endc
.end
";
    compare(source, |row| {
        let x = row[0].clamp(0., 1.);
        let weight = x * x * (3. - 2. * x);
        let resistance = (1000_f64.ln() + (10_f64.ln() - 1000_f64.ln()) * weight).exp();
        close(row[1], 100. / (100. + resistance));
        close(row[2], -row[0] / 1e12);
    })
}

#[test]
fn explicit_global_ground_and_literal_dotted_nodes_are_preserved() -> Result<()> {
    let source = br"PSpice global ground
.global gnd
V1 in 0 3
X1 in out CELL
.SUBCKT CELL GND out
R1 GND out 1k
.ENDS CELL
R2 out 0 1k
V2 literal.gnd 0 2
.control
set wr_singlescale
set wr_vecnames
op
wrdata response.dat v(out) v(literal.gnd)
quit
.endc
.end
";
    compare(source, |row| {
        close(row[1], 0.);
        close(row[2], 2.);
    })
}
