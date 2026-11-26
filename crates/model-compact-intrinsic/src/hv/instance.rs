//! Source-derived HiSIM-HV1/HV2 instance preparation and geometry binning.
//!
//! Mirrors the instance defaulting and `BINNING` sections of ngspice 47's
//! `hsmhvset.c` and `hsmhv2set.c`, without claiming nonlinear device support.

use std::sync::{Arc, OnceLock};

use super::{
    HiSimHvEffectiveGeometry, PreparedHiSimHvModel, compiled_hv_by_family,
    effective::prepare_effective_geometry,
    lookup_slot, source_real_at,
    transport::{HiSimHvTransportGeometry, PreparedHiSimHvTransport, prepare_transport_geometry},
};
use crate::{
    BoundParameters, ModelError, ModelVariant, ParameterScope, ParameterType, ParameterValue,
};

/// Gate geometry used by the upstream binning macro (not effective channel L/W).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvBinningGeometry {
    /// `(L + XL)` in meters.
    pub gate_length_m: f64,
    /// `(W/NF + XW)` in meters.
    pub gate_width_per_finger_m: f64,
    /// `(gate_length_m * 1e6)^LBINN`, dimensionless C binning divisor.
    pub length_bin: f64,
    /// `(gate_width_per_finger_m * 1e6)^WBINN`, dimensionless C binning divisor.
    pub width_bin: f64,
}

/// Per-instance, immutable source-accurate defaults and size-dependent parameters.
///
/// `binned` stores only parameters explicitly listed in the selected family's
/// `BINNING` macro calls. The other model parameters are not size-corrected here.
#[derive(Clone, Debug, PartialEq)]
pub struct PreparedHiSimHvInstance {
    parameters: BoundParameters,
    geometry: HiSimHvBinningGeometry,
    effective_geometry: HiSimHvEffectiveGeometry,
    transport_geometry: HiSimHvTransportGeometry,
    binned: Box<[Option<f64>]>,
    initial_conditions: [f64; 3],
    pub(super) model_identity: Arc<()>,
    pub(super) identity: Arc<()>,
}

impl PreparedHiSimHvInstance {
    pub(crate) fn validate_transport(
        &self,
        transport: &PreparedHiSimHvTransport,
    ) -> Result<(), ModelError> {
        if !Arc::ptr_eq(&self.identity, &transport.instance_identity) {
            return Err(ModelError::MismatchedPreparedContext);
        }
        Ok(())
    }

    /// Original physical implementation for this instance.
    #[must_use = "the variant identifies the physical implementation"]
    pub const fn variant(&self) -> ModelVariant {
        self.parameters.variant()
    }

    /// C-setup instance parameter values; use canonical numeric slots.
    #[must_use = "the returned buffer supplies the prepared instance values"]
    pub const fn parameters(&self) -> &BoundParameters {
        &self.parameters
    }

    /// Source-bin geometry, independent of the model's later temperature laws.
    #[must_use = "the geometry is used to evaluate size-dependent model laws"]
    pub const fn geometry(&self) -> HiSimHvBinningGeometry {
        self.geometry
    }

    /// Metallurgical effective channel dimensions and other temperature-independent
    /// expressions from the beginning of the C `*temp.c` implementation.
    #[must_use = "the returned dimensions are inputs to the temperature and transport laws"]
    pub const fn effective_geometry(&self) -> HiSimHvEffectiveGeometry {
        self.effective_geometry
    }

    /// Temperature-independent mobility, velocity, and thermal layout factors.
    #[must_use = "the returned factors are inputs to mobility and thermal laws"]
    pub const fn transport_geometry(&self) -> HiSimHvTransportGeometry {
        self.transport_geometry
    }

    /// One of the source family's 92/95 size-dependent model parameters.
    #[must_use = "the lookup result distinguishes absent binning from a computed value"]
    pub fn binned_model_slot(&self, slot: u16) -> Option<f64> {
        self.binned.get(usize::from(slot)).copied().flatten()
    }

    /// SPICE IC vector in the upstream C order `VDS`, `VGS`, `VBS` (volts).
    #[must_use = "the returned vector establishes the initial device state"]
    pub const fn initial_conditions(&self) -> [f64; 3] {
        self.initial_conditions
    }
}

const INSTANCE_REAL_DEFAULTS: &[(&str, f64)] = &[
    ("l", 2.0e-6),
    ("w", 5.0e-6),
    ("ad", 0.0),
    ("as", 0.0),
    ("pd", 0.0),
    ("ps", 0.0),
    ("nrd", 1.0),
    ("nrs", 1.0),
    ("ngcon", 1.0),
    ("xgw", 0.0),
    ("xgl", 0.0),
    ("nf", 1.0),
    ("sa", 0.0),
    ("sb", 0.0),
    ("sd", 0.0),
    ("dtemp", 0.0),
    ("m", 1.0),
];

