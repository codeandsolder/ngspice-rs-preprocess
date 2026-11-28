//! Independent native CLM and normal-channel mobility constitutive oracle.
use model_compact_intrinsic::{
    BoundParameters, HiSimHvChannelChargePoint, HiSimHvChannelMobilityCoefficients,
    HiSimHvChannelMobilityInputs, HiSimHvChannelModulationInputs, HiSimHvChannelRegion,
    HiSimHvIntrinsicBias, HiSimHvIntrinsicScalar as Jet, ModelError, ModelVariant, ParameterScope,
    ParameterValue, PreparedHiSimHvChannelModulation, prepare_hisim_hv_instance,
    prepare_hisim_hv_model, prepare_hisim_hv_normal_channel, prepare_hisim_hv_transport,
};
type TestResult = Result<(), Box<dyn std::error::Error>>;

fn close(actual: f64, expected: f64, relative: f64, absolute: f64, context: impl std::fmt::Debug) {
    assert!(
        (actual - expected).abs() <= relative.mul_add(actual.abs().max(expected.abs()), absolute),
        "{context:?}: {actual:e} vs {expected:e}"
    );
}

fn values(fields: &[f64; 72]) -> Result<[Jet; 8], ModelError> {
    let v = |index: usize| fields[index + 2];
    let j = |index: usize| Jet {
        value: v(index),
        gradient: std::array::from_fn(|axis| {
            let percent = u32::try_from(1 + ((index + axis * 3) % 7)).unwrap_or_default();
            v(index) * f64::from(percent) / 100.
        }),
    };
    let zero = Jet::default();
    let region = match fields[1] {
        1. => HiSimHvChannelRegion::WeakInversion,
        2. => HiSimHvChannelRegion::Transition,
        3. => HiSimHvChannelRegion::StrongInversion,
        _ => {
            return Err(ModelError::InvalidPhysicalParameter(
                "native channel region",
            ));
        }
    };
    let charge = HiSimHvChannelChargePoint {
        region,
        source_surface: j(1),
        drain_surface: j(2),
        surface_difference: Jet {
            value: v(2) - v(1),
            gradient: std::array::from_fn(|axis| j(2).gradient[axis] - j(1).gradient[axis]),
        },
        source_bulk_density: zero,
        source_inversion_density: j(4),
        inversion_voltage: zero,
        drift_diffusion_integral: j(7),
        bulk_density: j(5),
        inversion_density: j(6),
        drain_fraction: zero,
        transition_weight: j(10),
        bulk_feedback_shape: zero,
    };
    let clm = PreparedHiSimHvChannelModulation {
        channel_length_m: v(11),
        channel_doping_m3: v(12),
        potential_partition: v(13),
        bulk_factor: v(14),
        inversion_factor: v(15),
        length_factor: v(16),
    };
    // Leff and FD2 in D3 are fixed geometry/region parameters in Rust.
    let mut charge = charge;
    if region == HiSimHvChannelRegion::StrongInversion {
        charge.transition_weight.gradient = [0.; 4];
    }
    let modulation = clm.evaluate(
        &HiSimHvChannelModulationInputs {
            drain_bias: j(0),
            confined_body: j(3),
            inverse_thermal_voltage: j(8),
            symmetry_factor: j(9),
        },
        &charge,
    )?;
    let mobility = HiSimHvChannelMobilityCoefficients {
        depletion_field_factor: v(17),
        inversion_field_factor: v(18),
        coulomb_mobility: v(20),
        coulomb_density_factor: v(21),
        phonon_exponent: v(22),
        roughness_exponent: v(23),
        roughness_mobility: v(24),
        velocity_exponent: v(27),
        effective_width_m: v(28),
        multiplicity: v(29),
    }
    .evaluate(
        &HiSimHvChannelMobilityInputs {
            inverse_thermal_voltage: j(8),
            density_gradient: j(19),
            phonon_coefficient: j(25),
            saturation_velocity_m_per_s: j(26),
        },
        &charge,
        &modulation,
    )?;
    Ok([
        modulation.shortening_m,
        modulation.effective_length_m,
        modulation.drain_surface,
        mobility.transverse_field,
        mobility.universal_mobility_m2_per_vs,
        mobility.longitudinal_field,
        mobility.mobility_m2_per_vs,
        mobility.drain_current_a,
    ])
}

