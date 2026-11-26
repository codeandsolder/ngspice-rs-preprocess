//! Independent instrumentation of preparation, ownership and lane evaluation.

use model_kernel_api::{
    ABI_VERSION, Artifact, Binding, Capability, Descriptor, Equation, EquationKind, Error, Kernel,
    Parameter, Point, PointBuffer, Result,
    batch::{BatchOutput, LaneUpdate, PreparedTile, evaluate_scalar_tile},
    noise::{Injection, Sink, Source},
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Debug, Default)]
struct Counts {
    preparations: AtomicUsize,
    batches: AtomicUsize,
}

#[derive(Debug)]
struct Witness {
    conductance: f64,
    ambient: f64,
}

#[derive(Debug)]
struct CountingKernel(Arc<Counts>);

impl Kernel for CountingKernel {
    // Deliberately not Clone: a selective update cannot copy the old tile.
    type Prepared = Witness;

    fn prepare(&self, values: &[f64], ambient_kelvin: f64) -> Result<Witness> {
        self.0.preparations.fetch_add(1, Ordering::Relaxed);
        if values[0] < 0. {
            return Err(Error::Domain("negative conductance".into()));
        }
        Ok(Witness {
            conductance: values[0],
            ambient: ambient_kelvin,
        })
    }

    fn evaluate(&self, prepared: &Witness, x: &[f64], out: &mut Point<'_>) -> Result<()> {
        let g = prepared.conductance * prepared.ambient / 300.;
        let c = prepared.ambient * 1e-12;
        let voltage = x[0] - x[1];
        out.current.copy_from_slice(&[g * voltage, -g * voltage]);
        out.conductance.copy_from_slice(&[g, -g, -g, g]);
        out.charge.copy_from_slice(&[c * voltage, -c * voltage]);
        out.capacitance.copy_from_slice(&[c, -c, -c, c]);
        Ok(())
    }

    fn evaluate_batch(
        &self,
        prepared: &[Witness],
        coordinates: &[f64],
        out: &mut BatchOutput,
    ) -> Result<()> {
        self.0.batches.fetch_add(1, Ordering::Relaxed);
        evaluate_scalar_tile(self, prepared, coordinates, out)
    }

    fn evaluate_noise(
        &self,
        prepared: &Witness,
        _coordinates: &[f64],
        frequency: f64,
        sink: &mut dyn Sink,
    ) -> Result<()> {
        sink.push(Source {
            name: "thermal",
            density: prepared.conductance * prepared.ambient / frequency,
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
            ],
        })
    }
}

fn artifact(counts: &Arc<Counts>) -> Result<Arc<Artifact<CountingKernel>>> {
    Artifact::new(
        Descriptor {
            family: "witness".into(),
            fingerprint: [1; 16],
            abi: ABI_VERSION,
            equations: ["p", "n"]
                .into_iter()
                .map(|name| Equation {
                    name: name.into(),
                    kind: EquationKind::Electrical,
                    binding: Binding::Terminal,
                })
                .collect(),
            parameters: vec![
                Parameter {
                    name: "g".into(),
                    default: 1.,
                    changes_topology: false,
                },
                Parameter {
                    name: "topology".into(),
                    default: 0.,
                    changes_topology: true,
                },
            ],
            charge: Capability::Computed,
            noise: Capability::Computed,
        },
        CountingKernel(Arc::clone(counts)),
    )
}

fn bits(point: &Point<'_>) -> Vec<u64> {
    point
        .current
        .iter()
        .chain(point.conductance.iter())
        .chain(point.charge.iter())
        .chain(point.capacitance.iter())
        .map(|v| v.to_bits())
        .collect()
}

fn scalar(tile: &PreparedTile<CountingKernel>, lane: usize, x: &[f64]) -> Result<Vec<u64>> {
    let mut out = PointBuffer::new(2)?;
    tile.evaluate_lane(lane, x, &mut out.point())?;
    Ok(bits(&out.point()))
}

fn snapshot(tile: &PreparedTile<CountingKernel>, x: &[f64]) -> Result<Vec<Vec<u64>>> {
    (0..tile.lanes())
        .map(|lane| scalar(tile, lane, x))
        .collect()
}

