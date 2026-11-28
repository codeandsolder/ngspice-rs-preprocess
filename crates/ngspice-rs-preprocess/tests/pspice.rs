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
        assert_ne!(native, [] as [Vec<f64>; 0]);
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
    let tolerance = 2e-7 * expected.abs().max(1e-6);
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
            (x.signum() * x).mul_add(x, x.mul_add(x, x.clamp(-1., 3.))) + step - 2. + condition,
        );
        close(
            row[2],
            2. * if x <= 0. {
                3.0_f64.mul_add(x, 5.)
            } else {
                5.0_f64.mul_add(x, 5.)
            },
        );
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
        close(row[2], (65.0_f64 + 273.15).mul_add(8.617_330_3e-5, 66.));
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
            -1. / (1000. * ((0.0001_f64 * 38.).mul_add(38., 0.01_f64.mul_add(38., 1.)))),
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
    let Provenance::Native { lines, .. } = &prepared.primary.provenance else {
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
        let weight = x * x * (2.0_f64.mul_add(-x, 3.));
        let resistance = ((10_f64.ln() - 1000_f64.ln()).mul_add(weight, 1000_f64.ln())).exp();
        close(row[1], 100. / (100. + resistance));
        // Evaluate the input loading in picoamps, not with a voltage-scale tolerance.
        close(row[2] * 1e12, -row[0]);
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

#[test]
fn switch_identity_tracks_source_scope_and_collision_free_native_name() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("root.cir");
    let lib = dir.path().join("cell.lib");
    fs::write(
        &root,
        b"Switch identities\n.lib cell.lib\nX1 a b c 0 CELL\n.end\n",
    )?;
    fs::write(&lib, b".subckt CELL a b c n\nS1 a b c n SM\n.model SM VSWITCH(VON=1 VOFF=0 RON=10 ROFF=1k)\nR1 a A__ps_S1 1k\n.ends CELL\n")?;
    let prepared = prepare_source(&root, InputFormat::PspiceDeck)?;
    assert_eq!(prepared.device_identities.len(), 1);
    let identity = &prepared.device_identities[0];
    assert_eq!(identity.source, lib.canonicalize()?.to_string_lossy());
    assert_eq!(identity.scope, ["CELL"]);
    assert_eq!(identity.declared, "S1");
    assert_eq!(identity.displayed, "S1");
    assert_ne!(identity.linked.to_ascii_lowercase(), "a__ps_s1");
    let native = std::str::from_utf8(&prepared.primary.bytes)?;
    assert!(native.contains(&format!("{} %gd(c n) %gd(a b) SM", identity.linked)));
    Ok(())
}

#[test]
fn continuous_current_switch_matches_vendor_cubic_log_resistance() -> Result<()> {
    let source = br"PSpice current switch
I1 0 c -1
V1 c 0 0
V2 supply 0 1
W1 supply out V1 MOD1
R1 out 0 100
.model MOD1 ISWITCH(ION=1 IOFF=0 RON=10 ROFF=1000)
.control
set wr_singlescale
set wr_vecnames
dc I1 -0.5 1.5 0.25
wrdata response.dat v(out) i(V1)
quit
.endc
.end
";
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("source.cir");
    fs::write(&path, source)?;
    let prepared = prepare_source(&path, InputFormat::PspiceDeck)?;
    // The C compatibility frontend substitutes aswitch (log-linear),
    // whereas the vendor specifies the cubic law for current control.
    if let Some(values) = rows(
        &directory.path().join("native"),
        &prepared.primary.bytes,
        false,
    )? {
        assert_eq!(values.len(), 9);
        for row in values {
            let x = row[0].clamp(0., 1.);
            let weight = x * x * (2.0_f64.mul_add(-x, 3.));
            let resistance = ((10_f64.ln() - 1000_f64.ln()).mul_add(weight, 1000_f64.ln())).exp();
            close(row[1], 100. / (100. + resistance));
            close(row[2], row[0]);
        }
    }
    let identity = prepared
        .device_identities
        .first()
        .context("missing current switch identity")?;
    assert_eq!(identity.declared, "W1");
    assert!(identity.linked.starts_with('A'));
    Ok(())
}

