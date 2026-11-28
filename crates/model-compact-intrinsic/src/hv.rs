//! HiSIM-HV1 and HiSIM-HV2 model initialization, in original source order.
//!
//! This is setup-stage parameter preparation, not a physical model implementation.

mod access_correction;
mod accumulation;
mod bias_conditioning;
mod channel_charge;
mod channel_correction;
mod channel_electrostatics;
mod channel_mobility;
mod channel_modulation;
mod depletion_drain;
mod depletion_surface;
mod depletion_transport;
mod drain_smoothing;
mod drift;
mod edge_tunneling;
mod effective;
mod electrostatic_material;
mod explicit_series;
mod fringing;
mod gate_tunneling;
mod gidl;
mod heat;
mod impact;
mod instance;
mod junction;
mod junction_current;
mod lateral_field;
mod leakage;
mod noise;
mod nqs;
mod overlap;
mod poly_depletion;
mod quantum_oxide;
mod rules;
mod smoothing;
mod sti;
mod surface;
mod temperature;
mod threshold;
mod transport;

pub use access_correction::{
    HiSimHvAccessCorrectionPoint, HiSimHvAccessInputs, HiSimHvExternalBias,
    PreparedHiSimHvAccessCorrection, prepare_hisim_hv_access_correction,
};
pub use accumulation::{
    HiSimHvAccumulationInputs, HiSimHvAccumulationPoint, evaluate_hisim_hv_accumulation,
};
pub use bias_conditioning::{
    HiSimHvBiasConditioningInputs, HiSimHvBiasConditioningPoint,
    evaluate_hisim_hv_bias_conditioning,
};
pub use channel_charge::{
    HiSimHvChannelChargeInputs, HiSimHvChannelChargePoint, HiSimHvChannelRegion,
    evaluate_hisim_hv_channel_charge,
};
pub use channel_correction::HiSimHvChannelCorrectionPoint;
pub use channel_electrostatics::{
    HiSimHvChannelElectrostaticPoint, PreparedHiSimHvChannelElectrostatics,
    prepare_hisim_hv_channel_electrostatics,
};
pub use channel_mobility::{
    HiSimHvChannelMobilityCoefficients, HiSimHvChannelMobilityInputs, HiSimHvChannelMobilityPoint,
    HiSimHvNormalChannelPoint, HiSimHvNormalChannelWithAccessPoint,
    HiSimHvNormalChannelWithParasiticsPoint, PreparedHiSimHvNormalChannel,
    prepare_hisim_hv_normal_channel,
};
pub use channel_modulation::{
    HiSimHvChannelModulationInputs, HiSimHvChannelModulationPoint, HiSimHvChannelTerminalCharges,
    PreparedHiSimHvChannelChargeArea, PreparedHiSimHvChannelModulation,
    prepare_hisim_hv_channel_modulation,
};
pub use depletion_drain::{
    HiSimHvDepletionDrainInputs, HiSimHvDepletionDrainPoint, evaluate_hisim_hv_depletion_drain,
};
pub use depletion_surface::{
    HiSimHvDepletionEndpointPoint, HiSimHvDepletionSurfaceInputs, HiSimHvDepletionSurfacePoint,
    evaluate_hisim_hv_depletion_endpoint, evaluate_hisim_hv_depletion_surface,
};
pub use depletion_transport::{
    HiSimHvDepletionCarrierMobility, HiSimHvDepletionChannelCoefficients,
    HiSimHvDepletionChannelInputs, HiSimHvDepletionChannelPoint,
};
pub use drain_smoothing::{
    HiSimHvDrainSmoothingInputs, HiSimHvDrainSmoothingPoint, evaluate_hisim_hv_drain_smoothing,
};
pub use drift::{
    HiSimHvDriftBias, HiSimHvDriftPoint, HiSimHvDriftScalar, PreparedHiSimHvDrift,
    prepare_hisim_hv_drift,
};
pub use edge_tunneling::{
    HiSimHvEdgeTunnelingCurrent, HiSimHvEdgeTunnelingCurrents, PreparedHiSimHvEdgeTunneling,
    prepare_hisim_hv_edge_tunneling,
};
pub use effective::HiSimHvEffectiveGeometry;
pub use electrostatic_material::{
    HiSimHvDepletionMaterialPoint, HiSimHvElectrostaticMaterialPoint,
    PreparedHiSimHvElectrostaticMaterial, prepare_hisim_hv_electrostatic_material,
};
pub use explicit_series::{
    HiSimHvExplicitSeriesBias, HiSimHvExplicitSeriesBranch, HiSimHvExplicitSeriesPoint,
    HiSimHvExplicitSeriesScalar, PreparedHiSimHvExplicitSeries, prepare_hisim_hv_explicit_series,
};
pub use fringing::{HiSimHvFringingCharges, PreparedHiSimHvFringing, prepare_hisim_hv_fringing};
pub use gate_tunneling::{
    HiSimHvGateChannelState, HiSimHvGateTunnelingCurrents, PreparedHiSimHvGateTunneling,
    prepare_hisim_hv_gate_tunneling,
};
pub use gidl::{
    HiSimHvGidlCurrents, HiSimHvIntrinsicBias, HiSimHvIntrinsicScalar, PreparedHiSimHvGidl,
    prepare_hisim_hv_gidl,
};
pub use heat::{
    HiSimHvDrainBias, HiSimHvDrainCurrent, HiSimHvElectricalGradient, HiSimHvGeneratedHeat,
    HiSimHvThermalJacobianRow, evaluate_hisim_hv_generated_heat,
};
pub use impact::{HiSimHvImpactPoint, HiSimHvImpactScalar};
pub use instance::{HiSimHvBinningGeometry, PreparedHiSimHvInstance, prepare_hisim_hv_instance};
pub use junction::{
    HiSimHvJunctionBias, HiSimHvJunctionCharge, HiSimHvJunctionCharges,
    PreparedHiSimHvJunctionCharges, prepare_hisim_hv_junction_charges,
};
pub use junction_current::{
    HiSimHvJunctionCurrent, HiSimHvJunctionCurrents, PreparedHiSimHvJunctionDiodes,
    prepare_hisim_hv_junction_diodes,
};
pub use lateral_field::{
    HiSimHvLateralFieldCharge, PreparedHiSimHvLateralFieldCharge,
    prepare_hisim_hv_lateral_field_charge,
};
pub use leakage::HiSimHvLeakagePoint;
pub use noise::HiSimHvChannelNoise;
pub use nqs::{HiSimHvNqsEquilibrium, HiSimHvNqsPoint, HiSimHvNqsScalar, HiSimHvNqsState};
pub use overlap::{
    HiSimHvChargeNetwork, HiSimHvDrainOverlap, HiSimHvOverlapBias, HiSimHvOverlapPoint,
    PreparedHiSimHvOverlap, prepare_hisim_hv_overlap,
};
pub use poly_depletion::{
    HiSimHvPolyDepletionPoint, PreparedHiSimHvPolyDepletion, prepare_hisim_hv_poly_depletion,
};
pub use quantum_oxide::{
    HiSimHvQuantumOxidePoint, PreparedHiSimHvQuantumOxide, prepare_hisim_hv_quantum_oxide,
};
pub use surface::{HiSimHvSurfaceInputs, HiSimHvSurfacePoint, evaluate_hisim_hv_surface};
pub use temperature::{PreparedHiSimHvTemperature, prepare_hisim_hv_temperature};
pub use threshold::{HiSimHvThresholdPoint, PreparedHiSimHvThreshold, prepare_hisim_hv_threshold};
pub use transport::{
    HiSimHvDepletionTransport, HiSimHvThermalBranch, HiSimHvTransportGeometry,
    PreparedHiSimHvTransport, prepare_hisim_hv_transport,
};

