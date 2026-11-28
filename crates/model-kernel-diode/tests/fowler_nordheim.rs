//! Independent LTspice reference measurements and analytic range witnesses.
use model_kernel_diode::fowler_nordheim::{Instance, Model, PreparedInstance};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn close(actual: f64, expected: f64, relative: f64, absolute: f64) -> Result<()> {
    if actual.is_finite() && (actual - expected).abs() <= relative.mul_add(expected.abs(), absolute)
    {
        Ok(())
    } else {
        Err(format!("{actual:.17e} != {expected:.17e}").into())
    }
}

fn optional(value: &str) -> Result<Option<f64>> {
    if value == "-" {
        Ok(None)
    } else {
        Ok(Some(value.parse()?))
    }
}

fn config(fields: &[&str]) -> Result<(Model, Instance)> {
    let [tox, ml, mw, il, iw, m, n] = fields else {
        return Err("invalid geometry witness width".into());
    };
    Ok((
        Model {
            oxide_thickness: tox.parse()?,
            length: ml.parse()?,
            width: mw.parse()?,
        },
        Instance {
            length: optional(il)?,
            width: optional(iw)?,
            multiplier: m.parse()?,
            series: n.parse()?,
        },
    ))
}

fn row_error(
    name: &str,
    quantity: &str,
    actual: f64,
    expected: f64,
    rel: f64,
    abs: f64,
) -> Result<()> {
    close(actual, expected, rel, abs).map_err(|error| format!("{name} {quantity}: {error}").into())
}

#[test]
fn measured_dc_sweep() -> Result<()> {
    let diode = Model::default().bind(Instance::default())?;
    let mut count = 0;
    for row in include_str!("fixtures/fowler_nordheim/families-dc-compact.tsv")
        .lines()
        .skip(1)
    {
        let fields = row
            .split_ascii_whitespace()
            .map(str::parse::<f64>)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let [v, measured, repeated, ideal, repeated_ideal] = fields.as_slice() else {
            return Err("invalid DC witness width".into());
        };
        assert_eq!(measured.to_bits(), repeated.to_bits());
        assert_eq!(ideal.to_bits(), repeated_ideal.to_bits());
        let evaluation = diode.evaluate(*v)?;
        row_error(row, "I", evaluation.current, *measured, 2e-12, 0.)?;
        count += 1;
    }
    assert_eq!(count, 181);
    Ok(())
}

#[test]
fn measured_geometry_and_signed_ac() -> Result<()> {
    let mut count = 0;
    for row in include_str!("fixtures/fowler_nordheim/ac.tsv")
        .lines()
        .skip(1)
    {
        let fields: Vec<_> = row.split('\t').collect();
        let [name, voltage, geometry @ .., current, g, b] = fields.as_slice() else {
            return Err("invalid AC witness width".into());
        };
        let (model, instance) = config(geometry)?;
        let g: f64 = g.parse()?;
        let b: f64 = b.parse()?;
        let voltage = voltage.parse::<f64>()?;
        let result = model
            .bind(instance)
            .and_then(|device| device.evaluate(voltage));
        if !g.is_finite() || !b.is_finite() {
            assert!(result.is_err(), "{name}: vendor nonfinite terminal law");
        } else {
            let value = result?;
            if *current != "-" {
                row_error(name, "I", value.current, current.parse()?, 2e-12, 0.)?;
            }
            row_error(name, "g", value.conductance, g, 2e-12, 0.)?;
            row_error(
                name,
                "B",
                std::f64::consts::TAU * 1e3 * value.capacitance,
                b,
                2e-12,
                0.,
            )?;
        }
        count += 1;
    }
    assert_eq!(count, 63);
    Ok(())
}

