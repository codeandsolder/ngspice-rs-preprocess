//! Typed source-derived schemas for intrinsic `HiSIM`, SOI and bulk BSIM MOS compact models.
//!
//! This crate provides model/instance parameter binding and family-specific
//! constitutive routines. Binding alone is not proof of complete device support.
//! Solver-ready residual/charge artifacts are provided by separate kernel crates.

mod bsim_soi;
mod catalog;
mod defaults;
mod differential;
mod hv;
mod legacy_bsim;

pub use bsim_soi::{
    BsimSoiBodyMode, BsimSoiChannelBias, BsimSoiChannelPoint, BsimSoiChannelScalar,
    BsimSoiCollectorCurrent, BsimSoiDrainChannelPoint, BsimSoiElectrostaticBase,
    BsimSoiElectrostaticPoint, BsimSoiExternalSeries, BsimSoiExtrinsicChargeBias,
    BsimSoiExtrinsicCharges, BsimSoiGateBias, BsimSoiGateCurrents, BsimSoiGateInducedLeakage,
    BsimSoiGeometry, BsimSoiHeatPoint, BsimSoiImpactBias, BsimSoiImpactBranch,
    BsimSoiImpactIonization, BsimSoiInstanceInput, BsimSoiInternalResistance,
    BsimSoiIntrinsicCharges, BsimSoiJunctionCharge, BsimSoiJunctionCharges, BsimSoiJunctionCurrent,
    BsimSoiJunctionDensities, BsimSoiJunctionPoint, BsimSoiJunctionSide, BsimSoiJunctionStorage,
    BsimSoiNeutralBody, BsimSoiNode, BsimSoiNoiseBias, BsimSoiNoisePoint, BsimSoiParasiticPoint,
    BsimSoiSeriesBias, BsimSoiSeriesConductance, BsimSoiSeriesParameters, BsimSoiStressOffsets,
    BsimSoiTemperaturePoint, BsimSoiThermalNetwork, BsimSoiThermalScalar, BsimSoiTopology,
    BsimSoiTransportPoint, PreparedBsimSoiChannel, PreparedBsimSoiDrainChannel,
    PreparedBsimSoiElectrostatics, PreparedBsimSoiExtrinsicCharges,
    PreparedBsimSoiGateInducedLeakage, PreparedBsimSoiGateTunneling,
    PreparedBsimSoiImpactIonization, PreparedBsimSoiInstance, PreparedBsimSoiIntrinsicCharges,
    PreparedBsimSoiJunctions, PreparedBsimSoiNoise, PreparedBsimSoiParasitics,
    PreparedBsimSoiSetup, PreparedBsimSoiSize, PreparedBsimSoiTemperature,
    PreparedBsimSoiTransport, prepare_bsim_soi_setup,
};
pub use defaults::{MosPolarity, PreparedBsimSoiModel, prepare_bsim_soi_model};
pub use legacy_bsim::{
    BSIM1_REFERENCE_THERMAL_VOLTAGE, Bsim1Coefficients, Bsim2Coefficients, LegacyBsimBias,
    LegacyBsimChannel, LegacyBsimGeometry, LegacyBsimIntrinsicPoint, LegacyBsimJunction,
    LegacyBsimJunctionLaw, LegacyBsimOverlap, PreparedLegacyBsimInstance, PreparedLegacyBsimModel,
    evaluate_bsim1_intrinsic, evaluate_bsim2_intrinsic, evaluate_legacy_bsim_intrinsic,
    prepare_legacy_bsim_model,
};