#[test]
fn nested_braces_are_grouping_inside_parameter_and_model_expressions() -> Result<()> {
    let source = br"Nested expressions
.param a=2 b={{a+1}*2}
V1 in 0 {b}
X1 in out CELL
.subckt CELL a b params: r=1k
R1 a b {{r*2}+r}
.ends
R2 out 0 3k
.model DM D(IS={{1e-12*2}+1e-12})
D1 0 out DM
.control
set wr_singlescale
set wr_vecnames
op
wrdata response.dat v(in) v(out)
quit
.endc
.end
";
    compare(source, |row| {
        close(row[1], 6.);
        close(row[2], 3.);
    })
}

#[test]
fn cur_table_and_conditional_local_ground_preserve_sign_and_topology() -> Result<()> {
    let source = br"Current table and conditional ground
V1 in 0 0.5
G1 out 0 CUR={TABLE(V(in),0,0,1,1m)}
R1 out 0 1k
V2 source 0 3
X1 source divided CELL
.subckt CELL GND out
.param a=2
.if (a>1)
R1 GND out 1k
.else
R1 GND out 2k
.endif
.ends
R2 divided 0 1k
.control
set wr_singlescale
set wr_vecnames
op
wrdata response.dat v(out) v(divided)
quit
.endc
.end
";
    compare(source, |row| {
        close(row[1], -0.5);
        close(row[2], 1.5);
    })
}

#[test]
fn model_temperature_inheritance_preserves_measurement_and_operating_coordinates() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("source.cir");
    let source = br"Temperature model inheritance
.param base_temp=25 delta=10
V1 a 0 1
V2 b 0 1
V3 c 0 1
V4 d 0 1
V5 e 0 1
R1 a 0 ABSOLUTE 1k
R2 b 0 RELATIVE 1k
R3 c 0 DERIVED 1k
R4 d 0 AMBIENT 1k
R5 e 0 DERIVED2 1k
.model ABSOLUTE RES(R=1 TC1=0.001 T_MEASURED=0 T_ABS={base_temp})
.model RELATIVE AKO:ABSOLUTE RES(T_REL_GLOBAL={delta})
.model DERIVED AKO:ABSOLUTE RES(T_REL_LOCAL={delta})
.model AMBIENT AKO:RELATIVE RES(T_REL_LOCAL={delta})
.model DERIVED2 AKO:DERIVED RES(T_REL_LOCAL=5)
.temp 100
.control
set wr_singlescale
set wr_vecnames
op
wrdata response.dat i(v1) i(v2) i(v3) i(v4) i(v5)
quit
.endc
.end
";
    fs::write(&path, source)?;
    let prepared = prepare_source(&path, InputFormat::PspiceDeck)?;
    let native = std::str::from_utf8(&prepared.primary.bytes)?;
    assert!(!native.to_ascii_lowercase().contains("t_rel_"));
    assert!(!native.to_ascii_lowercase().contains("t_abs"));
    assert!(!native.to_ascii_lowercase().contains("t_measured"));
    // Independent vendor-defined temperature coordinates. The C compatibility
    // frontend does not implement T_REL_LOCAL and can miss T_MEASURED when
    // T_ABS also occurs, so it is not the oracle for this particular case.
    if let Some(rows) = rows(
        &directory.path().join("native"),
        &prepared.primary.bytes,
        false,
    )? {
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].len(), 6);
        for (&current, temperature) in rows[0][1..].iter().zip([25., 110., 35., 100., 40.]) {
            close(
                current,
                -1. / (1000. * (0.001_f64.mul_add(temperature, 1.))),
            );
        }
    }
    Ok(())
}

#[test]
fn conflicting_model_temperature_customizations_fail() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("source.lib");
    for input in [
        ".model D1 D(T_ABS=20 T_REL_GLOBAL=5)\n",
        ".model D1 D(T_REL_LOCAL=5)\n",
        ".model D1 D(T_ABS=20)\n.model D2 AKO:D1 D(T_REL_LOCAL=5 T_ABS=30)\n",
    ] {
        fs::write(&path, input)?;
        assert!(
            prepare_source(&path, InputFormat::PspiceLibrary).is_err(),
            "{input}"
        );
    }
    Ok(())
}

#[test]
fn passive_model_multipliers_bind_in_declaration_scope_and_per_occurrence() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("source.cir");
    let source = br"Scoped passive multipliers
