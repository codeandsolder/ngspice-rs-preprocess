//! Ordered BSIMSOI 4.4 instance setup (ngspice B4SOIparam/B4SOIsetup).
//! This is source topology and parameter preparation, not a physical kernel.

mod binning;
mod channel;
mod electrostatics;
mod geometry;
mod junction;
mod stress;
mod temperature;
mod transport;

pub use channel::{
    BsimSoiChannelBias, BsimSoiChannelPoint, BsimSoiChannelScalar, BsimSoiDrainChannelPoint,
    BsimSoiExtrinsicChargeBias, BsimSoiExtrinsicCharges, BsimSoiGateBias, BsimSoiGateCurrents,
    BsimSoiGateInducedLeakage, BsimSoiHeatPoint, BsimSoiImpactBias, BsimSoiImpactBranch,
    BsimSoiImpactIonization, BsimSoiIntrinsicCharges, BsimSoiParasiticPoint,
    PreparedBsimSoiChannel, PreparedBsimSoiDrainChannel, PreparedBsimSoiExtrinsicCharges,
    PreparedBsimSoiGateInducedLeakage, PreparedBsimSoiGateTunneling,
    PreparedBsimSoiImpactIonization, PreparedBsimSoiIntrinsicCharges, PreparedBsimSoiParasitics,
};
pub use electrostatics::{
    BsimSoiElectrostaticBase, BsimSoiElectrostaticPoint, PreparedBsimSoiElectrostatics,
};
pub use geometry::BsimSoiGeometry;
pub use junction::{
    BsimSoiCollectorCurrent, BsimSoiJunctionCharge, BsimSoiJunctionCharges, BsimSoiJunctionCurrent,
    BsimSoiJunctionDensities, BsimSoiJunctionPoint, BsimSoiJunctionSide, BsimSoiJunctionStorage,
    BsimSoiNeutralBody, PreparedBsimSoiJunctions,
};
pub use temperature::{
    BsimSoiTemperaturePoint, BsimSoiThermalNetwork, BsimSoiThermalScalar,
    PreparedBsimSoiTemperature,
};

pub use stress::BsimSoiStressOffsets;
pub use transport::{
    BsimSoiExternalSeries, BsimSoiInternalResistance, BsimSoiSeriesBias, BsimSoiSeriesConductance,
    BsimSoiSeriesParameters, BsimSoiTransportPoint, PreparedBsimSoiTransport,
};

use crate::{
    BoundParameters, ModelError, ModelVariant, ParameterScope, ParameterValue, PreparedBsimSoiModel,
};

/// External positional terminal or source-defined internal node.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BsimSoiNode {
    /// Ground or a disabled optional branch.
    Ground,
    /// Zero-based supplied terminal position.
    External(u8),
    /// Drain prime.
    DrainPrime,
    /// Source prime.
    SourcePrime,
    /// Internal body.
    Body,
    /// Internal gate.
    Gate,
    /// Middle gate.
    GateMid,
    /// Drain-side body.
    DrainBody,
    /// Source-side body.
    SourceBody,
    /// Internally solved thermal node.
    Temperature,
}

/// Source body-contact mode, independent of depletion mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BsimSoiBodyMode {
    /// Ideal fully depleted or floating body.
    NoContact,
    /// Explicit or internally resolved resistive body contact.
    Resistive,
    /// Ideal short between body and contact.
    Ideal,
}

/// `B4SOIsetup` node aliases and hidden unknowns before numerical stamping.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BsimSoiTopology {
    pub drain_prime: BsimSoiNode,
    pub source_prime: BsimSoiNode,
    pub gate: BsimSoiNode,
    pub gate_mid: BsimSoiNode,
    pub body: BsimSoiNode,
    pub contact: BsimSoiNode,
    pub drain_body: BsimSoiNode,
    pub source_body: BsimSoiNode,
    pub temperature: BsimSoiNode,
    pub body_mode: BsimSoiBodyMode,
    pub floating_body: bool,
}

/// Input in native setup traversal order (not necessarily netlist text order).
#[derive(Clone, Copy, Debug)]
pub struct BsimSoiInstanceInput<'a> {
    /// Source-bound instance assignments.
    pub parameters: &'a BoundParameters,
    /// Number of supplied terminals, including the four required terminals.
    pub terminal_count: u8,
    /// Frontend geometric scale applied only to explicitly supplied L/W/AD/AS/PD/PS.
    pub geometry_scale: f64,
}

