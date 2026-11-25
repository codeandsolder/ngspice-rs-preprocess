//! Cross-family analytic HICRBI thermal derivatives checked against fully
//! independent source temperature re-preparation at nearby Kelvin values.

use model_kernel_bipolar::{
    ModelError, Temperature,
    hicum2::{self, device, internal_base, nqs, temperature},
};
type TestResult = Result<(), Box<dyn std::error::Error>>;

fn close(actual: f64, expected: f64, relative: f64, absolute: f64) {
    let tolerance = relative.mul_add(actual.abs().max(expected.abs()), absolute);
    assert!(
        (actual - expected).abs() <= tolerance,
        "actual={actual:.15e} expected={expected:.15e} tolerance={tolerance:.6e}"
    );
}

const fn model(flcomp: f64, geometry: f64, short: bool) -> temperature::Nominal {
    let mut m = temperature::Nominal::source_defaults();
    m.transport.flcomp = flcomp;
    m.auxiliary.rbi0 = if short { 0.0009 } else { 15.0 };
    m.auxiliary.fgeo = geometry;
    m.auxiliary.fqi = 0.32;
    m.auxiliary.fcrbi = 0.55;
    m.auxiliary.fdqr0 = 0.15;
    m.coefficients.zetarbi = 1.35;
    m.coefficients.zetabet = 3.25;
    m.coefficients.alt0 = 0.004;
    m
}

fn bias(polarity: hicum2::Polarity, be: f64, bc: f64) -> device::Bias {
    let sign = if matches!(polarity, hicum2::Polarity::Npn) {
        1.0
    } else {
        -1.0
    };
    let bpbi = 0.012;
    device::Bias {
        nodes: device::Nodes {
            b: sign * (be + bpbi),
            bp: sign * (be + bpbi),
            bi: sign * be,
            c: sign * (be - bc),
            ci: sign * (be - bc),
            e: 0.0,
            ei: 0.0,
            s: 0.0,
            si: 0.0,
        },
        nqs: nqs::State::default(),
    }
}

fn compare(
    derivative: internal_base::TemperatureSensitivity,
    low: &device::Output,
    high: &device::Output,
    step: f64,
    drop: f64,
) {
    let ratio = 1.0 / (2.0 * step);
    let l = low.internal_base;
    let h = high.internal_base;
    close(
        derivative.resistance,
        (h.resistance.value - l.resistance.value) * ratio,
        2e-4,
        3e-12,
    );
    close(
        derivative.resistor_current,
        (h.resistor_current.value - l.resistor_current.value) * ratio,
        2e-4,
        3e-13,
    );
    close(
        derivative.branch_conductance,
        (h.branch_conductance - l.branch_conductance) * ratio,
        2e-4,
        5e-15,
    );
    close(
        derivative.hf_shunt_charge,
        (h.hf_shunt_charge.value - l.hf_shunt_charge.value) * ratio,
        2e-4,
        2e-24,
    );
    close(
        derivative.hf_branch_capacitance,
        (h.hf_branch_capacitance - l.hf_branch_capacitance) * ratio,
        2e-4,
        2e-22,
    );
    close(
        derivative.base_dissipation_slope(drop),
        drop * (h.resistor_current.value - l.resistor_current.value) * ratio,
        2e-4,
        3e-15,
    );
}

#[test]
fn real_device_hicrbi_exact_temperature_slopes_cover_transport_junction_and_crowding() -> TestResult
{
    let mut points = 0;
    let mut nonzero_crowding = 0;
    for flcomp in [0.0, 2.2, 2.3] {
        for geometry in [0.0, 0.45, 1.5] {
            for kelvin in [253.15, 300.15, 373.15] {
                for polarity in [hicum2::Polarity::Npn, hicum2::Polarity::Pnp] {
                    let mut nominal = model(flcomp, geometry, false);
                    nominal.polarity = polarity;
                    let h = 0.006;
                    let mid = nominal
                        .prepare(Temperature::new(kelvin)?, 0.0)?
                        .device_kernel()?;
                    let below = nominal
                        .prepare(Temperature::new(kelvin - h)?, 0.0)?
                        .device_kernel()?;
                    let above = nominal
                        .prepare(Temperature::new(kelvin + h)?, 0.0)?
                        .device_kernel()?;
                    for be in [0.61, 0.7, 0.77] {
                        for bc in [-0.75, -0.2, 0.2] {
                            let input = bias(polarity, be, bc);
                            let operating = mid.evaluate(&input)?;
                            let derivative =
                                mid.internal_base_temperature_sensitivity(&operating)?;
                            let lower = below.evaluate(&input)?;
                            let upper = above.evaluate(&input)?;
                            compare(derivative, &lower, &upper, h, operating.voltage.vbpbi_raw);
                            if geometry > 0.0 && derivative.resistance.abs() > 1e-11 {
                                nonzero_crowding += 1;
                            }
                            points += 1;
                        }
                    }
                }
            }
        }
    }
    assert_eq!(points, 486);
    assert!(nonzero_crowding > 100);
    Ok(())
}

#[test]
fn source_short_gate_keeps_rbi_current_charge_and_temperature_derivatives_exactly_zero()
-> TestResult {
    let mut nominal = model(2.3, 1.5, true);
    nominal.coefficients.zetarbi = -3.0;
    for kelvin in [253.15, 300.15, 373.15] {
        let kernel = nominal
            .prepare(Temperature::new(kelvin)?, 0.0)?
            .device_kernel()?;
        let op = kernel.evaluate(&bias(hicum2::Polarity::Npn, 0.73, -0.4))?;
        assert!(op.internal_base.shorted);
        assert_eq!(
            kernel.internal_base_temperature_sensitivity(&op)?,
            internal_base::TemperatureSensitivity::default()
        );
    }
    Ok(())
}

#[test]
fn nonfinite_intrinsic_base_temperature_parameter_slopes_are_rejected() -> TestResult {
    let source = model(2.3, 0.2, false);
    let prepared = source.prepare(Temperature::new(300.15)?, 0.0)?;
    let mut slopes = prepared.internal_base_temperature_slopes;
    let p = internal_base::Parameters {
        nominal_rbi0: prepared.nominal_rbi0,
        rbi0: prepared.auxiliary.rbi0,
        qp0: prepared.transport.qp0,
        fdqr0: prepared.auxiliary.fdqr0,
        fgeo: prepared.auxiliary.fgeo,
        fqi: prepared.auxiliary.fqi,
        fcrbi: prepared.auxiliary.fcrbi,
        tr: prepared.transport.tr,
    };
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        slopes.rbi0 = value;
        assert!(matches!(
            internal_base::Prepared::with_temperature_slopes(
                p,
                prepared.effective_temperature,
                slopes,
            ),
            Err(ModelError::InvalidParameter("rbi_temperature_slopes"))
        ));
        slopes.rbi0 = 0.0;
        slopes.qp0 = value;
        assert!(matches!(
            internal_base::Prepared::with_temperature_slopes(
                p,
                prepared.effective_temperature,
                slopes,
            ),
            Err(ModelError::InvalidParameter("rbi_temperature_slopes"))
        ));
        slopes.qp0 = 0.0;
    }
    Ok(())
}
