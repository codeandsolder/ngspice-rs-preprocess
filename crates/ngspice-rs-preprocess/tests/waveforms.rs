//! Independent source-domain laws, scope binding, and reference execution.

use anyhow::{Context, Result, bail, ensure};
use model_normalize_ngspice::param_expression::SpiceExpression;
use ngspice_rs_preprocess::{
    Cancellation, InputFormat, Options, PreparedBundle, Request, SourceInput, prepare,
    waveform::{Forcing, Knot, PreparedWaveform, Repeat, ResolvedWaveform},
};
use std::{fs, path::Path, process::Command};

fn prepared(dir: &Path, source: &str, format: InputFormat) -> Result<PreparedBundle> {
    let input = dir.join("source.net");
    fs::write(&input, source)?;
    prepare(
        &Request {
            format,
            input: SourceInput::File(input),
            libraries: Vec::new(),
            search_paths: Vec::new(),
        },
        &Options::default(),
        &Cancellation::default(),
    )
}

fn resolve(declaration: &PreparedWaveform) -> Result<ResolvedWaveform> {
    declaration
        .resolve(|text| SpiceExpression::parse(text)?.evaluate(|name| bail!("unbound {name}")))
}

fn close(actual: f64, want: f64, absolute: f64) -> Result<()> {
    ensure!(
        (actual - want).abs() <= absolute,
        "{actual:.17e} != {want:.17e}, tolerance {absolute}"
    );
    Ok(())
}

fn cycle(waveform: &ResolvedWaveform) -> Result<(&[Knot], Repeat)> {
    let Forcing::PiecewiseLinear {
        knots,
        repeat: Some(cycle),
    } = &waveform.forcing
    else {
        bail!("expected periodic piecewise waveform")
    };
    ensure!(
        knots.windows(2).all(|pair| pair[0].time < pair[1].time),
        "coincident events were not collapsed"
    );
    assert_eq!(knots[0].time.to_bits(), 0f64.to_bits());
    ensure!(
        knots.last().context("missing cycle")?.time < cycle.period,
        "period end must not be an extra event"
    );
    Ok((knots, *cycle))
}

fn period_area(knots: &[Knot], repeat: Repeat) -> f64 {
    let first = knots[0];
    let end = Knot {
        time: repeat.period,
        left: first.left,
        right: first.right,
    };
    knots
        .iter()
        .zip(knots.iter().skip(1).chain(std::iter::once(&end)))
        .map(|(left, right)| 0.5 * (left.right + right.left) * (right.time - left.time))
        .sum()
}

#[test]
fn exact_pulse_limits_and_charge_for_all_edge_degeneracies() -> Result<()> {
    let dir = tempfile::tempdir()?;
    for (rise, width, fall) in [
        (0., 0.4, 0.),
        (0., 0.4, 0.1),
        (0.1, 0.4, 0.),
        (0.2, 0., 0.3),
        (0., 1., 0.),
        (0.5, 0., 0.5),
        (1., 0., 0.),
        (0., 0., 1.),
        (0., 0., 0.),
        (0.1, 0.2, 0.7),
    ] {
        let source = format!(
            "* pulse witness\nI1 0 out PUL V1=-2 V2=6 PERIOD=1 T_RISE={rise} PWIDTH={width} T_FALL={fall} DELAY=2.75 OFF_UNTIL_DELAY=NO\n.end\n"
        );
        let bundle = prepared(dir.path(), &source, InputFormat::ExpandedDeck)?;
        let [declaration] = bundle.waveforms.as_slice() else {
            bail!("missing waveform")
        };
        assert_eq!(declaration.source, "I1");
        let resolved = resolve(declaration)?;
        let (knots, repeat) = cycle(&resolved)?;
        close(repeat.origin, -0.25, 1e-15)?;
        close(
            period_area(knots, repeat),
            -2. + 8. * (width + 0.5 * (rise + fall)),
            1e-14,
        )?;
        let first = knots[0];
        let end = rise + width + fall;
        let right = if rise > 0. || end <= 0. { -2. } else { 6. };
        let left = if fall == 0. && end >= 1. { 6. } else { -2. };
        close(first.right, right, 0.)?;
        close(first.left, left, 0.)?;
        // Explicit algebra for the right-sided value at phase 0.25.
        let t = 0.25;
        let value = if t < rise {
            -2. + 8. * t / rise
        } else if t < rise + width {
            6.
        } else if t < end {
            6. - 8. * (t - rise - width) / fall
        } else {
            -2.
        };
        close(resolved.dc, value, 1e-14)?;
        let roundtrip: PreparedWaveform =
            serde_json::from_slice(&serde_json::to_vec(declaration)?)?;
        assert_eq!(&roundtrip, declaration);
    }
    Ok(())
}