fn update(lane: usize, values: &[f64], ambient_kelvin: f64) -> LaneUpdate<'_> {
    LaneUpdate {
        lane,
        values,
        ambient_kelvin,
    }
}

#[derive(Default)]
struct Density(Option<u64>);
impl Sink for Density {
    fn push(&mut self, source: Source<'_>) -> Result<()> {
        assert_eq!(source.name, "thermal");
        assert!(self.0.is_none());
        self.0 = Some(source.density.to_bits());
        Ok(())
    }
}

#[test]
fn one_changed_lane_prepares_once_and_preserves_all_other_bits() -> Result<()> {
    const LANES: usize = 257;
    const CHANGED: usize = 129;
    let counts = Arc::new(Counts::default());
    let artifact = artifact(&counts)?;
    let values: Vec<[f64; 2]> = (1..=257_u32).map(|i| [f64::from(i), 0.]).collect();
    let recipes: Vec<_> = values.iter().map(|v| (v.as_slice(), 300.)).collect();
    let mut tile = PreparedTile::new(&artifact, &recipes)?;
    assert_eq!(counts.preparations.load(Ordering::Relaxed), LANES);
    let x = [0.75, -0.25];
    let coords = x.repeat(LANES);
    let before = snapshot(&tile, &x)?;
    let mut out = BatchOutput::new(2, LANES)?;
    let storage = [
        out.current.as_ptr(),
        out.conductance.as_ptr(),
        out.charge.as_ptr(),
        out.capacitance.as_ptr(),
    ];
    tile.evaluate(&coords, &mut out)?;
    for (lane, expected) in before.iter().enumerate() {
        assert_eq!(bits(&out.point(lane)?), *expected);
    }
    // Temperature-only change. The conductance slot has the same bits.
    tile.stage_replacements(&artifact, &[update(CHANGED, &values[CHANGED], 450.)])?
        .commit();
    assert_eq!(counts.preparations.load(Ordering::Relaxed), LANES + 1);
    for _ in 0..3 {
        tile.evaluate(&coords, &mut out)?;
        for (lane, expected) in before.iter().enumerate() {
            let point = out.point(lane)?;
            let actual = bits(&point);
            assert_eq!(actual, scalar(&tile, lane, &x)?);
            if lane == CHANGED {
                assert_ne!(actual, *expected);
                assert_eq!(
                    point.current[0].to_bits(),
                    (values[lane][0] * 450. / 300.).to_bits()
                );
            } else {
                assert_eq!(actual, *expected);
            }
        }
        assert_eq!(
            [
                out.current.as_ptr(),
                out.conductance.as_ptr(),
                out.charge.as_ptr(),
                out.capacitance.as_ptr()
            ],
            storage
        );
    }
    let mut density = Density::default();
    tile.evaluate_lane_noise(CHANGED, &x, 1e3, &mut density)?;
    assert_eq!(density.0, Some((values[CHANGED][0] * 450. / 1e3).to_bits()));
    assert_eq!(counts.preparations.load(Ordering::Relaxed), LANES + 1);
    assert_eq!(counts.batches.load(Ordering::Relaxed), 4);
    Ok(())
}

