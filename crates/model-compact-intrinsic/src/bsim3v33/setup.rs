// BSIM3v3.3.0 numerical extraction from codeandsolder/ngspice-rs-fast
// revision 439ea0903380e7f5e2a651653f595150bad61e9e. See package LICENSE.
//! BSIM3 instance setup and parameter validation.
//!
//! Instance setup: `b3set.c`'s instance defaulting, `b3temp.c`'s
//! size-dependent and temperature-dependent parameters, and `b3check.c`'s
//! model/instance diagnostics. This is geometry and parameter setup only: no
//! nodes are interned, no states are allocated and no load equations run.
#![expect(
    clippy::similar_names,
    reason = "C-translated equation symbols keep their original distinct names for source-level audit"
)]
#![expect(
    clippy::float_cmp,
    reason = "C device branches compare floating-point values exactly, including nonfinite cases"
)]
#![expect(
    clippy::too_many_lines,
    reason = "a complete translated C numerical routine remains contiguous for equation parity review"
)]

use super::defaults::{ModelDefaults, ModelValues};
use super::temperature::{METER2MICRON, TemperatureModel};
use super::{Diagnostic, Error, Result};
use crate::{BoundParameters, ModelError, ModelVariant, ParameterScope, ParameterValue};
use std::sync::Arc;

const EXP_THRESHOLD: f64 = 34.0;
const MIN_EXP: f64 = 1.713_908_431e-15;

/// A C printf `%g` spelling for `b3check.c` messages.
fn c_g(value: f64) -> String {
    if value == 0.0 {
        return if value.is_sign_negative() {
            "-0".into()
        } else {
            "0".into()
        };
    }
    if !value.is_finite() {
        return if value.is_nan() {
            if value.is_sign_negative() {
                "-nan"
            } else {
                "nan"
            }
            .into()
        } else if value.is_sign_negative() {
            "-inf".into()
        } else {
            "inf".into()
        };
    }
    let scientific = format!("{value:.5e}");
    let Some((mantissa, exponent)) = scientific.split_once('e') else {
        return scientific;
    };
    let Ok(exponent) = exponent.parse::<i32>() else {
        return scientific;
    };
    if (-4..6).contains(&exponent) {
        let decimals = usize::try_from(5 - exponent).unwrap_or(0);
        let fixed = format!("{value:.decimals$}");
        // In fixed format only fractional trailing zeros are insignificant.
        if decimals == 0 {
            fixed
        } else {
            fixed.trim_end_matches('0').trim_end_matches('.').to_owned()
        }
    } else {
        let mantissa = mantissa.trim_end_matches('0').trim_end_matches('.');
        format!(
            "{mantissa}e{}{:02}",
            if exponent < 0 { '-' } else { '+' },
            exponent.unsigned_abs()
        )
    }
}

/// Effective geometry, size-dependent parameters and instance-level setup
/// results. Every `pparam` entry is a `b3temp.c` `pParam` field by keyword.
#[derive(Debug, Clone, PartialEq)]
pub struct InstanceSetup {
    /// Scaled deck L (C `here->BSIM3l`), after the 5 µm default.
    pub length: f64,
    /// Scaled deck W (C `here->BSIM3w`), after the 5 µm default.
    pub width: f64,
    /// Effective length (C `pParam->BSIM3leff`).
    pub leff: f64,
    /// Effective width (C `pParam->BSIM3weff`).
    pub weff: f64,
    /// Effective length for C-V.
    pub leff_cv: f64,
    /// Effective width for C-V.
    pub weff_cv: f64,
    /// Model polarity (+1 NMOS, -1 PMOS).
    pub polarity: i8,
    /// Instance multiplicity (C `here->BSIM3m`).
    pub multiplier: f64,
    /// NQS selector after instance/model resolution.
    pub nqs_mod: i32,
    /// AC NQS selector after instance/model resolution.
    pub acnqs_mod: i32,
    /// Geometry selector (C `here->BSIM3geo`).
    pub geo: i32,
    /// Drain/source junction areas (m²).
    pub drain_area: f64,
    /// Source junction area (m²).
    pub source_area: f64,
    /// Drain junction perimeter (m).
    pub drain_perimeter: f64,
    /// Source junction perimeter (m).
    pub source_perimeter: f64,
    /// Drain squares (ACM off: C defaults to 1).
    pub drain_squares: f64,
    /// Source squares (ACM off: C defaults to 1).
    pub source_squares: f64,
    /// Instance threshold shift (V).
    pub delvto: f64,
    /// Instance mobility multiplier.
    pub mulu0: f64,
    /// Initial VBS.
    pub ic_vbs: f64,
    /// Initial VDS.
    pub ic_vds: f64,
    /// Initial VGS.
    pub ic_vgs: f64,
    /// Instance threshold after `delvto` (C `here->BSIM3vth0`).
    pub vth0: f64,
    /// Instance flat-band voltage after `delvto`.
    pub vfb: f64,
    /// Instance zero-bias flat-band voltage after `delvto`.
    pub vfbzb: f64,
    /// Temperature-adjusted mobility times `mulu0`.
    pub u0temp: f64,
    /// C's recomputed `tconst` (see `InstanceSetup::new` for its quirk).
    pub tconst: f64,
    /// Gate-source overlap capacitance (F).
    pub cgso: f64,
    /// Gate-drain overlap capacitance (F).
    pub cgdo: f64,
    /// Drain series conductance (S).
    pub drain_conductance: f64,
    /// Source series conductance (S).
    pub source_conductance: f64,
    /// Source junction saturation current (A).
    pub source_sat_current: f64,
    /// Drain junction saturation current (A).
    pub drain_sat_current: f64,
    /// Source junction breakdown voltage (0 when C leaves it unset).
    pub vjsm: f64,
    /// Drain junction breakdown voltage (0 when C leaves it unset).
    pub vjdm: f64,
    /// Source junction breakdown current (0 when C leaves it unset).
    pub isevjsm: f64,
    /// Drain junction breakdown current (0 when C leaves it unset).
    pub isevjdm: f64,
    /// Geometry deltas, distinct from the public effective dimensions.
    dl: f64,
    dw: f64,
    dlc: f64,
    dwc: f64,
    /// Complete binning/temperature/derivation records for numerical load.
    pub(super) binned: BinnedParameters,
    pub(super) thermal: ThermalParameters,
    pub(super) derived: DerivedParameters,
    /// Nonfatal `b3temp.c`/`b3check.c` messages in C order.
    pub warnings: Vec<Diagnostic>,
}

// The generated coefficient relation supplies the field declarations,
// binning equations and introspection names from a single source.
macro_rules! define_binned_parameters {
    ($(($base:ident, $l:ident, $w:ident, $area:ident),)*) => {
        #[derive(Debug, Clone, PartialEq)]
        pub(super) struct BinnedParameters {
            $(pub(super) $base: f64,)*
        }

        impl BinnedParameters {
            #[expect(clippy::suboptimal_flops, reason = "C binning uses separately rounded products and left-associated additions")]
const fn new(model: &ModelValues, inv_l: f64, inv_w: f64, inv_lw: f64) -> Self {
                Self {
                    $($base: model.$base.value
                        + model.$l.value * inv_l
                        + model.$w.value * inv_w
                        + model.$area.value * inv_lw,)*
                }
            }

            fn get(&self, name: &str) -> Option<f64> {
                match name {
                    $(stringify!($base) => Some(self.$base),)*
                    _ => None,
                }
            }
        }

        const BINNED_NAMES: &[&str] = &[$(stringify!($base),)*];
    };
}

