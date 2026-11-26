use model_kernel_bipolar::{
    EvaluationError, Temperature,
    hicum2::{
        self, device,
        nodal::{self, Layout, MAX_UNKNOWN, TemperatureCoupling},
        nqs, stamps, temperature,
        topology::{Branch, Node, Resistor},
    },
};
#[path = "fixtures/hicum2_baseline.rs"]
mod baseline;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn close(actual: f64, expected: f64, relative: f64, absolute: f64) {
    let tolerance = relative.mul_add(actual.abs().max(expected.abs()), absolute);
    assert!(
        (actual - expected).abs() <= tolerance,
        "actual={actual:.15e} expected={expected:.15e} tol={tolerance:.5e}"
    );
}

const fn model() -> temperature::Nominal {
    let mut p = temperature::Nominal::source_defaults();
    p.transport = baseline::parameters();
    p.transport.flcomp = 2.3;
    p.transport.t0 = 5e-12;
    p.auxiliary.flnqs = true;
    p.auxiliary.rbx = 5.0;
    p.auxiliary.rbi0 = 9.0;
    p.auxiliary.rcx = 7.0;
    p.auxiliary.re = 3.0;
    p.auxiliary.rsu = 2.0;
    p
}

const fn input() -> device::Bias {
    device::Bias {
        nodes: device::Nodes {
            b: 0.79,
            bp: 0.78,
            bi: 0.77,
            c: 1.2,
            ci: 1.1,
            ei: 0.02,
            e: 0.0,
            si: -0.1,
            s: -0.12,
        },
        nqs: nqs::State {
            current_first: 0.002,
            current_second: 0.004,
            stored_charge: 2e-15,
        },
    }
}

