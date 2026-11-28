// BSIM3v3.3.0 numerical extraction from codeandsolder/ngspice-rs-fast
// revision 439ea0903380e7f5e2a651653f595150bad61e9e. See package LICENSE.
//! BSIM3v3.3 noise generators, a port of `b3noi.c`.
//!
//! C's `BSIM3noiMod` selects the channel-thermal and flicker laws
//! independently: `1`/`3` use the SPICE2 `2/3 |gm+gds+gmbs|` thermal source,
//! `2`/`4` the BSIM3 `qinv` source, `5`/`6` the SPICE2 source with the
//! linear/saturation fix, and `1`/`4`/`5` the SPICE2 flicker law while
//! `2`/`3`/`6` combine the strong-inversion and weak-inversion flicker
//! densities. Values outside 1..6 leave C's `noizDens` uninitialized, so they
//! are refused here instead of being given a substitute law.
#![expect(
    clippy::similar_names,
    reason = "C-translated equation symbols keep their original distinct names for source-level audit"
)]
#![expect(
    clippy::too_many_lines,
    reason = "a complete translated C numerical routine remains contiguous for equation parity review"
)]
#![expect(
    clippy::many_single_char_names,
    reason = "physical-model equation symbols are kept consistent with the ngspice C reference"
)]

use super::charges::ChargeEvaluation;
use super::dc::DcEvaluation;
use super::defaults::ModelDefaults;
use super::setup::InstanceSetup;
use super::{Error, Result};

/// `const.h` `CHARGE`.
const CHARGE: f64 = 1.602_176_620_8e-19;
/// `noisedef.h` `N_MINLOG`.
const N_MINLOG: f64 = 1.0e-38;

/// The bias-dependent part of every generator C's `BSIM3noise` emits, with the
/// multiplicity already applied as C applies it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NoiseDensities {
    /// Thermal conductance of the drain series resistance
    /// (C `drainConductance * m`), injected between the drain and its prime node.
    pub drain_conductance: f64,
    /// The same for the source.
    pub source_conductance: f64,
    /// The channel thermal source's conductance argument to `NevalSrc`
    /// (injected between the drain and source prime nodes).
    pub channel: f64,
    /// The flicker source's bias-dependent numerator, after the C operation
    /// `noizDens *= <factor>` in `N_GAIN`, injected between the drain and source
    /// prime nodes.
    pub flicker_numerator: f64,
    /// C's three frequency-law factors after `pow(f, EF)`.
    pub flicker_factors: [f64; 3],
    /// C's `EF`.
    pub flicker_exponent: f64,
}

/// The BSIM3 noise model for one defaulted model and one set-up instance.
#[derive(Debug, Clone, Copy)]
pub struct NoiseModel<'a> {
    /// The defaulted model (`b3set.c`).
    pub model: &'a ModelDefaults,
    /// This instance's effective parameters (`b3temp.c`).
    pub setup: &'a InstanceSetup,
}

