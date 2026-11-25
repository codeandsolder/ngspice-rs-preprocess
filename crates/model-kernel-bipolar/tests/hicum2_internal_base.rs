use model_kernel_bipolar::{
    Branch, EvaluationError, ModelError, Temperature,
    hicum2::{self, internal_base, junction, transport},
};
#[path = "fixtures/hicum2_baseline.rs"]
mod baseline;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn close(actual: f64, expected: f64, relative: f64, absolute: f64) {
    let tolerance = relative.mul_add(actual.abs().max(expected.abs()), absolute);
    assert!(
        (actual - expected).abs() <= tolerance,
        "actual={actual:.15e};expected={expected:.15e};limit={tolerance:.4e}"
    );
}

struct Kernels {
    transport: transport::Prepared,
    junction: junction::Prepared,
    diode: hicum2::Prepared,
    base: internal_base::Prepared,
}

impl Kernels {
    fn new(p: internal_base::Parameters) -> Result<Self, ModelError> {
        let temp = Temperature::new(300.15)?;
        let mut model = baseline::parameters();
        model.latb = 0.0;
        model.latl = 0.0;
        model.vcbar = 0.0;
        let transport = transport::Prepared::new(&model, temp)?;
        let junction = junction::Prepared::new(model.junction, temp)?;
        let diode = hicum2::Prepared::new(
            hicum2::Parameters {
                ibeis: 1e-18,
                mbei: 1.0,
                ireis: 0.0,
                mrei: 2.0,
                ibeps: 0.0,
                mbep: 1.0,
                ireps: 0.0,
                mrep: 2.0,
                ibcis: 1e-16,
                mbci: 1.0,
                ibcxs: 0.0,
                mbcx: 1.0,
                iscs: 0.0,
                msc: 1.0,
            },
            temp,
        )?;
        let base = internal_base::Prepared::new(p, temp)?;
        Ok(Self {
            transport,
            junction,
            diode,
            base,
        })
    }

    fn evaluate(
        &self,
        emitter_voltage: f64,
        collector_voltage: f64,
        branch_voltage: f64,
    ) -> Result<internal_base::Output, EvaluationError> {
        let j = self.junction.evaluate(junction::Bias {
            vbiei: emitter_voltage,
            vbpei: emitter_voltage,
            vbici: collector_voltage,
            vbci: collector_voltage,
            vbpci: collector_voltage,
            vsici: -1.0,
            vsc: -1.0,
        })?;
        let t = self.transport.evaluate(transport::Bias {
            vbiei: emitter_voltage,
            vbici: collector_voltage,
        })?;
        let diode = self.diode.evaluate(hicum2::Bias {
            vbiei: emitter_voltage,
            vbpei: emitter_voltage,
            vbici: collector_voltage,
            vbpci: collector_voltage,
            vsici: -1.0,
        })?;
        self.base.evaluate(&internal_base::Bias {
            vbpbi: branch_voltage,
            emitter: j.intrinsic_emitter,
            collector: j.intrinsic_collector,
            transport: t,
            intrinsic_diode: diode.ibei,
        })
    }
}

const fn base_parameters() -> internal_base::Parameters {
    internal_base::Parameters {
        nominal_rbi0: 20.0,
        rbi0: 20.0,
        qp0: 2e-14,
        fdqr0: 0.0,
        fgeo: 0.6557,
        fqi: 1.0,
        fcrbi: 0.35,
        tr: 1e-12,
    }
}