super::size_data::binned_fields!(define_binned_parameters);

/// Validated geometry before any binning or parameter check.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Geometry {
    dl: f64,
    dw: f64,
    dlc: f64,
    dwc: f64,
    leff: f64,
    weff: f64,
    leff_cv: f64,
    weff_cv: f64,
}

impl Geometry {
    fn new(model: &ModelDefaults, length: f64, width: f64) -> Result<Self> {
        // Size-dependent geometry (`b3temp.c`).
        let m_lln = model.values.lln.value;
        let m_lwn = model.values.lwn.value;
        let m_wln = model.values.wln.value;
        let m_wwn = model.values.wwn.value;
        let t0 = length.powf(m_lln);
        let t1 = width.powf(m_lwn);
        let dl = model.values.lint.value
            + model.values.ll.value / t0
            + model.values.lw.value / t1
            + model.values.lwl.value / (t0 * t1);
        let dlc = model.values.dlc.value
            + model.values.llc.value / t0
            + model.values.lwc.value / t1
            + model.values.lwlc.value / (t0 * t1);
        let t2 = length.powf(m_wln);
        let t3 = width.powf(m_wwn);
        let dw = model.values.wint.value
            + model.values.wl.value / t2
            + model.values.ww.value / t3
            + model.values.wwl.value / (t2 * t3);
        let dwc = model.values.dwc.value
            + model.values.wlc.value / t2
            + model.values.wwc.value / t3
            + model.values.wwlc.value / (t2 * t3);
        let leff = 2.0f64.mul_add(-dl, length + model.values.xl.value);
        let weff = 2.0f64.mul_add(-dw, width + model.values.xw.value);
        let leff_cv = 2.0f64.mul_add(-dlc, length + model.values.xl.value);
        let weff_cv = 2.0f64.mul_add(-dwc, width + model.values.xw.value);
        for (name, value, message) in [
            ("Leff", leff, "Effective channel length <= 0"),
            ("Weff", weff, "Effective channel width <= 0"),
            ("LeffCV", leff_cv, "Effective channel length for C-V <= 0"),
            ("WeffCV", weff_cv, "Effective channel width for C-V <= 0"),
        ] {
            if !(value.is_finite() && value > 0.0) {
                return Err(Error::Domain(format!(
                    "BSIM3: {message} ({name} = {})",
                    c_g(value)
                )));
            }
        }
        Ok(Self {
            dl,
            dw,
            dlc,
            dwc,
            leff,
            weff,
            leff_cv,
            weff_cv,
        })
    }

    const fn binning_factors(self, bin_unit: i32) -> (f64, f64, f64) {
        let (leff, weff) = (self.leff, self.weff);
        if bin_unit == 1 {
            (1.0e-6 / leff, 1.0e-6 / weff, 1.0e-12 / (leff * weff))
        } else {
            (1.0 / leff, 1.0 / weff, 1.0 / (leff * weff))
        }
    }
}

/// Additional quantities calculated by the temperature pass before checks.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct ThermalParameters {
    pub(super) abulkcvfactor: f64,
    pub(super) u0temp: f64,
    pub(super) vsattemp: f64,
    pub(super) rds0: f64,
}

/// Model-bound threshold, charge and depletion values derived only after
/// geometry and temperature-dependent parameter checks have succeeded.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct DerivedParameters {
    pub(super) npeak: f64,
    pub(super) phi: f64,
    pub(super) sqrtphi: f64,
    pub(super) phis3: f64,
    pub(super) xdep0: f64,
    pub(super) sqrtxdep0: f64,
    pub(super) litl: f64,
    pub(super) vbi: f64,
    pub(super) cdep0: f64,
    pub(super) ldeb: f64,
    pub(super) vbsc: f64,
    pub(super) k1ox: f64,
    pub(super) k2ox: f64,
    pub(super) theta0vb0: f64,
    pub(super) thetarout: f64,
    pub(super) vfbzb: f64,
    pub(super) cgbo: f64,
}

/// Binning and temperature scaling produce exactly these quantities before
/// the model-check pass is allowed to change any of them.
struct AdjustedSize {
    geometry: Geometry,
    binned: BinnedParameters,
    thermal: ThermalParameters,
    ratio: f64,
}

impl AdjustedSize {
    fn new(model: &ModelDefaults, temperature: &TemperatureModel, geometry: Geometry) -> Self {
        let (inv_l, inv_w, inv_lw) = geometry.binning_factors(model.values.binunit.value);
        let mut binned = BinnedParameters::new(&model.values, inv_l, inv_w, inv_lw);
        let abulkcvfactor = 1.0 + (binned.clc / geometry.leff_cv).powf(binned.cle);
        let ratio = temperature.temp / temperature.tnom;
        let t0 = ratio - 1.0;
        // C takes the temperature coefficients from the model, not the
        // binned coefficient values.
        binned.ua = model.values.ua1.value.mul_add(t0, binned.ua);
        binned.ub = model.values.ub1.value.mul_add(t0, binned.ub);
        binned.uc = model.values.uc1.value.mul_add(t0, binned.uc);
        if binned.u0 > 1.0 {
            binned.u0 /= 1.0e4;
        }
        let u0temp = binned.u0 * ratio.powf(binned.ute);
        let vsattemp = binned.at.mul_add(-t0, binned.vsat);
        let rds0 =
            binned.prt.mul_add(t0, binned.rdsw) / (geometry.weff * METER2MICRON).powf(binned.wr);
        Self {
            geometry,
            binned,
            ratio,
            thermal: ThermalParameters {
                abulkcvfactor,
                u0temp,
                vsattemp,
                rds0,
            },
        }
    }
}

/// Defaulted, typed instance input, before geometry can be computed.
struct InstanceInputs {
    length: f64,
    width: f64,
    drain_area: f64,
    source_area: f64,
    drain_perimeter: f64,
    source_perimeter: f64,
    drain_squares: f64,
    source_squares: f64,
    delvto: f64,
    mulu0: f64,
    ic_vbs: f64,
    ic_vds: f64,
    ic_vgs: f64,
    multiplier: f64,
    geo: i32,
    nqs_mod: i32,
    acnqs_mod: i32,
}

