use model_kernel_bipolar::{
    Temperature,
    hicum2::{
        self, device, nqs, stamps, temperature,
        topology::{Branch, Node},
    },
};
#[path = "fixtures/hicum2_baseline.rs"]
mod baseline;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn close(actual: f64, expected: f64, relative: f64, absolute: f64) {
    let tolerance = relative.mul_add(actual.abs().max(expected.abs()), absolute);
    assert!(
        (actual - expected).abs() <= tolerance,
        "actual={actual:.15e},expected={expected:.15e},tol={tolerance:.5e}"
    );
}

const fn model() -> temperature::Nominal {
    let mut p = temperature::Nominal::source_defaults();
    p.transport = baseline::parameters();
    p.transport.flcomp = 2.3;
    p.auxiliary.rbx = 11.0;
    p.auxiliary.rbi0 = 23.0;
    p.auxiliary.rcx = 5.0;
    p.auxiliary.re = 4.0;
    p.auxiliary.rsu = 25.0;
    p.auxiliary.csu = 1e-15;
    p.auxiliary.cbepar = 4e-15;
    p.auxiliary.fbepar = 0.3;
    p.auxiliary.cbcpar = 2e-16;
    p.auxiliary.fbcpar = 0.2;
    p.auxiliary.itss = 1e-17;
    p.auxiliary.tsf = 1e-12;
    p.auxiliary.ibets = 1e-5;
    p.auxiliary.abet = 0.25;
    p.auxiliary.favl = 0.2;
    p.auxiliary.qavl = 1e-15;
    p.auxiliary.fcrbi = 0.2;
    p.auxiliary.flnqs = true;
    p.auxiliary.flsh = 2;
    p.auxiliary.rth = 200.0;
    p.auxiliary.cth = 2e-9;
    p
}

const fn bias() -> device::Bias {
    device::Bias {
        nodes: device::Nodes {
            b: 0.79,
            bp: 0.77,
            bi: 0.76,
            c: 1.23,
            ci: 1.13,
            ei: 0.02,
            e: 0.0,
            si: -0.1,
            s: -0.12,
        },
        nqs: nqs::State {
            current_first: 0.003,
            current_second: 0.005,
            stored_charge: 2e-15,
        },
    }
}

fn move_node(input: &mut device::Bias, node: Node, delta: f64) {
    let n = &mut input.nodes;
    match node {
        Node::C => n.c += delta,
        Node::B => n.b += delta,
        Node::E => n.e += delta,
        Node::S => n.s += delta,
        Node::Ci => n.ci += delta,
        Node::Ei => n.ei += delta,
        Node::Bp => n.bp += delta,
        Node::Bi => n.bi += delta,
        Node::Si => n.si += delta,
    }
}

fn checked(
    kernel: &device::Prepared,
    input: &device::Bias,
) -> Result<(device::Output, stamps::Output), Box<dyn std::error::Error>> {
    let op = kernel.evaluate(input)?;
    let stamps = kernel.source_stamps(&op)?;
    Ok((op, stamps))
}

#[test]
fn all_native_default_collector_currents_reconstructed_from_source_branches() -> TestResult {
    let model = temperature::Nominal::source_defaults();
    let map = model.physical_topology()?;
    assert_eq!(map.count(), 4);
    let csv = include_str!("fixtures/hicum2_ngspice47_source_defaults.csv");
    let mut rows = 0;
    for line in csv.lines().filter(|line| !line.starts_with('#')) {
        let values = line
            .split(',')
            .map(str::parse::<f64>)
            .collect::<Result<Vec<_>, _>>()?;
        assert_eq!(values.len(), 10);
        let prepared = model.prepare(Temperature::new(values[0] + 273.15)?, 0.0)?;
        let evaluator = prepared.device_kernel()?;
        let input = device::Bias {
            nodes: device::Nodes {
                b: values[1],
                bp: values[1],
                bi: values[1],
                c: values[2],
                ci: values[2],
                ei: 0.0,
                e: 0.0,
                si: 0.0,
                s: 0.0,
            },
            nqs: nqs::State::default(),
        };
        let (_, source) = checked(&evaluator, &input)?;
        let collector = map.index(Node::C);
        let mut current = 0.0;
        for branch in Branch::ALL {
            let (from, to) = map.ends(branch);
            if from == collector {
                current += source.branch(branch).current.value;
            }
            if to == collector {
                current -= source.branch(branch).current.value;
            }
        }
        // Published HICUM has four distinct solver-owned Gmin branches.
        // Their native collector terminal contribution for this setup is
        // Gmin*(VBE-3*VBC), separately from the compact-model branches.
        let source_current = 1e-12f64.mul_add(3.0f64.mul_add(values[2], -2.0 * values[1]), current);
        close(source_current, values[5], 1e-4, 1e-20);
        rows += 1;
    }
    assert_eq!(rows, 36);
    Ok(())
}

