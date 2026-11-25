use super::*;
use std::hint::black_box;

fn close(label: &str, actual: f64, expected: f64, atol: f64, rtol: f64) {
    assert!(
        (actual - expected).abs() <= rtol.mul_add(actual.abs().max(expected.abs()), atol),
        "{label}: actual={actual:e}, expected={expected:e}, diff={:e}",
        actual - expected
    );
}

fn calibrated(polarity: Polarity) -> Model {
    let mut model = Model::defaults(polarity);
    model.channel.lambda = 0.004;
    model.channel.theta = 0.07;
    model.channel.subshift = 0.2;
    model.channel.tcvth = -0.003 * polarity.sign();
    model.channel.tksubthres1 = 0.0003;
    model.channel.tksubthres2 = 0.000_000_1;
    model.gate.a = 0.8;
    model.diode.tt = 80e-9;
    model.diode.trb1 = 0.002;
    model.series.rd = 0.04;
    model.series.rs = 0.02;
    model.series.rb = 0.01;
    model.series.rg = 2.0;
    model.series.trd1 = 0.003;
    model.series.trd2 = 0.000_002;
    model
}

fn evaluate(model: &Model, v: Bias) -> Result<Evaluation, ModelError> {
    model.prepare()?.bind(Instance::default())?.evaluate(v)
}

fn derivative<F: Fn(Bias) -> Result<f64, ModelError>>(
    mut bias: Bias,
    mut apply: impl FnMut(&mut Bias, f64),
    f: F,
    eps: f64,
) -> Result<f64, ModelError> {
    apply(&mut bias, eps);
    let pos = f(bias)?;
    apply(&mut bias, -2.0 * eps);
    let neg = f(bias)?;
    Ok((pos - neg) / (2.0 * eps))
}

#[test]
fn source_parameter_zero_saturation_current_uses_c47_epsmin() -> Result<(), ParameterError> {
    // Physical semantics: ngspice47's VDMOSset floors Is to CKTepsmin,
    // rather than silently disabling the diode, and preserves Cjo/BV.
    for raw_is in [0.0, -1e-10, 1e-31] {
        let model = compile_ngspice(
            Polarity::N,
            [
                ("VTO", 3.91),
                ("KP", 425.0),
                ("IS", raw_is),
                ("BV", 83.0),
                ("IBV", 250e-6),
                ("CJO", 6.26e-9),
            ],
        )?;
        assert_eq!(model.parameters().diode.is.to_bits(), (1e-28_f64).to_bits());
        let evaluation = model.bind(Instance::default())?.evaluate(Bias {
            vdiode: 1.5,
            temperature: 300.15,
            ..Bias::default()
        })?;
        assert!(evaluation.diode.current.value.is_finite());
        assert!(evaluation.diode.current.value > 0.0);
        assert!(evaluation.diode.depletion.d_voltage > 0.0);
    }
    Ok(())
}

#[test]
fn zero_gate_and_charge_reference() -> Result<(), ModelError> {
    let m = Model::defaults(Polarity::N);
    let bias = Bias {
        vgs: 4.0,
        vds: 0.25,
        vdiode: -0.1,
        temperature: m.tnom,
        ..Bias::default()
    };
    let result = evaluate(&m, bias)?;
    let vgst = (1.0_f64 / 0.1).exp();
    let weak_v = 0.1 * vgst.ln_1p();
    close(
        "ngspice47 channel triode formula",
        result.channel.value,
        m.channel.kp * 0.25 * (weak_v - 0.125),
        1e-12,
        1e-13,
    );
    close(
        "constant gate-source capacitance",
        result.qgs.d_voltage,
        m.gate.cgs,
        0.0,
        1e-13,
    );
    close(
        "shunt conductance",
        result.series.shunt,
        1.0 / m.series.rds,
        0.0,
        1e-13,
    );
    assert!(result.diode.recovery.is_none());
    Ok(())
}

#[test]
fn nonlinear_channel_jacobian_and_heat_are_consistent() -> Result<(), ModelError> {
    for polarity in [Polarity::N, Polarity::P] {
        let model = calibrated(polarity);
        let sign = polarity.sign();
        for (vgs, vds) in [(1.0, 0.3), (4.0, 0.2), (5.0, 6.0), (6.0, -0.6), (7.0, -5.0)] {
            let bias = Bias {
                vgs: vgs * sign,
                vds: vds * sign,
                vds_external: vds * sign,
                vdiode: -0.2 * sign,
                temperature: 345.0,
                ..Bias::default()
            };
            let result = evaluate(&model, bias)?;
            let dvg = derivative(
                bias,
                |b, eps| b.vgs += eps,
                |b| Ok(evaluate(&model, b)?.channel.value),
                1e-5,
            )?;
            let dvd = derivative(
                bias,
                |b, eps| b.vds += eps,
                |b| Ok(evaluate(&model, b)?.channel.value),
                1e-5,
            )?;
            let dt = derivative(
                bias,
                |b, eps| b.temperature += eps,
                |b| Ok(evaluate(&model, b)?.channel.value),
                1e-3,
            )?;
            close("dIds/dVgs", result.channel.d_vgs, dvg, 1e-8, 3e-5);
            close("dIds/dVds", result.channel.d_vds, dvd, 1e-8, 3e-5);
            close("dIds/dT", result.channel.d_temperature, dt, 1e-9, 3e-5);
            close(
                "conduction thermal power",
                result.heat.channel,
                result.channel.value * bias.vds,
                1e-12,
                1e-12,
            );
            close(
                "thermal derivative dP/dVgs",
                result.heat.d_vgs,
                result.channel.d_vgs * bias.vds,
                1e-9,
                1e-12,
            );
            close(
                "thermal derivative dP/dVds",
                result.heat.d_vds,
                result.channel.d_vds.mul_add(bias.vds, result.channel.value),
                1e-9,
                1e-12,
            );
        }
    }
    Ok(())
}

