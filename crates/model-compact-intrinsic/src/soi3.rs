//! STAG 2.7 (SOI3 level 60) model and instance preparation.
//! Copyright 2000 UK Secretary of State for Defence through DERA.
//! Developed at the University of Southampton. See package LICENSE.

mod instance;
mod junction;

pub use junction::{JunctionResult, PreparedJunctions};

pub use instance::{
    CircuitDefaults, PreparedInstance, SeriesBranch, TemperatureState, ThermalNetwork,
    ThermalSection, prepare_instance,
};

use crate::{
    BoundParameters, ModelError, ModelVariant, MosPolarity, ParameterScope, ParameterType,
    ParameterValue,
    preparation::{assign, default, flag, given, integer, positive, real},
};

const EPSILON_0: f64 = 8.854_214_871e-12;
const CHARGE: f64 = 1.602_176_620_8e-19;
const BOLTZMANN: f64 = 1.380_648_52e-23;
const K_OVER_Q: f64 = BOLTZMANN / CHARGE;
const INTRINSIC_DENSITY: f64 = 1.45e16;

fn bandgap(temperature: f64) -> f64 {
    (7.02e-4 * temperature / (temperature + 1108.0)).mul_add(-temperature, 1.16)
}

/// Nominal film capacitance densities, in F/m².
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FilmCapacitance {
    /// Front oxide.
    pub front_oxide: f64,
    /// Back oxide.
    pub back_oxide: f64,
    /// Silicon film.
    pub silicon: f64,
    /// Front interface states.
    pub front_surface_states: f64,
    /// Back interface states.
    pub back_surface_states: f64,
}

impl FilmCapacitance {
    fn surface_factor(self) -> f64 {
        1.0 + self.front_surface_states / self.front_oxide
    }
}

/// Channel-length modulation selection derived from the original given flags.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChannelLengthModulation {
    /// Conventional lambda law.
    Lambda,
    /// Alternative LX/VP/MEXP law.
    LxVp,
}

/// Shared model preparation after nominal electrostatic derivation.
///
/// CHID and CHID1 remain absent when not explicitly supplied because the
/// native temperature stage derives them from the instance's temperature.
#[derive(Clone, Debug, PartialEq)]
pub struct PreparedModel {
    parameters: BoundParameters,
    given: Box<[bool]>,
    polarity: MosPolarity,
    film: FilmCapacitance,
    modulation: ChannelLengthModulation,
    minimum_body_thickness: f64,
    saturation_velocity_factor: f64,
}

impl PreparedModel {
    /// Prepared values in native storage units, including kelvin TNOM.
    #[must_use = "the model card contains prepared parameters"]
    pub const fn parameters(&self) -> &BoundParameters {
        &self.parameters
    }

    /// Whether the original card explicitly supplied this slot.
    #[must_use = "given flags select the model's physical branches"]
    pub fn explicitly_given(&self, slot: u16) -> bool {
        self.given.get(usize::from(slot)).copied().unwrap_or(false)
    }

    /// N- or P-channel polarity.
    #[must_use = "polarity determines the model equations"]
    pub const fn polarity(&self) -> MosPolarity {
        self.polarity
    }

    /// Nominal film and interface-state capacitance densities.
    #[must_use = "film capacitance determines electrostatics"]
    pub const fn film(&self) -> FilmCapacitance {
        self.film
    }

    /// Selected channel-length modulation equation.
    #[must_use = "selects a transport equation"]
    pub const fn modulation(&self) -> ChannelLengthModulation {
        self.modulation
    }

    /// Thick-film validity threshold in meters; native reports a warning below this.
    #[must_use = "the model has a thick-film validity criterion"]
    pub const fn minimum_body_thickness(&self) -> f64 {
        self.minimum_body_thickness
    }

    /// Native temperature factor for a nonzero explicit VSAT.
    #[must_use = "the factor is used by the saturation-velocity law"]
    pub const fn saturation_velocity_factor(&self) -> f64 {
        self.saturation_velocity_factor
    }
}