pub use hv::{
    HiSimHvAccumulationInputs, HiSimHvAccumulationPoint, HiSimHvBiasConditioningInputs,
    HiSimHvBiasConditioningPoint, HiSimHvBinningGeometry, HiSimHvDepletionMaterialPoint,
    HiSimHvDepletionTransport, HiSimHvDrainBias, HiSimHvDrainCurrent, HiSimHvDrainSmoothingInputs,
    HiSimHvDrainSmoothingPoint, HiSimHvEdgeTunnelingCurrent, HiSimHvEdgeTunnelingCurrents,
    HiSimHvEffectiveGeometry, HiSimHvElectricalGradient, HiSimHvElectrostaticMaterialPoint,
    HiSimHvFringingCharges, HiSimHvGateChannelState, HiSimHvGateTunnelingCurrents,
    HiSimHvGeneratedHeat, HiSimHvGidlCurrents, HiSimHvIntrinsicBias, HiSimHvIntrinsicScalar,
    HiSimHvJunctionBias, HiSimHvJunctionCharge, HiSimHvJunctionCharges, HiSimHvJunctionCurrent,
    HiSimHvJunctionCurrents, HiSimHvLateralFieldCharge, HiSimHvPolyDepletionPoint,
    HiSimHvQuantumOxidePoint, HiSimHvSurfaceInputs, HiSimHvSurfacePoint, HiSimHvThermalBranch,
    HiSimHvThermalJacobianRow, HiSimHvThresholdPoint, HiSimHvTransportGeometry,
    PreparedHiSimHvEdgeTunneling, PreparedHiSimHvElectrostaticMaterial, PreparedHiSimHvFringing,
    PreparedHiSimHvGateTunneling, PreparedHiSimHvGidl, PreparedHiSimHvInstance,
    PreparedHiSimHvJunctionCharges, PreparedHiSimHvJunctionDiodes,
    PreparedHiSimHvLateralFieldCharge, PreparedHiSimHvModel, PreparedHiSimHvPolyDepletion,
    PreparedHiSimHvQuantumOxide, PreparedHiSimHvTemperature, PreparedHiSimHvThreshold,
    PreparedHiSimHvTransport, evaluate_hisim_hv_accumulation, evaluate_hisim_hv_bias_conditioning,
    evaluate_hisim_hv_drain_smoothing, evaluate_hisim_hv_generated_heat, evaluate_hisim_hv_surface,
    prepare_hisim_hv_edge_tunneling, prepare_hisim_hv_electrostatic_material,
    prepare_hisim_hv_fringing, prepare_hisim_hv_gate_tunneling, prepare_hisim_hv_gidl,
    prepare_hisim_hv_instance, prepare_hisim_hv_junction_charges, prepare_hisim_hv_junction_diodes,
    prepare_hisim_hv_lateral_field_charge, prepare_hisim_hv_model, prepare_hisim_hv_poly_depletion,
    prepare_hisim_hv_quantum_oxide, prepare_hisim_hv_temperature, prepare_hisim_hv_threshold,
    prepare_hisim_hv_transport,
};

/// Exact ngspice model implementation selected by the SPICE model level/version.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ModelVariant {
    /// `HiSIM2`, level 68.
    HiSim2,
    /// HiSIM-HV 1.1.x/1.2.x, level 73.
    HiSimHv1,
    /// HiSIM-HV 2.0.x/2.1.x/2.2.x or default, level 73.
    HiSimHv2,
    /// Berkeley BSIMSOI 4 and compatible versions, levels 10/58.
    BsimSoi,
    /// Fully depleted BSIM3SOI, level 55.
    Bsim3SoiFd,
    /// Dynamic depletion BSIM3SOI, level 56.
    Bsim3SoiDd,
    /// Partially depleted BSIM3SOI, level 57.
    Bsim3SoiPd,
    /// Southampton STAG SOI3, level 60.
    Soi3,
    /// Berkeley BSIM1, level 4.
    Bsim1,
    /// Berkeley BSIM2, level 5.
    Bsim2,
    /// BSIM3 3.0.x implementation, levels 8/49.
    Bsim3V0,
    /// BSIM3 3.1.x implementation, levels 8/49.
    Bsim3V1,
    /// BSIM3 3.2.x implementation, levels 8/49.
    Bsim3V32,
    /// BSIM3 3.3.x or default implementation, levels 8/49.
    Bsim3V33,
}

/// Source registration information for all intrinsic model implementations.
pub const VARIANTS: [ModelVariant; 14] = [
    ModelVariant::HiSim2,
    ModelVariant::HiSimHv1,
    ModelVariant::HiSimHv2,
    ModelVariant::BsimSoi,
    ModelVariant::Bsim3SoiFd,
    ModelVariant::Bsim3SoiDd,
    ModelVariant::Bsim3SoiPd,
    ModelVariant::Soi3,
    ModelVariant::Bsim1,
    ModelVariant::Bsim2,
    ModelVariant::Bsim3V0,
    ModelVariant::Bsim3V1,
    ModelVariant::Bsim3V32,
    ModelVariant::Bsim3V33,
];

/// Kernel availability within this producer crate, not in a consuming simulator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KernelStatus {
    /// Requires a separate, verified physical current/charge/derivative implementation.
    NotImplemented,
}