const SHARED_MODEL_INHERITED: &[&str] = &[
    "coselfheat",
    "cosubnode",
    "corbnet",
    "rbdb",
    "rbsb",
    "rbpb",
    "rbps",
    "rbpd",
    "corg",
    "lovers",
    "loverld",
    "ldrift1",
    "ldrift2",
    "ldrift1s",
    "ldrift2s",
];

const HV1_MODEL_INHERITED: &[&str] = &["subld1", "subld2"];
const HV2_MODEL_INHERITED: &[&str] = &["lover"];

fn instance_slot(variant: ModelVariant, name: &str) -> Result<u16, ModelError> {
    variant
        .parameter(ParameterScope::Instance, name)
        .map(|spec| spec.slot)
        .ok_or_else(|| ModelError::UnknownParameter(name.to_owned()))
}

fn model_value<'a>(
    model: &'a PreparedHiSimHvModel,
    name: &str,
) -> Result<&'a ParameterValue, ModelError> {
    let slot = lookup_slot(model.variant(), name)?;
    model
        .get_slot(slot)
        .ok_or(ModelError::MissingDefaultDependency(slot))
}

pub(super) fn model_real(model: &PreparedHiSimHvModel, name: &str) -> Result<f64, ModelError> {
    match model_value(model, name)? {
        ParameterValue::Real(value) => Ok(*value),
        _ => Err(ModelError::InvalidPhysicalParameter(
            "model binning coefficient",
        )),
    }
}

pub(super) fn instance_real(
    values: &[Option<ParameterValue>],
    variant: ModelVariant,
    name: &str,
) -> Result<f64, ModelError> {
    let slot = instance_slot(variant, name)?;
    match values.get(usize::from(slot)).and_then(Option::as_ref) {
        Some(ParameterValue::Real(value)) => Ok(*value),
        _ => Err(ModelError::MissingDefaultDependency(slot)),
    }
}

fn instance_integer(
    values: &[Option<ParameterValue>],
    variant: ModelVariant,
    name: &str,
) -> Result<i32, ModelError> {
    let slot = instance_slot(variant, name)?;
    match values.get(usize::from(slot)).and_then(Option::as_ref) {
        Some(ParameterValue::Integer(value)) => Ok(*value),
        _ => Err(ModelError::MissingDefaultDependency(slot)),
    }
}

fn write_instance_value(
    values: &mut [Option<ParameterValue>],
    variant: ModelVariant,
    name: &str,
    value: ParameterValue,
) -> Result<(), ModelError> {
    let slot = instance_slot(variant, name)?;
    values[usize::from(slot)] = Some(value);
    Ok(())
}

fn instance_given(source: &BoundParameters, name: &str) -> Result<bool, ModelError> {
    Ok(source
        .get_slot(instance_slot(source.variant(), name)?)
        .is_some())
}

fn model_given(model: &PreparedHiSimHvModel, name: &str) -> Result<bool, ModelError> {
    Ok(model.explicitly_given(lookup_slot(model.variant(), name)?))
}

fn copy_instance_value(
    values: &mut [Option<ParameterValue>],
    variant: ModelVariant,
    destination: &str,
    source: &str,
) -> Result<(), ModelError> {
    let src_slot = instance_slot(variant, source)?;
    let value = values[usize::from(src_slot)]
        .clone()
        .ok_or(ModelError::MissingDefaultDependency(src_slot))?;
    write_instance_value(values, variant, destination, value)
}

