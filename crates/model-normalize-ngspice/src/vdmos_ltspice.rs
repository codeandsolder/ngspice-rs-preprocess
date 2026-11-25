//! Explicit translation of verified `LTspice` VDMOS extensions to native
//! ngspice electrical fields.
//!
//! Thermal/SOA observation coefficients are kept
//! separate: `LTspice`'s monitoring ladder does not feed back into the
//! three-terminal electrical model, unlike ngspice's thermal instance.
use anyhow::{Result, ensure};

/// Resolved source fields separated by physical ownership.
///
/// This is a field translation, not a claim that all implicit `LTspice` and
/// ngspice defaults or unrecognized model extensions are equivalent.
#[derive(Debug)]
pub struct VdmosElectricalTranslation<'a> {
    /// Native electrical parameter pairs, preserving source order and
    /// last-definition precedence. Unknown fields remain for validation.
    pub electrical: Vec<(&'a str, f64)>,
    /// Original thermal/SOA observation fields, not ngspice Rthjc/Rthca.
    /// Consumers requesting observation outputs must process these explicitly.
    pub thermal_observation: Vec<(&'a str, f64)>,
}

/// Translate documented and independently measured VDMOS source extensions.
///
/// Cgso and Cgs share a slot; positive explicit Ksubthres supersedes the
/// legacy Subthres conversion, while zero allows the legacy conversion.
/// No unspecified target defaults are changed, except Kp=1 when required
/// for a legacy Subthres conversion without an explicit source Kp.
///
/// # Errors
/// Rejects nonfinite source fields and positive legacy Subthres with a
/// nonpositive transconductance factor.
pub fn translate_electrical<'a>(
    parameters: &[(&'a str, f64)],
) -> Result<VdmosElectricalTranslation<'a>> {
    let mut electrical = Vec::with_capacity(parameters.len());
    let mut thermal_observation = Vec::new();
    let mut kp = None;
    let mut legacy = None;
    let mut width = None;
    let thermal = parameters.iter().any(|(name, _)| {
        name.eq_ignore_ascii_case("rthetaja")
            || name.eq_ignore_ascii_case("imult")
            || name.eq_ignore_ascii_case("iexponent")
    });
    for &(name, value) in parameters {
        ensure!(value.is_finite(), "nonfinite VDMOS field {name}");
        let lower = name.to_ascii_lowercase();
        match lower.as_str() {
            "subthres" => legacy = Some(value),
            "ksubthres" => {
                width = Some(value);
                electrical.push((name, value));
            }
            "kp" => {
                kp = Some(value);
                electrical.push((name, value));
            }
            "cgso" => electrical.push(("cgs", value)),
            "rthetaja" | "imult" | "iexponent" => thermal_observation.push((name, value)),
            _ if thermal && is_ladder_field(&lower) => thermal_observation.push((name, value)),
            _ => electrical.push((name, value)),
        }
    }
    if let Some(subthres) = legacy
        && width.is_none_or(|value| value == 0.0)
    {
        let factor = kp.unwrap_or(1.0);
        if kp.is_none() {
            electrical.push(("kp", factor));
        }
        electrical.push(("ksubthres", legacy_subthreshold_width(subthres, factor)?));
    }
    Ok(VdmosElectricalTranslation {
        electrical,
        thermal_observation,
    })
}

fn is_ladder_field(name: &str) -> bool {
    let Some(index) = name.strip_prefix('r').or_else(|| name.strip_prefix('c')) else {
        return false;
    };
    !index.starts_with('0') && !index.is_empty() && index.bytes().all(|b| b.is_ascii_digit())
}

/// Convert legacy Subthres (A/V) to the softplus voltage width.
///
/// The original square-law transition is x0=sqrt(2*Subthres/Kp), limited
/// to the 1 V reference drain bias. Match its tangent exponential
/// continuation at Vgs-Vto=-1 V, as described by the `LTspice` author.
/// Use the historical rounded log(1+exp()) in the *source calibration*,
/// not log1p: independent `LTspice` XVII and 24.0.12 measurements reveal
/// the IEEE-754 quantization floor at exp(-1/s)=2^-53. The native runtime
/// evaluation itself must use stable scaled softplus and exact derivatives.
///
/// # Errors
/// Rejects nonfinite inputs and positive Subthres with nonpositive Kp.
pub fn legacy_subthreshold_width(subthres: f64, kp: f64) -> Result<f64> {
    ensure!(
        subthres.is_finite() && kp.is_finite(),
        "nonfinite legacy Subthres"
    );
    if subthres <= 0.0 {
        return Ok(0.0);
    }
    ensure!(kp > 0.0, "positive legacy Subthres requires positive Kp");
    let x0 = (2.0 * (subthres / kp).min(0.5)).sqrt();
    if x0 == 0.0 {
        return Ok(0.0);
    }
    // Original reference current divided by Kp. A fully underflowed
    // reference current disables the conversion in both measured releases.
    let target = 2.0_f64
        .mul_add(x0.ln() - 1.0 - x0.recip(), -std::f64::consts::LN_2)
        .exp();
    if target == 0.0 {
        return Ok(0.0);
    }

    // The source conversion is monotone including its rounded logarithm.
    // A unit-width upper bracket covers every x0 <= 1 V; unlike x0 itself
    // it also covers the observed small-current quantization floor.
    let mut lo = 0.0_f64;
    let mut hi = 1.0_f64;
    for _ in 0..64 {
        let mid = lo.midpoint(hi);
        // Preserve LTspice calibration rounding, including the measured quantization floor.
        #[expect(
            clippy::imprecise_flops,
            reason = "Measured LTspice calibration requires rounded log argument"
        )]
        let drive = mid * (1.0 + (-mid.recip()).exp()).ln();
        if 0.5 * drive * drive < target {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    Ok(lo.midpoint(hi))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_conversion_matches_independent_ltspice_measurements() -> Result<()> {
        // Recovered from saturation current at nominal Vgs=Vto, using
        // Kp/2*(s*ln(2))^2. Measured with both LTspice XVII and 24.0.12.
        for (kp, subthres, expected) in [
            (1.0, 1e-6, 0.0),
            (1.0, 3.7e-6, 0.0),
            (1.0, 3.8e-6, 0.027_220_661_487_874_442),
            (1.0, 0.0004, 0.027_220_661_487_874_442),
            (1.0, 0.000_407, 0.028_059_789_318_833_114),
            (1.0, 0.00045, 0.029_121_488_465_480_37),
            (1.0, 0.0005, 0.030_680_807_980_493_91),
            (1.0, 0.001, 0.042_883_993_214_732_19),
            (1.0, 0.5, 0.668_894_868_733_882_5),
            (1.0, 1.0, 0.668_894_868_733_882_5),
            (1.0, 0.03, 0.204_212_119_349_703_62),
            (2.0, 0.03, 0.150_740_261_583_106_77),
            (4.0, 0.03, 0.110_365_498_107_615_46),
            (20.0, 0.03, 0.052_065_088_938_543_39),
            (2.0, 0.12, 0.274_610_784_199_039_45),
            (2.0, 0.0075, 0.080_191_849_108_159_75),
            (300.0, 0.014, 0.027_220_661_487_874_442),
        ] {
            let actual = legacy_subthreshold_width(subthres, kp)?;
            assert!(
                (actual - expected).abs() <= 5e-7 * expected,
                "Kp={kp} Subthres={subthres}: {actual:e} vs {expected:e}"
            );
        }
        Ok(())
    }

    #[test]
    fn translation_preserves_precedence_and_observation_fields() -> Result<()> {
        let source = [
            ("Cgs", 1e-9),
            ("CGSO", 2e-9),
            ("Cgs", 3e-9),
            ("Subthres", 0.03),
            ("Kp", 2.0),
            ("Ksubthres", 0.0),
            ("RthetaJA", 50.0),
            ("R6", 0.3),
            ("C6", 1e-6),
            ("Imult", 10.0),
            ("Iexponent", 0.3),
            ("UNKNOWN", 1.0),
        ];
        let translated = translate_electrical(&source)?;
        assert_eq!(translated.thermal_observation, source[6..11]);
        assert_eq!(
            translated.electrical[..3],
            [("Cgs", 1e-9), ("cgs", 2e-9), ("Cgs", 3e-9)]
        );
        assert_eq!(
            translated.electrical.last().map(|pair| pair.0),
            Some("ksubthres")
        );
        assert!(translated.electrical.contains(&("UNKNOWN", 1.0)));
        let explicit = translate_electrical(&[("Ksubthres", 0.07), ("Subthres", 0.03)])?;
        assert_eq!(explicit.electrical, [("Ksubthres", 0.07)]);
        let standalone = translate_electrical(&[("R6", 0.3)])?;
        assert_eq!(standalone.electrical, [("R6", 0.3)]);
        assert_eq!(standalone.thermal_observation, [] as [(&str, f64); 0]);
        Ok(())
    }
}