fn shift(input: &mut device::Bias, node: Node, delta: f64) {
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

fn stamp(
    kernel: &device::Prepared,
    layout: Layout,
    bias: &device::Bias,
) -> Result<(device::Output, nodal::Partials), Box<dyn std::error::Error>> {
    let op = kernel.evaluate(bias)?;
    let branches = kernel.source_stamps(&op)?;
    Ok((op, layout.reduce(&branches)?))
}

#[test]
fn all_32_resistor_short_combinations_reduce_and_conserve_kcl_and_charge() -> TestResult {
    let mut checked = 0;
    for bits in 0_u8..32 {
        let mut p = temperature::Nominal::source_defaults();
        let value = |index: u32| {
            if (bits & (1 << index)) != 0 {
                10.0
            } else {
                0.0
            }
        };
        p.auxiliary.rbx = value(0);
        p.auxiliary.rbi0 = value(1);
        p.auxiliary.rcx = value(2);
        p.auxiliary.re = value(3);
        p.auxiliary.rsu = value(4);
        let layout = Layout::new(p.physical_topology()?);
        assert_eq!(layout.count(), 4 + bits.count_ones() as usize);
        assert_eq!(layout.thermal_slot(), None);
        assert_eq!(layout.nqs_slot(0), None);
        let mut branches = stamps::Output::default();
        for kind in Branch::ALL {
            if kind
                .resistor()
                .is_some_and(|r| !layout.topology().present(r))
            {
                continue;
            }
            let amount = f64::from(u32::try_from(kind.index() + 1)?);
            branches.branches[kind.index()].current.value = amount;
            branches.branches[kind.index()].charge.value = 2.0 * amount;
            branches.branches[kind.index()].current.gradient.nodes[Node::Bi.index()] =
                amount * 0.25;
            branches.branches[kind.index()].charge.gradient.nodes[Node::Ci.index()] =
                amount * 0.125;
        }
        let reduced = layout.reduce(&branches)?;
        let sum_current: f64 = reduced.current[..layout.count()].iter().sum();
        let sum_charge: f64 = reduced.charge[..layout.count()].iter().sum();
        close(sum_current, 0.0, 0.0, 2e-13);
        close(sum_charge, 0.0, 0.0, 4e-13);
        for col in 0..layout.count() {
            let g_sum = (0..layout.count())
                .map(|row| reduced.conductance[row][col])
                .sum::<f64>();
            let c_sum = (0..layout.count())
                .map(|row| reduced.capacitance[row][col])
                .sum::<f64>();
            close(g_sum, 0.0, 0.0, 5e-13);
            close(c_sum, 0.0, 0.0, 5e-13);
        }
        for resistor in Resistor::ALL {
            let (from, to) = resistor.nodes();
            assert_eq!(
                layout.node(from) == layout.node(to),
                !layout.topology().present(resistor)
            );
        }
        checked += 1;
    }
    assert_eq!(checked, 32);
    Ok(())
}

#[test]
fn missing_thermal_or_transit_derivative_is_explicitly_rejected() -> TestResult {
    let mut p = model();
    let layout = Layout::new(p.physical_topology()?);
    assert_eq!(layout.count(), 12);
    for index in 0..3 {
        assert_eq!(layout.nqs_slot(index), Some(9 + index));
    }
    assert_eq!(layout.thermal_slot(), None);
    let prep = p.prepare(Temperature::new(300.15)?, 0.0)?;
    let (_, part) = stamp(&prep.device_kernel()?, layout, &input())?;
    assert!(matches!(
        part.complete(None, None),
        Err(EvaluationError::MissingCoupling("transit_time"))
    ));
    let complete = part.complete(Some([0.0; 9]), None)?;
    assert_eq!(complete.layout.count(), 12);
    assert!(complete.current[..12].iter().all(|x| x.is_finite()));
    assert!(matches!(
        part.complete(
            None,
            Some(TemperatureCoupling {
                current: [0.0; MAX_UNKNOWN],
                charge: [0.0; MAX_UNKNOWN],
                d_tf_d_rise: 0.0,
                d_thermal_current_d_rise_from_power: 0.0,
            })
        ),
        Err(EvaluationError::MissingCoupling("transit_time"))
    ));
    p.auxiliary.flsh = 2;
    p.auxiliary.rth = 200.0;
    let thermal_layout = Layout::new(p.physical_topology()?);
    assert_eq!(thermal_layout.thermal_slot(), Some(12));
    let prepared = p.prepare(Temperature::new(300.15)?, 0.0)?;
    let (_, with_thermal) = stamp(&prepared.device_kernel()?, thermal_layout, &input())?;
    assert!(matches!(
        with_thermal.complete(Some([0.0; 9]), None),
        Err(EvaluationError::MissingCoupling("electrothermal"))
    ));
    let with_coupling = with_thermal.complete(
        Some([0.0; 9]),
        Some(TemperatureCoupling {
            current: [0.0; MAX_UNKNOWN],
            charge: [0.0; MAX_UNKNOWN],
            d_tf_d_rise: 0.0,
            d_thermal_current_d_rise_from_power: 0.0,
        }),
    )?;
    assert!(with_coupling.conductance[12][12] > 0.0);
    assert_eq!(with_coupling.capacitance[12][12], p.auxiliary.cth);
    Ok(())
}

#[test]
fn reduced_full_nqs_newton_jacobian_matches_independent_device_perturbations() -> TestResult {
    let p = model();
    let layout = Layout::new(p.physical_topology()?);
    let prepared = p.prepare(Temperature::new(330.15)?, 0.0)?;
    let kernel = prepared.device_kernel()?;
    let bias = input();
    let (_, center) = stamp(&kernel, layout, &bias)?;
    let mut tf_gradient = [0.0; Node::COUNT];
    let mut perturbed = Vec::new();
    let h = 1e-6;
    for node in Node::ALL {
        let mut low = bias;
        let mut high = bias;
        shift(&mut low, node, -h);
        shift(&mut high, node, h);
        let (low_op, below) = stamp(&kernel, layout, &low)?;
        let (high_op, above) = stamp(&kernel, layout, &high)?;
        tf_gradient[node.index()] = (high_op.transfer.forward_transit_time
            - low_op.transfer.forward_transit_time)
            / (2.0 * h);
        perturbed.push((node, below, above));
    }
    let full = center.complete(Some(tf_gradient), None)?;
    let n = layout.count();
    assert_eq!(n, 12);
    for (node, below, above) in perturbed {
        let col = layout.node(node);
        for row in 0..n {
            close(
                full.conductance[row][col],
                (above.current[row] - below.current[row]) / (2.0 * h),
                8e-3,
                2e-8,
            );
            close(
                full.capacitance[row][col],
                (above.charge[row] - below.charge[row]) / (2.0 * h),
                8e-3,
                1e-17,
            );
        }
    }
    Ok(())
}

#[test]
fn thermal_and_nqs_complete_matrix_matches_full_temperature_trial_differences() -> TestResult {
    let mut nominal = model();
    nominal.auxiliary.flsh = 2;
    nominal.auxiliary.rth = 200.0;
    nominal.auxiliary.cth = 2e-9;
    nominal.coefficients.zetarbi = 0.7;
    nominal.coefficients.zetarbx = 0.5;
    nominal.coefficients.zetare = 0.8;
    nominal.coefficients.zetarcx = -0.3;
    nominal.coefficients.zetarth = 0.2;
    nominal.coefficients.alrth = 1e-4;
    let layout = Layout::new(nominal.physical_topology()?);
    let thermal_row = layout.thermal_slot().ok_or("expected thermal unknown")?;
    let bias = input();
    let ambient = Temperature::new(330.15)?;
    let prepared = nominal.prepare(ambient, 0.0)?;
    let kernel = prepared.device_kernel()?;
    let (middle, source) = stamp(&kernel, layout, &bias)?;
    let h = 1e-2;
    let (below, bottom) = stamp(
        &nominal.prepare(ambient, -h)?.device_kernel()?,
        layout,
        &bias,
    )?;
    let (above, top) = stamp(
        &nominal.prepare(ambient, h)?.device_kernel()?,
        layout,
        &bias,
    )?;
    let tf_rise =
        (above.transfer.forward_transit_time - below.transfer.forward_transit_time) / (2.0 * h);
    let mut source_thermal_current = [0.0; MAX_UNKNOWN];
    let mut source_thermal_charge = [0.0; MAX_UNKNOWN];
    for row in 0..thermal_row {
        source_thermal_current[row] = source.d_current_d_tf[row].mul_add(
            -tf_rise,
            (top.current[row] - bottom.current[row]) / (2.0 * h),
        );
        source_thermal_charge[row] = source.d_charge_d_tf[row]
            .mul_add(-tf_rise, (top.charge[row] - bottom.charge[row]) / (2.0 * h));
    }
    let power_slope = (top.current[thermal_row] - bottom.current[thermal_row]) / (2.0 * h)
        - source.conductance[thermal_row][thermal_row];
    assert!(power_slope.abs() > 1e-15);
    let thermal = TemperatureCoupling {
        current: source_thermal_current,
        charge: source_thermal_charge,
        d_tf_d_rise: tf_rise,
        d_thermal_current_d_rise_from_power: power_slope,
    };
    // Tf gradient wrt named electrical nodes is a separate rank-one
    // correction from the three NQS auxiliary equations.
    let mut tf_named = [0.0; Node::COUNT];
    for node in Node::ALL {
        let mut low = bias;
        let mut high = bias;
        shift(&mut low, node, -1e-6);
        shift(&mut high, node, 1e-6);
        tf_named[node.index()] = (kernel.evaluate(&high)?.transfer.forward_transit_time
            - kernel.evaluate(&low)?.transfer.forward_transit_time)
            / (2e-6);
    }
    let complete = source.complete(Some(tf_named), Some(thermal))?;
    assert_eq!(complete.layout.count(), 13);
    for row in 0..layout.count() {
        close(
            complete.conductance[row][thermal_row],
            (top.current[row] - bottom.current[row]) / (2.0 * h),
            2e-3,
            2e-11,
        );
        close(
            complete.capacitance[row][thermal_row],
            (top.charge[row] - bottom.charge[row]) / (2.0 * h),
            2e-3,
            2e-17,
        );
    }
    close(
        complete.conductance[thermal_row][thermal_row],
        (top.current[thermal_row] - bottom.current[thermal_row]) / (2.0 * h),
        2e-3,
        2e-11,
    );
    close(
        complete.capacitance[thermal_row][thermal_row],
        nominal.auxiliary.cth,
        0.0,
        0.0,
    );
    close(
        middle.thermal.thermal_current,
        complete.current[thermal_row],
        0.0,
        0.0,
    );
    Ok(())
}

// Solve the real-valued 6x6 representation of a 3x3 complex NQS
// small-signal matrix. This deliberately uses *assembled* nodal G and
// C rather than the source NQS closed-form frequency-response function.
fn solve_nqs_ac(
    complete: &nodal::Complete,
    frequency_hz: f64,
    excited: usize,
) -> Result<[(f64, f64); 3], Box<dyn std::error::Error>> {
    let omega = 2.0 * std::f64::consts::PI * frequency_hz;
    let mut rows = [[0.0_f64; 7]; 6];
    for target in 0..3 {
        let r = complete.layout.nqs_slot(target).ok_or("NQS slot absent")?;
        rows[target][6] = -complete.conductance[r][excited];
        rows[target + 3][6] = -omega * complete.capacitance[r][excited];
        for source in 0..3 {
            let c = complete
                .layout
                .nqs_slot(source)
                .ok_or("NQS source absent")?;
            rows[target][source] = complete.conductance[r][c];
            rows[target][source + 3] = -omega * complete.capacitance[r][c];
            rows[target + 3][source] = omega * complete.capacitance[r][c];
            rows[target + 3][source + 3] = complete.conductance[r][c];
        }
    }
    // Dense partial-pivot elimination over only 6 unknowns, independent
    // of any production sparse solver. No external complex-matrix API.
    for pivot in 0..6 {
        let candidate = (pivot..6)
            .max_by(|&a, &b| rows[a][pivot].abs().total_cmp(&rows[b][pivot].abs()))
            .ok_or("missing NQS AC pivot")?;
        rows.swap(candidate, pivot);
        let scale = rows[pivot][pivot];
        if !scale.is_finite() || scale.abs() < 1e-70 {
            return Err("underdetermined NQS AC network".into());
        }
        for value in rows[pivot].iter_mut().skip(pivot) {
            *value /= scale;
        }
        for row in 0..6 {
            if row == pivot {
                continue;
            }
            let multiplier = rows[row][pivot];
            for col in pivot..7 {
                rows[row][col] = (-multiplier).mul_add(rows[pivot][col], rows[row][col]);
            }
        }
    }
    Ok([
        (rows[0][6], rows[3][6]),
        (rows[1][6], rows[4][6]),
        (rows[2][6], rows[5][6]),
    ])
}

#[test]
fn assembled_nodal_ac_network_matches_source_nqs_current_and_charge_transfers() -> TestResult {
    for flcomp in [0.0, 2.1, 2.2, 2.3] {
        for enabled in [false, true] {
            for kelvin in [253.15, 300.15, 373.15] {
                let mut nominal = model();
                nominal.transport.flcomp = flcomp;
                nominal.auxiliary.flnqs = enabled;
                let prepared = nominal.prepare(Temperature::new(kelvin)?, 0.0)?;
                let kernel = prepared.device_kernel()?;
                let layout = Layout::new(nominal.physical_topology()?);
                let mut bias = input();
                let initial = kernel.evaluate(&bias)?;
                let reference = prepared.nqs_kernel()?;
                if layout.nqs_slot(0).is_none() {
                    assert!(!reference.is_active(initial.transfer.forward_transit_time));
                    continue;
                }
                assert!(reference.is_active(initial.transfer.forward_transit_time));
                let tf = initial.transfer.forward_transit_time;
                bias.nqs = nqs::State {
                    current_first: initial.transfer.forward_current.value,
                    current_second: initial.transfer.forward_current.value,
                    stored_charge: initial.transfer.forward_diffusion_charge.value,
                };
                let (equilibrium, partial) = stamp(&kernel, layout, &bias)?;
                // At the exact stationary physical NQS state each source
                // residual multiplying 1/Tf is zero, hence dI/dTf=0.
                assert!(
                    partial.d_current_d_tf[..layout.count()]
                        .iter()
                        .all(|value| value.abs() < 1e-18)
                );
                let complete = partial.complete(None, None)?;
                let norm_current = equilibrium.transfer.forward_current.d_vbiei
                    + equilibrium.transfer.forward_current.d_vbici;
                let norm_charge = equilibrium.transfer.forward_diffusion_charge.d_vbiei
                    + equilibrium.transfer.forward_diffusion_charge.d_vbici;
                assert!(norm_current.abs() > 1e-18);
                assert!(norm_charge.abs() > 1e-25);
                for frequency_hz in [0.0, 1e3, 1e6, 1e9, 1e11] {
                    let solved =
                        solve_nqs_ac(&complete, frequency_hz, complete.layout.node(Node::Bi))?;
                    let source = reference.frequency_response(tf, frequency_hz)?;
                    close(solved[1].0 / norm_current, source.current.real, 2e-8, 1e-11);
                    close(
                        solved[1].1 / norm_current,
                        source.current.imaginary,
                        2e-8,
                        1e-11,
                    );
                    close(solved[2].0 / norm_charge, source.charge.real, 2e-8, 1e-11);
                    close(
                        solved[2].1 / norm_charge,
                        source.charge.imaginary,
                        2e-8,
                        1e-11,
                    );
                }
            }
        }
    }
    Ok(())
}

#[test]
fn analytic_transit_gradient_completes_all_npn_pnp_nqs_matrix_columns() -> TestResult {
    let mut checks = 0;
    for polarity in [hicum2::Polarity::Npn, hicum2::Polarity::Pnp] {
        for geometry in [(0.0, 0.0), (0.2, 0.1), (0.0, 0.2)] {
            for kelvin in [253.15, 300.15, 373.15] {
                let mut nominal = model();
                nominal.polarity = polarity;
                nominal.transport.latb = geometry.0;
                nominal.transport.latl = geometry.1;
                let map = Layout::new(nominal.physical_topology()?);
                let kernel = nominal
                    .prepare(Temperature::new(kelvin)?, 0.0)?
                    .device_kernel()?;
                let mut bias = input();
                if matches!(polarity, hicum2::Polarity::Pnp) {
                    bias.nodes.b = -bias.nodes.b;
                    bias.nodes.bp = -bias.nodes.bp;
                    bias.nodes.bi = -bias.nodes.bi;
                    bias.nodes.c = -bias.nodes.c;
                    bias.nodes.ci = -bias.nodes.ci;
                    bias.nodes.ei = -bias.nodes.ei;
                    bias.nodes.si = -bias.nodes.si;
                    bias.nodes.s = -bias.nodes.s;
                }
                let (op, center) = stamp(&kernel, map, &bias)?;
                let complete = center.complete_from_operating(&op, None)?;
                let predicted = nodal::Partials::transit_time_gradient(
                    polarity,
                    op.transfer.forward_transit_time_gradient,
                );
                assert_eq!(map.count(), 12);
                for node in Node::ALL {
                    let mut lower = bias;
                    let mut upper = bias;
                    let delta = 2e-6;
                    shift(&mut lower, node, -delta);
                    shift(&mut upper, node, delta);
                    let (below, lower_stamps) = stamp(&kernel, map, &lower)?;
                    let (above, upper_stamps) = stamp(&kernel, map, &upper)?;
                    close(
                        predicted[node.index()],
                        (above.transfer.forward_transit_time - below.transfer.forward_transit_time)
                            / (2.0 * delta),
                        3e-3,
                        1e-17,
                    );
                    for row in 0..map.count() {
                        close(
                            complete.conductance[row][map.node(node)],
                            (upper_stamps.current[row] - lower_stamps.current[row]) / (2.0 * delta),
                            1e-2,
                            2e-8,
                        );
                        close(
                            complete.capacitance[row][map.node(node)],
                            (upper_stamps.charge[row] - lower_stamps.charge[row]) / (2.0 * delta),
                            1e-2,
                            1e-17,
                        );
                        checks += 1;
                    }
                }
            }
        }
    }
    assert_eq!(checks, 1944);
    Ok(())
}

fn forced_thermal_model(mode: u8, nqs: bool) -> temperature::Nominal {
    let mut p = temperature::Nominal::source_defaults();
    p.transport.flcomp = 2.3;
    p.transport.t0 = 5e-12;
    p.transport.tr = 1e-12;
    p.transport.junction.intrinsic_emitter.zero_bias_capacitance = 2.5e-15;
    p.transport
        .junction
        .intrinsic_collector
        .zero_bias_capacitance = 1.4e-15;
    p.transport
        .junction
        .peripheral_emitter
        .zero_bias_capacitance = 1e-15;
    p.transport
        .junction
        .external_collector
        .external_zero_bias_capacitance = 0.9e-15;
    p.transport.junction.substrate.zero_bias_capacitance = 0.4e-15;
    p.auxiliary.flsh = mode;
    p.auxiliary.flnqs = nqs;
    p.auxiliary.rth = 200.;
    p.auxiliary.cth = 1e-9;
    p.coefficients.alrth = 0.0003;
    p.coefficients.zetarth = 0.5;
    p.noise.alit = 0.5;
    p.noise.alqf = 0.5;
    p
}

#[test]
fn native_ngspice47_forced_thermal_device_dc_ac() -> TestResult {
    use hicum2::{
        artifact::{Model, Trial},
        instance::Options,
    };
    let mut count = 0;
    for line in include_str!("fixtures/hicum2_ngspice47_device_thermal.csv")
        .lines()
        .filter(|line| !line.starts_with('#'))
    {
        let fields: Vec<_> = line.split(',').collect();
        assert_eq!(fields.len(), 25);
        let number = |i: usize| fields[i].parse::<f64>();
        let ambient = Temperature::new(number(2)? + 273.15)?;
        let rise = number(3)?;
        let model = Model::new(
            &forced_thermal_model(fields[0].parse()?, fields[1] == "1"),
            None,
        )?;
        let instance = model.bind(ambient, Options::default())?;
        let prepared = instance.prepare(rise)?;
        let layout = instance.layout();
        let thermal = layout.thermal_slot().ok_or("thermal row")?;
        let mut x = [0.; MAX_UNKNOWN];
        x[layout.node(Node::C)] = 1.2;
        x[layout.node(Node::B)] = number(4)?;
        x[layout.node(Node::S)] = -0.1;
        x[thermal] = rise;
        let trial = prepared.evaluate(Trial {
            unknowns: &x[..layout.count()],
        })?;
        if trial.operating.nqs.active {
            let transfer = trial.operating.transfer;
            for (state, value) in [
                transfer.forward_current.value,
                transfer.forward_current.value,
                transfer.forward_diffusion_charge.value,
            ]
            .into_iter()
            .enumerate()
            {
                x[layout.nqs_slot(state).ok_or("NQS row")?] = value;
            }
        }
        let frame = prepared.evaluate(Trial {
            unknowns: &x[..layout.count()],
        })?;
        let local = &frame.local;
        for (row, reference) in [0, 1, 2, 3, thermal].into_iter().zip(7..12) {
            let expected = number(reference)?;
            let actual = local.current[row];
            assert!(
                (actual - expected).abs() <= 3e-4 * expected.abs().max(actual.abs()) + 4e-11,
                "DC row {row}: {actual:e} vs {expected:e}; {line}"
            );
        }
        let excited = if fields[5] == "b" {
            layout.node(Node::B)
        } else {
            thermal
        };
        // Compare static derivatives against independent central DC sweeps,
        // not the native AC stamp (which has a cold low-current gm error).
        let static_states = if layout.nqs_slot(0).is_some() {
            solve_nqs_ac(local, 0., excited)?
        } else {
            [(0., 0.); 3]
        };
        for (row, column) in [0, 1, thermal].into_iter().zip(18..21) {
            let mut slope = local.conductance[row][excited];
            for (state, (value, _)) in static_states.into_iter().enumerate() {
                if let Some(col) = layout.nqs_slot(state) {
                    slope += local.conductance[row][col] * value;
                }
            }
            let expected = number(column)?;
            assert!(
                (slope - expected).abs() <= 5e-4 * slope.abs().max(expected.abs()) + 2e-10,
                "DC slope row {row}: {slope:e} vs {expected:e}; {line}"
            );
        }
        let frequency = number(6)?;
        let omega = 2. * std::f64::consts::PI * frequency;
        let states = if layout.nqs_slot(0).is_some() {
            solve_nqs_ac(local, frequency, excited)?
        } else {
            [(0., 0.); 3]
        };
        for (row, reference) in [0, 1, thermal].into_iter().zip([12, 14, 16]) {
            let mut re = local.conductance[row][excited];
            let mut im = omega * local.capacitance[row][excited];
            for (state, (a, b)) in states.into_iter().enumerate() {
                if let Some(col) = layout.nqs_slot(state) {
                    let g = local.conductance[row][col];
                    let c = omega * local.capacitance[row][col];
                    re += g * a - c * b;
                    im += g * b + c * a;
                }
            }
            let mut expected_re = number(reference)?;
            let mut expected_im = number(reference + 1)?;
            // Ngspice 47's analytic small-signal transport slope disagrees
            // with its own cold low-current DC curve. Retain the raw AC
            // samples and correct *only* the independently measured static
            // discrepancy. Transport contributes through the published NQS
            // current transfer, while dissipated power uses undelayed It.
            if fields[2] == "-40"
                && fields[4] == ".65"
                && fields[5] == "b"
                && (row == 0 || row == thermal)
            {
                let index = (reference - 12) / 2;
                let derivative = number(18 + index)?;
                let quasistatic = number(21 + index)?;
                let delta = derivative - quasistatic;
                assert!(
                    delta.abs() > 0.005 * derivative.abs(),
                    "expected documented native AC/DC discrepancy: {line}"
                );
                if row == 0 && fields[1] == "1" {
                    let z = omega * 0.5 * number(24)?;
                    let real = 1. - z * z / 3.;
                    let norm = real * real + z * z;
                    expected_re += delta * real / norm;
                    expected_im -= delta * z / norm;
                } else {
                    expected_re += delta;
                }
            }
            let tol = 3e-3 * re.hypot(im).max(expected_re.hypot(expected_im)) + 2e-10;
            assert!(
                (re - expected_re).hypot(im - expected_im) <= tol,
                "AC row {row}: ({re:e},{im:e}) vs ({expected_re:e},{expected_im:e}); {line}"
            );
        }
        count += 1;
    }
    assert_eq!(count, 384);
    Ok(())
}