#[test]
fn intrinsic_gate_charge_derivative_is_differential_capacitance() -> Result<(), ModelError> {
    for polarity in [Polarity::N, Polarity::P] {
        let model = calibrated(polarity);
        for normalized in [-7.0, -0.9, 0.0, 0.1, 5.0] {
            let bias = Bias {
                vgs: normalized * polarity.sign(),
                temperature: 300.15,
                ..Bias::default()
            };
            let result = evaluate(&model, bias)?;
            let fd = derivative(
                bias,
                |b, eps| b.vgs += eps,
                |b| Ok(evaluate(&model, b)?.qgd.value),
                1e-5,
            )?;
            close("dQgd/dVgd", result.qgd.d_voltage, fd, 1e-18, 2e-8);
            assert!(result.qgd.d_voltage >= model.gate.cgdmin);
            assert!(result.qgd.d_voltage <= model.gate.cgdmax);
        }
    }
    Ok(())
}

#[test]
fn diode_current_charge_and_temperature_jacobians() -> Result<(), ModelError> {
    for polarity in [Polarity::N, Polarity::P] {
        let model = calibrated(polarity);
        for voltage in [-1.0, -0.15, -0.04, 0.05, 0.25, 0.6] {
            let bias = Bias {
                vdiode: polarity.sign() * voltage,
                temperature: 325.0,
                ..Bias::default()
            };
            let result = evaluate(&model, bias)?;
            let di = derivative(
                bias,
                |b, eps| b.vdiode += eps,
                |b| Ok(evaluate(&model, b)?.diode.current.value),
                1e-6,
            )?;
            let dq = derivative(
                bias,
                |b, eps| b.vdiode += eps,
                |b| Ok(evaluate(&model, b)?.diode.depletion.value),
                1e-6,
            )?;
            let dti = derivative(
                bias,
                |b, eps| b.temperature += eps,
                |b| Ok(evaluate(&model, b)?.diode.current.value),
                1e-3,
            )?;
            let dtq = derivative(
                bias,
                |b, eps| b.temperature += eps,
                |b| Ok(evaluate(&model, b)?.diode.depletion.value),
                1e-3,
            )?;
            close(
                "diode dI/dV",
                result.diode.current.d_voltage,
                di,
                1e-14,
                1e-6,
            );
            close(
                "diode dQdep/dV",
                result.diode.depletion.d_voltage,
                dq,
                1e-16,
                2e-7,
            );
            close(
                "diode dI/dT",
                result.diode.current.d_temperature,
                dti,
                1e-15,
                1e-6,
            );
            close(
                "diode dQdep/dT",
                result.diode.depletion.d_temperature,
                dtq,
                1e-16,
                2e-6,
            );
            close(
                "diffusion charge=TT*current",
                result.diode.diffusion.value,
                model.diode.tt * result.diode.current.value,
                1e-25,
                2e-6,
            );
            close(
                "diffusion derivative",
                result.diode.diffusion.d_voltage,
                model.diode.tt * result.diode.current.d_voltage,
                1e-25,
                2e-6,
            );
        }
    }
    Ok(())
}

#[test]
fn depletion_grading_unity_including_temperature_sensitivity() -> Result<(), ModelError> {
    let mut model = calibrated(Polarity::N);
    model.diode.m = 1.0;
    model.diode.tm1 = 1e-4;
    for voltage in [-0.25, 0.28, 0.7] {
        let bias = Bias {
            vdiode: voltage,
            temperature: model.tnom,
            ..Bias::default()
        };
        let result = evaluate(&model, bias)?;
        let dtdq = derivative(
            bias,
            |b, eps| b.temperature += eps,
            |b| Ok(evaluate(&model, b)?.diode.depletion.value),
            1e-3,
        )?;
        close(
            "m=1 dQ/dT",
            result.diode.depletion.d_temperature,
            dtdq,
            1e-14,
            2e-6,
        );
        let dqdv = derivative(
            bias,
            |b, eps| b.vdiode += eps,
            |b| Ok(evaluate(&model, b)?.diode.depletion.value),
            1e-5,
        )?;
        close(
            "m=1 dQ/dV",
            result.diode.depletion.d_voltage,
            dqdv,
            1e-15,
            1e-6,
        );
    }
    Ok(())
}