impl NoiseModel<'_> {
    /// Evaluate the generator laws at `dc`/`charge` and the circuit
    /// temperature `circuit_kelvin` (C `ckt->CKTtemp`).
    ///
    /// # Errors
    /// A `noiMod` outside 1..6, or a nonfinite result.
    pub fn densities(
        &self,
        dc: &DcEvaluation,
        charge: &ChargeEvaluation,
        circuit_kelvin: f64,
    ) -> Result<NoiseDensities> {
        let noi_mod = self.model.values.noimod.value;
        if !(1..=6).contains(&noi_mod) {
            return Err(Error::unsupported(
                format!(
                    "BSIM3 noiMod={noi_mod} noise (C leaves its densities \
                     uninitialized; only noiMod 1..6 are defined)"
                ),
                "src/spicelib/devices/bsim3/b3noi.c",
            ));
        }
        let p = self.setup;
        let m = self.model;
        let multiplier = p.multiplier;
        let cox = m.oxide_capacitance;
        let leff = p.leff;
        let weff = p.weff;
        let ueff = dc.ueff;
        let cd = dc.ids;

        // Channel thermal noise (`b3noi.c`'s `BSIM3IDNOIZ`).
        let channel = match noi_mod {
            1 | 3 => 2.0 * (dc.gm + dc.gds + dc.gmbs).abs() / 3.0 * multiplier,
            5 | 6 => {
                // C clamps the drain voltage it reads from state by `vdsat`.
                let vds = dc.vds.min(dc.vdsat);
                (3.0 - vds / dc.vdsat) * (dc.gm + dc.gds + dc.gmbs).abs() / 3.0 * multiplier
            }
            _ => {
                // `m * ueff * |qinv| / (leff^2 + ueff * |qinv| * rds)`.
                let qinv = charge.qinv.abs();
                multiplier * ueff * qinv / (ueff * qinv).mul_add(dc.rds, leff * leff)
            }
        };

        // Flicker noise (`b3noi.c`'s `BSIM3FLNOIZ`).
        let (flicker_numerator, flicker_factors) = match noi_mod {
            1 | 4 | 5 => {
                let noise_current = cd.abs().max(N_MINLOG);
                let numerator =
                    multiplier * m.values.kf.value * (m.values.af.value * noise_current.ln()).exp();
                (numerator, [leff, leff, cox])
            }
            _ => {
                // C's strong-inversion/weak-inversion blend. Both densities
                // carry one `pow(f, EF)` in their denominators, so the blend
                // keeps the same law and the numerator below is the
                // frequency-free part.
                let lintnoi = m.values.lintnoi.value;
                let leff_noi = 2.0f64.mul_add(-lintnoi, leff);
                let leff_noi_sq = leff_noi * leff_noi;
                let vgsteff = dc.vgsteff;
                let vdseff = dc.vdseff;
                let abulk = dc.abulk;
                let em = m.values.em.value;
                let vsattemp = p.thermal.vsattemp;
                let litl = p.derived.litl;
                let esat = 2.0 * vsattemp / ueff;
                let del_clm = if em <= 0.0 {
                    0.0
                } else {
                    let t0 = ((dc.vds - vdseff) / litl + em) / esat;
                    (litl * t0.max(N_MINLOG).ln()).max(0.0)
                };
                let n0 = cox * vgsteff / CHARGE;
                let channel_drop = dc.abulk_over_vgst2vtm * vdseff;
                let nl = cox * vgsteff * (1.0 - channel_drop) / CHARGE;
                let density_drop = n0 * channel_drop;
                let density_ratio = (n0 + 2.0e14) / (nl + 2.0e14);
                let log_ratio = if density_ratio > 0.5 && density_ratio < 2.0 {
                    (density_drop / (nl + 2.0e14)).ln_1p()
                } else {
                    density_ratio.max(N_MINLOG).ln()
                };
                let noia = m.values.noia.value;
                let noib = m.values.noib.value;
                let noic = m.values.noic.value;
                let thermal = 8.62e-5 * circuit_kelvin;
                // `Ssi = A / pow(f,EF)`, `Swi = C / pow(f,EF)`.
                let a = CHARGE
                    * CHARGE
                    * thermal
                    * cd
                    * ueff
                    * (noic * 0.5 * density_drop)
                        .mul_add(n0 + nl, noib.mul_add(density_drop, noia * log_ratio))
                    / (1.0e8 * abulk * cox * leff_noi_sq);
                let b = thermal * cd * cd / (1.0e8 * leff_noi_sq * weff)
                    * del_clm
                    * (noic * nl).mul_add(nl, noib.mul_add(nl, noia))
                    / ((nl + 2.0e14) * (nl + 2.0e14));
                let c = noia * thermal * cd * cd / (weff * leff * 4.0e36);
                let sum = a + b + c;
                let numerator = if sum > 0.0 {
                    multiplier * (a + b) * c / sum
                } else {
                    0.0
                };
                (numerator, [1.0, 1.0, 1.0])
            }
        };

        let result = NoiseDensities {
            drain_conductance: p.drain_conductance * multiplier,
            source_conductance: p.source_conductance * multiplier,
            channel,
            flicker_numerator,
            flicker_factors,
            flicker_exponent: m.values.ef.value,
        };
        for (name, value) in [
            ("drain conductance", result.drain_conductance),
            ("source conductance", result.source_conductance),
            ("channel", result.channel),
            ("flicker numerator", result.flicker_numerator),
            ("flicker exponent", result.flicker_exponent),
        ] {
            if !value.is_finite() {
                return Err(Error::Numerical {
                    context: "BSIM3 noise evaluation (bsim3/b3noi.c)".into(),
                    message: format!("{name} is not finite"),
                });
            }
        }
        Ok(result)
    }
}
