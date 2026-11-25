//! Independent local Newton solve and source-coordinate Jacobian checks.
use model_kernel_api::PointBuffer;
use model_kernel_bipolar::{
    Temperature,
    vbic::{
        binding::{self, Assignment, Value},
        device::{Layout, Node, Prepared},
    },
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn close(a: f64, b: f64, rel: f64, abs: f64) {
    let limit = rel.mul_add(a.abs().max(b.abs()), abs);
    assert!(
        (a - b).abs() <= limit,
        "{a:.15e} != {b:.15e} (limit {limit:.4e})"
    );
}
fn scalar(name: &str, value: f64) -> Assignment<'_> {
    Assignment {
        name,
        value: Value::Scalar(value),
    }
}
fn model(
    polarity: &str,
    mask: u8,
    phase: bool,
    heat: bool,
) -> Result<binding::Model, binding::Error> {
    let mut values = vec![
        scalar("rci", 3.),
        scalar("rbi", 5.),
        scalar("rbp", 7.),
        scalar("is", 4.81e-17),
        scalar("nf", 1.018),
        scalar("nr", 1.01),
        scalar("vef", 189.),
        scalar("ver", 5.3),
        scalar("ikf", 0.009),
        scalar("ikr", 0.01),
        scalar("ibei", 2e-18),
        scalar("ibci", 1e-16),
        scalar("isp", 1e-16),
        scalar("ibeip", 1e-17),
        scalar("ibcip", 1e-17),
        scalar("cje", 1e-12),
        scalar("cjc", 0.5e-12),
        scalar("cjep", 0.1e-12),
        scalar("cjcp", 0.2e-12),
        scalar("tf", 10e-12),
        scalar("tr", 1e-9),
        scalar("avc1", 0.01),
        scalar("avc2", 10.),
        scalar("gamm", 0.1),
        scalar("vo", 0.5),
        scalar("hrcf", 0.5),
        scalar("td", if phase { 2e-12 } else { 0. }),
    ];
    if heat {
        values.extend([
            scalar("rth", 200.),
            scalar("cth", 2e-10),
            scalar("xrcx", 0.8),
            scalar("xrci", 1.2),
            scalar("xrbx", 1.1),
            scalar("xrbi", 0.9),
            scalar("xrbp", 0.7),
            scalar("xre", 0.5),
            scalar("xrs", 1.0),
            scalar("tnf", 0.001),
            scalar("tavc", 0.002),
            scalar("xikf", 0.7),
            scalar("xvo", 0.5),
            scalar("wbe", 0.6),
            scalar("qco", 3e-16),
        ]);
    }
    for (index, (name, value)) in [("rcx", 2.), ("rbx", 4.), ("re", 1.), ("rs", 6.)]
        .into_iter()
        .enumerate()
    {
        if mask & (1 << index) != 0 {
            values.push(scalar(name, value));
        }
    }
    binding::model(polarity, 9., &values)
}
fn instance() -> Result<binding::Instance, binding::Error> {
    binding::instance(&[
        scalar("area", 2.),
        scalar("m", 3.),
        scalar("temp", 45.),
        scalar("dtemp", 5.),
    ])
}

fn initial(layout: Layout, sign: f64, bias: [f64; 4]) -> Vec<f64> {
    let mut values = vec![0.; layout.count()];
    for node in Node::ALL {
        let source = match node {
            Node::C | Node::Cx | Node::Ci | Node::Bp => 0,
            Node::B | Node::Bx | Node::Bi => 1,
            Node::E | Node::Ei => 2,
            Node::S | Node::Si => 3,
        };
        values[layout.node(node)] = sign * bias[source];
    }
    values
}

fn check_jacobian(p: &Prepared, x: &[f64], context: &str) -> TestResult {
    let layout = p.layout();
    let n = layout.count();
    let mut buf = PointBuffer::new(n)?;
    let mut above = PointBuffer::new(n)?;
    let mut below = PointBuffer::new(n)?;
    let mut expected = buf.point();
    let _ = p.evaluate(x, &mut expected)?;
    let electrical =
        n - layout.phase_rows().map_or(0, |_| 2) - usize::from(layout.thermal_row().is_some());
    close(expected.current[..electrical].iter().sum(), 0., 0., 1e-12);
    close(expected.charge[..electrical].iter().sum(), 0., 0., 1e-22);
    for col in 0..n {
        // Richardson extrapolation controls truncation in the strongly
        // nonlinear epitaxial resistance around its velocity-saturation knee.
        let step = if Some(col) == layout.thermal_row() {
            1e-3
        } else {
            1e-6
        };
        let mut coarse_i = vec![0.; n];
        let mut coarse_q = vec![0.; n];
        for (iteration, h) in [step, step * 0.5].into_iter().enumerate() {
            let mut up = x.to_vec();
            up[col] += h;
            let mut down = x.to_vec();
            down[col] -= h;
            let mut u = above.point();
            let mut d = below.point();
            let _ = p.evaluate(&up, &mut u)?;
            let _ = p.evaluate(&down, &mut d)?;
            for row in 0..n {
                let numeric_i = (u.current[row] - d.current[row]) / (2. * h);
                let numeric_q = (u.charge[row] - d.charge[row]) / (2. * h);
                if iteration == 0 {
                    coarse_i[row] = numeric_i;
                    coarse_q[row] = numeric_q;
                    continue;
                }
                for (quantity, exact, numeric, absolute) in [
                    (
                        "G",
                        expected.conductance[row * n + col],
                        4.0_f64.mul_add(numeric_i, -coarse_i[row]) / 3.0,
                        1e-8,
                    ),
                    (
                        "C",
                        expected.capacitance[row * n + col],
                        4.0_f64.mul_add(numeric_q, -coarse_q[row]) / 3.0,
                        1e-18,
                    ),
                ] {
                    let limit = 3e-5_f64.mul_add(exact.abs().max(numeric.abs()), absolute);
                    assert!(
                        (exact - numeric).abs() <= limit,
                        "{context} row={row} col={col} x={x:?} {quantity}={exact:.15e} finite_difference={numeric:.15e} limit={limit:.4e}"
                    );
                }
            }
        }
    }
    Ok(())
}

