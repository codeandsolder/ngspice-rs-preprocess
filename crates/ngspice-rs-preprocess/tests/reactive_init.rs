//! Independent storage-conservation witnesses for prepared SIMPLIS snapshots.
use anyhow::{Context, Result, bail, ensure};
use ngspice_rs_preprocess::{
    Cancellation, InputFormat, Options, PreparedBundle, Request, SourceInput,
    passive_ic::{Analysis, BiasConstraint, Mode, evaluate_expression},
    prepare,
};
use std::{fmt::Write as _, fs, path::Path, process::Command};

fn prepare_source(dir: &Path, source: &str) -> Result<PreparedBundle> {
    let path = dir.join("input.net");
    fs::write(&path, source)?;
    prepare(
        &Request {
            format: InputFormat::SimplisDeck,
            input: SourceInput::File(path),
            libraries: Vec::new(),
            search_paths: Vec::new(),
        },
        &Options::default(),
        &Cancellation::default(),
    )
}

/// Independent native reference adapter: translate the typed storage target
/// into the ordinary ngspice UIC slot for the exact emitted occurrence.
/// It does not recompute voltage-to-charge or current-to-flux conversion.
fn materialize_uic(dir: &Path, bundle: &PreparedBundle, control: &str) -> Result<()> {
    let policy = bundle
        .initial_conditions
        .as_ref()
        .context("missing reactive startup policy")?;
    let analysis = Analysis::Transient { uic: true };
    let resolver = |expression: &str| {
        evaluate_expression(expression, analysis, |symbol| {
            bail!("unexpected symbol {symbol}")
        })
    };
    let defaults = policy.resolve_defaults(resolver)?;
    assert_eq!(defaults.mode, Mode::Hard);
    for artifact in bundle.artifacts() {
        let text = std::str::from_utf8(&artifact.bytes)?;
        let mut output = String::new();
        let mut scope = Vec::new();
        let mut count = 0;
        for line in text.lines() {
            let mut fields = line.split_ascii_whitespace();
            let first = fields.next().unwrap_or_default();
            if first.eq_ignore_ascii_case(".subckt") {
                scope.push(fields.next().context("missing scope name")?.to_owned());
            }
            if let Some(device) = policy.devices.iter().find(|d| {
                d.artifact == artifact.filename
                    && d.scope == scope
                    && d.device.eq_ignore_ascii_case(first)
            }) {
                let startup = device.resolve(analysis, defaults, resolver)?;
                assert_eq!(startup.bias, BiasConstraint::None);
                let initial = startup.storage_ic.context("missing storage IC")?;
                writeln!(output, "{line} IC={initial:.17e}")?;
                count += 1;
            } else if first.eq_ignore_ascii_case(".end")
                && artifact.filename == bundle.primary.filename
            {
                writeln!(output, "{control}\n.end")?;
            } else {
                writeln!(output, "{line}")?;
            }
            if first.eq_ignore_ascii_case(".ends") {
                scope.pop();
            }
        }
        assert_eq!(
            count,
            policy
                .devices
                .iter()
                .filter(|d| d.artifact == artifact.filename)
                .count()
        );
        fs::write(dir.join(&artifact.filename), output)?;
    }
    Ok(())
}

fn run_native(dir: &Path, bundle: &PreparedBundle, control: &str) -> Result<Vec<Vec<f64>>> {
    materialize_uic(dir, bundle, control)?;
    let execution = Command::new("ngspice")
        .current_dir(dir)
        .args(["-b", "-n", &bundle.primary.filename])
        .output()?;
    let log = format!(
        "{}\n{}",
        String::from_utf8_lossy(&execution.stdout),
        String::from_utf8_lossy(&execution.stderr)
    );
    ensure!(
        execution.status.success()
            && !log.to_ascii_lowercase().contains("error:")
            && !log.to_ascii_lowercase().contains("aborted"),
        "{log}"
    );
    fs::read_to_string(dir.join("wave.dat"))?
        .lines()
        .map(|line| {
            line.split_ascii_whitespace()
                .map(str::parse::<f64>)
                .collect::<std::result::Result<Vec<_>, _>>()
                .context("invalid native trace")
        })
        .collect()
}

