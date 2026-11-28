use model_kernel_api::{
    Capability, PointBuffer, Result,
    noise::{Sink, Source},
};
use model_kernel_ekv::{
    Geometry, Model, Polarity,
    artifact::{Noise, artifact},
};

const GEOMETRY: Geometry = Geometry {
    length: 1e-6,
    width: 10e-6,
    parallel: 1.,
    series: 1.,
};
const SLOTS: [f64; 4] = [1e-6, 10e-6, 1., 1.];

#[derive(Default)]
struct Collector {
    density: f64,
    count: usize,
}

impl Sink for Collector {
    fn push(&mut self, source: Source<'_>) -> Result<()> {
        assert_eq!(source.injections.len(), 2);
        assert_eq!(source.injections[0].row, 0);
        assert_eq!(source.injections[1].row, 2);
        self.density += source.density;
        self.count += 1;
        Ok(())
    }
}

#[test]
fn intrinsic_contract() -> Result<()> {
    for polarity in [Polarity::N, Polarity::P] {
        let kernel = artifact(Model::reference_defaults(polarity), GEOMETRY, None, [1; 16])?;
        assert_eq!(kernel.descriptor().charge, Capability::Computed);
        assert_eq!(kernel.descriptor().noise, Capability::Zero);
        let prepared = kernel.prepare(&SLOTS, 300.15)?;
        let sign = if polarity == Polarity::N { 1. } else { -1. };
        let mut buffer = PointBuffer::new(4)?;
        for drain in [-1., -0.1, 0., 0.1, 1., 3.] {
            for gate in [-0.5, 0.1, 0.5, 1., 2.] {
                kernel.evaluate(
                    &prepared,
                    &[drain * sign, gate * sign, 0., -0.2 * sign],
                    &mut buffer.point(),
                )?;
            }
        }
        let mut sink = Collector::default();
        kernel.evaluate_noise(&prepared, &[sign, 0.75 * sign, 0., 0.], 1., &mut sink)?;
        assert_eq!(sink.count, 0);
    }
    Ok(())
}

#[test]
fn reference_noise() -> std::result::Result<(), Box<dyn std::error::Error>> {
    for (coefficient, exponent, fixture) in [
        (0., 1., include_str!("fixtures/reference-noise.tsv")),
        (1e-25, 1.2, include_str!("fixtures/reference-flicker.tsv")),
    ] {
        let kernel = artifact(
            Model::reference_defaults(Polarity::N),
            GEOMETRY,
            Some(Noise {
                flicker_coefficient: coefficient,
                flicker_frequency_exponent: exponent,
            }),
            [2; 16],
        )?;
        let prepared = kernel.prepare(&SLOTS, 300.15)?;
        for line in fixture.lines().skip(1) {
            let row: Vec<f64> = line
                .split_whitespace()
                .map(str::parse)
                .collect::<std::result::Result<_, _>>()?;
            let mut sink = Collector::default();
            kernel.evaluate_noise(&prepared, &[1., 0.75, 0., 0.], row[0], &mut sink)?;
            assert_eq!(sink.count, 2);
            let actual = sink.density.sqrt();
            let expected = row[1];
            assert!(
                (actual - expected).abs() <= 2e-7 * expected,
                "noise {actual:.12e} != {expected:.12e}, frequency {}",
                row[0]
            );
        }
    }
    Ok(())
}