impl InstanceInputs {
    fn new(
        instance: &BoundParameters,
        model: &ModelDefaults,

        warnings: &mut Vec<Diagnostic>,
    ) -> Result<Self> {
        // `b3set.c` instance defaults, in C order.
        let given = |name: &str| instance_value(instance, name).is_some();
        let real = |name: &str| instance_real(instance, name);
        let length = if given("l") { real("l")? } else { 5.0e-6 };
        let width = if given("w") { real("w")? } else { 5.0e-6 };
        let drain_area = if given("ad") { real("ad")? } else { 0.0 };
        let source_area = if given("as") { real("as")? } else { 0.0 };
        let drain_perimeter = if given("pd") { real("pd")? } else { 0.0 };
        let source_perimeter = if given("ps") { real("ps")? } else { 0.0 };
        let default_squares = 1.0; // ACM has already been rejected.
        let drain_squares = if given("nrd") {
            real("nrd")?
        } else {
            default_squares
        };
        let source_squares = if given("nrs") {
            real("nrs")?
        } else {
            default_squares
        };
        let delvto = if given("delvto") {
            real("delvto")?
        } else {
            0.0
        };
        let mulu0 = if given("mulu0") { real("mulu0")? } else { 1.0 };
        let ic_vbs = if given("icvbs") { real("icvbs")? } else { 0.0 };
        let ic_vds = if given("icvds") { real("icvds")? } else { 0.0 };
        let ic_vgs = if given("icvgs") { real("icvgs")? } else { 0.0 };
        let multiplier = if given("m") { real("m")? } else { 1.0 };
        if !(multiplier.is_finite() && multiplier > 0.0) {
            return Err(Error::Domain(
                "BSIM3 instance multiplier must be finite and positive".into(),
            ));
        }
        let geo = if given("geo") {
            integer(instance, "geo")?
        } else {
            0
        };
        let model_nqs = model.values.nqsmod.value;
        let model_acnqs = model.values.acnqsmod.value;
        let resolve_selector = |name: &str,
                                model_value: i32,
                                label: &str,
                                warnings: &mut Vec<Diagnostic>|
         -> Result<i32> {
            if !given(name) {
                return Ok(model_value);
            }
            let value = integer(instance, name)?;
            if value == 0 || value == 1 {
                return Ok(value);
            }
            warnings.push(Diagnostic::warning(
                format!("Warning: {label} has been set to its global value {model_value}."),
                "src/spicelib/devices/bsim3/b3set.c",
            ));
            Ok(model_value)
        };
        let nqs_mod = resolve_selector("nqsmod", model_nqs, "nqsMod", warnings)?;
        let acnqs_mod = resolve_selector("acnqsmod", model_acnqs, "acnqsMod", warnings)?;
        Ok(Self {
            length,
            width,
            drain_area,
            source_area,
            drain_perimeter,
            source_perimeter,
            drain_squares,
            source_squares,
            delvto,
            mulu0,
            ic_vbs,
            ic_vds,
            ic_vgs,
            multiplier,
            geo,
            nqs_mod,
            acnqs_mod,
        })
    }
}

/// Only this stage may enter threshold derivation. The checks can mutate
/// binned fields, temperature-adjusted resistance, and shared model resets.
struct CheckedSize(AdjustedSize);