fn assert_source_resistors(out: &stamps::Output, op: &device::Output) {
    let test = |branch: Branch, current: f64, charge: f64| {
        let actual = out.branch(branch);
        close(actual.current.value, current, 1e-13, 1e-26);
        close(actual.charge.value, charge, 1e-13, 1e-26);
    };
    test(Branch::Rbx, op.external.external_base.amperes, 0.0);
    test(
        Branch::Rbi,
        op.internal_base.resistor_current.value,
        op.internal_base.hf_shunt_charge.value,
    );
    test(Branch::Rcx, op.external.external_collector.amperes, 0.0);
    test(Branch::Re, op.external.emitter.amperes, 0.0);
    test(
        Branch::Rsu,
        op.external.substrate.amperes,
        op.external.substrate_shunt.coulombs,
    );
}

#[test]
fn source_branch_values_reproduce_every_physical_current_and_charge_family() -> TestResult {
    for kind in [hicum2::Polarity::Npn, hicum2::Polarity::Pnp] {
        let mut nominal = model();
        nominal.polarity = kind;
        let prepared = nominal.prepare(Temperature::new(300.15)?, 0.0)?;
        let kernel = prepared.device_kernel()?;
        let (op, stamps) = checked(&kernel, &bias())?;
        let sign = if matches!(kind, hicum2::Polarity::Npn) {
            1.0
        } else {
            -1.0
        };
        let test = |branch: Branch, current: f64, charge: f64| {
            let actual = stamps.branch(branch);
            close(actual.current.value, current, 1e-13, 1e-26);
            close(actual.charge.value, charge, 1e-13, 1e-26);
        };
        assert_source_resistors(&stamps, &op);
        test(
            Branch::IntrinsicCollector,
            sign * op.diode.ibci.amperes,
            sign * (op.junction.intrinsic_collector.coulombs
                + op.transfer.reverse_diffusion_charge.value),
        );
        test(
            Branch::PeripheralCollector,
            sign * op.diode.ijbcx.amperes,
            sign.mul_add(
                op.junction.peripheral_collector.coulombs + op.substrate.diffusion_charge.value,
                op.external.peripheral_collector_isolation.coulombs,
            ),
        );
        test(
            Branch::ExternalCollectorJunction,
            0.0,
            sign.mul_add(
                op.junction.external_collector.coulombs,
                op.external.external_collector_isolation.coulombs,
            ),
        );
        test(
            Branch::IntrinsicEmitter,
            sign * (op.diode.ibei.amperes
                + op.diode.irei.amperes
                + op.transfer.barrier_base_current.value),
            sign * (op.junction.intrinsic_emitter.coulombs + op.nqs.delayed_charge.value),
        );
        test(
            Branch::PeripheralEmitter,
            sign * (op.diode.ibep.amperes + op.diode.irep.amperes - op.tunneling.amperes),
            sign * op.junction.peripheral_emitter.coulombs,
        );
        test(
            Branch::TransferForward,
            sign * op.nqs.delayed_current.value,
            0.0,
        );
        test(
            Branch::TransferReverse,
            sign * op.transfer.reverse_current.value,
            0.0,
        );
        test(
            Branch::SubstrateJunction,
            sign * op.diode.ijsc.amperes,
            sign * op.junction.substrate.coulombs,
        );
        test(
            Branch::PeripheralSubstrateJunction,
            0.0,
            sign * op.junction.peripheral_substrate.coulombs,
        );
        test(
            Branch::SubstrateTransfer,
            sign * op.substrate.current.value,
            0.0,
        );
        test(Branch::Avalanche, sign * op.avalanche.generation.value, 0.0);
        test(
            Branch::ExternalEmitterIsolation,
            0.0,
            op.external.external_emitter_isolation.coulombs,
        );
        test(
            Branch::PeripheralEmitterIsolation,
            0.0,
            op.external.peripheral_emitter_isolation.coulombs,
        );
    }
    Ok(())
}