#[test]
fn measured_noise_with_geometry_and_signed_multiplicity() -> Result<()> {
    let mut count = 0;
    for row in include_str!("fixtures/fowler_nordheim/noise.tsv")
        .lines()
        .skip(1)
    {
        let fields: Vec<_> = row.split('\t').collect();
        let [
            name,
            voltage,
            current,
            geometry @ ..,
            frequency,
            gain,
            noise,
        ] = fields.as_slice()
        else {
            return Err("invalid noise witness width".into());
        };
        let (model, instance) = config(geometry)?;
        let diode = model.bind(instance)?;
        let value = diode.evaluate(voltage.parse()?)?;
        row_error(name, "I", value.current, current.parse()?, 2e-12, 0.)?;
        let frequency: f64 = frequency.parse()?;
        let input: f64 = noise.parse()?;
        let impedance = value
            .conductance
            .hypot(std::f64::consts::TAU * frequency * value.capacitance)
            .recip();
        row_error(name, "gain", impedance, gain.parse()?, 2e-12, 0.)?;
        let density = diode.shot_noise(value.current);
        if input.is_nan() {
            assert!(
                density.is_err(),
                "{name}: negative N must not be made positive"
            );
        } else {
            row_error(name, "noise", density?.sqrt(), input, 2e-12, 0.)?;
        }
        count += 1;
    }
    assert_eq!(count, 59);
    Ok(())
}

#[test]
fn measured_transient_charge_and_current() -> Result<()> {
    let diode = Model {
        oxide_thickness: 50.,
        length: 0.,
        width: 0.,
    }
    .bind(Instance {
        length: Some(2.),
        width: Some(3.),
        multiplier: 2.,
        series: 2.,
    })?;
    let mut count = 0;
    for row in include_str!("fixtures/fowler_nordheim/transient-witness.tsv")
        .lines()
        .skip(1)
    {
        let fields = row
            .split_ascii_whitespace()
            .map(str::parse::<f64>)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let [t, v, current, supply] = fields.as_slice() else {
            return Err("invalid transient witness width".into());
        };
        let value = diode.evaluate(*v)?;
        let slope = if *t < 1. {
            1.
        } else if *t < 2. {
            0.
        } else {
            -1.
        };
        let predicted = value.capacitance.mul_add(slope, value.current);
        row_error(row, "I", predicted, *current, 2e-12, 1e-13)?;
        row_error(row, "supply", predicted, -*supply, 2e-12, 1e-13)?;
        close(value.charge, value.capacitance * v, 2e-14, 0.)?;
        count += 1;
    }
    assert_eq!(count, 72);
    Ok(())
}

#[test]
fn high_precision_scaled_products_keep_representable_outputs() -> Result<()> {
    for row in include_str!("fixtures/fowler_nordheim/extreme.tsv")
        .lines()
        .skip(1)
    {
        let (name, data) = row.split_once('\t').ok_or("missing extreme case name")?;
        let values = data
            .split_ascii_whitespace()
            .map(str::parse::<f64>)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let [t, n, l, w, m, v, i, g, q, c] = values.as_slice() else {
            return Err("invalid extreme witness width".into());
        };
        let diode = Model {
            oxide_thickness: *t,
            length: *l,
            width: *w,
        }
        .bind(Instance {
            multiplier: *m,
            series: *n,
            ..Instance::default()
        })?;
        let value = diode.evaluate(*v)?;
        for (quantity, actual, expected) in [
            ("I", value.current, *i),
            ("g", value.conductance, *g),
            ("Q", value.charge, *q),
            ("C", value.capacitance, *c),
        ] {
            row_error(name, quantity, actual, expected, 3e-12, 0.)?;
        }
    }
    Ok(())
}

fn derivative(diode: PreparedInstance, v: f64) -> Result<()> {
    let h = v.abs() * 1e-5;
    let low = diode.evaluate(v - h)?;
    let high = diode.evaluate(v + h)?;
    let point = diode.evaluate(v)?;
    let difference = (high.current - low.current) / (2. * h);
    close(difference, point.conductance, 2e-7, 0.)?;
    close(
        (high.charge - low.charge) / (2. * h),
        point.capacitance,
        2e-10,
        0.,
    )
}

#[test]
fn exact_zero_and_signed_derivative_domains() -> Result<()> {
    for tox in [-2., 2., 100.] {
        for series in [-2., 0.5, 1.] {
            for multiplier in [-3., 0., 1.] {
                let diode = Model {
                    oxide_thickness: tox,
                    ..Model::default()
                }
                .bind(Instance {
                    series,
                    multiplier,
                    ..Instance::default()
                })?;
                let zero = diode.evaluate(0.)?;
                close(zero.current, 0., 0., 0.)?;
                close(zero.conductance, 0., 0., 0.)?;
                close(zero.charge, 0., 0., 0.)?;
                close(diode.shot_noise(zero.current)?, 0., 0., 0.)?;
                for v in [-10., -4., 4., 10.] {
                    derivative(diode, v)?;
                }
            }
        }
    }
    Ok(())
}