/// Fully resolved instance inputs and source topology.
#[derive(Clone, Debug, PartialEq)]
pub struct PreparedBsimSoiInstance {
    parameters: BoundParameters,
    topology: BsimSoiTopology,
    /// Each explicit IC component, preserving `B4SOIparam`'s non-cumulative setter.
    initial_conditions: [Option<f64>; 5],
    /// Automatic depletion-mode selection intermediate.
    vbs0t: f64,
    original_given: Box<[bool]>,
}

impl PreparedBsimSoiInstance {
    #[must_use = "prepared instance parameters are needed by size and thermal preparation"]
    pub const fn parameters(&self) -> &BoundParameters {
        &self.parameters
    }
    #[must_use = "the topology determines terminal bindings and internal unknowns"]
    pub const fn topology(&self) -> BsimSoiTopology {
        self.topology
    }
    /// Whether a source slot was explicitly assigned, before defaulting.
    #[must_use = "source presence controls later initialization and temperature laws"]
    pub fn was_given(&self, slot: u16) -> bool {
        self.original_given
            .get(usize::from(slot))
            .copied()
            .unwrap_or(false)
    }
    /// Explicit DS, GS, BS, ES and PS initial conditions; absent slots use
    /// native initialization from circuit voltages, not an imposed zero.
    #[must_use = "the IC presence bits determine source initialization"]
    pub const fn initial_conditions(&self) -> [Option<f64>; 5] {
        self.initial_conditions
    }
    #[must_use = "the automatic depletion selector uses this source intermediate"]
    pub const fn vbs0t(&self) -> f64 {
        self.vbs0t
    }
}

/// Raw size-dependent parameters before temperature, mobility, and other
/// `B4SOItemp` physical corrections. No nonlinear model is claimed.
#[derive(Clone, Debug, PartialEq)]
pub struct PreparedBsimSoiSize {
    geometry: BsimSoiGeometry,
    binned: Box<[Option<f64>]>,
}

impl PreparedBsimSoiSize {
    #[must_use = "effective dimensions are required by physical evaluation"]
    pub const fn geometry(&self) -> BsimSoiGeometry {
        self.geometry
    }

    #[must_use = "the slot distinguishes binned parameters from model-wide inputs"]
    pub fn binned_model_slot(&self, slot: u16) -> Option<f64> {
        self.binned.get(usize::from(slot)).copied().flatten()
    }
}

fn bin_sizes(
    model: &BoundParameters,
    geometry: BsimSoiGeometry,
) -> Result<PreparedBsimSoiSize, ModelError> {
    let [length, width, cross] = geometry.inverse_bins;
    let mut binned = vec![None; model.values.len()].into_boxed_slice();
    for [base, l, w, p] in binning::RULES {
        let get = |slot| match model.get_slot(slot) {
            Some(ParameterValue::Real(value)) => Ok(*value),
            // C model allocation is zero-initialized. Only these four
            // bases lack setup defaults; B4SOItemp derives/overrides
            // them later from their original Given flags.
            None if matches!(slot, 131 | 135 | 694 | 695) => Ok(0.),
            _ => Err(ModelError::MissingDefaultDependency(slot)),
        };
        let value = get(p)?.mul_add(
            cross,
            get(w)?.mul_add(width, get(l)?.mul_add(length, get(base)?)),
        );
        if !value.is_finite() {
            return Err(ModelError::InvalidBinningParameter(base));
        }
        binned[usize::from(base)] = Some(value);
    }
    Ok(PreparedBsimSoiSize { geometry, binned })
}

/// One ordered native model setup, including model-wide source corrections.
///
/// Keeping the resolved model together with its instances prevents a body
/// contact in one instance from silently changing an unrelated model.
#[derive(Clone, Debug, PartialEq)]
pub struct PreparedBsimSoiSetup {
    model: PreparedBsimSoiModel,
    instances: Vec<PreparedBsimSoiInstance>,
    sizes: Vec<PreparedBsimSoiSize>,
}

impl PreparedBsimSoiSetup {
    #[must_use = "the final shared model supplies downstream temperature and size laws"]
    pub const fn model(&self) -> &PreparedBsimSoiModel {
        &self.model
    }
    /// Size records use the final model after ordered `B4SOIsetup` mutations.
    #[must_use = "size records supply later temperature and physical preparation"]
    pub fn sizes(&self) -> &[PreparedBsimSoiSize] {
        &self.sizes
    }
    #[must_use = "resolved instances preserve native setup traversal order"]
    pub fn instances(&self) -> &[PreparedBsimSoiInstance] {
        &self.instances
    }
}

