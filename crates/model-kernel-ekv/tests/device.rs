use model_kernel_api::{
    Binding, Capability, PointBuffer, Result,
    noise::{Sink, Source},
};
use model_kernel_ekv::{
    Geometry, Model, Polarity,
    device::{Device, ResistanceTemperature},
    junction::{Depletion, Junction},
};

const GEOMETRY: Geometry = Geometry {
    length: 1e-6,
    width: 4e-6,
    parallel: 2.,
    series: 3.,
};
const SLOTS: [f64; 4] = [1e-6, 4e-6, 2., 3.];

fn device(polarity: Polarity) -> Device {
    let mut channel = Model::reference_defaults(polarity);
    channel.nominal_temperature = 300.15;
    let junction = |saturation, bottom, sidewall| Junction {
        saturation_current: saturation,
        emission: 1.5,
        bottom: Depletion {
            capacitance: bottom,
            potential: 0.8,
            grading: 0.5,
        },
        sidewall: Depletion {
            capacitance: sidewall,
            potential: 0.7,
            grading: 0.33,
        },
        transit_time: 3e-9,
        ..Junction::default()
    };
    Device {
        channel,
        junctions: [junction(2e-13, 3e-14, 5e-15), junction(3e-13, 4e-14, 1e-14)],
        overlap: [1e-10, 2e-10, 3e-10],
        resistance: [17., 29., 13., 43.],
        resistance_temperature: [ResistanceTemperature::default(); 4],
        shunt: 2e-6,
        noise: None,
    }
}

#[test]
fn independent_device_matrices() -> std::result::Result<(), Box<dyn std::error::Error>> {
    for line in include_str!("fixtures/reference-device.tsv")
        .lines()
        .skip(1)
    {
        let row: Vec<&str> = line.split_whitespace().collect();
        let val = |i: usize| row[i].parse::<f64>();
        let polarity = if row[0] == "1" {
            Polarity::N
        } else {
            Polarity::P
        };
        let column = row[1].parse::<usize>()?;
        let mut voltage = [0.; 8];
        for (i, dst) in voltage.iter_mut().enumerate() {
            *dst = val(2 + i)?;
        }
        let artifact = device(polarity).artifact(GEOMETRY, [3; 16])?;
        let prepared = artifact.prepare(&SLOTS, 300.15)?;
        let mut buffer = PointBuffer::new(8)?;
        let mut point = buffer.point();
        artifact.evaluate(&prepared, &voltage, &mut point)?;
        for r in 0..8 {
            let expected = val(10 + r)?;
            assert!(
                (point.current[r] - expected).abs() <= expected.abs().mul_add(3e-6, 2e-14),
                "I {r}: {:.12e} != {expected:.12e}, {line}",
                point.current[r]
            );
            let expected = val(18 + 2 * r)?;
            let actual = point.conductance[r * 8 + column];
            assert!(
                (actual - expected).abs() <= expected.abs().mul_add(4e-6, 2e-12),
                "G {r} {column}: {actual:.12e} != {expected:.12e}, {line}"
            );
            let expected = val(19 + 2 * r)?;
            let actual = point.capacitance[r * 8 + column];
            assert!(
                (actual - expected).abs() <= expected.abs().mul_add(4e-6, 2e-24),
                "C {r} {column}: {actual:.12e} != {expected:.12e}, {line}"
            );
        }
    }
    Ok(())
}

#[derive(Default)]
struct NoiseCollector {
    sources: Vec<(String, f64, [usize; 2])>,
}

impl Sink for NoiseCollector {
    fn push(&mut self, source: Source<'_>) -> Result<()> {
        self.sources.push((
            source.name.into(),
            source.density,
            [source.injections[0].row, source.injections[1].row],
        ));
        Ok(())
    }
}

#[test]
fn structural_nodes_and_noise() -> Result<()> {
    for mask in 0_u8..16 {
        let mut device = device(Polarity::N);
        for (i, value) in device.resistance.iter_mut().enumerate() {
            if mask & (1 << i) == 0 {
                *value = 0.;
            }
        }
        let artifact = device.artifact(GEOMETRY, [4; 16])?;
        let desc = artifact.descriptor();
        let width = 4 + mask.count_ones() as usize;
        assert_eq!(desc.equations.len(), width);
        assert!(
            desc.equations[..4]
                .iter()
                .all(|e| e.binding == Binding::Terminal)
        );
        assert!(
            desc.equations[4..]
                .iter()
                .all(|e| e.binding == Binding::Hidden)
        );
        assert_eq!(desc.noise, Capability::Computed);
        let prepared = artifact.prepare(&SLOTS, 300.15)?;
        let mut voltage = vec![0.; width];
        voltage[0] = 1.;
        voltage[1] = 0.7;
        let mut buffer = PointBuffer::new(width)?;
        artifact.evaluate(&prepared, &voltage, &mut buffer.point())?;
        let mut sink = NoiseCollector::default();
        artifact.evaluate_noise(&prepared, &voltage, 1000., &mut sink)?;
        assert_eq!(sink.sources.len(), mask.count_ones() as usize + 1);
        for (name, density, ends) in &sink.sources {
            assert!(ends[0] < width && ends[1] < width && ends[0] != ends[1]);
            assert!(*density > 0. && density.is_finite(), "{name}");
        }
    }
    Ok(())
}
