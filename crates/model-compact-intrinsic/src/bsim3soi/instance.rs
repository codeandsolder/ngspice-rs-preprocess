//! Source setup and size-bin arithmetic for the three BSIM3SOI implementations.
//! Copyright 1999/1990 Regents of the University of California. See package LICENSE.

use super::{PreparedModel, rules};
use crate::preparation::{default, integer, positive, real, slot};
use crate::{BoundParameters, ModelError, ModelVariant, ParameterScope, ParameterValue};

/// The additional geometry used only by the partially depleted implementation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PdGeometry {
    /// Effective body C-V length in meters.
    pub body_cv_length: f64,
    /// Effective backgate C-V length in meters.
    pub backgate_cv_length: f64,
    /// Drain junction width per segment in meters.
    pub drain_diode_width: f64,
    /// Source junction width per segment in meters.
    pub source_diode_width: f64,
    /// Drain C-V junction width per segment in meters.
    pub drain_diode_cv_width: f64,
    /// Source C-V junction width per segment in meters.
    pub source_diode_cv_width: f64,
}

/// Source-corrected channel geometry before temperature-dependent model derivation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geometry {
    /// Effective I-V length in meters.
    pub length_m: f64,
    /// Effective I-V width in meters.
    pub width_m: f64,
    /// Effective C-V length in meters.
    pub cv_length_m: f64,
    /// Effective C-V width in meters.
    pub cv_width_m: f64,
    /// Size factor for the bulk charge equations.
    pub abulk_cv_factor: f64,
    /// PD-specific body/contact geometry, absent for FD/DD.
    pub pd: Option<PdGeometry>,
}

/// A source size-bin result before the physical temperature derivation.
///
/// The zero-initialized C base value of a missing model parameter is represented
/// explicitly as an absent base plus its size terms, not as a resolved zero.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RawBinned {
    /// Prepared model-card base; absent for deferred size-derived fields.
    pub base: Option<f64>,
    /// Sum of the length, width and cross terms.
    pub correction: f64,
}

impl RawBinned {
    /// Raw C prelude value; may still be overwritten by physical derivations.
    #[must_use = "the raw sum is not yet the physical parameter"]
    pub fn prelude_value(self) -> f64 {
        self.base.unwrap_or_default() + self.correction
    }
}

/// Bound instance defaults, physical geometry and the raw size-bin program.
#[derive(Clone, Debug, PartialEq)]
pub struct PreparedInstance {
    parameters: BoundParameters,
    geometry: Geometry,
    given: Box<[bool]>,
    binned: Box<[Option<RawBinned>]>,
    initial_conditions: [Option<f64>; 5],
}

impl PreparedInstance {
    /// Instance values after setup in source units.
    #[must_use = "the returned parameters define the instance"]
    pub const fn parameters(&self) -> &BoundParameters {
        &self.parameters
    }

    /// Corrected I-V and C-V geometry.
    #[must_use = "geometry is required for physical preparation"]
    pub const fn geometry(&self) -> Geometry {
        self.geometry
    }

    /// Original explicitly supplied instance fields.
    #[must_use = "the original given flags affect later setup"]
    pub fn explicitly_given(&self, slot: u16) -> bool {
        self.given.get(usize::from(slot)).copied().unwrap_or(false)
    }

    /// Raw size-bin coefficient by canonical model slot.
    #[must_use = "the returned coefficient precedes further physical derivation"]
    pub fn raw_binned_slot(&self, slot: u16) -> Option<RawBinned> {
        self.binned.get(usize::from(slot)).copied().flatten()
    }

    /// Explicit initial conditions in VDS, VGS, VBS, VES, VPS order.
    ///
    /// Absent components retain their given state for later circuit initialization.
    #[must_use = "initial conditions define startup constraints"]
    pub const fn initial_conditions(&self) -> [Option<f64>; 5] {
        self.initial_conditions
    }
}