// The native temperature cache compares these five fields in traversal order.
fn native_size_key(instance: &PreparedBsimSoiInstance) -> Result<[u64; 5], ModelError> {
    let mut key = [0; 5];
    for (slot, name) in key.iter_mut().zip(["l", "w", "rth0", "cth0", "nf"]) {
        let value = real(instance.parameters(), name)?;
        *slot = if value == 0. { 0 } else { value.to_bits() };
    }
    Ok(key)
}

fn binned(size: &PreparedBsimSoiSize, name: &str) -> Result<f64, ModelError> {
    let id = slot(ParameterScope::Model, name)?;
    size.binned_model_slot(id)
        .ok_or(ModelError::MissingDefaultDependency(id))
}

fn slot(scope: ParameterScope, name: &str) -> Result<u16, ModelError> {
    ModelVariant::BsimSoi
        .parameter(scope, name)
        .map(|spec| spec.slot)
        .ok_or_else(|| ModelError::UnknownParameter(name.to_owned()))
}

fn get<'a>(params: &'a BoundParameters, name: &str) -> Result<&'a ParameterValue, ModelError> {
    let id = slot(params.scope(), name)?;
    params
        .get_slot(id)
        .ok_or(ModelError::MissingDefaultDependency(id))
}

fn real(params: &BoundParameters, name: &str) -> Result<f64, ModelError> {
    match get(params, name)? {
        ParameterValue::Real(value) => Ok(*value),
        _ => Err(ModelError::InvalidPhysicalParameter(
            "BSIMSOI real parameter",
        )),
    }
}

fn integer(params: &BoundParameters, name: &str) -> Result<i32, ModelError> {
    match get(params, name)? {
        ParameterValue::Integer(value) => Ok(*value),
        _ => Err(ModelError::InvalidPhysicalParameter(
            "BSIMSOI integer parameter",
        )),
    }
}

fn put(params: &mut BoundParameters, name: &str, value: ParameterValue) -> Result<(), ModelError> {
    let id = slot(params.scope(), name)?;
    params.values[usize::from(id)] = Some(value);
    Ok(())
}

fn fill(params: &mut BoundParameters, name: &str, value: ParameterValue) -> Result<(), ModelError> {
    let id = slot(params.scope(), name)?;
    if params.values[usize::from(id)].is_none() {
        params.values[usize::from(id)] = Some(value);
    }
    Ok(())
}

fn selector(params: &BoundParameters, name: &str, max: i32) -> Result<i32, ModelError> {
    let value = integer(params, name)?;
    if (0..=max).contains(&value) {
        Ok(value)
    } else {
        Err(ModelError::InvalidSelector {
            name: name.to_owned(),
            value,
        })
    }
}

fn positive(value: f64, name: &'static str) -> Result<f64, ModelError> {
    if value.is_finite() && value > 0.0 {
        Ok(value)
    } else {
        Err(ModelError::InvalidPhysicalParameter(name))
    }
}

fn fill_instance(
    model: &BoundParameters,
    raw: &BoundParameters,
    scale: f64,
) -> Result<BoundParameters, ModelError> {
    positive(scale, "geometry scale")?;
    if raw.variant() != ModelVariant::BsimSoi || raw.scope() != ParameterScope::Instance {
        return Err(ModelError::WrongModelScope);
    }
    let mut params = raw.clone();
    for (name, factor) in [
        ("l", scale),
        ("w", scale),
        ("pd", scale),
        ("ps", scale),
        ("ad", scale * scale),
        ("as", scale * scale),
    ] {
        let id = slot(ParameterScope::Instance, name)?;
        if let Some(ParameterValue::Real(value)) = params.values[usize::from(id)].as_mut() {
            *value *= factor;
            if !value.is_finite() {
                return Err(ModelError::NonFinite(name.to_owned()));
            }
        }
    }
    for (name, value) in [
        ("ad", 0.),
        ("as", 0.),
        ("pd", 0.),
        ("ps", 0.),
        ("nrd", 1.),
        ("nrs", 1.),
        ("nrb", 1.),
        ("l", 5e-6),
        ("w", 5e-6),
        ("m", 1.),
        ("nbc", 0.),
        ("nseg", 1.),
        ("pdbcp", 0.),
        ("psbcp", 0.),
        ("agbcp", 0.),
        ("agbcp2", 0.),
        ("aebcp", 0.),
        ("sa", 0.),
        ("sb", 0.),
        ("sd", 0.),
        ("nf", 1.),
        ("delvto", 0.),
        ("vbsusr", 0.),
    ] {
        fill(&mut params, name, ParameterValue::Real(value))?;
    }
    for name in [
        "rth0", "cth0", "frbody", "soimod", "rgatemod", "rbodymod", "rbdb", "rbsb",
    ] {
        fill(&mut params, name, get(model, name)?.clone())?;
    }
    let gate_area = real(&params, "agbcp")?;
    fill(&mut params, "agbcpd", ParameterValue::Real(gate_area))?;
    for name in ["bjtoff", "debug"] {
        fill(&mut params, name, ParameterValue::Integer(0))?;
    }
    for name in ["off", "tnodeout"] {
        fill(&mut params, name, ParameterValue::Flag(false))?;
    }
    for name in ["l", "w", "m", "nf", "nseg"] {
        positive(real(&params, name)?, name)?;
    }
    for name in ["rth0", "cth0"] {
        if real(&params, name)? < 0.0 {
            return Err(ModelError::InvalidPhysicalParameter(name));
        }
    }
    Ok(params)
}

