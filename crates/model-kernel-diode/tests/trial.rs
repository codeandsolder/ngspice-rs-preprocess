//! Exercise the prepared ABI with caller-owned Newton and accepted histories.
use model_kernel_api::{
    Artifact, PointBuffer, Prepared,
    batch::{BatchOutput, PreparedTile},
    trial::{Convergence, ConvergenceInput, TrialInput, TrialOutcome, TrialPhase, TrialSample},
};
use model_kernel_diode::{
    Instance,
    artifact::{DiodeKernel, artifact},
    compile_simetrix,
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const VT: f64 = (1.380_622_6e-23 / 1.602_191_8e-19) * 300.15;
const PARAMS: [f64; 4] = [1., 1., 0., 0.];

fn sample<'a>(coordinates: &'a [f64], buffer: &'a mut PointBuffer) -> TrialSample<'a> {
    let point = buffer.point();
    TrialSample {
        coordinates,
        residual: point.current,
        jacobian: point.conductance,
    }
}

#[test]
fn forward_reverse_topology_and_rejected_trials() -> Result<()> {
    for resistance in [0., 3.] {
        let model = compile_simetrix([
            ("IS", 1e-14),
            ("RS", resistance),
            ("BV", 4.),
            ("IBV", 1e-5),
            ("NBV", 1.4),
            ("IBVL", 1e-7),
            ("NBVL", 2.2),
        ])?;
        let compiled = artifact(model, Instance::default(), [7; 16])?;
        let candidate = compiled.prepare(&PARAMS, 300.15)?;
        let width = compiled.descriptor().equations.len();
        let intrinsic = if width == 3 { 2 } else { 0 };
        let mut raw = vec![12.; width];
        raw[intrinsic] = 1012.;
        let mut output = vec![0.; width];
        assert_eq!(
            compiled.linearize_trial(
                &candidate,
                TrialInput {
                    proposed: &raw,
                    phase: TrialPhase::ColdStart
                },
                &mut output
            )?,
            TrialOutcome::Initialized
        );
        let critical = VT * (VT / (2_f64.sqrt() * 1e-14)).ln();
        assert!((output[intrinsic] - 12. - critical).abs() < 1e-12);
        let accepted = output.clone();
        let mut buffer = PointBuffer::new(width)?;
        assert!(
            compiled
                .evaluate(&candidate, &raw, &mut buffer.point())
                .is_err()
        );
        assert_eq!(
            compiled.linearize_trial(
                &candidate,
                TrialInput {
                    proposed: &raw,
                    phase: TrialPhase::Predictor {
                        accepted: &accepted
                    }
                },
                &mut output
            )?,
            TrialOutcome::Limited
        );
        compiled.evaluate(&candidate, &output, &mut buffer.point())?;
        assert!(output[intrinsic] > accepted[intrinsic] && output[intrinsic] < 1012.);
        assert_eq!(output[1], 12.);
        if width == 3 {
            assert_eq!(output[0], 12.);
        }
        let first = output.clone();
        let mut reverse = raw.clone();
        reverse[intrinsic] = -988.;
        assert_eq!(
            compiled.linearize_trial(
                &candidate,
                TrialInput {
                    proposed: &reverse,
                    phase: TrialPhase::Newton { previous: &first }
                },
                &mut output
            )?,
            TrialOutcome::Limited
        );
        assert!(output[intrinsic] - 12. < -4. && output[intrinsic] > reverse[intrinsic]);
        compiled.evaluate(&candidate, &output, &mut buffer.point())?;
        // Reject that trial: retry from exactly the same accepted coordinates.
        assert_eq!(
            compiled.linearize_trial(
                &candidate,
                TrialInput {
                    proposed: &raw,
                    phase: TrialPhase::Predictor {
                        accepted: &accepted
                    }
                },
                &mut output
            )?,
            TrialOutcome::Limited
        );
        assert_eq!(output, first);
        assert_eq!(
            compiled.linearize_trial(
                &candidate,
                TrialInput {
                    proposed: &accepted,
                    phase: TrialPhase::Exact
                },
                &mut output
            )?,
            TrialOutcome::Exact
        );
        assert_eq!(output, accepted);
    }
    Ok(())
}

#[test]
fn tile_policy_and_physical_evaluation_agree_with_scalar() -> Result<()> {
    let compiled = artifact(
        compile_simetrix([("IS", 1e-14)])?,
        Instance::default(),
        [3; 16],
    )?;
    let params = [2., 3., 0., 0.];
    let first = compiled.prepare(&PARAMS, 300.15)?;
    let second = compiled.prepare(&params, 340.)?;
    let tile = PreparedTile::new(&compiled, &[(&PARAMS, 300.15), (&params, 340.)])?;
    let inputs = [
        TrialInput {
            proposed: &[1000., 0.],
            phase: TrialPhase::Newton {
                previous: &[0.65, 0.],
            },
        },
        TrialInput {
            proposed: &[1000., 0.],
            phase: TrialPhase::Predictor {
                accepted: &[0.4, 0.],
            },
        },
    ];
    let mut points = [0.; 4];
    let mut outcomes = [TrialOutcome::Exact; 2];
    tile.linearize_trials(&inputs, &mut points, &mut outcomes)?;
    assert_eq!(outcomes, [TrialOutcome::Limited; 2]);
    let mut batch = BatchOutput::new(2, 2)?;
    tile.evaluate(&points, &mut batch)?;
    for (index, prepared) in [&first, &second].into_iter().enumerate() {
        let mut output = [0.; 2];
        assert_eq!(
            compiled.linearize_trial(prepared, inputs[index], &mut output)?,
            outcomes[index]
        );
        assert_eq!(output, points[2 * index..2 * index + 2]);
        let mut lane = [0.; 2];
        assert_eq!(
            tile.linearize_lane_trial(index, inputs[index], &mut lane)?,
            outcomes[index]
        );
        assert_eq!(lane, output);
        let mut scalar = PointBuffer::new(2)?;
        compiled.evaluate(prepared, &output, &mut scalar.point())?;
        let a = scalar.point();
        let b = batch.point(index)?;
        assert_eq!(a.current, b.current);
        assert_eq!(a.conductance, b.conductance);
        assert_eq!(a.charge, b.charge);
        assert_eq!(a.capacitance, b.capacitance);
    }
    let saved = points;
    let invalid = [
        inputs[0],
        TrialInput {
            proposed: &[f64::NAN, 0.],
            phase: TrialPhase::Exact,
        },
    ];
    assert!(
        tile.linearize_trials(&invalid, &mut points, &mut outcomes)
            .is_err()
    );
    tile.linearize_trials(&inputs, &mut points, &mut outcomes)?;
    assert_eq!(points, saved);
    Ok(())
}

#[test]
fn additional_convergence_tests_the_exact_nonlinear_residual() -> Result<()> {
    let compiled = artifact(
        compile_simetrix([("IS", 1e-14)])?,
        Instance::default(),
        [2; 16],
    )?;
    let prepared = compiled.prepare(&PARAMS, 300.15)?;
    let mut old = PointBuffer::new(2)?;
    let mut new = PointBuffer::new(2)?;
    let x = [0.6, 0.];
    compiled.evaluate(&prepared, &x, &mut old.point())?;
    for (y, expected) in [
        ([0.8, 0.], Convergence::Retry),
        ([0.600_000_000_1, 0.], Convergence::Satisfied),
    ] {
        compiled.evaluate(&prepared, &y, &mut new.point())?;
        let mut input = ConvergenceInput {
            previous: sample(&x, &mut old),
            candidate: sample(&y, &mut new),
            candidate_outcome: TrialOutcome::Exact,
            relative_tolerance: 1e-6,
            absolute_tolerances: &[1e-12; 2],
        };
        assert_eq!(
            compiled.check_trial_convergence(&prepared, input)?,
            expected
        );
        input.candidate_outcome = TrialOutcome::Limited;
        assert_eq!(
            compiled.check_trial_convergence(&prepared, input)?,
            Convergence::Retry
        );
    }
    Ok(())
}

struct LoadLine<'a> {
    artifact: &'a Artifact<DiodeKernel>,
    prepared: &'a Prepared<DiodeKernel>,
    source: f64,
    capacitance_over_dt: f64,
    accepted: f64,
}