/// A schema lookup or binding error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModelError {
    /// No supported intrinsic family uses this level.
    UnknownLevel(i32),
    /// The specific HiSIM-HV version has no matching source-family implementation.
    UnknownHiSimHvVersion(String),
    /// The literal BSIM3 version has no matching upstream implementation.
    UnknownBsim3Version(String),
    /// The input parameter name does not appear in the upstream table.
    UnknownParameter(String),
    /// The provided typed parameter value differs from the upstream declaration.
    WrongType {
        /// Parameter name.
        name: String,
        /// Required upstream value type.
        expected: ParameterType,
    },
    /// Non-finite real value/real vector element.
    NonFinite(String),
    /// The parameter source is for the wrong family or binding scope.
    WrongModelScope,
    /// Prepared model/instance/thermal state derives from different contexts.
    MismatchedPreparedContext,
    /// Both NMOS and PMOS true flags were set; polarity is ambiguous.
    ConflictingPolarity,
    /// Nominal circuit or model-card temperature is not positive and finite.
    InvalidNominalTemperature,
    /// An upstream explicit mode selector is outside its documented domain.
    InvalidSelector {
        /// Name of the selector.
        name: String,
        /// Out-of-domain selector value.
        value: i32,
    },
    /// A source-derived default depends on a not-yet-prepared parameter slot.
    MissingDefaultDependency(u16),
    /// Physical preparation requires a valid value for this parameter.
    InvalidPhysicalParameter(&'static str),
    /// A caller supplied an ignored/unsupported version-specific instance input.
    UnsupportedInstanceParameter(&'static str),
    /// Initial condition vector length is invalid for the selected source family.
    InvalidInitialConditions(usize),
    /// A size-binned parameter became non-finite during C-source arithmetic.
    InvalidBinningParameter(u16),
    /// Embedded source-derived initialization table is invalid or corrupted.
    CorruptSourceCatalog(String),
}

impl std::fmt::Display for ModelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownLevel(level) => write!(f, "unsupported intrinsic MOS level {level}"),
            Self::UnknownHiSimHvVersion(version) => {
                write!(f, "unsupported HiSIM-HV version '{version}'")
            }
            Self::UnknownBsim3Version(version) => {
                write!(f, "unsupported BSIM3 version '{version}'")
            }
            Self::UnknownParameter(name) => write!(f, "unrecognized model parameter '{name}'"),
            Self::WrongType { name, expected } => {
                write!(f, "parameter '{name}' requires {expected:?}")
            }
            Self::NonFinite(name) => write!(f, "non-finite parameter '{name}'"),
            Self::WrongModelScope => write!(
                f,
                "model preparation requires a supported family and model parameter block"
            ),
            Self::MismatchedPreparedContext => {
                write!(f, "incompatible prepared model, instance, or thermal state")
            }
            Self::ConflictingPolarity => write!(f, "both NMOS and PMOS flags were set"),
            Self::InvalidNominalTemperature => {
                write!(f, "invalid nominal model/circuit temperature")
            }
            Self::InvalidSelector { name, value } => {
                write!(f, "unsupported selector {name}={value}")
            }
            Self::MissingDefaultDependency(slot) => write!(
                f,
                "missing source-derived default dependency for slot {slot}"
            ),
            Self::InvalidPhysicalParameter(name) => {
                write!(f, "invalid physical parameter '{name}'")
            }
            Self::UnsupportedInstanceParameter(name) => {
                write!(f, "unsupported instance input '{name}'")
            }
            Self::InvalidInitialConditions(length) => {
                write!(f, "invalid IC vector length {length}")
            }
            Self::InvalidBinningParameter(slot) => {
                write!(f, "non-finite binned model parameter slot {slot}")
            }
            Self::CorruptSourceCatalog(problem) => write!(f, "invalid source catalog: {problem}"),
        }
    }
}

impl std::error::Error for ModelError {}

/// Distinguishes shared, immutable model parameters from instance geometry/overrides.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParameterScope {
    /// Shared model-card parameters.
    Model,
    /// Per-device instance parameters.
    Instance,
}

/// ngspice `IFparm` input value categories. A token's textual parsing happens upstream.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParameterType {
    /// `IF_REAL`.
    Real,
    /// `IF_INTEGER`.
    Integer,
    /// `IF_FLAG`.
    Flag,
    /// `IF_STRING`.
    Text,
    /// `IF_REALVEC`.
    RealVector,
}

/// Source-derived parameter name, alias identity, and type.
///
/// The slot is stable within a specific variant and scope. Different spellings
/// with the same upstream parameter ID share a slot, preventing false duplicates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ParameterSpec {
    /// Lowercase name as accepted by the ngspice model/instance table.
    pub name: &'static str,
    /// Canonical numeric slot derived from the upstream C parameter identifier.
    pub slot: u16,
    /// Declared value type.
    pub kind: ParameterType,
}