use std::sync::{Arc, OnceLock};

use crate::{
    BoundParameters, ModelError, ModelVariant, MosPolarity, ParameterScope, ParameterValue,
};

/// Immutable model-card output after the selected HiSIM-HV implementation's
/// source-defined defaults, polarity/mode-dependent adjustments and aliases.
#[derive(Clone, Debug, PartialEq)]
pub struct PreparedHiSimHvModel {
    parameters: BoundParameters,
    polarity: MosPolarity,
    original_given: Box<[bool]>,
    /// Unique stable model provenance; retained across model clones and
    /// prepared instances so equal-valued but different model cards cannot mix.
    identity: Arc<()>,
}

impl PreparedHiSimHvModel {
    /// Resolved source-level model implementation (HV1 or HV2).
    #[must_use = "the resolved variant selects the physical model equations"]
    pub const fn variant(&self) -> ModelVariant {
        self.parameters.variant()
    }

    /// Resolved channel polarity.
    #[must_use = "the channel polarity determines the device equations"]
    pub const fn polarity(&self) -> MosPolarity {
        self.polarity
    }

    /// O(1) numerical slot access, with no runtime string lookup.
    #[must_use = "the lookup result distinguishes a missing slot from a bound value"]
    pub fn get_slot(&self, slot: u16) -> Option<&ParameterValue> {
        self.parameters.get_slot(slot)
    }