fn compare_node_derivatives(
    kernel: &device::Prepared,
    input: device::Bias,
    node: Node,
) -> TestResult {
    let (center_op, center) = checked(kernel, &input)?;
    let h = 1e-6;
    let mut low = input;
    let mut high = input;
    move_node(&mut low, node, -h);
    move_node(&mut high, node, h);
    let (lower_op, lower) = checked(kernel, &low)?;
    let (upper_op, upper) = checked(kernel, &high)?;
    let dtf = (upper_op.transfer.forward_transit_time - lower_op.transfer.forward_transit_time)
        / (2.0 * h);
    let index = node.index();
    for branch in Branch::ALL {
        let output = center.branch(branch);
        let up = upper.branch(branch);
        let down = lower.branch(branch);
        let numerical_current = (up.current.value - down.current.value) / (2.0 * h);
        let numerical_charge = (up.charge.value - down.charge.value) / (2.0 * h);
        close(
            output.current.gradient.nodes[index],
            numerical_current,
            4e-3,
            8e-9,
        );
        close(
            output.charge.gradient.nodes[index],
            numerical_charge,
            4e-3,
            6e-18,
        );
    }
    for auxiliary in 0..3 {
        let output = center.auxiliary[auxiliary];
        let up = upper.auxiliary[auxiliary];
        let down = lower.auxiliary[auxiliary];
        let actual = output
            .current
            .gradient
            .d_tf
            .mul_add(dtf, output.current.gradient.nodes[index]);
        close(
            actual,
            (up.current.value - down.current.value) / (2.0 * h),
            6e-3,
            1e-8,
        );
        close(
            output.charge.gradient.nodes[index],
            (up.charge.value - down.charge.value) / (2.0 * h),
            3e-3,
            1e-18,
        );
    }
    let predicted_thermal = center.thermal.current.gradient.nodes[index];
    let observed_thermal = (upper.thermal.current.value - lower.thermal.current.value) / (2.0 * h);
    close(predicted_thermal, observed_thermal, 4e-3, 2e-8);
    assert_eq!(center.thermal.charge.gradient.nodes[index], 0.0);
    // The primitive model Tf is itself bias dependent. Its auxiliary
    // current gradient carries dI/dTf explicitly; node partials at
    // fixed Tf alone must not be misrepresented as a full Jacobian.
    assert!(center_op.transfer.forward_transit_time > 0.0);
    Ok(())
}

fn shift_state(mut input: device::Bias, index: usize, delta: f64) -> device::Bias {
    match index {
        0 => input.nqs.current_first += delta,
        1 => input.nqs.current_second += delta,
        _ => input.nqs.stored_charge += delta,
    }
    input
}

const fn state_derivative(state: nqs::State, index: usize) -> f64 {
    match index {
        0 => state.current_first,
        1 => state.current_second,
        _ => state.stored_charge,
    }
}

fn compare_state_derivatives(
    kernel: &device::Prepared,
    input: device::Bias,
    index: usize,
) -> TestResult {
    let (_, center) = checked(kernel, &input)?;
    let h = if index == 2 { 1e-18 } else { 1e-6 };
    let (_, up) = checked(kernel, &shift_state(input, index, h))?;
    let (_, down) = checked(kernel, &shift_state(input, index, -h))?;
    for branch in Branch::ALL {
        let expected = center.branch(branch);
        let high = up.branch(branch);
        let low = down.branch(branch);
        let numerical = (high.current.value - low.current.value) / (2.0 * h);
        close(
            state_derivative(expected.current.gradient.states, index),
            numerical,
            2e-5,
            1e-8,
        );
        let charge = (high.charge.value - low.charge.value) / (2.0 * h);
        close(
            state_derivative(expected.charge.gradient.states, index),
            charge,
            2e-5,
            1e-12,
        );
    }
    for position in 0..3 {
        let middle = center.auxiliary[position];
        let high = up.auxiliary[position];
        let low = down.auxiliary[position];
        close(
            state_derivative(middle.current.gradient.states, index),
            (high.current.value - low.current.value) / (2.0 * h),
            3e-5,
            1e-8,
        );
        close(
            state_derivative(middle.charge.gradient.states, index),
            (high.charge.value - low.charge.value) / (2.0 * h),
            3e-5,
            1e-12,
        );
    }
    Ok(())
}

