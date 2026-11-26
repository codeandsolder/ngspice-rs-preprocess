//! Source-order BSIM3SOI FD/DD 2.1 and PD 2.2.3 preparation.
//! Copyright 1999/1990 Regents of the University of California. See package LICENSE.

mod instance;

pub use instance::{Geometry, PdGeometry, PreparedInstance, RawBinned, prepare_instance};
mod rules;

use crate::{
    BoundParameters, ModelError, ModelVariant, MosPolarity, ParameterScope, ParameterValue,
};

const EPS_OX: f64 = 3.453_133e-11;
// This release uses a truncated pi in the fringe-capacitance expression.
const SOURCE_PI: f64 = 3_141_592_654.0 / 1e9;

#[derive(Clone, Copy)]
enum Expression {
    Real(f64),
    Integer(i32),
    Copy(&'static str),
    Polarity(f64, f64),
    MobilityThree(f64, f64),
    NominalTemperature,
    Fringe,
    Overlap(&'static str),
}

#[derive(Clone, Copy)]
struct Rule {
    name: &'static str,
    expression: Expression,
}

impl Rule {
    const fn new(name: &'static str, expression: Expression) -> Self {
        Self { name, expression }
    }
}

fn slot(p: &BoundParameters, name: &str) -> Result<u16, ModelError> {
    p.variant()
        .parameter(p.scope(), name)
        .map(|spec| spec.slot)
        .ok_or_else(|| ModelError::UnknownParameter(name.to_owned()))
}

fn value<'a>(p: &'a BoundParameters, name: &str) -> Result<&'a ParameterValue, ModelError> {
    let id = slot(p, name)?;
    p.get_slot(id)
        .ok_or(ModelError::MissingDefaultDependency(id))
}

fn real(p: &BoundParameters, name: &str) -> Result<f64, ModelError> {
    if let ParameterValue::Real(v) = value(p, name)? {
        Ok(*v)
    } else {
        Err(ModelError::MissingDefaultDependency(slot(p, name)?))
    }
}

fn integer(p: &BoundParameters, name: &str) -> Result<i32, ModelError> {
    if let ParameterValue::Integer(v) = value(p, name)? {
        Ok(*v)
    } else {
        Err(ModelError::MissingDefaultDependency(slot(p, name)?))
    }
}

fn given(p: &BoundParameters, name: &str) -> Result<bool, ModelError> {
    Ok(p.get_slot(slot(p, name)?).is_some())
}

fn flag(p: &BoundParameters, name: &str) -> Result<bool, ModelError> {
    Ok(matches!(
        p.get_slot(slot(p, name)?),
        Some(ParameterValue::Flag(true))
    ))
}

fn assign(p: &mut BoundParameters, name: &str, v: ParameterValue) -> Result<(), ModelError> {
    let spec = p
        .variant()
        .parameter(p.scope(), name)
        .ok_or_else(|| ModelError::UnknownParameter(name.to_owned()))?;
    if spec.kind != v.kind() {
        return Err(ModelError::WrongType {
            name: name.to_owned(),
            expected: spec.kind,
        });
    }
    if !v.finite() {
        return Err(ModelError::NonFinite(name.to_owned()));
    }
    p.values[usize::from(spec.slot)] = Some(v);
    Ok(())
}

fn default(p: &mut BoundParameters, name: &str, v: ParameterValue) -> Result<(), ModelError> {
    if !given(p, name)? {
        assign(p, name, v)?;
    }
    Ok(())
}

fn positive(v: f64, name: &'static str) -> Result<f64, ModelError> {
    if v.is_finite() && v > 0.0 {
        Ok(v)
    } else {
        Err(ModelError::InvalidPhysicalParameter(name))
    }
}

/// Prepared BSIM3SOI card, with source-given state for later size preparation.
#[derive(Clone, Debug, PartialEq)]
pub struct PreparedModel {
    parameters: BoundParameters,
    given: Box<[bool]>,
    polarity: MosPolarity,
    oxide_capacitance_f_per_m2: f64,
    diffusion_temperature_exponent: f64,
}

impl PreparedModel {
    /// Prepared model parameter values. Absent fields still need physical derivation.
    #[must_use = "the buffer contains prepared model parameters"]
    pub const fn parameters(&self) -> &BoundParameters {
        &self.parameters
    }

    /// Channel polarity.
    #[must_use = "polarity determines the compact model's equations"]
    pub const fn polarity(&self) -> MosPolarity {
        self.polarity
    }

    /// Whether a canonical card slot was explicitly specified.
    #[must_use = "given bits select later physical derivations"]
    pub fn explicitly_given(&self, slot: u16) -> bool {
        self.given.get(usize::from(slot)).copied().unwrap_or(false)
    }

    /// Oxide capacitance density in farads per square meter.
    #[must_use = "oxide capacitance is used by charge and transport equations"]
    pub const fn oxide_capacitance_f_per_m2(&self) -> f64 {
        self.oxide_capacitance_f_per_m2
    }

    /// Internal diffusion current temperature exponent.
    ///
    /// FD/DD register XDIF as an alias for XBJT, but the separate internal
    /// diffusion exponent stays at 2. PD exposes the actual diffusion exponent.
    #[must_use = "the exponent affects the junction temperature laws"]
    pub const fn diffusion_temperature_exponent(&self) -> f64 {
        self.diffusion_temperature_exponent
    }
}

