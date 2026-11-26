//! Source-derived `HiSIM2` 2.80 setup from ngspice 47.
//! Copyright (C) 2014 Hiroshima University & STARC; see package LICENSE.
//! Setup parameters retain card units. This is not a nonlinear kernel.

mod physical;
mod rules;

pub use physical::{
    BodyConductance, EffectiveGeometry, PreparedPhysical, SeriesResistance, prepare_physical,
};
use std::sync::Arc;

use crate::{
    BoundParameters, ModelError, ModelVariant, MosPolarity, ParameterScope, ParameterValue,
};

const FAMILY: ModelVariant = ModelVariant::HiSim2;

fn slot(scope: ParameterScope, name: &str) -> Result<u16, ModelError> {
    FAMILY
        .parameter(scope, name)
        .map(|spec| spec.slot)
        .ok_or_else(|| ModelError::UnknownParameter(name.to_owned()))
}

fn value<'a>(p: &'a BoundParameters, name: &str) -> Result<&'a ParameterValue, ModelError> {
    let index = slot(p.scope(), name)?;
    p.get_slot(index)
        .ok_or(ModelError::MissingDefaultDependency(index))
}

fn real(p: &BoundParameters, name: &str) -> Result<f64, ModelError> {
    match value(p, name)? {
        ParameterValue::Real(v) => Ok(*v),
        _ => Err(ModelError::MissingDefaultDependency(slot(p.scope(), name)?)),
    }
}

fn integer(p: &BoundParameters, name: &str) -> Result<i32, ModelError> {
    match value(p, name)? {
        ParameterValue::Integer(v) => Ok(*v),
        _ => Err(ModelError::MissingDefaultDependency(slot(p.scope(), name)?)),
    }
}

fn flag(p: &BoundParameters, name: &str) -> Result<bool, ModelError> {
    Ok(matches!(
        p.get_slot(slot(p.scope(), name)?),
        Some(ParameterValue::Flag(true))
    ))
}

fn assign(
    p: &mut BoundParameters,
    name: &str,
    value: ParameterValue,
    force: bool,
) -> Result<(), ModelError> {
    let spec = FAMILY
        .parameter(p.scope(), name)
        .ok_or_else(|| ModelError::UnknownParameter(name.to_owned()))?;
    if spec.kind != value.kind() {
        return Err(ModelError::WrongType {
            name: name.to_owned(),
            expected: spec.kind,
        });
    }
    if !value.finite() {
        return Err(ModelError::NonFinite(name.to_owned()));
    }
    let dst = &mut p.values[usize::from(spec.slot)];
    if force || dst.is_none() {
        *dst = Some(value);
    }
    Ok(())
}

fn integer_default(p: &mut BoundParameters, name: &str, v: i32) -> Result<(), ModelError> {
    assign(p, name, ParameterValue::Integer(v), false)
}

fn real_defaults(p: &mut BoundParameters, defaults: &[(&str, f64)]) -> Result<(), ModelError> {
    for &(name, v) in defaults {
        assign(p, name, ParameterValue::Real(v), false)?;
    }
    Ok(())
}

fn selector(p: &BoundParameters, name: &str, permitted: &[i32]) -> Result<i32, ModelError> {
    let v = integer(p, name)?;
    if permitted.contains(&v) {
        Ok(v)
    } else {
        Err(ModelError::InvalidSelector {
            name: name.to_owned(),
            value: v,
        })
    }
}

fn given_bits(p: &BoundParameters) -> Box<[bool]> {
    p.values.iter().map(Option::is_some).collect()
}

/// `HiSIM2` setup parameters with original source-given bits.
#[derive(Clone, Debug, PartialEq)]
pub struct PreparedModel {
    parameters: BoundParameters,
    polarity: MosPolarity,
    given: Box<[bool]>,
    identity: Arc<()>,
}

impl PreparedModel {
    /// Resolved channel polarity.
    #[must_use = "polarity selects the physical equations"]
    pub const fn polarity(&self) -> MosPolarity {
        self.polarity
    }

    /// Source-unit model parameters after setup.
    #[must_use = "the buffer supplies prepared model parameters"]
    pub const fn parameters(&self) -> &BoundParameters {
        &self.parameters
    }

    /// Whether a model field was explicitly provided.
    #[must_use = "source-given bits affect physical evaluation"]
    pub fn explicitly_given(&self, slot: u16) -> bool {
        self.given.get(usize::from(slot)).copied().unwrap_or(false)
    }
}

fn clamp(p: &mut BoundParameters, name: &str, min: f64, max: f64) -> Result<(), ModelError> {
    let v = real(p, name)?.clamp(min, max);
    assign(p, name, ParameterValue::Real(v), true)
}

