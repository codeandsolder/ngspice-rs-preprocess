//! Test the shared generalized equation representation, not a second device law.

use std::sync::Arc;

use model_kernel_api::{
    Binding, EquationKind, PointBuffer, Result,
    batch::{BatchOutput, PreparedTile},
    noise::{Sink, Source},
};
use model_kernel_vdmos::{Bias, Instance, Model, Polarity, QuasiSaturation, artifact::artifact};

fn close(actual: f64, expected: f64, atol: f64, rtol: f64) {
    assert!(
        (actual - expected).abs() <= rtol.mul_add(actual.abs().max(expected.abs()), atol),
        "actual={actual:e}, expected={expected:e}"
    );
}

fn model(polarity: Polarity) -> Model {
    let mut model = Model::defaults(polarity);
    model.channel.lambda = 0.01;
    model.channel.tcvth = -0.003 * polarity.sign();
    model.series.rd = 0.04;
    model.series.rs = 0.02;
    model.series.rg = 2.;
    model.series.rb = 0.015;
    model.series.trd1 = 0.003;
    model.series.quasi_saturation = Some(QuasiSaturation { rq: 0.02, vq: 2. });
    model.diode.tt = 80e-9;
    model.diode.vp = 0.2;
    model.diode.ttt1 = 0.001;
    model.noise.kf = 1e-25;
    model
}

#[test]
fn generalized_jacobian_matches_numerical_derivatives()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    for polarity in [Polarity::N, Polarity::P] {
        for thermal in [false, true] {
            let model = Arc::new(model(polarity).prepare()?);
            let artifact = artifact(Arc::clone(&model), thermal, [7; 16])?;
            let prepared = artifact.prepare(&[1.5], 310.)?;
            let descriptor = artifact.descriptor();
            let width = descriptor.equations.len();
            let sign = polarity.sign();
            let coordinates: Vec<_> = descriptor
                .equations
                .iter()
                .map(|row| match row.name.as_str() {
                    "drain" => 2. * sign,
                    "gate" => 5.1 * sign,
                    "source" => 0.,
                    "drain_prime" => 1.9 * sign,
                    "gate_prime" => 5. * sign,
                    "source_prime" => 0.1 * sign,
                    "body_prime" => 0.02 * sign,
                    "junction" => 340.,
                    "case" => 320.,
                    "recovery" => 1e-8,
                    name => panic!("unexpected row: {name}"),
                })
                .collect();
            assert_eq!(
                descriptor
                    .equations
                    .iter()
                    .filter(|e| e.binding == Binding::Terminal)
                    .count(),
                if thermal { 5 } else { 3 }
            );
            let mut nominal = PointBuffer::new(width)?;
            artifact.evaluate(&prepared, &coordinates, &mut nominal.point())?;
            let mut perturbed = coordinates.clone();
            let mut plus = PointBuffer::new(width)?;
            let mut minus = PointBuffer::new(width)?;
            for (column, equation) in descriptor.equations.iter().enumerate() {
                let step = match equation.kind {
                    EquationKind::Electrical => 1e-6,
                    EquationKind::Thermal => 1e-3,
                    EquationKind::Internal => 1e-11,
                };
                perturbed[column] = coordinates[column] + step;
                artifact.evaluate(&prepared, &perturbed, &mut plus.point())?;
                perturbed[column] = coordinates[column] - step;
                artifact.evaluate(&prepared, &perturbed, &mut minus.point())?;
                perturbed[column] = coordinates[column];
                let center = nominal.point();
                let pos = plus.point();
                let neg = minus.point();
                for row in 0..width {
                    let numeric_f = (pos.current[row] - neg.current[row]) / (2. * step);
                    let numeric_q = (pos.charge[row] - neg.charge[row]) / (2. * step);
                    // Use a rounding bound based on the actual differenced
                    // values, not a fixed relaxed Jacobian tolerance.
                    let f_round =
                        16. * f64::EPSILON * (pos.current[row].abs() + neg.current[row].abs())
                            / step;
                    let q_round =
                        16. * f64::EPSILON * (pos.charge[row].abs() + neg.charge[row].abs()) / step;
                    close(
                        center.conductance[row * width + column],
                        numeric_f,
                        f_round + 1e-9,
                        2e-5,
                    );
                    close(
                        center.capacitance[row * width + column],
                        numeric_q,
                        q_round + 1e-16,
                        2e-5,
                    );
                }
            }
            // Independent gauge shift of every electrical potential.
            for (value, equation) in perturbed.iter_mut().zip(&descriptor.equations) {
                if equation.kind == EquationKind::Electrical {
                    *value += 0.75;
                }
            }
            artifact.evaluate(&prepared, &perturbed, &mut plus.point())?;
            for (actual, expected) in plus
                .point()
                .current
                .iter()
                .zip(nominal.point().current.iter())
            {
                close(*actual, *expected, 1e-12, 1e-12);
            }
        }
    }
    Ok(())
}

