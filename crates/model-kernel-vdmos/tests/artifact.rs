//! Test the shared generalized equation representation, not a second device law.

use std::sync::Arc;

use model_kernel_api::{
    Binding, EquationKind, Error, PointBuffer, Result,
    batch::{BatchOutput, LaneUpdate, PreparedTile},
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
                .map(|row| {
                    Ok(match row.name.as_str() {
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
                        name => return Err(Error::Domain(format!("unexpected row: {name}"))),
                    })
                })
                .collect::<Result<Vec<_>>>()?;
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
                .ok_or(Error::Shape("missing test coordinate"))
        };
        let mut x = vec![0.; width];
        let sign = polarity.sign();
        x[idx("drain")?] = -0.55 * sign;
        x[idx("gate")?] = -sign;
        x[idx("junction")?] = 330.;
        x[idx("case")?] = 310.;
        x[idx("recovery")?] = 2e-9;
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
        let recovery = raw.diode.recovery.ok_or(Error::Shape("missing recovery"))?;
        let mut out = PointBuffer::new(width)?;
        artifact.evaluate(&instance, &x, &mut out.point())?;
        let point = out.point();
        let expected_source = raw.series.shunt.mul_add(
            -x[idx("drain")?],
            recovery.diode_charge_rate_gain.mul_add(
                -recovery.residual,
                raw.diode.current.value - raw.channel.value,
            ),
        );
        close(point.current[idx("source")?], expected_source, 1e-14, 1e-12);
        close(
            point.current[idx("recovery")?],
            recovery.residual,
            1e-18,
            1e-12,
        );
        close(
            point.charge[idx("recovery")?],
            recovery.stored_charge.value,
            1e-24,
            1e-12,
        );
        // At a converged recovery equation dQs/dt=-Fq. The row
        // transformation is exact even when the gain varies with temperature.
        close(
            raw.series.shunt.mul_add(
                x[idx("drain")?],
                point.current[idx("source")?] + raw.channel.value,
            ),
            recovery
                .diode_charge_rate_gain
                .mul_add(-recovery.residual, raw.diode.current.value),
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
fn selective_lanes_reuse_actual_vdmos_physics_and_noise()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    for polarity in [Polarity::N, Polarity::P] {
        let sign = polarity.sign();
        let artifact = artifact(
            Arc::new(Model::defaults(polarity).prepare()?),
            false,
            [9; 16],
        )?;
        let x = [2. * sign, 5. * sign, 0.];
        let coords = x.repeat(3);
        let unchanged = artifact.prepare(&[1.], 300.)?;
        let changed = artifact.prepare(&[2.], 310.)?;
        let expected = artifact.prepare(&[3.], 375.)?;
        let mut out = BatchOutput::new(3, 3)?;
        let mut point = PointBuffer::new(3)?;
        let mut tile = PreparedTile::from_prepared(
            &artifact,
            vec![
                artifact.prepare(&[1.], 300.)?,
                changed,
                artifact.prepare(&[1.], 300.)?,
            ],
        )?;
        tile.stage_replacements(
            &artifact,
            &[LaneUpdate {
                lane: 1,
                values: &[3.],
                ambient_kelvin: 375.,
            }],
        )?
        .commit();
        tile.evaluate(&coords, &mut out)?;
        for (lane, reference) in [&unchanged, &expected, &unchanged].into_iter().enumerate() {
            artifact.evaluate(reference, &x, &mut point.point())?;
            let scalar = point.point();
            let batch = out.point(lane)?;
            assert_eq!(scalar.current, batch.current);
            assert_eq!(scalar.conductance, batch.conductance);
            assert_eq!(scalar.charge, batch.charge);
            assert_eq!(scalar.capacitance, batch.capacitance);
            let mut other = PointBuffer::new(3)?;
            tile.evaluate_lane(lane, &x, &mut other.point())?;
            let other = other.point();
            assert_eq!(scalar.current, other.current);
            assert_eq!(scalar.conductance, other.conductance);
            assert_eq!(scalar.charge, other.charge);
            assert_eq!(scalar.capacitance, other.capacitance);
            let mut expected_noise = Collect::default();
            artifact.evaluate_noise(reference, &x, 1e3, &mut expected_noise)?;
            let mut lane_noise = Collect::default();
            tile.evaluate_lane_noise(lane, &x, 1e3, &mut lane_noise)?;
            assert_eq!(expected_noise.0, lane_noise.0);
        }
    }
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
            .ok_or(Error::Shape("missing recovery"))?;
        let next_zero = device
            .evaluate(bias(350., 0., 0.5 * sign))?
            .diode
            .recovery
            .ok_or(Error::Shape("missing recovery"))?;
        let step = 20e-9;
        // Independently solve the affine backward-Euler recovery equation:
        // Fq(0)+qp/VP+(TT_new*qp-Qs_old)/h=0.
        let qp = (old.stored_charge.value / step - next_zero.residual)
            / (next_zero.d_node + next_zero.stored_charge.d_voltage / step);
        let next = device.evaluate(bias(350., qp, 0.5 * sign))?;
        let recovery = next
            .diode
            .recovery
            .ok_or(Error::Shape("missing recovery"))?;
        let rate = (recovery.stored_charge.value - old.stored_charge.value) / step;
        close(recovery.residual + rate, 0., 1e-18, 1e-12);
        let coords: Vec<_> = kernel
            .descriptor()
            .equations
            .iter()
            .map(|row| {
                Ok(match row.name.as_str() {
                    "drain" => -0.5 * sign,
                    "gate" => -sign,
                    "source" => 0.,
                    "junction" => 350.,
                    "case" => 315.,
                    "recovery" => qp,
                    name => return Err(Error::Domain(format!("unexpected row: {name}"))),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let mut out = PointBuffer::new(coords.len())?;
        kernel.evaluate(&instance, &coords, &mut out.point())?;
        let source = out.point().current[2];
        let expected = (next.series.shunt * 0.5).mul_add(
            sign,
            recovery
                .diode_charge_rate_gain
                .mul_add(rate, next.diode.current.value)
                - next.channel.value,
        );
        close(source, expected, 1e-14, 1e-12);
    }
    Ok(())
}

#[test]
fn physical_taylor_reconstructs_electrothermal_laws()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    use model_kernel_api::taylor::{TaylorBuffer, TaylorInput};
    const ORDER: usize = 10;
    for polarity in [Polarity::N, Polarity::P] {
        for thermal in [false, true] {
            for drain in [2., -0.5, 6.] {
                let mut m = model(polarity);
                m.diode.breakdown = Some(model_kernel_vdmos::Breakdown {
                    bv: 5.,
                    ibv: 1e-4,
                    nbv: 1.2,
                });
                let a = artifact(Arc::new(m.prepare()?), thermal, [8; 16])?;
                let prepared = a.prepare(&[1.5], 310.)?;
                let width = a.descriptor().equations.len();
                let mut x = vec![0.; width * (ORDER + 1)];
                let sign = polarity.sign();
                for (i, eq) in a.descriptor().equations.iter().enumerate() {
                    let (value, slope, curvature) = match eq.name.as_str() {
                        "drain" => (drain * sign, 0.02 * sign, 0.001),
                        "gate" => (5.1 * sign, 0.03 * sign, -0.002),
                        "source" => (0., -0.01, 0.001),
                        "drain_prime" => ((drain - 0.1) * sign, 0.017 * sign, 0.002),
                        "gate_prime" => (5. * sign, 0.025 * sign, -0.001),
                        "source_prime" => (0.1 * sign, 0.003, -0.002),
                        "body_prime" => (0.02 * sign, 0.007, 0.001),
                        "junction" => (340., 0.7, -0.1),
                        "case" => (320., 0.3, 0.05),
                        "recovery" => (1e-8, 2e-10, -1e-11),
                        name => return Err(format!("unexpected coordinate {name}").into()),
                    };
                    x[i] = value;
                    x[width + i] = slope;
                    x[2 * width + i] = curvature;
                }
                let mut t = TaylorBuffer::new(width, ORDER)?;
                a.evaluate_taylor(
                    &prepared,
                    TaylorInput {
                        order: ORDER,
                        coefficients: &x,
                    },
                    &mut t.point(),
                )?;
                let p = t.point();
                let mut scalar = PointBuffer::new(width)?;
                for tau in [0., -0.25, 0.25, 0.5] {
                    let reconstruct = |coeff: &[f64], stride: usize, i: usize| -> f64 {
                        (0..=ORDER)
                            .rev()
                            .fold(0., |v: f64, k| v.mul_add(tau, coeff[k * stride + i]))
                    };
                    let coords: Vec<_> = (0..width).map(|i| reconstruct(&x, width, i)).collect();
                    a.evaluate(&prepared, &coords, &mut scalar.point())?;
                    let s = scalar.point();
                    for (series, expected, stride, atol) in [
                        (&*p.current, &*s.current, width, 1e-11),
                        (&*p.charge, &*s.charge, width, 1e-21),
                        (&*p.conductance, &*s.conductance, width * width, 1e-11),
                        (&*p.capacitance, &*s.capacitance, width * width, 1e-21),
                    ] {
                        for (i, expected) in expected.iter().enumerate() {
                            close(reconstruct(series, stride, i), *expected, atol, 2e-8);
                        }
                    }
                }
                for (values, jac) in [
                    (&*p.current, &*p.conductance),
                    (&*p.charge, &*p.capacitance),
                ] {
                    for k in 0..ORDER {
                        let degree = f64::from(u32::try_from(k + 1)?);
                        for row in 0..width {
                            let mut sum = 0.;
                            let mut norm = 0.;
                            for j in 0..=k {
                                let d = f64::from(u32::try_from(k + 1 - j)?);
                                for col in 0..width {
                                    let term = jac[j * width * width + row * width + col]
                                        * x[(k + 1 - j) * width + col]
                                        * d;
                                    sum += term;
                                    norm += term.abs();
                                }
                            }
                            close(
                                degree * values[(k + 1) * width + row],
                                sum,
                                1e-8 * norm,
                                1e-8,
                            );
                        }
                    }
                }
            }
        }
    }
    Ok(())
}
