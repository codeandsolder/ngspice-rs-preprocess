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

fn hidden_rows(p: &Prepared, fix_thermal: bool) -> Vec<usize> {
    let layout = p.layout();
    (4..layout.count())
        .filter(|&row| !fix_thermal || Some(row) != layout.thermal_row())
        .collect()
}

fn solve(p: &Prepared, x: Vec<f64>) -> Result<Vec<f64>, Box<dyn std::error::Error>> {
    solve_local(p, x, false)
}

fn solve_local(
    p: &Prepared,
    mut x: Vec<f64>,
    fix_thermal: bool,
) -> Result<Vec<f64>, Box<dyn std::error::Error>> {
    let n = x.len();
    let hidden = hidden_rows(p, fix_thermal);
    let norm = |currents: &[f64]| {
        hidden
            .iter()
            .map(|&row| currents[row].abs())
            .fold(0.0_f64, f64::max)
    };
    let mut buf = PointBuffer::new(n)?;
    for _ in 0..100 {
        let mut point = buf.point();
        let _ = p.evaluate(&x, &mut point)?;
        let previous_norm = norm(point.current);
        if previous_norm <= 2e-14 {
            return Ok(x);
        }
        let matrix = hidden
            .iter()
            .map(|&r| {
                let mut row: Vec<_> = hidden
                    .iter()
                    .map(|&c| point.conductance[r * n + c])
                    .collect();
                row.push(-point.current[r]);
                row
            })
            .collect();
        let delta = solve_linear(matrix)?;
        let mut fraction = 1.;
        let mut accepted = false;
        for _ in 0..35 {
            let mut trial = x.clone();
            for (&row, step) in hidden.iter().zip(&delta) {
                let limit = if Some(row) == p.layout().thermal_row() {
                    20.
                } else {
                    0.15
                };
                trial[row] += (fraction * step).clamp(-limit, limit);
            }
            if p.evaluate(&trial, &mut point).is_ok() && norm(point.current) < previous_norm {
                x = trial;
                accepted = true;
                break;
            }
            fraction *= 0.5;
        }
        if !accepted {
            return Err(format!("local Newton stalled at {previous_norm:e}").into());
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
    drive: usize,
) -> Result<Vec<[f64; 2]>, Box<dyn std::error::Error>> {
    let width = x.len();
    let rows = hidden_rows(p, true);
    let hidden = rows.len();
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
    for (row, &row_slot) in rows.iter().enumerate() {
        for (col, &col_slot) in rows.iter().enumerate() {
            let [real, imag] = admittance(row_slot, col_slot);
            matrix[row][col] = real;
            matrix[row][col + hidden] = -imag;
            matrix[row + hidden][col] = imag;
            matrix[row + hidden][col + hidden] = real;
        }
        let [real, imag] = admittance(row_slot, drive);
        matrix[row][2 * hidden] = -real;
        matrix[row + hidden][2 * hidden] = -imag;
    }
    let solution = solve_linear(matrix)?;
    let mut result = Vec::new();
    for row in (0..4).chain(p.layout().thermal_row()) {
        let mut value = admittance(row, drive);
        for (col, &slot) in rows.iter().enumerate() {
            let [real, imag] = admittance(row, slot);
            value[0] = real.mul_add(
                solution[col],
                (-imag).mul_add(solution[col + hidden], value[0]),
            );
            value[1] = imag.mul_add(
                solution[col],
                real.mul_add(solution[col + hidden], value[1]),
            );
        }
        result.push(value);
    }
    Ok(result)
}

fn ihp_card() -> Result<Vec<Assignment<'static>>, Box<dyn std::error::Error>> {
    include_str!("fixtures/vbic_ihp_nx4_card.txt")
        .lines()
        .map(|line| {
            let (name, value) = line.split_once('=').ok_or("invalid resolved card")?;
            Ok(scalar(name, value.parse::<f64>()?))
        })
        .collect()
}

#[test]
fn ihp_sg13g2_resolved_card_matches_independent_dc_and_ac() -> TestResult {
    let assignments = ihp_card()?;
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
        let ac = ac_currents(&p, &x, 1e9, 1)?;
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

#[test]
fn ihp_sg13g2_forced_temperature_matches_native_dc_and_ac() -> TestResult {
    let mut assignments = ihp_card()?;
    assignments.extend([
        scalar("rth", 3260.),
        scalar("cth", 1.6e-12),
        scalar("selft", 1.),
    ]);
    let model = binding::model("npn", 9., &assignments)?;
    let instance = binding::instance(&[])?;
    for line in include_str!("fixtures/vbic_ihp_thermal.csv")
        .lines()
        .filter(|line| !line.starts_with('#'))
    {
        let values = line
            .split(',')
            .map(str::parse::<f64>)
            .collect::<Result<Vec<_>, _>>()?;
        let [
            temperature,
            rise,
            base,
            collector,
            substrate,
            ic,
            ib,
            ie,
            is,
            heat,
            ci,
            ac_ic_real,
            ac_ic_imag,
            ac_ib_real,
            ac_ib_imag,
            ac_heat_real,
            ac_heat_imag,
            dc_heat_slope,
        ] = values.as_slice()
        else {
            return Err("invalid IHP thermal oracle row".into());
        };
        let ambient = temperature + 273.15;
        let p = Prepared::new(&model, instance, Temperature::new(ambient)?)?;
        let thermal = p.layout().thermal_row().ok_or("thermal topology missing")?;
        let mut x = initial(p.layout(), 1., [*collector, *base, 0., *substrate]);
        x[thermal] = ambient + rise;
        let x = solve_local(&p, x, true)?;
        let mut buffer = PointBuffer::new(x.len())?;
        let mut point = buffer.point();
        let _ = p.evaluate(&x, &mut point)?;
        eprintln!("IHP electrothermal: {line}");
        for (actual, expected) in point.current.iter().zip([ic, ib, ie, is]) {
            close(*actual, *expected, 1e-5, 3e-13);
        }
        close(point.current[thermal], *heat, 1e-5, 3e-12);
        close(x[p.layout().node(Node::Ci)], *ci, 2e-7, 1e-10);
        let ac = ac_currents(&p, &x, 1e9, thermal)?;
        for (actual, expected) in ac[0]
            .into_iter()
            .chain(ac[1])
            .zip([ac_ic_real, ac_ic_imag, ac_ib_real, ac_ib_imag])
        {
            close(actual, *expected, 2e-4, 1e-10);
        }
        // ngspice 47 stamps the heat-generation derivatives with reversed
        // signs. Preserve the native DC residual and its measured slope.
        let dc = ac_currents(&p, &x, 0., thermal)?;
        close(dc[4][0], *dc_heat_slope, 2e-5, 1e-10);
        // The passive thermal R/C stamp is correct, so reflect the native
        // AC response about that passive admittance.
        close(ac[4][0], 2. / 3260. - ac_heat_real, 2e-4, 1e-10);
        close(
            ac[4][1],
            2. * std::f64::consts::TAU * 1e9 * 1.6e-12 - ac_heat_imag,
            2e-4,
            1e-10,
        );
    }
    Ok(())
}

#[derive(Default)]
struct Spectra(Vec<(String, f64, Vec<model_kernel_api::noise::Injection>)>);

impl model_kernel_api::noise::Sink for Spectra {
    fn push(
        &mut self,
        source: model_kernel_api::noise::Source<'_>,
    ) -> model_kernel_api::Result<()> {
        self.0.push((
            source.name.into(),
            source.density,
            source.injections.to_vec(),
        ));
        Ok(())
    }
}

#[test]
fn shared_artifact_runtime_geometry_and_temperature_batch() -> TestResult {
    use model_kernel_api::{
        Binding, EquationKind,
        batch::{BatchOutput, PreparedTile},
    };
    use model_kernel_bipolar::vbic::shared;
    for heat in [false, true] {
        let model = model("pnp", 11, true, heat)?;
        let ambient = Temperature::new(300.15)?;
        let reference = binding::instance(&[])?;
        let artifact = shared::artifact(&model, ambient, reference)?;
        let template = Prepared::new(&model, reference, ambient)?;
        let layout = template.layout();
        let width = layout.count();
        let descriptor = artifact.descriptor();
        assert_eq!(descriptor.equations.len(), width);
        assert_eq!(descriptor.parameters.len(), 3);
        for (index, equation) in descriptor.equations.iter().enumerate() {
            let thermal = Some(index) == layout.thermal_row();
            assert_eq!(
                equation.binding,
                if index < 4 || thermal {
                    Binding::Terminal
                } else {
                    Binding::Hidden
                }
            );
            assert_eq!(
                equation.kind,
                if thermal {
                    EquationKind::Thermal
                } else if layout
                    .phase_rows()
                    .is_some_and(|rows| rows.contains(&index))
                {
                    EquationKind::Internal
                } else {
                    EquationKind::Electrical
                }
            );
        }
        let candidates: [(&[f64], f64); 3] = [
            (&[1., 1., 0.], 300.15),
            (&[2., 3., 5.], 330.15),
            (&[0.4, 0.5, -3.], 273.15),
        ];
        let tile = PreparedTile::new(&artifact, &candidates)?;
        let mut points = BatchOutput::new(width, candidates.len())?;
        let mut coordinates = Vec::new();
        for (values, temp) in candidates {
            let mut local = initial(layout, -1., [1.2, 0.75, 0., -0.1]);
            if let Some([first, second]) = layout.phase_rows() {
                local[first] = 0.001 * values[0];
                local[second] = 0.002 * values[1];
            }
            if let Some(row) = layout.thermal_row() {
                local[row] = temp + values[2] + 10.;
            }
            coordinates.extend(local);
        }
        tile.evaluate(&coordinates, &mut points)?;
        for (lane, (values, temp)) in candidates.into_iter().enumerate() {
            let instance = artifact.prepare(values, temp)?;
            let x = &coordinates[lane * width..(lane + 1) * width];
            let direct = Prepared::new(
                &model,
                binding::Instance::new(values[0], values[1], None, values[2])?,
                Temperature::new(temp)?,
            )?;
            let mut expected = PointBuffer::new(width)?;
            let mut actual = PointBuffer::new(width)?;
            let mut expected = expected.point();
            let mut actual = actual.point();
            let op = direct.evaluate(x, &mut expected)?;
            artifact.evaluate(&instance, x, &mut actual)?;
            let batch = points.point(lane)?;
            for (individual, reference, vector) in [
                (actual.current, expected.current, batch.current),
                (actual.charge, expected.charge, batch.charge),
                (actual.conductance, expected.conductance, batch.conductance),
                (actual.capacitance, expected.capacitance, batch.capacitance),
            ] {
                assert_eq!(individual, reference);
                assert_eq!(individual, vector);
            }
            let mut sink = Spectra::default();
            artifact.evaluate_noise(&instance, x, 1e6, &mut sink)?;
            let spectrum = direct.noise(op, 1e6)?;
            close(
                sink.0.iter().map(|(_, density, _)| density).sum(),
                spectrum.total_source,
                2e-15,
                0.,
            );
            for (_, density, injections) in sink.0 {
                assert!(density > 0.);
                assert_eq!(injections.len(), 2);
                assert_ne!(injections[0].row, injections[1].row);
                close(injections.iter().map(|i| i.re).sum(), 0., 0., 0.);
                close(injections.iter().map(|i| i.im).sum(), 0., 0., 0.);
            }
        }
    }
    Ok(())
}

#[test]
fn ihp_sg13g2_excess_phase_matches_native_ac() -> TestResult {
    let mut assignments = ihp_card()?;
    assignments.push(scalar("td", 2e-12));
    let model = binding::model("npn", 9., &assignments)?;
    let instance = binding::instance(&[])?;
    for line in include_str!("fixtures/vbic_ihp_phase.csv")
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
            frequency,
            ic,
            ib,
            ie,
            is,
            ci,
            ic_real,
            ic_imag,
            ib_real,
            ib_imag,
        ] = values.as_slice()
        else {
            return Err("invalid IHP phase oracle row".into());
        };
        let p = Prepared::new(&model, instance, Temperature::new(temperature + 273.15)?)?;
        assert!(p.layout().phase_rows().is_some());
        let x = solve(
            &p,
            initial(p.layout(), 1., [*collector, *base, 0., *substrate]),
        )?;
        let mut buffer = PointBuffer::new(x.len())?;
        let mut point = buffer.point();
        let _ = p.evaluate(&x, &mut point)?;
        for (actual, expected) in point.current.iter().zip([ic, ib, ie, is]) {
            close(*actual, *expected, 1e-5, 3e-13);
        }
        close(x[p.layout().node(Node::Ci)], *ci, 2e-7, 1e-10);
        let ac = ac_currents(&p, &x, *frequency, 1)?;
        eprintln!("IHP excess phase: {line}");
        for (actual, expected) in ac[0]
            .into_iter()
            .chain(ac[1])
            .zip([ic_real, ic_imag, ib_real, ib_imag])
        {
            close(actual, *expected, 2e-4, 1e-10);
        }
    }
    Ok(())
}