impl AdjustedSize {
    fn check(
        mut self,
        model: &ModelDefaults,
        checks: &mut ModelChecks,
        inputs: &InstanceInputs,

        warnings: &mut Vec<Diagnostic>,
    ) -> Result<CheckedSize> {
        let Self {
            geometry,
            binned,
            thermal,
            ..
        } = &mut self;
        let Geometry {
            leff,
            weff,
            leff_cv,
            weff_cv,
            ..
        } = *geometry;
        let ModelChecks {
            cgdo: model_cgdo,
            cgso: model_cgso,
            cgbo: model_cgbo,
            ..
        } = checks;
        let drain_perimeter = inputs.drain_perimeter;
        let source_perimeter = inputs.source_perimeter;
        // `b3check.c`, in C order. Fatal checks accumulate before failing.
        let mut diagnostics = Vec::new();
        let mut fatal = false;
        {
            let fail = |diagnostics: &mut Vec<Diagnostic>, message: String, fatal: &mut bool| {
                *fatal = true;
                diagnostics.push(Diagnostic::fatal(
                    message,
                    "src/spicelib/devices/bsim3/b3check.c",
                ));
            };
            let version = model.version.as_str();
            if !(version.starts_with("3.3.0")
                || version.starts_with("3.30")
                || version.starts_with("3.3"))
            {
                diagnostics.push(Diagnostic::warning(
                    "Warning: This model is BSIM3v3.3.0; you specified a wrong version number.",
                    "src/spicelib/devices/bsim3/b3check.c",
                ));
            }
            if binned.nlx < -leff {
                fail(
                    &mut diagnostics,
                    format!("Fatal: Nlx = {} is less than -Leff.", c_g(binned.nlx)),
                    &mut fatal,
                );
            }
            if model.values.tox.value <= 0.0 {
                fail(
                    &mut diagnostics,
                    format!(
                        "Fatal: Tox = {} is not positive.",
                        c_g(model.values.tox.value)
                    ),
                    &mut fatal,
                );
            }
            if model.values.toxm.value <= 0.0 {
                fail(
                    &mut diagnostics,
                    format!(
                        "Fatal: Toxm = {} is not positive.",
                        c_g(model.values.toxm.value)
                    ),
                    &mut fatal,
                );
            }
            if model.values.lintnoi.value > leff / 2.0 {
                fail(
                    &mut diagnostics,
                    format!(
                        "Fatal: Lintnoi = {} is too large - Leff for noise is negative.",
                        c_g(model.values.lintnoi.value)
                    ),
                    &mut fatal,
                );
            }
            if binned.nch <= 0.0 {
                fail(
                    &mut diagnostics,
                    format!("Fatal: Nch = {} is not positive.", c_g(binned.nch)),
                    &mut fatal,
                );
            }
            if binned.nsub <= 0.0 {
                fail(
                    &mut diagnostics,
                    format!("Fatal: Nsub = {} is not positive.", c_g(binned.nsub)),
                    &mut fatal,
                );
            }
            if binned.ngate < 0.0 {
                fail(
                    &mut diagnostics,
                    format!("Fatal: Ngate = {} is not positive.", c_g(binned.ngate)),
                    &mut fatal,
                );
            }
            if binned.ngate > 1.0e25 {
                fail(
                    &mut diagnostics,
                    format!("Fatal: Ngate = {} is too high.", c_g(binned.ngate)),
                    &mut fatal,
                );
            }
            if binned.xj <= 0.0 {
                fail(
                    &mut diagnostics,
                    format!("Fatal: Xj = {} is not positive.", c_g(binned.xj)),
                    &mut fatal,
                );
            }
            if binned.dvt1 < 0.0 {
                fail(
                    &mut diagnostics,
                    format!("Fatal: Dvt1 = {} is negative.", c_g(binned.dvt1)),
                    &mut fatal,
                );
            }
            if binned.dvt1w < 0.0 {
                fail(
                    &mut diagnostics,
                    format!("Fatal: Dvt1w = {} is negative.", c_g(binned.dvt1w)),
                    &mut fatal,
                );
            }
            if binned.w0 == -weff {
                fail(
                    &mut diagnostics,
                    "Fatal: (W0 + Weff) = 0 causing divided-by-zero.".into(),
                    &mut fatal,
                );
            }
            if binned.dsub < 0.0 {
                fail(
                    &mut diagnostics,
                    format!("Fatal: Dsub = {} is negative.", c_g(binned.dsub)),
                    &mut fatal,
                );
            }
            if binned.b1 == -weff {
                fail(
                    &mut diagnostics,
                    "Fatal: (B1 + Weff) = 0 causing divided-by-zero.".into(),
                    &mut fatal,
                );
            }
            if thermal.u0temp <= 0.0 {
                fail(
                    &mut diagnostics,
                    format!(
                        "Fatal: u0 at current temperature = {} is not positive.",
                        c_g(thermal.u0temp)
                    ),
                    &mut fatal,
                );
            }
            if binned.delta < 0.0 {
                fail(
                    &mut diagnostics,
                    format!("Fatal: Delta = {} is less than zero.", c_g(binned.delta)),
                    &mut fatal,
                );
            }
            if thermal.vsattemp <= 0.0 {
                fail(
                    &mut diagnostics,
                    format!(
                        "Fatal: Vsat at current temperature = {} is not positive.",
                        c_g(thermal.vsattemp)
                    ),
                    &mut fatal,
                );
            }
            if binned.pclm <= 0.0 {
                fail(
                    &mut diagnostics,
                    format!("Fatal: Pclm = {} is not positive.", c_g(binned.pclm)),
                    &mut fatal,
                );
            }
            if binned.drout < 0.0 {
                fail(
                    &mut diagnostics,
                    format!("Fatal: Drout = {} is negative.", c_g(binned.drout)),
                    &mut fatal,
                );
            }
            if binned.pscbe2 <= 0.0 {
                diagnostics.push(Diagnostic::warning(
                    format!("Warning: Pscbe2 = {} is not positive.", c_g(binned.pscbe2)),
                    "src/spicelib/devices/bsim3/b3check.c",
                ));
            }
            if model.values.cjsw.value > 0.0 || model.values.cjswg.value > 0.0 {
                if drain_perimeter < weff {
                    diagnostics.push(Diagnostic::warning(
                        format!("Warning: Pd = {} is less than W.", c_g(drain_perimeter)),
                        "src/spicelib/devices/bsim3/b3check.c",
                    ));
                }
                if source_perimeter < weff {
                    diagnostics.push(Diagnostic::warning(
                        format!("Warning: Ps = {} is less than W.", c_g(source_perimeter)),
                        "src/spicelib/devices/bsim3/b3check.c",
                    ));
                }
            }
            if binned.noff < 0.1 {
                diagnostics.push(Diagnostic::warning(
                    format!("Warning: Noff = {} is too small.", c_g(binned.noff)),
                    "src/spicelib/devices/bsim3/b3check.c",
                ));
            }
            if binned.noff > 4.0 {
                diagnostics.push(Diagnostic::warning(
                    format!("Warning: Noff = {} is too large.", c_g(binned.noff)),
                    "src/spicelib/devices/bsim3/b3check.c",
                ));
            }
            if binned.voffcv < -0.5 {
                diagnostics.push(Diagnostic::warning(
                    format!("Warning: Voffcv = {} is too small.", c_g(binned.voffcv)),
                    "src/spicelib/devices/bsim3/b3check.c",
                ));
            }
            if binned.voffcv > 0.5 {
                diagnostics.push(Diagnostic::warning(
                    format!("Warning: Voffcv = {} is too large.", c_g(binned.voffcv)),
                    "src/spicelib/devices/bsim3/b3check.c",
                ));
            }
            if model.values.ijth.value < 0.0 {
                fail(
                    &mut diagnostics,
                    format!(
                        "Fatal: Ijth = {} cannot be negative.",
                        c_g(model.values.ijth.value)
                    ),
                    &mut fatal,
                );
            }
            if binned.clc < 0.0 {
                fail(
                    &mut diagnostics,
                    format!("Fatal: Clc = {} is negative.", c_g(binned.clc)),
                    &mut fatal,
                );
            }
            if binned.moin < 5.0 {
                diagnostics.push(Diagnostic::warning(
                    format!("Warning: Moin = {} is too small.", c_g(binned.moin)),
                    "src/spicelib/devices/bsim3/b3check.c",
                ));
            }
            if binned.moin > 25.0 {
                diagnostics.push(Diagnostic::warning(
                    format!("Warning: Moin = {} is too large.", c_g(binned.moin)),
                    "src/spicelib/devices/bsim3/b3check.c",
                ));
            }
            if model.values.capmod.value == 3 {
                if binned.acde < 0.4 {
                    diagnostics.push(Diagnostic::warning(
                        format!("Warning:  Acde = {} is too small.", c_g(binned.acde)),
                        "src/spicelib/devices/bsim3/b3check.c",
                    ));
                }
                if binned.acde > 1.6 {
                    diagnostics.push(Diagnostic::warning(
                        format!("Warning:  Acde = {} is too large.", c_g(binned.acde)),
                        "src/spicelib/devices/bsim3/b3check.c",
                    ));
                }
            }
            if model.values.paramchk.value == 1 {
                if leff <= 5.0e-8 {
                    diagnostics.push(Diagnostic::warning(
                        format!("Warning: Leff = {} may be too small.", c_g(leff)),
                        "src/spicelib/devices/bsim3/b3check.c",
                    ));
                }
                if leff_cv <= 5.0e-8 {
                    diagnostics.push(Diagnostic::warning(
                        format!("Warning: Leff for CV = {} may be too small.", c_g(leff_cv)),
                        "src/spicelib/devices/bsim3/b3check.c",
                    ));
                }
                if weff <= 1.0e-7 {
                    diagnostics.push(Diagnostic::warning(
                        format!("Warning: Weff = {} may be too small.", c_g(weff)),
                        "src/spicelib/devices/bsim3/b3check.c",
                    ));
                }
                if weff_cv <= 1.0e-7 {
                    diagnostics.push(Diagnostic::warning(
                        format!("Warning: Weff for CV = {} may be too small.", c_g(weff_cv)),
                        "src/spicelib/devices/bsim3/b3check.c",
                    ));
                }
                if binned.nlx < 0.0 {
                    diagnostics.push(Diagnostic::warning(
                        format!("Warning: Nlx = {} is negative.", c_g(binned.nlx)),
                        "src/spicelib/devices/bsim3/b3check.c",
                    ));
                }
                if model.values.tox.value < 1.0e-9 {
                    diagnostics.push(Diagnostic::warning(
                        format!(
                            "Warning: Tox = {} is less than 10A.",
                            c_g(model.values.tox.value)
                        ),
                        "src/spicelib/devices/bsim3/b3check.c",
                    ));
                }
                let npeak = binned.nch;
                if npeak <= 1.0e15 {
                    diagnostics.push(Diagnostic::warning(
                        format!("Warning: Nch = {} may be too small.", c_g(npeak)),
                        "src/spicelib/devices/bsim3/b3check.c",
                    ));
                } else if npeak >= 1.0e21 {
                    diagnostics.push(Diagnostic::warning(
                        format!("Warning: Nch = {} may be too large.", c_g(npeak)),
                        "src/spicelib/devices/bsim3/b3check.c",
                    ));
                }
                let nsub = binned.nsub;
                if nsub <= 1.0e14 {
                    diagnostics.push(Diagnostic::warning(
                        format!("Warning: Nsub = {} may be too small.", c_g(nsub)),
                        "src/spicelib/devices/bsim3/b3check.c",
                    ));
                } else if nsub >= 1.0e21 {
                    diagnostics.push(Diagnostic::warning(
                        format!("Warning: Nsub = {} may be too large.", c_g(nsub)),
                        "src/spicelib/devices/bsim3/b3check.c",
                    ));
                }
                let ngate = binned.ngate;
                if ngate > 0.0 && ngate <= 1.0e18 {
                    diagnostics.push(Diagnostic::warning(
                        format!("Warning: Ngate = {} is less than 1.E18cm^-3.", c_g(ngate)),
                        "src/spicelib/devices/bsim3/b3check.c",
                    ));
                }
                if binned.dvt0 < 0.0 {
                    diagnostics.push(Diagnostic::warning(
                        format!("Warning: Dvt0 = {} is negative.", c_g(binned.dvt0)),
                        "src/spicelib/devices/bsim3/b3check.c",
                    ));
                }
                if (1.0e-6 / (binned.w0 + weff)).abs() > 10.0 {
                    diagnostics.push(Diagnostic::warning(
                        "Warning: (W0 + Weff) may be too small.",
                        "src/spicelib/devices/bsim3/b3check.c",
                    ));
                }
                if binned.nfactor < 0.0 {
                    diagnostics.push(Diagnostic::warning(
                        format!("Warning: Nfactor = {} is negative.", c_g(binned.nfactor)),
                        "src/spicelib/devices/bsim3/b3check.c",
                    ));
                }
                if binned.cdsc < 0.0 {
                    diagnostics.push(Diagnostic::warning(
                        format!("Warning: Cdsc = {} is negative.", c_g(binned.cdsc)),
                        "src/spicelib/devices/bsim3/b3check.c",
                    ));
                }
                if binned.cdscd < 0.0 {
                    diagnostics.push(Diagnostic::warning(
                        format!("Warning: Cdscd = {} is negative.", c_g(binned.cdscd)),
                        "src/spicelib/devices/bsim3/b3check.c",
                    ));
                }
                if binned.eta0 < 0.0 {
                    diagnostics.push(Diagnostic::warning(
                        format!("Warning: Eta0 = {} is negative.", c_g(binned.eta0)),
                        "src/spicelib/devices/bsim3/b3check.c",
                    ));
                }
                if (1.0e-6 / (binned.b1 + weff)).abs() > 10.0 {
                    diagnostics.push(Diagnostic::warning(
                        "Warning: (B1 + Weff) may be too small.",
                        "src/spicelib/devices/bsim3/b3check.c",
                    ));
                }
                let a2 = binned.a2;
                if a2 < 0.01 {
                    diagnostics.push(Diagnostic::warning(
                        format!("Warning: A2 = {} is too small. Set to 0.01.", c_g(a2)),
                        "src/spicelib/devices/bsim3/b3check.c",
                    ));
                    binned.a2 = 0.01;
                } else if a2 > 1.0 {
                    diagnostics.push(Diagnostic::warning(
                        format!(
                            "Warning: A2 = {} is larger than 1. A2 is set to 1 and A1 is set to 0.",
                            c_g(a2)
                        ),
                        "src/spicelib/devices/bsim3/b3check.c",
                    ));
                    binned.a2 = 1.0;
                    binned.a1 = 0.0;
                }
                let rdsw = binned.rdsw;
                if rdsw < 0.0 {
                    diagnostics.push(Diagnostic::warning(
                        format!("Warning: Rdsw = {} is negative. Set to zero.", c_g(rdsw)),
                        "src/spicelib/devices/bsim3/b3check.c",
                    ));
                    binned.rdsw = 0.0;
                    thermal.rds0 = 0.0;
                } else if thermal.rds0 > 0.0 && thermal.rds0 < 0.001 {
                    diagnostics.push(Diagnostic::warning(format!("Warning: Rds at current temperature = {} is less than 0.001 ohm. Set to zero.", c_g(thermal.rds0)), "src/spicelib/devices/bsim3/b3check.c"));
                    thermal.rds0 = 0.0;
                }
                if thermal.vsattemp < 1.0e3 {
                    diagnostics.push(Diagnostic::warning(
                        format!(
                            "Warning: Vsat at current temperature = {} may be too small.",
                            c_g(thermal.vsattemp)
                        ),
                        "src/spicelib/devices/bsim3/b3check.c",
                    ));
                }
                if binned.pdiblc1 < 0.0 {
                    diagnostics.push(Diagnostic::warning(
                        format!("Warning: Pdibl1 = {} is negative.", c_g(binned.pdiblc1)),
                        "src/spicelib/devices/bsim3/b3check.c",
                    ));
                }
                if binned.pdiblc2 < 0.0 {
                    diagnostics.push(Diagnostic::warning(
                        format!("Warning: Pdibl2 = {} is negative.", c_g(binned.pdiblc2)),
                        "src/spicelib/devices/bsim3/b3check.c",
                    ));
                }
            }
        }
        // The CGDO/CGSO/CGBO resets belong to the `paramchk` block in C and
        // persist in the shared model.
        if model.values.paramchk.value == 1 {
            let reset = |value: &mut f64, name: &str, diagnostics: &mut Vec<Diagnostic>| {
                if *value < 0.0 {
                    diagnostics.push(Diagnostic::warning(
                        format!(
                            "Warning: {name} = {} is negative. Set to zero.",
                            c_g(*value)
                        ),
                        "src/spicelib/devices/bsim3/b3check.c",
                    ));
                    *value = 0.0;
                }
            };
            reset(model_cgdo, "cgdo", &mut diagnostics);
            reset(model_cgso, "cgso", &mut diagnostics);
            reset(model_cgbo, "cgbo", &mut diagnostics);
        }
        if fatal {
            return Err(Error::Checks(diagnostics));
        }
        warnings.append(&mut diagnostics);
        Ok(CheckedSize(self))
    }
}

