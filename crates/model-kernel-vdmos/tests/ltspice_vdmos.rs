//! Independent `LTspice` XVII and 24.0.12 measured references.
//! The probes isolate the channel (no series resistors) to distinguish
//! compact-law agreement from solver convergence and simulator defaults.
use model_kernel_vdmos::{Bias, Instance, Polarity, compile_ngspice};
use model_normalize_ngspice::vdmos_ltspice::translate_electrical;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn compare(actual: f64, reference: f64, context: &str) {
    assert!(
        (actual - reference).abs() <= 2e-6_f64.mul_add(reference.abs(), 3e-14),
        "{context}: Rust={actual:e}, LTspice={reference:e}"
    );
}

#[test]
fn legacy_subthreshold_transfer_curve_and_cgso_alias() -> Result<()> {
    // Measured with .dc Vg 2.5 3.5 .05, Vd=0.01, Vto=3, Kp=2.
    // LTspice .ac list 1meg and Cgdmin=Cgdmax=0 separately establish
    // Cgso=2n => I(Vg)=-j0.01256637061435917 A.
    for (fields, expected) in [
        ([("cgs", 1e-9), ("cgso", 2e-9)], 2e-9_f64),
        ([("cgso", 2e-9), ("cgs", 1e-9)], 1e-9_f64),
    ] {
        let translated = translate_electrical(&fields)?;
        let model = compile_ngspice(Polarity::N, translated.electrical)?;
        let result = model.bind(Instance::default())?.evaluate(Bias {
            vgs: 4.0,
            vds: 1.0,
            temperature: 300.15,
            ..Bias::default()
        })?;
        assert_eq!(result.qgs.d_voltage.to_bits(), expected.to_bits());
    }

    let source = [("kp", 2.0), ("vto", 3.0), ("subthres", 0.03)];
    let translated = translate_electrical(&source)?;
    let model = compile_ngspice(Polarity::N, translated.electrical)?;
    let device = model.bind(Instance::default())?;
    for (vg, reference) in [
        (2.5, 2.883_131_912_260_32e-5),
        (2.6, 1.051_026_677_213_881e-4),
        (2.7, 2.862_081_907_789_241e-4),
        (2.8, 6.094_801_241_427_686e-4),
        (2.9, 1.152_597_115_350_923e-3),
        (3.0, 1.989_703_745_903_384e-3),
        (3.1, 3.152_597_115_350_909e-3),
        (3.2, 4.609_480_124_142_737e-3),
        (3.3, 6.286_208_190_778_88e-3),
        (3.4, 8.105_102_667_721_338e-3),
        (3.5, 1.000_738_960_678_178e-2),
    ] {
        let result = device.evaluate(Bias {
            vgs: vg,
            vds: 0.01,
            temperature: 300.15,
            ..Bias::default()
        })?;
        compare(result.channel.value, reference, "legacy transfer");
    }
    Ok(())
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "Keep measured reference matrix together with its test"
)]
fn real_catalog_channel_coefficients_with_c47_overflow_match_ltspice() -> Result<()> {
    // Channel coefficients from independently obtained HenniePeters
    // standard.mos declarations. Isolate intrinsic channel by setting
    // series resistance to zero and diode Is=1e-28 in the LTspice deck.
    // Vd=polarity*0.25, Vg=polarity*(abs(Vto)+Vx).
    // .dc Vx -.4 1.2 .4, .temp 27, gmin=1e-30.
    struct Case {
        name: &'static str,
        polarity: Polarity,
        kp: f64,
        vto: f64,
        mtriode: f64,
        width: f64,
        lambda: f64,
        current: [f64; 5],
    }
    for case in [
        Case {
            name: "AO4614B_N",
            polarity: Polarity::N,
            kp: 19.4363,
            vto: 2.68,
            mtriode: 1.2,
            width: 1e-4,
            lambda: 0.14,
            current: [
                0.0,
                4.832_533_463_216_93e-8,
                1.508_742_787_500_001,
                3.922_731_247_500_003,
                6.336_719_707_499_999,
            ],
        },
        Case {
            name: "AOD4185",
            polarity: Polarity::P,
            kp: 33.6973,
            vto: 2.07,
            mtriode: 0.5,
            width: 1e-4,
            lambda: 0.04,
            current: [
                0.0,
                8.175_934_519_683_015e-8,
                1.435_820_892_187_499,
                3.137_534_542_187_501,
                4.839_248_192_187_499,
            ],
        },
        Case {
            name: "AON2392",
            polarity: Polarity::N,
            kp: 25.3986,
            vto: 2.09,
            mtriode: 0.9,
            width: 1e-3,
            lambda: 0.02,
            current: [
                0.0,
                6.131_924_044_448_095e-6,
                1.651_186_797_187_499,
                3.948_490_167_187_497,
                6.245_793_537_187_499,
            ],
        },
        Case {
            name: "AON6278",
            polarity: Polarity::N,
            kp: 96.6,
            vto: 3.15,
            mtriode: 1.1,
            width: 0.0,
            lambda: 0.15,
            current: [
                0.0,
                0.0,
                7.234_811_718_749_995,
                18.259_286_718_75,
                29.283_761_718_749_99,
            ],
        },
        Case {
            name: "AON7407",
            polarity: Polarity::P,
            kp: 91.0922,
            vto: 0.65,
            mtriode: 0.6,
            width: 1e-3,
            lambda: 0.01,
            current: [
                0.0,
                2.193_746_791_976_162e-5,
                4.451_846_611_875_002,
                9.931_042_441_875,
                15.410_238_271_875,
            ],
        },
        Case {
            name: "AON7423",
            polarity: Polarity::P,
            kp: 267.2604,
            vto: 0.67,
            mtriode: 1.0,
            width: 1e-3,
            lambda: 0.01969,
            current: [
                0.0,
                6.451_907_176_718_841e-5,
                18.464_599_265_681_25,
                45.322_198_197_581_25,
                72.179_797_129_481_23,
            ],
        },
        Case {
            name: "AONR66922",
            polarity: Polarity::N,
            kp: 80.522,
            vto: 2.52,
            mtriode: 1.6,
            width: 1e-3,
            lambda: 0.03,
            current: [
                0.0,
                1.948_859_518_431_656e-5,
                6.490_073_199_999_998,
                19.470_219_600_000_01,
                32.450_366_000_000_03,
            ],
        },
        Case {
            name: "AONS66916",
            polarity: Polarity::N,
            kp: 63.0,
            vto: 3.52,
            mtriode: 1.2,
            width: 0.0,
            lambda: 0.07,
            current: [0.0, 0.0, 4.807_687_5, 12.499_987_5, 20.192_287_500_000_01],
        },
    ] {
        let sign = case.polarity.sign();
        let model = compile_ngspice(
            case.polarity,
            [
                ("kp", case.kp),
                ("vto", sign * case.vto),
                ("mtriode", case.mtriode),
                ("ksubthres", case.width),
                ("lambda", case.lambda),
                ("is", 1e-28),
            ],
        )?;
        let device = model.bind(Instance::default())?;
        for (overdrive, expected) in [-0.4, 0.0, 0.4, 0.8, 1.2].into_iter().zip(case.current) {
            let result = device.evaluate(Bias {
                vgs: sign * (case.vto + overdrive),
                vds: sign * 0.25,
                temperature: 300.15,
                ..Bias::default()
            })?;
            compare(sign * result.channel.value, expected, case.name);
        }
    }
    Ok(())
}
