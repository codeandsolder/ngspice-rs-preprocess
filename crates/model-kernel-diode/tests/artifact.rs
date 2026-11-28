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
            assert_eq!(noise.0, []);
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

#[test]
fn fowler_nordheim_artifact_reuses_terminal_law_and_candidate_slots() -> Result<()> {
    use model_kernel_api::{Capability, trial::TrialCapabilities};
    use model_kernel_diode::{
        artifact::fowler_nordheim::{FAMILY, artifact},
        fowler_nordheim::{Instance, Model},
    };
    let model = Model {
        oxide_thickness: 50.,
        length: 2.,
        width: 3.,
    };
    let compiled = artifact(model, [7; 16])?;
    let descriptor = compiled.descriptor();
    assert_eq!(descriptor.family, FAMILY);
    assert_eq!(descriptor.trial, TrialCapabilities::ENGINE);
    assert_eq!(descriptor.charge, Capability::Computed);
    assert_eq!(descriptor.noise, Capability::Computed);
    assert_eq!(descriptor.equations.len(), 2);
    assert!(
        descriptor
            .equations
            .iter()
            .all(|e| e.binding == Binding::Terminal)
    );
    assert_eq!(
        descriptor
            .parameters
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>(),
        ["l", "w", "m", "n"]
    );
    let mut buffer = PointBuffer::new(2)?;
    for [length, width, multiplier, series] in [
        [2., 3., 1., 1.],
        [0., 3., 2., 2.],
        [-2., 3., 0.5, 0.5],
        [2., 3., 0., 1.],
        [2., 3., -2., -2.],
    ] {
        let local = model.bind(Instance {
            length: Some(length),
            width: Some(width),
            multiplier,
            series,
        })?;
        let prepared = compiled.prepare(&[length, width, multiplier, series], 300.15)?;
        for voltage in [-10., -4., 0., 4., 10.] {
            let coordinates = [voltage + 2., 2.];
            let expected = local.evaluate(voltage)?;
            compiled.evaluate(&prepared, &coordinates, &mut buffer.point())?;
            let point = buffer.point();
            assert_eq!(point.current, [expected.current, -expected.current]);
            assert_eq!(point.charge, [expected.charge, -expected.charge]);
            let g = expected.conductance;
            let c = expected.capacitance;
            assert_eq!(point.conductance, [g, -g, -g, g]);
            assert_eq!(point.capacitance, [c, -c, -c, c]);
            let mut noise = Noise::default();
            let result = compiled.evaluate_noise(&prepared, &coordinates, 1e3, &mut noise);
            if let Ok(density) = local.shot_noise(expected.current) {
                result?;
                assert_eq!(noise.0, [("shot".into(), [0, 1], density)]);
            } else {
                assert!(result.is_err());
                assert_eq!(noise.0, []);
            }
        }
    }
    assert!(compiled.prepare(&[2., 3., 1., 0.], 300.15).is_err());
    Ok(())
}