fn instance_defaults(
    model: &PreparedModel,
    source: &BoundParameters,
) -> Result<BoundParameters, ModelError> {
    let mut p = source.clone();
    for (name, v) in [
        ("ad", 0.0),
        ("as", 0.0),
        ("pd", 0.0),
        ("ps", 0.0),
        ("nrd", 1.0),
        ("nrs", 1.0),
        ("nrb", 1.0),
        ("l", 5e-6),
        ("w", 5e-6),
        ("m", 1.0),
    ] {
        default(&mut p, name, ParameterValue::Real(v))?;
    }
    for name in ["bjtoff", "debug"] {
        default(&mut p, name, ParameterValue::Integer(0))?;
    }
    for name in ["rth0", "cth0"] {
        default(
            &mut p,
            name,
            ParameterValue::Real(real(model.parameters(), name)?),
        )?;
    }
    default(&mut p, "off", ParameterValue::Flag(false))?;
    if source.variant() == ModelVariant::Bsim3SoiPd {
        for (name, v) in [
            ("frbody", 1.0),
            ("nbc", 0.0),
            ("nseg", 1.0),
            ("pdbcp", 0.0),
            ("psbcp", 0.0),
            ("agbcp", 0.0),
            ("aebcp", 0.0),
            ("vbsusr", 0.0),
        ] {
            default(&mut p, name, ParameterValue::Real(v))?;
        }
        default(&mut p, "tnodeout", ParameterValue::Flag(false))?;
    }
    positive(real(&p, "m")?, "m")?;
    Ok(p)
}

fn initial_conditions(p: &BoundParameters) -> Result<[Option<f64>; 5], ModelError> {
    let mut ic = [None; 5];
    if let Some(ParameterValue::RealVector(values)) = p.get_slot(slot(p, "ic")?) {
        if !(1..=5).contains(&values.len()) {
            return Err(ModelError::InvalidInitialConditions(values.len()));
        }
        for (dst, value) in ic.iter_mut().zip(values) {
            *dst = Some(*value);
        }
    }
    Ok(ic)
}

fn correction(
    model: &BoundParameters,
    length: f64,
    width: f64,
    names: [&str; 5],
) -> Result<f64, ModelError> {
    let [length_coef, width_coef, cross_coef, l_power, w_power] = names;
    let length_term = length.powf(real(model, l_power)?);
    let width_term = width.powf(real(model, w_power)?);
    Ok(real(model, length_coef)? / length_term
        + real(model, width_coef)? / width_term
        + real(model, cross_coef)? / (length_term * width_term))
}

fn geometry(m: &BoundParameters, i: &BoundParameters) -> Result<Geometry, ModelError> {
    let l = positive(real(i, "l")?, "l")?;
    let w = positive(real(i, "w")?, "w")?;
    let l_corr = correction(m, l, w, ["ll", "lw", "lwl", "lln", "lwn"])?;
    let w_corr = correction(m, l, w, ["wl", "ww", "wwl", "wln", "wwn"])?;
    let dl = real(m, "lint")? + l_corr;
    let dw = real(m, "wint")? + w_corr;
    let is_pd = m.variant() == ModelVariant::Bsim3SoiPd;
    let (dlc, dwc, nbc, body_contact_width) = if is_pd {
        (
            real(m, "dlc")? + correction(m, l, w, ["llc", "lwc", "lwlc", "lln", "lwn"])?,
            real(m, "dwc")? + correction(m, l, w, ["wlc", "wwc", "wwlc", "wln", "wwn"])?,
            real(i, "nbc")?,
            real(m, "dwbc")?,
        )
    } else {
        (real(m, "dlc")? + l_corr, real(m, "dwc")? + w_corr, 0.0, 0.0)
    };
    let length_m = positive(l - (dl + dl), "leff")?;
    let width_m = positive(
        (2.0 - nbc).mul_add(-dw, nbc.mul_add(-body_contact_width, w)),
        "weff",
    )?;
    let cv_length_m = positive(l - (dlc + dlc), "leffCV")?;
    let cv_width_m = positive(
        (2.0 - nbc).mul_add(-dwc, nbc.mul_add(-body_contact_width, w)),
        "weffCV",
    )?;
    let clc_ratio = real(m, "clc")? / length_m;
    let cle = real(m, "cle")?;
    let abulk_cv_factor = positive(
        if is_pd {
            1.0 + clc_ratio.powf(cle)
        } else {
            (1.0 + clc_ratio).powf(cle)
        },
        "abulkCVfactor",
    )?;
    let pd = if is_pd {
        let body_cv_length = positive(cv_length_m - real(m, "dlcb")?, "leffCVb")?;
        let backgate_cv_length = positive(
            2.0_f64.mul_add(real(m, "dlbg")?, body_cv_length),
            "leffCVbg",
        )?;
        let nseg = positive(real(i, "nseg")?, "nseg")?;
        Some(PdGeometry {
            body_cv_length,
            backgate_cv_length,
            drain_diode_width: positive(width_m / nseg + real(i, "pdbcp")?, "wdiod")?,
            source_diode_width: positive(width_m / nseg + real(i, "psbcp")?, "wdios")?,
            drain_diode_cv_width: positive(cv_width_m / nseg + real(i, "pdbcp")?, "wdiodCV")?,
            source_diode_cv_width: positive(cv_width_m / nseg + real(i, "psbcp")?, "wdiosCV")?,
        })
    } else {
        None
    };
    Ok(Geometry {
        length_m,
        width_m,
        cv_length_m,
        cv_width_m,
        abulk_cv_factor,
        pd,
    })
}