// Independent source law: q(v) in nC (or pC). The arbitrary charge origin
// is 4 at zero voltage. The two capacitances are 2 and 4 respectively.
fn physical_charge(voltage: f64) -> f64 {
    if voltage < 0. {
        2.0_f64.mul_add(voltage, 4.)
    } else {
        4.0_f64.mul_add(voltage, 4.)
    }
}

#[test]
fn floating_nested_pwlc_repeated_snapshots_conserve_charge() -> Result<()> {
    for (unit, current) in [("n", "1m"), ("p", "1u")] {
        let dir = tempfile::tempdir()?;
        let source = format!(
            r"* storage witness
Vbase n 0 0.75
I1 n a {current}
I2 n b {current}
I3 n c {current}
I4 n d {current}
X1 a n PARENT
X2 b n PARENT
X3 c n PARENT
X4 d n PARENT
.INIT V(X1.XC.!C1)=-2 V(X2.XC.!C1)=1
+ V(X3.XC.!C1)=3
.subckt PARENT p n
XC p n CELL
.INIT V(XC.!C1)=0
.subckt CELL p n
!C1 p n MC IC=1.5
.MODEL MC PWLC NSEG=2 X0=-1 Y0=2{unit} X1=0 Y1=4{unit} X2=2 Y2=12{unit}
.INIT V(!C1)=-0.5
.ends CELL
.ends PARENT
.end
"
        );
        let bundle = prepare_source(dir.path(), &source)?;
        let policy = bundle
            .initial_conditions
            .as_ref()
            .context("missing initialization")?;
        assert_eq!(policy.devices.len(), 4);
        let analysis = Analysis::Transient { uic: false };
        let defaults = policy
            .resolve_defaults(|expr| evaluate_expression(expr, analysis, |s| bail!("{s}")))?;
        for device in &policy.devices {
            let startup = device.resolve(analysis, defaults, |expr| {
                evaluate_expression(expr, analysis, |s| bail!("{s}"))
            })?;
            assert_eq!(
                startup.bias,
                BiasConstraint::CapacitorHard {
                    voltage: startup.storage_ic.context("missing coordinate")?
                }
            );
        }
        let trace = run_native(
            dir.path(),
            &bundle,
            r".control
set numdgt=17
set wr_singlescale
tran 10n 10u 0 5n uic
let va=v(a)-v(n)
let vb=v(b)-v(n)
let vc=v(c)-v(n)
let vd=v(d)-v(n)
linearize va vb vc vd
wrdata wave.dat va vb vc vd
quit
.endc",
        )?;
        assert_eq!(trace.len(), 1001);
        for row in trace {
            let [time, a, b, c, d] = row.as_slice() else {
                bail!("unexpected trace width")
            };
            for (voltage, start) in [(*a, -2.), (*b, 1.), (*c, 3.), (*d, 0.)] {
                let expected = physical_charge(start) + time * 1e6;
                ensure!(
                    (physical_charge(voltage) - expected).abs() < 1e-7,
                    "{unit} t={time} initial={start} actual={voltage} charge delta={}",
                    physical_charge(voltage) - expected
                );
                if time.abs() < 1e-15 {
                    assert!((voltage - start).abs() < 1e-10);
                }
            }
        }
    }
    Ok(())
}