#[test]
fn native_ngspice47_hicrbi_and_crbi_operating_point_oracle() -> TestResult {
    let fixture = include_str!("fixtures/hicum2_ngspice47_internal_base.csv");
    let mut count = 0;
    let mut worst = [(0.0f64, ""); 2];
    for line in fixture.lines().filter(|line| !line.starts_with('#')) {
        let [
            variant,
            _external_emitter,
            _external_collector,
            internal_emitter,
            internal_collector,
            ibei,
            native_resistance,
            native_shunt,
        ] = <[&str; 8]>::try_from(line.split(',').collect::<Vec<_>>())
            .map_err(|_| "invalid HICUM HICRBI oracle row")?;
        let mut p = base_parameters();
        match variant {
            "baseline" => {}
            "no_crowding" => p.fgeo = 0.0,
            "partition_half" => p.fqi = 0.4,
            "charge_modulation" => p.fdqr0 = 1.5,
            "no_shunt" => p.fcrbi = 0.0,
            _ => return Err("unknown native HICRBI variant".into()),
        }
        let kernel = Kernels::new(p)?;
        let emitter_voltage: f64 = internal_emitter.parse()?;
        let collector_voltage: f64 = internal_collector.parse()?;
        let j = kernel.junction.evaluate(junction::Bias {
            vbiei: emitter_voltage,
            vbpei: emitter_voltage,
            vbici: collector_voltage,
            vbci: collector_voltage,
            vbpci: collector_voltage,
            vsici: -1.0,
            vsc: -1.0,
        })?;
        let transport = kernel.transport.evaluate(transport::Bias {
            vbiei: emitter_voltage,
            vbici: collector_voltage,
        })?;
        let native_ibei: f64 = ibei.parse()?;
        let diode = kernel.diode.evaluate(hicum2::Bias {
            vbiei: emitter_voltage,
            vbpei: emitter_voltage,
            vbici: collector_voltage,
            vbpci: collector_voltage,
            vsici: -1.0,
        })?;
        close(diode.ibei.amperes, native_ibei, 1e-4, 1e-18);
        let output = kernel.base.evaluate(&internal_base::Bias {
            vbpbi: 0.0,
            emitter: j.intrinsic_emitter,
            collector: j.intrinsic_collector,
            transport,
            intrinsic_diode: Branch {
                amperes: native_ibei,
                siemens: 0.0,
            },
        })?;
        let expected = [
            native_resistance.parse::<f64>()?,
            native_shunt.parse::<f64>()?,
        ];
        let actual = [output.resistance.value, output.hf_branch_capacitance];
        for index in 0..2 {
            let difference = (actual[index] - expected[index]).abs()
                / actual[index].abs().max(expected[index].abs()).max(1e-30);
            if difference > worst[index].0 {
                worst[index] = (difference, variant);
            }
            close(actual[index], expected[index], 1e-4, 1e-25);
        }
        count += 1;
    }
    assert_eq!(count, 35);
    assert!(
        worst.iter().all(|(difference, _)| *difference < 1e-4),
        "worst native HICRBI: {worst:?}"
    );
    Ok(())
}

fn check_two_axis_gradients(
    actual: transport::Value2,
    emitter_plus: transport::Value2,
    emitter_minus: transport::Value2,
    collector_plus: transport::Value2,
    collector_minus: transport::Value2,
) {
    let denominator = 2e-6;
    close(
        actual.d_vbiei,
        (emitter_plus.value - emitter_minus.value) / denominator,
        4e-4,
        1e-18,
    );
    close(
        actual.d_vbici,
        (collector_plus.value - collector_minus.value) / denominator,
        4e-4,
        1e-18,
    );
}

#[test]
fn hicrbi_resistance_current_and_shunt_derivatives_are_conservative() -> TestResult {
    for fgeo in [0.0, 0.6557, 3.0] {
        for fqi in [0.3, 1.0] {
            for fcrbi in [0.0, 0.2, 0.8] {
                let p = internal_base::Parameters {
                    nominal_rbi0: 20.0,
                    rbi0: 20.0,
                    qp0: 2e-14,
                    fdqr0: 0.5,
                    fgeo,
                    fqi,
                    fcrbi,
                    tr: 1e-12,
                };
                let kernels = Kernels::new(p)?;
                for emitter in [0.55, 0.7, 0.82, 0.9] {
                    for collector in [-1.0, -0.3, 0.1] {
                        let voltage = 0.012;
                        let center = kernels.evaluate(emitter, collector, voltage)?;
                        assert!(!center.shorted);
                        assert!(center.resistance.value > 0.0);
                        close(
                            center.branch_conductance,
                            1.0 / center.resistance.value,
                            1e-14,
                            0.0,
                        );
                        close(
                            center.resistor_current.value,
                            voltage / center.resistance.value,
                            1e-14,
                            0.0,
                        );
                        close(
                            center.hf_shunt_charge.value,
                            center.hf_branch_capacitance * voltage,
                            1e-14,
                            1e-32,
                        );
                        let h = 1e-6;
                        let ep = kernels.evaluate(emitter + h, collector, voltage)?;
                        let em = kernels.evaluate(emitter - h, collector, voltage)?;
                        let cp = kernels.evaluate(emitter, collector + h, voltage)?;
                        let cm = kernels.evaluate(emitter, collector - h, voltage)?;
                        check_two_axis_gradients(
                            center.resistance,
                            ep.resistance,
                            em.resistance,
                            cp.resistance,
                            cm.resistance,
                        );
                        check_two_axis_gradients(
                            center.resistor_current,
                            ep.resistor_current,
                            em.resistor_current,
                            cp.resistor_current,
                            cm.resistor_current,
                        );
                        check_two_axis_gradients(
                            center.hf_shunt_charge,
                            ep.hf_shunt_charge,
                            em.hf_shunt_charge,
                            cp.hf_shunt_charge,
                            cm.hf_shunt_charge,
                        );
                        let bp = kernels.evaluate(emitter, collector, voltage + h)?;
                        let bm = kernels.evaluate(emitter, collector, voltage - h)?;
                        close(
                            center.branch_conductance,
                            (bp.resistor_current.value - bm.resistor_current.value) / (2.0 * h),
                            2e-9,
                            0.0,
                        );
                        close(
                            center.hf_branch_capacitance,
                            (bp.hf_shunt_charge.value - bm.hf_shunt_charge.value) / (2.0 * h),
                            2e-9,
                            1e-25,
                        );
                    }
                }
            }
        }
    }
    let mut shorted = base_parameters();
    shorted.nominal_rbi0 = 0.0;
    shorted.rbi0 = 0.0;
    let kernels = Kernels::new(shorted)?;
    let output = kernels.evaluate(0.8, -0.5, 0.0)?;
    assert!(output.shorted);
    assert_eq!(output.resistor_current, transport::Value2::default());
    assert_eq!(output.hf_shunt_charge, transport::Value2::default());
    Ok(())
}