#[test]
fn rectangular_and_saw_signed_phase_and_hold_are_exact() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let bundle = prepared(
        dir.path(),
        r"* signed phase witness
Vgate a 0 PUL V1=2 V2=-6 FREQ=1k DRATIO=0.4 DELAY=750u OFF_UNTIL_DELAY=YES
Vphase b 0 PUL V1=2 V2=-6 FREQ=1k DRATIO=0.4 DELAY=750u OFF_UNTIL_DELAY=NO
Vneg c 0 PUL V1=2 V2=-6 FREQ=1k DRATIO=0.4 DELAY=-250u OFF_UNTIL_DELAY=YES
Vedge d 0 PUL V1=2 V2=-6 FREQ=1k DRATIO=0.4 DELAY=0 OFF_UNTIL_DELAY=NO
Isaw 0 e SAW V1=-1 V2=3 FREQ=1k DELAY=750u OFF_UNTIL_DELAY=NO IDLE_IN_POP=YES
Vhold f 0 SAW V1=-1 V2=3 FREQ=1k DELAY=750u OFF_UNTIL_DELAY=YES
.end
",
        InputFormat::ExpandedDeck,
    )?;
    assert_eq!(bundle.waveforms.len(), 6);
    for (declaration, (dc, origin, hold)) in bundle.waveforms.iter().zip([
        (2., 750e-6, Some(2.)),
        (-6., -250e-6, None),
        (-6., -250e-6, None),
        (-6., 0., None),
        (0., -250e-6, None),
        (-1., 750e-6, Some(-1.)),
    ]) {
        let waveform = resolve(declaration)?;
        let (knots, repeat) = cycle(&waveform)?;
        close(waveform.dc, dc, 1e-14)?;
        close(repeat.origin, origin, 1e-15)?;
        assert_eq!(repeat.hold, hold);
        if declaration.source.to_ascii_lowercase().contains("saw") || declaration.source == "Vhold"
        {
            assert_eq!(knots.len(), 1);
            close(knots[0].left, 3., 0.)?;
            close(knots[0].right, -1., 0.)?;
        } else {
            assert_eq!(knots.len(), 2);
            close(knots[0].left, 2., 0.)?;
            close(knots[0].right, -6., 0.)?;
            close(knots[1].time, 400e-6, 1e-15)?;
            close(knots[1].left, -6., 0.)?;
            close(knots[1].right, 2., 0.)?;
            close(period_area(knots, repeat), -0.0012, 1e-15)?;
        }
        assert_eq!(waveform.idle_in_pop, declaration.source == "Isaw");
    }
    Ok(())
}

#[test]
fn aperiodic_duplicate_knots_and_signed_sine_startup() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let bundle = prepared(
        dir.path(),
        r"* PWL and damped sine
Vstep a 0 PWL NSEG=5 X0=-1 Y0=-2 X1=0 Y1=1 X2=0 Y2=99 X3=0 Y3=3 X4=2 Y4=7 X5=2 Y5=-4
Vsin b 0 SIN VOFFSET=1 APEAK=2 FREQ=1k PDELAY=90 OFF_UNTIL_DELAY=NO DAMP_COEF=200
Ihold 0 c SIN VOFFSET=1 APEAK=2 FREQ=1k PDELAY=90 OFF_UNTIL_DELAY=YES DAMP_COEF=200
Vadvance d 0 SIN VOFFSET=1 APEAK=2 FREQ=1k TDELAY=-250u OFF_UNTIL_DELAY=YES DAMP_COEF=200
.end
",
        InputFormat::ExpandedDeck,
    )?;
    assert_eq!(bundle.waveforms.len(), 4);
    let waveform = resolve(&bundle.waveforms[0])?;
    assert_eq!(
        waveform.forcing,
        Forcing::PiecewiseLinear {
            knots: vec![
                Knot {
                    time: -1.,
                    left: -2.,
                    right: -2.
                },
                Knot {
                    time: 0.,
                    left: 1.,
                    right: 3.
                },
                Knot {
                    time: 2.,
                    left: 7.,
                    right: -4.
                }
            ],
            repeat: None,
        }
    );
    close(waveform.dc, 3., 0.)?;
    for (declaration, expected) in bundle.waveforms.iter().skip(1).zip([
        1. - 2. * 0.05f64.exp(),
        1.,
        1. + 2. * (-0.05f64).exp(),
    ]) {
        close(resolve(declaration)?.dc, expected, 1e-14)?;
    }
    Ok(())
}

