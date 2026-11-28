//! Independent coefficient and contact-resistance checks of Tables 111–112.
use model_compact_intrinsic::{
    Bsim4ChannelModifiers, Bsim4OccurrenceModifiers, Bsim4PrimaryCoefficients, Bsim4SeriesModifiers,
};

fn near(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() <= 2e-14 * expected.abs().max(1e-30),
        "{actual} != {expected}"
    );
}

#[test]
fn channel_changes_are_local_to_each_resolved_bin_and_preserve_signed_threshold()
-> Result<(), Box<dyn std::error::Error>> {
    let modifiers = Bsim4ChannelModifiers {
        threshold_shift_v: 0.1,
        mobility_multiplier: 1.25,
        body_effect_shift_sqrt_v: -0.5,
        subthreshold_factor_shift: -0.1,
    };
    for threshold in [-0.75, 0.75] {
        for (k1, expected, reset) in [(0.3, 0.3, true), (0.5, 0., false), (0.8, 0.3, false)] {
            let base = Bsim4PrimaryCoefficients {
                threshold_voltage_v: threshold,
                mobility_m2_per_vs: 0.04,
                body_effect_sqrt_v: k1,
                subthreshold_factor: 1.2,
            };
            let modified = modifiers.apply(base)?;
            near(modified.coefficients.threshold_voltage_v, threshold + 0.1);
            near(modified.coefficients.mobility_m2_per_vs, 0.05);
            near(modified.coefficients.body_effect_sqrt_v, expected);
            near(modified.coefficients.subthreshold_factor, 1.1);
            assert_eq!(modified.resets.body_effect_shift, reset);
            assert!(!modified.resets.mobility_multiplier);
            assert_eq!(
                Bsim4OccurrenceModifiers::default()
                    .channel
                    .apply(base)?
                    .coefficients,
                base
            );
            for multiplier in [-1., 0., 2.] {
                let other = Bsim4ChannelModifiers {
                    mobility_multiplier: multiplier,
                    ..modifiers
                }
                .apply(base)?;
                near(
                    other.coefficients.mobility_m2_per_vs,
                    if multiplier < 0. {
                        0.04
                    } else {
                        multiplier * 0.04
                    },
                );
                assert_eq!(other.resets.mobility_multiplier, multiplier < 0.);
            }
        }
    }
    Ok(())
}

#[test]
fn contacts_scale_per_finger_and_temperature_correct_the_full_terminal()
-> Result<(), Box<dyn std::error::Error>> {
    let modifiers = Bsim4SeriesModifiers {
        contact_per_finger_ohm: [12., -9.],
        temperature_per_k: [0.002, -0.001],
    };
    for fingers in [1., 2., 4., 7.5] {
        for temperature in [250., 300., 400.] {
            let prepared = modifiers.prepare([3., 8.], fingers, 300., temperature)?;
            let delta = temperature - 300.;
            near(
                prepared.resistance_ohm[0],
                (3. + 12. / fingers) * (1. + 0.002 * delta),
            );
            near(prepared.resistance_ohm[1], 8. * (1. - 0.001 * delta));
            assert_eq!(prepared.reset_contact, [false, true]);
        }
    }
    // Contact resistance alone requires a series branch; missing modifiers do not.
    let contact_only = modifiers.prepare([0., 0.], 4., 300., 300.)?;
    assert_eq!(contact_only.resistance_ohm, [3., 0.]);
    assert_eq!(
        Bsim4SeriesModifiers::default()
            .prepare([0., 0.], 4., 300., 300.)?
            .resistance_ohm,
        [0., 0.]
    );
    for fingers in [0., -1., f64::NAN, f64::INFINITY] {
        assert!(modifiers.prepare([3., 8.], fingers, 300., 300.).is_err());
    }
    assert!(modifiers.prepare([-1., 8.], 2., 300., 300.).is_err());
    assert!(modifiers.prepare([3., 8.], 2., 300., 1500.).is_err());
    assert!(
        Bsim4SeriesModifiers {
            contact_per_finger_ohm: [f64::NAN, 1.],
            ..modifiers
        }
        .prepare([3., 8.], 2., 300., 300.)
        .is_err()
    );
    Ok(())
}