#[test]
fn physical_taylor_diode_reconstructs_full_model() -> Result<()> {
    use model_kernel_api::taylor::{TaylorBuffer, TaylorInput};
    const ORDER: usize = 12;
    for grading in [0.4, 1., 1.2] {
        for rs in [0., 3.] {
            let model = compile_simetrix([
                ("IS", 1e-12),
                ("N", 1.3),
                ("ISR", 2e-8),
                ("NR", 2.),
                ("IKF", 1e-4),
                ("BV", 4.),
                ("NBV", 1.4),
                ("IBV", 1e-5),
                ("IBVL", 1e-7),
                ("NBVL", 2.2),
                ("CJO", 1e-9),
                ("VJ", 0.7),
                ("M", grading),
                ("FC", 0.5),
                ("TT", 20e-9),
                ("RS", rs),
            ])?;
            let compiled = artifact(model, Instance::default(), [6; 16])?;
            let prepared = compiled.prepare(&[1., 1., 1e-12, 0.], 300.15)?;
            let width = compiled.descriptor().equations.len();
            let anode = if width == 3 { 2 } else { 0 };
            for bias in [-4.1, -1., 0., 0.35, 0.6, 0.8] {
                for slope in [-0.01, 0.01] {
                    let mut coefficients = vec![0.; width * (ORDER + 1)];
                    coefficients[anode] = bias;
                    coefficients[width + anode] = slope;
                    coefficients[2 * width + anode] = 0.001;
                    let mut series_buffer = TaylorBuffer::new(width, ORDER)?;
                    compiled.evaluate_taylor(
                        &prepared,
                        TaylorInput {
                            order: ORDER,
                            coefficients: &coefficients,
                        },
                        &mut series_buffer.point(),
                    )?;
                    let series_point = series_buffer.point();
                    let mut scalar = PointBuffer::new(width)?;
                    for tau in [0., 0.125, 0.5] {
                        let reconstruct = |coeff: &[f64], stride: usize, i: usize| -> f64 {
                            (0..=ORDER)
                                .rev()
                                .fold(0., |bias: f64, k| bias.mul_add(tau, coeff[k * stride + i]))
                        };
                        let coords: Vec<_> = (0..width)
                            .map(|i| reconstruct(&coefficients, width, i))
                            .collect();
                        compiled.evaluate(&prepared, &coords, &mut scalar.point())?;
                        let scalar_point = scalar.point();
                        for (series, expected, stride, atol) in [
                            (&*series_point.current, &*scalar_point.current, width, 1e-13),
                            (&*series_point.charge, &*scalar_point.charge, width, 1e-21),
                            (
                                &*series_point.conductance,
                                &*scalar_point.conductance,
                                width * width,
                                1e-13,
                            ),
                            (
                                &*series_point.capacitance,
                                &*scalar_point.capacitance,
                                width * width,
                                1e-21,
                            ),
                        ] {
                            for (i, expected) in expected.iter().enumerate() {
                                close(reconstruct(series,stride,i), *expected, atol, 1e-8).map_err(|e| format!("M={grading} RS={rs} bias={bias} slope={slope} tau={tau} entry={i}: {e}"))?;
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

#[test]
fn exact_diode_join_and_full_order_exponential() -> Result<()> {
    use model_kernel_api::{
        batch::{LaneUpdate, PreparedTile},
        taylor::{MAX_ORDER, TaylorBuffer, TaylorInput},
    };
    // At nominal temperature with no extra transport terms the exact current
    // is Is*expm1(v/Vt), and the storage is TT*I plus depletion charge.
    let model = compile_simetrix([("IS", 1e-12), ("TT", 3e-9)])?;
    let a = artifact(model, Instance::default(), [9; 16])?;
    let mut tile = PreparedTile::new(
        &a,
        &[(&[1., 1., 0., 0.], 300.15), (&[2., 1., 0., 0.], 300.15)],
    )?;
    let vt = model_kernel_diode::K_OVER_Q * 300.15;
    let mut x = vec![0.; 2 * (MAX_ORDER + 1)];
    x[0] = vt;
    x[2] = vt;
    let input = TaylorInput {
        order: MAX_ORDER,
        coefficients: &x,
    };
    let mut buffer = TaylorBuffer::new(2, MAX_ORDER)?;
    for (lane, area) in [(0, 1.), (1, 2.)] {
        tile.evaluate_lane_taylor(lane, input, &mut buffer.point())?;
        let p = buffer.point();
        let mut i = area * 1e-12 * std::f64::consts::E;
        close(p.current[0], area * 1e-12 * 1_f64.exp_m1(), 0., 1e-13)?;
        for k in 0..=MAX_ORDER {
            if k > 0 {
                i /= f64::from(u32::try_from(k)?);
                close(p.current[2 * k], i, 0., 2e-13)?;
                close(p.charge[2 * k], 3e-9 * i, 0., 2e-13)?;
            }
            close(p.conductance[4 * k], i / vt, 0., 2e-13)?;
            close(p.capacitance[4 * k], 3e-9 * i / vt, 0., 2e-13)?;
        }
    }
    let replacement = [LaneUpdate {
        lane: 1,
        values: &[3., 1., 0., 0.],
        ambient_kelvin: 300.15,
    }];
    tile.stage_replacements(&a, &replacement)?.commit();
    tile.evaluate_lane_taylor(1, input, &mut buffer.point())?;
    close(
        buffer.point().current[2],
        3e-12 * std::f64::consts::E,
        0.,
        1e-13,
    )?;
    assert!(
        tile.evaluate_lane_taylor(2, input, &mut buffer.point())
            .is_err()
    );

    // FC=0 is an exact depletion join, independent of rounded temperature
    // adjustment of VJ. Q is quadratic on the positive side and has nonzero
    // cubic derivative on the negative side. No perturbation of v0 is used.
    let model = compile_simetrix([
        ("IS", 0.),
        ("CJO", 2e-9),
        ("VJ", 1.),
        ("M", 0.5),
        ("FC", 0.),
    ])?;
    let a = artifact(model, Instance::default(), [10; 16])?;
    let prepared = a.prepare(&[1., 1., 0., 0.], 300.15)?;
    let mut buffer = TaylorBuffer::new(2, 4)?;
    for slope in [-0.1, 0.1] {
        let coefficients = [0., 0., slope, 0., 0., 0., 0., 0., 0., 0.];
        a.evaluate_taylor(
            &prepared,
            TaylorInput {
                order: 4,
                coefficients: &coefficients,
            },
            &mut buffer.point(),
        )?;
        let p = buffer.point();
        close(p.charge[2], 2e-9 * slope, 0., 1e-13)?;
        close(p.charge[4], 0.5e-9 * slope * slope, 0., 1e-13)?;
        let cubic = if slope < 0. {
            0.25e-9 * slope * slope * slope
        } else {
            0.
        };
        close(p.charge[6], cubic, 0., 1e-13)?;
        close(
            p.capacitance[8],
            if slope < 0. {
                0.75e-9 * slope * slope
            } else {
                0.
            },
            0.,
            1e-13,
        )?;
    }
    Ok(())
}

#[test]
fn scaled_exponential_series_avoids_unscaled_overflow() -> Result<()> {
    use model_kernel_api::taylor::{MAX_ORDER, TaylorBuffer, TaylorInput};
    let model = compile_simetrix([("IS", 1e-300)])?;
    let a = artifact(model, Instance::default(), [12; 16])?;
    let p = a.prepare(&[1., 1., 0., 0.], 300.15)?;
    let vt = model_kernel_diode::K_OVER_Q * 300.15;
    let mut x = vec![0.; 2 * (MAX_ORDER + 1)];
    x[0] = 700. * vt;
    x[2] = 100. * vt;
    let mut output = TaylorBuffer::new(2, MAX_ORDER)?;
    a.evaluate_taylor(
        &p,
        TaylorInput {
            order: MAX_ORDER,
            coefficients: &x,
        },
        &mut output.point(),
    )?;
    let out = output.point();
    let mut expected = 1e-300 * 700_f64.exp();
    for (degree, k) in (0..=MAX_ORDER).zip(0_u32..) {
        if k != 0 {
            expected *= 100. / f64::from(k);
        }
        close(out.current[2 * degree], expected, 0., 2e-12)?;
        close(out.conductance[4 * degree], expected / vt, 0., 2e-12)?;
    }
    Ok(())
}

#[test]
fn physical_guards_follow_intrinsic_voltage_and_selected_charge_germ() -> Result<()> {
    use model_kernel_api::{
        batch::PreparedTile,
        taylor::{GuardPredicate, TaylorBuffer, TaylorInput},
    };
    // The series branch has no constitutive join; both surfaces depend on
    // intrinsic, not external, voltage. Check the exact branch and the next
    // constitutive coefficients at zero and the depletion continuation.
    for resistance in [0., 0.5] {
        let model = compile_simetrix([
            ("IS", 1e-12),
            ("IKF", 1e-3),
            ("CJO", 20e-12),
            ("VJ", 0.8),
            ("M", 0.4),
            ("FC", 0.5),
            ("RS", resistance),
        ])?;
        let a = artifact(model, Instance::default(), [13; 16])?;
        let p = a.prepare(&[1., 1., 0., 0.], 300.15)?;
        let tile = PreparedTile::new(&a, &[(&[1., 1., 0., 0.], 300.15)])?;
        let width = a.descriptor().equations.len();
        let anode = if width == 3 { 2 } else { 0 };
        let mut buffer = TaylorBuffer::new(width, 3)?;
        for (bias, site) in [(0., 0), (0.4, 1)] {
            for slope in [-0.03, 0., 0.03] {
                let mut x = vec![0.; width * 4];
                x[anode] = bias;
                x[width + anode] = slope;
                if width == 3 {
                    x[0] = 2.;
                    x[width] = -0.5;
                }
                let input = TaylorInput {
                    order: 3,
                    coefficients: &x,
                };
                let verify = |out: &mut TaylorBuffer| {
                    let point = out.point();
                    let guards = point.guards.records();
                    assert_eq!(guards.len(), 2);
                    let guard = guards.iter().find(|guard| guard.site == site).unwrap();
                    let predicate = if site == 0 {
                        GuardPredicate::Greater
                    } else {
                        GuardPredicate::Less
                    };
                    assert_eq!(guard.predicate, predicate);
                    assert_eq!(
                        guard.selected,
                        if site == 0 { slope > 0. } else { slope < 0. }
                    );
                    assert_eq!(guard.surface.coefficient(0).to_bits(), 0_f64.to_bits());
                    assert_eq!(guard.surface.coefficient(1).to_bits(), slope.to_bits());
                };
                a.evaluate_taylor(&p, input, &mut buffer.point())?;
                verify(&mut buffer);
                tile.evaluate_lane_taylor(0, input, &mut buffer.point())?;
                verify(&mut buffer);
            }
        }
    }
    // A linear depletion charge (M=0) and disabled IKF have no physical joins.
    let model = compile_simetrix([("IS", 1e-12), ("CJO", 20e-12), ("M", 0.)])?;
    let a = artifact(model, Instance::default(), [14; 16])?;
    let p = a.prepare(&[1., 1., 0., 0.], 300.15)?;
    let mut buffer = TaylorBuffer::new(2, 1)?;
    a.evaluate_taylor(
        &p,
        TaylorInput {
            order: 1,
            coefficients: &[0., 0., 1., 0.],
        },
        &mut buffer.point(),
    )?;
    assert!(buffer.point().guards.records().is_empty());
    Ok(())
}