fn binning(
    model: &BoundParameters,
    geometry: Geometry,
) -> Result<Box<[Option<RawBinned>]>, ModelError> {
    let program = if model.variant() == ModelVariant::Bsim3SoiPd {
        rules::PD_BINS
    } else {
        rules::FD_DD_BINS
    };
    let unit = if integer(model, "binunit")? == 1 {
        1e-6
    } else {
        1.0
    };
    let inv_l = unit / geometry.length_m;
    let inv_w = unit / geometry.width_m;
    let inv_area = unit * unit / (geometry.length_m * geometry.width_m);
    let mut values = vec![None; model.values.len()].into_boxed_slice();
    for &rule in program {
        let [name, length_coef, width_coef, cross_coef] = rules::BIN_RULES[usize::from(rule)];
        let id = slot(model, name)?;
        let base = match model.get_slot(id) {
            Some(ParameterValue::Real(value)) => Some(*value),
            None => None,
            _ => return Err(ModelError::MissingDefaultDependency(id)),
        };
        let correction = real(model, length_coef)?.mul_add(
            inv_l,
            real(model, width_coef)?.mul_add(inv_w, real(model, cross_coef)? * inv_area),
        );
        let raw = RawBinned { base, correction };
        if !raw.prelude_value().is_finite() {
            return Err(ModelError::InvalidBinningParameter(id));
        }
        values[usize::from(id)] = Some(raw);
    }
    Ok(values)
}

/// Prepare instance defaults and the source size-bin prelude.
///
/// The model family determines independent FD/DD or PD C-V/geometry equations.
/// The returned coefficients have not yet undergone temperature, threshold,
/// electrostatic or mobility derivation.
///
/// # Errors
/// Wrong model context, invalid geometry, initial-condition vector or size bin.
pub fn prepare_instance(
    model: &PreparedModel,
    source: &BoundParameters,
) -> Result<PreparedInstance, ModelError> {
    if source.variant() != model.parameters().variant()
        || source.scope() != ParameterScope::Instance
    {
        return Err(ModelError::WrongModelScope);
    }
    let parameters = instance_defaults(model, source)?;
    let geometry = geometry(model.parameters(), &parameters)?;
    let binned = binning(model.parameters(), geometry)?;
    let initial_conditions = initial_conditions(&parameters)?;
    Ok(PreparedInstance {
        parameters,
        geometry,
        given: source.values.iter().map(Option::is_some).collect(),
        binned,
        initial_conditions,
    })
}