#[test]
fn soft_recovery_uses_an_independent_qp_state_and_consistent_jacobian() -> Result<(), ModelError> {
    let mut model = calibrated(Polarity::N);
    model.diode.vp = 0.4;
    let bias = Bias {
        vdiode: 0.2,
        temperature: 310.0,
        ..Bias::default()
    };
    let current = evaluate(&model, bias)?.diode.current.value;
    let bias = Bias {
        recovery_node: model.diode.tt * current,
        ..bias
    };
    let result = evaluate(&model, bias)?;
    let Some(recovery) = result.diode.recovery else {
        return Err(ModelError::Invalid("expected enabled recovery"));
    };
    close(
        "QP steady state residual",
        recovery.residual,
        0.0,
        1e-22,
        0.0,
    );
    close(
        "QP slope",
        recovery.d_node,
        1.0 / model.diode.vp,
        0.0,
        1e-14,
    );
    close(
        "diode rate coefficient",
        recovery.diode_charge_rate_gain,
        (1.0 - model.diode.vp) / model.diode.tt,
        0.0,
        1e-14,
    );
    assert_eq!(result.diode.diffusion.value, 0.0);
    let dv = derivative(
        bias,
        |b, eps| b.vdiode += eps,
        |b| {
            let eval = evaluate(&model, b)?;
            eval.diode
                .recovery
                .map(|state| state.residual)
                .ok_or(ModelError::Invalid("lost recovery"))
        },
        1e-6,
    )?;
    close("QP dR/dV", recovery.d_voltage, dv, 1e-16, 1e-6);
    let dt = derivative(
        bias,
        |b, eps| b.temperature += eps,
        |b| {
            let eval = evaluate(&model, b)?;
            eval.diode
                .recovery
                .map(|state| state.residual)
                .ok_or(ModelError::Invalid("lost recovery"))
        },
        1e-3,
    )?;
    close("QP dR/dT", recovery.d_temperature, dt, 1e-17, 1e-6);
    Ok(())
}

#[test]
fn prepared_instances_and_batches_do_not_share_state() -> Result<(), ModelError> {
    let model = calibrated(Polarity::N).prepare()?;
    let first = model.bind(Instance::default())?;
    let second = model.bind(Instance {
        multiplier: 4.0,
        self_heating: true,
    })?;
    let bias = Bias {
        vgs: 4.5,
        vds: 2.0,
        vds_external: 2.0,
        vdiode: 0.1,
        temperature: 360.0,
        ..Bias::default()
    };
    let single = first.evaluate(bias)?;
    let quadruple = second.evaluate(bias)?;
    close(
        "independent channel scaling",
        quadruple.channel.value,
        4.0 * single.channel.value,
        1e-12,
        1e-12,
    );
    close(
        "independent body diode scaling",
        quadruple.diode.current.value,
        4.0 * single.diode.current.value,
        1e-13,
        1e-12,
    );
    assert_eq!(single.heat.junction_capacitance, 0.0);
    close(
        "thermal capacitance scaling",
        quadruple.heat.junction_capacitance,
        4.0 * model.model.thermal.cthj,
        0.0,
        1e-14,
    );
    let inputs = [bias, Bias { vgs: 3.5, ..bias }, Bias { vds: 5.0, ..bias }];
    let mut outputs = [single; 3];
    evaluate_batch(&first, &inputs, &mut outputs)?;
    for (input, output) in inputs.into_iter().zip(outputs) {
        close(
            "batch exactly matches scalar",
            output.channel.value,
            first.evaluate(input)?.channel.value,
            0.0,
            0.0,
        );
    }
    Ok(())
}

#[test]
fn quasi_saturation_series_derivatives() -> Result<(), ModelError> {
    let mut model = calibrated(Polarity::N);
    model.series.quasi_saturation = Some(QuasiSaturation { rq: 0.4, vq: 2.0 });
    let bias = Bias {
        vgs: 5.0,
        vds: 1.0,
        vds_external: 2.0,
        temperature: 350.0,
        ..Bias::default()
    };
    let conductance = evaluate(&model, bias)?.series.drain;
    let Some(actual) = conductance.value else {
        return Err(ModelError::Invalid("expected drain conductance"));
    };
    let dvd = derivative(
        bias,
        |b, eps| b.vds_external += eps,
        |b| {
            evaluate(&model, b)?
                .series
                .drain
                .value
                .ok_or(ModelError::Invalid("missing conductance"))
        },
        1e-5,
    )?;
    let dt = derivative(
        bias,
        |b, eps| b.temperature += eps,
        |b| {
            evaluate(&model, b)?
                .series
                .drain
                .value
                .ok_or(ModelError::Invalid("missing conductance"))
        },
        1e-3,
    )?;
    assert!(actual > 0.0);
    close(
        "quasi dG/dVds",
        conductance.d_vds_external,
        dvd,
        1e-11,
        1e-6,
    );
    close("quasi dG/dT", conductance.d_temperature, dt, 1e-11, 1e-6);
    Ok(())
}

