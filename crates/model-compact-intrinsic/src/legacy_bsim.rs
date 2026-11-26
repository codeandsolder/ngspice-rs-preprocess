//! Immutable BSIM1/2 setup from the upstream `b1set.c`, `b2set.c`,
//! `b1temp.c` and `b2temp.c` physical rules.
//!
//! Copyright 1990 Regents of the University of California; Hong J. Park,
//! Thomas L. Quarles (BSIM1), Min-Chie Jeng (BSIM2). See the package license.
//! No topology, history, model-name resolution or circuit rows belong here.

mod coefficients;

pub use coefficients::{Bsim1Coefficients, Bsim2Coefficients, LegacyBsimChannel};

use crate::{
    BoundParameters, ModelError, ModelVariant, MosPolarity, ParameterScope, ParameterType,
    ParameterValue,
};

/// Model-card values after the family-specific defaults and junction-potential limits.
#[derive(Clone, Debug, PartialEq)]
pub struct PreparedLegacyBsimModel {
    source: BoundParameters,
    values: BoundParameters,
    polarity: MosPolarity,
}

/// Physical lengths in metres and per-device multiplicity.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LegacyBsimGeometry {
    /// Drawn channel length.
    pub length: f64,
    /// Drawn channel width.
    pub width: f64,
    /// Channel length after the micrometre-valued DL correction.
    pub effective_length: f64,
    /// Channel width after the micrometre-valued DW correction.
    pub effective_width: f64,
    /// Number of parallel devices.
    pub multiplier: f64,
}

/// One drain or source junction, prior to applying the multiplier.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LegacyBsimJunction {
    /// Area in square metres.
    pub area: f64,
    /// Perimeter in metres.
    pub perimeter: f64,
    /// Saturation current, including the upstream lower bound, in amperes.
    pub saturation_current: f64,
    /// Bottom zero-bias capacitance in farads.
    pub bottom_capacitance: f64,
    /// Sidewall zero-bias capacitance in farads.
    pub sidewall_capacitance: f64,
    /// Series conductance in siemens; zero means no separate series branch.
    pub series_conductance: f64,
}

/// Gate overlaps in farads, as prepared by the reference temperature routine.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LegacyBsimOverlap {
    /// Gate-drain capacitance.
    pub drain: f64,
    /// Gate-source capacitance.
    pub source: f64,
    /// Gate-bulk capacitance.
    pub bulk: f64,
}

/// Model-wide junction and flicker-noise parameters.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LegacyBsimJunctionLaw {
    /// Bottom depletion potential in volts.
    pub bottom_potential: f64,
    /// Sidewall depletion potential in volts.
    pub sidewall_potential: f64,
    /// Bottom grading exponent.
    pub bottom_grading: f64,
    /// Sidewall grading exponent.
    pub sidewall_grading: f64,
    /// Flicker-noise coefficient, in the family source convention.
    pub flicker_coefficient: f64,
    /// Flicker-noise current exponent.
    pub flicker_exponent: f64,
}

/// Immutable physical inputs for a single BSIM1/2 device. These are not
/// a solver-ready artifact: the family current and charge laws remain separate.
#[derive(Clone, Debug, PartialEq)]
pub struct PreparedLegacyBsimInstance {
    /// Physical polarity.
    pub polarity: MosPolarity,
    /// Source and corrected dimensions.
    pub geometry: LegacyBsimGeometry,
    /// Intrinsic channel parameters, distinct for each model family.
    pub channel: LegacyBsimChannel,
    /// Drain junction.
    pub drain: LegacyBsimJunction,
    /// Source junction.
    pub source: LegacyBsimJunction,
    /// Reference temperature-stage overlap capacitances.
    pub overlap: LegacyBsimOverlap,
    /// Model junction and noise law.
    pub junction_law: LegacyBsimJunctionLaw,
    /// Oxide capacitance density in F/m².
    pub oxide_capacitance_density: f64,
    /// Channel oxide capacitance in F.
    pub oxide_capacitance: f64,
    /// Zero-bias normalized threshold in volts.
    pub threshold: f64,
}