.param scale=2
V1 a 0 1
V2 b 0 1
V3 c 0 1
V4 d 0 1
X1 a 0 GLOBAL
X2 b 0 LOCAL PARAMS: scale=3
X3 c 0 LOCAL PARAMS: scale=7
R4 d 0 DERIVED 1k
.subckt GLOBAL p n
.param scale=9
R1 p n RM 1k
.ends GLOBAL
.subckt LOCAL p n PARAMS: scale=1
.model RM RES(R={scale})
XH p n NESTED
.subckt NESTED a b
R1 a b RM 1k
.ends NESTED
.ends LOCAL
.model RM RES(R={scale})
.model DERIVED AKO: RM RES(R=4)
.control
set wr_singlescale
set wr_vecnames
op
wrdata response.dat i(v1) i(v2) i(v3) i(v4)
quit
.endc
.end
";
    fs::write(&path, source)?;
    let prepared = prepare_source(&path, InputFormat::PspiceDeck)?;
    if let Some(values) = rows(
        &directory.path().join("native"),
        &prepared.primary.bytes,
        false,
    )? {
        assert_eq!(values.len(), 1);
        for (&current, factor) in values[0][1..].iter().zip([2., 3., 7., 4.]) {
            close(current, -1. / (1000. * factor));
        }
    }
    let Provenance::Native { lines, .. } = &prepared.primary.provenance else {
        anyhow::bail!("missing passive multiplier provenance");
    };
    let emitted = std::str::from_utf8(&prepared.primary.bytes)?;
    assert_eq!(lines.len(), emitted.lines().count());
    for (text, origin) in emitted.lines().zip(lines) {
        if text.starts_with(".param ps_passive_") {
            assert!(
                std::str::from_utf8(source)?
                    .lines()
                    .nth(origin.line - 1)
                    .context("missing original model card")?
                    .starts_with(".model")
            );
        }
    }
    Ok(())
}

#[test]
fn linear_passive_model_multipliers_preserve_complex_ac_admittance() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("source.cir");
    let source = br"Passive AC analytic fixture
.param factor=2
V1 r 0 AC 1
V2 c 0 AC 1
I1 0 l AC 1
R1 r 0 RM 1k
C1 c 0 CM 1n
L1 l 0 LM 1m
.model RM RES(R={factor} TC1=0.001 T_MEASURED=0 T_ABS=25)
.model CM CAP(C={factor} TC1=0.001 T_MEASURED=0 T_ABS=25 VC1=0 VC2=0)
.model LM IND(L={factor} TC1=0.001 T_MEASURED=0 T_ABS=25 IL1=0 IL2=0)
.temp 100
.control
set wr_singlescale
set wr_vecnames
ac lin 3 1000 3000
wrdata response.dat i(v1) i(v2) v(l)
quit
.endc
.end
";
    fs::write(&path, source)?;
    let prepared = prepare_source(&path, InputFormat::PspiceDeck)?;
    if let Some(values) = rows(
        &directory.path().join("native"),
        &prepared.primary.bytes,
        false,
    )? {
        assert_eq!(values.len(), 3);
        for row in values {
            assert_eq!(row.len(), 7);
            let omega = std::f64::consts::TAU * row[0];
            let factor = 2. * 1.025;
            let expected = [
                -1. / (1000. * factor),
                0.,
                0.,
                -omega * 1e-9 * factor,
                0.,
                omega * 1e-3 * factor,
            ];
            for (&value, expected) in row[1..].iter().zip(expected) {
                close(value, expected);
            }
        }
    }
    Ok(())
}

#[test]
fn nonlinear_passive_models_preserve_scope_temperature_and_initial_coordinates() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("source.cir");
    let source = br"Polynomial passive declaration binding
.param a=0.2
.model CM CAP(C=2 VC1={a} VC2=0.07 TC1=0.001 T_MEASURED=0 T_ABS=25)
.model LM IND(L=3 IL1=-0.1 IL2=0.02 TC2=0.001 T_REL_GLOBAL=5)
C1 c 0 CM 1n IC=-2
L1 l 0 LM 1m IC=-0.5
.subckt cell p n params: a=0.4 base=2
.model CM CAP(C={base} VC1={a})
C2 p n CM {base*1n} IC={-a}
.ends cell
X1 c 0 cell a=0.1 base=3
X2 l 0 cell a=0 base=4
.end
";
    fs::write(&path, source)?;
    let prepared = prepare_source(&path, InputFormat::PspiceDeck)?;
    let text = std::str::from_utf8(&prepared.primary.bytes)?.to_ascii_lowercase();
    let cards = model_normalize_ngspice::model_card::parse_model_cards(&prepared.primary.bytes)?;
    let model = cards
        .iter()
        .find(|card| card.name.eq_ignore_ascii_case("CM"))
        .context("missing original capacitor model")?;
    assert_eq!(model.family, "pspice_cap");
    assert_eq!(
        cards
            .iter()
            .filter(|card| card.family == "pspice_cap")
            .count(),
        2
    );
    assert_eq!(
        cards
            .iter()
            .filter(|card| card.family == "pspice_ind")
            .count(),
        1
    );
    for field in [
        "vc1={a}",
        "vc2=0.07",
        "tc1=0.001",
        "tnom=0",
        "temp=25",
        "il1=-0.1",
        "il2=0.02",
        "tc2=0.001",
        "dtemp=5",
    ] {
        assert!(text.contains(field), "{field}: {text}");
    }
    assert!(text.contains("c1 c 0 cm 1n ic=-2"), "{text}");
    assert!(text.contains("l1 l 0 lm 1m ic=-0.5"), "{text}");
    assert!(text.contains("c2 p n cm {base*1n} ic={-a}"), "{text}");
    assert!(!text.contains("ps_passive_"), "{text}");
    Ok(())
}