#[test]
fn recovery_row_transformation_preserves_physical_current()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    for polarity in [Polarity::N, Polarity::P] {
        let mut m = Model::defaults(polarity);
        m.diode.tt = 100e-9;
        m.diode.vp = 0.3;
        m.diode.ttt1 = 0.002;
        let model = Arc::new(m.prepare()?);
        let artifact = artifact(Arc::clone(&model), true, [9; 16])?;
        let instance = artifact.prepare(&[2.], 300.)?;
        let width = artifact.descriptor().equations.len();
        let idx = |name: &str| {
            artifact
                .descriptor()
                .equations
                .iter()
                .position(|eq| eq.name == name)
                .unwrap()
        };
        let mut x = vec![0.; width];
        let sign = polarity.sign();
        x[idx("drain")] = -0.55 * sign;
        x[idx("gate")] = -1. * sign;
        x[idx("junction")] = 330.;
        x[idx("case")] = 310.;
        x[idx("recovery")] = 2e-9;
        let raw = model
            .bind(Instance {
                multiplier: 2.,
                self_heating: true,
            })?
            .evaluate(Bias {
                vgs: -sign,
                vds: -0.55 * sign,
                vds_external: -0.55 * sign,
                vdiode: 0.55 * sign,
                recovery_node: 2e-9,
                temperature: 330.,
            })?;
        let recovery = raw.diode.recovery.unwrap();
        let mut out = PointBuffer::new(width)?;
        artifact.evaluate(&instance, &x, &mut out.point())?;
        let point = out.point();
        let expected_source = -raw.channel.value + raw.diode.current.value
            - recovery.diode_charge_rate_gain * recovery.residual
            - raw.series.shunt * x[idx("drain")];
        close(point.current[idx("source")], expected_source, 1e-14, 1e-12);
        close(
            point.current[idx("recovery")],
            recovery.residual,
            1e-18,
            1e-12,
        );
        close(
            point.charge[idx("recovery")],
            recovery.stored_charge.value,
            1e-24,
            1e-12,
        );
        // At a converged recovery equation dQs/dt=-Fq. The row
        // transformation is exact even when the gain varies with temperature.
        close(
            point.current[idx("source")] + raw.channel.value + raw.series.shunt * x[idx("drain")],
            raw.diode.current.value + recovery.diode_charge_rate_gain * (-recovery.residual),
            1e-14,
            1e-12,
        );
    }
    Ok(())
}

#[derive(Default)]
struct Collect(Vec<(String, f64)>);
impl Sink for Collect {
    fn push(&mut self, source: Source<'_>) -> Result<()> {
        assert_eq!(source.injections.len(), 2);
        self.0.push((source.name.into(), source.density));
        Ok(())
    }
}