fn slot(variant: ModelVariant, scope: ParameterScope, name: &str) -> Result<u16, ModelError> {
    variant
        .parameter(scope, name)
        .map(|spec| spec.slot)
        .ok_or_else(|| ModelError::UnknownParameter(name.into()))
}

fn real_input(source: &BoundParameters, name: &str, default: f64) -> Result<f64, ModelError> {
    let ordinal = slot(source.variant(), source.scope(), name)?;
    match source.get_slot(ordinal) {
        Some(ParameterValue::Real(value)) => Ok(*value),
        None => Ok(default),
        _ => Err(ModelError::WrongType {
            name: name.into(),
            expected: ParameterType::Real,
        }),
    }
}

fn flag_input(source: &BoundParameters, name: &str) -> Result<bool, ModelError> {
    let ordinal = slot(source.variant(), source.scope(), name)?;
    match source.get_slot(ordinal) {
        Some(ParameterValue::Flag(value)) => Ok(*value),
        None => Ok(false),
        _ => Err(ModelError::WrongType {
            name: name.into(),
            expected: ParameterType::Flag,
        }),
    }
}

fn nonnegative(value: f64, name: &'static str) -> Result<f64, ModelError> {
    if value.is_finite() && value >= 0. {
        Ok(value)
    } else {
        Err(ModelError::InvalidPhysicalParameter(name))
    }
}

fn positive(value: f64, name: &'static str) -> Result<f64, ModelError> {
    if value.is_finite() && value > 0. {
        Ok(value)
    } else {
        Err(ModelError::InvalidPhysicalParameter(name))
    }
}

const fn finite(value: f64, name: &'static str) -> Result<f64, ModelError> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(ModelError::InvalidPhysicalParameter(name))
    }
}

fn model_default(variant: ModelVariant, name: &str) -> f64 {
    // All remaining real fields initialize to zero, including B2 AF/KF
    // (zero-allocated, with no explicit default in B2setup).
    match (variant, name) {
        (ModelVariant::Bsim1, "af") => 1.,
        (ModelVariant::Bsim2, "vfb") => -1.,
        (ModelVariant::Bsim2, "phi") => 0.75,
        (ModelVariant::Bsim2, "k1") => 0.8,
        (ModelVariant::Bsim2, "mu0") => 400.,
        (ModelVariant::Bsim2, "mus0") => 500.,
        (ModelVariant::Bsim2, "mu20") => 1.5,
        (ModelVariant::Bsim2, "mu30" | "wdf") => 10.,
        (ModelVariant::Bsim2, "ua0" | "vghigh") => 0.2,
        (ModelVariant::Bsim2, "u10") => 0.1,
        (ModelVariant::Bsim2, "n0") => 1.4,
        (ModelVariant::Bsim2, "nb") => 0.5,
        (ModelVariant::Bsim2, "vof0") => 1.8,
        (ModelVariant::Bsim2, "vglow") => -0.15,
        (ModelVariant::Bsim2, "tox") => 0.03,
        (ModelVariant::Bsim2, "temp") => 27.,
        (ModelVariant::Bsim2, "vdd" | "vgg" | "vbb") => 5.,
        _ => 0.,
    }
}

