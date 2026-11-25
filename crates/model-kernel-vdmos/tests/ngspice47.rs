//! ngspice 47 external differential oracle for the supported channel region.
use model_kernel_vdmos::{Bias, Instance, Polarity, PreparedModel, compile_ngspice};
use model_normalize_ngspice::{
    model_card::parse_model_cards,
    vdmos_card::{VdmosPolarity, vdmos_literals},
};
use std::{error::Error, path::Path, process::Command};
type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn model_from_fixture(fixture: &str) -> Result<(Polarity, PreparedModel)> {
    let file = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(fixture);
    let source = std::fs::read(&file)?;
    let cards = parse_model_cards(&source)?;
    if cards.len() != 1 {
        return Err(format!(
            "{} must have exactly one model card, got {}",
            file.display(),
            cards.len()
        )
        .into());
    }
    let card = &cards[0];
    let extracted = vdmos_literals(card)?
        .ok_or_else(|| format!("{fixture}: non-VDMOS family {}", card.family))?;
    let polarity = match extracted.polarity {
        VdmosPolarity::N => Polarity::N,
        VdmosPolarity::P => Polarity::P,
    };
    Ok((polarity, compile_ngspice(polarity, extracted.parameters)?))
}

fn prepared_from_fixture(polarity: Polarity, fixture: &str) -> Result<PreparedModel> {
    let (actual_polarity, prepared) = model_from_fixture(fixture)?;
    if polarity != actual_polarity {
        return Err(format!("{fixture}: device polarity mismatch").into());
    }
    Ok(prepared)
}

fn run_oracle(fixture: &str) -> Result<String> {
    let file = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(fixture);
    let output = Command::new("ngspice").arg("-b").arg(file).output()?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
    }
    let stdout = String::from_utf8(output.stdout)?;
    if !stdout.contains("ngspice-47 done") {
        return Err(format!("oracle is not ngspice 47: {stdout}").into());
    }
    Ok(stdout)
}

fn oracle(fixture: &str) -> Result<[f64; 3]> {
    let stdout = run_oracle(fixture)?;
    let mut values = [None; 3];
    for line in stdout.lines() {
        if let Some((key, value)) = line.split_once('=') {
            let index = match key.trim() {
                "@m1[id]" => Some(0),
                "@m1[gm]" => Some(1),
                "@m1[gds]" => Some(2),
                _ => None,
            };
            if let Some(i) = index {
                values[i] = Some(value.trim().parse()?);
            }
        }
    }
    let [Some(id), Some(gm), Some(gds)] = values else {
        return Err(format!("missing ngspice values: {stdout}").into());
    };
    Ok([id, gm, gds])
}

fn compare(polarity: Polarity, fixture: &str) -> Result<()> {
    let sign = match polarity {
        Polarity::N => 1.0,
        Polarity::P => -1.0,
    };
    let model = prepared_from_fixture(polarity, fixture)?;
    let device = model.bind(Instance::default())?;
    let evaluated = device.evaluate(Bias {
        vgs: sign * 4.0,
        vds: sign * 0.25,
        temperature: 300.15,
        ..Bias::default()
    })?;
    let [id, gm, gds] = oracle(fixture)?;
    for (kind, actual, expected) in [
        ("Id magnitude", evaluated.channel.value.abs(), id),
        ("gm", evaluated.channel.d_vgs, gm),
        ("gds", evaluated.channel.d_vds, gds),
    ] {
        if (actual - expected).abs() > 2e-5_f64.mul_add(actual.abs().max(expected.abs()), 2e-8) {
            return Err(format!("{fixture} {kind}: Rust {actual:e} != C {expected:e}").into());
        }
    }
    Ok(())
}

#[test]
#[ignore = "requires installed ngspice 47 C reference"]
fn nmos_operating_point() -> Result<()> {
    compare(Polarity::N, "nmos.cir")
}

#[test]
#[ignore = "requires installed ngspice 47 C reference"]
fn pmos_operating_point() -> Result<()> {
    compare(Polarity::P, "pmos.cir")
}

fn oracle_samples<const N: usize>(fixture: &str) -> Result<Vec<[f64; N]>> {
    let stdout = run_oracle(fixture)?;
    let mut samples = Vec::new();
    for line in stdout.lines() {
        let mut columns = line.split_ascii_whitespace();
        let Some(index) = columns.next() else {
            continue;
        };
        if index.parse::<usize>().is_err() {
            continue;
        }
        let mut values = [0.0; N];
        for value in &mut values {
            *value = columns
                .next()
                .ok_or_else(|| format!("incomplete oracle row: {line}"))?
                .parse()?;
        }
        samples.push(values);
    }
    Ok(samples)
}

fn oracle_table<const N: usize>(fixture: &str, expected_rows: usize) -> Result<Vec<[f64; N]>> {
    let samples = oracle_samples::<N>(fixture)?;
    if samples.len() != expected_rows {
        return Err(format!(
            "{fixture}: expected {expected_rows} rows, got {}",
            samples.len()
        )
        .into());
    }
    Ok(samples)
}

