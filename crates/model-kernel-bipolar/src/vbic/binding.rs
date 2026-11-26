//! Bind resolved native level-4/9 VBIC source assignments once.

use super::temperature::{self, COUNT, Coefficient};
use crate::{ModelError, Temperature, validate_positive};
use sha2::{Digest, Sha256};

/// Terminal voltage/current orientation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Polarity {
    Npn,
    Pnp,
}

impl Polarity {
    #[must_use = "the sign converts source voltages and currents"]
    pub const fn sign(self) -> f64 {
        match self {
            Self::Npn => 1.0,
            Self::Pnp => -1.0,
        }
    }
}

/// Resolved source value. Expressions and engineering suffixes are parser-owned.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Value<'a> {
    Scalar(f64),
    Flag(bool),
    Initial(&'a [f64]),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Assignment<'a> {
    pub name: &'a str,
    pub value: Value<'a>,
}

/// Source-indexed diagnostics for model/instance binding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Selector,
    Assignment {
        index: usize,
        name: String,
        reason: &'static str,
    },
    Physics(ModelError),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Selector => f.write_str("VBIC requires NPN/PNP and native LEVEL=4 or 9"),
            Self::Assignment {
                index,
                name,
                reason,
            } => write!(f, "VBIC assignment #{index} {name}: {reason}"),
            Self::Physics(error) => write!(f, "{error}"),
        }
    }
}
impl std::error::Error for Error {}
impl From<ModelError> for Error {
    fn from(value: ModelError) -> Self {
        Self::Physics(value)
    }
}

fn invalid(index: usize, name: &str, reason: &'static str) -> Error {
    Error::Assignment {
        index,
        name: name.to_owned(),
        reason,
    }
}

fn scalar(index: usize, a: Assignment<'_>) -> Result<f64, Error> {
    match a.value {
        Value::Scalar(value) if value.is_finite() => Ok(value),
        _ => Err(invalid(index, a.name, "expected finite scalar")),
    }
}

/// Native SOA limits, in volts; these are solver diagnostics, not current clamps.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Limits {
    pub reverse_be: f64,
    pub reverse_bc: f64,
    pub ce: f64,
    pub reverse_substrate: f64,
    pub forward_be: f64,
    pub forward_bc: f64,
    pub forward_substrate: f64,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            reverse_be: 1e99,
            reverse_bc: 1e99,
            ce: 1e99,
            reverse_substrate: 1e99,
            forward_be: 0.2,
            forward_bc: 0.2,
            forward_substrate: 0.2,
        }
    }
}

/// Native model, including setup decisions distinct from numeric coefficients.
#[derive(Clone, Copy, Debug)]
pub struct Model {
    nominal: temperature::Nominal,
    polarity: Polarity,
    self_heating: bool,
    limits: Limits,
    digest: [u8; 32],
}
impl Model {
    #[must_use = "the nominal parameters are the input to temperature preparation"]
    pub const fn nominal(&self) -> &temperature::Nominal {
        &self.nominal
    }
    #[must_use = "the orientation is required for source voltages"]
    pub const fn polarity(&self) -> Polarity {
        self.polarity
    }
    #[must_use = "the capability controls the required thermal topology"]
    pub const fn self_heating(&self) -> bool {
        self.self_heating
    }
    #[must_use = "the limits are used for independent solver diagnostics"]
    pub const fn limits(&self) -> Limits {
        self.limits
    }
    #[must_use = "the digest identifies the complete nominal model"]
    pub const fn digest(&self) -> &[u8; 32] {
        &self.digest
    }
}

const fn is_resistance(c: Coefficient) -> bool {
    matches!(
        c,
        Coefficient::Rcx
            | Coefficient::Rci
            | Coefficient::Rbx
            | Coefficient::Rbi
            | Coefficient::Re
            | Coefficient::Rs
            | Coefficient::Rbp
    )
}