/// Resolve model defaults without substituting BSIM3 or the other legacy family.
/// A zero oxide thickness can be inspected here as the reference default; it
/// cannot produce a physical instance.
///
/// # Errors
/// Incorrect scope/family, conflicting polarity or invalid parameter binding.
pub fn prepare_legacy_bsim_model(
    source: &BoundParameters,
) -> Result<PreparedLegacyBsimModel, ModelError> {
    let variant = source.variant();
    if source.scope() != ParameterScope::Model
        || !matches!(variant, ModelVariant::Bsim1 | ModelVariant::Bsim2)
    {
        return Err(ModelError::WrongModelScope);
    }
    let nmos = flag_input(source, "nmos")?;
    let pmos = flag_input(source, "pmos")?;
    if nmos && pmos {
        return Err(ModelError::ConflictingPolarity);
    }
    let polarity = if pmos {
        MosPolarity::Pmos
    } else {
        MosPolarity::Nmos
    };
    let mut values = source.clone();
    for spec in variant.parameters(ParameterScope::Model) {
        let index = usize::from(spec.slot);
        let value = match spec.kind {
            ParameterType::Real => {
                let mut value = real_input(source, spec.name, model_default(variant, spec.name))?;
                if matches!(spec.name, "pb" | "pbsw") {
                    value = value.max(0.1);
                }
                ParameterValue::Real(value)
            }
            ParameterType::Flag => ParameterValue::Flag(match spec.name {
                "nmos" => !pmos,
                "pmos" => pmos,
                _ => flag_input(source, spec.name)?,
            }),
            _ => {
                return Err(ModelError::WrongType {
                    name: spec.name.into(),
                    expected: ParameterType::Real,
                });
            }
        };
        values.values[index] = Some(value);
    }
    Ok(PreparedLegacyBsimModel {
        source: source.clone(),
        values,
        polarity,
    })
}

impl PreparedLegacyBsimModel {
    /// Exact family identity of this prepared model.
    #[must_use = "the family determines the instance equations"]
    pub const fn variant(&self) -> ModelVariant {
        self.values.variant()
    }

    /// Resolved physical polarity.
    #[must_use = "polarity determines voltage/current normalization"]
    pub const fn polarity(&self) -> MosPolarity {
        self.polarity
    }

    /// Read a prepared model value in its source-defined units.
    #[must_use = "the lookup distinguishes a registered slot"]
    pub fn get_slot(&self, slot: u16) -> Option<&ParameterValue> {
        self.values.get_slot(slot)
    }

    /// Read the explicitly bound value before defaulting and limits.
    #[must_use = "the lookup distinguishes an explicitly given parameter"]
    pub fn given(&self, slot: u16) -> Option<&ParameterValue> {
        self.source.get_slot(slot)
    }

    fn real(&self, name: &str) -> Result<f64, ModelError> {
        let ordinal = slot(self.variant(), ParameterScope::Model, name)?;
        match self.get_slot(ordinal) {
            Some(ParameterValue::Real(value)) => Ok(*value),
            _ => Err(ModelError::WrongType {
                name: name.into(),
                expected: ParameterType::Real,
            }),
        }
    }

    fn geometry(&self, source: &BoundParameters) -> Result<LegacyBsimGeometry, ModelError> {
        let length = positive(real_input(source, "l", 5e-6)?, "l")?;
        let width = positive(real_input(source, "w", 5e-6)?, "w")?;
        let effective_length = positive(
            self.real("dl")?.mul_add(-1e-6, length),
            "effective channel length",
        )?;
        let effective_width = positive(
            self.real("dw")?.mul_add(-1e-6, width),
            "effective channel width",
        )?;
        let multiplier = positive(real_input(source, "m", 1.)?, "m")?;
        Ok(LegacyBsimGeometry {
            length,
            width,
            effective_length,
            effective_width,
            multiplier,
        })
    }