#[test]
fn every_branch_and_auxiliary_jacobian_matches_independent_finite_differences() -> TestResult {
    for polarity in [hicum2::Polarity::Npn, hicum2::Polarity::Pnp] {
        for tunnel in [0, 1] {
            for kelvin in [260.0, 300.15, 373.15] {
                let mut nominal = model();
                nominal.polarity = polarity;
                nominal.auxiliary.tunode = tunnel;
                let prepared = nominal.prepare(Temperature::new(kelvin)?, 0.0)?;
                let kernel = prepared.device_kernel()?;
                let mut input = bias();
                if matches!(polarity, hicum2::Polarity::Pnp) {
                    let original = input.nodes;
                    for node in Node::ALL {
                        move_node(
                            &mut input,
                            node,
                            -2.0 * match node {
                                Node::C => original.c,
                                Node::B => original.b,
                                Node::E => original.e,
                                Node::S => original.s,
                                Node::Ci => original.ci,
                                Node::Ei => original.ei,
                                Node::Bp => original.bp,
                                Node::Bi => original.bi,
                                Node::Si => original.si,
                            },
                        );
                    }
                }
                for node in Node::ALL {
                    compare_node_derivatives(&kernel, input, node)?;
                }
                for state in 0..3 {
                    compare_state_derivatives(&kernel, input, state)?;
                }
            }
        }
    }
    Ok(())
}

#[test]
fn all_heating_modes_provide_source_thermal_row_and_charge_contract() -> TestResult {
    for mode in [0, 1, 2] {
        let mut nominal = model();
        nominal.auxiliary.flsh = mode;
        for kelvin in [233.15, 300.15, 373.15] {
            let prepared = nominal.prepare(Temperature::new(kelvin)?, 0.0)?;
            let kernel = prepared.device_kernel()?;
            let (op, source) = checked(&kernel, &bias())?;
            close(
                source.thermal.current.value,
                op.thermal.thermal_current,
                0.0,
                0.0,
            );
            close(
                source.thermal.charge.value,
                op.thermal.heat_charge,
                0.0,
                0.0,
            );
            close(
                source.thermal.heat_capacitance,
                op.thermal.heat_capacitance,
                0.0,
                0.0,
            );
            close(
                source.thermal.d_rise_at_fixed_electrical,
                op.thermal.d_thermal_current_d_rise_at_fixed_electrical,
                0.0,
                0.0,
            );
            for node in Node::ALL {
                compare_node_derivatives(&kernel, bias(), node)?;
            }
            if mode == 0 {
                assert!(op.thermal.shorted);
                assert!(
                    source
                        .thermal
                        .current
                        .gradient
                        .nodes
                        .iter()
                        .all(|value| *value == 0.0)
                );
            } else {
                assert!(!op.thermal.shorted);
                assert!(
                    source
                        .thermal
                        .current
                        .gradient
                        .nodes
                        .iter()
                        .any(|value| value.abs() > 0.0)
                );
            }
        }
    }
    Ok(())
}

#[test]
fn vertical_nqs_inactive_branch_retains_source_auxiliary_constraints() -> TestResult {
    let mut nominal = model();
    nominal.transport.flcomp = 2.3;
    nominal.auxiliary.flnqs = false;
    let prepared = nominal.prepare(Temperature::new(300.15)?, 0.0)?;
    let kernel = prepared.device_kernel()?;
    let (op, stamps) = checked(&kernel, &bias())?;
    assert!(!op.nqs.active);
    close(
        stamps.branch(Branch::TransferForward).current.value,
        op.transfer.forward_current.value,
        0.0,
        0.0,
    );
    close(
        stamps.branch(Branch::IntrinsicEmitter).charge.value,
        op.junction.intrinsic_emitter.coulombs + op.transfer.forward_diffusion_charge.value,
        2e-14,
        0.0,
    );
    for (index, expected) in [
        bias().nqs.current_first,
        bias().nqs.current_second,
        bias().nqs.stored_charge,
    ]
    .into_iter()
    .enumerate()
    {
        close(stamps.auxiliary[index].current.value, expected, 0.0, 0.0);
        assert_eq!(stamps.auxiliary[index].charge.value, 0.0);
    }
    assert_eq!(nominal.physical_topology()?.nqs_state_count(), 0);
    Ok(())
}
