use model_kernel_ekv::{Geometry, Model, Polarity};

const GEOMETRY: Geometry = Geometry {
    length: 1e-6,
    width: 10e-6,
    parallel: 1.,
    series: 1.,
};

#[test]
fn reference_dc() -> Result<(), Box<dyn std::error::Error>> {
    let model = Model::reference_defaults(Polarity::N).prepare(GEOMETRY, 300.15)?;
    for line in include_str!("fixtures/reference-dc.tsv").lines().skip(1) {
        let row: Vec<f64> = line
            .split_whitespace()
            .map(str::parse)
            .collect::<Result<_, _>>()?;
        let actual = model.evaluate([1., row[1], 0., 0.])?.current[0];
        let expected = -row[2];
        assert!(
            (actual - expected).abs() <= 2e-7 * expected.abs(),
            "gate={} actual={actual:.12e} expected={expected:.12e}",
            row[1]
        );
    }
    Ok(())
}

#[test]
fn conservation_and_derivatives() -> Result<(), Box<dyn std::error::Error>> {
    for polarity in [Polarity::N, Polarity::P] {
        let model = Model::reference_defaults(polarity).prepare(GEOMETRY, 315.)?;
        let sign = if polarity == Polarity::N { 1. } else { -1. };
        for drain in [-1., -0.1, 0.1, 1., 3.] {
            for gate in [-0.5, 0.1, 0.5, 1., 2.] {
                let v = [drain * sign, gate * sign, 0., -0.2 * sign];
                let p = model.linearize(v)?;
                let sum_i = p.point.current.iter().sum::<f64>();
                let sum_q = p.point.charge.iter().sum::<f64>();
                assert!(sum_i.abs() <= 1e-14);
                assert!(sum_q.abs() <= 1e-27);
                let shifted = model.evaluate(v.map(|x| x + 3.))?;
                for row in 0..4 {
                    assert!((shifted.current[row] - p.point.current[row]).abs() <= 1e-14);
                    assert!((shifted.charge[row] - p.point.charge[row]).abs() <= 1e-27);
                    assert!(p.conductance[row].iter().sum::<f64>().abs() <= 1e-12);
                    assert!(p.capacitance[row].iter().sum::<f64>().abs() <= 1e-25);
                }
                for col in 0..4 {
                    let mut lo = v;
                    let mut hi = v;
                    lo[col] -= 1e-5;
                    hi[col] += 1e-5;
                    let lo = model.evaluate(lo)?;
                    let hi = model.evaluate(hi)?;
                    for row in 0..4 {
                        let conductance = (hi.current[row] - lo.current[row]) / 2e-5;
                        let capacitance = (hi.charge[row] - lo.charge[row]) / 2e-5;
                        assert!(
                            (conductance - p.conductance[row][col]).abs()
                                <= conductance.abs().mul_add(1e-6, 1e-10),
                            "G {v:?} {row} {col}"
                        );
                        assert!(
                            (capacitance - p.capacitance[row][col]).abs()
                                <= capacitance.abs().mul_add(1e-6, 1e-22),
                            "C {v:?} {row} {col}"
                        );
                    }
                }
            }
        }
    }
    Ok(())
}

#[test]
fn reference_linearization() -> Result<(), Box<dyn std::error::Error>> {
    for line in include_str!("fixtures/reference-linearization.tsv")
        .lines()
        .skip(1)
    {
        let row: Vec<&str> = line.split_whitespace().collect();
        let value = |i: usize| row[i].parse::<f64>();
        let polarity = if row[0] == "1" {
            Polarity::N
        } else {
            Polarity::P
        };
        let mut model = Model::reference_defaults(polarity);
        model.mobility_field = value(6)?;
        model.theta = value(7)?;
        let geometry = Geometry {
            length: value(2)?,
            width: value(3)?,
            parallel: value(4)?,
            series: value(5)?,
        };
        let prepared = model.prepare(geometry, value(1)? + 273.15)?;
        let v = [value(8)?, value(9)?, value(10)?, value(11)?];
        let col: usize = row[12].parse()?;
        let linear = prepared.linearize(v)?;
        for r in 0..4 {
            let actual = linear.point.current[r];
            let expected = value(13 + r)?;
            assert!(
                (actual - expected).abs() <= expected.abs().mul_add(2e-7, 1e-24),
                "I {r}: {actual:.12e} != {expected:.12e}, {line}"
            );
            let actual = linear.conductance[r][col];
            let expected = value(17 + 2 * r)?;
            assert!(
                (actual - expected).abs() <= expected.abs().mul_add(2e-7, 1e-18),
                "G {r} {col}: {actual:.12e} != {expected:.12e}, {line}"
            );
            let actual = linear.capacitance[r][col];
            let expected = value(18 + 2 * r)?;
            assert!(
                (actual - expected).abs() <= expected.abs().mul_add(2e-7, 1e-26),
                "C {r} {col}: {actual:.12e} != {expected:.12e}, {line}"
            );
        }
    }
    Ok(())
}

#[test]
fn reference_transient_charge() -> Result<(), Box<dyn std::error::Error>> {
    let model = Model::reference_defaults(Polarity::N).prepare(GEOMETRY, 300.15)?;
    let mut previous: Option<(f64, [f64; 4])> = None;
    let mut first_charge = [0.; 4];
    let mut final_charge = [0.; 4];
    let mut integral = [0.; 4];
    // Skip the initial operating point, which has no ramp displacement.
    for line in include_str!("fixtures/reference-transient.tsv")
        .lines()
        .skip(2)
    {
        let row: Vec<f64> = line
            .split_whitespace()
            .map(str::parse)
            .collect::<Result<_, _>>()?;
        let p = model.evaluate([1., row[1], 0., 0.])?;
        let displacement = std::array::from_fn(|r| -row[2 + r] - p.current[r]);
        if let Some((time, last)) = previous {
            for r in 0..4 {
                integral[r] =
                    (row[0] - time).mul_add(f64::midpoint(last[r], displacement[r]), integral[r]);
            }
        } else {
            first_charge = p.charge;
        }
        final_charge = p.charge;
        previous = Some((row[0], displacement));
    }
    for r in 0..4 {
        let expected = final_charge[r] - first_charge[r];
        assert!(
            (integral[r] - expected).abs() <= expected.abs().mul_add(3e-3, 5e-18),
            "terminal {r}: integrated {:+.12e} != charge change {expected:+.12e}",
            integral[r]
        );
    }
    Ok(())
}