fn mode_defaults(p: &mut BoundParameters, polarity: MosPolarity) -> Result<(), ModelError> {
    let depletion = selector(p, "codep", &[0, 1])? == 1;
    let ddlt = selector(p, "coddlt", &[0, 1])? == 1;
    selector(p, "corbnet", &[0, 1])?;
    let nmos = polarity == MosPolarity::Nmos;
    real_defaults(
        p,
        &[
            ("bb", if nmos { 2.0 } else { 1.0 }),
            ("mueph1", if nmos { 25e3 } else { 9e3 }),
            ("vgsmin", if nmos { -5.0 } else { 5.0 }),
            ("ddltslp", if ddlt { 10.0 } else { 0.0 }),
            ("ddltict", if ddlt { 0.0 } else { 10.0 }),
            ("vfbc", if depletion { 0.2 } else { -1.0 }),
            ("nsubc", if depletion { 5e16 } else { 5e17 }),
            ("lp", if depletion { 0.0 } else { 15e-9 }),
            ("nsubp", if depletion { 1e17 } else { 1e18 }),
            ("muesr1", if depletion { 5e15 } else { 1e15 }),
        ],
    )?;
    integer_default(p, "corecip", i32::from(!depletion))?;
    integer_default(p, "copprv", i32::from(!depletion))?;
    if depletion {
        for name in ["corecip", "copprv", "coisti", "cothrml", "coign"] {
            assign(p, name, ParameterValue::Integer(0), true)?;
        }
        if integer(p, "corsrd")? == 1 {
            assign(p, "corsrd", ParameterValue::Integer(-1), true)?;
        }
        clamp(p, "ndepm", 5e15, 2e17)?;
        clamp(p, "tndep", 1e-7, 1e-6)?;
    }
    if integer(p, "corecip")? != 0 {
        for name in [
            "sc2", "lsc2", "wsc2", "psc2", "scp2", "lscp2", "wscp2", "pscp2", "sc4", "lsc4",
            "wsc4", "psc4",
        ] {
            assign(p, name, ParameterValue::Real(0.0), true)?;
        }
        assign(p, "coqy", ParameterValue::Integer(0), true)?;
    }
    Ok(())
}

/// Apply `HiSIM2` 2.80 setup defaults and mode-dependent overrides.
///
/// TNOM remains Celsius. Unsupported explicit versions and checked invalid
/// selectors are rejected instead of silently changing physics. Source range
/// resets and supported mode adjustments are preserved.
///
/// # Errors
/// Wrong family/scope, conflicting polarity, invalid selector or temperature,
/// malformed source rules, or non-finite parameters.
pub fn prepare_model(source: &BoundParameters) -> Result<PreparedModel, ModelError> {
    if source.variant() != FAMILY || source.scope() != ParameterScope::Model {
        return Err(ModelError::WrongModelScope);
    }
    let nmos = flag(source, "nmos")?;
    let pmos = flag(source, "pmos")?;
    if nmos && pmos {
        return Err(ModelError::ConflictingPolarity);
    }
    let polarity = if pmos {
        MosPolarity::Pmos
    } else {
        MosPolarity::Nmos
    };
    let mut parameters = source.clone();
    integer_default(&mut parameters, "level", 68)?;
    selector(&parameters, "level", &[68])?;
    integer_default(&mut parameters, "version", 280)?;
    selector(&parameters, "version", &[280])?;
    integer_default(&mut parameters, "show", 0)?;
    assign(&mut parameters, "noise", ParameterValue::Integer(1), true)?;
    for &(name, v) in rules::INTEGER_DEFAULTS {
        integer_default(&mut parameters, name, v)?;
    }
    real_defaults(&mut parameters, rules::REAL_DEFAULTS)?;
    mode_defaults(&mut parameters, polarity)?;
    clamp(&mut parameters, "nsubpfac", 0.2, 1.0)?;
    clamp(&mut parameters, "nsubpdlt", 1e-50, 0.1)?;
    if real(&parameters, "tnom")? <= -273.15 {
        return Err(ModelError::InvalidNominalTemperature);
    }
    Ok(PreparedModel {
        parameters,
        polarity,
        given: given_bits(source),
        identity: Arc::new(()),
    })
}

/// Gate geometry used by the `HiSIM2` binning equations.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BinningGeometry {
    /// Drawn length plus model XL, in meters.
    pub length_m: f64,
    /// Drawn width divided by finger count plus XW, in meters.
    pub width_per_finger_m: f64,
    /// Length in micrometers raised to LBINN.
    pub length_bin: f64,
    /// Width per finger in micrometers raised to WBINN.
    pub width_bin: f64,
}

/// Prime-node connectivity after source setup.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrimeNode {
    /// Alias the external terminal.
    External,
    /// Allocate an independent internal node.
    Internal,
}

impl PrimeNode {
    const fn from_internal(required: bool) -> Self {
        if required {
            Self::Internal
        } else {
            Self::External
        }
    }
}

