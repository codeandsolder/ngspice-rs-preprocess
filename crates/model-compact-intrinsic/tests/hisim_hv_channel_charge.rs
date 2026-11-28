//! Independent native endpoint and integrated charge reference.
use model_compact_intrinsic::{
    HiSimHvChannelChargeInputs, HiSimHvChannelRegion, HiSimHvIntrinsicScalar as Jet,
    evaluate_hisim_hv_channel_charge,
};
type TestResult = Result<(), Box<dyn std::error::Error>>;
fn close(actual: f64, expected: f64, rel: f64, abs: f64, context: impl std::fmt::Debug) {
    assert!(
        (actual - expected).abs() <= rel.mul_add(actual.abs().max(expected.abs()), abs),
        "{context:?}: {actual:e} vs {expected:e}"
    );
}
fn inputs(fields: &[f64; 65]) -> HiSimHvChannelChargeInputs {
    HiSimHvChannelChargeInputs {
        effective_gate: Jet {
            value: fields[8],
            gradient: [1., 0.015, -0.03, 0.0002],
        },
        confined_body: Jet {
            value: fields[2],
            gradient: [0., 0.02, 0.91, -0.00015],
        },
        effective_drain: Jet {
            value: fields[3],
            gradient: [0., 0.8, 0., 0.],
        },
        inverse_thermal_voltage: Jet {
            value: fields[4],
            gradient: [0., 0., 0., -0.13],
        },
        oxide_capacitance: Jet {
            value: fields[5],
            gradient: [0.0001, 0.00002, -0.00003, 2e-6],
        },
        poisson_coefficient: Jet {
            value: fields[6],
            gradient: [0., 0., 0., -0.0015 * fields[6]],
        },
        minority_coefficient: Jet {
            value: fields[7],
            gradient: [0., 0., 0., 0.025 * fields[7]],
        },
    }
}
#[test]
fn native_normal_channel_integrals() -> TestResult {
    let mut count = 0;
    let mut regions = [0; 4];
    for (index, line) in include_str!("fixtures/hisim-hv-channel-charge-ngspice47.txt")
        .lines()
        .filter(|l| !l.starts_with('#'))
        .enumerate()
    {
        let fields: [f64; 65] = line
            .split_whitespace()
            .map(str::parse::<f64>)
            .collect::<Result<Vec<_>, _>>()?
            .try_into()
            .map_err(|_| "channel fixture width")?;
        let input = inputs(&fields);
        let point = evaluate_hisim_hv_channel_charge(&input)
            .map_err(|e| format!("case {index} {input:?}: {e}"))?;
        let (region, slot) = match fields[9] {
            1. => (HiSimHvChannelRegion::WeakInversion, 0),
            2. => (HiSimHvChannelRegion::Transition, 1),
            3. => (HiSimHvChannelRegion::StrongInversion, 2),
            4. => (HiSimHvChannelRegion::NoInversion, 3),
            _ => return Err("native region".into()),
        };
        assert_eq!(point.region, region, "case {index}");
        regions[slot] += 1;
        let values = [
            point.source_surface,
            point.drain_surface,
            point.surface_difference,
            point.source_bulk_density,
            point.source_inversion_density,
            point.inversion_voltage,
            point.drift_diffusion_integral,
            point.bulk_density,
            point.inversion_density,
            point.drain_fraction,
            point.transition_weight,
        ];
        let floors = [
            1e-14, 1e-14, 1e-14, 1e-17, 1e-24, 1e-20, 1e-15, 1e-17, 1e-24, 1e-13, 1e-13,
        ];
        for (field, jet) in values.into_iter().enumerate() {
            close(
                jet.value,
                fields[10 + field],
                3e-9,
                floors[field],
                (index, field, "value"),
            );
            for (axis, slope) in jet.gradient.into_iter().enumerate() {
                close(
                    slope,
                    fields[21 + 11 * axis + field],
                    5e-5,
                    1000. * floors[field],
                    (index, field, axis),
                );
            }
        }
        if input.effective_drain.value == 0. && region != HiSimHvChannelRegion::NoInversion {
            close(
                point.source_surface.value,
                point.drain_surface.value,
                0.,
                1e-14,
                (index, "zero drain"),
            );
            assert!(point.surface_difference.gradient[1] > 0., "case {index}");
        }
        count += 1;
    }
    assert_eq!(count, 1536);
    assert!(regions.into_iter().all(|n| n > 0), "{regions:?}");
    Ok(())
}