fn compare_avalanche(polarity: Polarity, temperature: f64, file: &str) -> Result<()> {
    let prepared = prepared_from_fixture(polarity, file)?;
    let device = prepared.bind(Instance::default())?;
    for [vds, c_current] in oracle_table::<2>(file, 7)? {
        let rust = device.evaluate(Bias {
            vdiode: -vds,
            vds,
            temperature,
            ..Bias::default()
        })?;
        let actual = rust.diode.current.value.abs();
        let expected = c_current.abs();
        if (actual - expected).abs() > 6e-5_f64.mul_add(expected, 1e-18) {
            return Err(
                format!("{file} Vds={vds}: Rust {actual:e} vs ngspice 47 {expected:e}").into(),
            );
        }
    }
    Ok(())
}

#[test]
#[ignore = "requires installed ngspice 47 C reference"]
fn avalanche_nmos_27c_sweep() -> Result<()> {
    compare_avalanche(Polarity::N, 300.15, "avalanche_n.cir")
}

#[test]
#[ignore = "requires installed ngspice 47 C reference"]
fn avalanche_nmos_70c_sweep() -> Result<()> {
    compare_avalanche(Polarity::N, 343.15, "avalanche_n_hot.cir")
}

#[test]
#[ignore = "requires installed ngspice 47 C reference"]
fn avalanche_pmos_27c_sweep() -> Result<()> {
    compare_avalanche(Polarity::P, 300.15, "avalanche_p.cir")
}

fn compare_charge(
    fixture: &str,
    polarity: Polarity,
    temperature: f64,
    soft_recovery: bool,
) -> Result<()> {
    let prepared = prepared_from_fixture(polarity, fixture)?;
    if (prepared.parameters().diode.vp > 0.0) != soft_recovery {
        return Err(format!("{fixture}: VP source mismatch").into());
    }
    let device = prepared.bind(Instance::default())?;
    for [vds, cgd, cds, cgs] in oracle_table::<4>(fixture, 6)? {
        let eval = device.evaluate(Bias {
            vds,
            vdiode: -vds,
            temperature,
            ..Bias::default()
        })?;
        let rust_cds = eval.diode.depletion.d_voltage + eval.diode.diffusion.d_voltage;
        for (parameter, actual, expected) in [
            ("Cgd", eval.qgd.d_voltage, cgd),
            ("Cds", rust_cds, cds),
            ("Cgs", eval.qgs.d_voltage, cgs),
        ] {
            if (actual - expected).abs() > 2e-6_f64.mul_add(expected.abs(), 4e-16) {
                return Err(format!(
                    "{fixture} at Vds={vds} {parameter}: Rust {actual:e} vs ngspice47 {expected:e}"
                )
                .into());
            }
        }
    }
    Ok(())
}

#[test]
#[ignore = "requires installed ngspice 47 C reference"]
fn gate_and_diode_charge_sweep() -> Result<()> {
    compare_charge("charge_n.cir", Polarity::N, 300.15, false)
}

#[test]
#[ignore = "requires installed ngspice 47 C reference"]
fn gate_and_depletion_with_soft_recovery_sweep() -> Result<()> {
    compare_charge("charge_recovery_n.cir", Polarity::N, 300.15, true)
}

#[test]
#[ignore = "requires installed ngspice 47 C reference"]
fn temperature_adjusted_charge_sweep() -> Result<()> {
    compare_charge("charge_n_hot.cir", Polarity::N, 343.15, false)
}

#[test]
#[ignore = "requires installed ngspice 47 C reference"]
fn p_channel_gate_and_diode_charge_sweep() -> Result<()> {
    compare_charge("charge_p.cir", Polarity::P, 300.15, false)
}

fn thermal_operating_point(fixture: &str) -> Result<[f64; 3]> {
    let stdout = run_oracle(fixture)?;
    let mut values = [None; 3];
    for line in stdout.lines() {
        if let Some((key, value)) = line.split_once('=') {
            let index = match key.trim() {
                "v(junction)" => Some(0),
                "v(case)" => Some(1),
                "@m1[id]" => Some(2),
                _ => None,
            };
            if let Some(i) = index {
                values[i] = Some(value.trim().parse()?);
            }
        }
    }
    let [Some(junction), Some(case), Some(id)] = values else {
        return Err(format!("missing thermal operating-point values: {stdout}").into());
    };
    Ok([junction, case, id])
}