#[test]
fn unsupported_modes_reject_instead_of_substituting() {
    let mut model = Model::defaults(Polarity::N);
    model.diode.breakdown = Some(Breakdown {
        bv: 55.0,
        ibv: 1e-9,
        nbv: 1.0,
    });
    assert!(model.prepare().is_ok());
    model.diode.breakdown = Some(Breakdown {
        bv: 0.0,
        ibv: 1e-9,
        nbv: 1.0,
    });
    assert!(matches!(
        model.prepare(),
        Err(ModelError::Invalid("bv/ibv/nbv"))
    ));
    let mut model = Model::defaults(Polarity::N);
    model.channel.ksubthres = -0.1;
    assert!(matches!(model.prepare(), Err(ModelError::Invalid(_))));
}

fn benchmark_variant(model: &Model, label: &str) -> Result<(), ModelError> {
    const WIDTH: usize = 4096;
    const ROUNDS: usize = 256;
    let model = (*model).prepare()?;
    let device = model.bind(Instance {
        multiplier: 2.0,
        self_heating: true,
    })?;
    let mut input = vec![Bias::default(); WIDTH];
    for (i, bias) in input.iter_mut().enumerate() {
        let idx = f64::from(u32::try_from(i).map_err(|_| ModelError::Invalid("benchmark index"))?);
        *bias = Bias {
            vgs: idx.mul_add(0.0006, 2.0),
            vds: idx.mul_add(0.001, 0.3),
            vds_external: idx.mul_add(0.001, 0.3),
            vdiode: idx.mul_add(0.00005, -0.2),
            temperature: idx.mul_add(0.005, 300.15),
            ..Bias::default()
        };
    }
    let one = device.evaluate(input[0])?;
    let mut output = vec![one; WIDTH];
    let start = std::time::Instant::now();
    for _ in 0..ROUNDS {
        evaluate_batch(
            black_box(&device),
            black_box(&input),
            black_box(&mut output),
        )?;
    }
    let elapsed = start.elapsed().as_secs_f64();
    eprintln!(
        "VDMOS kernel ({label}) {} device samples/s",
        f64::from(
            u32::try_from(WIDTH * ROUNDS).map_err(|_| ModelError::Invalid("benchmark size"))?
        ) / elapsed
    );
    black_box(output);
    Ok(())
}

#[test]
#[ignore = "explicit throughput benchmark for prepared homogeneous kernel"]
fn throughput_prepared_batch() -> Result<(), ModelError> {
    let mut model = calibrated(Polarity::N);
    model.diode.vp = 0.4;
    benchmark_variant(&model, "standard")?;
    model.diode.breakdown = Some(Breakdown {
        bv: 40.0,
        ibv: 1e-9,
        nbv: 1.3,
    });
    benchmark_variant(&model, "avalanche")?;
    Ok(())
}

#[test]
fn junction_case_network_and_ohmic_energy_derivatives() -> Result<(), ModelError> {
    let model = calibrated(Polarity::N);
    let prepared = model.prepare()?;
    let instance = prepared.bind(Instance {
        multiplier: 3.0,
        self_heating: true,
    })?;
    let bias = Bias {
        vgs: 5.0,
        vds: 1.0,
        vds_external: 2.0,
        temperature: 330.0,
        ..Bias::default()
    };
    let result = instance.evaluate(bias)?;
    close(
        "junction-case G",
        result.heat.junction_to_case,
        3.0 / model.thermal.rthjc,
        0.0,
        1e-14,
    );
    close(
        "case-ambient G",
        result.heat.case_to_ambient,
        3.0 / model.thermal.rthca,
        0.0,
        1e-14,
    );
    let dissipated = result.series.drain.ohmic_power(0.3);
    close(
        "drain resistor P=G*V²",
        dissipated.value,
        0.09 * result
            .series
            .drain
            .value
            .ok_or(ModelError::Invalid("drain branch"))?,
        1e-14,
        1e-14,
    );
    close(
        "drain resistor dP/dV",
        dissipated.d_voltage_drop,
        0.6 * result
            .series
            .drain
            .value
            .ok_or(ModelError::Invalid("drain branch"))?,
        1e-13,
        1e-14,
    );
    let fd = derivative(
        bias,
        |b, eps| b.temperature += eps,
        |b| Ok(instance.evaluate(b)?.series.drain.ohmic_power(0.3).value),
        1e-3,
    )?;
    close(
        "drain resistor dP/dT",
        dissipated.d_temperature,
        fd,
        1e-12,
        1e-6,
    );
    Ok(())
}