// B4SOIsetup constants, not revised SI values: changing these changes the
// native automatic depletion boundary.
const CHARGE: f64 = 1.60219e-19;
const K_OVER_Q: f64 = 8.617_087e-5;
const EPS_0: f64 = 8.85418e-12;
const EPS_OX: f64 = 3.453_133e-11;
const EPS_SI: f64 = 1.03594e-10;

fn film(model: &BoundParameters) -> Result<f64, ModelError> {
    let name = if integer(model, "mtrlmod")? == 0 {
        "tsi"
    } else {
        "etsi"
    };
    positive(real(model, name)?, name)
}

fn clamp_ideal_fd_doping(model: &mut BoundParameters) -> Result<(), ModelError> {
    if integer(model, "soimod")? != 2 {
        return Ok(());
    }
    let thickness = film(model)?;
    let (bandgap, epsilon) = if integer(model, "mtrlmod")? == 0 {
        (1.12, EPS_SI)
    } else {
        (real(model, "bg0sub")?, EPS_0 * real(model, "epsrsub")?)
    };
    let maximum = positive(
        (bandgap - 0.1) / CHARGE * 2e-6 * epsilon / (thickness * thickness),
        "ideal FD doping limit",
    )?;
    let doping = real(model, "nch")?;
    if doping > maximum {
        put(model, "nch", ParameterValue::Real(maximum))?;
    }
    Ok(())
}

fn depletion_mode(
    model: &BoundParameters,
    params: &mut BoundParameters,
) -> Result<(i32, f64), ModelError> {
    let thickness = film(model)?;
    let charge = CHARGE
        * real(model, "nch")?
        * (1. + real(model, "lpe0")? / real(params, "l")?)
        * 1e6
        * thickness;
    let vbs0t = 0.8 - 0.5 * charge / (EPS_SI / thickness) + real(model, "vbsa")?;
    if !vbs0t.is_finite() {
        return Err(ModelError::InvalidPhysicalParameter("vbs0t"));
    }
    let mut mode = selector(params, "soimod", 3)?;
    if mode == 3 {
        mode = if vbs0t > real(model, "vbs0fd")? {
            2
        } else {
            i32::from(vbs0t >= real(model, "vbs0pd")?)
        };
        put(params, "soimod", ParameterValue::Integer(mode))?;
    }
    Ok((mode, vbs0t))
}

fn body_contact(
    model: &mut BoundParameters,
    mode: i32,
    count: u8,
    thermal_flag_given: bool,
) -> Result<(BsimSoiNode, BsimSoiNode, BsimSoiNode, BsimSoiBodyMode, bool), ModelError> {
    use BsimSoiNode::{Body, External, Ground, Temperature};
    let mut temperature = if count == 7 { External(6) } else { Temperature };
    if mode == 2 {
        return Ok((
            Ground,
            Ground,
            temperature,
            BsimSoiBodyMode::NoContact,
            false,
        ));
    }
    if count == 4 || (count == 5 && thermal_flag_given) {
        if count == 5 {
            temperature = External(4);
        }
        return Ok((Body, Ground, temperature, BsimSoiBodyMode::NoContact, true));
    }
    let explicit_body = count == 7 || (count == 6 && !thermal_flag_given);
    if count == 6 && thermal_flag_given {
        temperature = External(5);
    }
    let zero_resistance = real(model, "rbody")? == 0. && real(model, "rbsh")? == 0.;
    if explicit_body {
        // The native setup modifies the shared model at this point. Subsequent
        // instances see the change; previously resolved aliases remain intact.
        if zero_resistance {
            put(model, "rbody", ParameterValue::Real(1.))?;
        }
        Ok((
            External(5),
            External(4),
            temperature,
            BsimSoiBodyMode::Resistive,
            false,
        ))
    } else if zero_resistance {
        Ok((
            External(4),
            External(4),
            temperature,
            BsimSoiBodyMode::Ideal,
            false,
        ))
    } else {
        Ok((
            Body,
            External(4),
            temperature,
            BsimSoiBodyMode::Resistive,
            false,
        ))
    }
}