impl LoadLine<'_> {
    fn solve(&self, cold: bool) -> Result<(f64, usize)> {
        let accepted = [self.accepted, 0.];
        let mut proposed = [self.source, 0.];
        let mut previous = accepted;
        let mut old = PointBuffer::new(2)?;
        let mut current = PointBuffer::new(2)?;
        let mut limited = 0;
        for iteration in 0..100 {
            let phase = if iteration == 0 {
                if cold {
                    TrialPhase::ColdStart
                } else {
                    TrialPhase::Predictor {
                        accepted: &accepted,
                    }
                }
            } else {
                TrialPhase::Newton {
                    previous: &previous,
                }
            };
            let mut point = [0.; 2];
            let outcome = self.artifact.linearize_trial(
                self.prepared,
                TrialInput {
                    proposed: &proposed,
                    phase,
                },
                &mut point,
            )?;
            limited += usize::from(outcome == TrialOutcome::Limited);
            self.artifact
                .evaluate(self.prepared, &point, &mut current.point())?;
            let nodal = current.point();
            let residual = self.capacitance_over_dt.mul_add(
                point[0] - self.accepted,
                nodal.current[0] + (point[0] - self.source) / 1000.,
            );
            let jacobian = nodal.conductance[0] + 0.001 + self.capacitance_over_dt;
            let correction = residual / jacobian;
            if iteration > 0
                && outcome == TrialOutcome::Exact
                && correction.abs() < 1e-11
                && residual.abs() < 1e-9
            {
                let input = ConvergenceInput {
                    previous: sample(&previous, &mut old),
                    candidate: sample(&point, &mut current),
                    candidate_outcome: outcome,
                    relative_tolerance: 1e-7,
                    absolute_tolerances: &[1e-12; 2],
                };
                if self
                    .artifact
                    .check_trial_convergence(self.prepared, input)?
                    == Convergence::Satisfied
                {
                    return Ok((point[0], limited));
                }
            }
            proposed = [point[0] - correction, 0.];
            previous = point;
            std::mem::swap(&mut current, &mut old);
        }
        Err(format!(
            "Newton failed: source={} accepted={} proposed={proposed:?}",
            self.source, self.accepted
        )
        .into())
    }

    fn reference(&self) -> f64 {
        // Independent monotone Shockley load-line bisection; no kernel evaluation,
        // limiting, derivatives or Newton iteration enters this oracle.
        let mut lo = self.source.min(self.accepted).min(0.) - 1.;
        let mut hi = self.source.max(self.accepted).max(0.) + 1.;
        for _ in 0..80 {
            let x = f64::midpoint(lo, hi);
            let residual = self.capacitance_over_dt.mul_add(
                x - self.accepted,
                1e-14_f64.mul_add((x / VT).exp_m1(), (x - self.source) / 1000.),
            );
            if residual > 0. {
                hi = x;
            } else {
                lo = x;
            }
        }
        f64::midpoint(lo, hi)
    }
}

