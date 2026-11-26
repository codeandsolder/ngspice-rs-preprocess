//! STAG 2.7 instance topology and temperature preparation.

use super::{BOLTZMANN, CHARGE, EPSILON_0, K_OVER_Q, PreparedModel, bandgap, set_real};
use crate::{
    BoundParameters, ModelError, ModelVariant, MosPolarity, ParameterScope, ParameterValue,
    preparation::{default, given, positive, real, slot},
};

/// Circuit temperatures and geometry defaults required by the source instance setup.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CircuitDefaults {
    /// Ambient circuit temperature in kelvin.
    pub temperature_kelvin: f64,
    /// Default drawn channel length in meters.
    pub length_m: f64,
    /// Default drawn channel width in meters.
    pub width_m: f64,
}

/// A positive-conductance branch between an external and intrinsic terminal.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SeriesBranch {
    /// Conductance of all parallel devices in siemens.
    pub conductance_s: f64,
}

impl SeriesBranch {
    /// True when an independent prime node is required.
    #[must_use = "the branch determines the prime-node topology"]
    pub fn requires_prime(self) -> bool {
        self.conductance_s > 0.0
    }
}

/// One thermal ladder section with the multiplicity already applied.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ThermalSection {
    /// Thermal resistance in K/W.
    pub resistance_k_per_w: f64,
    /// Thermal capacitance in J/K.
    pub capacitance_j_per_k: f64,
}

/// Thermal terminal and ladder topology.
#[derive(Clone, Debug, PartialEq)]
pub enum ThermalNetwork {
    /// Zero RT: constrain the external thermal terminal to thermal ground.
    Isothermal,
    /// Nonzero RT: external thermal node followed by successive RC sections.
    Ladder(Box<[ThermalSection]>),
}

/// Temperature-adjusted values before nonlinear evaluation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TemperatureState {
    /// Actual initial device temperature in kelvin.
    pub kelvin: f64,
    /// Thermal voltage in volts.
    pub thermal_voltage: f64,
    /// Temperature-adjusted KP.
    pub transconductance: f64,
    /// Temperature-adjusted mobility in cm²/(V s).
    pub mobility: f64,
    /// Temperature-adjusted surface potential in volts.
    pub surface_potential: f64,
    /// Front flat-band voltage in volts.
    pub front_flatband: f64,
    /// Back flat-band voltage in volts.
    pub back_flatband: f64,
    /// Threshold voltage at the prepared temperature.
    pub threshold: f64,
    /// Junction activation coefficients CHID and CHID1 in kelvin.
    pub junction_activation: [f64; 2],
    /// Saturation-current and density multiplier.
    pub junction_current_factor: f64,
    /// Temperature-adjusted junction potential in volts.
    pub junction_potential: f64,
    /// Drain/source zero-bias junction capacitance including multiplicity.
    pub zero_bias_capacitance: [f64; 2],
    /// Junction limiting voltage in volts.
    pub critical_voltage: f64,
}

/// A prepared instance, including source flags, geometry and physical topology.
#[derive(Clone, Debug, PartialEq)]
pub struct PreparedInstance {
    parameters: BoundParameters,
    given: Box<[bool]>,
    initial_conditions: [Option<f64>; 4],
    effective_length: f64,
    thermal: ThermalNetwork,
    series: [SeriesBranch; 2],
    temperature: TemperatureState,
    junctions: super::PreparedJunctions,
}

impl PreparedInstance {
    /// Source-unit instance values; TEMP is stored in kelvin.
    #[must_use = "the instance contains prepared parameters"]
    pub const fn parameters(&self) -> &BoundParameters {
        &self.parameters
    }

    /// Original explicitly supplied parameter slot.
    #[must_use = "given flags select source branches"]
    pub fn explicitly_given(&self, slot: u16) -> bool {
        self.given.get(usize::from(slot)).copied().unwrap_or(false)
    }

    /// Explicit IC values in D-S, GF-S, GB-S, B-S order.
    #[must_use = "IC values determine startup constraints"]
    pub const fn initial_conditions(&self) -> [Option<f64>; 4] {
        self.initial_conditions
    }

    /// Effective channel length after lateral-diffusion correction.
    #[must_use = "effective length is required for current and charge"]
    pub const fn effective_length_m(&self) -> f64 {
        self.effective_length
    }

    /// Thermal terminal topology and scaled thermal ladder.
    #[must_use = "thermal topology is required for assembly"]
    pub const fn thermal(&self) -> &ThermalNetwork {
        &self.thermal
    }