    fn junction(
        &self,
        source: &BoundParameters,
        names: [&'static str; 3],
    ) -> Result<LegacyBsimJunction, ModelError> {
        let [area_name, perimeter_name, squares_name] = names;
        let area = nonnegative(real_input(source, area_name, 0.)?, area_name)?;
        let perimeter = nonnegative(real_input(source, perimeter_name, 0.)?, perimeter_name)?;
        let squares = nonnegative(real_input(source, squares_name, 1.)?, squares_name)?;
        let sheet = nonnegative(self.real("rsh")?, "rsh")?;
        let series_conductance = if sheet == 0. || squares == 0. {
            0.
        } else {
            let resistance = positive(sheet * squares, "series resistance")?;
            positive(resistance.recip(), "series conductance")?
        };
        let saturation_current = finite(
            area * nonnegative(self.real("js")?, "js")?,
            "junction saturation current",
        )?
        .max(1e-15);
        let bottom_capacitance = nonnegative(
            area * nonnegative(self.real("cj")?, "cj")?,
            "bottom capacitance",
        )?;
        let sidewall_capacitance = nonnegative(
            perimeter * nonnegative(self.real("cjsw")?, "cjsw")?,
            "sidewall capacitance",
        )?;
        Ok(LegacyBsimJunction {
            area,
            perimeter,
            saturation_current,
            bottom_capacitance,
            sidewall_capacitance,
            series_conductance,
        })
    }

    fn overlap(&self, geometry: &LegacyBsimGeometry) -> Result<LegacyBsimOverlap, ModelError> {
        // B1temp uses drawn L for Cgbo; B2temp uses effective L.
        let bulk_length = if self.variant() == ModelVariant::Bsim1 {
            geometry.length
        } else {
            geometry.effective_length
        };
        Ok(LegacyBsimOverlap {
            drain: nonnegative(
                geometry.effective_width * nonnegative(self.real("cgdo")?, "cgdo")?,
                "gate-drain overlap",
            )?,
            source: nonnegative(
                geometry.effective_width * nonnegative(self.real("cgso")?, "cgso")?,
                "gate-source overlap",
            )?,
            bulk: nonnegative(
                bulk_length * nonnegative(self.real("cgbo")?, "cgbo")?,
                "gate-bulk overlap",
            )?,
        })
    }

    fn junction_law(&self) -> Result<LegacyBsimJunctionLaw, ModelError> {
        Ok(LegacyBsimJunctionLaw {
            bottom_potential: self.real("pb")?,
            sidewall_potential: self.real("pbsw")?,
            bottom_grading: self.real("mj")?,
            sidewall_grading: self.real("mjsw")?,
            flicker_coefficient: nonnegative(self.real("kf")?, "kf")?,
            flicker_exponent: self.real("af")?,
        })
    }

    /// Prepare an immutable device with physical (already resolved) geometry.
    /// BSIM1 and BSIM2 instance setters do not apply the global scale option.
    /// Initial conditions and OFF are solver inputs, not constitutive parameters.
    ///
    /// # Errors
    /// Different family or scope, invalid geometry, oxide or nonfinite preparation.
    pub fn prepare_instance(
        &self,
        source: &BoundParameters,
    ) -> Result<PreparedLegacyBsimInstance, ModelError> {
        if source.variant() != self.variant() || source.scope() != ParameterScope::Instance {
            return Err(ModelError::WrongModelScope);
        }
        let geometry = self.geometry(source)?;
        let tox = positive(self.real("tox")?, "tox")?;
        // C uses F/cm² and micrometre oxide thickness. Expose SI F/m².
        let cox_cm2 = positive(3.453e-13 / (tox * 1e-4), "oxide capacitance density")?;
        let oxide_capacitance_density = positive(cox_cm2 * 1e4, "oxide capacitance density")?;
        let oxide_capacitance = positive(
            cox_cm2 * geometry.effective_length * geometry.effective_width * 1e4,
            "channel oxide capacitance",
        )?;
        let beta_scale = positive(
            cox_cm2 * geometry.effective_width / geometry.effective_length,
            "mobility scale",
        )?;
        let channel = coefficients::prepare(self, &geometry, beta_scale)?;
        let threshold = channel.threshold()?;
        Ok(PreparedLegacyBsimInstance {
            polarity: self.polarity,
            geometry,
            channel,
            drain: self.junction(source, ["ad", "pd", "nrd"])?,
            source: self.junction(source, ["as", "ps", "nrs"])?,
            overlap: self.overlap(&geometry)?,
            junction_law: self.junction_law()?,
            oxide_capacitance_density,
            oxide_capacitance,
            threshold,
        })
    }
}