fn compare_thermal_dc(polarity: Polarity, fixture: &str) -> Result<()> {
    let prepared = prepared_from_fixture(polarity, fixture)?;
    let device = prepared.bind(Instance {
        multiplier: 1.0,
        self_heating: true,
    })?;
    let sign = match polarity {
        Polarity::N => 1.0,
        Polarity::P => -1.0,
    };
    let ambient_kelvin = 300.15;
    let mut temperature = ambient_kelvin;
    for _ in 0..12 {
        let eval = device.evaluate(Bias {
            vgs: sign * 4.0,
            vds: sign * 0.3,
            temperature,
            ..Bias::default()
        })?;
        let thermal_conductance = eval.heat.junction_to_case * eval.heat.case_to_ambient
            / (eval.heat.junction_to_case + eval.heat.case_to_ambient);
        let residual =
            (temperature - ambient_kelvin).mul_add(thermal_conductance, -eval.heat.intrinsic);
        temperature -= residual / (thermal_conductance - eval.heat.d_temperature);
    }
    let eval = device.evaluate(Bias {
        vgs: sign * 4.0,
        vds: sign * 0.3,
        temperature,
        ..Bias::default()
    })?;
    let case_celsius = 27.0 + eval.heat.intrinsic / eval.heat.case_to_ambient;
    let [junction_c, case_c, id] = thermal_operating_point(fixture)?;
    for (name, actual, reference) in [
        ("junction temperature C", temperature - 273.15, junction_c),
        ("case temperature C", case_celsius, case_c),
        ("channel current A", eval.channel.value.abs(), id),
    ] {
        if (actual - reference).abs() > 4e-6_f64.mul_add(reference.abs(), 1e-4) {
            return Err(
                format!("{fixture} {name}: Rust {actual:e} vs ngspice47 {reference:e}").into(),
            );
        }
    }
    Ok(())
}

#[test]
#[ignore = "requires installed ngspice 47 C reference"]
fn nmos_self_heating_dc_equilibrium() -> Result<()> {
    compare_thermal_dc(Polarity::N, "thermal_dc_n.cir")
}

#[test]
#[ignore = "requires installed ngspice 47 C reference"]
fn pmos_self_heating_dc_equilibrium() -> Result<()> {
    compare_thermal_dc(Polarity::P, "thermal_dc_p.cir")
}

// The prescribed ideal voltage is independent of model and circuit history.
// The RK4 integration below is test-only: the runtime kernel deliberately
// neither integrates states nor implements an electrical circuit solver.
fn recovery_voltage(time: f64) -> f64 {
    if time <= 200e-9 {
        0.55
    } else if time >= 205e-9 {
        -0.1
    } else {
        (time - 200e-9).mul_add(-0.65 / 5e-9, 0.55)
    }
}

fn recovery_voltage_rate(time: f64) -> f64 {
    if (200e-9..205e-9).contains(&time) {
        -0.65 / 5e-9
    } else {
        0.0
    }
}

/// Test-only integrator for scalar transient states at imposed terminal
/// voltages. Exact breakpoints avoid integrating across PWL corner kinks.
fn integrate_piecewise_rk4(
    value: &mut f64,
    from: f64,
    to: f64,
    breaks: &[f64],
    max_step: f64,
    rate: impl Fn(f64, f64) -> Result<f64>,
) -> Result<()> {
    let mut time = from;
    for &end in breaks.iter().chain(std::iter::once(&to)) {
        if end <= time || end > to {
            continue;
        }
        loop {
            let remaining = end - time;
            if remaining <= 1e-18 {
                break;
            }
            let dt = remaining.min(max_step);
            let halfway = dt.mul_add(0.5, time);
            let k1 = rate(time, *value)?;
            let k2 = rate(halfway, (dt * 0.5).mul_add(k1, *value))?;
            let k3 = rate(halfway, (dt * 0.5).mul_add(k2, *value))?;
            let k4 = rate(time + dt, dt.mul_add(k3, *value))?;
            let slope_sum = 2.0f64.mul_add(k3, 2.0f64.mul_add(k2, k1) + k4);
            *value = (dt / 6.0).mul_add(slope_sum, *value);
            time += dt;
        }
    }
    Ok(())
}

fn oracle_transient(fixture: &str) -> Result<Vec<[f64; 4]>> {
    let samples = oracle_samples::<4>(fixture)?;
    if samples.len() < 250 {
        return Err(format!("incomplete ngspice transient: only {} rows", samples.len()).into());
    }
    Ok(samples)
}