/// The validated size state, with all derived values initialized, ready to
/// receive per-instance adjustments. C's final tconst calculation consumes
/// the surviving square-root temporary from threshold derivation.
struct DerivedSize {
    geometry: Geometry,
    binned: BinnedParameters,
    thermal: ThermalParameters,
    derived: DerivedParameters,
    cgdo: f64,
    cgso: f64,
    sqrt_length_factor: f64,
}

impl CheckedSize {
    fn derive(
        self,
        model: &ModelDefaults,
        temperature: &TemperatureModel,
        checks: &ModelChecks,
        warnings: &mut Vec<Diagnostic>,
    ) -> Result<DerivedSize> {
        let AdjustedSize {
            geometry,
            mut binned,
            thermal,
            ratio,
        } = self.0;
        let Geometry {
            leff,
            weff,
            leff_cv,
            weff_cv,
            ..
        } = geometry;
        // Overlap capacitances and C's `tconst` (see the quirk note above).
        let cox = model.oxide_capacitance;
        let cf = binned.cf;
        let cgdo = (checks.cgdo + cf) * weff_cv;
        let cgso = (checks.cgso + cf) * weff_cv;
        let cgbo = checks.cgbo * leff_cv;
        let elm = binned.elm;
        let tconst_quirk = thermal.u0temp * elm / (cox * weff_cv * leff_cv * (leff_cv * leff_cv));

        // Potential and threshold derivations.
        let vtm0 = temperature.vtm0;
        let ni = temperature.ni;
        let npeak = if !model.values.nch.given && model.values.gamma1.given {
            let gamma1 = binned.gamma1;
            let t0 = gamma1 * cox;
            3.021e22 * t0 * t0
        } else {
            binned.nch
        };
        let phi = 2.0 * vtm0 * (npeak / ni).ln();
        let sqrt_phi = phi.sqrt();
        let phis3 = sqrt_phi * phi;
        let eps_si = 1.035_94e-10;
        let charge_q = 1.602_19e-19;
        let xdep0 = (2.0 * eps_si / (charge_q * npeak * 1.0e6)).sqrt() * sqrt_phi;
        let sqrtxdep0 = xdep0.sqrt();
        let litl = (3.0 * binned.xj * model.values.tox.value).sqrt();
        let vbi = vtm0 * (1.0e20 * npeak / (ni * ni)).ln();
        let cdep0 = (charge_q * eps_si * npeak * 1.0e6 / 2.0 / phi).sqrt();
        let ldeb = (eps_si * vtm0 / (charge_q * npeak * 1.0e6)).sqrt() / 3.0;
        let acde = binned.acde * (npeak / 2.0e16).powf(-0.25);
        binned.acde = acde;

        // K1/K2: explicit pair, or derived from doping and flat-band data.
        let warn = |message: String, warnings: &mut Vec<Diagnostic>| {
            warnings.push(Diagnostic::warning(
                message,
                "src/spicelib/devices/bsim3/b3temp.c",
            ));
        };
        if model.values.k1.given || model.values.k2.given {
            if !model.values.k1.given {
                warn("Warning: k1 should be specified with k2.".into(), warnings);
                binned.k1 = 0.53;
            }
            if !model.values.k2.given {
                warn("Warning: k2 should be specified with k1.".into(), warnings);
                binned.k2 = -0.0186;
            }
            for (name, given) in [
                ("nsub", model.values.nsub.given),
                ("xt", model.values.xt.given),
                ("vbx", model.values.vbx.given),
                ("gamma1", model.values.gamma1.given),
                ("gamma2", model.values.gamma2.given),
            ] {
                if given {
                    warn(
                        format!("Warning: {name} is ignored because k1 or k2 is given."),
                        warnings,
                    );
                }
            }
        } else {
            if !model.values.vbx.given {
                let vbx = f64::mul_add(7.7348e-4 * npeak, -binned.xt.powi(2), phi);
                binned.vbx = vbx;
            }
            if binned.vbx > 0.0 {
                let value = -binned.vbx;
                binned.vbx = value;
            }
            if binned.vbm > 0.0 {
                let value = -binned.vbm;
                binned.vbm = value;
            }
            if !model.values.gamma1.given {
                binned.gamma1 = 5.753e-12 * npeak.sqrt() / cox;
            }
            if !model.values.gamma2.given {
                binned.gamma2 = 5.753e-12 * binned.nsub.sqrt() / cox;
            }
            let gamma1 = binned.gamma1;
            let gamma2 = binned.gamma2;
            let vbx = binned.vbx;
            let vbm = binned.vbm;
            let t0 = gamma1 - gamma2;
            let t1 = (phi - vbx).sqrt() - sqrt_phi;
            let t2 = (phi * (phi - vbm)).sqrt() - phi;
            let k2 = t0 * t1 / 2.0f64.mul_add(t2, vbm);
            let k1 = (2.0 * k2).mul_add(-(phi - vbm).sqrt(), gamma2);
            binned.k2 = k2;
            binned.k1 = k1;
        }

        let k1 = binned.k1;
        let k2 = binned.k2;
        let vbm = binned.vbm;
        let vbsc = if k2 < 0.0 {
            let t0 = 0.5 * k1 / k2;
            // `b3temp.c` clamps to [-30, -3]; endpoint values are unchanged.
            (0.9 * f64::mul_add(t0, -t0, phi)).clamp(-30.0, -3.0)
        } else {
            -30.0
        };
        let vbsc = if vbsc > vbm { vbm } else { vbsc };

        let polarity = f64::from(model.polarity);
        if !model.values.vfb.given {
            let vfb = if model.values.vth0.given {
                f64::mul_add(k1, -sqrt_phi, f64::mul_add(polarity, binned.vth0, -phi))
            } else {
                -1.0
            };
            binned.vfb = vfb;
        }
        if !model.values.vth0.given {
            let vfb = binned.vfb;
            binned.vth0 = polarity * f64::mul_add(k1, sqrt_phi, vfb + phi);
        }
        let k1ox = k1 * model.values.tox.value / model.values.toxm.value;
        let k2ox = k2 * model.values.tox.value / model.values.toxm.value;

        let factor1 = temperature.factor1;
        let dsub = binned.dsub;
        let t1 = (eps_si / 3.453_133e-11 * model.values.tox.value * xdep0).sqrt();
        let t0 = (-0.5 * dsub * leff / t1).exp();
        let theta0vb0 = (2.0 * t0).mul_add(t0, t0);
        let t0 = (-0.5 * binned.drout * leff / t1).exp();
        let t2 = (2.0 * t0).mul_add(t0, t0);
        let thetarout = f64::mul_add(binned.pdiblc1, t2, binned.pdiblc2);

        let tmp = xdep0.sqrt();
        let tmp1 = vbi - phi;
        let tmp2 = factor1 * tmp;
        let t0 = -0.5 * binned.dvt1w * weff * leff / tmp2;
        let t2 = if t0 > -EXP_THRESHOLD {
            let e = t0.exp();
            e * 2.0f64.mul_add(e, 1.0)
        } else {
            MIN_EXP * 2.0f64.mul_add(MIN_EXP, 1.0)
        };
        let t0 = binned.dvt0w * t2;
        let t2w = t0 * tmp1;
        let t0 = -0.5 * binned.dvt1 * leff / tmp2;
        let t3 = if t0 > -EXP_THRESHOLD {
            let e = t0.exp();
            e * 2.0f64.mul_add(e, 1.0)
        } else {
            MIN_EXP * 2.0f64.mul_add(MIN_EXP, 1.0)
        };
        let t3 = binned.dvt0 * t3 * tmp1;
        let t4 = model.values.tox.value * phi / (weff + binned.w0);
        let nlx = binned.nlx;
        let length_ratio = nlx / leff;
        let root_offset = length_ratio / ((1.0 + length_ratio).sqrt() + 1.0);
        let t5 = f64::mul_add(
            binned.kt1 + binned.kt1l / leff,
            ratio - 1.0,
            k1ox * root_offset * sqrt_phi,
        );
        let tmp3 = f64::mul_add(
            binned.k3,
            t4,
            f64::mul_add(polarity, binned.vth0, -t2w) - t3,
        ) + t5;
        let vfbzb = f64::mul_add(k1, -sqrt_phi, tmp3 - phi);
        let derived = DerivedParameters {
            npeak,
            phi,
            sqrtphi: sqrt_phi,
            phis3,
            xdep0,
            sqrtxdep0,
            litl,
            vbi,
            cdep0,
            ldeb,
            vbsc,
            k1ox,
            k2ox,
            theta0vb0,
            thetarout,
            vfbzb,
            cgbo,
        };

        for (name, value) in [
            ("phi", derived.phi),
            ("sqrtphi", derived.sqrtphi),
            ("phis3", derived.phis3),
            ("xdep0", derived.xdep0),
            ("sqrtxdep0", derived.sqrtxdep0),
            ("litl", derived.litl),
            ("vbi", derived.vbi),
            ("cdep0", derived.cdep0),
            ("ldeb", derived.ldeb),
            ("acde", binned.acde),
            ("k1", binned.k1),
            ("k2", binned.k2),
            ("k1ox", derived.k1ox),
            ("k2ox", derived.k2ox),
            ("vbsc", derived.vbsc),
            ("vfb", binned.vfb),
            ("vth0", binned.vth0),
            ("vfbzb", derived.vfbzb),
            ("theta0vb0", derived.theta0vb0),
            ("thetarout", derived.thetarout),
            ("u0temp", thermal.u0temp),
            ("vsattemp", thermal.vsattemp),
            ("rds0", thermal.rds0),
            ("cgdo", cgdo),
            ("cgso", cgso),
            ("cgbo", derived.cgbo),
            ("tconst", tconst_quirk),
            ("abulkcvfactor", thermal.abulkcvfactor),
        ] {
            if !value.is_finite() {
                return Err(Error::Numerical {
                    context: "BSIM3 size-dependent setup (bsim3/b3temp.c)".into(),
                    message: format!("{name} is not finite"),
                });
            }
        }
        Ok(DerivedSize {
            geometry,
            binned,
            thermal,
            derived,
            cgdo,
            cgso,
            sqrt_length_factor: t0,
        })
    }
}