    /// Drain and source branches in that order.
    #[must_use = "series branches determine internal electrical topology"]
    pub const fn series(&self) -> [SeriesBranch; 2] {
        self.series
    }

    /// Immutable physical junction coefficients bound to this instance.
    #[must_use = "junction coefficients supply device evaluation"]
    pub const fn junctions(&self) -> super::PreparedJunctions {
        self.junctions
    }

    /// Physical quantities adjusted to the initial device temperature.
    #[must_use = "temperature state supplies the nonlinear kernel"]
    pub const fn temperature(&self) -> TemperatureState {
        self.temperature
    }
}

fn model_given(model: &PreparedModel, name: &str) -> Result<bool, ModelError> {
    Ok(model.explicitly_given(slot(model.parameters(), name)?))
}

fn initial_conditions(
    source: &BoundParameters,
    p: &mut BoundParameters,
) -> Result<[Option<f64>; 4], ModelError> {
    let mut result = [None; 4];
    if let Some(ParameterValue::RealVector(values)) = source.get_slot(slot(source, "ic")?) {
        if !(1..=4).contains(&values.len()) {
            return Err(ModelError::InvalidInitialConditions(values.len()));
        }
        for (target, value) in result.iter_mut().zip(values) {
            *target = Some(*value);
        }
    }
    for (target, name) in result
        .iter_mut()
        .zip(["icvds", "icvgfs", "icvgbs", "icvbs"])
    {
        if given(source, name)? {
            // A slot buffer cannot recover the ordering of two overlapping
            // assignments. Reject ambiguity instead of guessing precedence.
            if target.is_some() {
                return Err(ModelError::UnsupportedInstanceParameter(name));
            }
            *target = Some(real(source, name)?);
        }
        set_real(p, name, target.unwrap_or_default())?;
    }
    Ok(result)
}

fn defaults(
    source: &BoundParameters,
    circuit: CircuitDefaults,
) -> Result<BoundParameters, ModelError> {
    let mut p = source.clone();
    for (name, value) in [
        ("l", circuit.length_m),
        ("w", circuit.width_m),
        ("m", 1.0),
        ("ad", 0.0),
        ("as", 0.0),
        ("ab", 0.0),
    ] {
        default(&mut p, name, ParameterValue::Real(value))?;
    }
    default(&mut p, "off", ParameterValue::Flag(false))?;
    for name in ["nrd", "nrs"] {
        if !given(source, name)? || real(source, name)? == 0.0 {
            set_real(&mut p, name, 1.0)?;
        }
    }
    for name in ["l", "w", "m", "nrd", "nrs"] {
        positive(real(&p, name)?, name)?;
    }
    Ok(p)
}

fn thermal(model: &PreparedModel, p: &mut BoundParameters) -> Result<ThermalNetwork, ModelError> {
    let card = model.parameters();
    let feature = real(card, "fmin")? * 1e-6;
    let area = positive(2.0_f64.mul_add(feature, real(p, "w")?), "thermal width")?
        * positive(4.0_f64.mul_add(feature, real(p, "l")?), "thermal length")?;
    positive(area, "thermal area")?;
    let rt = if model_given(model, "rta")? {
        real(card, "rta")? / area
    } else if real(card, "kox")? == 0.0 {
        0.0
    } else {
        real(card, "tob")? / (positive(real(card, "kox")?, "kox")? * area)
    };
    let ct = if model_given(model, "cta")? {
        real(card, "cta")? * area
    } else {
        real(card, "dsi")? * real(card, "shsi")? * area * real(card, "tb")?
    };
    default(p, "rt", ParameterValue::Real(rt))?;
    default(p, "ct", ParameterValue::Real(ct))?;
    let multiplier = real(p, "m")?;
    let mut sections = Vec::with_capacity(5);
    let mut active = true;
    for (r_name, c_name) in [
        ("rt", "ct"),
        ("rt1", "ct1"),
        ("rt2", "ct2"),
        ("rt3", "ct3"),
        ("rt4", "ct4"),
    ] {
        default(p, r_name, ParameterValue::Real(0.0))?;
        default(p, c_name, ParameterValue::Real(0.0))?;
        let resistance = real(p, r_name)?;
        let capacitance = real(p, c_name)?;
        if resistance < 0.0 || capacitance < 0.0 {
            return Err(ModelError::InvalidPhysicalParameter("thermal RC"));
        }
        active &= resistance != 0.0;
        if active {
            sections.push(ThermalSection {
                resistance_k_per_w: positive(resistance / multiplier, r_name)?,
                capacitance_j_per_k: finite(capacitance * multiplier, c_name)?,
            });
        }
    }
    Ok(if sections.is_empty() {
        ThermalNetwork::Isothermal
    } else {
        ThermalNetwork::Ladder(sections.into_boxed_slice())
    })
}