    /// Immutable prepared parameter buffer.
    #[must_use = "the returned buffer supplies the immutable model parameters"]
    pub const fn parameters(&self) -> &BoundParameters {
        &self.parameters
    }

    pub(crate) fn validate_instance(
        &self,
        instance: &PreparedHiSimHvInstance,
    ) -> Result<(), ModelError> {
        if self.variant() != instance.variant() {
            return Err(ModelError::WrongModelScope);
        }
        if !Arc::ptr_eq(&self.identity, &instance.model_identity) {
            return Err(ModelError::MismatchedPreparedContext);
        }
        Ok(())
    }

    /// Source model-card `*Given` flag, retained across setup assignments.
    pub(crate) fn explicitly_given(&self, slot: u16) -> bool {
        self.original_given
            .get(usize::from(slot))
            .is_some_and(|given| *given)
    }
}

#[derive(Clone, Copy)]
enum HvValue {
    Real(f64),
    Integer(i32),
    Text(&'static str),
    Copy(&'static str),
    Polarity(f64, f64),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HvCondition {
    Always,
    Depletion,
    Symmetry,
    Drift,
    SourceGiven,
}

#[derive(Clone, Copy)]
struct HvRule {
    name: &'static str,
    value: HvValue,
    condition: HvCondition,
    /// ngspice conditions inspect the *Given bits*, not whether an earlier
    /// setup-default assignment has already populated the parameter.
    overwrite: bool,
}

impl HvRule {
    const fn new(name: &'static str, value: HvValue, condition: HvCondition) -> Self {
        Self {
            name,
            value,
            condition,
            overwrite: false,
        }
    }

    const fn force(name: &'static str, value: HvValue, condition: HvCondition) -> Self {
        Self {
            name,
            value,
            condition,
            overwrite: true,
        }
    }
}

static SOURCE_RULES: OnceLock<Result<Box<[HvRule]>, ModelError>> = OnceLock::new();

fn invalid_rule(line: usize, problem: &str) -> ModelError {
    ModelError::CorruptSourceCatalog(format!("HiSIM-HV source row {line}: {problem}"))
}

fn parse_rule(line_number: usize, line: &'static str) -> Result<HvRule, ModelError> {
    let columns = line.split('\t').collect::<Vec<_>>();
    let [name, kind, raw, condition, assignment] = columns.as_slice() else {
        return Err(invalid_rule(
            line_number,
            "expected five tab-separated fields",
        ));
    };
    let value = match *kind {
        "R" => HvValue::Real(
            raw.parse()
                .map_err(|_| invalid_rule(line_number, "invalid real"))?,
        ),
        "I" => HvValue::Integer(
            raw.parse()
                .map_err(|_| invalid_rule(line_number, "invalid integer"))?,
        ),
        "T" => HvValue::Text(raw),
        "C" => HvValue::Copy(raw),
        "P" => {
            let (nmos, pmos) = raw
                .split_once(',')
                .ok_or_else(|| invalid_rule(line_number, "invalid polarity expression"))?;
            HvValue::Polarity(
                nmos.parse()
                    .map_err(|_| invalid_rule(line_number, "invalid NMOS real"))?,
                pmos.parse()
                    .map_err(|_| invalid_rule(line_number, "invalid PMOS real"))?,
            )
        }
        _ => return Err(invalid_rule(line_number, "unknown value kind")),
    };
    let condition = match *condition {
        "Always" => HvCondition::Always,
        "Depletion" => HvCondition::Depletion,
        "Symmetry" => HvCondition::Symmetry,
        "Drift" => HvCondition::Drift,
        "SourceGiven" => HvCondition::SourceGiven,
        _ => return Err(invalid_rule(line_number, "unknown execution condition")),
    };
    match *assignment {
        "new" => Ok(HvRule::new(name, value, condition)),
        "force" => Ok(HvRule::force(name, value, condition)),
        _ => Err(invalid_rule(line_number, "unknown assignment semantics")),
    }
}

fn parse_source_rules(source: &'static str) -> Result<Box<[HvRule]>, ModelError> {
    let mut parsed = Vec::new();
    for (line, text) in source.lines().enumerate() {
        if text.starts_with('#') || text.is_empty() {
            continue;
        }
        parsed.push(parse_rule(line + 1, text)?);
    }
    if parsed.len() != 773 {
        return Err(ModelError::CorruptSourceCatalog(format!(
            "HiSIM-HV source rule pool has {} rows, expected 773",
            parsed.len()
        )));
    }
    Ok(parsed.into_boxed_slice())
}

fn source_rules() -> Result<&'static [HvRule], ModelError> {
    match SOURCE_RULES.get_or_init(|| parse_source_rules(rules::SOURCE_RULES)) {
        Ok(rules) => Ok(rules),
        Err(error) => Err(error.clone()),
    }
}

/// Resolved once for each physical family. No string resolution occurs while
/// preparing subsequent candidate/model parameter batches.
#[derive(Clone, Copy)]
struct BoundHvRule {
    slot: u16,
    source_slot: Option<u16>,
    value: HvValue,
    condition: HvCondition,
    overwrite: bool,
}

static HV1_COMPILED: OnceLock<Result<Box<[BoundHvRule]>, ModelError>> = OnceLock::new();
static HV2_COMPILED: OnceLock<Result<Box<[BoundHvRule]>, ModelError>> = OnceLock::new();

fn compile_hv_program(variant: ModelVariant) -> Result<Box<[BoundHvRule]>, ModelError> {
    let pool = source_rules()?;
    let program = if variant == ModelVariant::HiSimHv1 {
        rules::HV1_PROGRAM
    } else {
        rules::HV2_PROGRAM
    };
    program
        .iter()
        .map(|&index| {
            let rule = pool.get(usize::from(index)).ok_or_else(|| {
                ModelError::CorruptSourceCatalog(format!(
                    "HiSIM-HV rule index {index} out of range"
                ))
            })?;
            let source_slot = match rule.value {
                HvValue::Copy(name) => Some(lookup_slot(variant, name)?),
                _ => None,
            };
            Ok(BoundHvRule {
                slot: lookup_slot(variant, rule.name)?,
                source_slot,
                value: rule.value,
                condition: rule.condition,
                overwrite: rule.overwrite,
            })
        })
        .collect::<Result<Box<[_]>, _>>()
}

/// Select and compile the original variant's cached source program once.
/// Applied to model setup, binning, and temperature transport slot catalogs.
fn compiled_hv_by_family<T, F>(
    variant: ModelVariant,
    hv1: &'static OnceLock<Result<T, ModelError>>,
    hv2: &'static OnceLock<Result<T, ModelError>>,
    build: F,
) -> Result<&'static T, ModelError>
where
    F: FnOnce(ModelVariant) -> Result<T, ModelError>,
{
    let catalog = if variant == ModelVariant::HiSimHv1 {
        hv1
    } else {
        hv2
    };
    catalog
        .get_or_init(|| build(variant))
        .as_ref()
        .map_err(Clone::clone)
}

fn bound_hv_program(variant: ModelVariant) -> Result<&'static [BoundHvRule], ModelError> {
    let program = compiled_hv_by_family(variant, &HV1_COMPILED, &HV2_COMPILED, compile_hv_program)?;
    Ok(program.as_ref())
}

/// Common numeric slot shape for additive physical charge coefficients:
/// two original model scalars, model `COADOV`, and instance `M`.
/// Each physical law retains its own `OnceLock` and original numeric equations.
#[derive(Clone, Copy)]
struct AdditiveChargeSlots {
    model_first: u16,
    model_second: u16,
    coadov: u16,
    multiplier: u16,
}

fn compile_additive_charge_slots(
    variant: ModelVariant,
    first: &'static str,
    second: &'static str,
) -> Result<AdditiveChargeSlots, ModelError> {
    let multiplier = variant
        .parameter(ParameterScope::Instance, "m")
        .ok_or_else(|| ModelError::UnknownParameter("m".to_owned()))?
        .slot;
    Ok(AdditiveChargeSlots {
        model_first: lookup_slot(variant, first)?,
        model_second: lookup_slot(variant, second)?,
        coadov: lookup_slot(variant, "coadov")?,
        multiplier,
    })
}

fn lookup_slot(variant: ModelVariant, name: &str) -> Result<u16, ModelError> {
    variant
        .parameter(ParameterScope::Model, name)
        .map(|entry| entry.slot)
        .ok_or_else(|| ModelError::UnknownParameter(name.to_owned()))
}

/// Shared required numeric instance-slot read for all source-accurate
/// physical HiSIM-HV equations. The caller resolves its numeric slot once.
fn instance_real_at(instance: &PreparedHiSimHvInstance, slot: u16) -> Result<f64, ModelError> {
    match instance.parameters().get_slot(slot) {
        Some(ParameterValue::Real(value)) => Ok(*value),
        _ => Err(ModelError::MissingDefaultDependency(slot)),
    }
}

/// Preserve a per-physical-component error while sharing finite-value policy.
const fn hv_finite_or_error(value: f64, reason: &'static str) -> Result<f64, ModelError> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(ModelError::InvalidPhysicalParameter(reason))
    }
}