fn defaults(source: &BoundParameters, nominal: f64) -> Result<BoundParameters, ModelError> {
    let mut p = source.clone();
    for (name, number) in [
        ("js", 1e-10),
        ("kp", 2e-5),
        ("pb", 0.8),
        ("mj", 0.5),
        ("fc", 0.5),
        ("kox", 1.4),
        ("shsi", 700.0),
        ("dsi", 2330.0),
        ("af", 1.0),
        ("k", 1.5),
        ("eta", 1.0),
        ("beta0", 1.92e6),
        ("betaexp", 2.0),
        ("csf", 0.5),
        ("nplus", 1e20),
    ] {
        default(&mut p, name, ParameterValue::Real(number))?;
    }
    for name in ["etad", "etad1"] {
        if !given(source, name)? || real(source, name)? == 0.0 {
            assign(&mut p, name, ParameterValue::Real(1.0))?;
        }
    }
    default(&mut p, "dvt", ParameterValue::Integer(1))?;
    // The original C model is zero-initialized. Preserve absence for the
    // per-instance temperature coefficients; all other real defaults are
    // resolved now, including setup fields without explicit C assignments.
    for spec in ModelVariant::Soi3.parameters(ParameterScope::Model) {
        if !given(&p, spec.name)? && !matches!(spec.name, "chid" | "chid1" | "tnom") {
            match spec.kind {
                ParameterType::Real => assign(&mut p, spec.name, ParameterValue::Real(0.0))?,
                ParameterType::Integer => assign(&mut p, spec.name, ParameterValue::Integer(0))?,
                _ => {}
            }
        }
    }
    let tnom = if given(source, "tnom")? {
        real(source, "tnom")? + 273.15
    } else {
        nominal
    };
    if !tnom.is_finite() || tnom <= 0.0 {
        return Err(ModelError::InvalidNominalTemperature);
    }
    assign(&mut p, "tnom", ParameterValue::Real(tnom))?;
    Ok(p)
}

fn film(p: &BoundParameters) -> Result<FilmCapacitance, ModelError> {
    Ok(FilmCapacitance {
        front_oxide: 3.9 * EPSILON_0 / positive(real(p, "tof")?, "tof")?,
        back_oxide: 3.9 * EPSILON_0 / positive(real(p, "tob")?, "tob")?,
        silicon: 11.7 * EPSILON_0 / positive(real(p, "tb")?, "tb")?,
        front_surface_states: CHARGE * real(p, "nssf")? * 1e4,
        back_surface_states: CHARGE * real(p, "nssb")? * 1e4,
    })
}

fn set_real(p: &mut BoundParameters, name: &str, v: f64) -> Result<(), ModelError> {
    assign(p, name, ParameterValue::Real(v))
}

fn derive_doped(
    p: &mut BoundParameters,
    source: &BoundParameters,
    film: FilmCapacitance,
    sign: f64,
    thermal_voltage: f64,
    gap: f64,
) -> Result<f64, ModelError> {
    let density = real(p, "nsub")? * 1e6;
    if density <= INTRINSIC_DENSITY {
        return Err(ModelError::InvalidPhysicalParameter("nsub"));
    }
    if !given(source, "phi")? {
        set_real(
            p,
            "phi",
            (2.0 * thermal_voltage * (density / INTRINSIC_DENSITY).ln()).max(0.1),
        )?;
    }
    let phi = positive(real(p, "phi")?, "phi")?;
    let depletion = 2.0 * (2.0 * 11.7 * EPSILON_0 * phi / (CHARGE * density)).sqrt();
    if !given(source, "vfbf")? {
        if !given(source, "tpg")? {
            assign(p, "tpg", ParameterValue::Integer(1))?;
        }
        let gate_type = integer(p, "tpg")?;
        let silicon_work = (0.5 * sign).mul_add(phi, 0.5_f64.mul_add(gap, 3.25));
        let gate_work = if gate_type == 0 {
            3.2
        } else {
            (sign * f64::from(gate_type))
                .mul_add(-0.5, 0.5)
                .mul_add(gap, 3.25)
        };
        let flatband =
            gate_work - silicon_work - real(p, "nqff")? * 1e4 * CHARGE / film.front_oxide;
        set_real(p, "vfbf", flatband)?;
    }
    if !given(source, "vfbb")? {
        let flatband =
            ((1.0 - sign) * 0.5).mul_add(phi, -(real(p, "nqfb")? * 1e4 * CHARGE / film.back_oxide));
        set_real(p, "vfbb", flatband)?;
    }
    let gamma_numerator = (2.0 * 11.7 * EPSILON_0 * CHARGE * density).sqrt();
    for (name, cap) in [("gamma", film.front_oxide), ("gammab", film.back_oxide)] {
        if !given(source, name)? {
            set_real(p, name, gamma_numerator / cap)?;
        }
    }
    Ok(depletion)
}