fn series(
    model: &PreparedModel,
    p: &BoundParameters,
    names: [&str; 3],
) -> Result<SeriesBranch, ModelError> {
    let [explicit, squares, width_scaled] = names;
    let card = model.parameters();
    let resistance = if model_given(model, explicit)? {
        real(card, explicit)?
    } else if model_given(model, "rsh")? {
        real(card, "rsh")? * real(p, squares)?
    } else if model_given(model, width_scaled)? {
        real(card, width_scaled)? / (real(p, "w")? * 1e6)
    } else {
        0.0
    };
    if !resistance.is_finite() || resistance < 0.0 {
        return Err(ModelError::InvalidPhysicalParameter("series resistance"));
    }
    let conductance_s = if resistance == 0.0 {
        0.0
    } else {
        positive(real(p, "m")? / resistance, "series conductance")?
    };
    Ok(SeriesBranch { conductance_s })
}

const REFERENCE_TEMP: f64 = 300.15;

fn pb_factor(temp: f64) -> f64 {
    let activation = (1.115_087_7 / REFERENCE_TEMP - bandgap(temp) / temp) / (2.0 * K_OVER_Q);
    -2.0 * temp * K_OVER_Q * 1.5_f64.mul_add((temp / REFERENCE_TEMP).ln(), activation)
}

const fn finite(value: f64, name: &'static str) -> Result<f64, ModelError> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(ModelError::InvalidPhysicalParameter(name))
    }
}

fn junction_activation(model: &PreparedModel, temp: f64) -> Result<[f64; 2], ModelError> {
    let computed = bandgap(temp) * CHARGE / BOLTZMANN;
    let mut result = [computed; 2];
    for (target, name) in result.iter_mut().zip(["chid", "chid1"]) {
        if model_given(model, name)? {
            *target = real(model.parameters(), name)?;
        }
    }
    Ok(result)
}

fn junction_caps(
    model: &PreparedModel,
    p: &BoundParameters,
    nominal: f64,
    temp: f64,
    potential: f64,
    base_potential: f64,
) -> Result<[f64; 2], ModelError> {
    let card = model.parameters();
    let grading = real(card, "mj")?;
    let cap_factor = |t: f64, pb: f64| {
        grading.mul_add(
            4e-4_f64.mul_add(
                t - REFERENCE_TEMP,
                -((pb - base_potential) / base_potential),
            ),
            1.0,
        )
    };
    let scale = positive(
        cap_factor(temp, potential) / cap_factor(nominal, real(card, "pb")?),
        "junction capacitance scale",
    )?;
    let area = real(p, "w")? * real(card, "tb")?;
    let nsub = real(card, "nsub")?;
    let nplus = real(card, "nplus")?;
    let mut result = [0.0; 2];
    for (target, name) in result.iter_mut().zip(["cbd", "cbs"]) {
        let base = if model_given(model, name)? {
            real(card, name)?
        } else if model_given(model, "cj")? {
            real(card, "cj")? * area
        } else {
            let ratio = 1e6 * nsub * nplus / positive(nplus + nsub, "junction doping sum")?;
            (ratio * 11.7 * EPSILON_0 * CHARGE / (2.0 * potential)).sqrt() * area
        };
        *target = finite(base * scale * real(p, "m")?, "zero-bias capacitance")?;
    }
    Ok(result)
}

