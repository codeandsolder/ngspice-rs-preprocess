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
    use model_kernel_api::trial::{TrialInput, TrialOutcome, TrialPhase, TrialPolicy};

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

    Ok(())
}

#[test]
fn exact_trial_convergence_policy() -> Result<()> {
    use model_kernel_api::trial::{Convergence, ConvergenceInput, TrialOutcome, TrialSample};
    let model = Artifact::new(descriptor(), Linear)?;
    let prepared = model.prepare(&[2.], 300.)?;
    let proposed = [2., 1., 310.];
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

#[test]
fn physical_taylor_contract_rejects_bad_inputs_and_unavailable_kernels() -> Result<()> {
    use model_kernel_api::taylor::{MAX_ORDER, TaylorBuffer, TaylorCapability, TaylorInput};
    let mut invalid = descriptor();
    invalid.taylor = TaylorCapability::Computed {
        max_order: MAX_ORDER + 1,
    };
    assert!(Artifact::new(invalid, Linear).is_err());
    let mut invalid = descriptor();
    invalid.taylor = TaylorCapability::Computed { max_order: 2 };
    invalid.charge = Capability::Unavailable;
    assert!(Artifact::new(invalid, Linear).is_err());
    assert!(TaylorBuffer::new(0, 0).is_err());
    assert!(TaylorBuffer::new(usize::MAX, 1).is_err());
    assert!(TaylorBuffer::new(1000, MAX_ORDER).is_err());
    let model = Artifact::new(descriptor(), Linear)?;
    let prepared = model.prepare(&[2.], 300.)?;
    let input = TaylorInput {
        order: 1,
        coefficients: &[2., 1., 300., 0.1, 0., -2.],
    };
    let mut out = TaylorBuffer::new(3, 1)?;
    assert!(matches!(
        model.evaluate_taylor(&prepared, input, &mut out.point()),
        Err(Error::Unsupported(_))
    ));
    let mut available = descriptor();
    available.taylor = TaylorCapability::Computed { max_order: 1 };
    let model = Artifact::new(available, Linear)?;
    let other = model.prepare(&[2.], 300.)?;
    assert_eq!(
        model.evaluate_taylor(&prepared, input, &mut out.point()),
        Err(Error::ForeignArtifact)
    );
    assert!(
        model
            .evaluate_taylor(&other, TaylorInput { order: 2, ..input }, &mut out.point())
            .is_err()
    );
    assert!(
        model
            .evaluate_taylor(
                &other,
                TaylorInput {
                    coefficients: &[0.; 6],
                    ..input
                },
                &mut out.point()
            )
            .is_err()
    );
    assert!(
        model
            .evaluate_taylor(
                &other,
                TaylorInput {
                    coefficients: &[1., 2., 300., f64::NAN, 0., 0.],
                    ..input
                },
                &mut out.point()
            )
            .is_err()
    );
    assert!(
        model
            .evaluate_taylor(
                &other,
                TaylorInput {
                    coefficients: &[1., 2., 300.],
                    ..input
                },
                &mut out.point()
            )
            .is_err()
    );
    // Claiming support without an implementation does not silently use the ordinary point path.
    assert!(matches!(
        model.evaluate_taylor(&other, input, &mut out.point()),
        Err(Error::Unsupported(_))
    ));
    Ok(())
}

#[derive(Debug)]
enum TaylorWitness {
    Valid,
    Incomplete,
    Unbalanced,
    Gauge,
    Storage,
    GuardValid,
    GuardDuplicate,
    GuardInvalid,
    GuardOrder,
    GuardLimit,
    GuardUnbalanced,
    GuardKernelError,
}

impl Kernel for TaylorWitness {
    type Prepared = f64;
    fn prepare(&self, values: &[f64], temperature: f64) -> Result<f64> {
        Linear.prepare(values, temperature)
    }
    fn evaluate(&self, data: &f64, x: &[f64], out: &mut Point<'_>) -> Result<()> {
        Linear.evaluate(data, x, out)
    }
    fn evaluate_taylor(
        &self,
        _: &f64,
        input: model_kernel_api::taylor::TaylorInput<'_>,
        out: &mut model_kernel_api::taylor::TaylorPoint<'_>,
    ) -> Result<()> {
        // A separate degree-one witness; point zero is always valid, and
        // only the higher degree contains the deliberately malformed data.
        out.current.fill(0.);
        out.conductance.fill(0.);
        out.charge.fill(0.);
        out.capacitance.fill(0.);
        match self {
            Self::Valid => {}
            Self::Incomplete => out.current[3] = f64::NAN,
            Self::Unbalanced => out.current[3] = 1.,
            Self::Gauge => out.conductance[9 + 6] = 1.,
            Self::Storage => {
                out.charge[3] = 1.;
                out.charge[4] = -1.;
            }
            Self::GuardValid
            | Self::GuardDuplicate
            | Self::GuardUnbalanced
            | Self::GuardKernelError => {
                use model_kernel_api::taylor::{GuardPredicate, GuardRecorder};
                let mut recorder = GuardRecorder::new(out.guards);
                let lhs = input.coordinate(3, 0)?;
                let rhs = input.coordinate(3, 1)?;
                recorder.select(7, GuardPredicate::Less, lhs, rhs, true);
                recorder.select(8, GuardPredicate::Greater, lhs, rhs, false);
                if matches!(self, Self::GuardDuplicate) {
                    recorder.select(7, GuardPredicate::Greater, lhs, rhs, true);
                }
                if matches!(self, Self::GuardUnbalanced) {
                    out.current[3] = 1.;
                }
                if matches!(self, Self::GuardKernelError) {
                    return Err(Error::Domain("witness kernel failure".into()));
                }
            }
            Self::GuardInvalid => {
                use model_kernel_api::taylor::{GuardPredicate, GuardRecorder};
                GuardRecorder::new(out.guards).select(
                    1,
                    GuardPredicate::Less,
                    f64::INFINITY,
                    0.,
                    true,
                );
            }
            Self::GuardOrder => {
                use model_kernel_api::taylor::{
                    GuardPredicate, GuardRecorder,
                    arithmetic::{Scalar, Series},
                };
                GuardRecorder::new(out.guards).select(
                    1,
                    GuardPredicate::Greater,
                    Series::from_coefficients([0., 0., 1.])?,
                    Series::constant(0.),
                    true,
                );
            }
            Self::GuardLimit => {
                use model_kernel_api::taylor::{GuardPredicate, GuardRecorder};
                let mut recorder = GuardRecorder::new(out.guards);
                for site in 0..=256 {
                    recorder.select(site, GuardPredicate::Greater, 1., 0., true);
                }
            }
        }
        Ok(())
    }
}

#[test]
fn physical_taylor_checks_every_degree_and_known_zero_charge() -> Result<()> {
    use model_kernel_api::taylor::{TaylorBuffer, TaylorCapability, TaylorInput};
    let input = TaylorInput {
        order: 1,
        coefficients: &[2., 1., 300., 0.1, 0., 0.],
    };
    let mut buffer = TaylorBuffer::new(3, 1)?;
    for witness in [
        TaylorWitness::Valid,
        TaylorWitness::Incomplete,
        TaylorWitness::Unbalanced,
        TaylorWitness::Gauge,
        TaylorWitness::Storage,
        TaylorWitness::GuardValid,
        TaylorWitness::GuardDuplicate,
        TaylorWitness::GuardInvalid,
        TaylorWitness::GuardOrder,
        TaylorWitness::GuardLimit,
        TaylorWitness::GuardUnbalanced,
        TaylorWitness::GuardKernelError,
    ] {
        let expected = match witness {
            TaylorWitness::Valid | TaylorWitness::GuardValid => Ok(()),
            TaylorWitness::Incomplete => Err(Error::Domain(
                "incomplete static residual or Jacobian".into(),
            )),
            TaylorWitness::Unbalanced | TaylorWitness::Gauge | TaylorWitness::GuardUnbalanced => {
                Err(Error::Conservation)
            }
            TaylorWitness::GuardKernelError => Err(Error::Domain("witness kernel failure".into())),
            TaylorWitness::Storage => Err(Error::Domain("nonzero declared-zero storage".into())),
            TaylorWitness::GuardDuplicate => Err(Error::Schema("duplicate physical guard site")),
            TaylorWitness::GuardInvalid | TaylorWitness::GuardOrder => {
                Err(Error::Domain("inconsistent physical guard germ".into()))
            }
            TaylorWitness::GuardLimit => Err(Error::Shape("physical guard site limit")),
        };
        let mut d = descriptor();
        d.charge = Capability::Zero;
        d.taylor = TaylorCapability::Computed { max_order: 1 };
        let a = Artifact::new(d, witness)?;
        let p = a.prepare(&[2.], 300.)?;
        assert_eq!(a.evaluate_taylor(&p, input, &mut buffer.point()), expected);
        if expected.is_err() {
            assert!(buffer.point().guards.records().is_empty());
        }
    }
    Ok(())
}

#[test]
fn executed_guard_trace_is_one_sided_and_reusable_across_tiles() -> Result<()> {
    use model_kernel_api::taylor::{GuardPredicate, TaylorBuffer, TaylorCapability, TaylorInput};
    let mut descriptor = descriptor();
    descriptor.taylor = TaylorCapability::Computed { max_order: 1 };
    descriptor.charge = Capability::Zero;
    let a = Artifact::new(descriptor.clone(), TaylorWitness::GuardValid)?;
    let p = a.prepare(&[2.], 300.)?;
    let tile = PreparedTile::new(&a, &[(&[2.], 300.), (&[3.], 300.)])?;
    let mut buffer = TaylorBuffer::new(3, 1)?;
    for slope in [-1., 0., 1.] {
        let input = TaylorInput {
            order: 1,
            coefficients: &[2., 2., 300., slope, 0., 0.],
        };
        a.evaluate_taylor(&p, input, &mut buffer.point())?;
        let verify = |out: &mut TaylorBuffer| {
            let point = out.point();
            let guards = point.guards.records();
            assert_eq!(guards.len(), 1);
            let guard = &guards[0];
            assert_eq!(guard.site, 7);
            assert_eq!(guard.predicate, GuardPredicate::Less);
            assert_eq!(guard.selected, slope < 0.);
            assert_eq!(guard.surface.coefficient(0).to_bits(), 0_f64.to_bits());
            assert_eq!(guard.surface.coefficient(1).to_bits(), slope.to_bits());
        };
        verify(&mut buffer);
        tile.evaluate_lane_taylor(1, input, &mut buffer.point())?;
        verify(&mut buffer);
        assert!(
            tile.evaluate_lane_taylor(2, input, &mut buffer.point())
                .is_err()
        );
        assert!(buffer.point().guards.records().is_empty());
    }
    let empty = Artifact::new(descriptor, TaylorWitness::Valid)?;
    let p = empty.prepare(&[2.], 300.)?;
    empty.evaluate_taylor(
        &p,
        TaylorInput {
            order: 1,
            coefficients: &[2., 2., 300., 0., 0., 0.],
        },
        &mut buffer.point(),
    )?;
    assert!(buffer.point().guards.records().is_empty());
    Ok(())
}
