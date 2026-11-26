//! Source-order `B4SOIsetup` defaults; independent of numerical model evaluation.

mod bsim_soi_rules;

use crate::{BoundParameters, ModelError, ModelVariant, ParameterScope, ParameterValue};

/// MOSFET polarity selected by model-card flags; defaults to NMOS.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MosPolarity {
    /// N-channel polarity.
    Nmos,
    /// P-channel polarity.
    Pmos,
}

/// BSIMSOI model parameter preparation after source-accurate defaults.
///
/// This is *not* a compiled physical kernel. Actual temperature-dependent
/// binning and model equations remain separate operations.
#[derive(Clone, Debug, PartialEq)]
pub struct PreparedBsimSoiModel {
    polarity: MosPolarity,
    pub(super) parameters: BoundParameters,
    original_given: Box<[bool]>,
}

impl PreparedBsimSoiModel {
    /// Polarity after resolving explicit model flags.
    #[must_use = "the resolved polarity determines the device equations"]
    pub const fn polarity(&self) -> MosPolarity {
        self.polarity
    }

    /// O(1) prepared slot access. None means this parameter has no applicable
    /// source-derived default, not that its implicit physical value is zero.
    #[must_use = "the lookup result distinguishes an absent default from a bound value"]
    pub fn get_slot(&self, slot: u16) -> Option<&ParameterValue> {
        self.parameters.get_slot(slot)
    }

    /// Whether a source parameter was explicitly assigned before setup.
    #[must_use = "source presence controls size-dependent physical defaults"]
    pub fn was_given(&self, slot: u16) -> bool {
        self.original_given
            .get(usize::from(slot))
            .copied()
            .unwrap_or(false)
    }

    /// Borrow the complete prepared model parameter buffer.
    #[must_use = "the returned buffer supplies the prepared parameter values"]
    pub const fn parameters(&self) -> &BoundParameters {
        &self.parameters
    }
}

#[derive(Clone, Copy)]
struct DefaultRule {
    slot: u16,
    expression: DefaultExpression,
}

#[derive(Clone, Copy)]
enum DefaultExpression {
    Real(f64),
    Integer(i32),
    Copy(u16),
    Polarity { nmos: f64, pmos: f64 },
    MobilityThree { mode3: f64, other: f64 },
    Legacy { old_slot: u16, fallback: f64 },
    NominalTemperature,
    Fringe { tox_slot: u16 },
}

impl DefaultRule {
    const fn real(slot: u16, value: f64) -> Self {
        Self {
            slot,
            expression: DefaultExpression::Real(value),
        }
    }

    const fn integer(slot: u16, value: i32) -> Self {
        Self {
            slot,
            expression: DefaultExpression::Integer(value),
        }
    }

    const fn copy(slot: u16, source: u16) -> Self {
        Self {
            slot,
            expression: DefaultExpression::Copy(source),
        }
    }

    const fn polarity(slot: u16, nmos: f64, pmos: f64) -> Self {
        Self {
            slot,
            expression: DefaultExpression::Polarity { nmos, pmos },
        }
    }

    const fn mobility_three(slot: u16, mode3: f64, other: f64) -> Self {
        Self {
            slot,
            expression: DefaultExpression::MobilityThree { mode3, other },
        }
    }

    const fn legacy(slot: u16, old_slot: u16, fallback: f64) -> Self {
        Self {
            slot,
            expression: DefaultExpression::Legacy { old_slot, fallback },
        }
    }

    const fn nominal_temperature(slot: u16) -> Self {
        Self {
            slot,
            expression: DefaultExpression::NominalTemperature,
        }
    }

    const fn fringe(slot: u16, tox_slot: u16) -> Self {
        Self {
            slot,
            expression: DefaultExpression::Fringe { tox_slot },
        }
    }
}

fn model_slot(name: &str) -> Result<u16, ModelError> {
    ModelVariant::BsimSoi
        .parameter(ParameterScope::Model, name)
        .map(|spec| spec.slot)
        .ok_or_else(|| ModelError::UnknownParameter(name.to_owned()))
}

fn real(values: &[Option<ParameterValue>], slot: u16) -> Result<f64, ModelError> {
    let value = values.get(usize::from(slot)).and_then(Option::as_ref);
    if let Some(ParameterValue::Real(value)) = value {
        return Ok(*value);
    }
    Err(ModelError::MissingDefaultDependency(slot))
}

fn model_integer(values: &[Option<ParameterValue>], name: &str) -> Result<i32, ModelError> {
    let slot = model_slot(name)?;
    if let Some(ParameterValue::Integer(value)) =
        values.get(usize::from(slot)).and_then(Option::as_ref)
    {
        return Ok(*value);
    }
    Err(ModelError::MissingDefaultDependency(slot))
}

fn optional_flag(values: &[Option<ParameterValue>], name: &str) -> Result<bool, ModelError> {
    let slot = model_slot(name)?;
    match values.get(usize::from(slot)).and_then(Option::as_ref) {
        Some(ParameterValue::Flag(value)) => Ok(*value),
        None => Ok(false),
        _ => Err(ModelError::MissingDefaultDependency(slot)),
    }
}