fn temperature(
    model: &PreparedModel,
    p: &BoundParameters,
    temp: f64,
) -> Result<TemperatureState, ModelError> {
    let card = model.parameters();
    let nominal = real(card, "tnom")?;
    let ratio = temp / nominal;
    let vt = temp * K_OVER_Q;
    let vtnom = nominal * K_OVER_Q;
    let mobility_scale = positive(ratio.powf(real(card, "k")?), "mobility scale")?;
    let sign = if model.polarity() == MosPolarity::Nmos {
        1.0
    } else {
        -1.0
    };
    let nominal_phi = real(card, "phi")?;
    let pb_nom = pb_factor(nominal);
    let pb_actual = pb_factor(temp);
    let surface_potential = positive(
        ratio.mul_add(nominal_phi - pb_nom, pb_actual),
        "temperature-adjusted phi",
    )?;
    let gap = bandgap(temp);
    let nominal_gap = bandgap(nominal);
    let front_flatband = (sign * 0.5 * f64::from(crate::preparation::integer(card, "tpg")?))
        .mul_add(
            nominal_gap - gap,
            (sign * 0.5).mul_add(nominal_phi - surface_potential, real(card, "vfbf")?),
        );
    let back_flatband =
        ((1.0 - sign) * 0.5).mul_add(surface_potential - nominal_phi, real(card, "vfbb")?);
    let threshold = sign.mul_add(
        real(card, "gamma")?.mul_add(
            surface_potential.sqrt(),
            model.film().surface_factor() * surface_potential,
        ),
        front_flatband,
    );
    let junction_current_factor = finite(
        (nominal_gap / vtnom - gap / vt).exp(),
        "junction temperature factor",
    )?;
    let base_potential = positive(
        (real(card, "pb")? - pb_nom) / (nominal / REFERENCE_TEMP),
        "reference junction potential",
    )?;
    let junction_potential = positive(
        (temp / REFERENCE_TEMP).mul_add(base_potential, pb_actual),
        "junction potential",
    )?;
    let density = real(card, "js")? * junction_current_factor;
    let current = real(card, "is")? * junction_current_factor;
    let limiting_current = if density != 0.0 {
        density * real(p, "w")?
    } else if current != 0.0 {
        current
    } else {
        1e-15
    };
    let critical_voltage = finite(
        vt * (vt
            / (std::f64::consts::SQRT_2
                * positive(limiting_current, "limiting junction current")?))
        .ln(),
        "junction critical voltage",
    )?;
    Ok(TemperatureState {
        kelvin: temp,
        thermal_voltage: vt,
        transconductance: finite(real(card, "kp")? / mobility_scale, "kp(T)")?,
        mobility: finite(real(card, "u0")? / mobility_scale, "u0(T)")?,
        surface_potential,
        front_flatband: finite(front_flatband, "vfbf(T)")?,
        back_flatband: finite(back_flatband, "vfbb(T)")?,
        threshold: finite(threshold, "vto(T)")?,
        junction_activation: junction_activation(model, temp)?,
        junction_current_factor,
        junction_potential,
        critical_voltage,
        zero_bias_capacitance: junction_caps(
            model,
            p,
            nominal,
            temp,
            junction_potential,
            base_potential,
        )?,
    })
}

/// Resolve instance defaults, thermal ladder, electrical series branches and
/// initial-temperature material state.
///
/// # Errors
/// Wrong scope, invalid physical geometry/temperature/thermal or electrical
/// coefficients, or ambiguous/invalid initial-condition assignments.
pub fn prepare_instance(
    model: &PreparedModel,
    source: &BoundParameters,
    circuit: CircuitDefaults,
) -> Result<PreparedInstance, ModelError> {
    if source.variant() != ModelVariant::Soi3 || source.scope() != ParameterScope::Instance {
        return Err(ModelError::WrongModelScope);
    }
    let ambient = positive(circuit.temperature_kelvin, "circuit temperature")?;
    let mut parameters = defaults(source, circuit)?;
    let initial_conditions = initial_conditions(source, &mut parameters)?;
    let thermal = thermal(model, &mut parameters)?;
    let ld = real(model.parameters(), "ld")?;
    let drawn_length = real(&parameters, "l")?;
    let mut effective_length = (-2.0_f64).mul_add(ld, drawn_length);
    if effective_length <= 0.0 {
        // The native temp routine adds 2*LD to L when the correction
        // would otherwise create a nonpositive effective channel length.
        set_real(&mut parameters, "l", 2.0_f64.mul_add(ld, drawn_length))?;
        effective_length = drawn_length;
    }
    let temp = if matches!(thermal, ThermalNetwork::Isothermal) && given(source, "temp")? {
        positive(real(source, "temp")? + 273.15, "instance temperature")?
    } else {
        ambient
    };
    set_real(&mut parameters, "temp", temp)?;
    let series = [
        series(model, &parameters, ["rd", "nrd", "rdw"])?,
        series(model, &parameters, ["rs", "nrs", "rsw"])?,
    ];
    let temperature = temperature(model, &parameters, temp)?;
    let junctions = super::junction::prepare(model, &parameters, temperature, effective_length)?;
    Ok(PreparedInstance {
        parameters,
        given: source.values.iter().map(Option::is_some).collect(),
        initial_conditions,
        effective_length,
        thermal,
        series,
        temperature,
        junctions,
    })
}