fn threshold_to_flatband(
    p: &mut BoundParameters,
    source: &BoundParameters,
    film: FilmCapacitance,
    sign: f64,
    thermal_voltage: f64,
    doped: bool,
) -> Result<(), ModelError> {
    let phi = positive(real(p, "phi")?, "phi")?;
    let gamma = real(p, "gamma")?;
    let eta = film.surface_factor();
    if !given(source, "vt0")? && given(source, "vtex")? {
        let delta = real(p, "delta0")?.max(0.0);
        let vdex = real(p, "vdex")?.abs();
        set_real(p, "delta0", delta)?;
        set_real(p, "vdex", vdex)?;
        let exponential = (delta * phi / (2.0 * thermal_voltage)).min(709.0).exp();
        let psi = ((2.0 + delta) * phi).midpoint(vdex);
        let flat = sign.mul_add(
            -gamma.mul_add(
                (thermal_voltage.mul_add(exponential, psi)).sqrt(),
                eta * psi,
            ),
            real(p, "vtex")?,
        );
        set_real(p, "vfbf", flat)?;
    } else if given(source, "vt0")? || !doped {
        let flat = sign.mul_add(-gamma.mul_add(phi.sqrt(), eta * phi), real(p, "vt0")?);
        set_real(p, "vfbf", flat)?;
    }
    Ok(())
}

/// Prepare the STAG nominal model and preserve source branch-selection flags.
///
/// # Errors
/// Wrong model scope, contradictory polarity, missing/invalid film thickness,
/// invalid temperature or doping, or non-finite nominal derivations.
pub fn prepare_model(
    source: &BoundParameters,
    nominal_kelvin: f64,
) -> Result<PreparedModel, ModelError> {
    if source.variant() != ModelVariant::Soi3 || source.scope() != ParameterScope::Model {
        return Err(ModelError::WrongModelScope);
    }
    if !nominal_kelvin.is_finite() || nominal_kelvin <= 0.0 {
        return Err(ModelError::InvalidNominalTemperature);
    }
    let pmos = flag(source, "psoi")?;
    if pmos && flag(source, "nsoi")? {
        return Err(ModelError::ConflictingPolarity);
    }
    let polarity = if pmos {
        MosPolarity::Pmos
    } else {
        MosPolarity::Nmos
    };
    let sign = if pmos { -1.0 } else { 1.0 };
    let mut parameters = defaults(source, nominal_kelvin)?;
    let film = film(&parameters)?;
    if !given(source, "kp")? {
        if !given(source, "u0")? {
            set_real(&mut parameters, "u0", 600.0)?;
        }
        let kp = real(&parameters, "u0")? * film.front_oxide * 1e-4;
        set_real(&mut parameters, "kp", kp)?;
    }
    let tnom = real(&parameters, "tnom")?;
    let thermal_voltage = tnom * K_OVER_Q;
    let doped = given(source, "nsub")?;
    let minimum_body_thickness = if doped {
        derive_doped(
            &mut parameters,
            source,
            film,
            sign,
            thermal_voltage,
            bandgap(tnom),
        )?
    } else {
        let phi = positive(real(&parameters, "phi")?, "phi")?;
        2.0 * 11.7 * EPSILON_0 * phi.sqrt() / (real(&parameters, "gamma")? * film.front_oxide)
    };
    threshold_to_flatband(&mut parameters, source, film, sign, thermal_voltage, doped)?;
    let modulation = if given(source, "lx")?
        && real(&parameters, "lx")? != 0.0
        && !given(source, "lambda")?
        && real(&parameters, "vp")? != 0.0
        && real(&parameters, "mexp")? > 0.0
    {
        ChannelLengthModulation::LxVp
    } else {
        ChannelLengthModulation::Lambda
    };
    let saturation_velocity_factor = if given(source, "vsat")? && real(&parameters, "vsat")? != 0.0
    {
        0.8 * (tnom / 600.0).exp()
    } else {
        0.0
    };
    Ok(PreparedModel {
        parameters,
        given: source.values.iter().map(Option::is_some).collect(),
        polarity,
        film,
        modulation,
        minimum_body_thickness,
        saturation_velocity_factor,
    })
}