/// Mutable model state that C keeps in one `BSIM3model` struct and that
/// `b3check.c` can reset while setting up an instance. Reuse one value for
/// every instance of a model: the resets persist, as they do in C.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelSetup {
    defaults: Arc<ModelDefaults>,
    temperature: Arc<TemperatureModel>,
    checks: ModelChecks,
}

/// Only these four model fields are mutated by the instance check pass.
/// A standalone instance needs its own copy of these scalars, not a clone
/// of the immutable defaulted model and temperature tables.
#[derive(Debug, Clone, Copy, PartialEq)]
struct ModelChecks {
    cgdo: f64,
    cgso: f64,
    cgbo: f64,
    calcacm: i32,
}

impl ModelChecks {
    const fn new(defaults: &ModelDefaults) -> Self {
        Self {
            cgdo: defaults.values.cgdo.value,
            cgso: defaults.values.cgso.value,
            cgbo: defaults.values.cgbo.value,
            calcacm: defaults.values.calcacm.value,
        }
    }

    fn prepare(&mut self, model: &ModelDefaults, warnings: &mut Vec<Diagnostic>) -> Result<()> {
        let acm = model.values.acm.value;
        if acm != 0 {
            return Err(Error::unsupported(
                format!("model BSIM3 ACM={acm}: the ACM source/drain geometry model is not ported"),
                "src/spicelib/devices/bsim3/b3set.c (ACM_SourceDrainResistances), b3temp.c",
            ));
        }
        let calcacm_value = self.calcacm;
        if calcacm_value > 0 {
            warnings.push(Diagnostic::warning(
                format!("Warning: CALCACM = {calcacm_value} is wrong. Set back to 0."),
                "src/spicelib/devices/bsim3/b3check.c",
            ));
            // C leaves the model field at zero, so later instances stay quiet.
            self.calcacm = 0;
        }
        Ok(())
    }
}