/// Already parsed, typed input value from a model card or device line.
#[derive(Clone, Debug, PartialEq)]
pub enum ParameterValue {
    /// Finite real scalar.
    Real(f64),
    /// Integer selector or scalar.
    Integer(i32),
    /// Boolean selector or flag.
    Flag(bool),
    /// Literal model string (e.g. HiSIM-HV version selector).
    Text(String),
    /// Finite real vector (e.g. initial conditions).
    RealVector(Vec<f64>),
}

impl ParameterValue {
    const fn kind(&self) -> ParameterType {
        match self {
            Self::Real(_) => ParameterType::Real,
            Self::Integer(_) => ParameterType::Integer,
            Self::Flag(_) => ParameterType::Flag,
            Self::Text(_) => ParameterType::Text,
            Self::RealVector(_) => ParameterType::RealVector,
        }
    }

    fn finite(&self) -> bool {
        match self {
            Self::Real(value) => value.is_finite(),
            Self::RealVector(values) => values.iter().all(|value| value.is_finite()),
            _ => true,
        }
    }
}

/// A dense per-scope parameter block; binds and validates names only at preparation.
///
/// Missing slots are `None`, **not** zero-valued defaults; physical model
/// preparation must supply each variant's actual default/dependency rules.
#[derive(Clone, Debug, PartialEq)]
pub struct BoundParameters {
    variant: ModelVariant,
    scope: ParameterScope,
    values: Box<[Option<ParameterValue>]>,
    // B4SOIparam writes only the final component of each IC vector.
    bsim_soi_ic: [Option<f64>; 5],
}

impl BoundParameters {
    /// Bind assignments in source order. As in ngspice, later assignments to the
    /// same canonical parameter (including aliases) replace earlier ones.
    ///
    /// This is a one-time operation, not a per-Newton-step API.
    ///
    /// # Errors
    /// Returns an error for an unrecognized name, wrong value type or nonfinite real.
    pub fn bind<'a>(
        variant: ModelVariant,
        scope: ParameterScope,
        inputs: impl IntoIterator<Item = (&'a str, ParameterValue)>,
    ) -> Result<Self, ModelError> {
        let specs = variant.parameters(scope);
        let slots = specs
            .iter()
            .map(|entry| usize::from(entry.slot))
            .max()
            .map_or(0, |max| max + 1);
        let mut bound = Self {
            variant,
            scope,
            values: vec![None; slots].into_boxed_slice(),
            bsim_soi_ic: [None; 5],
        };
        for (name, value) in inputs {
            bound.assign(name, value)?;
        }
        Ok(bound)
    }

    /// Apply one more source assignment, preserving prior presence and ordered
    /// initial-condition setters. This is a preparation-time operation.
    ///
    /// # Errors
    /// Unknown parameter, wrong type, nonfinite value or invalid IC vector.
    pub fn assign(&mut self, name: &str, value: ParameterValue) -> Result<(), ModelError> {
        let entry = self
            .variant
            .parameter(self.scope, name)
            .ok_or_else(|| ModelError::UnknownParameter(name.to_owned()))?;
        if value.kind() != entry.kind {
            return Err(ModelError::WrongType {
                name: name.to_owned(),
                expected: entry.kind,
            });
        }
        if !value.finite() {
            return Err(ModelError::NonFinite(name.to_owned()));
        }
        if self.variant == ModelVariant::BsimSoi
            && self.scope == ParameterScope::Instance
            && entry.name == "ic"
            && let ParameterValue::RealVector(ref vector) = value
        {
            let length = vector.len();
            if !(1..=5).contains(&length) {
                return Err(ModelError::InvalidInitialConditions(length));
            }
            self.bsim_soi_ic[length - 1] = vector.last().copied();
        }
        self.values[usize::from(entry.slot)] = Some(value);
        Ok(())
    }

    /// Retrieve a prepared parameter by slot, without names or allocations.
    #[must_use = "the lookup result distinguishes a missing slot from a bound value"]
    pub fn get_slot(&self, slot: u16) -> Option<&ParameterValue> {
        self.values.get(usize::from(slot)).and_then(Option::as_ref)
    }

    /// The exact family implementation chosen at binding.
    #[must_use = "the resolved model family determines the numerical implementation"]
    pub const fn variant(&self) -> ModelVariant {
        self.variant
    }

