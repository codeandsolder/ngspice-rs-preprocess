//! Trial policies are separate from physical evaluation and retain one
//! coherent coordinate point for every terminal-alias configuration.
use std::sync::Arc;

use model_kernel_api::{
    Descriptor, EquationKind, PointBuffer,
    batch::{BatchOutput, PreparedTile},
    trial::{TrialInput, TrialOutcome, TrialPhase},
};
use model_kernel_vdmos::{Breakdown, Model, Polarity, artifact::artifact};

fn points(descriptor: &Descriptor, polarity: Polarity, forward: bool) -> (Vec<f64>, Vec<f64>) {
    let sign = polarity.sign();
    let mut previous = Vec::new();
    let mut proposed = Vec::new();
    for equation in &descriptor.equations {
        let (old, new) = match equation.name.as_str() {
            "drain" => (
                0.25 * sign,
                if forward { -1000. * sign } else { 1000. * sign },
            ),
            "drain_prime" => (
                0.125 * sign,
                if forward { -500. * sign } else { 500. * sign },
            ),
            "gate" | "gate_prime" => (4. * sign, 100. * sign),
            "source" | "source_prime" => (0., 0.),
            "body_prime" => (0., 10. * sign),
            "junction" => (300.15, 100_000.),
            "case" => (300.15, -1000.),
            "recovery" => (0., 1e-10),
            name => panic!("unexpected equation {name}"),
        };
        previous.push(old);
        proposed.push(new);
    }
    (previous, proposed)
}

#[test]
fn extreme_trials_are_limited_coherently_across_all_series_aliases()
-> Result<(), Box<dyn std::error::Error>> {
    for polarity in [Polarity::N, Polarity::P] {
        for thermal in [false, true] {
            for series_mask in 0..16_u8 {
                let mut model = Model::defaults(polarity);
                model.series.rd = if series_mask & 1 == 0 { 0. } else { 0.02 };
                model.series.rg = if series_mask & 2 == 0 { 0. } else { 0.5 };
                model.series.rs = if series_mask & 4 == 0 { 0. } else { 0.01 };
                model.series.rb = if series_mask & 8 == 0 { 0. } else { 0.03 };
                model.diode.tt = 50e-9;
                model.diode.vp = 0.35;
                model.diode.breakdown = Some(Breakdown {
                    bv: 40.,
                    ibv: 1e-3,
                    nbv: 1.,
                });
                let compiled = artifact(Arc::new(model.prepare()?), thermal, [9; 16])?;
                let prepared = compiled.prepare(&[2.], 300.15)?;
                let width = compiled.descriptor().equations.len();
                let mut buffer = PointBuffer::new(width)?;
                for forward in [true, false] {
                    let (previous, proposed) = points(compiled.descriptor(), polarity, forward);
                    let saved = previous.clone();
                    let mut output = vec![0.; width];
                    compiled.evaluate(&prepared, &previous, &mut buffer.point())?;
                    assert!(
                        compiled
                            .evaluate(&prepared, &proposed, &mut buffer.point())
                            .is_err()
                    );
                    let input = TrialInput {
                        proposed: &proposed,
                        phase: TrialPhase::Predictor {
                            accepted: &previous,
                        },
                    };
                    assert_eq!(
                        compiled.linearize_trial(&prepared, input, &mut output)?,
                        TrialOutcome::Limited
                    );
                    compiled.evaluate(&prepared, &output, &mut buffer.point())?;
                    let first = output.clone();
                    assert_eq!(
                        compiled.linearize_trial(&prepared, input, &mut output)?,
                        TrialOutcome::Limited
                    );
                    assert_eq!(output, first, "retry must use immutable accepted reference");
                    assert_eq!(previous, saved);
                    assert_eq!(
                        compiled.linearize_trial(
                            &prepared,
                            TrialInput {
                                proposed: &first,
                                phase: TrialPhase::Exact
                            },
                            &mut output
                        )?,
                        TrialOutcome::Exact
                    );
                    assert_eq!(output, first);
                    let mut small = previous.clone();
                    for (coordinate, row) in small.iter_mut().zip(&compiled.descriptor().equations)
                    {
                        if row.name == "gate" || row.name == "gate_prime" {
                            *coordinate += 0.001 * polarity.sign();
                        }
                        if row.kind == EquationKind::Thermal {
                            *coordinate += 0.01;
                        }
                    }
                    assert_eq!(
                        compiled.linearize_trial(
                            &prepared,
                            TrialInput {
                                proposed: &small,
                                phase: TrialPhase::Newton {
                                    previous: &previous
                                }
                            },
                            &mut output
                        )?,
                        TrialOutcome::Exact
                    );
                    assert_eq!(output, small);
                    // A common-mode shift cannot change the local electrical
                    // differences that the family actually limits.
                    let mut shifted = proposed.clone();
                    for (coordinate, row) in
                        shifted.iter_mut().zip(&compiled.descriptor().equations)
                    {
                        if row.kind == EquationKind::Electrical {
                            *coordinate += 16.;
                        }
                    }
                    assert_eq!(
                        compiled.linearize_trial(
                            &prepared,
                            TrialInput {
                                proposed: &shifted,
                                phase: TrialPhase::Newton {
                                    previous: &previous
                                }
                            },
                            &mut output
                        )?,
                        TrialOutcome::Limited
                    );
                    for ((actual, expected), row) in output
                        .iter()
                        .zip(&first)
                        .zip(&compiled.descriptor().equations)
                    {
                        let offset = if row.kind == EquationKind::Electrical {
                            16.
                        } else {
                            0.
                        };
                        assert!(
                            (actual - expected - offset).abs() <= 1e-12,
                            "common mode: {row:?}"
                        );
                    }
                }
            }
        }
    }
    Ok(())
}