#[test]
fn artifact_tiles_and_noise_preserve_native_model_laws()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let model = Arc::new(Model::defaults(Polarity::N).prepare()?);
    let artifact = artifact(Arc::clone(&model), false, [5; 16])?;
    let one = artifact.prepare(&[1.], 300.)?;
    let two = artifact.prepare(&[2.], 300.)?;
    let tile = PreparedTile::new(&artifact, &[(&[1.], 300.), (&[2.], 300.)])?;
    let width = artifact.descriptor().equations.len();
    assert_eq!(width, 3);
    let x = [2., 5., 0.];
    let mut output = BatchOutput::new(width, 2)?;
    tile.evaluate(&[2., 5., 0., 2., 5., 0.], &mut output)?;
    let mut point = PointBuffer::new(width)?;
    for (lane, prepared) in [&one, &two].into_iter().enumerate() {
        artifact.evaluate(prepared, &x, &mut point.point())?;
        assert_eq!(output.point(lane)?.current, point.point().current);
        assert_eq!(output.point(lane)?.capacitance, point.point().capacitance);
    }
    let mut noise = Collect::default();
    artifact.evaluate_noise(&one, &x, 1e3, &mut noise)?;
    let device = model.bind(Instance::default())?;
    let raw = device.evaluate(Bias {
        vgs: 5.,
        vds: 2.,
        vds_external: 2.,
        vdiode: -2.,
        temperature: 300.,
        ..Bias::default()
    })?;
    let densities = device.noise_density(&raw, 1e3)?;
    assert_eq!(
        noise.0,
        vec![
            ("channel".into(), densities.channel),
            ("flicker".into(), densities.flicker)
        ]
    );
    Ok(())
}

#[test]
fn changing_temperature_backward_euler_recovery_is_equivalent()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    for polarity in [Polarity::N, Polarity::P] {
        let sign = polarity.sign();
        let mut model = Model::defaults(polarity);
        model.diode.tt = 100e-9;
        model.diode.vp = 0.4;
        model.diode.ttt1 = 0.005;
        let native = Arc::new(model.prepare()?);
        let kernel = artifact(Arc::clone(&native), true, [2; 16])?;
        let instance = kernel.prepare(&[1.], 300.)?;
        let device = native.bind(Instance {
            multiplier: 1.,
            self_heating: true,
        })?;
        let bias = |temperature, recovery_node, vdiode| Bias {
            vgs: -sign,
            vds: -vdiode,
            vds_external: -vdiode,
            vdiode,
            temperature,
            recovery_node,
        };
        let old = device
            .evaluate(bias(300., 2e-9, 0.6 * sign))?
            .diode
            .recovery
            .unwrap();
        let next_zero = device
            .evaluate(bias(350., 0., 0.5 * sign))?
            .diode
            .recovery
            .unwrap();
        let step = 20e-9;
        // Independently solve the affine backward-Euler recovery equation:
        // Fq(0)+qp/VP+(TT_new*qp-Qs_old)/h=0.
        let qp = (old.stored_charge.value / step - next_zero.residual)
            / (next_zero.d_node + next_zero.stored_charge.d_voltage / step);
        let next = device.evaluate(bias(350., qp, 0.5 * sign))?;
        let recovery = next.diode.recovery.unwrap();
        let rate = (recovery.stored_charge.value - old.stored_charge.value) / step;
        close(recovery.residual + rate, 0., 1e-18, 1e-12);
        let coords: Vec<_> = kernel
            .descriptor()
            .equations
            .iter()
            .map(|row| match row.name.as_str() {
                "drain" => -0.5 * sign,
                "gate" => -sign,
                "source" => 0.,
                "junction" => 350.,
                "case" => 315.,
                "recovery" => qp,
                name => panic!("unexpected row: {name}"),
            })
            .collect();
        let mut out = PointBuffer::new(coords.len())?;
        kernel.evaluate(&instance, &coords, &mut out.point())?;
        let source = out.point().current[2];
        let expected = next.diode.current.value + recovery.diode_charge_rate_gain * rate
            - next.channel.value
            + next.series.shunt * 0.5 * sign;
        close(source, expected, 1e-14, 1e-12);
    }
    Ok(())
}
