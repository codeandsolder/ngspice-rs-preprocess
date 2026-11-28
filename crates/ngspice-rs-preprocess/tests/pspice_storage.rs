//! Independent integrated-charge witness, not a PSpice dialect oracle.
use anyhow::{Context, Result, ensure};
use ngspice_rs_preprocess::passive_storage::PolynomialStorage;
use std::{fs, path::Path, process::Command};

fn table<const N: usize>(path: &Path) -> Result<Vec<[f64; N]>> {
    fs::read_to_string(path)?
        .lines()
        .skip(1)
        .map(|line| {
            line.split_whitespace()
                .map(str::parse)
                .collect::<std::result::Result<Vec<f64>, _>>()?
                .try_into()
                .map_err(|_| anyhow::anyhow!("unexpected witness table shape"))
        })
        .collect()
}

fn close(actual: f64, expected: f64, relative: f64, absolute: f64) {
    assert!(
        (actual - expected).abs() <= relative * expected.abs() + absolute,
        "{actual} != {expected}"
    );
}

fn integral(rows: &[[f64; 5]], ordinate: impl Fn(&[f64; 5]) -> f64) -> f64 {
    rows.windows(2)
        .map(|pair| {
            let [a, b] = pair else { unreachable!() };
            (b[0] - a[0]) * (ordinate(a) + ordinate(b)) * 0.5
        })
        .sum()
}