fn compare_recovery_transient(polarity: Polarity, vp: f64, fixture: &str) -> Result<()> {
    let prepared = prepared_from_fixture(polarity, fixture)?;
    if (vp - prepared.parameters().diode.vp).abs() > 1e-15 {
        return Err(format!("{fixture}: VP parameter mismatch").into());
    }
    let device = prepared.bind(Instance::default())?;
    let sign = if matches!(polarity, Polarity::N) {
        1.0
    } else {
        -1.0
    };
    let at = |time: f64, qp: f64| {
        device.evaluate(Bias {
            vdiode: sign * recovery_voltage(time),
            vds: -sign * recovery_voltage(time),
            recovery_node: qp,
            temperature: 300.15,
            ..Bias::default()
        })
    };
    let tt = prepared.parameters().diode.tt;
    let initial = at(0.0, 0.0)?;
    let mut qp = tt * sign * initial.diode.current.value;
    let mut previous_time = 0.0;
    let mut checked_charges = 0;
    let mut checked_currents = 0;
    for [time, external_vds, qdio_c, idio_c] in oracle_transient(fixture)? {
        if time > 700e-9 + 1e-15 || time < previous_time {
            return Err(format!("invalid ngspice time {time}").into());
        }
        let expected_vds = -sign * recovery_voltage(time);
        if (expected_vds - external_vds).abs() > 2e-5 {
            return Err(format!("{fixture}: voltage source mismatch at {time:e}").into());
        }
        if vp > 0.0 {
            integrate_piecewise_rk4(
                &mut qp,
                previous_time,
                time,
                &[200e-9, 205e-9],
                0.1e-9,
                |t, q| {
                    let id = sign * at(t, q)?.diode.current.value;
                    Ok(tt.mul_add(id, -q) / (tt * vp))
                },
            )?;
        }
        previous_time = time;
        if time < 1e-11 {
            continue;
        } // ngspice initializes qdio=0 at time zero
        let evaluation = at(time, qp)?;
        let junction_current = sign * evaluation.diode.current.value;
        let depletion_charge = sign * evaluation.diode.depletion.value;
        let expected_charge = if vp > 0.0 {
            (1.0 - vp).mul_add(qp, depletion_charge)
        } else {
            sign.mul_add(evaluation.diode.diffusion.value, depletion_charge)
        };
        let tolerance = 2e-12_f64.max(expected_charge.abs() * 0.008);
        if (expected_charge - qdio_c).abs() > tolerance {
            return Err(format!(
                "{fixture} Qdio at t={time:e}: Rust {expected_charge:e}, C {qdio_c:e}, diff {:e}",
                expected_charge - qdio_c
            )
            .into());
        }
        checked_charges += 1;
        // Compare current only in the constant-bias windows: ngspice's
        // charge integration on the steep 5 ns ramp intentionally depends on
        // its time integration and voltage limiting algorithms.
        if time < 195e-9 || time > 210e-9 {
            let iq = if vp > 0.0 {
                (1.0 - vp) * tt.mul_add(junction_current, -qp) / (tt * vp)
            } else {
                0.0
            };
            let current = evaluation
                .diode
                .depletion
                .d_voltage
                .mul_add(recovery_voltage_rate(time), junction_current + iq);
            if (current - idio_c).abs() > 1e-5_f64.max(current.abs() * 0.01) {
                return Err(format!(
                    "{fixture} Idio at t={time:e}: Rust {current:e}, C {idio_c:e}"
                )
                .into());
            }
            checked_currents += 1;
        }
    }
    if checked_charges < 250 || checked_currents < 200 {
        return Err(
            format!("too little transient coverage: {checked_charges}/{checked_currents}").into(),
        );
    }
    Ok(())
}

#[test]
#[ignore = "requires installed ngspice 47 C reference"]
fn nmos_reverse_recovery_full_transient() -> Result<()> {
    compare_recovery_transient(Polarity::N, 0.4, "recovery_transient_n.cir")
}

#[test]
#[ignore = "requires installed ngspice 47 C reference"]
fn pmos_reverse_recovery_full_transient() -> Result<()> {
    compare_recovery_transient(Polarity::P, 0.4, "recovery_transient_p.cir")
}

#[test]
#[ignore = "requires installed ngspice 47 C reference"]
fn nmos_ordinary_diffusion_full_transient() -> Result<()> {
    compare_recovery_transient(Polarity::N, 0.0, "recovery_transient_ordinary.cir")
}

fn thermal_gate_voltage(time: f64) -> f64 {
    if time <= 20e-6 || time >= 401e-6 {
        0.0
    } else if time < 21e-6 {
        (time - 20e-6) * (4.0 / 1e-6)
    } else if time <= 400e-6 {
        4.0
    } else {
        (time - 400e-6).mul_add(-(4.0 / 1e-6), 4.0)
    }
}