#[test]
fn physical_gate_capacitance_asymptotes_match_model_parameters() -> Result<(), ModelError> {
    let model = calibrated(Polarity::N);
    for (vgd, expected) in [(-1e6, model.gate.cgdmin), (1e6, model.gate.cgdmax)] {
        let result = evaluate(
            &model,
            Bias {
                vgs: vgd,
                temperature: model.tnom,
                ..Bias::default()
            },
        )?;
        close(
            "full physical Cgd limit",
            result.qgd.d_voltage,
            expected,
            1e-14,
            1e-12,
        );
    }
    Ok(())
}

#[test]
fn avalanche_matches_requested_breakdown_current_and_thermal_jacobian() -> Result<(), ModelError> {
    for polarity in [Polarity::N, Polarity::P] {
        let mut model = calibrated(polarity);
        model.diode.breakdown = Some(Breakdown {
            bv: 40.0,
            ibv: 1e-9,
            nbv: 1.3,
        });
        for temperature in [280.0, 300.15, 340.0, 400.0] {
            for reverse_v in [30.0, 38.0, 39.5, 40.5, 41.0] {
                let bias = Bias {
                    vdiode: -polarity.sign() * reverse_v,
                    temperature,
                    ..Bias::default()
                };
                let evaluated = evaluate(&model, bias)?;
                let current = evaluated.diode.current;
                let voltage_fd = derivative(
                    bias,
                    |b, h| b.vdiode += h,
                    |b| Ok(evaluate(&model, b)?.diode.current.value),
                    1e-6,
                )?;
                let temperature_fd = derivative(
                    bias,
                    |b, h| b.temperature += h,
                    |b| Ok(evaluate(&model, b)?.diode.current.value),
                    1e-3,
                )?;
                close(
                    "avalanche dId/dV",
                    current.d_voltage,
                    voltage_fd,
                    1e-14,
                    2e-6,
                );
                close(
                    "avalanche dId/dT",
                    current.d_temperature,
                    temperature_fd,
                    1e-14,
                    2e-5,
                );
                assert!(current.value.is_finite());
            }
        }
        let nominal = evaluate(
            &model,
            Bias {
                vdiode: -polarity.sign() * 40.0,
                temperature: model.tnom,
                ..Bias::default()
            },
        )?;
        close(
            "nominal IBV at -BV",
            nominal.diode.current.value.abs(),
            1e-9,
            0.0,
            0.02,
        );
    }
    Ok(())
}

#[test]
fn avalanche_near_matching_threshold_and_nbv_variants() -> Result<(), ModelError> {
    for nbv in [0.3, 1.0, 2.0, 100.0] {
        for ibv in [1e-14, 1e-12, 1e-9] {
            let mut model = calibrated(Polarity::N);
            model.diode.breakdown = Some(Breakdown { bv: 40.0, ibv, nbv });
            for temperature in [260.0, 300.15, 450.0] {
                let bias = Bias {
                    temperature,
                    vdiode: -41.0,
                    ..Bias::default()
                };
                let result = evaluate(&model, bias)?;
                assert!(result.diode.current.value.is_finite());
                assert!(result.diode.current.d_voltage.is_finite());
                assert!(result.diode.current.d_temperature.is_finite());
                let fd = derivative(
                    bias,
                    |b, h| b.temperature += h,
                    |b| Ok(evaluate(&model, b)?.diode.current.value),
                    1e-4,
                )?;
                close(
                    "avalanche threshold thermal sensitivity",
                    result.diode.current.d_temperature,
                    fd,
                    1e-11,
                    3e-4,
                );
            }
        }
    }
    Ok(())
}

#[test]
fn source_parameters_compile_aliases_and_physical_groups() -> Result<(), ModelError> {
    let source = [
        ("VTH0", 2.6),
        ("KP", 11.0),
        ("LAMBDA", 0.03),
        ("THETA", 0.2),
        ("MTRIODE", 1.7),
        ("SUBSHIFT", 0.01),
        ("KSUBTHRES", 0.08),
        ("VTOTC", -0.001),
        ("BEX", -1.2),
        ("TKSUBTHRES1", 1e-4),
        ("TKSUBTHRES2", 1e-7),
        ("CGDMIN", 11e-12),
        ("CGDMAX", 1.9e-9),
        ("CGS", 0.9e-9),
        ("A", 0.8),
        ("IS", 3e-13),
        ("N", 1.4),
        ("CJO", 0.8e-9),
        ("VJ", 0.79),
        ("M", 0.35),
        ("FC", 0.43),
        ("TT", 100e-9),
        ("VP", 0.4),
        ("EG", 1.07),
        ("XTI", 3.5),
        ("TRB1", 0.002),
        ("TRB2", 1e-5),
        ("BV", 100.0),
        ("IBV", 1e-8),
        ("NBV", 1.7),
        ("RD", 0.02),
        ("RS", 0.01),
        ("RG", 1.9),
        ("RB", 0.09),
        ("RDS", 1e9),
        ("RQ", 0.15),
        ("VQ", 2.0),
        ("TEXP0", 1.7),
        ("TEXP1", 0.8),
        ("TRD1", 0.001),
        ("TRD2", 1e-6),
        ("TRG1", 0.002),
        ("TRG2", 2e-6),
        ("TRS1", 0.003),
        ("TRS2", 3e-6),
        ("RTHJC", 0.8),
        ("RTHCA", 12.0),
        ("CTHJ", 30e-6),
        ("TNOM", 40.0),
        ("KF", 2e-26),
        ("AF", 1.2),
    ];
    let prepared =
        compile_ngspice(Polarity::N, source).map_err(|_| ModelError::Invalid("source compiler"))?;
    let instance = prepared.bind(Instance {
        multiplier: 2.5,
        self_heating: true,
    })?;
    let bias = Bias {
        vgs: 5.0,
        vds: 1.0,
        vds_external: 1.5,
        vdiode: -0.6,
        recovery_node: 1e-10,
        temperature: 345.0,
    };
    let result = instance.evaluate(bias)?;
    assert!(result.channel.value > 0.0);
    assert!(result.diode.recovery.is_some());
    assert!(result.qgd.d_voltage > 0.0);
    assert!(result.heat.junction_capacitance > 0.0);
    assert!(result.series.drain.value.is_some());
    let noise = instance.noise_density(&result, 100.0)?;
    assert!(noise.flicker > 0.0);
    close(
        "TNOM Celsius-to-Kelvin",
        prepared.model.tnom,
        313.15,
        0.0,
        1e-14,
    );
    Ok(())
}

