//! Verify conservative generalized equations, topology and source placement.
use model_kernel_api::{
    Binding, Error, PointBuffer,
    noise::{Sink, Source},
};
use model_kernel_diode::{Instance, Model, artifact::artifact, compile_simetrix};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn close(actual: f64, expected: f64, absolute: f64, relative: f64) -> Result<()> {
    if (actual - expected).abs() <= relative.mul_add(expected.abs(), absolute) {
        Ok(())
    } else {
        Err(format!("{actual:e} != {expected:e}").into())
    }
}

#[derive(Default)]
struct Noise(Vec<(String, [usize; 2], f64)>);

impl Sink for Noise {
    fn push(&mut self, source: Source<'_>) -> model_kernel_api::Result<()> {
        let [positive, negative] = source.injections else {
            return Err(Error::Shape("two terminal noise source"));
        };
        assert_eq!(positive.re.to_bits(), 1_f64.to_bits());
        assert_eq!(negative.re.to_bits(), (-1_f64).to_bits());
        self.0.push((
            source.name.into(),
            [positive.row, negative.row],
            source.density,
        ));
        Ok(())
    }
}

#[test]
fn nodal_residual_storage_and_jacobians() -> Result<()> {
    for resistance in [-3., 0., 3.] {
        let model = compile_simetrix([
            ("RS", resistance),
            ("IS", 1e-12),
            ("CJO", 2e-9),
            ("TT", 5e-9),
            ("KF", 2e-14),
        ])?;
        let instance = Instance {
            area: 2.,
            multiplier: 3.,
            ..Instance::default()
        };
        let local = model.bind(instance)?;
        let compiled = artifact(model, instance, [1; 16])?;
        let candidate = compiled.prepare(&[2., 3., 1e-12, 0.], 300.15)?;
        let width = if resistance == 0. { 2 } else { 3 };
        assert_eq!(compiled.descriptor().equations.len(), width);
        if width == 3 {
            assert_eq!(compiled.descriptor().equations[2].binding, Binding::Hidden);
        }
        let mut x = [0.6, 0., 0.52][..width].to_vec();
        let anode = if width == 3 { 2 } else { 0 };
        let expected = local.evaluate(x[anode] - x[1])?;
        let mut buffer = PointBuffer::new(width)?;
        compiled.evaluate(&candidate, &x, &mut buffer.point())?;
        let point = buffer.point();
        close(point.current[1], -expected.current, 1e-15, 1e-12)?;
        close(point.charge[anode], expected.charge, 1e-24, 1e-12)?;
        if width == 3 {
            close(
                point.current[0],
                (x[0] - x[2]) / local.series_resistance(),
                1e-15,
                1e-12,
            )?;
        }
        let jacobian = point.conductance.to_vec();
        let storage = point.capacitance.to_vec();
        let step = 1e-6;
        for col in 0..width {
            let original = x[col];
            x[col] = original + step;
            compiled.evaluate(&candidate, &x, &mut buffer.point())?;
            let upper_i = buffer.point().current.to_vec();
            let upper_q = buffer.point().charge.to_vec();
            x[col] = original - step;
            compiled.evaluate(&candidate, &x, &mut buffer.point())?;
            let lower = buffer.point();
            x[col] = original;
            for row in 0..width {
                close(
                    (upper_i[row] - lower.current[row]) / (2. * step),
                    jacobian[row * width + col],
                    1e-10,
                    1e-7,
                )?;
                close(
                    (upper_q[row] - lower.charge[row]) / (2. * step),
                    storage[row * width + col],
                    1e-17,
                    1e-7,
                )?;
            }
        }
        let mut noise = Noise::default();
        let result = compiled.evaluate_noise(&candidate, &x, 1e3, &mut noise);
        if resistance < 0. {
            assert_eq!(result, Err(Error::Unsupported("negative RS noise")));
            assert!(noise.0.is_empty());
        } else {
            result?;
            let expected_noise = local.noise_density(expected.current, 1e3)?;
            assert_eq!(noise.0.len(), width);
            assert_eq!(noise.0[0].1, [anode, 1]);
            assert_eq!(noise.0[1].1, [anode, 1]);
            close(noise.0[0].2, expected_noise.shot, 0., 1e-12)?;
            close(noise.0[1].2, expected_noise.flicker, 0., 1e-12)?;
            if width == 3 {
                assert_eq!(noise.0[2].1, [0, anode]);
                close(noise.0[2].2, expected_noise.series_thermal, 0., 1e-12)?;
            }
        }
    }
    Ok(())
}

#[test]
fn candidate_temperature_policy_and_topology() -> Result<()> {
    let mut model = Model::default();
    model.series.resistance = 2.;
    model.series.linear_temperature = -0.125;
    let model = model.prepare()?;
    let reference = Instance {
        absolute_temperature: Some(300.15),
        ..Instance::default()
    };
    let compiled = artifact(model, reference, [2; 16])?;
    let regular = compiled.prepare(&[1., 1., 1e-12, 0., 304.15], 340.)?;
    assert!(matches!(
        compiled.prepare(&[1., 1., 1e-12, 0., 308.15], 340.),
        Err(Error::Unsupported(
            "diode series topology requires new artifact"
        ))
    ));
    let zero = Instance {
        absolute_temperature: Some(308.15),
        ..reference
    };
    let short = artifact(model, zero, [3; 16])?;
    assert_eq!(short.descriptor().equations.len(), 2);
    assert!(short.prepare(&[1., 1., 1e-12, 0., 304.15], 340.).is_err());
    let mut buffer = PointBuffer::new(3)?;
    compiled.evaluate(&regular, &[0.4, 0., 0.3], &mut buffer.point())?;
    close(buffer.point().current[0], 0.1, 1e-15, 1e-12)?;
    Ok(())
}