#[test]
fn every_electrical_topology_and_phase_jacobian() -> TestResult {
    for (polarity, sign) in [("npn", 1.), ("pnp", -1.)] {
        for mask in 0_u8..16 {
            for phase in [false, true] {
                for heat in [false, true] {
                    let m = model(polarity, mask, phase, heat)?;
                    let p = Prepared::new(&m, instance()?, Temperature::new(300.15)?)?;
                    let layout = p.layout();
                    assert_eq!(
                        layout.count(),
                        7 + mask.count_ones() as usize + 2 * usize::from(phase) + usize::from(heat)
                    );
                    for bias in [
                        [1.2, 0.78, 0., -0.1],
                        [0.1, 0.65, 0., -0.2],
                        [0., -0.2, 0., -0.1],
                    ] {
                        let mut x = initial(layout, sign, bias);
                        for (i, v) in x.iter_mut().enumerate().skip(4) {
                            *v += sign * (i as f64 - 6.) * 0.0001;
                        }
                        if let Some([a, b]) = layout.phase_rows() {
                            x[a] = 1e-4;
                            x[b] = 2e-4;
                        }
                        if let Some(row) = layout.thermal_row() {
                            x[row] = 343.15;
                        }
                        check_jacobian(
                            &p,
                            &x,
                            &format!(
                                "polarity={polarity} mask={mask} phase={phase} heat={heat} bias={bias:?}"
                            ),
                        )?;
                    }
                }
            }
        }
    }
    Ok(())
}

fn solve_linear(mut matrix: Vec<Vec<f64>>) -> Result<Vec<f64>, Box<dyn std::error::Error>> {
    let n = matrix.len();
    for col in 0..n {
        let chosen = (col..n)
            .max_by(|&a, &b| matrix[a][col].abs().total_cmp(&matrix[b][col].abs()))
            .ok_or("empty pivot")?;
        matrix.swap(col, chosen);
        let den = matrix[col][col];
        if den.abs() < 1e-30 {
            return Err("singular matrix".into());
        }
        for v in &mut matrix[col][col..] {
            *v /= den;
        }
        for row in 0..n {
            if row == col {
                continue;
            }
            let s = matrix[row][col];
            for k in col..=n {
                matrix[row][k] = (-s).mul_add(matrix[col][k], matrix[row][k]);
            }
        }
    }
    Ok(matrix.iter().map(|row| row[n]).collect())
}

fn solve(p: &Prepared, mut x: Vec<f64>) -> Result<Vec<f64>, Box<dyn std::error::Error>> {
    let n = x.len();
    let mut buf = PointBuffer::new(n)?;
    for _ in 0..100 {
        let mut point = buf.point();
        let _ = p.evaluate(&x, &mut point)?;
        let norm = point.current[4..]
            .iter()
            .fold(0.0_f64, |a, v| a.max(v.abs()));
        if norm <= 2e-14 {
            return Ok(x);
        }
        let matrix = (4..n)
            .map(|r| {
                let mut row = point.conductance[r * n + 4..(r + 1) * n].to_vec();
                row.push(-point.current[r]);
                row
            })
            .collect();
        let delta = solve_linear(matrix)?;
        let mut fraction = 1.;
        let mut accepted = false;
        for _ in 0..35 {
            let mut trial = x.clone();
            for (v, step) in trial[4..].iter_mut().zip(&delta) {
                *v += (fraction * step).clamp(-0.15, 0.15);
            }
            if p.evaluate(&trial, &mut point).is_ok()
                && point.current[4..]
                    .iter()
                    .fold(0.0_f64, |a, v| a.max(v.abs()))
                    < norm
            {
                x = trial;
                accepted = true;
                break;
            }
            fraction *= 0.5;
        }
        if !accepted {
            return Err(format!("local Newton stalled at {norm:e}").into());
        }
    }
    Err("local Newton iteration limit".into())
}