#[test]
fn source_parameter_compiler_never_silently_drops_modes() {
    for unsupported in ["PHI", "VGS_MAX", "PD_MAX", "DERATING", "TYPE"] {
        assert!(matches!(
            compile_ngspice(Polarity::N, [(unsupported, 1.0)]),
            Err(ParameterError::Unsupported(_))
        ));
    }
    for unknown in ["SUBSLOPE", "IOPSPICE", "BSIMLEVEL"] {
        assert!(matches!(
            compile_ngspice(Polarity::N, [(unknown, 2.0)]),
            Err(ParameterError::Unknown(_))
        ));
    }
    for partial in [[("RQ", 0.5), ("KP", 5.0)], [("IBV", 1e-9), ("KP", 5.0)]] {
        assert!(matches!(
            compile_ngspice(Polarity::N, partial),
            Err(ParameterError::Incomplete(_))
        ));
    }
    assert!(compile_ngspice(Polarity::P, [("BV", 70.0)]).is_ok());
    assert!(matches!(
        compile_ngspice(Polarity::N, [("VP", f64::NAN)]),
        Err(ParameterError::Invalid(_))
    ));
}

#[test]
fn intrinsic_noise_densities_follow_ngspice47_laws() -> Result<(), ModelError> {
    let mut model = calibrated(Polarity::N);
    model.noise.kf = 3e-25;
    model.noise.af = 1.3;
    let prepared = model.prepare()?;
    let device = prepared.bind(Instance {
        multiplier: 2.0,
        self_heating: false,
    })?;
    let bias = Bias {
        vgs: 5.0,
        vds: 1.0,
        temperature: 345.0,
        ..Bias::default()
    };
    let eval = device.evaluate(bias)?;
    let low = device.noise_density(&eval, 100.0)?;
    let high = device.noise_density(&eval, 1000.0)?;
    close("flicker 1/f", low.flicker, 10.0 * high.flicker, 0.0, 1e-13);
    close(
        "channel 2/3*4kT|gm|",
        low.channel,
        4.0 * BOLTZ * bias.temperature * (2.0 / 3.0) * eval.channel.d_vgs.abs(),
        0.0,
        1e-13,
    );
    close(
        "drain Johnson noise",
        low.drain,
        4.0 * BOLTZ
            * bias.temperature
            * eval
                .series
                .drain
                .value
                .ok_or(ModelError::Invalid("drain resistor"))?,
        0.0,
        1e-13,
    );
    assert!(device.noise_density(&eval, -1.0).is_err());
    Ok(())
}