impl ModelSetup {
    /// Compute model-level temperature state from the selected defaults.
    /// # Errors
    /// Invalid operating temperature or nonfinite model quantities.
    pub fn new(defaults: ModelDefaults, temperature_k: f64) -> Result<Self> {
        let temperature = defaults.at_temperature(temperature_k)?;
        Ok(Self {
            checks: ModelChecks::new(&defaults),
            defaults: Arc::new(defaults),
            temperature: Arc::new(temperature),
        })
    }

    /// The model fields this state was built from.
    #[must_use = "the returned defaults identify the model fields underlying this instance setup context"]
    pub fn defaults(&self) -> &ModelDefaults {
        &self.defaults
    }

    /// The model-level temperature pass.
    #[must_use = "the returned temperature pass supplies model-level quantities used for instance setup"]
    pub fn temperature(&self) -> &TemperatureModel {
        &self.temperature
    }

    /// Prepare one immutable physical occurrence in model-check order.
    /// Input dimensions are already SI. IC vectors must be expanded to
    /// their ordered scalar components by the binding entry point.
    ///
    /// # Errors
    /// Wrong instance family/scope, unsupported physical mode or invalid
    /// geometry/coefficients. The model check state is committed only on success.
    pub fn prepare_instance(
        &mut self,
        instance: &BoundParameters,
    ) -> Result<super::PreparedInstance> {
        if instance.variant() != ModelVariant::Bsim3V33
            || instance.scope() != ParameterScope::Instance
        {
            return Err(ModelError::WrongModelScope.into());
        }
        if instance_value(instance, "ic").is_some() {
            return Err(
                ModelError::UnsupportedInstanceParameter("unexpanded BSIM3 IC vector").into(),
            );
        }
        let mut checks = self.checks;
        let setup =
            InstanceSetup::with_model(&self.defaults, &self.temperature, &mut checks, instance)?;
        self.checks = checks;
        Ok(super::PreparedInstance {
            model: Arc::clone(&self.defaults),
            temperature: Arc::clone(&self.temperature),
            setup,
        })
    }
}

/// Every `b3temp.c` size-dependent parameter keyword, in C order.
#[must_use = "the returned ordered keywords identify the reference size-dependent parameter set"]
pub fn size_dependent_names() -> Vec<&'static str> {
    BINNED_NAMES.to_vec()
}

impl InstanceSetup {
    /// A size-dependent/derived parameter by keyword, e.g. `phi` or `rdsw`.
    #[must_use = "the returned option distinguishes a derived instance parameter from an unsupported keyword"]
    pub fn pparam(&self, name: &str) -> Option<f64> {
        match name {
            "dl" => Some(self.dl),
            "dw" => Some(self.dw),
            "dlc" => Some(self.dlc),
            "dwc" => Some(self.dwc),
            "leff" => Some(self.leff),
            "weff" => Some(self.weff),
            "leffcv" => Some(self.leff_cv),
            "weffcv" => Some(self.weff_cv),
            "abulkcvfactor" => Some(self.thermal.abulkcvfactor),
            "u0temp" => Some(self.thermal.u0temp),
            "vsattemp" => Some(self.thermal.vsattemp),
            "rds0" => Some(self.thermal.rds0),
            "npeak" => Some(self.derived.npeak),
            "phi" => Some(self.derived.phi),
            "sqrtphi" => Some(self.derived.sqrtphi),
            "phis3" => Some(self.derived.phis3),
            "xdep0" => Some(self.derived.xdep0),
            "sqrtxdep0" => Some(self.derived.sqrtxdep0),
            "litl" => Some(self.derived.litl),
            "vbi" => Some(self.derived.vbi),
            "cdep0" => Some(self.derived.cdep0),
            "ldeb" => Some(self.derived.ldeb),
            "vbsc" => Some(self.derived.vbsc),
            "k1ox" => Some(self.derived.k1ox),
            "k2ox" => Some(self.derived.k2ox),
            "theta0vb0" => Some(self.derived.theta0vb0),
            "thetarout" => Some(self.derived.thetarout),
            "vfbzb" => Some(self.derived.vfbzb),
            "cgbo" => Some(self.derived.cgbo),
            "pdibl1" => Some(self.binned.pdiblc1),
            "pdibl2" => Some(self.binned.pdiblc2),
            "pdiblb" => Some(self.binned.pdiblcb),
            "cgdo" => Some(self.cgdo),
            "cgso" => Some(self.cgso),
            "tconst" => Some(self.tconst),
            _ => self.binned.get(name),
        }
    }

