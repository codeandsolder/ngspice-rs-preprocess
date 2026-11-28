//! Independent LTspice numeric witnesses, with resolved numeric parameters.
use model_kernel_diode::{
    compile_ltspice,
    ltspice_semiconductor::{Instance, PreparedInstance},
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn assignments(text: &str) -> Result<Vec<(&str, f64)>> {
    text.split_whitespace()
        .map(|pair| {
            let (name, value) = pair.split_once('=').ok_or("missing assignment")?;
            Ok((name, value.parse()?))
        })
        .collect()
}

fn cases(text: &str) -> Result<Vec<(&str, PreparedInstance)>> {
    text.lines()
        .map(|row| {
            let mut fields = row.split('|');
            let name = fields.next().ok_or("missing name")?;
            let model = compile_ltspice(assignments(fields.next().ok_or("missing model")?)?)?;
            let mut instance = Instance::default();
            for (key, value) in assignments(fields.next().ok_or("missing instance")?)? {
                match key {
                    "area" => instance.area = value,
                    "m" => instance.multiplier = value,
                    "n" => instance.series = value,
                    "perim" => instance.perimeter = Some(value),
                    "temp" => instance.temperature = value + 273.15,
                    // PJ is a native spelling ignored by this source.
                    "pj" => {}
                    _ => return Err(format!("unknown fixture field {key}").into()),
                }
            }
            Ok((name, model.bind(instance)?))
        })
        .collect()
}

fn close(name: &str, voltage: f64, actual: f64, expected: f64, abs: f64, rel: f64) -> Result<()> {
    if (actual - expected).abs() <= rel.mul_add(expected.abs(), abs) {
        Ok(())
    } else {
        Err(format!("{name} at {voltage}: {actual:e} != {expected:e}").into())
    }
}

fn dc_rows(cases: &[(&str, PreparedInstance)], data: &str, rel: f64) -> Result<usize> {
    let mut count = 0;
    for row in data.lines().skip(1) {
        let mut fields = row.split('\t');
        let voltage: f64 = fields.next().ok_or("missing bias")?.parse()?;
        for ((name, device), expected) in cases.iter().zip(fields) {
            let mut actual = 0.;
            for branch in [device.bulk, device.sidewall].into_iter().flatten() {
                assert_eq!(branch.resistance().to_bits(), 0_f64.to_bits());
                actual += branch.evaluate(voltage)?.total.current;
            }
            let expected = expected.parse()?;
            close(name, voltage, actual, expected, 2e-18, rel)?;
            count += 1;
        }
    }
    Ok(count)
}

#[test]
fn intrinsic_dc() -> Result<()> {
    let cases = cases(include_str!("fixtures/ltspice/intrinsic-cases.tsv"))?;
    assert_eq!(
        dc_rows(
            &cases,
            include_str!("fixtures/ltspice/intrinsic-dc.tsv"),
            3e-8
        )?,
        236 * 15,
    );
    Ok(())
}

#[test]
fn transport_ceiling() -> Result<()> {
    let cases = cases(include_str!("fixtures/ltspice/clip-cases.tsv"))?;
    // The vendor numerically locates the combined forward-law tangent.
    // The reconstructed mathematical root differs by a few ppm.
    assert_eq!(
        dc_rows(&cases, include_str!("fixtures/ltspice/clip-dc.tsv"), 2e-5)?,
        401 * 14,
    );
    Ok(())
}

#[test]
fn intrinsic_ac_charge() -> Result<()> {
    let cases = cases(include_str!("fixtures/ltspice/charge-cases.tsv"))?;
    let mut count = 0;
    for row in include_str!("fixtures/ltspice/charge-ac-compact.tsv").lines() {
        let mut fields = row.split('\t');
        let voltage: f64 = fields.next().ok_or("missing bias")?.parse()?;
        for ((name, device), expected) in cases.iter().zip(fields) {
            if device.independent_sidewall {
                continue;
            }
            let (real, imaginary) = expected.split_once(',').ok_or("missing complex field")?;
            let mut g = 0.;
            let mut c = 0.;
            for branch in [device.bulk, device.sidewall].into_iter().flatten() {
                let evaluated = branch.evaluate(voltage)?.total;
                g += evaluated.conductance;
                c += evaluated.capacitance;
            }
            close(name, voltage, g, real.parse()?, 1e-16, 3e-8)?;
            close(
                name,
                voltage,
                c * std::f64::consts::TAU * 1000.,
                imaginary.parse()?,
                1e-18,
                if device.sidewall.is_some() {
                    1e-6
                } else {
                    3e-8
                },
            )?;
            count += 1;
        }
    }
    assert_eq!(count, 11 * 24);
    Ok(())
}