fn compare_thermal_transient(polarity: Polarity, fixture: &str) -> Result<()> {
    let sign = if matches!(polarity, Polarity::N) {
        1.0
    } else {
        -1.0
    };
    let prepared = prepared_from_fixture(polarity, fixture)?;
    let device = prepared.bind(Instance {
        multiplier: 1.0,
        self_heating: true,
    })?;
    let at = |time: f64, temperature: f64| {
        device.evaluate(Bias {
            vgs: sign * thermal_gate_voltage(time),
            vds: sign * 0.3,
            vds_external: sign * 0.3,
            temperature,
            ..Bias::default()
        })
    };
    let ambient = 300.15;
    let net = |time: f64, temperature: f64| -> Result<f64> {
        let result = at(time, temperature)?;
        let conductance = result.heat.junction_to_case * result.heat.case_to_ambient
            / (result.heat.junction_to_case + result.heat.case_to_ambient);
        Ok(
            conductance.mul_add(-(temperature - ambient), result.heat.intrinsic)
                / result.heat.junction_capacitance,
        )
    };
    let samples = oracle_samples::<5>(fixture)?;
    if samples.len() < 800 {
        return Err(format!(
            "{fixture}: missing thermal transient rows: {}",
            samples.len()
        )
        .into());
    }
    let mut temperature = ambient;
    let mut previous_time = 0.0;
    let mut checked = 0;
    for [time, v_gate_c, tj_c, tc_c, id_c] in samples {
        if time > 800e-6 + 1e-12 || time < previous_time {
            return Err(format!("invalid thermal transient time {time}").into());
        }
        if sign.mul_add(-thermal_gate_voltage(time), v_gate_c).abs() > 5e-4 {
            return Err(format!("{fixture}: gate source mismatch at {time:e}").into());
        }
        integrate_piecewise_rk4(
            &mut temperature,
            previous_time,
            time,
            &[20e-6, 21e-6, 400e-6, 401e-6],
            0.2e-6,
            net,
        )?;
        previous_time = time;
        let evaluation = at(time, temperature)?;
        let thermal = prepared.parameters().thermal;
        let case =
            ambient + (temperature - ambient) * thermal.rthca / (thermal.rthjc + thermal.rthca);
        for (label, actual, reference, absolute_tol, relative_tol) in [
            (
                "junction Celsius",
                temperature - 273.15,
                tj_c,
                0.03_f64,
                3e-4_f64,
            ),
            ("case Celsius", case - 273.15, tc_c, 0.03, 3e-4),
            (
                "channel current",
                evaluation.channel.value.abs(),
                id_c,
                0.015,
                0.004,
            ),
        ] {
            if (actual - reference).abs()
                > relative_tol.mul_add(actual.abs().max(reference.abs()), absolute_tol)
            {
                return Err(format!(
                    "{fixture} at t={time:e}, {label}: Rust {actual:e} vs ngspice47 {reference:e}"
                )
                .into());
            }
        }
        checked += 1;
    }
    if checked < 800 {
        return Err("insufficient thermal samples".into());
    }
    Ok(())
}

#[test]
#[ignore = "requires installed ngspice 47 C reference"]
fn nmos_electrothermal_on_off_full_transient() -> Result<()> {
    compare_thermal_transient(Polarity::N, "thermal_transient_n.cir")
}

#[test]
#[ignore = "requires installed ngspice 47 C reference"]
fn pmos_electrothermal_on_off_full_transient() -> Result<()> {
    compare_thermal_transient(Polarity::P, "thermal_transient_p.cir")
}

fn compare_gate_transient(polarity: Polarity, fixture: &str) -> Result<()> {
    let sign = if matches!(polarity, Polarity::N) {
        1.0
    } else {
        -1.0
    };
    let model = prepared_from_fixture(polarity, fixture)?;
    let device = model.bind(Instance::default())?;
    let base = device.evaluate(Bias {
        vgs: 0.0,
        vds: sign * 0.3,
        temperature: 300.15,
        ..Bias::default()
    })?;
    let mut count = 0;
    let mut switching = 0;
    let mut initial_c: Option<[f64; 2]> = None;
    for [time, vgate, drain_charge, source_charge] in oracle_samples::<4>(fixture)? {
        let origin = initial_c.get_or_insert([drain_charge, source_charge]);
        if (50e-9..110e-9).contains(&time) {
            switching += 1;
        }
        let evaluated = device.evaluate(Bias {
            vgs: vgate,
            vds: sign * 0.3,
            temperature: 300.15,
            ..Bias::default()
        })?;
        for (name, charge, initial, expected) in [
            (
                "qgd",
                evaluated.qgd.value,
                base.qgd.value,
                drain_charge - origin[0],
            ),
            (
                "qgs",
                evaluated.qgs.value,
                base.qgs.value,
                source_charge - origin[1],
            ),
        ] {
            // C reports gate-charge state in the polarity-normalized model
            // coordinate; physical gate-plate charge is signed by polarity.
            let actual = sign * (charge - initial);
            if (actual - expected).abs() > 3e-11_f64.max(expected.abs() * 0.004) {
                return Err(format!("{fixture} t={time:e} {name}: Rust delta {actual:e}, ngspice47 delta {expected:e}, diff {:e}", actual-expected).into());
            }
        }
        count += 1;
    }
    if count < 350 || switching < 100 {
        return Err(format!("insufficient gate switching coverage: {count}/{switching}").into());
    }
    Ok(())
}

#[test]
#[ignore = "requires installed ngspice 47 C reference"]
fn nmos_gate_switching_charge_full_transient() -> Result<()> {
    compare_gate_transient(Polarity::N, "gate_transient_n.cir")
}

#[test]
#[ignore = "requires installed ngspice 47 C reference"]
fn pmos_gate_switching_charge_full_transient() -> Result<()> {
    compare_gate_transient(Polarity::P, "gate_transient_p.cir")
}