#[test]
fn difficult_bias_and_transient_against_independent_load_line() -> Result<()> {
    let compiled = artifact(
        compile_simetrix([("IS", 1e-14), ("N", 1.)])?,
        Instance::default(),
        [4; 16],
    )?;
    let prepared = compiled.prepare(&PARAMS, 300.15)?;
    let mut limited = 0;
    for source in [-2., 0., 1., 1000.] {
        let load = LoadLine {
            artifact: &compiled,
            prepared: &prepared,
            source,
            capacitance_over_dt: 0.,
            accepted: 0.,
        };
        let (actual, count) = load.solve(true)?;
        assert!(
            (actual - load.reference()).abs() < 1e-10,
            "{source}: {actual}"
        );
        limited += count;
    }
    let mut accepted = -2.;
    for step in 1..=3000 {
        let source = if step <= 1000 { 1000. } else { -2. };
        let load = LoadLine {
            artifact: &compiled,
            prepared: &prepared,
            source,
            capacitance_over_dt: 0.5,
            accepted,
        };
        let (actual, count) = load.solve(false)?;
        assert!(
            (actual - load.reference()).abs() < 1e-10,
            "{step}: {actual}"
        );
        accepted = actual;
        limited += count;
    }
    assert!(limited > 1000, "test must exercise the limiter: {limited}");
    Ok(())
}

fn ngspice_output(fixture: &str) -> Result<String> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(fixture);
    let output = std::process::Command::new("ngspice")
        .args(["-n", "-b"])
        .arg(path)
        .output()?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

fn measured(output: &str, name: &str) -> Result<f64> {
    for line in output.lines() {
        if let Some((key, value)) = line.split_once('=')
            && key.trim() == name
        {
            return Ok(value.trim().parse()?);
        }
    }
    Err(format!("missing ngspice measure {name}").into())
}

