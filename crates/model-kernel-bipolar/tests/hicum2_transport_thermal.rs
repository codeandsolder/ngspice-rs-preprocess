//! Full HICCR implicit charge/current d/dTdev source validation.
//! Every numerical comparison independently re-prepares the complete
//! nominal model and solves its implicit hole charge at Tdev +/- delta.
//! Numerical differences are used exclusively in these tests.

use model_kernel_bipolar::{
    Temperature,
    hicum2::{temperature, transport},
};

#[path = "fixtures/hicum2_baseline.rs"]
mod baseline;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn model(variant: usize, mode: f64) -> temperature::Nominal {
    let mut nominal = temperature::Nominal::source_defaults();
    nominal.transport = baseline::parameters();
    nominal.transport.flcomp = mode;
    nominal.transport.t0 = 5e-12;
    nominal.transport.dt0h = 1.1e-12;
    nominal.transport.tbvl = 0.6e-12;
    nominal.coefficients.zetact = 3.4;
    nominal.coefficients.zetaci = 0.8;
    nominal.coefficients.zetabet = 3.6;
    nominal.coefficients.zetacx = 1.2;
    nominal.coefficients.zetahjei = 1.4;
    nominal.coefficients.zetavgbe = 1.3;
    nominal.coefficients.dvgbe = 0.05;
    nominal.coefficients.alb = 0.0004;
    nominal.coefficients.alvs = 0.0003;
    nominal.coefficients.alces = 0.0002;
    nominal.coefficients.alt0 = 0.0004;
    nominal.coefficients.kt0 = 4e-7;
    match variant {
        0 => {}
        1 => {
            nominal.transport.ich = 0.003;
            nominal.transport.tbhrec = 4e-12;
            nominal.transport.hf0 = 1.3;
            nominal.transport.hfe = 0.8;
            nominal.transport.hfc = 1.2;
            nominal.transport.hjei = 1.3;
            nominal.transport.ahjei_nominal = 0.25;
            nominal.transport.ahjei = 0.25;
            nominal.transport.hjei_zero_bias = 1.3;
        }
        2 => {
            nominal.transport.tef0 = 0.0;
            nominal.transport.thcs = 0.0;
            nominal.transport.latb = 0.0;
            nominal.transport.latl = 0.0;
            nominal.transport.vcbar = 0.0;
        }
        3 => {
            nominal
                .transport
                .junction
                .intrinsic_collector
                .zero_bias_capacitance = 0.0;
            nominal.transport.latb = 0.0;
            nominal.transport.latl = 0.3;
            nominal.transport.hjei = 0.0;
            nominal.transport.ahjei_nominal = 0.0;
        }
        _ => unreachable!(),
    }
    nominal
}

fn physical(
    nominal: &temperature::Nominal,
    kelvin: f64,
    bias: transport::Bias,
) -> Result<transport::Output, Box<dyn std::error::Error>> {
    Ok(nominal
        .prepare(Temperature::new(kelvin)?, 0.0)?
        .transport_kernel()?
        .evaluate(bias)?)
}

fn assert_source_temperature_sensitivity(
    actual: transport::TemperatureSensitivity,
    before: &transport::Output,
    after: &transport::Output,
    step: f64,
    case: (&str, usize, f64, f64),
) {
    macro_rules! source {
        ($name:literal,$sensitivity:ident,$field:ident,$absolute:expr) => {
            (
                $name,
                actual.$sensitivity,
                before.$field.value,
                after.$field.value,
                $absolute,
            )
        };
    }
    let comparisons = [
        source!("Itf", forward_current, forward_current, 1e-15),
        source!("Itr", reverse_current, reverse_current, 1e-15),
        source!("It", net_transfer_current, net_transfer_current, 1e-15),
        source!("Ick", critical_current, critical_current, 1e-15),
        source!(
            "Q0",
            equilibrium_hole_charge,
            equilibrium_hole_charge,
            1e-20
        ),
        source!("Qp", total_hole_charge, total_hole_charge, 1e-20),
        source!(
            "Qf",
            forward_diffusion_charge,
            forward_diffusion_charge,
            1e-20
        ),
        source!(
            "QfT",
            weighted_forward_charge,
            weighted_forward_charge,
            1e-20
        ),
        source!(
            "Qr",
            reverse_diffusion_charge,
            reverse_diffusion_charge,
            1e-20
        ),
        source!("Qbf", excess_base_charge, excess_base_charge, 1e-20),
        source!("Ibh", barrier_base_current, barrier_base_current, 1e-15),
        source!(
            "Tf0",
            low_current_transit_time,
            low_current_transit_time,
            1e-18
        ),
        (
            "Tf",
            actual.forward_transit_time,
            before.forward_transit_time,
            after.forward_transit_time,
            1e-18,
        ),
        (
            "TfWeighted",
            actual.weighted_transit_time,
            before.weighted_transit_time,
            after.weighted_transit_time,
            1e-18,
        ),
    ];
    for (name, exact, low, high, absolute) in comparisons {
        let numerical = (high - low) / (2.0 * step);
        let relative: f64 = 0.005;
        let allowed = relative.mul_add(exact.abs().max(numerical.abs()), absolute);
        assert!(
            (exact - numerical).abs() <= allowed,
            "{case:?} {name}: analytic={exact:.15e} numerical={numerical:.15e}, diff={:.3e} tol={allowed:.3e}",
            (exact - numerical).abs()
        );
    }
}

#[test]
fn implicit_hiccr_full_thermal_current_and_storage_match_independent_repreparation() -> TestResult {
    let mut checked = 0;
    let mut high_injection = 0;
    let mut barrier_active = 0;
    for mode in [0.0, 2.1, 2.2, 2.3] {
        for variant in 0..4 {
            let nominal = model(variant, mode);
            for kelvin in [233.15, 300.15, 373.15] {
                let step = 0.005;
                for emitter in [0.62, 0.72, 0.78, 0.85] {
                    for collector in [-0.8, -0.15, 0.18] {
                        let bias = transport::Bias {
                            vbiei: emitter,
                            vbici: collector,
                        };
                        let op = physical(&nominal, kelvin, bias)?;
                        let before = physical(&nominal, kelvin - step, bias)?;
                        let after = physical(&nominal, kelvin + step, bias)?;
                        assert_source_temperature_sensitivity(
                            op.temperature_sensitivity,
                            &before,
                            &after,
                            step,
                            ("hiccr", variant, mode, kelvin),
                        );
                        high_injection += usize::from(op.iterations > 0);
                        barrier_active += usize::from(op.barrier_base_current.value > 1e-12);
                        checked += 14;
                    }
                }
            }
        }
    }
    assert_eq!(checked, 8064);
    assert!(high_injection > 80);
    assert!(barrier_active > 5);
    Ok(())
}