/// Body-network connectivity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BodyNetwork {
    /// All body nodes alias the external bulk terminal.
    Collapsed,
    /// Allocate bulk-prime, drain-body and source-body nodes.
    Distributed,
}

/// Charge dynamics selected by CONQS.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChargeDynamics {
    /// Quasistatic device with 18 source history slots.
    Quasistatic,
    /// Non-quasistatic device with 21 source history slots.
    NonQuasistatic,
}

/// Topology requirements from the original setup routine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Topology {
    /// Drain-prime connectivity.
    pub drain_prime: PrimeNode,
    /// Source-prime connectivity.
    pub source_prime: PrimeNode,
    /// Gate-prime connectivity.
    pub gate_prime: PrimeNode,
    /// Body-network connectivity.
    pub body_network: BodyNetwork,
    /// Charge dynamics.
    pub charge_dynamics: ChargeDynamics,
}

/// `HiSIM2` instance preparation in source units, before SI conversion.
#[derive(Clone, Debug, PartialEq)]
pub struct PreparedInstance {
    parameters: BoundParameters,
    given: Box<[bool]>,
    geometry: BinningGeometry,
    binned: Box<[Option<f64>]>,
    topology: Topology,
    initial_conditions: [f64; 3],
    temperature_kelvin: f64,
    model_identity: Arc<()>,
}

impl PreparedInstance {
    /// Resolved instance parameters in source units.
    #[must_use = "the buffer supplies prepared instance parameters"]
    pub const fn parameters(&self) -> &BoundParameters {
        &self.parameters
    }

    /// Whether an instance field was explicitly provided.
    #[must_use = "source-given bits affect physical evaluation"]
    pub fn explicitly_given(&self, slot: u16) -> bool {
        self.given.get(usize::from(slot)).copied().unwrap_or(false)
    }

    /// Gate geometry after offsets and finger division.
    #[must_use = "geometry is required for physical preparation"]
    pub const fn geometry(&self) -> BinningGeometry {
        self.geometry
    }

    /// Binned model coefficient in source units, including VFBC length correction.
    #[must_use = "binned coefficients are not yet converted to internal SI units"]
    pub fn binned_model_slot(&self, slot: u16) -> Option<f64> {
        self.binned.get(usize::from(slot)).copied().flatten()
    }

    /// Electrical topology requirements.
    #[must_use = "topology determines the internal nodes"]
    pub const fn topology(&self) -> Topology {
        self.topology
    }

    /// Initial voltages in external VDS, VGS, VBS order.
    #[must_use = "initial voltages are required at startup"]
    pub const fn initial_conditions(&self) -> [f64; 3] {
        self.initial_conditions
    }

    /// Effective circuit/instance temperature in Kelvin.
    #[must_use = "temperature selects temperature-dependent physical laws"]
    pub const fn temperature_kelvin(&self) -> f64 {
        self.temperature_kelvin
    }
}

fn instance_defaults(
    model: &PreparedModel,
    source: &BoundParameters,
) -> Result<BoundParameters, ModelError> {
    let mut p = source.clone();
    real_defaults(
        &mut p,
        &[
            ("l", 5e-6),
            ("w", 5e-6),
            ("ad", 0.0),
            ("as", 0.0),
            ("pd", 0.0),
            ("ps", 0.0),
            ("nrd", 0.0),
            ("nrs", 0.0),
            ("ngcon", 1.0),
            ("xgw", 0.0),
            ("xgl", 0.0),
            ("nf", 1.0),
            ("sa", 0.0),
            ("sb", 0.0),
            ("sd", 0.0),
            ("temp", 27.0),
            ("dtemp", 0.0),
            ("m", 1.0),
            ("nsubcdfm", 0.0),
            ("sca", 0.0),
            ("scb", 0.0),
            ("scc", 0.0),
        ],
    )?;
    for name in [
        "corbnet", "corg", "rbdb", "rbsb", "rbpb", "rbps", "rbpd", "mphdfm",
    ] {
        assign(
            &mut p,
            name,
            value(model.parameters(), name)?.clone(),
            false,
        )?;
    }
    assign(&mut p, "off", ParameterValue::Flag(false), false)?;
    selector(&p, "corbnet", &[0, 1])?;
    selector(&p, "corg", &[0, 1])?;
    Ok(p)
}

fn initial_conditions(p: &BoundParameters) -> Result<[f64; 3], ModelError> {
    let mut ic = [0.0; 3];
    if let Some(ParameterValue::RealVector(values)) =
        p.get_slot(slot(ParameterScope::Instance, "ic")?)
    {
        if !(1..=3).contains(&values.len()) {
            return Err(ModelError::InvalidInitialConditions(values.len()));
        }
        ic[..values.len()].copy_from_slice(values);
    }
    Ok(ic)
}