    /// Model versus instance parameter storage.
    #[must_use = "the returned scope distinguishes model and instance parameters"]
    pub const fn scope(&self) -> ParameterScope {
        self.scope
    }
}

impl ModelVariant {
    /// Resolve the source-family dispatch in ngspice's `inpdomod.c`.
    ///
    /// HiSIM-HV and BSIM3 dispatch between different implementations by version;
    /// other families select by level and leave version handling to model physics.
    /// Pass the first literal version token, not the final ordered setter.
    ///
    /// # Errors
    /// Returns an error for an unknown level or unsupported HiSIM-HV/BSIM3 version.
    pub fn from_level_version(level: i32, version: Option<&str>) -> Result<Self, ModelError> {
        match level {
            4 => Ok(Self::Bsim1),
            5 => Ok(Self::Bsim2),
            8 | 49 => match version {
                None | Some("default") => Ok(Self::Bsim3V33),
                Some(ver) if ver.starts_with("3.0") => Ok(Self::Bsim3V0),
                Some(ver) if ver.starts_with("3.1") => Ok(Self::Bsim3V1),
                Some(ver) if ver.starts_with("3.2") => Ok(Self::Bsim3V32),
                Some(ver) if ver.starts_with("3.3") => Ok(Self::Bsim3V33),
                Some(ver) => Err(ModelError::UnknownBsim3Version(ver.to_owned())),
            },
            68 => Ok(Self::HiSim2),
            73 => match version {
                None => Ok(Self::HiSimHv2),
                Some(ver) if ["1.1", "1.2"].iter().any(|prefix| ver.starts_with(prefix)) => {
                    Ok(Self::HiSimHv1)
                }
                Some(ver)
                    if ["2.0", "2.1", "2.2"]
                        .iter()
                        .any(|prefix| ver.starts_with(prefix)) =>
                {
                    Ok(Self::HiSimHv2)
                }
                Some(ver) => Err(ModelError::UnknownHiSimHvVersion(ver.to_owned())),
            },
            10 | 58 => Ok(Self::BsimSoi),
            55 => Ok(Self::Bsim3SoiFd),
            56 => Ok(Self::Bsim3SoiDd),
            57 => Ok(Self::Bsim3SoiPd),
            60 => Ok(Self::Soi3),
            _ => Err(ModelError::UnknownLevel(level)),
        }
    }

    /// ngspice registration-order terminal labels; optional positions are preserved,
    /// including BSIMSOI's intentionally empty fifth terminal slot.
    #[must_use = "the terminal order determines the device connection layout"]
    pub const fn terminals(self) -> &'static [&'static str] {
        const MOS: &[&str] = &["Drain", "Gate", "Source", "Bulk"];
        const HV: &[&str] = &["Drain", "Gate", "Source", "Bulk", "Substrate", "Temp"];
        const SOI: &[&str] = &[
            "Drain", "Gate", "Source", "Backgate", "", "Body", "Temp", "Charge",
        ];
        const STAG: &[&str] = &[
            "Drain",
            "Front Gate",
            "Source",
            "Back Gate",
            "Bulk",
            "Thermal",
        ];
        match self {
            Self::HiSim2
            | Self::Bsim1
            | Self::Bsim2
            | Self::Bsim3V0
            | Self::Bsim3V1
            | Self::Bsim3V32
            | Self::Bsim3V33 => MOS,
            Self::HiSimHv1 | Self::HiSimHv2 => HV,
            Self::BsimSoi | Self::Bsim3SoiFd | Self::Bsim3SoiDd | Self::Bsim3SoiPd => SOI,
            Self::Soi3 => STAG,
        }
    }

    /// Schema recognition does not imply a physical numerical implementation.
    pub const KERNEL_STATUS: KernelStatus = KernelStatus::NotImplemented;

    /// Entire name-sorted source-derived model or instance parameter table.
    #[must_use = "the returned catalog defines valid source parameters"]
    pub fn parameters(self, scope: ParameterScope) -> &'static [ParameterSpec] {
        catalog::table(self, scope)
    }

    /// Lookup only during model/instance preparation, never during numerical evaluation.
    #[must_use = "the lookup result distinguishes an unknown parameter from a registered one"]
    pub fn parameter(self, scope: ParameterScope, name: &str) -> Option<&'static ParameterSpec> {
        let lower = name.to_ascii_lowercase();
        let table = self.parameters(scope);
        table
            .binary_search_by_key(&lower.as_str(), |entry| entry.name)
            .ok()
            .map(|index| &table[index])
    }
}