fn source_real_at(model: &PreparedHiSimHvModel, slot: u16) -> Result<f64, ModelError> {
    match model.get_slot(slot) {
        Some(ParameterValue::Real(value)) => Ok(*value),
        _ => Err(ModelError::MissingDefaultDependency(slot)),
    }
}

fn source_binned_at_name(
    model: &PreparedHiSimHvModel,
    values: &[Option<f64>],
    name: &str,
) -> Result<f64, ModelError> {
    let slot = lookup_slot(model.variant(), name)?;
    values
        .get(usize::from(slot))
        .copied()
        .flatten()
        .ok_or(ModelError::MissingDefaultDependency(slot))
}

fn param_integer(values: &[Option<ParameterValue>], slot: u16) -> Result<i32, ModelError> {
    match values.get(usize::from(slot)).and_then(Option::as_ref) {
        Some(ParameterValue::Integer(value)) => Ok(*value),
        _ => Err(ModelError::MissingDefaultDependency(slot)),
    }
}

fn apply_hv_rules(
    source: &BoundParameters,
    polarity: MosPolarity,
) -> Result<Box<[Option<ParameterValue>]>, ModelError> {
    let variant = source.variant();
    let mut values = source.values.clone();
    let level_slot = lookup_slot(variant, "level")?;
    match source.get_slot(level_slot) {
        Some(ParameterValue::Integer(73)) | None => {
            values[usize::from(level_slot)] = Some(ParameterValue::Integer(73));
        }
        Some(ParameterValue::Integer(level)) => return Err(ModelError::UnknownLevel(*level)),
        _ => return Err(ModelError::MissingDefaultDependency(level_slot)),
    }
    let slot_codep = if variant == ModelVariant::HiSimHv2 {
        Some(lookup_slot(variant, "codep")?)
    } else {
        None
    };
    let slot_cordrift = if variant == ModelVariant::HiSimHv2 {
        Some(lookup_slot(variant, "cordrift")?)
    } else {
        None
    };
    let slot_cosym = lookup_slot(variant, "cosym")?;
    let program = bound_hv_program(variant)?;
    for rule in program {
        let destination = usize::from(rule.slot);
        if !rule.overwrite && source.values[destination].is_some() {
            continue;
        }
        let matches = match rule.condition {
            HvCondition::Always => true,
            HvCondition::Depletion => {
                param_integer(&values, slot_codep.ok_or(ModelError::WrongModelScope)?)? != 0
            }
            HvCondition::Symmetry => param_integer(&values, slot_cosym)? != 0,
            HvCondition::Drift => {
                param_integer(&values, slot_cordrift.ok_or(ModelError::WrongModelScope)?)? != 0
            }
            HvCondition::SourceGiven => match rule.value {
                HvValue::Copy(_) => rule
                    .source_slot
                    .is_some_and(|slot| source.get_slot(slot).is_some()),
                _ => return Err(ModelError::MissingDefaultDependency(rule.slot)),
            },
        };
        if !matches {
            continue;
        }
        let value = match rule.value {
            HvValue::Real(value) => ParameterValue::Real(value),
            HvValue::Integer(value) => ParameterValue::Integer(value),
            HvValue::Text(value) => ParameterValue::Text(value.to_owned()),
            HvValue::Copy(_) => {
                let slot = rule
                    .source_slot
                    .ok_or(ModelError::MissingDefaultDependency(rule.slot))?;
                values
                    .get(usize::from(slot))
                    .and_then(Option::as_ref)
                    .cloned()
                    .ok_or(ModelError::MissingDefaultDependency(slot))?
            }
            HvValue::Polarity(nmos, pmos) => {
                ParameterValue::Real(if polarity == MosPolarity::Nmos {
                    nmos
                } else {
                    pmos
                })
            }
        };
        values[destination] = Some(value);
    }

    Ok(values)
}

