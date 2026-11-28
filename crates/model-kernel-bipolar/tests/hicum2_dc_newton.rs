//! Independent nonlinear DC integration harness for the actual source
//! HICUM branch-derived reduced nodal residual and conductance matrix.
//! The production circuit solver retains global assembly and policies.

use model_kernel_bipolar::{
    Temperature,
    hicum2::{
        device, instance,
        nodal::{self, Layout},
        nqs, temperature,
        topology::Node,
    },
};
type TestResult = Result<(), Box<dyn std::error::Error>>;

fn close(actual: f64, expected: f64, relative: f64, absolute: f64) {
    let limit = relative.mul_add(actual.abs().max(expected.abs()), absolute);
    assert!(
        (actual - expected).abs() <= limit,
        "actual={actual:.15e} expected={expected:.15e} tolerance={limit:.7e}"
    );
}

fn model(variant: &str) -> Result<temperature::Nominal, Box<dyn std::error::Error>> {
    let mut m = temperature::Nominal::source_defaults();
    let scaled = variant.ends_with("_scaled");
    let geometry = variant.strip_suffix("_scaled").unwrap_or(variant);
    m.transport.flcomp = 2.3;
    match geometry {
        "all" => {
            m.auxiliary.rbx = 12.0;
            m.auxiliary.rbi0 = 9.0;
            m.auxiliary.rcx = 6.0;
            m.auxiliary.re = 3.0;
            m.auxiliary.rsu = 4.0;
        }
        "half" => {
            m.auxiliary.rbx = 2.0;
            m.auxiliary.rcx = 4.0;
            m.auxiliary.rsu = 8.0;
        }
        "mostly" => {
            m.auxiliary.rbi0 = 5.0;
            m.auxiliary.re = 3.0;
        }
        "large" => {
            m.auxiliary.rbx = 50.0;
            m.auxiliary.rbi0 = 15.0;
            m.auxiliary.rcx = 10.0;
            m.auxiliary.re = 8.0;
            m.auxiliary.rsu = 5.0;
        }
        _ => return Err(format!("unknown HICUM test model geometry {variant}").into()),
    }
    let options = match variant {
        "all_scaled" => instance::Options {
            area: 2.0,
            multiplicity: 3.0,
            ..instance::Options::default()
        },
        "large_scaled" => instance::Options {
            area: 0.5,
            multiplicity: 4.0,
            ..instance::Options::default()
        },
        _ => instance::Options::default(),
    };
    if scaled {
        Ok(instance::scale(&m, options)?)
    } else {
        Ok(m)
    }
}

fn device_bias(layout: Layout, values: &[f64; 13]) -> device::Bias {
    let voltage = |node| values[layout.node(node)];
    device::Bias {
        nodes: device::Nodes {
            c: voltage(Node::C),
            b: voltage(Node::B),
            e: voltage(Node::E),
            s: voltage(Node::S),
            ci: voltage(Node::Ci),
            bp: voltage(Node::Bp),
            bi: voltage(Node::Bi),
            ei: voltage(Node::Ei),
            si: voltage(Node::Si),
        },
        nqs: nqs::State::default(),
    }
}

fn solve_linear(
    mut matrix: [[f64; 6]; 5],
    count: usize,
) -> Result<[f64; 5], Box<dyn std::error::Error>> {
    for pivot in 0..count {
        let chosen = (pivot..count)
            .max_by(|&a, &b| matrix[a][pivot].abs().total_cmp(&matrix[b][pivot].abs()))
            .ok_or("no pivot candidates")?;
        matrix.swap(chosen, pivot);
        let denominator = matrix[pivot][pivot];
        if denominator.abs() < 1e-30 || !denominator.is_finite() {
            return Err("singular device internal Jacobian".into());
        }
        for value in matrix[pivot].iter_mut().take(count + 1).skip(pivot) {
            *value /= denominator;
        }
        for row in 0..count {
            if row == pivot {
                continue;
            }
            let scale = matrix[row][pivot];
            for col in pivot..=count {
                matrix[row][col] = (-scale).mul_add(matrix[pivot][col], matrix[row][col]);
            }
        }
    }
    let mut result = [0.0; 5];
    for i in 0..count {
        result[i] = matrix[i][count];
    }
    Ok(result)
}

fn residual(
    evaluator: &device::Prepared,
    layout: Layout,
    voltage: &[f64; nodal::MAX_UNKNOWN],
) -> Result<(device::Output, nodal::Complete), Box<dyn std::error::Error>> {
    let bias = device_bias(layout, voltage);
    let op = evaluator.evaluate(&bias)?;
    let sources = evaluator.source_stamps(&op)?;
    let complete = layout
        .reduce(&sources)?
        .complete_from_operating(&op, None)?;
    Ok((op, complete))
}

