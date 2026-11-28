//! Replay measured vendor DC, complex AC, and direct transient-current data.
use model_kernel_diode::{Instance, PreparedInstance, compile_simetrix};
use std::{collections::BTreeMap, error::Error, path::Path};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
const HEADER: &str = "case\ttemp_c\ttnom_c\tarea\tmult\tgmin\tis\tn\tisr\tnr\teg\txti\tvj\tm\tikf\ttikf\tbv\tibv\tnbv\tibvl\tnbvl\ttbv1\ttbv2\trs\ttrs1\ttrs2\tcjo\tfc\ttt\tbias_v\tload_ohm\tfrequency_hz\tvendor_v\tvendor_i\tvendor_mag\tvendor_phase";

struct Witness<'a> {
    name: &'a str,
    device: PreparedInstance,
    bias: f64,
    load: f64,
    frequency: f64,
    voltage: f64,
    current: f64,
    magnitude: f64,
    phase: f64,
}

fn values<const N: usize>(line: &str) -> Result<[f64; N]> {
    let fields = line
        .split('\t')
        .map(str::parse::<f64>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if !fields.iter().all(|value| value.is_finite()) {
        return Err("nonfinite vendor fixture".into());
    }
    fields
        .try_into()
        .map_err(|fields: Vec<_>| format!("expected {N} numbers, got {}", fields.len()).into())
}

fn witnesses(input: &str) -> Result<Vec<Witness<'_>>> {
    let mut lines = input.lines();
    if lines.next() != Some(HEADER) {
        return Err("invalid vendor fixture header".into());
    }
    lines
        .map(|line| {
            let (name, rest) = line.split_once('\t').ok_or("missing case name")?;
            let [
                temp,
                nominal,
                area,
                multiplier,
                gmin,
                is,
                n,
                isr,
                nr,
                eg,
                xti,
                vj,
                m,
                ikf,
                tikf,
                bv,
                ibv,
                nbv,
                ibvl,
                nbvl,
                tbv1,
                tbv2,
                rs,
                trs1,
                trs2,
                cjo,
                fc,
                tt,
                bias,
                load,
                frequency,
                voltage,
                current,
                magnitude,
                phase,
            ] = values(rest)?;
            let mut parameters = vec![
                ("TNOM", nominal),
                ("IS", is),
                ("N", n),
                ("ISR", isr),
                ("NR", nr),
                ("EG", eg),
                ("XTI", xti),
                ("VJ", vj),
                ("M", m),
                ("RS", rs),
                ("TRS1", trs1),
                ("TRS2", trs2),
                ("CJO", cjo),
                ("FC", fc),
                ("TT", tt),
            ];
            if ikf > 0.0 {
                parameters.extend([("IKF", ikf), ("TIKF", tikf)]);
            }
            if bv > 0.0 {
                parameters.extend([
                    ("BV", bv),
                    ("IBV", ibv),
                    ("NBV", nbv),
                    ("IBVL", ibvl),
                    ("NBVL", nbvl),
                    ("TBV1", tbv1),
                    ("TBV2", tbv2),
                ]);
            }
            let device = compile_simetrix(parameters)?.bind(Instance {
                analysis_temperature: temp + 273.15,
                area,
                multiplier,
                gmin,
                ..Instance::default()
            })?;
            Ok(Witness {
                name,
                device,
                bias,
                load,
                frequency,
                voltage,
                current,
                magnitude,
                phase,
            })
        })
        .collect()
}

#[derive(Default, Debug)]
struct Statistics {
    count: usize,
    peak: f64,
    worst_normalized: f64,
}

