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