fn limit_slot<'a>(limits: &'a mut Limits, name: &str) -> Option<&'a mut f64> {
    // Each keyword and its published alias refer to the same diagnostic.
    match name.to_ascii_lowercase().as_str() {
        "vbe_max" | "bvbe" => Some(&mut limits.reverse_be),
        "vbc_max" | "bvbc" => Some(&mut limits.reverse_bc),
        "vce_max" | "bvce" => Some(&mut limits.ce),
        "vsub_max" | "bvsub" => Some(&mut limits.reverse_substrate),
        "vbefwd" => Some(&mut limits.forward_be),
        "vbcfwd" => Some(&mut limits.forward_bc),
        "vsubfwd" => Some(&mut limits.forward_substrate),
        _ => None,
    }
}

fn digest(values: &[f64; COUNT], polarity: Polarity, heat: bool, limits: Limits) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"VBIC-native-1.1.5-parameter-set-v1");
    hash.update([u8::from(polarity == Polarity::Pnp), u8::from(heat)]);
    for value in values.iter().copied().chain([
        limits.reverse_be,
        limits.reverse_bc,
        limits.ce,
        limits.reverse_substrate,
        limits.forward_be,
        limits.forward_bc,
        limits.forward_substrate,
    ]) {
        hash.update(value.to_bits().to_le_bytes());
    }
    hash.finalize().into()
}

/// Apply native defaults and ordered setters.
///
/// Explicit resistor values below
/// 10 mOhm are clamped by the native setter, whereas omitted external
/// resistors remain exact shorts. VERS/VREF are source metadata, not selectors.
///
/// # Errors
/// Unsupported selector, unknown or malformed assignment, invalid temperature
/// or invalid nominal denominators.
pub fn model(base: &str, level: f64, assignments: &[Assignment<'_>]) -> Result<Model, Error> {
    let mut polarity = if base.eq_ignore_ascii_case("npn") {
        Polarity::Npn
    } else if base.eq_ignore_ascii_case("pnp") {
        Polarity::Pnp
    } else {
        return Err(Error::Selector);
    };
    if ![4.0, 9.0].contains(&level) {
        return Err(Error::Selector);
    }
    let mut values = Coefficient::ALL.map(Coefficient::source_default);
    let mut heat = None;
    let mut rth_given = false;
    let mut limits = Limits::default();
    for (index, &a) in assignments.iter().enumerate() {
        if let Some(c) = Coefficient::from_name(a.name) {
            let value = scalar(index, a)?;
            values[c.index()] = if is_resistance(c) {
                value.max(0.01)
            } else {
                value
            };
            rth_given |= c == Coefficient::Rth;
        } else if a.name.eq_ignore_ascii_case("level") {
            if scalar(index, a)?.to_bits() != level.to_bits() {
                return Err(Error::Selector);
            }
        } else if a.name.eq_ignore_ascii_case("selft") {
            let value = scalar(index, a)?;
            if ![0.0, 1.0].contains(&value) {
                return Err(invalid(index, a.name, "SELFT requires 0 or 1"));
            }
            heat = Some(value.to_bits() == 1.0_f64.to_bits());
        } else if a.name.eq_ignore_ascii_case("npn") || a.name.eq_ignore_ascii_case("pnp") {
            let Value::Flag(flag) = a.value else {
                return Err(invalid(index, a.name, "expected polarity flag"));
            };
            if flag {
                polarity = if a.name.eq_ignore_ascii_case("npn") {
                    Polarity::Npn
                } else {
                    Polarity::Pnp
                };
            }
        } else if let Some(slot) = limit_slot(&mut limits, a.name) {
            *slot = scalar(index, a)?;
        } else {
            return Err(invalid(index, a.name, "unknown model parameter"));
        }
    }
    let self_heating = heat.unwrap_or(true) && rth_given && values[Coefficient::Rth.index()] > 0.0;
    if rth_given {
        values[Coefficient::Cth.index()] = values[Coefficient::Cth.index()].max(1e-12);
    }
    let nominal = temperature::Nominal::new(&values)?;
    Ok(Model {
        nominal,
        polarity,
        self_heating,
        limits,
        digest: digest(&values, polarity, self_heating, limits),
    })
}

/// Solver initialization metadata, not constitutive law inputs.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Initial {
    pub off: bool,
    pub vbe: Option<f64>,
    pub vce: Option<f64>,
}