fn convert_inputs(p: &mut BoundParameters) -> Result<(), ModelError> {
    // The source setter treats large NCH/NGATE values as inverse cubic meters.
    for (name, limit) in [("nch", 1e20), ("ngate", 1e23)] {
        if given(p, name)? {
            let v = real(p, name)?;
            if v > limit {
                assign(p, name, ParameterValue::Real(v * 1e-6))?;
            }
        }
    }
    if given(p, "tnom")? {
        let v = real(p, "tnom")? + 273.15;
        if !v.is_finite() || v <= 0.0 {
            return Err(ModelError::InvalidNominalTemperature);
        }
        assign(p, "tnom", ParameterValue::Real(v))?;
    }
    Ok(())
}

fn evaluate_rule(
    rule: Rule,
    p: &BoundParameters,
    source: &BoundParameters,
    polarity: MosPolarity,
    nominal: f64,
) -> Result<ParameterValue, ModelError> {
    use Expression::{
        Copy, Fringe, Integer, MobilityThree, NominalTemperature, Overlap, Polarity, Real,
    };
    Ok(match rule.expression {
        Real(v) => ParameterValue::Real(v),
        Integer(v) => ParameterValue::Integer(v),
        Copy(name) => value(p, name)?.clone(),
        Polarity(n, p) => ParameterValue::Real(if polarity == MosPolarity::Nmos { n } else { p }),
        MobilityThree(a, b) => ParameterValue::Real(if integer(p, "mobmod")? == 3 { a } else { b }),
        NominalTemperature => ParameterValue::Real(nominal),
        Fringe => ParameterValue::Real(
            2.0 * EPS_OX / SOURCE_PI * (0.4e-6 / positive(real(p, "tox")?, "tox")?).ln_1p(),
        ),
        Overlap(cg) => {
            let cox = EPS_OX / positive(real(p, "tox")?, "tox")?;
            let dlc = real(p, "dlc")?;
            let v = if given(source, "dlc")? && dlc > 0.0 {
                dlc.mul_add(cox, -real(p, cg)?)
            } else {
                0.6 * real(p, "xj")? * cox
            };
            ParameterValue::Real(v)
        }
    })
}

fn selectors(p: &BoundParameters) -> Result<(), ModelError> {
    for (name, min, max) in [
        ("mobmod", 1, 3),
        ("capmod", 2, 3),
        ("noimod", 1, 4),
        ("binunit", 1, 2),
        ("shmod", 0, 1),
        ("paramchk", 0, 1),
    ] {
        let v = integer(p, name)?;
        if !(min..=max).contains(&v) {
            return Err(ModelError::InvalidSelector {
                name: name.to_owned(),
                value: v,
            });
        }
    }
    if p.variant() == ModelVariant::Bsim3SoiPd {
        if given(p, "ddmod")? {
            return Err(ModelError::InvalidSelector {
                name: "ddmod".to_owned(),
                value: integer(p, "ddmod")?,
            });
        }
        let v = integer(p, "igmod")?;
        if !(0..=1).contains(&v) {
            return Err(ModelError::InvalidSelector {
                name: "igmod".to_owned(),
                value: v,
            });
        }
    }
    Ok(())
}

/// Prepare the exact registered FD/DD or PD model card, without nonlinear evaluation.
///
/// Model version is source metadata; the variant selects the compiled source
/// implementation. Values without setup defaults remain absent until size
/// preparation. In particular, the source registration of PD DDMOD has no
/// setter; an explicit DDMOD is rejected.
///
/// # Errors
/// Wrong family/scope, invalid temperature, conflicting polarity, unsupported
/// selectors, missing source dependency or non-finite preparation.
pub fn prepare_model(
    source: &BoundParameters,
    nominal_temperature_kelvin: f64,
) -> Result<PreparedModel, ModelError> {
    let program = match source.variant() {
        ModelVariant::Bsim3SoiFd | ModelVariant::Bsim3SoiDd => rules::FD_DD,
        ModelVariant::Bsim3SoiPd => rules::PD,
        _ => return Err(ModelError::WrongModelScope),
    };
    if source.scope() != ParameterScope::Model {
        return Err(ModelError::WrongModelScope);
    }
    if !nominal_temperature_kelvin.is_finite() || nominal_temperature_kelvin <= 0.0 {
        return Err(ModelError::InvalidNominalTemperature);
    }
    let pmos = flag(source, "pmos")?;
    if pmos && flag(source, "nmos")? {
        return Err(ModelError::ConflictingPolarity);
    }
    let polarity = if pmos {
        MosPolarity::Pmos
    } else {
        MosPolarity::Nmos
    };
    let mut parameters = source.clone();
    convert_inputs(&mut parameters)?;
    for &id in program {
        let rule = rules::RULES[usize::from(id)];
        if !given(source, rule.name)? {
            let v = evaluate_rule(
                rule,
                &parameters,
                source,
                polarity,
                nominal_temperature_kelvin,
            )?;
            assign(&mut parameters, rule.name, v)?;
        }
    }
    selectors(&parameters)?;
    let oxide_capacitance_f_per_m2 = positive(
        EPS_OX / positive(real(&parameters, "tox")?, "tox")?,
        "oxide capacitance",
    )?;
    let diffusion_temperature_exponent = if parameters.variant() == ModelVariant::Bsim3SoiPd {
        real(&parameters, "xdif")?
    } else {
        2.0
    };
    Ok(PreparedModel {
        parameters,
        polarity,
        given: source.values.iter().map(Option::is_some).collect(),
        oxide_capacitance_f_per_m2,
        diffusion_temperature_exponent,
    })
}