#[test]
fn nonlinear_flux_and_linear_reactive_snapshots_conserve_state() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let source = r"* signed flux witness
V1 a 0 1
V2 b 0 -1
!L1 a 0 ML IC=0
!L2 b 0 ML IC=0
.MODEL ML PWLL NSEG=2 X0=-2 Y0=-1m X1=0 Y1=3m X2=2 Y2=11m
.INIT I(!L1)=-1 I(!L2)=3
Icap 0 c 1m
C1 c 0 1u IC=-5
.INIT V(C1)=2
Vlin e 0 2
L1 e 0 2m IC=9
.INIT I(L1)=-0.5
.end
";
    let bundle = prepare_source(dir.path(), source)?;
    let policy = bundle
        .initial_conditions
        .as_ref()
        .context("missing initialization")?;
    assert_eq!(policy.devices.len(), 4);
    let trace = run_native(
        dir.path(),
        &bundle,
        r".control
set numdgt=17
set wr_singlescale
tran 1u 4m 0 0.5u uic
let ia=-i(v1)
let ib=-i(v2)
let ilin=-i(vlin)
wrdata wave.dat ia ib v(c) ilin
quit
.endc",
    )?;
    assert!(trace.len() > 8000);
    let flux = |current: f64| {
        if current < 0. {
            2e-3_f64.mul_add(current, 3e-3)
        } else {
            4e-3_f64.mul_add(current, 3e-3)
        }
    };
    for row in trace {
        let [time, a, b, c, i] = row.as_slice() else {
            bail!("unexpected flux trace width")
        };
        ensure!(
            (flux(*a) - flux(-1.) - time).abs() < 1e-10,
            "t={time} i1={a}"
        );
        ensure!(
            (flux(*b) - flux(3.) + time).abs() < 1e-10,
            "t={time} i2={b}"
        );
        assert!((c - (2. + time * 1000.)).abs() < 1e-7);
        assert!((i - (-0.5 + time * 1000.)).abs() < 1e-7);
    }
    Ok(())
}

#[test]
fn malformed_reactive_targets_and_nonpositive_storage_are_rejected() -> Result<()> {
    let dir = tempfile::tempdir()?;
    for source in [
        "C1 a 0 1u IC=0\n.INIT I(C1)=1\n.end\n",
        "!C1 a 0 MC IC=1\n.MODEL MC PWLC NSEG=2 X0=-1 Y0=0 X1=0 Y1=2n X2=1 Y2=1n\n.end\n",
        "!L1 a 0 ML IC=1\n.MODEL ML PWLL NSEG=2 X0=-1 Y0=0 X1=0 Y1=2m X2=1 Y2=2m\n.end\n",
        "!C1 a 0 ML IC=0\n.MODEL ML PWLL NSEG=2 X0=-1 Y0=0 X1=0 Y1=2m X2=1 Y2=3m\n.end\n",
    ] {
        assert!(prepare_source(dir.path(), source).is_err(), "{source}");
    }
    Ok(())
}

#[test]
fn mixed_source_snapshot_is_captured_before_native_ic_removal() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("mixed.net");
    fs::write(&path,b"* mixed reactive snapshot\nV1 a 0 10\nR1 a p 1k\nC1 p 0 1u IC=2 BRANCH=1\n.INIT V(C1)=3\nX1 a 0 CELL\n.INIT I(X1.L1)=-2m\n.subckt CELL p n\nR1 p q 1k\nL1 q n 1m IC=1m BRANCH=0\n.INIT I(L1)=0\n.ends CELL\n.end\n")?;
    let bundle = prepare(
        &Request {
            format: InputFormat::SimetrixDeck,
            input: SourceInput::File(path),
            libraries: Vec::new(),
            search_paths: Vec::new(),
        },
        &Options::default(),
        &Cancellation::default(),
    )?;
    let policy = bundle.initial_conditions.context("missing policy")?;
    assert_eq!(policy.devices.len(), 2);
    let c = policy
        .devices
        .iter()
        .find(|device| device.device.eq_ignore_ascii_case("C1"))
        .context("missing capacitor")?;
    let l = policy
        .devices
        .iter()
        .find(|device| device.device.eq_ignore_ascii_case("L1"))
        .context("missing inductor")?;
    let analysis = Analysis::Transient { uic: false };
    let resolver = |expression: &str| {
        evaluate_expression(expression, analysis, |symbol| {
            bail!("unexpected symbol {symbol}")
        })
    };
    let defaults = policy.resolve_defaults(resolver)?;
    assert_eq!(
        c.resolve(analysis, defaults, resolver)?.bias,
        BiasConstraint::CapacitorHard { voltage: 3. }
    );
    assert_eq!(
        l.resolve(analysis, defaults, resolver)?.bias,
        BiasConstraint::InductorHard { current: -0.002 }
    );
    Ok(())
}