#[derive(Default)]
struct Report(BTreeMap<&'static str, Statistics>);

impl Report {
    fn compare(
        &mut self,
        name: &str,
        quantity: &'static str,
        actual: f64,
        expected: f64,
        absolute: f64,
        relative: f64,
    ) -> Result<()> {
        let error = (actual - expected).abs();
        let normalized = error / relative.mul_add(expected.abs(), absolute);
        if !normalized.is_finite() || normalized > 1.0 {
            return Err(format!("{name} {quantity}: actual {actual:e}, vendor {expected:e}, error {error:e}, normalized {normalized}").into());
        }
        let stat = self.0.entry(quantity).or_default();
        stat.count += 1;
        stat.peak = stat.peak.max(error);
        stat.worst_normalized = stat.worst_normalized.max(normalized);
        Ok(())
    }

    fn print(&self) {
        for (name, stat) in &self.0 {
            println!("{name}: {stat:?}");
        }
    }

    #[cfg(test)]
    fn count(&self, name: &str) -> usize {
        self.0.get(name).map_or(0, |stat| stat.count)
    }
}

fn load_line(witness: &Witness<'_>) -> Result<(f64, f64)> {
    let series = witness.device.series_resistance();
    let resistance = witness.load + series;
    let mut voltage = witness.bias;
    if resistance > 0.0 {
        let mut lo = voltage.min(0.0);
        let mut hi = voltage.max(0.0);
        // This is a test load-line solver, not part of the device kernel.
        for _ in 0..100 {
            let mid = lo.midpoint(hi);
            let current = witness.device.evaluate(mid)?.current;
            if resistance.mul_add(current, mid) > witness.bias {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        voltage = lo.midpoint(hi);
    }
    let current = witness.device.evaluate(voltage)?.current;
    Ok((series.mul_add(current, voltage), current))
}

fn replay(input: &str) -> Result<Report> {
    let mut report = Report::default();
    for witness in witnesses(input)? {
        let name = witness.name;
        if witness.frequency == 0.0 {
            let (voltage, current) = load_line(&witness)?;
            if witness.load > 0.0 {
                report.compare(name, "load_voltage", voltage, witness.voltage, 1e-10, 1e-10)?;
                report.compare(name, "load_current", current, witness.current, 2e-13, 1e-8)?;
            } else {
                report.compare(name, "dc_current", current, witness.current, 2e-18, 1e-9)?;
            }
        } else {
            let output = witness.device.evaluate(witness.bias)?;
            let g = output.conductance;
            let susceptance = std::f64::consts::TAU * witness.frequency * output.capacitance;
            let resistance = witness.device.series_resistance();
            let real = resistance.mul_add(g, 1.0);
            let imag = resistance * susceptance;
            let magnitude = g.hypot(susceptance) / real.hypot(imag);
            let phase = (susceptance.atan2(g) - imag.atan2(real)).to_degrees();
            report.compare(
                name,
                "ac_magnitude",
                magnitude,
                witness.magnitude,
                1e-10,
                2e-9,
            )?;
            report.compare(name, "ac_phase_deg", phase, witness.phase, 8e-7, 1e-9)?;
        }
    }
    Ok(report)
}

fn transient(models: &str, samples: &str) -> Result<Report> {
    let models = witnesses(models)?
        .into_iter()
        .map(|w| (w.name, w.device))
        .collect::<BTreeMap<_, _>>();
    let mut lines = samples.lines();
    if lines.next() != Some("case\ttime_s\tjunction_v\tdv_dt_v_per_s\tvendor_current_a") {
        return Err("invalid transient header".into());
    }
    let mut report = Report::default();
    let mut coverage = BTreeMap::new();
    for line in lines {
        let (name, rest) = line.split_once('\t').ok_or("missing transient case")?;
        let [time, voltage, slope, expected] = values(rest)?;
        let device = models.get(name).ok_or("unknown transient model")?;
        let stat = coverage.entry(name).or_insert((f64::NEG_INFINITY, 0usize));
        if time <= stat.0 {
            return Err("nonincreasing time".into());
        }
        stat.0 = time;
        stat.1 += 1;
        let evaluated = device.evaluate(voltage)?;
        let actual = evaluated.capacitance.mul_add(slope, evaluated.current);
        report.compare(name, "transient_current", actual, expected, 1e-9, 0.001)?;
    }
    if coverage.len() != models.len() || coverage.values().any(|(_, count)| *count != 345) {
        return Err("incomplete transient coverage".into());
    }
    Ok(report)
}

fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    while let Some(path) = args.next() {
        if path == "--transient" {
            let models = args.next().ok_or("missing transient models file")?;
            let samples = args.next().ok_or("missing transient currents file")?;
            transient(
                &std::fs::read_to_string(models)?,
                &std::fs::read_to_string(samples)?,
            )?
            .print();
        } else {
            println!("{}", Path::new(&path).display());
            replay(&std::fs::read_to_string(path)?)?.print();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measured_dc_and_ac() -> Result<()> {
        let report = replay(include_str!("../tests/fixtures/vendor-authored.tsv"))?;
        report.print();
        assert_eq!(report.count("dc_current"), 504);
        assert_eq!(report.count("ac_magnitude"), 80);
        assert_eq!(report.count("ac_phase_deg"), 80);
        Ok(())
    }

    #[test]
    fn measured_transient_currents() -> Result<()> {
        let report = transient(
            include_str!("../tests/fixtures/transient-models.tsv"),
            include_str!("../tests/fixtures/transient-current.tsv"),
        )?;
        report.print();
        assert_eq!(report.count("transient_current"), 12_420);
        Ok(())
    }
}
