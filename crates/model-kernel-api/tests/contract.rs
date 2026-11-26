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

#[derive(Debug)]
struct CountedLinear(std::sync::Arc<std::sync::atomic::AtomicUsize>);

impl Kernel for CountedLinear {
    type Prepared = f64;

    fn prepare(&self, values: &[f64], ambient: f64) -> Result<f64> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if values[0] <= 0. {
            return Err(Error::Domain("nonpositive conductance".into()));
        }
        Ok(values[0] * ambient / 300.)
    }

    fn evaluate(&self, data: &f64, x: &[f64], out: &mut Point<'_>) -> Result<()> {
        Linear.evaluate(data, x, out)
    }

    fn evaluate_noise(
        &self,
        data: &f64,
        x: &[f64],
        frequency: f64,
        sink: &mut dyn Sink,
    ) -> Result<()> {
        Linear.evaluate_noise(data, x, frequency, sink)
    }
}

#[derive(Default)]
struct Density(Option<f64>);

impl Sink for Density {
    fn push(&mut self, source: Source<'_>) -> Result<()> {
        self.0 = Some(source.density);
        Ok(())
    }
}

#[test]
fn selective_replacement_is_transactional_and_shares_scalar_and_noise() -> Result<()> {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    const LANES: usize = 128;
    let count = Arc::new(AtomicUsize::new(0));
    let mut d = descriptor();
    d.parameters.push(Parameter {
        name: "topology".into(),
        default: 1.,
        changes_topology: true,
    });
    let model = Artifact::new(d.clone(), CountedLinear(Arc::clone(&count)))?;
    let foreign = Artifact::new(d, CountedLinear(Arc::clone(&count)))?;
    let values = [2., 1.];
    let candidates = [(values.as_slice(), 300.); LANES];
    let mut tile = PreparedTile::new(&model, &candidates)?;
    assert_eq!(count.load(Ordering::Relaxed), LANES);
    let x = [2., 1., 310.];
    let coordinates = x.repeat(LANES);
    let mut batch = BatchOutput::new(3, LANES)?;
    tile.evaluate(&coordinates, &mut batch)?;
    let original = batch.current.clone();
    let mut scalar = PointBuffer::new(3)?;

    tile.replace(&model, &[(37, &[3., 1.], 300.)])?;
    assert_eq!(count.load(Ordering::Relaxed), LANES + 1);
    tile.evaluate(&coordinates, &mut batch)?;
    for lane in 0..LANES {
        let point = batch.point(lane)?;
        let expected = if lane == 37 { 3. } else { original[lane * 3] };
        assert_eq!(point.current[0].to_bits(), expected.to_bits());
        tile.evaluate_lane(lane, &x, &mut scalar.point())?;
        let single = scalar.point();
        for (left, right) in [
            (&*point.current, &*single.current),
            (&*point.conductance, &*single.conductance),
            (&*point.charge, &*single.charge),
            (&*point.capacitance, &*single.capacitance),
        ] {
            assert!(
                left.iter()
                    .zip(right)
                    .all(|(a, b)| a.to_bits() == b.to_bits())
            );
        }
        let mut noise = Density::default();
        tile.evaluate_lane_noise(lane, &x, 1e3, &mut noise)?;
        assert_eq!(noise.0.map(f64::to_bits), Some(expected.to_bits()));
    }
    assert_eq!(count.load(Ordering::Relaxed), LANES + 1);
    let before = batch.current.clone();

    // A valid preparation staged before a failing one must not be published.
    assert!(
        tile.replace(&model, &[(0, &[4., 1.], 300.), (1, &[-1., 1.], 300.)])
            .is_err()
    );
    tile.evaluate(&coordinates, &mut batch)?;
    assert_eq!(batch.current, before);
    assert_eq!(count.load(Ordering::Relaxed), LANES + 3);
    // Invalid indices and duplicate ordinals are rejected before preparation.
    assert!(
        tile.replace(&model, &[(0, &values, 300.), (LANES, &values, 300.)])
            .is_err()
    );
    assert!(
        tile.replace(&model, &[(0, &values, 300.), (0, &values, 310.)])
            .is_err()
    );
    assert_eq!(
        tile.replace(&foreign, &[(0, &values, 300.)]),
        Err(Error::ForeignArtifact)
    );
    assert!(tile.replace(&model, &[(0, &[2., 2.], 300.)]).is_err());
    assert!(tile.replace(&model, &[(0, &values, 0.)]).is_err());
    assert!(tile.replace(&model, &[(0, &[2.], 300.)]).is_err());
    assert!(tile.replace(&model, &[(0, &[f64::NAN, 1.], 300.)]).is_err());
    tile.replace(&model, &[])?;
    assert_eq!(count.load(Ordering::Relaxed), LANES + 3);
    tile.evaluate(&coordinates, &mut batch)?;
    assert_eq!(batch.current, before);

    tile.replace(&model, &[(37, &[3., 1.], 400.)])?;
    assert_eq!(count.load(Ordering::Relaxed), LANES + 4);
    tile.evaluate_lane(37, &x, &mut scalar.point())?;
    assert_eq!(scalar.point().current[0].to_bits(), 4_f64.to_bits());
    assert!(tile.evaluate_lane(LANES, &x, &mut scalar.point()).is_err());
    assert!(
        tile.evaluate_lane(0, &[1., 0.], &mut scalar.point())
            .is_err()
    );
    assert!(
        tile.evaluate_lane_noise(0, &x, 0., &mut Density::default())
            .is_err()
    );
    assert!(
        tile.evaluate_lane_noise(LANES, &x, 1e3, &mut Density::default())
            .is_err()
    );
    Ok(())
}
