//! Independent contract and ownership tests.

use model_kernel_api::{
    ABI_VERSION, Artifact, Binding, Capability, Descriptor, Equation, EquationKind, Error, Kernel,
    Parameter, Point, PointBuffer, Result,
    batch::{BatchOutput, PreparedTile},
    noise::{Injection, Sink, Source},
};

#[derive(Debug)]
struct Linear;

impl Kernel for Linear {
    type Prepared = f64;

    fn prepare(&self, values: &[f64], _: f64) -> Result<f64> {
        values.first().copied().ok_or(Error::Shape("parameter"))
    }

    fn evaluate(&self, conductance: &f64, x: &[f64], out: &mut Point<'_>) -> Result<()> {
        let v = x[0] - x[1];
        let current = conductance * v;
        out.current
            .copy_from_slice(&[current, -current, x[2] - 300.]);
        out.conductance.copy_from_slice(&[
            *conductance,
            -*conductance,
            0.,
            -*conductance,
            *conductance,
            0.,
            0.,
            0.,
            1.,
        ]);
        out.charge.copy_from_slice(&[v, -v, x[2]]);
        out.capacitance
            .copy_from_slice(&[1., -1., 0., -1., 1., 0., 0., 0., 1.]);
        Ok(())
    }

    fn evaluate_noise(
        &self,
        conductance: &f64,
        _: &[f64],
        _: f64,
        sink: &mut dyn Sink,
    ) -> Result<()> {
        sink.push(Source {
            name: "thermal",
            density: *conductance,
            injections: &[
                Injection {
                    row: 0,
                    re: 1.,
                    im: 0.,
                },
                Injection {
                    row: 1,
                    re: -1.,
                    im: 0.,
                },
                Injection {
                    row: 2,
                    re: 2.,
                    im: 1.,
                },
            ],
        })
    }
}

fn descriptor() -> Descriptor {
    Descriptor {
        family: "linear".into(),
        fingerprint: [1; 16],
        abi: ABI_VERSION,
        taylor: model_kernel_api::taylor::TaylorCapability::Unavailable,
        equations: [
            ("positive", EquationKind::Electrical),
            ("negative", EquationKind::Electrical),
            ("heat", EquationKind::Thermal),
        ]
        .into_iter()
        .map(|(name, kind)| Equation {
            name: name.into(),
            kind,
            binding: Binding::Terminal,
        })
        .collect(),
        parameters: vec![Parameter {
            name: "conductance".into(),
            default: 2.,
            changes_topology: false,
        }],
        charge: Capability::Computed,
        noise: Capability::Computed,
        trial: model_kernel_api::trial::TrialCapabilities::ENGINE,
    }
}

#[test]
fn independent_candidates_and_contiguous_tile() -> Result<()> {
    let model = Artifact::new(descriptor(), Linear)?;
    let first = model.prepare(&[2.], 300.)?;
    let second = model.prepare(&[3.], 310.)?;
    let foreign = Artifact::new(descriptor(), Linear)?;
    let mut buffer = PointBuffer::new(3)?;
    let coords = [2., 1., 310.];
    assert_eq!(
        foreign.evaluate(&first, &coords, &mut buffer.point()),
        Err(Error::ForeignArtifact)
    );
    model.evaluate(&first, &coords, &mut buffer.point())?;
    let first_current = buffer.point().current.to_vec();
    model.evaluate(&second, &coords, &mut buffer.point())?;
    assert_eq!(buffer.point().current[0].to_bits(), 3_f64.to_bits());
    model.evaluate(&first, &coords, &mut buffer.point())?;
    assert_eq!(buffer.point().current, first_current);
    let tile = PreparedTile::new(&model, &[(&[2.], 300.), (&[3.], 310.)])?;
    let mut out = BatchOutput::new(3, 2)?;
    tile.evaluate(&[2., 1., 310., 2., 1., 310.], &mut out)?;
    assert_eq!(out.point(0)?.current, first_current);
    assert_eq!(out.point(1)?.current[0].to_bits(), 3_f64.to_bits());
    assert!(tile.evaluate(&[0.; 5], &mut out).is_err());
    out.current.pop();
    assert!(tile.evaluate(&[0.; 6], &mut out).is_err());
    Ok(())
}

#[test]
fn conservation_rejects_overflow_and_missing_columns() -> Result<()> {
    let mut buffer = PointBuffer::new(3)?;
    let d = descriptor();
    let mut point = buffer.point();
    point.reset(Capability::Computed);
    assert!(point.validate(&d).is_err());
    point.current.copy_from_slice(&[f64::MAX, f64::MAX, 0.]);
    point.conductance.fill(0.);
    point.charge.fill(0.);
    point.capacitance.fill(0.);
    assert_eq!(point.validate(&d), Err(Error::Conservation));
    point.current.copy_from_slice(&[f64::MAX, -f64::MAX, 1.]);
    point.validate(&d)?;
    point.conductance[6] = 1.;
    assert_eq!(point.validate(&d), Err(Error::Conservation));
    Ok(())
}