fn apply_instance_defaults(
    model: &PreparedHiSimHvModel,
    raw: &BoundParameters,
) -> Result<Box<[Option<ParameterValue>]>, ModelError> {
    let variant = raw.variant();
    let mut values = raw.values.clone();
    for &(name, value) in INSTANCE_REAL_DEFAULTS {
        if !instance_given(raw, name)? {
            write_instance_value(&mut values, variant, name, ParameterValue::Real(value))?;
        }
    }
    for &name in SHARED_MODEL_INHERITED
        .iter()
        .chain(if variant == ModelVariant::HiSimHv1 {
            HV1_MODEL_INHERITED.iter()
        } else {
            HV2_MODEL_INHERITED.iter()
        })
    {
        if !instance_given(raw, name)? {
            write_instance_value(
                &mut values,
                variant,
                name,
                model_value(model, name)?.clone(),
            )?;
        }
    }
    // C HV1 does this even when LOVERS was explicitly given on the instance.
    if variant == ModelVariant::HiSimHv1 && instance_given(raw, "lover")? {
        copy_instance_value(&mut values, variant, "lovers", "lover")?;
    }
    for name in ["corg", "corbnet"] {
        let value = instance_integer(&values, variant, name)?;
        if !(0..=1).contains(&value) {
            return Err(ModelError::InvalidSelector {
                name: name.to_owned(),
                value,
            });
        }
    }
    if variant == ModelVariant::HiSimHv2
        && (instance_given(raw, "subld1")? || instance_given(raw, "subld2")?)
    {
        return Err(ModelError::UnsupportedInstanceParameter("subld1/subld2"));
    }
    if matches!(model_value(model, "cosym")?, ParameterValue::Integer(sym) if *sym != 0) {
        apply_symmetric_instance_defaults(model, raw, &mut values)?;
    }
    // The C device structs are zero-initialized. Unlike model setup rules,
    // these remaining unassigned scalar instance fields therefore have a
    // definite physical value even without an explicit `*Given` flag.
    for spec in variant.parameters(ParameterScope::Instance) {
        let cell = &mut values[usize::from(spec.slot)];
        if cell.is_none() {
            *cell = match spec.kind {
                ParameterType::Real => Some(ParameterValue::Real(0.0)),
                ParameterType::Integer => Some(ParameterValue::Integer(0)),
                ParameterType::Flag => Some(ParameterValue::Flag(false)),
                ParameterType::Text | ParameterType::RealVector => None,
            };
        }
    }
    Ok(values)
}