#[test]
fn symbolic_waveform_fields_bind_to_actual_linked_occurrences() -> Result<()> {
    use model_normalize_ngspice::{
        linked_source::SourceLinker,
        parameter_scope::{ModelLibrary, topology::Limits},
    };
    let dir = tempfile::tempdir()?;
    let bundle = prepared(
        dir.path(),
        r"* per-occurrence phase and shape
.param lo=2
X1 a 0 cell params: f=1k duty=0.5 edge=0 gate=0
X2 b 0 cell params: f=2k duty=0.25 edge=10u gate=1
.subckt cell p n params: f=1k duty=0.5 edge=0 gate=0
Vraw p n PUL V1={lo} V2=6 FREQ={f} T_RISE={edge} T_FALL=0 PWIDTH={duty/f} DELAY={0.75/f} OFF_UNTIL_DELAY={gate}
.ends
.end
",
        InputFormat::SimetrixDeck,
    )?;
    for artifact in bundle.artifacts() {
        fs::write(dir.path().join(&artifact.filename), &artifact.bytes)?;
    }
    let linked = SourceLinker::default().link(&dir.path().join(&bundle.primary.filename))?;
    let library = ModelLibrary::parse(&linked.bytes)?;
    let topology = library.instantiate(Limits::default())?;
    let mut observed = Vec::new();
    for declaration in &bundle.waveforms {
        for frame in &topology.frames {
            let scope = &library.scopes[frame.declaration];
            if !scope.name.as_deref().is_some_and(|name| {
                declaration
                    .scope
                    .last()
                    .is_some_and(|emitted| name.eq_ignore_ascii_case(emitted))
            }) {
                continue;
            }
            let overrides = frame
                .overrides
                .iter()
                .map(|(name, &value)| (frame.declaration, name.as_str(), value))
                .collect::<Vec<_>>();
            let waveform = declaration
                .resolve(|expr| library.evaluate_scalar(frame.declaration, expr, &overrides))?;
            let (knots, repeat) = cycle(&waveform)?;
            observed.push((
                repeat.period,
                waveform.dc,
                repeat.hold,
                period_area(knots, repeat),
            ));
        }
    }
    observed.sort_by(|a, b| a.0.total_cmp(&b.0));
    assert_eq!(observed.len(), 2);
    for (actual, expected) in observed
        .into_iter()
        .zip([(0.0005, 2., Some(2.), 0.00152), (0.001, 6., None, 0.004)])
    {
        close(actual.0, expected.0, 1e-15)?;
        close(actual.1, expected.1, 1e-14)?;
        assert_eq!(actual.2, expected.2);
        close(actual.3, expected.3, 1e-14)?;
    }
    Ok(())
}

#[test]
fn invalid_static_fields_and_deferred_timing_are_rejected() -> Result<()> {
    let dir = tempfile::tempdir()?;
    for fields in [
        "PUL V1=0 V2=1 FREQ=0 DRATIO=0.5",
        "PUL V1=0 V2=1 FREQ=1k DRATIO=0",
        "PUL V1=0 V2=1 FREQ=1k DRATIO=1",
        "PUL V1=0 V2=1 FREQ=1k DRATIO=0.5 PWIDTH=1u",
        "PUL V1=0 V2=1 FREQ=1k T_RISE=-1u T_FALL=1u PWIDTH=1u",
        "PUL V1=0 V2=1 FREQ=1k T_RISE=1m T_FALL=1u PWIDTH=1u",
        "SAW V1=0 V2=1 FREQ=1k PERIOD=1m",
        "SAW V1=0 V2=1 FREQ=1k V1=2",
        "SIN VOFFSET=0 APEAK=-1 FREQ=1k TDELAY=0",
        "SIN VOFFSET=0 APEAK=1 FREQ=1k TDELAY=0 PDELAY=90",
        "PWL NSEG=2 X0=0 Y0=1 X1=1 Y1=3",
        "PWL NSEG=1 X0=2 Y0=1 X1=1 Y1=3",
        "PWL NSEG=1 X0=0 Y0=1 X1=1 Y1=3 WRONG=9",
    ] {
        let source = format!("* invalid source\nV1 a 0 {fields}\n.end\n");
        assert!(
            prepared(dir.path(), &source, InputFormat::ExpandedDeck).is_err(),
            "{fields}"
        );
    }
    let bundle = prepared(
        dir.path(),
        "* symbolic timing\n.param freq=0\nV1 a 0 PUL V1=0 V2=1 FREQ={freq} DRATIO=0.5\n.end\n",
        InputFormat::ExpandedDeck,
    )?;
    let [declaration] = bundle.waveforms.as_slice() else {
        bail!("missing symbolic declaration")
    };
    assert!(
        declaration
            .resolve(|expr| SpiceExpression::parse(expr)?.evaluate(|_| Ok(0.)))
            .is_err()
    );
    Ok(())
}