#[test]
fn validation_and_abort_leave_every_lane_and_tile_unchanged() -> Result<()> {
    let counts = Arc::new(Counts::default());
    let artifact = artifact(&counts)?;
    let foreign = Artifact::new(
        artifact.descriptor().clone(),
        CountingKernel(Arc::clone(&counts)),
    )?;
    let mut first = PreparedTile::new(&artifact, &[(&[1., 0.], 300.), (&[2., 0.], 300.)])?;
    let mut second = PreparedTile::new(&artifact, &[(&[3., 0.], 300.)])?;
    let x = [1., 0.];
    let before = snapshot(&first, &x)?;
    let other = snapshot(&second, &x)?;
    let initial = counts.preparations.load(Ordering::Relaxed);
    let valid = update(0, &[4., 0.], 300.);
    assert!(matches!(
        first.stage_replacements(&foreign, &[valid]),
        Err(Error::ForeignArtifact)
    ));
    assert!(
        first
            .stage_replacements(&artifact, &[valid, valid])
            .is_err()
    );
    assert!(
        first
            .stage_replacements(&artifact, &[valid, update(2, &[1., 0.], 300.)])
            .is_err()
    );
    assert_eq!(counts.preparations.load(Ordering::Relaxed), initial);
    for invalid in [
        update(1, &[2.], 300.),
        update(1, &[f64::NAN, 0.], 300.),
        update(1, &[2., 1.], 300.),
        update(1, &[2., 0.], 0.),
        update(1, &[2., 0.], f64::INFINITY),
        update(1, &[-2., 0.], 300.),
    ] {
        assert!(
            first
                .stage_replacements(&artifact, &[invalid, valid])
                .is_err()
        );
        assert_eq!(snapshot(&first, &x)?, before);
    }
    // Staging the first group is successful but the second group fails.
    let staged = first.stage_replacements(&artifact, &[valid])?;
    assert!(
        second
            .stage_replacements(&artifact, &[update(0, &[-1., 0.], 300.)])
            .is_err()
    );
    drop(staged);
    assert_eq!(snapshot(&first, &x)?, before);
    assert_eq!(snapshot(&second, &x)?, other);
    let n = counts.preparations.load(Ordering::Relaxed);
    first.stage_replacements(&artifact, &[])?.commit();
    assert_eq!(counts.preparations.load(Ordering::Relaxed), n);
    let staged = first.stage_replacements(&artifact, &[valid])?;
    let staged_second = second.stage_replacements(&artifact, &[update(0, &[5., 0.], 300.)])?;
    staged.commit();
    staged_second.commit();
    assert_ne!(snapshot(&first, &x)?, before);
    assert_ne!(snapshot(&second, &x)?, other);
    Ok(())
}

#[test]
fn moved_prepared_instances_share_scalar_and_noise_without_repreparing() -> Result<()> {
    let counts = Arc::new(Counts::default());
    let artifact = artifact(&counts)?;
    let foreign = Artifact::new(
        artifact.descriptor().clone(),
        CountingKernel(Arc::clone(&counts)),
    )?;
    let x = [0.5, 0.];
    let instance = artifact.prepare(&[7., 0.], 350.)?;
    let mut buffer = PointBuffer::new(2)?;
    artifact.evaluate(&instance, &x, &mut buffer.point())?;
    let expected = bits(&buffer.point());
    let mut noise = Density::default();
    artifact.evaluate_noise(&instance, &x, 1e3, &mut noise)?;
    let tile = PreparedTile::from_prepared(&artifact, vec![instance])?;
    assert_eq!(counts.preparations.load(Ordering::Relaxed), 1);
    assert_eq!(scalar(&tile, 0, &x)?, expected);
    let mut same = Density::default();
    tile.evaluate_lane_noise(0, &x, 1e3, &mut same)?;
    assert_eq!(same.0, noise.0);
    assert!(tile.evaluate_lane(1, &x, &mut buffer.point()).is_err());
    assert!(
        tile.evaluate_lane(0, &[f64::NAN, 0.], &mut buffer.point())
            .is_err()
    );
    assert!(tile.evaluate_lane(0, &[0.], &mut buffer.point()).is_err());
    assert!(
        tile.evaluate_lane(0, &x, &mut PointBuffer::new(3)?.point())
            .is_err()
    );
    for frequency in [0., -1., f64::NAN, f64::INFINITY] {
        assert!(
            tile.evaluate_lane_noise(0, &x, frequency, &mut Density::default())
                .is_err()
        );
    }
    assert!(
        tile.evaluate_lane_noise(1, &x, 1e3, &mut Density::default())
            .is_err()
    );
    assert_eq!(counts.preparations.load(Ordering::Relaxed), 1);
    let invalid = foreign.prepare(&[7., 0.], 350.)?;
    assert!(matches!(
        PreparedTile::from_prepared(&artifact, vec![invalid]),
        Err(Error::ForeignArtifact)
    ));
    assert!(PreparedTile::from_prepared(&artifact, Vec::new()).is_err());
    Ok(())
}