#[test]
fn hicrbi_batch_and_parameter_constraints() -> TestResult {
    let temperature = Temperature::new(300.15)?;
    for (which, value) in [
        ("rbi0", -0.1),
        ("fdqr0", -0.6),
        ("fdqr0", 101.0),
        ("fgeo", -0.1),
        ("fqi", -0.1),
        ("fqi", 1.1),
        ("fcrbi", 1.1),
    ] {
        let mut p = base_parameters();
        match which {
            "rbi0" => p.rbi0 = value,
            "fdqr0" => p.fdqr0 = value,
            "fgeo" => p.fgeo = value,
            "fqi" => p.fqi = value,
            "fcrbi" => p.fcrbi = value,
            _ => return Err("unknown invalid parameter".into()),
        }
        assert!(matches!(internal_base::Prepared::new(p,temperature),
            Err(ModelError::InvalidParameter(key)) if key==which));
    }
    let p = base_parameters();
    let kernels = Kernels::new(p)?;
    let t = kernels.transport.evaluate(transport::Bias {
        vbiei: 0.8,
        vbici: -0.4,
    })?;
    let j = kernels.junction.evaluate(junction::Bias {
        vbiei: 0.8,
        vbpei: 0.8,
        vbici: -0.4,
        vbci: -0.4,
        vbpci: -0.4,
        vsici: -1.0,
        vsc: -1.0,
    })?;
    let input = internal_base::Bias {
        vbpbi: 0.01,
        emitter: j.intrinsic_emitter,
        collector: j.intrinsic_collector,
        transport: t,
        intrinsic_diode: Branch {
            amperes: 1e-4,
            siemens: 1e-3,
        },
    };
    let mut outputs = [internal_base::Output::default(); 2];
    kernels.base.evaluate_batch(&[input, input], &mut outputs)?;
    assert_eq!(outputs[0], outputs[1]);
    assert!(matches!(
        kernels.base.evaluate_batch(&[input], &mut outputs),
        Err(EvaluationError::LengthMismatch)
    ));
    let mut invalid = input;
    invalid.emitter.coulombs = f64::NAN;
    assert!(matches!(
        kernels.base.evaluate(&invalid),
        Err(EvaluationError::NonFiniteBias)
    ));
    Ok(())
}

#[test]
fn native_internal_base_nominal_one_milliohm_gate_is_temperature_independent() -> TestResult {
    for (nominal, effective, expected_short) in [
        (0.0, 0.0, true),
        (0.0005, 100.0, true),
        (0.0009, 3.0, true),
        (0.001, 0.0001, false),
        (0.001, 2.0, false),
        (30.0, 0.0009, false),
    ] {
        let mut p = base_parameters();
        p.nominal_rbi0 = nominal;
        p.rbi0 = effective;
        let kernel = Kernels::new(p)?;
        let output = kernel.evaluate(0.78, -0.5, 0.01)?;
        assert_eq!(
            output.shorted, expected_short,
            "source nominal gate RBI0={nominal}, effective={effective}"
        );
        if expected_short {
            assert_eq!(output.resistor_current, transport::Value2::default());
            assert_eq!(output.hf_shunt_charge, transport::Value2::default());
            assert_eq!(output.branch_conductance, 0.0);
            assert_eq!(output.hf_branch_capacitance, 0.0);
        } else {
            assert!(output.resistance.value > 0.0);
            assert!(output.branch_conductance > 0.0);
            assert!(output.hf_branch_capacitance > 0.0);
        }
    }
    let mut invalid = base_parameters();
    invalid.nominal_rbi0 = 1.0;
    invalid.rbi0 = 0.0;
    assert!(matches!(
        internal_base::Prepared::new(invalid, Temperature::new(300.15)?,),
        Err(ModelError::InvalidParameter("rbi0"))
    ));
    invalid = base_parameters();
    invalid.nominal_rbi0 = -0.1;
    assert!(matches!(
        internal_base::Prepared::new(invalid, Temperature::new(300.15)?,),
        Err(ModelError::InvalidParameter("rbi0_nominal"))
    ));
    Ok(())
}