fn check_selector_range(
    values: &[Option<ParameterValue>],
    name: &str,
    max: i32,
) -> Result<(), ModelError> {
    let value = model_integer(values, name)?;
    if !(0..=max).contains(&value) {
        return Err(ModelError::InvalidSelector {
            name: name.to_owned(),
            value,
        });
    }
    Ok(())
}

/// Apply source-accurate BSIMSOI 4.x setup defaults to a bound model card.
///
/// Assignment order mirrors `B4SOIsetup` in ngspice 47. Inputs override defaults;
/// aliases share a canonical slot. Model card `TNOM` is Celsius; `CKTnomTemp` is
/// Kelvin. Polarity and mobility-dependent defaults, legacy parameter precedence,
/// and overlap fringe calculation are evaluated once, not during Newton iterations.
///
/// # Errors
/// Fails for a wrong model family/scope, invalid nominal temperature or explicit
/// mode selector, conflicting polarity flags, or a missing derived dependency.
pub fn prepare_bsim_soi_model(
    source: &BoundParameters,
    nominal_temperature_kelvin: f64,
) -> Result<PreparedBsimSoiModel, ModelError> {
    if source.variant() != ModelVariant::BsimSoi || source.scope() != ParameterScope::Model {
        return Err(ModelError::WrongModelScope);
    }
    if !nominal_temperature_kelvin.is_finite() || nominal_temperature_kelvin <= 0.0 {
        return Err(ModelError::InvalidNominalTemperature);
    }
    let nmos = optional_flag(&source.values, "nmos")?;
    let pmos = optional_flag(&source.values, "pmos")?;
    if nmos && pmos {
        return Err(ModelError::ConflictingPolarity);
    }
    let polarity = if pmos {
        MosPolarity::Pmos
    } else {
        MosPolarity::Nmos
    };
    let mut values = source.values.clone();

    // The C frontend transforms an explicitly given TNOM in degrees Celsius.
    // The nominal circuit temperature, already Kelvin, is used only if absent.
    let tnom_slot = model_slot("tnom")?;
    if let Some(ParameterValue::Real(given)) = values[usize::from(tnom_slot)].as_mut() {
        *given += 273.15;
        if !given.is_finite() || *given <= 0.0 {
            return Err(ModelError::InvalidNominalTemperature);
        }
    }

    for rule in bsim_soi_rules::DEFAULT_RULES {
        let dst = usize::from(rule.slot);
        if values[dst].is_some() {
            continue;
        }
        let value = match rule.expression {
            DefaultExpression::Real(value) => ParameterValue::Real(value),
            DefaultExpression::Integer(value) => ParameterValue::Integer(value),
            DefaultExpression::Copy(slot) => values
                .get(usize::from(slot))
                .and_then(Option::as_ref)
                .cloned()
                .ok_or(ModelError::MissingDefaultDependency(slot))?,
            DefaultExpression::Polarity { nmos, pmos } => ParameterValue::Real(match polarity {
                MosPolarity::Nmos => nmos,
                MosPolarity::Pmos => pmos,
            }),
            DefaultExpression::MobilityThree { mode3, other } => {
                ParameterValue::Real(if model_integer(&values, "mobmod")? == 3 {
                    mode3
                } else {
                    other
                })
            }
            DefaultExpression::Legacy { old_slot, fallback } => {
                // Only an explicitly given legacy value affects the new parameter.
                let value = if let Some(ParameterValue::Real(value)) = source.get_slot(old_slot) {
                    *value
                } else {
                    fallback
                };
                ParameterValue::Real(value)
            }
            DefaultExpression::NominalTemperature => {
                ParameterValue::Real(nominal_temperature_kelvin)
            }
            DefaultExpression::Fringe { tox_slot } => {
                let tox = real(&values, tox_slot)?;
                if tox <= 0.0 {
                    return Err(ModelError::InvalidPhysicalParameter("tox"));
                }
                ParameterValue::Real(
                    2.0 * 3.453_133e-11 / std::f64::consts::PI * (0.4e-6 / tox).ln_1p(),
                )
            }
        };
        values[dst] = Some(value);
    }

    // These are the selectors for which ngspice explicitly performs range
    // coercion. Rather than silently switching physical laws, reject here.
    for (name, max) in [
        ("soimod", 3),
        ("rgatemod", 3),
        ("fnoimod", 1),
        ("tnoimod", 2),
        ("rdsmod", 1),
        ("rbodymod", 1),
    ] {
        check_selector_range(&values, name, max)?;
    }
    Ok(PreparedBsimSoiModel {
        polarity,
        original_given: source.values.iter().map(Option::is_some).collect(),
        parameters: BoundParameters {
            variant: ModelVariant::BsimSoi,
            scope: ParameterScope::Model,
            values,
            bsim_soi_ic: [None; 5],
        },
    })
}