#[test]
fn nonlinear_passive_coefficients_require_values_and_compatible_instances() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("source.lib");
    for source in [
        ".model CM CAP(VC1)\n",
        ".model CM CAP(VC2=0.1 C)\n",
        ".model LM IND(IL2)\n",
        ".model LM IND(IL1=0.1)\nC1 a b LM 1n\n",
        ".model CM CAP(VC1=0.1)\nC1 a b CM IC=1\n",
    ] {
        fs::write(&path, source)?;
        assert!(
            prepare_source(&path, InputFormat::PspiceLibrary).is_err(),
            "{source}"
        );
    }
    Ok(())
}

#[test]
fn ako_nonlinear_storage_inherits_coefficients_and_local_temperature() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("source.lib");
    fs::write(
        &path,
        br".model base CAP(C=2 VC1=0.1 VC2=0.02 T_ABS=25)
.model child AKO:base CAP(VC2=0.03 T_REL_LOCAL=5)
.model linear AKO:base CAP(VC1=0 VC2=0)
",
    )?;
    let prepared = prepare_source(&path, InputFormat::PspiceLibrary)?;
    let text = std::str::from_utf8(&prepared.primary.bytes)?.to_ascii_lowercase();
    let cards = model_normalize_ngspice::model_card::parse_model_cards(&prepared.primary.bytes)?;
    let child = cards
        .iter()
        .find(|card| card.name.eq_ignore_ascii_case("child"))
        .context("missing inherited storage model")?;
    assert_eq!(child.family, "pspice_cap");
    let field = |name: &str| {
        child
            .parameters
            .iter()
            .rev()
            .find(|p| p.name.eq_ignore_ascii_case(name))
            .and_then(|p| p.expression.as_deref())
    };
    assert_eq!(field("vc1"), Some("0.1"));
    assert_eq!(field("vc2"), Some("0.03"));
    assert!(
        field("temp").is_some_and(|temp| temp.contains("25") && temp.contains("5")),
        "{text}"
    );
    let linear = cards
        .iter()
        .find(|card| card.name.eq_ignore_ascii_case("linear"))
        .context("missing linear override")?;
    assert!(linear.family.eq_ignore_ascii_case("c"), "{text}");
    Ok(())
}

#[test]
fn switch_control_loading_and_gmin_builtin_follow_circuit_option() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("source.cir");
    let source = br"Switch control loading
.options gmin=1e-9
V1 a 0 1
V2 b 0 1
V3 supply 0 1
S1 supply out a 0 THRESHOLD
S2 supply out2 b 0 CONTINUOUS
R1 out 0 100
R2 out2 0 100
.model THRESHOLD VSWITCH(VT=0.5 VH=0 TD=0 RON=10 ROFF=1000)
.model CONTINUOUS VSWITCH(RON=10 ROFF=1000)
E1 report 0 VALUE={1e9*GMIN}
R3 report 0 1k
.control
set wr_singlescale
set wr_vecnames
op
wrdata response.dat i(v1) i(v2) v(out) v(out2) v(report)
quit
.endc
.end
";
    fs::write(&path, source)?;
    let prepared = prepare_source(&path, InputFormat::PspiceDeck)?;
    if let Some(values) = rows(
        &directory.path().join("native"),
        &prepared.primary.bytes,
        false,
    )? {
        assert_eq!(values.len(), 1);
        let row = &values[0];
        close(row[1] * 1e9, -1.);
        close(row[2] * 1e9, -1.);
        close(row[3], 100. / 110.);
        close(row[4], 100. / 110.);
        close(row[5], 1.);
    }
    Ok(())
}