/// Fully resolved instance controls.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Instance {
    area: f64,
    multiplicity: f64,
    temperature_celsius: Option<f64>,
    delta_kelvin: f64,
    initial: Initial,
}

impl Instance {
    /// Construct resolved runtime controls without re-parsing model-card names.
    /// # Errors
    /// Invalid geometry or effective temperature.
    pub fn new(
        area: f64,
        multiplicity: f64,
        temperature_celsius: Option<f64>,
        delta_kelvin: f64,
    ) -> Result<Self, ModelError> {
        Self {
            area,
            multiplicity,
            temperature_celsius,
            delta_kelvin,
            initial: Initial::default(),
        }
        .validated()
    }

    #[must_use = "area is an independent physical instance parameter"]
    pub const fn area(self) -> f64 {
        self.area
    }

    #[must_use = "the optional override determines the instance temperature coordinate"]
    pub const fn temperature_celsius(self) -> Option<f64> {
        self.temperature_celsius
    }

    #[must_use = "the temperature offset is a separate runtime parameter"]
    pub const fn delta_kelvin(self) -> f64 {
        self.delta_kelvin
    }

    fn validated(self) -> Result<Self, ModelError> {
        validate_positive("area", self.area)?;
        validate_positive("m", self.multiplicity)?;
        validate_positive("area*m", self.scale())?;
        if !self.delta_kelvin.is_finite() {
            return Err(ModelError::InvalidParameter("dtemp"));
        }
        if let Some(c) = self.temperature_celsius {
            Temperature::new(c + 273.15 + self.delta_kelvin)?;
        }
        Ok(self)
    }

    #[must_use = "area times multiplicity scales every electrical current and charge"]
    pub fn scale(self) -> f64 {
        self.area * self.multiplicity
    }
    #[must_use = "the multiplier separately determines flicker noise"]
    pub const fn multiplicity(self) -> f64 {
        self.multiplicity
    }
    #[must_use = "the initial conditions belong to solver initialization"]
    pub const fn initial(self) -> Initial {
        self.initial
    }

    /// Resolve TEMP then DTEMP; model DTEMP is applied by the next stage.
    /// # Errors
    /// Nonphysical effective ambient temperature.
    pub fn ambient(self, circuit: Temperature) -> Result<Temperature, ModelError> {
        Temperature::new(
            self.temperature_celsius
                .map_or_else(|| circuit.kelvin(), |c| c + 273.15)
                + self.delta_kelvin,
        )
    }
}

/// Bind native Q instance controls in source order.
/// # Errors
/// Unknown or malformed assignments, nonphysical area/multiplicity or temperature.
pub fn instance(assignments: &[Assignment<'_>]) -> Result<Instance, Error> {
    let mut result = Instance {
        area: 1.0,
        multiplicity: 1.0,
        temperature_celsius: None,
        delta_kelvin: 0.0,
        initial: Initial::default(),
    };
    for (index, &a) in assignments.iter().enumerate() {
        match a.name.to_ascii_lowercase().as_str() {
            "area" => result.area = scalar(index, a)?,
            "m" => result.multiplicity = scalar(index, a)?,
            "temp" => result.temperature_celsius = Some(scalar(index, a)?),
            "dtemp" => result.delta_kelvin = scalar(index, a)?,
            "icvbe" => result.initial.vbe = Some(scalar(index, a)?),
            "icvce" => result.initial.vce = Some(scalar(index, a)?),
            "off" => {
                let Value::Flag(flag) = a.value else {
                    return Err(invalid(index, a.name, "expected OFF flag"));
                };
                result.initial.off = flag;
            }
            "ic" => {
                let Value::Initial(values) = a.value else {
                    return Err(invalid(index, a.name, "expected IC vector"));
                };
                if !(1..=2).contains(&values.len()) || !values.iter().all(|v| v.is_finite()) {
                    return Err(invalid(
                        index,
                        a.name,
                        "IC expects one or two finite voltages",
                    ));
                }
                result.initial.vbe = values.first().copied();
                if let Some(vce) = values.get(1) {
                    result.initial.vce = Some(*vce);
                }
            }
            _ => return Err(invalid(index, a.name, "unknown instance parameter")),
        }
    }
    Ok(result.validated()?)
}