#[test]
fn ihp_sg13g2_free_thermal_equilibrium_matches_native() -> TestResult {
    let mut assignments = ihp_card()?;
    assignments.extend([
        scalar("rth", 3260.),
        scalar("cth", 1.6e-12),
        scalar("selft", 1.),
    ]);
    let model = binding::model("npn", 9., &assignments)?;
    let instance = binding::instance(&[])?;
    for line in include_str!("fixtures/vbic_ihp_equilibrium.csv")
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
            rise,
            ci,
        ] = values.as_slice()
        else {
            return Err("invalid IHP equilibrium oracle row".into());
        };
        let ambient = temperature + 273.15;
        let p = Prepared::new(&model, instance, Temperature::new(ambient)?)?;
        let thermal = p.layout().thermal_row().ok_or("thermal topology missing")?;
        let mut x = initial(p.layout(), 1., [*collector, *base, 0., *substrate]);
        x[thermal] = ambient;
        let x = solve(&p, x)?;
        let mut buffer = PointBuffer::new(x.len())?;
        let mut point = buffer.point();
        let _ = p.evaluate(&x, &mut point)?;
        eprintln!("IHP free thermal equilibrium: {line}");
        for (actual, expected) in point.current.iter().zip([ic, ib, ie, is]) {
            close(*actual, *expected, 1e-5, 3e-13);
        }
        close(x[p.layout().node(Node::Ci)], *ci, 2e-7, 1e-10);
        close(x[thermal] - ambient, *rise, 1e-5, 1e-8);
        close(point.current[thermal], 0., 0., 2e-14);
    }
    Ok(())
}