#[test]
fn laplace_source_dialects_preserve_rational_numerator_and_poles() -> Result<()> {
    let directory = tempfile::tempdir()?;
    for (name, format, voltage, current) in [
        (
            "pspice",
            InputFormat::PspiceDeck,
            "E1 out 0 LAPLACE {V(in,ref)} = {(2+3*s*tau)/(1+2*s*tau+(s*tau)^2)}",
            "G1 gout 0 LAPLACE {V(in,ref)} = {(2+3*s*tau)/(1+2*s*tau+(s*tau)^2)}",
        ),
        (
            "simetrix",
            InputFormat::SimetrixDeck,
            "E1 out 0 LAPLACE {V(in,ref)} {(2+3*s*tau)/(1+2*s*tau+(s*tau)^2)}",
            "G1 gout 0 LAPLACE {V(in,ref)} {(2+3*s*tau)/(1+2*s*tau+(s*tau)^2)}",
        ),
        (
            "ltspice",
            InputFormat::NativeDeck,
            "E1 out 0 in ref Laplace=(2+3*s*tau)/(1+2*s*tau+(s*tau)^2)",
            "G1 gout 0 in ref Laplace=(2+3*s*tau)/(1+2*s*tau+(s*tau)^2)",
        ),
    ] {
        let path = directory.path().join(format!("{name}.cir"));
        let source = format!(
            "* Rational transfer syntax\n.param tau=1m\nV1 in ref dc 1 ac 1\nVref ref 0 3\n{voltage}\n{current}\nR1 out 0 1k\nRg gout 0 1\n.end\n"
        );
        fs::write(&path, &source)?;
        let prepared =
            prepare_source(&path, format).with_context(|| format!("preparing {name}"))?;
        let native = std::str::from_utf8(&prepared.primary.bytes)?;
        assert!(native.contains("s_xfer"), "{name}: {native}");
        assert!(
            !native.to_ascii_lowercase().contains(" laplace"),
            "{name}: {native}"
        );
        // The analysis harness is deliberately native and independent of
        // the source dialect's analysis declaration projection.
        let native = format!(
            "{}.control\nset wr_singlescale\nset wr_vecnames\nac lin 5 100 500\nwrdata response.dat v(out) v(gout)\nquit\n.endc\n.end\n",
            native
                .strip_suffix(".end\n")
                .context("missing native terminator")?
        );
        let result = rows(&directory.path().join(name), native.as_bytes(), false)
            .with_context(|| format!("native {name}: {native}"))?;
        if let Some(values) = result {
            assert_eq!(values.len(), 5);
            for row in values {
                let [frequency, real, imaginary, current_real, current_imaginary] = row.as_slice()
                else {
                    anyhow::bail!("unexpected AC row {row:?}");
                };
                // Evaluate the second-order rational function independently at s=jω.
                let omega = std::f64::consts::TAU * frequency * 0.001;
                let dr = omega.mul_add(-omega, 1.);
                let di = 2. * omega;
                let ni = 3. * omega;
                let scale = dr.mul_add(dr, di * di);
                close(*real, 2.0_f64.mul_add(dr, ni * di) / scale);
                close(*imaginary, ni.mul_add(dr, -2. * di) / scale);
                close(*current_real, -*real);
                close(*current_imaginary, -*imaginary);
            }
        }
    }
    Ok(())
}

#[test]
fn singleton_polynomial_gain_survives_native_pspice_admission() -> Result<()> {
    let source = br"PSpice singleton polynomial
V1 in 0 -2
Vs sense 0 0
Is 0 sense -2
E1 e 0 POLY(1) in 0 7
H1 h 0 POLY(1) Vs 7
G1 0 g POLY(1) in 0 7
F1 0 f POLY(1) Vs 7
Rg g 0 1
Rf f 0 1
.control
set wr_singlescale
set wr_vecnames
op
wrdata response.dat v(e) v(h) v(g) v(f)
quit
.endc
.end
";
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("source.cir");
    fs::write(&path, source)?;
    let prepared = prepare_source(&path, InputFormat::PspiceDeck)?;
    if let Some(values) = rows(
        &directory.path().join("native"),
        &prepared.primary.bytes,
        false,
    )? {
        assert_eq!(values.len(), 1);
        assert_eq!(values[0].len(), 5);
        for &value in &values[0][1..] {
            close(value, -14.);
        }
    }
    Ok(())
}