#[test]
fn scalar_tile_trial_evaluation_and_retry_are_identical() -> Result<(), Box<dyn std::error::Error>>
{
    for polarity in [Polarity::N, Polarity::P] {
        let mut model = Model::defaults(polarity);
        model.series.rd = 0.02;
        model.series.rg = 0.5;
        model.series.rs = 0.01;
        let compiled = artifact(Arc::new(model.prepare()?), true, [8; 16])?;
        let width = compiled.descriptor().equations.len();
        let (previous, forward) = points(compiled.descriptor(), polarity, true);
        let (_, reverse) = points(compiled.descriptor(), polarity, false);
        let inputs = [
            TrialInput {
                proposed: &forward,
                phase: TrialPhase::Newton {
                    previous: &previous,
                },
            },
            TrialInput {
                proposed: &reverse,
                phase: TrialPhase::Predictor {
                    accepted: &previous,
                },
            },
        ];
        let params = [(&[1.][..], 300.15), (&[3.][..], 350.)];
        let tile = PreparedTile::new(&compiled, &params)?;
        let mut coordinates = vec![0.; 2 * width];
        let mut outcomes = [TrialOutcome::Exact; 2];
        tile.linearize_trials(&inputs, &mut coordinates, &mut outcomes)?;
        assert_eq!(outcomes, [TrialOutcome::Limited; 2]);
        let mut batch = BatchOutput::new(2, width)?;
        tile.evaluate(&coordinates, &mut batch)?;
        let mut scalar = PointBuffer::new(width)?;
        let mut point = vec![0.; width];
        for (index, ((values, ambient), input)) in params.into_iter().zip(inputs).enumerate() {
            let instance = compiled.prepare(values, ambient)?;
            assert_eq!(
                compiled.linearize_trial(&instance, input, &mut point)?,
                outcomes[index]
            );
            assert_eq!(point, coordinates[index * width..(index + 1) * width]);
            compiled.evaluate(&instance, &point, &mut scalar.point())?;
            let a = scalar.point();
            let b = batch.point(index)?;
            assert_eq!(a.current, b.current);
            assert_eq!(a.conductance, b.conductance);
            assert_eq!(a.charge, b.charge);
            assert_eq!(a.capacitance, b.capacitance);
        }
        let saved = coordinates.clone();
        let invalid = vec![f64::NAN; width];
        assert!(
            tile.linearize_trials(
                &[
                    inputs[0],
                    TrialInput {
                        proposed: &invalid,
                        phase: TrialPhase::Exact
                    }
                ],
                &mut coordinates,
                &mut outcomes
            )
            .is_err()
        );
        tile.linearize_trials(&inputs, &mut coordinates, &mut outcomes)?;
        assert_eq!(coordinates, saved);
    }
    Ok(())
}