fn topology(
    model: &mut BoundParameters,
    params: &mut BoundParameters,
    count: u8,
    thermal_flag_given: bool,
    mode: i32,
) -> Result<BsimSoiTopology, ModelError> {
    use BsimSoiNode::{
        DrainBody, DrainPrime, External, Gate, GateMid, Ground, SourceBody, SourcePrime,
    };
    let (body, contact, mut temperature, body_mode, floating_body) =
        body_contact(model, mode, count, thermal_flag_given)?;
    if integer(model, "shmod")? != 1 || real(params, "rth0")? == 0. {
        temperature = Ground;
    }
    let gate_mode = selector(params, "rgatemod", 3)?;
    let mut resistance_mode = selector(params, "rbodymod", 1)?;
    if mode == 2 && resistance_mode == 1 {
        resistance_mode = 0;
        put(params, "rbodymod", ParameterValue::Integer(0))?;
    }
    let sheet = real(model, "rsh")?;
    Ok(BsimSoiTopology {
        drain_prime: if sheet > 0. && real(params, "nrd")? > 0. {
            DrainPrime
        } else {
            External(0)
        },
        source_prime: if sheet > 0. && real(params, "nrs")? > 0. {
            SourcePrime
        } else {
            External(2)
        },
        gate: if gate_mode > 0 { Gate } else { External(1) },
        gate_mid: if gate_mode == 3 { GateMid } else { External(1) },
        body,
        contact,
        temperature,
        body_mode,
        floating_body,
        drain_body: if resistance_mode == 1 {
            DrainBody
        } else {
            body
        },
        source_body: if resistance_mode == 1 {
            SourceBody
        } else {
            body
        },
    })
}

/// Resolve source model-wide and instance-specific setup in native traversal order.
///
/// # Errors
/// Rejects invalid scope, node count, scale, geometry, selector or required
/// physical dependency. Does not advertise a nonlinear BSIMSOI kernel.
pub fn prepare_bsim_soi_setup(
    model: &PreparedBsimSoiModel,
    inputs: &[BsimSoiInstanceInput<'_>],
) -> Result<PreparedBsimSoiSetup, ModelError> {
    let mut model = model.clone();
    if !inputs.is_empty() {
        clamp_ideal_fd_doping(&mut model.parameters)?;
    }
    let mut instances = Vec::with_capacity(inputs.len());
    for input in inputs {
        if !(4..=7).contains(&input.terminal_count) {
            return Err(ModelError::InvalidPhysicalParameter(
                "BSIMSOI terminal count",
            ));
        }
        let mut params = fill_instance(&model.parameters, input.parameters, input.geometry_scale)?;
        let (mode, vbs0t) = depletion_mode(&model.parameters, &mut params)?;
        let flag_given = input
            .parameters
            .get_slot(slot(ParameterScope::Instance, "tnodeout")?)
            .is_some();
        let topology = topology(
            &mut model.parameters,
            &mut params,
            input.terminal_count,
            flag_given,
            mode,
        )?;
        instances.push(PreparedBsimSoiInstance {
            initial_conditions: params.bsim_soi_ic,
            parameters: params,
            topology,
            vbs0t,
            original_given: input
                .parameters
                .values
                .iter()
                .map(Option::is_some)
                .collect(),
        });
    }
    let sizes = instances
        .iter()
        .map(|instance| {
            let geometry = geometry::prepare(&model.parameters, &instance.parameters)?;
            bin_sizes(&model.parameters, geometry)
        })
        .collect::<Result<_, _>>()?;
    Ok(PreparedBsimSoiSetup {
        model,
        instances,
        sizes,
    })
}