#[test]
fn recovery_charge_thermal_cross_derivative_and_polarity() -> Result<(), ModelError> {
    for polarity in [Polarity::N, Polarity::P] {
        let mut model = calibrated(polarity);
        model.diode.vp = 0.37;
        model.diode.ttt1 = 0.003;
        model.diode.ttt2 = 1e-6;
        let prepared = model.prepare()?;
        let device = prepared.bind(Instance::default())?;
        let bias = Bias {
            vdiode: 0.42 * polarity.sign(),
            recovery_node: 0.07,
            temperature: 350.0,
            ..Bias::default()
        };
        let result = device.evaluate(bias)?;
        let recovery = result
            .diode
            .recovery
            .ok_or(ModelError::Invalid("expected recovery"))?;
        close(
            "recovery dQs/dqp",
            recovery.stored_charge.d_voltage,
            model.diode.tt
                * (model.diode.ttt2 * 49.85).mul_add(49.85, model.diode.ttt1.mul_add(49.85, 1.0)),
            1e-15,
            1e-12,
        );
        let dqdt = derivative(
            bias,
            |b, h| b.temperature += h,
            |b| {
                Ok(device
                    .evaluate(b)?
                    .diode
                    .recovery
                    .ok_or(ModelError::Invalid("recovery"))?
                    .stored_charge
                    .value)
            },
            1e-3,
        )?;
        close(
            "recovery dQs/dT",
            recovery.stored_charge.d_temperature,
            dqdt,
            1e-14,
            1e-8,
        );
        let dgaindt = derivative(
            bias,
            |b, h| b.temperature += h,
            |b| {
                Ok(device
                    .evaluate(b)?
                    .diode
                    .recovery
                    .ok_or(ModelError::Invalid("recovery"))?
                    .diode_charge_rate_gain)
            },
            1e-3,
        )?;
        close(
            "recovery temperature charge-current gain",
            recovery.d_gain_d_temperature,
            dgaindt,
            1e-9,
            1e-8,
        );
        let coefficient = recovery.diode_charge_rate_gain;
        assert!(coefficient * polarity.sign() > 0.0);
        close(
            "steady state normalized diode gain",
            coefficient * recovery.stored_charge.d_voltage,
            polarity.sign() * (1.0 - model.diode.vp),
            1e-13,
            1e-12,
        );
    }
    Ok(())
}

#[test]
fn diode_avalanche_thermal_power_is_differentiable() -> Result<(), ModelError> {
    let mut model = calibrated(Polarity::N);
    model.diode.breakdown = Some(Breakdown {
        bv: 45.0,
        ibv: 1e-9,
        nbv: 1.4,
    });
    let bias = Bias {
        vdiode: -45.5,
        temperature: 330.0,
        ..Bias::default()
    };
    let result = evaluate(&model, bias)?;
    close(
        "diode heat=V*I",
        result.heat.diode,
        bias.vdiode * result.diode.current.value,
        1e-10,
        1e-12,
    );
    let temperature_derivative = derivative(
        bias,
        |b, h| b.temperature += h,
        |b| Ok(evaluate(&model, b)?.heat.diode),
        1e-3,
    )?;
    close(
        "avalanche dP/dT",
        result.heat.d_temperature,
        temperature_derivative,
        1e-9,
        1e-6,
    );
    let voltage_derivative = derivative(
        bias,
        |b, h| b.vdiode += h,
        |b| Ok(evaluate(&model, b)?.heat.diode),
        1e-6,
    )?;
    close(
        "avalanche dP/dV",
        result.heat.d_vdiode,
        voltage_derivative,
        1e-9,
        1e-6,
    );
    Ok(())
}

#[test]
fn reversed_cgd_endpoints_preserve_positive_differential_charge() -> Result<(), ModelError> {
    let mut model = Model::defaults(Polarity::P);
    model.gate.cgdmin = 6e-9;
    model.gate.cgdmax = 2e-9;
    let prepared = model.prepare()?;
    let device = prepared.bind(Instance::default())?;
    for vds in [-100.0, -5.0, -1.0, 0.0, 1.0, 5.0, 100.0] {
        let bias = Bias {
            vds,
            vgs: -0.1,
            temperature: 300.15,
            ..Bias::default()
        };
        let evaluated = device.evaluate(bias)?;
        // The analytic asymptotes can round by a few ulps.
        let roundoff = 8.0 * f64::EPSILON * model.gate.cgdmin;
        assert!(evaluated.qgd.d_voltage > 0.0);
        assert!(evaluated.qgd.d_voltage >= model.gate.cgdmax - roundoff);
        assert!(evaluated.qgd.d_voltage <= model.gate.cgdmin + roundoff);
        let numeric = derivative(
            bias,
            |b, delta| b.vgs += delta,
            |b| Ok(device.evaluate(b)?.qgd.value),
            1e-5,
        )?;
        close(
            "reversed Cgd positive charge derivative",
            evaluated.qgd.d_voltage,
            numeric,
            2e-15,
            2e-5,
        );
    }
    Ok(())
}