fn compare_weak_inversion(polarity: Polarity, temperature: f64, fixture: &str) -> Result<()> {
    let sign = if matches!(polarity, Polarity::N) {
        1.0
    } else {
        -1.0
    };
    let prepared = prepared_from_fixture(polarity, fixture)?;
    let model = prepared.parameters();
    let device = prepared.bind(Instance::default())?;
    for [swept, vgate, id_c, gm_c, gds_c] in oracle_table::<5>(fixture, 9)? {
        if (swept - vgate).abs() > 1e-8 {
            return Err("gate sweep mismatch".into());
        }
        let evaluated = device.evaluate(Bias {
            vgs: vgate,
            vds: sign * 0.15,
            temperature,
            ..Bias::default()
        })?;
        // ngspice47 unconditionally differentiates the mobility factor
        // `1/(1+theta*max(vgs-vth,0))` *as if* max() were active below
        // threshold. The real derivative is zero there. Preserve the
        // correct smooth Jacobian in Rust; compare the C gm with precisely
        // its historical spurious derivative, not with a loose tolerance.
        let normalized_gate = sign * vgate;
        let normalized_threshold = sign * model.channel.vth;
        let c_gm_correction = if normalized_gate <= normalized_threshold {
            let overdrive = normalized_gate - normalized_threshold;
            let slope = model.channel.ksubthres;
            let weak_drive = slope * ((overdrive - model.channel.subshift) / slope).exp().ln_1p();
            let beta = model.channel.kp * (temperature / model.tnom).powf(model.channel.mu);
            let d_beta_wrong =
                -beta * model.channel.theta * model.channel.lambda.mul_add(0.15, 1.0);
            0.5 * d_beta_wrong * weak_drive * weak_drive
        } else {
            0.0
        };
        for (name, actual, reference) in [
            ("Id", evaluated.channel.value.abs(), id_c),
            (
                "gm C historical",
                evaluated.channel.d_vgs + c_gm_correction,
                gm_c,
            ),
            ("gds", evaluated.channel.d_vds, gds_c),
        ] {
            if (actual - reference).abs() > 6e-5_f64.mul_add(reference.abs(), 1e-31) {
                return Err(format!(
                    "{fixture} Vg={vgate} {name}: Rust {actual:e} C {reference:e}, difference {:e}",
                    actual - reference
                )
                .into());
            }
        }
    }
    Ok(())
}

#[test]
#[ignore = "requires installed ngspice 47 C reference"]
fn nmos_weak_inversion_to_triode_sweep() -> Result<()> {
    compare_weak_inversion(Polarity::N, 300.15, "weak_n.cir")
}

#[test]
#[ignore = "requires installed ngspice 47 C reference"]
fn pmos_weak_inversion_to_triode_sweep() -> Result<()> {
    compare_weak_inversion(Polarity::P, 300.15, "weak_p.cir")
}

#[test]
#[ignore = "requires installed ngspice 47 C reference"]
fn nmos_hot_weak_inversion_to_triode_sweep() -> Result<()> {
    compare_weak_inversion(Polarity::N, 343.15, "weak_n_hot.cir")
}

fn compare_quasi_saturation(polarity: Polarity, temperature: f64, fixture: &str) -> Result<()> {
    let prepared = prepared_from_fixture(polarity, fixture)?;
    let device = prepared.bind(Instance::default())?;
    for [vds, resistance_c] in oracle_table::<2>(fixture, 9)? {
        let evaluated = device.evaluate(Bias {
            vds,
            vds_external: vds,
            temperature,
            ..Bias::default()
        })?;
        let resistance = evaluated
            .series
            .drain
            .value
            .ok_or("missing quasi drain branch")?
            .recip();
        if (resistance - resistance_c).abs() > 4e-7_f64.mul_add(resistance_c.abs(), 1e-9) {
            return Err(format!(
                "{fixture} Vds={vds} Rdrain: Rust {resistance:e} C {resistance_c:e}"
            )
            .into());
        }
    }
    Ok(())
}

#[test]
#[ignore = "requires installed ngspice 47 C reference"]
fn nmos_quasi_saturation_resistance_sweep() -> Result<()> {
    compare_quasi_saturation(Polarity::N, 300.15, "quasi_n.cir")
}

#[test]
#[ignore = "requires installed ngspice 47 C reference"]
fn pmos_quasi_saturation_resistance_sweep() -> Result<()> {
    compare_quasi_saturation(Polarity::P, 300.15, "quasi_p.cir")
}

#[test]
#[ignore = "requires installed ngspice 47 C reference"]
fn nmos_hot_quasi_saturation_resistance_sweep() -> Result<()> {
    compare_quasi_saturation(Polarity::N, 343.15, "quasi_n_hot.cir")
}

#[test]
fn all_native_c_oracle_cards_compile_from_plaintext() -> Result<()> {
    let fixture_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut fixture_count = 0;
    for entry in std::fs::read_dir(&fixture_dir)? {
        let entry = entry?;
        if entry.path().extension().and_then(std::ffi::OsStr::to_str) != Some("cir") {
            continue;
        }
        let filename = entry.file_name();
        let fixture = filename.to_str().ok_or("invalid fixture filename")?;
        let (polarity, model) = model_from_fixture(fixture)?;
        let device = model.bind(Instance::default())?;
        let sign = if matches!(polarity, Polarity::N) {
            1.0
        } else {
            -1.0
        };
        let result = device.evaluate(Bias {
            vgs: sign * 4.0,
            vds: sign * 0.3,
            vds_external: sign * 0.3,
            vdiode: -sign * 0.1,
            temperature: 300.15,
            ..Bias::default()
        })?;
        if !result.channel.value.is_finite() || !result.diode.current.value.is_finite() {
            return Err(format!("{fixture}: prepared invalid operating point").into());
        }
        fixture_count += 1;
    }
    if fixture_count < 24 {
        return Err(format!("only {fixture_count} VDMOS cards covered").into());
    }
    Ok(())
}