fn positive(v: f64, name: &'static str) -> Result<f64, ModelError> {
    if v.is_finite() && v > 0.0 {
        Ok(v)
    } else {
        Err(ModelError::InvalidPhysicalParameter(name))
    }
}

fn geometry(
    model: &BoundParameters,
    instance: &BoundParameters,
) -> Result<BinningGeometry, ModelError> {
    let l = positive(real(instance, "l")?, "l")?;
    let w = positive(real(instance, "w")?, "w")?;
    let nf = positive(real(instance, "nf")?, "nf")?;
    positive(real(instance, "m")?, "m")?;
    let length_m = positive(l + real(model, "xl")?, "lgate")?;
    let width_per_finger_m = positive(w / nf + real(model, "xw")?, "wgate")?;
    let length_bin = positive((length_m * 1e6).powf(real(model, "lbinn")?), "lbin")?;
    let width_bin = positive(
        (width_per_finger_m * 1e6).powf(real(model, "wbinn")?),
        "wbin",
    )?;
    positive(length_bin * width_bin, "lwbin")?;
    Ok(BinningGeometry {
        length_m,
        width_per_finger_m,
        length_bin,
        width_bin,
    })
}

fn binning(model: &BoundParameters, g: BinningGeometry) -> Result<Box<[Option<f64>]>, ModelError> {
    let mut values = vec![None; model.values.len()].into_boxed_slice();
    for &name in rules::BINNED {
        let index = slot(ParameterScope::Model, name)?;
        let mut v = real(model, name)?
            + real(model, &format!("l{name}"))? / g.length_bin
            + real(model, &format!("w{name}"))? / g.width_bin
            + real(model, &format!("p{name}"))? / (g.length_bin * g.width_bin);
        if name == "vfbc" {
            v *= 1.0 + real(model, "vfbcl")? / (g.length_m * 1e6).powf(real(model, "vfbclp")?);
        }
        if !v.is_finite() {
            return Err(ModelError::InvalidBinningParameter(index));
        }
        values[usize::from(index)] = Some(v);
    }
    Ok(values)
}

fn topology(model: &BoundParameters, instance: &BoundParameters) -> Result<Topology, ModelError> {
    let external_resistance = integer(model, "corsrd")? < 0;
    let sheet = real(model, "rsh")? > 0.0;
    Ok(Topology {
        drain_prime: PrimeNode::from_internal(
            external_resistance && (sheet || real(model, "rd")? > 0.0),
        ),
        source_prime: PrimeNode::from_internal(
            external_resistance && (sheet || real(model, "rs")? > 0.0),
        ),
        gate_prime: PrimeNode::from_internal(
            integer(instance, "corg")? == 1 && real(model, "rshg")? > 0.0,
        ),
        body_network: if integer(instance, "corbnet")? == 1 {
            BodyNetwork::Distributed
        } else {
            BodyNetwork::Collapsed
        },
        charge_dynamics: if integer(model, "conqs")? != 0 {
            ChargeDynamics::NonQuasistatic
        } else {
            ChargeDynamics::Quasistatic
        },
    })
}

/// Prepare `HiSIM2` instance defaults, geometry, binning and topology.
///
/// The caller applies frontend geometry scale. Circuit temperature is Kelvin;
/// card TEMP and DTEMP use Celsius and a temperature difference respectively.
/// The source IC vector is retained separately from resolved startup values.
///
/// # Errors
/// Wrong scope/family, invalid selectors, malformed IC, nonpositive
/// geometry/temperature, or non-finite binning.
pub fn prepare_instance(
    model: &PreparedModel,
    source: &BoundParameters,
    circuit_temperature_kelvin: f64,
) -> Result<PreparedInstance, ModelError> {
    if source.variant() != FAMILY || source.scope() != ParameterScope::Instance {
        return Err(ModelError::WrongModelScope);
    }
    positive(circuit_temperature_kelvin, "circuit temperature")?;
    let parameters = instance_defaults(model, source)?;
    let geometry = geometry(model.parameters(), &parameters)?;
    let binned = binning(model.parameters(), geometry)?;
    let topology = topology(model.parameters(), &parameters)?;
    let initial_conditions = initial_conditions(&parameters)?;
    let base = if source
        .get_slot(slot(ParameterScope::Instance, "temp")?)
        .is_some()
    {
        real(&parameters, "temp")? + 273.15
    } else {
        circuit_temperature_kelvin
    };
    let temperature_kelvin = positive(base + real(&parameters, "dtemp")?, "device temperature")?;
    Ok(PreparedInstance {
        parameters,
        given: given_bits(source),
        geometry,
        binned,
        topology,
        initial_conditions,
        temperature_kelvin,
        model_identity: Arc::clone(&model.identity),
    })
}