#[derive(Default)]
struct Collector(usize);
impl Sink for Collector {
    fn push(&mut self, source: Source<'_>) -> Result<()> {
        assert_eq!(source.density.to_bits(), 2_f64.to_bits());
        assert_eq!(source.injections.len(), 3);
        self.0 += 1;
        Ok(())
    }
}

#[test]
fn metadata_structural_edits_and_noise() -> Result<()> {
    let mut d = descriptor();
    d.parameters[0].changes_topology = true;
    let model = Artifact::new(d.clone(), Linear)?;
    assert!(model.prepare(&[3.], 300.).is_err());
    assert!(model.prepare(&[2.], 0.).is_err());
    let prepared = model.prepare(&[2.], 300.)?;
    let mut collector = Collector::default();
    model.evaluate_noise(&prepared, &[1., 0., 300.], 1e3, &mut collector)?;
    assert_eq!(collector.0, 1);
    assert!(
        model
            .evaluate_noise(&prepared, &[1., 0., 300.], 0., &mut collector)
            .is_err()
    );
    d.equations[1].name = d.equations[0].name.clone();
    assert!(Artifact::new(d, Linear).is_err());
    Ok(())
}

#[test]
fn explicit_trial_capabilities_and_validation() -> Result<()> {
    use model_kernel_api::trial::{
        Convergence, ConvergenceInput, TrialInput, TrialOutcome, TrialPhase, TrialPolicy,
        TrialSample,
    };

    let model = Artifact::new(descriptor(), Linear)?;
    let prepared = model.prepare(&[2.], 300.)?;
    let proposed = [2., 1., 310.];
    let mut output = [0.; 3];
    let input = TrialInput {
        proposed: &proposed,
        phase: TrialPhase::ColdStart,
    };
    assert_eq!(
        model.linearize_trial(&prepared, input, &mut output)?,
        TrialOutcome::Exact
    );
    assert_eq!(output, proposed);
    let foreign = Artifact::new(descriptor(), Linear)?;
    assert_eq!(
        foreign.linearize_trial(&prepared, input, &mut output),
        Err(Error::ForeignArtifact)
    );
    for phase in [
        TrialPhase::Newton {
            previous: &[0., 0.],
        },
        TrialPhase::Predictor {
            accepted: &[0., 0., 0.],
        },
    ] {
        assert!(
            model
                .linearize_trial(
                    &prepared,
                    TrialInput {
                        proposed: &proposed,
                        phase
                    },
                    &mut output
                )
                .is_err()
        );
    }
    assert!(
        model
            .linearize_trial(
                &prepared,
                TrialInput {
                    proposed: &[0., 0., -1.],
                    phase: TrialPhase::Exact
                },
                &mut output
            )
            .is_err()
    );
    let mut d = descriptor();
    d.trial.initialization = TrialPolicy::Kernel;
    let declared = Artifact::new(d, Linear)?;
    let candidate = declared.prepare(&[2.], 300.)?;
    assert_eq!(
        declared.linearize_trial(&candidate, input, &mut output),
        Err(Error::Unsupported("trial initialization"))
    );

    let sample = TrialSample {
        coordinates: &proposed,
        residual: &[2., -2., 10.],
        jacobian: &[0.; 9],
    };
    let mut convergence = ConvergenceInput {
        previous: sample,
        candidate: sample,
        candidate_outcome: TrialOutcome::Exact,
        relative_tolerance: 1e-6,
        absolute_tolerances: &[1e-12; 3],
    };
    assert_eq!(
        model.check_trial_convergence(&prepared, convergence)?,
        Convergence::Satisfied
    );
    assert_eq!(
        convergence.compare_residual(0, f64::MAX, -f64::MAX)?,
        Convergence::Retry
    );
    assert_eq!(
        convergence.compare_residual(0, f64::MAX, f64::MAX)?,
        Convergence::Satisfied
    );
    assert_eq!(
        convergence.compare_residual(0, f64::INFINITY, 0.)?,
        Convergence::Retry
    );
    convergence.candidate_outcome = TrialOutcome::Limited;
    assert_eq!(
        model.check_trial_convergence(&prepared, convergence)?,
        Convergence::Retry
    );
    convergence.absolute_tolerances = &[];
    assert!(
        model
            .check_trial_convergence(&prepared, convergence)
            .is_err()
    );
    Ok(())
}