#[test]
fn raw_native_reference_preserves_identity_timing_and_integrated_charge() -> Result<()> {
    // The static reference exporter targets ngspice's duplicate-knot PWL
    // behavior; its external sided limits are tested away from exact jumps.
    let dir = tempfile::tempdir()?;
    let deck = model_translate_simplis::translate_simplis_deck(
        br"* independent reference source
Irect 0 a PUL V1=0 V2=1m FREQ=1MEG DRATIO=0.2 DELAY=750n OFF_UNTIL_DELAY=NO
C1 a 0 1n IC=0
Vsaw b 0 SAW V1=-2 V2=2 FREQ=1MEG DELAY=-250n OFF_UNTIL_DELAY=YES
R1 b 0 1k
Vsin c 0 SIN VOFFSET=1 APEAK=2 FREQ=1MEG PDELAY=90 OFF_UNTIL_DELAY=NO DAMP_COEF=1k
R2 c 0 1k
.end
",
    )?;
    let text = std::str::from_utf8(&deck.bytes)?;
    assert!(text.contains("Irect 0 a DC "));
    assert!(text.contains("Vsaw b 0 DC "));
    for step in ["10n", "100n"] {
        let mut input = text
            .strip_suffix(".end\n")
            .context("missing end")?
            .to_owned();
        input.push_str(&format!(".control\nset numdgt=17\nset wr_singlescale\ntran {step} 3u 0 0.25n uic\nwrdata wave.dat v(a) v(b) v(c) i(vsaw)\nquit\n.endc\n.end\n"));
        fs::write(dir.path().join("reference.cir"), input)?;
        let run = Command::new("ngspice")
            .current_dir(dir.path())
            .args(["-b", "-n", "reference.cir"])
            .output()?;
        let log = format!(
            "{}\n{}",
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr)
        );
        ensure!(
            run.status.success() && !log.to_ascii_lowercase().contains("error:"),
            "{log}"
        );
        let data = fs::read_to_string(dir.path().join("wave.dat"))?;
        let mut last = 0.;
        let mut rows = 0;
        for line in data.lines() {
            let row = line
                .split_ascii_whitespace()
                .map(str::parse::<f64>)
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let [time, cap, saw, sine, branch] = row.as_slice() else {
                bail!("invalid trace width")
            };
            let phase = (*time + 250e-9).rem_euclid(1e-6);
            if phase > 1e-12 && phase < 1e-6 - 1e-12 {
                close(*saw, -2. + 4. * phase / 1e-6, 1e-8)?;
                close(*branch, -saw / 1000., 1e-10)?;
            }
            let elapsed = *time - 250e-9;
            let want =
                1. + 2. * (-1000. * elapsed).exp() * (std::f64::consts::TAU * 1e6 * elapsed).sin();
            close(*sine, want, 1e-7)?;
            last = *cap;
            rows += 1;
        }
        ensure!(rows > 10_000, "missing transient points");
        // 3 periods, each 200ns at 1mA. C=1nF -> 0.6V.
        // C ngspice's accepted-step handling at ideal jumps is not exact;
        // the bound is relative to the deliberately finite reference maxstep.
        close(last, 0.6, 0.002)?;
    }
    Ok(())
}

#[test]
fn waveform_identity_and_native_passthrough() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let bundle = prepared(
        dir.path(),
        "* waveform\nV1 a 0 PUL V1=0 V2=1 FREQ=1k DRATIO=0.5\n.end\n",
        InputFormat::ExpandedDeck,
    )?;
    let text = std::str::from_utf8(&bundle.primary.bytes)?;
    assert!(text.contains("mandatory prepared waveform binding for V1"));
    assert!(!text.contains(" PWL("));
    let artifact = &bundle.primary.filename;
    let declarations = &bundle.waveforms;
    ngspice_rs_preprocess::waveform::validate_artifact(declarations, artifact, b"v1 a 0 DC 1\n")?;
    for invalid in [b"V2 a 0 1\n".as_slice(), b"V1 a 0 1\nv1 b 0 2\n".as_slice()] {
        assert!(
            ngspice_rs_preprocess::waveform::validate_artifact(declarations, artifact, invalid)
                .is_err()
        );
    }
    let native = prepared(
        dir.path(),
        "* native\nV1 a 0 PULSE(0 1 0 0 0 1u 2u)\n.end\n",
        InputFormat::NativeDeck,
    )?;
    assert!(native.waveforms.is_empty());
    assert!(std::str::from_utf8(&native.primary.bytes)?.contains("PULSE(0 1 0 0 0 1u 2u)"));
    Ok(())
}