fn solve(
    nominal: &temperature::Nominal,
    kelvin: f64,
    vbe: f64,
    vce: f64,
) -> Result<(device::Output, nodal::Complete), Box<dyn std::error::Error>> {
    let layout = Layout::new(nominal.physical_topology()?);
    assert_eq!(layout.thermal_slot(), None);
    assert_eq!(layout.nqs_slot(0), None);
    let prepared = nominal.prepare(Temperature::new(kelvin)?, 0.0)?;
    let evaluator = prepared.device_kernel()?;
    let mut voltage = [0.0; nodal::MAX_UNKNOWN];
    voltage[layout.node(Node::C)] = vce;
    voltage[layout.node(Node::B)] = vbe;
    for node in Node::ALL {
        if layout.node(node) >= 4 {
            let external = match node {
                Node::Ci => Node::C,
                Node::Bi | Node::Bp => Node::B,
                Node::Ei => Node::E,
                Node::Si => Node::S,
                other => other,
            };
            voltage[layout.node(node)] = voltage[layout.node(external)];
        }
    }
    let hidden = layout.electrical_count() - 4;
    for _ in 0..100 {
        let (op, system) = residual(&evaluator, layout, &voltage)?;
        let max_residual = system.current[4..4 + hidden]
            .iter()
            .fold(0.0_f64, |max, value| max.max(value.abs()));
        if max_residual <= 1e-14 {
            return Ok((op, system));
        }
        let mut dense = [[0.0; 6]; 5];
        for row in 0..hidden {
            dense[row][hidden] = -system.current[4 + row];
            for col in 0..hidden {
                dense[row][col] = system.conductance[4 + row][4 + col];
            }
        }
        let step = solve_linear(dense, hidden)?;
        let mut fraction = 1.0;
        let mut found = false;
        // This line-search belongs to the *independent test solver*,
        // not to the compact-model source equations.
        for _ in 0..35 {
            let mut trial = voltage;
            for col in 0..hidden {
                trial[4 + col] += (fraction * step[col]).clamp(-0.15, 0.15);
            }
            if let Ok((_, next)) = residual(&evaluator, layout, &trial) {
                let next_res = next.current[4..4 + hidden]
                    .iter()
                    .fold(0.0_f64, |max, value| max.max(value.abs()));
                if next_res < max_residual {
                    voltage = trial;
                    found = true;
                    break;
                }
            }
            fraction *= 0.5;
        }
        if !found {
            return Err("Newton line search did not decrease local KCL residual".into());
        }
    }
    Err("HICUM DC internal Newton failed to converge".into())
}

#[test]
fn native_ngspice47_multi_internal_node_dc_oracle_from_complete_nodal_matrix() -> TestResult {
    let reference = include_str!("fixtures/hicum2_ngspice47_internal_dc.csv");
    let mut cases = 0;
    let mut high_current = 0;
    for line in reference.lines().filter(|line| !line.starts_with('#')) {
        let columns = line.split(',').collect::<Vec<_>>();
        let [
            variant,
            ambient,
            vbe,
            vce,
            measured_emitter_bias,
            measured_collector_bias,
            measured_base_drop,
            measured_collector_current,
            measured_base_current,
            _measured_substrate_current,
        ] = <[&str; 10]>::try_from(columns).map_err(|_| "malformed HICUM DC source fixture")?;
        let to_float = |value: &str| value.parse::<f64>();
        let (vbe, vce) = (to_float(vbe)?, to_float(vce)?);
        let nominal = model(variant)?;
        let (op, solved) = solve(&nominal, to_float(ambient)? + 273.15, vbe, vce)?;
        close(
            op.voltage.vbiei,
            to_float(measured_emitter_bias)?,
            3e-4,
            2e-8,
        );
        close(
            op.voltage.vbici,
            to_float(measured_collector_bias)?,
            3e-4,
            2e-8,
        );
        close(
            op.voltage.vbpbi_raw,
            to_float(measured_base_drop)?,
            3e-4,
            2e-8,
        );
        close(
            solved.current[0],
            to_float(measured_collector_current)?,
            3e-4,
            4e-11,
        );
        close(
            solved.current[1],
            to_float(measured_base_current)?,
            3e-4,
            4e-11,
        );
        high_current += usize::from(op.transfer.net_transfer_current.value > 1e-3);
        cases += 1;
    }
    assert_eq!(cases, 72);
    assert!(high_current > 5);
    Ok(())
}

#[test]
fn pnp_and_npn_reduced_newton_solutions_are_exact_polarity_mirrors() -> TestResult {
    let mut cases = 0;
    for geometry in [
        "all",
        "half",
        "mostly",
        "large",
        "all_scaled",
        "large_scaled",
    ] {
        for kelvin in [233.15, 300.15, 373.15] {
            let npn = model(geometry)?;
            let mut pnp = npn;
            pnp.polarity = model_kernel_bipolar::hicum2::Polarity::Pnp;
            for (vbe, vce) in [(0.62, 0.4), (0.72, 1.2), (0.80, 0.8)] {
                let (npn_op, npn_jac) = solve(&npn, kelvin, vbe, vce)?;
                let (pnp_op, pnp_jac) = solve(&pnp, kelvin, -vbe, -vce)?;
                close(npn_op.voltage.vbiei, pnp_op.voltage.vbiei, 1e-9, 1e-12);
                close(npn_op.voltage.vbici, pnp_op.voltage.vbici, 1e-9, 1e-12);
                close(
                    npn_op.transfer.net_transfer_current.value,
                    pnp_op.transfer.net_transfer_current.value,
                    1e-9,
                    1e-15,
                );
                let count = npn_jac.layout.count();
                assert_eq!(count, pnp_jac.layout.count());
                for row in 0..count {
                    close(npn_jac.current[row], -pnp_jac.current[row], 2e-8, 3e-13);
                    for col in 0..count {
                        close(
                            npn_jac.conductance[row][col],
                            pnp_jac.conductance[row][col],
                            2e-8,
                            3e-13,
                        );
                    }
                }
                cases += 1;
            }
        }
    }
    assert_eq!(cases, 54);
    Ok(())
}