#[test]
fn independent_charge_flux_circuit_matches_prepared_polynomial() -> Result<()> {
    // Explicit mathematical primitives, independent of model normalization
    // and Rust's polynomial kernel. The flux witness uses the dual
    // charge circuit: its voltage represents signed inductor current.
    let deck = r"Independent polynomial storage witness
Vdrive v 0 DC -2 AC 1 PWL(0 -2 1m 2 2m 2)
Cq v 0 Q='2n*(v(v)+0.1*v(v)*v(v)+0.01*v(v)*v(v)*v(v))'
Iin 0 i DC -0.5 AC 1 PWL(0 -0.5 1m 1.5 2m 1.5)
Vsense i n 0
Bphi aux 0 V='i(vsense)'
Cphi aux 0 Q='3m*(v(aux)-0.05*v(aux)*v(aux)+0.02*v(aux)*v(aux)*v(aux)/3)'
Bvoltage n 0 V='-i(Bphi)'
.control
set wr_singlescale
set wr_vecnames
op
wrdata op.dat v(v) i(vdrive) i(vsense) v(n)
ac lin 3 1k 3k
wrdata ac.dat i(vdrive) v(n)
tran 1u 1m 0 1u
wrdata tran.dat v(v) i(vdrive) i(vsense) v(n)
quit
.endc
.end
";
    let dir = tempfile::tempdir()?;
    fs::write(dir.path().join("deck.cir"), deck)?;
    let output = match Command::new("ngspice")
        .args(["-n", "-b", "deck.cir"])
        .current_dir(dir.path())
        .output()
    {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    ensure!(
        output.status.success(),
        "ngspice: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let capacitor = PolynomialStorage::new(2e-9, 0.2, 0.03)?;
    let inductor = PolynomialStorage::new(3e-3, -0.1, 0.02)?;
    let op = table::<5>(&dir.path().join("op.dat"))?;
    assert_eq!(op.len(), 1);
    let [_, v, c_current, i, l_voltage] = op[0];
    close(v, -2., 0., 1e-12);
    close(i, -0.5, 0., 1e-12);
    close(c_current, 0., 0., 1e-12);
    close(l_voltage, 0., 0., 1e-12);
    let ac = table::<5>(&dir.path().join("ac.dat"))?;
    assert_eq!(ac.len(), 3);
    for [f, re_c, im_c, re_l, im_l] in ac {
        let omega = std::f64::consts::TAU * f;
        close(re_c, 0., 0., 1e-12);
        close(re_l, 0., 0., 1e-12);
        close(
            im_c,
            -omega * capacitor.evaluate(v)?.differential,
            1e-7,
            1e-12,
        );
        close(
            im_l,
            omega * inductor.evaluate(i)?.differential,
            1e-7,
            1e-10,
        );
    }
    let tr = table::<5>(&dir.path().join("tran.dat"))?;
    assert!(tr.len() >= 1000);
    let first = tr.first().context("missing initial state")?;
    let last = tr.last().context("missing final state")?;
    close(first[1], -2., 0., 1e-12);
    close(first[3], -0.5, 0., 1e-12);
    close(last[1], 2., 0., 1e-9);
    close(last[3], 1.5, 0., 1e-9);
    // The initial OP row precedes the ramp, and the external simulator
    // reports finite-step samples. Integrate those samples independently.
    close(
        integral(&tr, |r| -r[2]),
        capacitor.evaluate(last[1])?.stored - capacitor.evaluate(first[1])?.stored,
        1e-5,
        1e-15,
    );
    close(
        integral(&tr, |r| r[4]),
        inductor.evaluate(last[3])?.stored - inductor.evaluate(first[3])?.stored,
        1e-5,
        1e-12,
    );
    close(
        integral(&tr, |r| -r[1] * r[2]),
        capacitor.energy(last[1])? - capacitor.energy(first[1])?,
        5e-5,
        1e-15,
    );
    close(
        integral(&tr, |r| r[3] * r[4]),
        inductor.energy(last[3])? - inductor.energy(first[3])?,
        1e-5,
        1e-12,
    );
    Ok(())
}

#[test]
fn device_ic_bias_policy_is_phase_specific_and_preserves_signed_occurrences() -> Result<()> {
    use ngspice_rs_preprocess::{
        Cancellation, InputFormat, Options, Request, SourceInput,
        passive_ic::{Analysis, BiasConstraint, MissingStorage, evaluate_expression},
        prepare,
    };

    let dir = tempfile::tempdir()?;
    let path = dir.path().join("device.cir");
    for skip_translation in [false, true] {
        let flag = if skip_translation {
            " NOICTRANSLATE"
        } else {
            ""
        };
        let source = format!(
            "Device IC policy\n.model CM CAP(VC1=0.1)\n.model LM IND(IL1=0.2)\n\
             C1 p n 1n IC=-2\nL1 a b 1m IC=0\nCabsent a b 2n\nLabsent p n 2m\n\
             .subckt cell p n params: x=3\nC2 p n CM 1n\n+ IC={{-x}}\n\
             L2 p n LM 1m IC={{x/10}}\n.ends cell\nX1 p n cell x=5\n\
             .options reltol=0.001{flag}\n.tran 1u 1m SKIPBP\n.end\n"
        );
        fs::write(&path, &source)?;
        let bundle = prepare(
            &Request {
                format: InputFormat::PspiceDeck,
                input: SourceInput::File(path.clone()),
                libraries: Vec::new(),
                search_paths: Vec::new(),
            },
            &Options::default(),
            &Cancellation::default(),
        )?;
        let native = std::str::from_utf8(&bundle.primary.bytes)?;
        assert!(!native.to_ascii_lowercase().contains("ic="), "{native}");
        assert!(
            !native.to_ascii_lowercase().contains("noictranslate"),
            "{native}"
        );
        assert!(native.contains(".tran 1u 1m UIC"), "{native}");
        assert!(native.contains("reltol=0.001"), "{native}");
        assert_eq!(native.lines().count(), source.lines().count());
        let policy = bundle
            .initial_conditions
            .context("missing IC declarations")?;
        assert_eq!(policy.devices.len(), 6);
        for (name, value, capacitor) in [
            ("C1", Some(-2.), true),
            ("L1", Some(0.), false),
            ("C2", Some(-5.), true),
            ("L2", Some(0.5), false),
            ("Cabsent", None, true),
            ("Labsent", None, false),
        ] {
            let device = policy
                .devices
                .iter()
                .find(|d| d.device.eq_ignore_ascii_case(name))
                .context("missing device")?;
            assert_eq!(device.missing_storage, MissingStorage::Zero);
            assert_eq!(device.target.is_some(), value.is_some());
            for analysis in [
                Analysis::OperatingPoint,
                Analysis::Ac,
                Analysis::Noise,
                Analysis::DcSweep,
                Analysis::Transient { uic: false },
                Analysis::Transient { uic: true },
            ] {
                let evaluate = |text: &str| {
                    evaluate_expression(text, analysis, |symbol| {
                        ensure!(symbol.eq_ignore_ascii_case("x"), "unknown symbol {symbol}");
                        Ok(5.)
                    })
                };
                let defaults = policy.resolve_defaults(evaluate)?;
                let resolved = device.resolve(analysis, defaults, evaluate)?;
                assert_eq!(
                    resolved.storage_ic,
                    if matches!(analysis, Analysis::Transient { uic: true }) {
                        Some(value.unwrap_or_default())
                    } else {
                        value
                    }
                );
                let expected = if skip_translation
                    || value.is_none()
                    || matches!(
                        analysis,
                        Analysis::DcSweep | Analysis::Transient { uic: true }
                    ) {
                    BiasConstraint::None
                } else if capacitor {
                    BiasConstraint::CapacitorSoft {
                        voltage: value.context("missing bias target")?,
                        resistance: 0.002,
                    }
                } else {
                    BiasConstraint::InductorSoft {
                        current: value.context("missing bias target")?,
                        conductance: 1e-9,
                    }
                };
                assert_eq!(resolved.bias, expected);
            }
        }
    }
    Ok(())
}

#[test]
fn mixed_source_initial_conditions_follow_their_own_occurrence_dialect() -> Result<()> {
    use ngspice_rs_preprocess::{
        Cancellation, InputFormat, Options, Provenance, Request, SourceInput, prepare,
    };

    let dir = tempfile::tempdir()?;
    let root = dir.path().join("root.cir");
    let vendor = dir.path().join("vendor.inc");
    let native = dir.path().join("native.inc");
    fs::write(&vendor, b"*#PSPICE\n.subckt cell p n\n.model CM CAP(VC1=0.1)\nCvendor p n CM 1n\n+ IC={-2}\n.include native.inc\nLvendor p n 1m IC=0\n.ends\n")?;
    fs::write(
        &native,
        b"*#NATIVE\nCnative p n 1n IC=3\nLnative p n 1m IC=-4\n",
    )?;
    for (format, source, expected) in [
        (
            InputFormat::NativeDeck,
            "Mixed native root\nCroot p 0 1n IC=6\n.include vendor.inc\nX1 p 0 cell\n.tran 1u 1m UIC\n.end\n",
            2,
        ),
        (
            InputFormat::PspiceDeck,
            "Mixed PSpice root\nCroot p 0 1n IC=6\n.include vendor.inc\nX1 p 0 cell\n.tran 1u 1m SKIPBP\n.options NOICTRANSLATE\n.end\n",
            3,
        ),
    ] {
        fs::write(&root, source)?;
        let bundle = prepare(
            &Request {
                format,
                input: SourceInput::File(root.clone()),
                libraries: Vec::new(),
                search_paths: Vec::new(),
            },
            &Options::default(),
            &Cancellation::default(),
        )?;
        let text = std::str::from_utf8(&bundle.primary.bytes)?;
        let policy = bundle
            .initial_conditions
            .context("missing PSpice sidecar")?;
        assert_eq!(policy.devices.len(), expected, "{text}");
        assert!(text.contains("Cnative p n 1n IC=3"), "{text}");
        assert!(text.contains("Lnative p n 1m IC=-4"), "{text}");
        assert!(text.contains(".tran 1u 1m UIC"), "{text}");
        assert!(!text.contains("NOICTRANSLATE"), "{text}");
        assert_eq!(text.contains("Croot p 0 1n IC=6"), expected == 2, "{text}");
        for (name, value) in [("Cvendor", "{-2}"), ("Lvendor", "0")] {
            let device = policy
                .devices
                .iter()
                .find(|d| d.device.eq_ignore_ascii_case(name))
                .context("missing vendor device IC")?;
            assert_eq!(device.scope, ["cell"]);
            assert_eq!(device.target.as_deref(), Some(value));
            assert_eq!(
                device.mode.as_deref(),
                Some(if expected == 2 { "2" } else { "0" })
            );
        }
        let Provenance::Native { lines, .. } = &bundle.primary.provenance else {
            anyhow::bail!("missing native source provenance");
        };
        assert_eq!(lines.len(), text.lines().count());
        for (name, file, line) in [
            ("Cvendor", &vendor, 4),
            ("Cnative", &native, 2),
            ("Lvendor", &vendor, 7),
        ] {
            let location = text
                .lines()
                .zip(lines)
                .find_map(|(text, origin)| text.starts_with(name).then_some(origin))
                .context("missing mapped device")?;
            assert_eq!(location.source, file.canonicalize()?);
            assert_eq!(location.line, line);
        }
    }
    Ok(())
}