#[test]
fn native_ngspice_operating_point() -> TestResult {
    let m = model("npn", 15, false, false)?;
    let p = Prepared::new(&m, instance()?, Temperature::new(300.15)?)?;
    let x = solve(&p, initial(p.layout(), 1., [1.2, 0.78, 0., -0.1]))?;
    let mut buffer = PointBuffer::new(x.len())?;
    let mut point = buffer.point();
    let op = p.evaluate(&x, &mut point)?;
    // ngspice-47 independently solved synthetic card with native .options
    // gmin=1e-18 reltol=1e-9 abstol=1e-15 vntol=1e-12.
    for (actual, expected) in point.current.iter().zip([
        4.7612894236328529e-3,
        4.5271076887098971e-4,
        -5.21400019250319643e-3,
        -1.63757896132210590e-15,
    ]) {
        close(*actual, expected, 1e-6, 2e-13);
    }
    close(
        x[p.layout().node(Node::Ci)],
        1.19603224056957669,
        1e-8,
        1e-10,
    );
    let spectrum = p.noise(op, 1e6)?;
    assert!(spectrum.total_source > 0.);
    Ok(())
}

/// Solve the complex small-signal local system by embedding it into a real
/// matrix. Test oracle only; production solver retains its own global matrix.
fn ac_currents(
    p: &Prepared,
    x: &[f64],
    frequency: f64,
) -> Result<[[f64; 2]; 4], Box<dyn std::error::Error>> {
    let width = x.len();
    let hidden = width - 4;
    let mut buffer = PointBuffer::new(width)?;
    let mut point = buffer.point();
    let _ = p.evaluate(x, &mut point)?;
    let omega = std::f64::consts::TAU * frequency;
    let admittance = |row: usize, col: usize| {
        [
            point.conductance[row * width + col],
            omega * point.capacitance[row * width + col],
        ]
    };
    let mut matrix = vec![vec![0.; 2 * hidden + 1]; 2 * hidden];
    for row in 0..hidden {
        for col in 0..hidden {
            let [real, imag] = admittance(row + 4, col + 4);
            matrix[row][col] = real;
            matrix[row][col + hidden] = -imag;
            matrix[row + hidden][col] = imag;
            matrix[row + hidden][col + hidden] = real;
        }
        let [real, imag] = admittance(row + 4, 1);
        matrix[row][2 * hidden] = -real;
        matrix[row + hidden][2 * hidden] = -imag;
    }
    let solution = solve_linear(matrix)?;
    let mut result = [[0.; 2]; 4];
    for (row, value) in result.iter_mut().enumerate() {
        *value = admittance(row, 1);
        for col in 0..hidden {
            let [real, imag] = admittance(row, col + 4);
            value[0] = real.mul_add(
                solution[col],
                (-imag).mul_add(solution[col + hidden], value[0]),
            );
            value[1] = imag.mul_add(
                solution[col],
                real.mul_add(solution[col + hidden], value[1]),
            );
        }
    }
    Ok(result)
}

#[test]
fn ihp_sg13g2_resolved_card_matches_independent_dc_and_ac() -> TestResult {
    let card = include_str!("fixtures/vbic_ihp_nx4_card.txt");
    let assignments = card
        .lines()
        .map(|line| {
            let (name, value) = line.split_once('=').ok_or("invalid resolved card")?;
            Ok(scalar(name, value.parse::<f64>()?))
        })
        .collect::<Result<Vec<_>, Box<dyn std::error::Error>>>()?;
    let model = binding::model("npn", 9., &assignments)?;
    let instance = binding::instance(&[])?;
    for line in include_str!("fixtures/vbic_ihp_device.csv")
        .lines()
        .filter(|line| !line.starts_with('#'))
    {
        let values = line
            .split(',')
            .map(str::parse::<f64>)
            .collect::<Result<Vec<_>, _>>()?;
        let [
            temperature,
            base,
            collector,
            substrate,
            ic,
            ib,
            ie,
            is,
            ci,
            ac_ic_real,
            ac_ic_imag,
            ac_ib_real,
            ac_ib_imag,
        ] = values.as_slice()
        else {
            return Err("invalid IHP oracle row".into());
        };
        let p = Prepared::new(&model, instance, Temperature::new(temperature + 273.15)?)?;
        let x = solve(
            &p,
            initial(p.layout(), 1., [*collector, *base, 0., *substrate]),
        )?;
        let mut buffer = PointBuffer::new(x.len())?;
        let mut point = buffer.point();
        let _ = p.evaluate(&x, &mut point)?;
        eprintln!("IHP: {line}");
        for (actual, expected) in point.current.iter().zip([ic, ib, ie, is]) {
            close(*actual, *expected, 1e-5, 3e-13);
        }
        close(x[p.layout().node(Node::Ci)], *ci, 2e-7, 1e-10);
        let ac = ac_currents(&p, &x, 1e9)?;
        for (actual, expected) in ac[0]
            .into_iter()
            .chain(ac[1])
            .zip([ac_ic_real, ac_ic_imag, ac_ib_real, ac_ib_imag])
        {
            close(actual, *expected, 1e-4, 1e-10);
        }
    }
    Ok(())
}