#[test]
#[ignore = "requires installed ngspice 47 C reference"]
fn irf7220_reversed_cgd_endpoints_match_ngspice47() -> Result<()> {
    let model = prepared_from_fixture(Polarity::P, "vendor_irf7220.cir")?;
    let device = model.bind(Instance::default())?;
    if model.parameters().gate.cgdmax >= model.parameters().gate.cgdmin {
        return Err("IRF7220 reversed Cgd regression fixture lost its special case".into());
    }
    for [
        source_vds,
        observed_vds,
        cap_c,
        intrinsic_vds,
        intrinsic_vgs,
    ] in oracle_table::<5>("vendor_irf7220.cir", 11)?
    {
        if (source_vds - observed_vds).abs() > 1e-12 {
            return Err("Vds source mismatch".into());
        }
        // This catalog card has nonzero ohmic series resistance, and its
        // P-channel diode is forward biased in the positive Vds half-sweep.
        // C reports polarity-normalized *intrinsic* device terminal voltages.
        // Compare the capacitive constitutive law at those biases, not at
        // source nodes separated by the series diode/resistor drops.
        let evaluated = device.evaluate(Bias {
            vgs: -intrinsic_vgs,
            vds: -intrinsic_vds,
            temperature: 300.15,
            ..Bias::default()
        })?;
        let calculated = evaluated.qgd.d_voltage;
        if (calculated - cap_c).abs() > 2e-6_f64.mul_add(cap_c.abs(), 5e-15) {
            return Err(format!(
                "IRF7220 Vds={source_vds}: Rust Cgd {calculated:e}, ngspice47 {cap_c:e}"
            )
            .into());
        }
    }
    Ok(())
}

fn parameterized_oracle(fixture: &str) -> Result<()> {
    use model_normalize_ngspice::{parameter_scope::ModelLibrary, vdmos_card::VdmosPolarity};
    let source = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(fixture),
    )?;
    let library = ModelLibrary::parse(&source)?;
    if library.models.len() != 1 {
        return Err("unexpected source model count".into());
    }
    let stdout = run_oracle(fixture)?;
    let call_gain = if fixture.ends_with("scope_expr.cir") {
        library.evaluate_scalar(0, "{gain*2}", &[])?
    } else {
        4.0
    };
    let overrides = [(1, "gain", call_gain)];
    let cases = if fixture.ends_with("scope.cir")
        || fixture.ends_with("scope_expr.cir")
        || fixture.ends_with("function_scope.cir")
        || fixture.ends_with("function_capture.cir")
        || fixture.ends_with("param_capture.cir")
    {
        vec![
            ("@m.x1.m1[id]", overrides.as_slice()),
            ("@m.x2.m1[id]", &[][..]),
        ]
    } else {
        vec![("@m1[id]", &[][..])]
    };
    for (vector, overrides) in cases {
        let model = library
            .resolve_vdmos(0, overrides)?
            .ok_or("no VDMOS model")?;
        let sign = if matches!(model.polarity, VdmosPolarity::N) {
            1.0
        } else {
            -1.0
        };
        let prepared = compile_ngspice(
            if sign > 0.0 { Polarity::N } else { Polarity::P },
            model.parameters,
        )?;
        let device = prepared.bind(Instance::default())?;
        let evaluated = device.evaluate(Bias {
            vgs: sign * 4.0,
            vds: sign * 0.25,
            temperature: 300.15,
            ..Bias::default()
        })?;
        let line = stdout
            .lines()
            .find(|line| line.trim_start().starts_with(vector))
            .ok_or_else(|| format!("{fixture}: no {vector} in ngspice output"))?;
        let (_, raw) = line
            .split_once('=')
            .ok_or("missing numerical oracle value")?;
        let actual = evaluated.channel.value.abs();
        let reference: f64 = raw.trim().parse()?;
        if (actual - reference).abs() > 2e-5_f64.mul_add(reference.abs(), 1e-8) {
            return Err(format!("{fixture}: {vector} Rust {actual:e}, C {reference:e}").into());
        }
    }
    Ok(())
}

#[test]
#[ignore = "requires installed ngspice 47 C reference"]
fn ngspice47_parameter_arithmetic_and_conditional_compile() -> Result<()> {
    parameterized_oracle("params/basic.cir")
}

#[test]
#[ignore = "requires installed ngspice 47 C reference"]
fn ngspice47_parameter_forward_reference_compile() -> Result<()> {
    parameterized_oracle("params/forward.cir")
}

#[test]
#[ignore = "requires installed ngspice 47 C reference"]
fn ngspice47_parameter_last_definition_wins_compile() -> Result<()> {
    parameterized_oracle("params/duplicate.cir")
}