#[test]
#[ignore = "requires an installed ngspice C reference"]
fn difficult_load_line_and_ramp_against_ngspice() -> Result<()> {
    let compiled = artifact(
        compile_simetrix([("IS", 1e-14), ("N", 1.)])?,
        Instance::default(),
        [5; 16],
    )?;
    let prepared = compiled.prepare(&PARAMS, 300.15)?;
    let dc = LoadLine {
        artifact: &compiled,
        prepared: &prepared,
        source: 1000.,
        capacitance_over_dt: 0.,
        accepted: 0.,
    };
    let actual = dc.solve(true)?.0;
    let expected = measured(&ngspice_output("trial-load-line.cir")?, "v(out)")?;
    // SIMetrix uses historical k/q constants, not ngspice's constants.
    assert!(
        (actual - expected).abs() < 5e-5,
        "dc actual={actual} C={expected}"
    );
    let output = ngspice_output("trial-transient.cir")?;
    let observations = [
        (2100, "ramp_up"),
        (4000, "high"),
        (6100, "ramp_down"),
        (7000, "recovery1"),
        (8000, "recovery2"),
        (14000, "low"),
    ];
    let mut accepted = -2.;
    for tick in 1..=16000 {
        let source = match tick {
            2001..=2200 => -2. + 1002. * f64::from(tick - 2000) / 200.,
            2201..=6000 => 1000.,
            6001..=6200 => 1000. - 1002. * f64::from(tick - 6000) / 200.,
            _ => -2.,
        };
        let load = LoadLine {
            artifact: &compiled,
            prepared: &prepared,
            source,
            capacitance_over_dt: 2.,
            accepted,
        };
        accepted = load.solve(false)?.0;
        for (at, name) in observations {
            if tick == at {
                let expected = measured(&output, name)?;
                // Fixed-step backward Euler versus adaptive Gear, in addition
                // to different physical constants.
                assert!(
                    (accepted - expected).abs() < 0.002,
                    "{name}: actual={accepted} C={expected}"
                );
            }
        }
    }
    Ok(())
}

#[test]
fn selective_trial_lane_uses_committed_candidate_and_retains_rejected_state() -> Result<()> {
    use model_kernel_api::batch::LaneUpdate;
    let compiled = artifact(
        compile_simetrix([("IS", 1e-14)])?,
        Instance::default(),
        [9; 16],
    )?;
    let mut tile = PreparedTile::new(&compiled, &[(&PARAMS, 300.15), (&PARAMS, 320.)])?;
    let proposed = [1000., 0.];
    let accepted = [0.6, 0.];
    let trial = TrialInput {
        proposed: &proposed,
        phase: TrialPhase::Predictor {
            accepted: &accepted,
        },
    };
    let mut before = [[0.; 2]; 2];
    for (lane, point) in before.iter_mut().enumerate() {
        assert_eq!(
            tile.linearize_lane_trial(lane, trial, point)?,
            TrialOutcome::Limited
        );
    }
    let params = [2., 3., 0., 0.];
    let update = LaneUpdate {
        lane: 1,
        values: &params,
        ambient_kelvin: 380.,
    };
    drop(tile.stage_replacements(&compiled, &[update])?);
    let mut point = [0.; 2];
    assert_eq!(
        tile.linearize_lane_trial(1, trial, &mut point)?,
        TrialOutcome::Limited
    );
    assert_eq!(point, before[1]);
    tile.stage_replacements(&compiled, &[update])?.commit();
    assert_eq!(
        tile.linearize_lane_trial(0, trial, &mut point)?,
        TrialOutcome::Limited
    );
    assert_eq!(point, before[0]);
    assert_eq!(
        tile.linearize_lane_trial(1, trial, &mut point)?,
        TrialOutcome::Limited
    );
    assert_ne!(point, before[1]);
    let scalar = compiled.prepare(&params, 380.)?;
    let mut expected = [0.; 2];
    assert_eq!(
        compiled.linearize_trial(&scalar, trial, &mut expected)?,
        TrialOutcome::Limited
    );
    assert_eq!(point, expected);
    assert!(tile.linearize_lane_trial(2, trial, &mut point).is_err());
    let mut old = PointBuffer::new(2)?;
    let mut new = PointBuffer::new(2)?;
    let candidate = [0.601, 0.];
    tile.evaluate_lane(1, &accepted, &mut old.point())?;
    tile.evaluate_lane(1, &candidate, &mut new.point())?;
    let input = ConvergenceInput {
        previous: sample(&accepted, &mut old),
        candidate: sample(&candidate, &mut new),
        candidate_outcome: TrialOutcome::Exact,
        relative_tolerance: 1e-6,
        absolute_tolerances: &[1e-12; 2],
    };
    let expected = compiled.check_trial_convergence(&scalar, input)?;
    assert_eq!(tile.check_lane_trial_convergence(1, input)?, expected);
    assert!(tile.check_lane_trial_convergence(2, input).is_err());
    Ok(())
}
