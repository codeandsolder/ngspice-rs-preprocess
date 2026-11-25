//! Independent branch-resolved `SIMetrix` noise spectra.
use model_kernel_diode::{Instance, PreparedInstance, compile_simetrix};
use std::collections::BTreeMap;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn close(actual: f64, expected: f64) -> Result<()> {
    if (actual - expected).abs() <= 5e-7 * expected.abs() {
        Ok(())
    } else {
        Err(format!("{actual:e} != {expected:e}").into())
    }
}

#[test]
fn vendor_branch_noise_spectra() -> Result<()> {
    let mut models = BTreeMap::<&str, (PreparedInstance, f64)>::new();
    for line in include_str!("fixtures/noise-models.tsv").lines().skip(1) {
        let [name, temp, area, mult, gmin, bias, current, params]: [&str; 8] = line
            .split('\t')
            .collect::<Vec<_>>()
            .try_into()
            .map_err(|_| "model fixture width")?;
        let params = params
            .split_whitespace()
            .map(|param| {
                let (key, value) = param.split_once('=').ok_or("parameter assignment")?;
                Ok((key, value.parse::<f64>()?))
            })
            .collect::<Result<Vec<_>>>()?;
        let device = compile_simetrix(params)?.bind(Instance {
            analysis_temperature: temp.parse::<f64>()? + 273.15,
            area: area.parse()?,
            multiplier: mult.parse()?,
            gmin: gmin.parse()?,
            ..Instance::default()
        })?;
        let junction = device
            .series_resistance()
            .mul_add(-current.parse::<f64>()?, bias.parse()?);
        assert!(models.insert(name, (device, junction)).is_none());
    }
    assert_eq!(models.len(), 36);
    let mut count = 0;
    for line in include_str!("fixtures/noise-spectra.tsv").lines().skip(1) {
        let [name, freq, shot, total, series]: [&str; 5] = line
            .split('\t')
            .collect::<Vec<_>>()
            .try_into()
            .map_err(|_| "spectral fixture width")?;
        let (device, voltage) = models.get(name).ok_or("unknown case")?;
        let frequency = freq.parse::<f64>()?;
        let evaluation = device.evaluate(*voltage)?;
        let noise = device.noise_density(evaluation.current, frequency)?;
        let g = evaluation.conductance;
        let b = std::f64::consts::TAU * frequency * evaluation.capacitance;
        let r = device.series_resistance();
        let denominator = r.mul_add(g, 1.0).hypot(r * b);
        let shot_asd = noise.shot.sqrt() / denominator;
        let series_asd = noise.series_thermal.sqrt() * (r * g.hypot(b)) / denominator;
        let total_asd = ((noise.shot + noise.flicker).sqrt() / denominator).hypot(series_asd);
        close(shot_asd, shot.parse()?)?;
        close(total_asd, total.parse()?)?;
        count += 2;
        if series != "-" {
            close(series_asd, series.parse()?)?;
            count += 1;
        }
    }
    assert_eq!(count, 525);
    Ok(())
}