#[test]
#[ignore = "requires installed ngspice 47 C reference"]
fn ngspice47_per_instance_subcircuit_parameter_scoping() -> Result<()> {
    parameterized_oracle("params/scope.cir")
}

#[test]
#[ignore = "requires installed ngspice 47 C reference"]
fn ngspice47_parent_evaluated_subcircuit_call_expression() -> Result<()> {
    use model_normalize_ngspice::parameter_scope::ModelLibrary;
    let fixture = "params/scope_expr.cir";
    let source = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(fixture),
    )?;
    let library = ModelLibrary::parse(&source)?;
    let gain = library.evaluate_scalar(0, "{gain*2}", &[])?;
    if (gain - 4.0).abs() > f64::EPSILON {
        return Err("caller override was not 4".into());
    }
    parameterized_oracle(fixture)
}

#[test]
#[ignore = "requires installed ngspice 47 C reference"]
fn ngspice47_nested_user_defined_model_functions() -> Result<()> {
    parameterized_oracle("params/function.cir")
}

#[test]
#[ignore = "requires installed ngspice 47 C reference"]
fn ngspice47_lexically_scoped_user_functions_in_subcircuits() -> Result<()> {
    parameterized_oracle("params/function_scope.cir")
}

#[test]
#[ignore = "requires installed ngspice 47 C reference"]
fn ngspice47_global_user_function_captures_callsite_parameters() -> Result<()> {
    parameterized_oracle("params/function_capture.cir")
}

#[test]
#[ignore = "requires installed ngspice 47 C reference"]
fn ngspice47_global_parameter_retains_owning_scope() -> Result<()> {
    parameterized_oracle("params/param_capture.cir")
}

fn compare_linked_source(fixture: &str, model_file: &str, model_line: usize) -> Result<()> {
    use model_normalize_ngspice::{
        linked_source::SourceLinker, parameter_scope::ModelLibrary, vdmos_card::VdmosPolarity,
    };
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(fixture);
    let linked = SourceLinker::default().link(&root)?;
    let source = ModelLibrary::parse(&linked.bytes)?;
    if source.models.len() != 1 {
        return Err("unexpected linked model count".into());
    }
    let origin = linked
        .origin(source.models[0].card.source_line)
        .ok_or("no linked model origin")?;
    if origin.path.file_name().and_then(std::ffi::OsStr::to_str) != Some(model_file)
        || origin.line != model_line
    {
        return Err(format!("incorrect linked model origin: {origin:?}").into());
    }
    let model = source.resolve_vdmos(0, &[])?.ok_or("no resolved model")?;
    if model.polarity != VdmosPolarity::N {
        return Err("linked model polarity".into());
    }
    let prepared = compile_ngspice(Polarity::N, model.parameters)?;
    let evaluation = prepared.bind(Instance::default())?.evaluate(Bias {
        vgs: 4.0,
        vds: 0.25,
        temperature: 300.15,
        ..Bias::default()
    })?;
    let oracle = run_oracle(fixture)?;
    for (vector, calculated) in [
        ("@m1[id]", evaluation.channel.value.abs()),
        ("@m1[gm]", evaluation.channel.d_vgs),
        ("@m1[gds]", evaluation.channel.d_vds),
    ] {
        let row = oracle
            .lines()
            .find(|line| line.trim_start().starts_with(vector))
            .ok_or_else(|| format!("ngspice oracle missing {vector}"))?;
        let reference: f64 = row
            .split_once('=')
            .ok_or("missing oracle =")?
            .1
            .trim()
            .parse()?;
        if (calculated - reference).abs() > 3e-5_f64.mul_add(reference.abs(), 1e-8) {
            return Err(
                format!("linked source {vector}: Rust {calculated:e} vs C {reference:e}").into(),
            );
        }
    }
    Ok(())
}

#[test]
#[ignore = "requires installed ngspice 47 C reference"]
fn ngspice47_included_function_and_selected_library_section() -> Result<()> {
    compare_linked_source("params/linked_source.cir", "linked_devices.lib", 6)
}

#[test]
#[ignore = "requires installed ngspice 47 C reference"]
fn ngspice47_same_file_named_sections_share_parameters() -> Result<()> {
    compare_linked_source("params/sections_root.cir", "sections_parts.lib", 6)
}

#[test]
#[ignore = "requires installed ngspice 47 C reference"]
fn large_junction_potential_charge_sweep() -> Result<()> {
    compare_charge("charge_large_vj.cir", Polarity::N, 343.15, false)
}

#[test]
#[ignore = "requires installed ngspice 47 C reference"]
fn negative_fitted_grading_charge_sweep() -> Result<()> {
    compare_charge("charge_negative_grading.cir", Polarity::N, 300.15, false)
}

#[test]
#[ignore = "requires installed ngspice 47 C reference"]
fn negative_series_resistance_means_absent_branch() -> Result<()> {
    compare(Polarity::N, "nmos_negative_series.cir")
}
