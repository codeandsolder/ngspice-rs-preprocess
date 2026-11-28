use model_compact_intrinsic::{
    HiSimHvDepletionCarrierMobility, HiSimHvDepletionChannelCoefficients,
    HiSimHvDepletionChannelInputs, HiSimHvDepletionChannelPoint, HiSimHvDepletionEndpointPoint,
    HiSimHvDepletionSurfacePoint, HiSimHvIntrinsicScalar, ModelError,
};
type Jet = HiSimHvIntrinsicScalar;
type TestResult = Result<(), Box<dyn std::error::Error>>;
fn eval(row: &[f64; 41], offset: [f64; 4]) -> Result<HiSimHvDepletionChannelPoint, ModelError> {
    let state = |i: usize| {
        let value = row[i];
        let gradient = std::array::from_fn(|axis| {
            value.abs()
                * 0.015
                * [1., 2., 3., 4., 5., 6., 7.][i % 7]
                * [1., 2., 3., 4.][axis]
                * if (i + axis).is_multiple_of(2) {
                    1.
                } else {
                    -1.
                }
        });
        Jet {
            value: value + gradient.iter().zip(offset).map(|(g, x)| g * x).sum::<f64>(),
            gradient,
        }
    };
    let zero = Jet {
        value: 0.,
        gradient: [0.; 4],
    };
    let endpoint = |drain: bool| {
        let side = usize::from(drain);
        HiSimHvDepletionEndpointPoint {
            surface: HiSimHvDepletionSurfacePoint {
                surface: state(side),
                buried: zero,
                junction: zero,
                surface_charge_density: state(2 + side),
                buried_charge_density: state(17 + side),
                substrate_charge_density: state(21 + side),
            },
            accumulation_charge: state(13 + side),
            substrate_inversion: state(19 + side),
            surface_depletion: state(15 + side),
            residual_carrier: state(6),
            back_carrier: state(7),
            transport_carrier: state(4 + side),
            smoothing_charge: state(8),
        }
    };
    let carrier = |i: usize| HiSimHvDepletionCarrierMobility {
        coulomb_mobility: row[i],
        coulomb_density_factor: row[i + 1],
    };
    HiSimHvDepletionChannelCoefficients {
        surface: carrier(38),
        residual: carrier(33),
        back: carrier(35),
        inversion_field_factor: row[23],
        surface_phonon_exponent: row[30],
        neutral_phonon_exponent: row[32],
        roughness_exponent: row[25],
        roughness_mobility: row[40],
        surface_velocity_exponent: row[31],
        neutral_velocity_exponent: row[37],
        length_m: row[11],
        width_m: row[29],
        multiplicity: 1.,
    }
    .evaluate(
        &HiSimHvDepletionChannelInputs {
            inverse_thermal_voltage: state(10),
            density_gradient: state(24),
            surface_phonon_coefficient: state(26),
            neutral_phonon_coefficient: state(27),
            surface_saturation_velocity: state(12),
            neutral_saturation_velocity: state(28),
            effective_neutral_drain: state(9),
        },
        &endpoint(false),
        &endpoint(true),
    )
}
fn outputs(p: &HiSimHvDepletionChannelPoint) -> [Jet; 11] {
    [
        p.surface_difference,
        p.drift_diffusion_integral,
        p.surface_universal_mobility,
        p.surface_mobility,
        p.residual_mobility,
        p.back_mobility,
        p.surface_current_a,
        p.residual_current_a,
        p.back_current_a,
        p.bulk_density,
        p.channel_density,
    ]
}
#[test]
fn native_depletion_transport() -> TestResult {
    let mut count = 0;
    let mut failures = [0; 11];
    let mut max_error = [0_f64; 11];
    for line in include_str!("fixtures/hisim-hv-depletion-transport-ngspice47.txt").lines() {
        if line.starts_with('#') {
            continue;
        }
        let row: Vec<f64> = line
            .split_whitespace()
            .map(str::parse)
            .collect::<Result<_, _>>()?;
        let actual = outputs(&eval(&row[..41].try_into()?, [0.; 4])?);
        for (i, p) in actual.into_iter().enumerate() {
            let expected = row[41 + i];
            let err = (p.value - expected).abs();
            if err
                > 1e-10 * expected.abs().max(p.value.abs())
                    + if (6..9).contains(&i) { 1e-22 } else { 1e-15 }
            {
                failures[i] += 1;
                if failures[i] <= 3 || err > max_error[i] {
                    eprintln!(
                        "field {i}: native={expected:e}, Rust={:e}, input={:?}",
                        p.value,
                        &row[..41]
                    );
                }
                max_error[i] = max_error[i].max(err);
            }
        }
        count += 1;
    }
    assert_eq!(count, 3456);
    assert_eq!(failures, [0; 11], "maximum errors {max_error:?}");
    Ok(())
}

#[test]
fn independent_depletion_transport_jacobians() -> TestResult {
    let mut count = 0;
    for (index, line) in include_str!("fixtures/hisim-hv-depletion-transport-ngspice47.txt")
        .lines()
        .enumerate()
    {
        if line.starts_with('#') {
            continue;
        }
        let row: Vec<f64> = line
            .split_whitespace()
            .map(str::parse)
            .collect::<Result<_, _>>()?;
        if index % 7 != 0 || row[1] - row[0] < 0.04 || row[9] < 0.009 {
            continue;
        }
        let row: [f64; 41] = row[..41].try_into()?;
        let actual = outputs(&eval(&row, [0.; 4])?);
        for axis in 0..4 {
            let step = 1e-4;
            let ev = |n: f64| {
                let mut offset = [0.; 4];
                offset[axis] = n * step;
                eval(&row, offset).map(|p| outputs(&p))
            };
            let minus_two = ev(-2.)?;
            let minus_one = ev(-1.)?;
            let plus_one = ev(1.)?;
            let plus_two = ev(2.)?;
            for i in 0..11 {
                let fd = ((minus_two[i].value - plus_two[i].value)
                    + 8. * (plus_one[i].value - minus_one[i].value))
                    / (12. * step);
                let exact = actual[i].gradient[axis];
                let budget = 2e-5 * fd.abs().max(exact.abs())
                    + 64. * f64::EPSILON * actual[i].value.abs() / step
                    + if (6..9).contains(&i) { 1e-15 } else { 1e-12 };
                assert!(
                    (fd - exact).abs() <= budget,
                    "field {i} axis {axis} input={row:?}, FD={fd:e} AD={exact:e}"
                );
                count += 1;
            }
        }
    }
    assert!(count > 4000);
    Ok(())
}
