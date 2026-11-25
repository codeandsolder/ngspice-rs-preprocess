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
fn synthetic_channel_extreme_weak_inversion_matches_closed_form() -> Result<()> {
    // Independently chosen dimensions exercise both polarities, extreme
    // positive/negative softplus arguments, and the zero-width limit.
    // At nominal temperature with theta=0, the ideal channel law is
    // beta*(u*x - x²/2), where x=min(u, effective drain voltage).
    for polarity in [Polarity::N, Polarity::P] {
        let sign = polarity.sign();
        for (kp, vto, mtriode, width, lambda) in [
            (12.0, 2.0, 0.5, 0.0_f64, 0.0),
            (27.0, 1.5, 1.25, 1e-4, 0.08),
            (45.0, 3.5, 0.75, 1e-3, 0.02),
            (100.0, 2.5, 1.5, 1e-5, 0.1),
        ] {
            let model = compile_ngspice(
                polarity,
                [
                    ("kp", kp),
                    ("vto", sign * vto),
                    ("mtriode", mtriode),
                    ("ksubthres", width),
                    ("lambda", lambda),
                    ("is", 1e-28),
                ],
            )?;
            let device = model.bind(Instance::default())?;
            for overdrive in [-0.4_f64, 0.0, 0.4, 0.8, 1.2] {
                let u = if width > 0.0 {
                    width.mul_add((-overdrive.abs() / width).exp().ln_1p(), overdrive.max(0.0))
                } else {
                    overdrive.max(0.0)
                };
                let x = u.min(0.25 * mtriode);
                let expected = kp * 0.25_f64.mul_add(lambda, 1.0) * x * 0.5_f64.mul_add(-x, u);
                let result = device.evaluate(Bias {
                    vgs: sign * (vto + overdrive),
                    vds: sign * 0.25,
                    temperature: 300.15,
                    ..Bias::default()
                })?;
                compare(sign * result.channel.value, expected, "synthetic channel");
            }
        }
    }
    Ok(())
}
