//! Prepared kernel artifact acceptance tests. This stands outside the
//! ngspice-rs-fast netlist parser and sparse solver intentionally.

use model_kernel_bipolar::{
    EvaluationError, ModelError, Temperature,
    hicum2::{
        Polarity,
        artifact::{
            self, Electrothermal, Instance, LocalPhysics, Model, Provenance, SemanticDigest,
            SourceFormat, StructureKey, Trial,
        },
        instance, nqs, temperature,
        topology::{Node, Resistor},
    },
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn source_model() -> Result<Model, ModelError> {
    Model::new(&temperature::Nominal::source_defaults(), None)
}

fn layout_unknowns(instance: &Instance, b: f64, c: f64) -> Vec<f64> {
    let layout = instance.layout();
    let mut voltage = vec![0.0; layout.count()];
    let v = |name: Node| match name {
        Node::B | Node::Bp | Node::Bi => b,
        Node::C | Node::Ci => c,
        Node::E | Node::Ei | Node::S | Node::Si => 0.0,
    };
    for node in Node::ALL {
        voltage[layout.node(node)] = v(node);
    }
    voltage
}

fn option(area: f64, multiplicity: f64) -> instance::Options {
    instance::Options {
        area,
        multiplicity,
        ..instance::Options::default()
    }
}

#[test]
fn artifact_semantic_hash_is_stable_over_provenance_and_model_card_spelling() -> TestResult {
    use model_kernel_bipolar::hicum2::binding::{self, Assignment, Value};
    let defaults = temperature::Nominal::source_defaults();
    let built = Model::new(&defaults, None)?;
    let source = Provenance {
        format: SourceFormat::NativeSpiceModel,
        source_sha256: Some([73; 32]),
    };
    let from_deck = binding::model(
        "NPN",
        Some(8.0),
        &[
            Assignment {
                name: "LEVEL",
                value: Value::Scalar(8.0),
            },
            Assignment {
                name: "Qp0",
                value: Value::Scalar(defaults.transport.qp0),
            },
            Assignment {
                name: "TYPE",
                value: Value::Text("NpN"),
            },
            Assignment {
                name: "VERSION",
                value: Value::Text("2.4.0"),
            },
        ],
    )?;
    let alternate = Model::new(&from_deck, Some(source))?;
    assert_eq!(built.identity(), alternate.identity());
    assert_eq!(built.provenance(), None);
    assert_eq!(alternate.provenance(), Some(source));
    let mut altered = defaults;
    altered.transport.qp0 *= 1.01;
    assert_ne!(built.identity(), Model::new(&altered, None)?.identity());
    altered = defaults;
    altered.polarity = Polarity::Pnp;
    assert_ne!(built.identity(), Model::new(&altered, None)?.identity());
    altered = defaults;
    altered.transport.qp0 = 0.0;
    assert!(matches!(
        Model::new(&altered, None),
        Err(ModelError::InvalidParameter("qp0"))
    ));
    altered = defaults;
    altered.noise.af = 11.5;
    assert!(matches!(
        Model::new(&altered, None),
        Err(ModelError::InvalidParameter("af"))
    ));
    // Signed zero does not create a different physical compact model.
    altered = defaults;
    altered.transport.ich = -0.0;
    assert_eq!(built.identity(), Model::new(&altered, None)?.identity());
    assert_ne!(SemanticDigest([0; 32]), built.identity());
    assert_eq!(artifact::SOURCE_VERSION, "2.4.0");
    assert_eq!(artifact::SCHEMA_REVISION, 1);
    Ok(())
}

#[test]
fn artifact_structure_key_and_layout_cover_all_32_source_resistor_short_sets() -> TestResult {
    let base = temperature::Nominal::source_defaults();
    let ambient = Temperature::new(300.15)?;
    for presence in 0u8..32 {
        let mut model = base;
        for (index, resistor) in Resistor::ALL.into_iter().enumerate() {
            let present = presence & (1 << index) != 0;
            let value = if present { 3.0 } else { 0.0 };
            match resistor {
                Resistor::Rbx => model.auxiliary.rbx = value,
                Resistor::Rbi => model.auxiliary.rbi0 = value,
                Resistor::Rcx => model.auxiliary.rcx = value,
                Resistor::Re => model.auxiliary.re = value,
                Resistor::Rsu => model.auxiliary.rsu = value,
            }
        }
        let artifact = Model::new(&model, None)?;
        let instance = artifact.bind(ambient, instance::Options::default())?;
        let layout = instance.layout();
        assert_eq!(
            layout.electrical_count(),
            4 + presence.count_ones() as usize
        );
        assert_eq!(layout.count(), layout.electrical_count());
        assert_eq!(StructureKey::from_layout(layout).0, presence);
        assert_eq!(instance.identity().structure, StructureKey(presence));
        for resistor in Resistor::ALL {
            let (a, b) = resistor.nodes();
            let distinct = layout.node(a) != layout.node(b);
            assert_eq!(distinct, presence & (1 << resistor.index()) != 0);
        }
        assert_eq!(layout.node(Node::C), 0);
        assert_eq!(layout.node(Node::B), 1);
        assert_eq!(layout.node(Node::E), 2);
        assert_eq!(layout.node(Node::S), 3);
    }
    Ok(())
}

#[test]
fn independent_geometry_temperature_and_solver_initials_have_correct_identity() -> TestResult {
    let artifact = source_model()?;
    let ambient = Temperature::new(300.15)?;
    let defaults = artifact.bind(ambient, option(1.0, 1.0))?;
    let geometry = artifact.bind(ambient, option(2.0, 3.0))?;
    let geometry2 = artifact.bind(ambient, option(3.0, 2.0))?;
    assert_eq!(defaults.identity().model, geometry.identity().model);
    assert_ne!(defaults.identity().scaled, geometry.identity().scaled);
    // The AREA*M product is the same but source peripheral coefficients
    // depend independently on M. Identity must not conflate them.
    assert_ne!(geometry.identity().scaled, geometry2.identity().scaled);
    assert_eq!(
        geometry.identity().structure,
        geometry2.identity().structure
    );
    assert_eq!(
        geometry.identity().ambient_kelvin_bits,
        ambient.kelvin().to_bits()
    );
    let warmer = artifact.bind(Temperature::new(310.15)?, option(2.0, 3.0))?;
    assert_eq!(geometry.identity().scaled, warmer.identity().scaled);
    assert_ne!(geometry.identity(), warmer.identity());
    let initial = instance::InitialVoltages {
        vbe: Some(0.72),
        vce: Some(1.3),
        vcs: Some(-0.2),
    };
    let frontend = artifact.bind(
        ambient,
        instance::Options {
            off: true,
            initial,
            ..option(2.0, 3.0)
        },
    )?;
    assert_eq!(frontend.identity(), geometry.identity());
    assert_eq!(frontend.initial_voltages(), initial);
    assert!(frontend.off());
    assert!(!geometry.off());
    assert_eq!(
        geometry.initial_voltages(),
        instance::InitialVoltages::default()
    );
    // The strict input source schema must not be misapplied to
    // valid scaled physical values that exceed a model-card upper bound.
    let mut high = temperature::Nominal::source_defaults();
    high.transport.qp0 = 0.1;
    let high = Model::new(&high, None)?;
    let high_instance = high.bind(ambient, option(100.0, 2.0))?;
    assert_ne!(high.identity(), high_instance.identity().scaled);
    Ok(())
}

#[test]
fn immutable_artifact_trial_matches_direct_physical_device_on_all_topologies() -> TestResult {
    let ambient = Temperature::new(300.15)?;
    let mut checks = 0;
    for presence in 0u8..32 {
        let mut nominal = temperature::Nominal::source_defaults();
        for resistor in Resistor::ALL {
            let resistance = if presence & (1 << resistor.index()) != 0 {
                2.0
            } else {
                0.0
            };
            match resistor {
                Resistor::Rbx => nominal.auxiliary.rbx = resistance,
                Resistor::Rbi => nominal.auxiliary.rbi0 = resistance,
                Resistor::Rcx => nominal.auxiliary.rcx = resistance,
                Resistor::Re => nominal.auxiliary.re = resistance,
                Resistor::Rsu => nominal.auxiliary.rsu = resistance,
            }
        }
        let artifact = Model::new(&nominal, None)?;
        let instance = artifact.bind(ambient, option(2.0, 3.0))?;
        let prepared = instance.prepare(0.0)?;
        assert_eq!(
            instance.capabilities().local,
            LocalPhysics::CurrentsChargesNoise
        );
        assert_eq!(
            instance.capabilities().electrothermal,
            Electrothermal::NotPresent
        );
        assert!(!instance.capabilities().vertical_nqs);
        assert_eq!(prepared.layout(), instance.layout());
        for (b, c) in [(0.5, 0.1), (0.72, 1.2), (0.84, -0.2)] {
            let unknowns = layout_unknowns(&instance, b, c);
            let trial = Trial {
                unknowns: &unknowns,
                electrothermal: None,
            };
            let frame = prepared.evaluate(trial)?;
            let direct = instance::Bound::new(&nominal, ambient, option(2.0, 3.0))?
                .prepare(0.0)?
                .device_kernel()?;
            let operating = direct.evaluate(&model_kernel_bipolar::hicum2::device::Bias {
                nodes: frame.operating.nodes,
                nqs: nqs::State::default(),
            })?;
            // Compare to independently assembled branch-to-nodal
            // source residuals, including full implicit Tf chain rule.
            let source = direct.source_stamps(&frame.operating)?;
            let expected = instance
                .layout()
                .reduce(&source)?
                .complete_from_operating(&frame.operating, None)?;
            assert_eq!(frame.local.layout, instance.layout());
            for row in 0..instance.layout().count() {
                assert_eq!(frame.local.current[row], expected.current[row]);
                assert_eq!(frame.local.charge[row], expected.charge[row]);
                for col in 0..instance.layout().count() {
                    assert_eq!(
                        frame.local.conductance[row][col],
                        expected.conductance[row][col]
                    );
                    assert_eq!(
                        frame.local.capacitance[row][col],
                        expected.capacitance[row][col]
                    );
                }
            }
            // Trial inputs stay borrowed and the operating point is not
            // accepted implicitly; repeat evaluation is deterministic.
            let again = prepared.evaluate(trial)?;
            assert_eq!(again.operating, frame.operating);
            assert_eq!(operating, frame.operating);
            checks += 1;
        }
    }
    assert_eq!(checks, 96);
    Ok(())
}

#[test]
fn borrowed_batch_reuses_output_slots_and_never_owns_accepted_state() -> TestResult {
    let artifact = source_model()?;
    let ambient = Temperature::new(300.15)?;
    let instance = artifact.bind(ambient, instance::Options::default())?;
    let prepared = instance.prepare(0.0)?;
    let first = layout_unknowns(&instance, 0.68, 1.2);
    let second = layout_unknowns(&instance, 0.72, 1.2);
    let third = layout_unknowns(&instance, 0.78, 0.4);
    let inputs = [
        Trial {
            unknowns: &first,
            electrothermal: None,
        },
        Trial {
            unknowns: &second,
            electrothermal: None,
        },
        Trial {
            unknowns: &third,
            electrothermal: None,
        },
    ];
    let mut outputs = [None; 3];
    assert!(matches!(
        prepared.evaluate_batch(&inputs, &mut outputs[..2]),
        Err(EvaluationError::LengthMismatch)
    ));
    assert!(outputs.iter().all(Option::is_none));
    prepared.evaluate_batch(&inputs, &mut outputs)?;
    for (trial, output) in inputs.iter().zip(outputs) {
        let frame = output.ok_or("missing borrowed batch frame")?;
        let separate = prepared.evaluate(*trial)?;
        assert_eq!(frame.operating, separate.operating);
        assert_eq!(frame.local.current, separate.local.current);
        assert_eq!(frame.local.charge, separate.local.charge);
        assert_eq!(frame.local.conductance, separate.local.conductance);
        assert_eq!(frame.local.capacitance, separate.local.capacitance);
        let frequency = 1e6;
        let noise = prepared.evaluate_noise(&frame.operating, frequency)?;
        let direct = instance::Bound::new(
            &temperature::Nominal::source_defaults(),
            ambient,
            instance::Options::default(),
        )?
        .prepare(0.0)?
        .device_kernel()?
        .evaluate_noise(&frame.operating, frequency)?;
        assert_eq!(noise, direct);
    }
    let mut failed = third.clone();
    failed[0] = f64::NAN;
    let inputs = [
        Trial {
            unknowns: &first,
            electrothermal: None,
        },
        Trial {
            unknowns: &failed,
            electrothermal: None,
        },
    ];
    let mut output = [None; 2];
    assert!(matches!(
        prepared.evaluate_batch(&inputs, &mut output),
        Err(EvaluationError::NonFiniteBias)
    ));
    assert!(output[0].is_some());
    assert!(output[1].is_none());
    let mut invalid = third;
    invalid.push(0.0);
    assert!(matches!(
        prepared.evaluate(Trial {
            unknowns: &invalid,
            electrothermal: None
        }),
        Err(EvaluationError::LengthMismatch)
    ));
    assert!(matches!(
        prepared.evaluate(Trial {
            unknowns: &[],
            electrothermal: None
        }),
        Err(EvaluationError::LengthMismatch)
    ));
    Ok(())
}

#[test]
fn borrowed_nqs_unknowns_are_distinct_and_source_residual_is_differentiable() -> TestResult {
    let mut nominal = temperature::Nominal::source_defaults();
    nominal.transport.flcomp = 2.3;
    nominal.transport.t0 = 5e-12;
    nominal.auxiliary.flnqs = true;
    nominal.noise.alit = 0.5;
    nominal.noise.alqf = 0.5;
    let artifact = Model::new(&nominal, None)?;
    let device = artifact.bind(Temperature::new(300.15)?, option(1.0, 1.0))?;
    let layout = device.layout();
    assert!(device.capabilities().vertical_nqs);
    assert_eq!(layout.topology().nqs_state_count(), 3);
    assert_eq!(layout.count(), 7);
    assert_eq!(device.identity().structure.0 & (1 << 5), 1 << 5);
    let prepared = device.prepare(0.0)?;
    let mut candidate = layout_unknowns(&device, 0.72, 1.0);
    candidate[layout.nqs_slot(0).ok_or("missing first NQS unknown")?] = 1e-4;
    candidate[layout.nqs_slot(1).ok_or("missing second NQS unknown")?] = 2e-4;
    candidate[layout.nqs_slot(2).ok_or("missing stored-charge unknown")?] = 1e-14;
    let center = prepared.evaluate(Trial {
        unknowns: &candidate,
        electrothermal: None,
    })?;
    assert!(center.operating.nqs.active);
    for (slot, delta) in [(0, 1e-7), (1, 1e-7), (2, 1e-17)] {
        let col = layout.nqs_slot(slot).ok_or("missing source NQS column")?;
        let mut high = candidate.clone();
        let mut low = candidate.clone();
        high[col] += delta;
        low[col] -= delta;
        let upper = prepared.evaluate(Trial {
            unknowns: &high,
            electrothermal: None,
        })?;
        let lower = prepared.evaluate(Trial {
            unknowns: &low,
            electrothermal: None,
        })?;
        for row in 0..layout.count() {
            let derivative = (upper.local.current[row] - lower.local.current[row]) / (2.0 * delta);
            let predicted = center.local.conductance[row][col];
            let tolerance = 1e-6f64.mul_add(derivative.abs().max(predicted.abs()), 1e-9);
            assert!(
                (derivative - predicted).abs() <= tolerance,
                "NQS row={row}, slot={slot}, derivative={derivative}, predicted={predicted}"
            );
            let dynamic = (upper.local.charge[row] - lower.local.charge[row]) / (2.0 * delta);
            let capacitance = center.local.capacitance[row][col];
            let tolerance = 1e-6f64.mul_add(dynamic.abs().max(capacitance.abs()), 1e-12);
            assert!(
                (dynamic - capacitance).abs() <= tolerance,
                "NQS Q row={row}, slot={slot}, derivative={dynamic}, predicted={capacitance}"
            );
        }
    }
    let mut changed = candidate;
    changed[layout.nqs_slot(0).ok_or("missing source NQS slot")?] *= 1.3;
    let new_frame = prepared.evaluate(Trial {
        unknowns: &changed,
        electrothermal: None,
    })?;
    assert_ne!(center.operating.nqs, new_frame.operating.nqs);
    Ok(())
}

#[test]
fn self_heating_requires_complete_external_thermal_column_and_fresh_preparation() -> TestResult {
    let mut nominal = temperature::Nominal::source_defaults();
    nominal.auxiliary.flsh = 2;
    nominal.auxiliary.rth = 250.0;
    nominal.auxiliary.cth = 1e-9;
    let artifact = Model::new(&nominal, None)?;
    let device = artifact.bind(Temperature::new(300.15)?, instance::Options::default())?;
    assert_eq!(
        device.capabilities().electrothermal,
        Electrothermal::ExternalCouplingRequired
    );
    assert!(device.layout().thermal_slot().is_some());
    assert_eq!(device.layout().count(), 5);
    assert_eq!(device.identity().structure.0 & (1 << 6), 1 << 6);
    let previous = device.prepare(0.0)?;
    let mut candidate = layout_unknowns(&device, 0.72, 1.2);
    assert!(matches!(
        previous.evaluate(Trial {
            unknowns: &candidate,
            electrothermal: None,
        }),
        Err(EvaluationError::MissingCoupling("electrothermal"))
    ));
    let slot = device
        .layout()
        .thermal_slot()
        .ok_or("missing thermal unknown")?;
    candidate[slot] = 6.0;
    assert!(matches!(
        previous.evaluate(Trial {
            unknowns: &candidate,
            electrothermal: None,
        }),
        Err(EvaluationError::InvalidPhysicalDomain)
    ));
    let next = device.prepare(6.0)?;
    assert!(matches!(
        next.evaluate(Trial {
            unknowns: &candidate,
            electrothermal: None,
        }),
        Err(EvaluationError::MissingCoupling("electrothermal"))
    ));
    assert!(matches!(
        device.prepare(f64::NAN),
        Err(ModelError::InvalidParameter("thermal_rise"))
    ));
    let ordinary = source_model()?.bind(Temperature::new(300.15)?, instance::Options::default())?;
    assert!(matches!(
        ordinary.prepare(1.0),
        Err(ModelError::InvalidParameter("thermal_rise"))
    ));
    // Signed zero is the same physical zero temperature rise.
    ordinary.prepare(-0.0)?;
    Ok(())
}

#[test]
fn resolved_netlist_assignments_bind_directly_to_identical_kernel_artifacts() -> TestResult {
    use model_kernel_bipolar::hicum2::binding::{self, Assignment, Value};
    let source = Provenance {
        format: SourceFormat::Normalized,
        source_sha256: Some([0x47; 32]),
    };
    let card = [
        Assignment {
            name: "level",
            value: Value::Scalar(8.0),
        },
        Assignment {
            name: "VERSION",
            value: Value::Text("2.4.0"),
        },
        Assignment {
            name: "rBx",
            value: Value::Scalar(8.0),
        },
        Assignment {
            name: "RBX",
            value: Value::Scalar(12.0),
        },
        Assignment {
            name: "TYPE",
            value: Value::Text("pNp"),
        },
    ];
    let generated = Model::from_resolved_model_card("npn", Some(8.0), &card, Some(source))?;
    let nominal = binding::model("npn", Some(8.0), &card)?;
    let equivalent = Model::new(&nominal, None)?;
    assert_eq!(generated.identity(), equivalent.identity());
    assert_eq!(generated.provenance(), Some(source));
    Ok(())
}

#[test]
fn resolved_q_instance_options_preserve_prepared_model_identity() -> TestResult {
    use model_kernel_bipolar::hicum2::binding::{Assignment, Value};
    let generated = source_model()?;
    let ic = instance::InitialVoltages {
        vbe: Some(-0.72),
        vce: Some(-1.4),
        vcs: None,
    };
    let options = [
        Assignment {
            name: "area",
            value: Value::Scalar(2.0),
        },
        Assignment {
            name: "m",
            value: Value::Scalar(3.0),
        },
        Assignment {
            name: "temp",
            value: Value::Scalar(40.0),
        },
        Assignment {
            name: "dt",
            value: Value::Scalar(-4.0),
        },
        Assignment {
            name: "off",
            value: Value::Flag,
        },
        Assignment {
            name: "ic",
            value: Value::InitialVoltages(ic),
        },
    ];
    let ambient = Temperature::new(298.15)?;
    let bound = generated.bind_resolved_instance(ambient, &options)?;
    let manual = generated.bind(
        ambient,
        instance::Options {
            area: 2.0,
            multiplicity: 3.0,
            temp_celsius: Some(40.0),
            dt_kelvin: -4.0,
            off: true,
            initial: ic,
        },
    )?;
    assert_eq!(bound.identity(), manual.identity());
    assert_eq!(bound.initial_voltages(), ic);
    assert!(bound.off());
    assert_eq!(
        bound.identity().ambient_kelvin_bits,
        Temperature::new(309.15)?.kelvin().to_bits(),
    );
    assert_eq!(bound.layout(), manual.layout());
    Ok(())
}

#[test]
fn resolved_setter_errors_retain_original_assignment_indices() -> TestResult {
    use model_kernel_bipolar::hicum2::{
        artifact::ArtifactError,
        binding::{self, Assignment, Value},
    };
    let generated = source_model()?;
    let ambient = Temperature::new(298.15)?;
    let error = Model::from_resolved_model_card(
        "npn",
        Some(8.0),
        &[
            Assignment {
                name: "level",
                value: Value::Scalar(8.0),
            },
            Assignment {
                name: "rbx",
                value: Value::Scalar(1.0),
            },
            Assignment {
                name: "nonsense",
                value: Value::Scalar(2.0),
            },
        ],
        None,
    );
    assert!(matches!(
        error,
        Err(ArtifactError::Binding(binding::Error::UnknownParameter {
            assignment: 2,
            ..
        }))
    ));
    let wrong = generated.bind_resolved_instance(
        ambient,
        &[
            Assignment {
                name: "area",
                value: Value::Scalar(2.0),
            },
            Assignment {
                name: "off",
                value: Value::Scalar(1.0),
            },
        ],
    );
    assert!(matches!(
        wrong,
        Err(ArtifactError::Binding(binding::Error::WrongKind {
            assignment: 1,
            ..
        }))
    ));
    Ok(())
}

/// On-demand performance diagnostic, not a pass/fail gate.
/// Run with cargo test --release -p model-kernel-bipolar --test
// hicum2_artifact -- --ignored --nocapture on the target worker.
#[test]
#[ignore = "on-demand native HICUM artifact throughput measurement"]
fn benchmark_prepared_artifact_binding_and_trial_throughput() -> TestResult {
    use std::hint::black_box;
    use std::time::Instant;
    let ambient = Temperature::new(300.15)?;
    let nominal = temperature::Nominal::source_defaults();
    let created = Instant::now();
    for _ in 0..2048 {
        black_box(Model::new(black_box(&nominal), None)?);
    }
    let model_ns = created.elapsed().as_nanos() / 2048;
    let model = Model::new(&nominal, None)?;
    let options = option(2.0, 3.0);
    let bound_at = Instant::now();
    for _ in 0..2048 {
        black_box(model.bind(ambient, black_box(options))?);
    }
    let bind_ns = bound_at.elapsed().as_nanos() / 2048;
    let bound = model.bind(ambient, options)?;
    let prepared_at = Instant::now();
    for _ in 0..2048 {
        black_box(bound.prepare(0.0)?);
    }
    let prepare_ns = prepared_at.elapsed().as_nanos() / 2048;
    let evaluator = bound.prepare(0.0)?;
    let values = layout_unknowns(&bound, 0.73, 1.2);
    let trial = Trial {
        unknowns: &values,
        electrothermal: None,
    };
    for _ in 0..256 {
        black_box(evaluator.evaluate(black_box(trial))?);
    }
    let count = 20_000u128;
    let started = Instant::now();
    for _ in 0..count {
        black_box(evaluator.evaluate(black_box(trial))?);
    }
    let evaluation_ns = started.elapsed().as_nanos() / count;
    let direct = instance::Bound::new(&nominal, ambient, options)?
        .prepare(0.0)?
        .device_kernel()?;
    let baseline = evaluator.evaluate(trial)?;
    let bias = model_kernel_bipolar::hicum2::device::Bias {
        nodes: baseline.operating.nodes,
        nqs: model_kernel_bipolar::hicum2::nqs::State::default(),
    };
    let measured = Instant::now();
    for _ in 0..count {
        black_box(direct.evaluate(black_box(&bias))?);
    }
    let physical_ns = measured.elapsed().as_nanos() / count;
    let operating = direct.evaluate(&bias)?;
    let measured = Instant::now();
    for _ in 0..count {
        black_box(direct.source_stamps(black_box(&operating))?);
    }
    let physical_stamps_ns = measured.elapsed().as_nanos() / count;
    let source = direct.source_stamps(&operating)?;
    let measured = Instant::now();
    for _ in 0..count {
        let partial = bound.layout().reduce(black_box(&source))?;
        black_box(partial.complete_from_operating(&operating, None)?);
    }
    let reduction_ns = measured.elapsed().as_nanos() / count;
    eprintln!(
        "HICUM source stages: device={physical_ns} ns, physical branches={physical_stamps_ns} ns, reduced I/Q/G/C={reduction_ns} ns"
    );
    eprintln!(
        "HICUM source artifact: model={model_ns} ns, instance={bind_ns} ns, temperature prep={prepare_ns} ns, full local trial I/Q/G/C={evaluation_ns} ns; {count} iterations"
    );
    Ok(())
}