fn apply_symmetric_instance_defaults(
    model: &PreparedHiSimHvModel,
    raw: &BoundParameters,
    values: &mut [Option<ParameterValue>],
) -> Result<(), ModelError> {
    let variant = raw.variant();
    if variant == ModelVariant::HiSimHv1 {
        if !instance_given(raw, "lovers")? && !model_given(model, "lovers")? {
            copy_instance_value(values, variant, "lovers", "loverld")?;
        }
        copy_instance_value(values, variant, "lover", "lovers")?;
    } else {
        if !instance_given(raw, "lover")? && !model_given(model, "lover")? {
            copy_instance_value(values, variant, "lover", "loverld")?;
        }
        if !instance_given(raw, "lovers")? && !model_given(model, "lovers")? {
            copy_instance_value(values, variant, "lovers", "lover")?;
        }
    }
    for (target, drain) in [("ldrift1s", "ldrift1"), ("ldrift2s", "ldrift2")] {
        if !instance_given(raw, target)? && !model_given(model, target)? {
            copy_instance_value(values, variant, target, drain)?;
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct BinningRule {
    parameter: u16,
    length: u16,
    width: u16,
    cross: u16,
}

static HV1_BINNING: OnceLock<Result<Box<[BinningRule]>, ModelError>> = OnceLock::new();
static HV2_BINNING: OnceLock<Result<Box<[BinningRule]>, ModelError>> = OnceLock::new();

fn compile_binning(variant: ModelVariant) -> Result<Box<[BinningRule]>, ModelError> {
    let key = if variant == ModelVariant::HiSimHv1 {
        "HV1"
    } else {
        "HV2"
    };
    let contents = include_str!("binning.tsv");
    let (_, values) = contents
        .lines()
        .find_map(|line| line.split_once('\t').filter(|(name, _)| *name == key))
        .ok_or_else(|| {
            ModelError::CorruptSourceCatalog(format!("missing {key} binning program"))
        })?;
    let program: Box<[BinningRule]> = values
        .split_whitespace()
        .map(|name| {
            Ok(BinningRule {
                parameter: lookup_slot(variant, name)?,
                length: lookup_slot(variant, &format!("l{name}"))?,
                width: lookup_slot(variant, &format!("w{name}"))?,
                cross: lookup_slot(variant, &format!("p{name}"))?,
            })
        })
        .collect::<Result<_, ModelError>>()?;
    let expected = if variant == ModelVariant::HiSimHv1 {
        92
    } else {
        95
    };
    if program.len() != expected {
        return Err(ModelError::CorruptSourceCatalog(format!(
            "{key}: expected {expected} binning operations, got {}",
            program.len()
        )));
    }
    Ok(program)
}

fn binning_rules(variant: ModelVariant) -> Result<&'static [BinningRule], ModelError> {
    let rules = compiled_hv_by_family(variant, &HV1_BINNING, &HV2_BINNING, compile_binning)?;
    Ok(rules.as_ref())
}

fn bin_geometry(
    model: &PreparedHiSimHvModel,
    instance: &[Option<ParameterValue>],
) -> Result<HiSimHvBinningGeometry, ModelError> {
    let variant = model.variant();
    let l = instance_real(instance, variant, "l")?;
    let w = instance_real(instance, variant, "w")?;
    let nf = instance_real(instance, variant, "nf")?;
    let m = instance_real(instance, variant, "m")?;
    if l <= 0.0 || w <= 0.0 || nf <= 0.0 || m <= 0.0 {
        return Err(ModelError::InvalidPhysicalParameter("instance L/W/NF/M"));
    }
    let lg = l + model_real(model, "xl")?;
    let wg = w / nf + model_real(model, "xw")?;
    if !lg.is_finite() || !wg.is_finite() || lg <= 0.0 || wg <= 0.0 {
        return Err(ModelError::InvalidPhysicalParameter(
            "HiSIM-HV binning geometry",
        ));
    }
    let lb = (lg * 1e6).powf(model_real(model, "lbinn")?);
    let wb = (wg * 1e6).powf(model_real(model, "wbinn")?);
    if !lb.is_finite()
        || !wb.is_finite()
        || lb <= 0.0
        || wb <= 0.0
        || !(lb * wb).is_finite()
        || lb * wb <= 0.0
    {
        return Err(ModelError::InvalidPhysicalParameter(
            "HiSIM-HV binning divisor",
        ));
    }
    Ok(HiSimHvBinningGeometry {
        gate_length_m: lg,
        gate_width_per_finger_m: wg,
        length_bin: lb,
        width_bin: wb,
    })
}

fn bin_values(
    model: &PreparedHiSimHvModel,
    geometry: HiSimHvBinningGeometry,
) -> Result<Box<[Option<f64>]>, ModelError> {
    let mut binned = vec![None; model.parameters().values.len()].into_boxed_slice();
    for rule in binning_rules(model.variant())? {
        let value = source_real_at(model, rule.parameter)?
            + source_real_at(model, rule.length)? / geometry.length_bin
            + source_real_at(model, rule.width)? / geometry.width_bin
            + source_real_at(model, rule.cross)? / (geometry.length_bin * geometry.width_bin);
        if !value.is_finite() {
            return Err(ModelError::InvalidBinningParameter(rule.parameter));
        }
        binned[usize::from(rule.parameter)] = Some(value);
    }
    Ok(binned)
}

fn initial_voltages(raw: &BoundParameters) -> Result<[f64; 3], ModelError> {
    let slot = instance_slot(raw.variant(), "ic")?;
    match raw.get_slot(slot) {
        None => Ok([0.0; 3]),
        Some(ParameterValue::RealVector(values)) if (1..=3).contains(&values.len()) => {
            let mut result = [0.0; 3];
            result[..values.len()].copy_from_slice(values);
            Ok(result)
        }
        Some(ParameterValue::RealVector(values)) => {
            Err(ModelError::InvalidInitialConditions(values.len()))
        }
        _ => Err(ModelError::MissingDefaultDependency(slot)),
    }
}

/// Prepare one HV1 or HV2 instance against a fully prepared model block.
///
/// # Errors
/// Returns errors for model/scope mismatch, unsupported selector, invalid
/// physical geometry or binning divisor, invalid IC vector, or missing C default.
pub fn prepare_hisim_hv_instance(
    model: &PreparedHiSimHvModel,
    raw: &BoundParameters,
) -> Result<PreparedHiSimHvInstance, ModelError> {
    if raw.variant() != model.variant() || raw.scope() != ParameterScope::Instance {
        return Err(ModelError::WrongModelScope);
    }
    let initial_conditions = initial_voltages(raw)?;
    let values = apply_instance_defaults(model, raw)?;
    let geometry = bin_geometry(model, &values)?;
    let binned = bin_values(model, geometry)?;
    let effective_geometry = prepare_effective_geometry(model, &values, geometry, &binned)?;
    let transport_geometry =
        prepare_transport_geometry(model, raw, &values, geometry, effective_geometry, &binned)?;
    Ok(PreparedHiSimHvInstance {
        parameters: BoundParameters {
            variant: raw.variant(),
            scope: ParameterScope::Instance,
            values,
            bsim_soi_ic: [None; 5],
        },
        geometry,
        effective_geometry,
        transport_geometry,
        binned,
        initial_conditions,
        model_identity: Arc::clone(&model.identity),
        identity: Arc::new(()),
    })
}