/// Apply source-order model-level parameter preparation for HiSIM-HV1/HV2.
///
/// Both families are treated separately: HV2's `CODEP` depletion defaults,
/// `CORDRIFT` override and sub-version rules do not apply to HV1. When `COSYM`
/// is enabled, the source-side parameters are copied in the same order and
/// with the same `*Given` semantics as the upstream C implementation.
///
/// # Errors
/// Rejects the wrong model family/scope, contradictory polarity, unrecognized
/// physical version or invalid selector instead of silently switching modes.
pub fn prepare_hisim_hv_model(
    source: &BoundParameters,
) -> Result<PreparedHiSimHvModel, ModelError> {
    let variant = source.variant();
    if !matches!(variant, ModelVariant::HiSimHv1 | ModelVariant::HiSimHv2)
        || source.scope() != ParameterScope::Model
    {
        return Err(ModelError::WrongModelScope);
    }
    let flag = |name| -> Result<bool, ModelError> {
        let slot = lookup_slot(variant, name)?;
        match source.get_slot(slot) {
            Some(ParameterValue::Flag(value)) => Ok(*value),
            None => Ok(false),
            _ => Err(ModelError::MissingDefaultDependency(slot)),
        }
    };
    let pmos = flag("pmos")?;
    if pmos && flag("nmos")? {
        return Err(ModelError::ConflictingPolarity);
    }
    let polarity = if pmos {
        MosPolarity::Pmos
    } else {
        MosPolarity::Nmos
    };
    if let Some(ParameterValue::Text(given)) = source.get_slot(lookup_slot(variant, "version")?) {
        let accepted = match variant {
            ModelVariant::HiSimHv1 => given.starts_with("1.24"),
            ModelVariant::HiSimHv2 => given.starts_with("2.2"),
            _ => unreachable!(),
        };
        if !accepted {
            return Err(ModelError::UnknownHiSimHvVersion(given.clone()));
        }
    }

    let values = apply_hv_rules(source, polarity)?;

    let tox_slot = lookup_slot(variant, "tox")?;
    if !matches!(values[usize::from(tox_slot)], Some(ParameterValue::Real(value)) if value > 0.0) {
        return Err(ModelError::InvalidPhysicalParameter("tox"));
    }
    // The HV1 C setup clamps an explicitly given sub-nanometer XQY to 1 nm.
    // Reject instead, so a parameter batch cannot silently change the model.
    if variant == ModelVariant::HiSimHv1 {
        let xqy_slot = lookup_slot(variant, "xqy")?;
        if matches!(values[usize::from(xqy_slot)], Some(ParameterValue::Real(value)) if value > 0.0 && value < 1e-9)
        {
            return Err(ModelError::InvalidPhysicalParameter("xqy"));
        }
    }
    if variant == ModelVariant::HiSimHv2 {
        for name in ["codep", "coddlt", "corbnet"] {
            let slot = lookup_slot(variant, name)?;
            let value = param_integer(&values, slot)?;
            if !(0..=1).contains(&value) {
                return Err(ModelError::InvalidSelector {
                    name: name.to_owned(),
                    value,
                });
            }
        }
    }

    Ok(PreparedHiSimHvModel {
        parameters: BoundParameters {
            variant,
            scope: ParameterScope::Model,
            values,
            bsim_soi_ic: [None; 5],
        },
        polarity,
        original_given: source.values.iter().map(Option::is_some).collect(),
        identity: Arc::new(()),
    })
}