#[test]
fn large_junction_potential_limit_preserves_charge_derivatives() -> Result<(), ModelError> {
    for polarity in [Polarity::N, Polarity::P] {
        for emission in [0.8_f64, 1.0, 1.856] {
            let mut model = calibrated(polarity);
            model.diode.vj = 16.58;
            model.diode.n = emission;
            model.diode.is = 1e-20;
            model.diode.tt = 0.0;
            let effective_potential = 2.5 / emission;
            let boundary = effective_potential * emission.min(0.95);
            for voltage in [-3.0, 0.3, boundary - 0.01, boundary + 0.01] {
                let bias = Bias {
                    vdiode: polarity.sign() * voltage,
                    temperature: 345.0,
                    ..Bias::default()
                };
                let result = evaluate(&model, bias)?;
                let dv = derivative(
                    bias,
                    |b, eps| b.vdiode += eps,
                    |b| Ok(evaluate(&model, b)?.diode.depletion.value),
                    1e-6,
                )?;
                let dt = derivative(
                    bias,
                    |b, eps| b.temperature += eps,
                    |b| Ok(evaluate(&model, b)?.diode.depletion.value),
                    1e-3,
                )?;
                assert!(result.diode.depletion.d_voltage > 0.0);
                close(
                    "clamped dQ/dV",
                    result.diode.depletion.d_voltage,
                    dv,
                    1e-16,
                    2e-6,
                );
                close(
                    "clamped dQ/dT",
                    result.diode.depletion.d_temperature,
                    dt,
                    1e-16,
                    2e-6,
                );
            }
            let eval = |voltage| {
                evaluate(
                    &model,
                    Bias {
                        vdiode: polarity.sign() * voltage,
                        temperature: 345.0,
                        ..Bias::default()
                    },
                )
            };
            let before = eval(boundary - 1e-8)?.diode.depletion;
            let after = eval(boundary + 1e-8)?.diode.depletion;
            close("continuation Q", before.value, after.value, 1e-14, 2e-6);
            close(
                "continuation C",
                before.d_voltage,
                after.d_voltage,
                1e-14,
                2e-6,
            );
        }
    }
    Ok(())
}

#[test]
fn source_diode_setup_limits_match_c47() -> Result<(), ParameterError> {
    let model = compile_ngspice(Polarity::N, [("EG", -0.1), ("FC", 1.2)])?;
    assert_eq!(model.parameters().diode.eg.to_bits(), 0.1_f64.to_bits());
    assert_eq!(model.parameters().diode.fc.to_bits(), 0.95_f64.to_bits());
    Ok(())
}

#[test]
fn nonpositive_source_series_resistance_omits_internal_branch() -> Result<(), ParameterError> {
    let model = compile_ngspice(
        Polarity::N,
        [("RD", -0.02), ("RS", -0.01), ("RG", -1.0), ("RB", -0.001)],
    )?;
    let series = model.parameters().series;
    for value in [series.rd, series.rs, series.rg, series.rb] {
        assert_eq!(value.to_bits(), 0.0_f64.to_bits());
    }
    Ok(())
}

#[test]
fn fitted_negative_grading_is_checked_at_actual_bias() -> Result<(), ModelError> {
    let mut model = calibrated(Polarity::N);
    model.diode.m = -0.5;
    model.diode.vj = 0.1;
    model.diode.is = 1e-20;
    let prepared = model.prepare()?;
    let device = prepared.bind(Instance::default())?;
    let bias = Bias {
        vdiode: -0.1,
        temperature: 300.15,
        ..Bias::default()
    };
    assert!(device.evaluate(bias)?.diode.depletion.d_voltage > 0.0);
    assert!(matches!(
        device.evaluate(Bias {
            vdiode: 0.3,
            ..bias
        }),
        Err(ModelError::Invalid("negative diode depletion capacitance"))
    ));
    Ok(())
}

#[test]
fn zero_and_extremely_small_weak_inversion_slopes_have_finite_square_law_limit()
-> Result<(), ModelError> {
    for polarity in [Polarity::N, Polarity::P] {
        let sign = polarity.sign();
        let mut model = calibrated(polarity);
        model.channel.ksubthres = 0.0;
        for overdrive in [-1.0, 0.0, 0.2, 0.4, 1.0, 5.0] {
            for vds in [-0.3, 0.05, 2.0] {
                let bias = Bias {
                    vgs: sign.mul_add(overdrive, model.channel.vth),
                    vds: sign * vds,
                    temperature: 340.0,
                    ..Bias::default()
                };
                let limit = evaluate(&model, bias)?.channel;
                for slope in [1e-4, 1e-100, 1e-200, 1e-310] {
                    let mut smoothed = model;
                    smoothed.channel.ksubthres = slope;
                    let result = evaluate(&smoothed, bias)?.channel;
                    for (actual, expected) in [
                        (result.value, limit.value),
                        (result.d_vgs, limit.d_vgs),
                        (result.d_vds, limit.d_vds),
                        (result.d_temperature, limit.d_temperature),
                    ] {
                        close("small slope limit", actual, expected, 1e-12, 1e-10);
                    }
                }
            }
        }
    }
    Ok(())
}

#[test]
fn scaled_softplus_is_finite_at_threshold_and_extreme_ratios() {
    let slope = Jet::temperature(1e-310);
    for x in [-1.0, 0.0, 1.0] {
        let value = Jet::gate(x).scaled_softplus(slope);
        assert!(value.v.is_finite() && value.dx.is_finite() && value.dt.is_finite());
    }
    let midpoint = Jet::gate(0.0).scaled_softplus(Jet::temperature(0.1));
    close(
        "midpoint",
        midpoint.v,
        0.1 * std::f64::consts::LN_2,
        1e-16,
        1e-14,
    );
    close("midpoint gate slope", midpoint.dx, 0.5, 1e-16, 1e-14);
    close(
        "midpoint width slope",
        midpoint.dt,
        std::f64::consts::LN_2,
        1e-16,
        1e-14,
    );
}