    fn with_model(
        model: &ModelDefaults,
        temperature: &TemperatureModel,
        checks: &mut ModelChecks,
        instance: &BoundParameters,
    ) -> Result<Self> {
        let mut warnings = Vec::new();
        checks.prepare(model, &mut warnings)?;
        let inputs = InstanceInputs::new(instance, model, &mut warnings)?;
        let geometry = Geometry::new(model, inputs.length, inputs.width)?;
        let checked = AdjustedSize::new(model, temperature, geometry).check(
            model,
            checks,
            &inputs,
            &mut warnings,
        )?;
        let DerivedSize {
            geometry,
            binned,
            thermal,
            derived,
            cgdo,
            cgso,
            sqrt_length_factor,
        } = checked.derive(model, temperature, checks, &mut warnings)?;
        let Geometry {
            dl,
            dw,
            dlc,
            dwc,
            leff,
            weff,
            leff_cv,
            weff_cv,
        } = geometry;
        let InstanceInputs {
            length,
            width,
            drain_area,
            source_area,
            drain_perimeter,
            source_perimeter,
            drain_squares,
            source_squares,
            delvto,
            mulu0,
            ic_vbs,
            ic_vds,
            ic_vgs,
            multiplier,
            geo,
            nqs_mod,
            acnqs_mod,
        } = inputs;
        let polarity = f64::from(model.polarity);

        // Instance-level adjustments: DELVTO and MULU0.
        let vth0 = binned.vth0 + delvto;
        let vfb = f64::mul_add(polarity, delvto, binned.vfb);
        let vfbzb = f64::mul_add(polarity, delvto, derived.vfbzb);
        let u0temp_instance = thermal.u0temp * mulu0;
        // C recomputes `tconst` here with `T0 = sqrt(1 + nlx/leff)` from above.
        let tconst_quirk = u0temp_instance * binned.elm
            / (model.oxide_capacitance * weff_cv * leff_cv * sqrt_length_factor);

        // Series resistances (ACM off): sheet resistance times squares.
        let sheet_resistance = model.values.rsh.value;
        let drain_resistance = sheet_resistance * drain_squares;
        let source_resistance = sheet_resistance * source_squares;
        let drain_conductance = if drain_resistance > 0.0 {
            1.0 / drain_resistance
        } else {
            0.0
        };
        let source_conductance = if source_resistance > 0.0 {
            1.0 / source_resistance
        } else {
            0.0
        };

        // Junction saturation currents and breakdown voltages.
        let vtm = temperature.vtm;
        let nvtm = vtm * model.values.nj.value;
        let jct_temp = temperature.jct_temp_sat_cur_density;
        let jct_sidewall_temp = temperature.jct_sidewall_temp_sat_cur_density;
        let ijth = model.values.ijth.value;
        let source_sat_current = if source_area <= 0.0 && source_perimeter <= 0.0 {
            1.0e-14
        } else {
            f64::mul_add(source_perimeter, jct_sidewall_temp, source_area * jct_temp)
        };
        let drain_sat_current = if drain_area <= 0.0 && drain_perimeter <= 0.0 {
            1.0e-14
        } else {
            f64::mul_add(drain_perimeter, jct_sidewall_temp, drain_area * jct_temp)
        };
        let (mut vjsm, mut isevjsm) = (0.0, 0.0);
        if source_sat_current > 0.0 && ijth > 0.0 {
            vjsm = nvtm * (ijth / source_sat_current).ln_1p();
            isevjsm = source_sat_current * (vjsm / nvtm).exp();
        }
        let (mut vjdm, mut isevjdm) = (0.0, 0.0);
        if drain_sat_current > 0.0 && ijth > 0.0 {
            vjdm = nvtm * (ijth / drain_sat_current).ln_1p();
            isevjdm = drain_sat_current * (vjdm / nvtm).exp();
        }
        let setup = Self {
            length,
            width,
            leff,
            weff,
            leff_cv,
            weff_cv,
            polarity: model.polarity,
            multiplier,
            nqs_mod,
            acnqs_mod,
            geo,
            drain_area,
            source_area,
            drain_perimeter,
            source_perimeter,
            drain_squares,
            source_squares,
            delvto,
            mulu0,
            ic_vbs,
            ic_vds,
            ic_vgs,
            vth0,
            vfb,
            vfbzb,
            u0temp: u0temp_instance,
            tconst: tconst_quirk,
            cgso,
            cgdo,
            drain_conductance,
            source_conductance,
            source_sat_current,
            drain_sat_current,
            vjsm,
            vjdm,
            isevjsm,
            isevjdm,
            dl,
            dw,
            dlc,
            dwc,
            binned,
            thermal,
            derived,
            warnings,
        };
        Ok(setup)
    }
}

fn instance_value<'a>(inputs: &'a BoundParameters, name: &str) -> Option<&'a ParameterValue> {
    let slot = ModelVariant::Bsim3V33
        .parameter(ParameterScope::Instance, name)?
        .slot;
    inputs.get_slot(slot)
}

fn integer(inputs: &BoundParameters, name: &str) -> Result<i32> {
    match instance_value(inputs, name) {
        Some(ParameterValue::Integer(value)) => Ok(*value),
        _ => Err(Error::Domain(format!(
            "BSIM3 setup expected integer {name}"
        ))),
    }
}

fn instance_real(inputs: &BoundParameters, name: &str) -> Result<f64> {
    match instance_value(inputs, name) {
        Some(ParameterValue::Real(value)) => Ok(*value),
        _ => Err(Error::Domain(format!("BSIM3 setup expected real {name}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn c_g_preserves_integer_zeros_and_c_exponent_boundaries() {
        for (value, expected) in [
            (100_000., "100000"),
            (-120_000., "-120000"),
            (999_999.4, "999999"),
            (999_999.6, "1e+06"),
            (0.000_01, "1e-05"),
            (0.0001, "0.0001"),
            (-0., "-0"),
            (f64::NAN, "nan"),
            (f64::INFINITY, "inf"),
            (f64::NEG_INFINITY, "-inf"),
        ] {
            assert_eq!(c_g(value), expected);
        }
    }
}