#[test]
fn native_channel_shortening_and_mobility() -> TestResult {
    let mut count = 0;
    for (index, line) in include_str!("fixtures/hisim-hv-channel-transport-ngspice47.txt")
        .lines()
        .filter(|l| !l.starts_with('#'))
        .enumerate()
    {
        let fields: [f64; 72] = line
            .split_whitespace()
            .map(str::parse::<f64>)
            .collect::<Result<Vec<_>, _>>()?
            .try_into()
            .map_err(|_| "transport fixture width")?;
        let actual = values(&fields).map_err(|e| format!("case {index}: {e}"))?;
        let floors = [1e-17, 1e-17, 1e-13, 1e-7, 1e-13, 1e-7, 1e-13, 1e-15];
        for (field, jet) in actual.into_iter().enumerate() {
            close(
                jet.value,
                fields[32 + field],
                2e-10,
                floors[field],
                (index, field),
            );
            for (axis, slope) in jet.gradient.into_iter().enumerate() {
                close(
                    slope,
                    fields[40 + axis * 8 + field],
                    5e-5,
                    100. * floors[field],
                    (index, field, axis),
                );
            }
        }
        count += 1;
    }
    assert_eq!(count, 864);
    Ok(())
}

#[test]
fn composed_normal_channel_conserves_charge_and_context() -> TestResult {
    for variant in [ModelVariant::HiSimHv1, ModelVariant::HiSimHv2] {
        for pmos in [false, true] {
            let card = BoundParameters::bind(
                variant,
                ParameterScope::Model,
                [
                    ("pmos", ParameterValue::Flag(pmos)),
                    ("nmos", ParameterValue::Flag(!pmos)),
                ],
            )?;
            let model = prepare_hisim_hv_model(&card)?;
            let raw = BoundParameters::bind(
                variant,
                ParameterScope::Instance,
                [
                    ("w", ParameterValue::Real(10e-6)),
                    ("l", ParameterValue::Real(3e-6)),
                    ("m", ParameterValue::Real(2.)),
                ],
            )?;
            let instance = prepare_hisim_hv_instance(&model, &raw)?;
            let channel = prepare_hisim_hv_normal_channel(&model, &instance)?;
            let temp = prepare_hisim_hv_transport(&model, &instance, 310., 0.)?;
            for vgs in [-1., 0., 1., 3., 5.] {
                for vds in [0., 0.1, 2.] {
                    let bias = HiSimHvIntrinsicBias {
                        vgs,
                        vds,
                        vbs: -0.2,
                    };
                    let point = channel
                        .evaluate(bias, &temp)
                        .map_err(|e| format!("{variant:?} pmos={pmos} bias={bias:?}: {e}"))?;
                    let q = point.electrostatics.terminal_charges;
                    let charges = [q.gate, q.drain, q.source, q.bulk];
                    close(charges.iter().map(|q| q.value).sum(), 0., 0., 1e-24, bias);
                    for axis in 0..4 {
                        close(
                            charges.iter().map(|q| q.gradient[axis]).sum(),
                            0.,
                            0.,
                            1e-24,
                            (bias, axis),
                        );
                    }
                    assert!(point.mobility.drain_current_a.value >= 0.);
                    if vds == 0. {
                        assert_eq!(point.mobility.drain_current_a.value, 0.);
                    }
                }
            }
            let other = prepare_hisim_hv_instance(&model, &raw)?;
            let other_temp = prepare_hisim_hv_transport(&model, &other, 310., 0.)?;
            assert!(
                channel
                    .evaluate(
                        HiSimHvIntrinsicBias {
                            vgs: 2.,
                            vds: 0.1,
                            vbs: 0.
                        },
                        &other_temp
                    )
                    .is_err()
            );
        }
    }
    Ok(())
}
