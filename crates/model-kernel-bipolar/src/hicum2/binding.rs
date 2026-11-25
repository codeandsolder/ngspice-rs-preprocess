//! Once-only HICUM level 8 model and Q instance semantic binding.
//!
//! The upstream parser owns SPICE tokenization, engineering suffixes,
//! expression resolution, source locations and initial-vector parsing.
//! This layer validates resolved source parameters in original setter
//! order and constructs typed immutable nominal/per-instance kernels.
//! No names or parser state appear in source Newton evaluation.

use super::{Polarity, instance, parameters, temperature};
use crate::{ModelError, Temperature};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Value<'a> {
    Scalar(f64),
    Text(&'a str),
    Flag,
    InitialVoltages(instance::InitialVoltages),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Assignment<'a> {
    pub name: &'a str,
    pub value: Value<'a>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Error {
    ModelType(String),
    Level(Option<f64>),
    Version(String),
    UnknownParameter {
        assignment: usize,
        name: String,
    },
    WrongKind {
        assignment: usize,
        name: String,
    },
    InvalidValue {
        assignment: usize,
        name: String,
        source: ModelError,
    },
    Instance(ModelError),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ModelType(name) => write!(f, "unsupported bipolar model type: {name}"),
            Self::Level(value) => write!(f, "HICUM/L2 requires model LEVEL=8, got {value:?}"),
            Self::Version(value) => write!(f, "unsupported HICUM/L2 source version: {value}"),
            Self::UnknownParameter { assignment, name } => {
                write!(f, "unrecognized HICUM parameter #{assignment}: {name}")
            }
            Self::WrongKind { assignment, name } => write!(
                f,
                "HICUM parameter #{assignment} has incompatible kind: {name}"
            ),
            Self::InvalidValue {
                assignment,
                name,
                source,
            } => write!(f, "invalid HICUM parameter #{assignment} {name}: {source}"),
            Self::Instance(source) => write!(f, "invalid HICUM instance: {source}"),
        }
    }
}
impl std::error::Error for Error {}

/// Bind an ngspice Q/BJT model with explicit LEVEL=8. The base
/// NPN/PNP polarity can be overridden by source TYPE or ordered
/// NPN/PNP flags, exactly once during model preparation.
///
/// # Errors
/// Unsupported model selectors and invalid source coefficient values
/// return original assignment indices for upstream location diagnostics.
pub fn model(
    base: &str,
    level: Option<f64>,
    assignments: &[Assignment<'_>],
) -> Result<temperature::Nominal, Error> {
    let default_polarity = if base.eq_ignore_ascii_case("npn") {
        Polarity::Npn
    } else if base.eq_ignore_ascii_case("pnp") {
        Polarity::Pnp
    } else {
        return Err(Error::ModelType(base.to_string()));
    };
    if level != Some(8.0) {
        return Err(Error::Level(level));
    }
    let mut bound = temperature::Nominal::source_defaults();
    bound.polarity = default_polarity;
    for (index, assignment) in assignments.iter().enumerate() {
        if assignment.name.eq_ignore_ascii_case("level") {
            match assignment.value {
                Value::Scalar(value) if value.to_bits() == 8.0f64.to_bits() => {}
                Value::Scalar(value) => return Err(Error::Level(Some(value))),
                _ => {
                    return Err(Error::WrongKind {
                        assignment: index,
                        name: assignment.name.to_string(),
                    });
                }
            }
        } else if assignment.name.eq_ignore_ascii_case("version") {
            let Value::Text(value) = assignment.value else {
                return Err(Error::WrongKind {
                    assignment: index,
                    name: assignment.name.to_string(),
                });
            };
            if value != "2.4.0" {
                return Err(Error::Version(value.to_string()));
            }
        } else if assignment.name.eq_ignore_ascii_case("type")
            && matches!(assignment.value, Value::Text(_))
        {
            let Value::Text(value) = assignment.value else {
                return Err(Error::WrongKind {
                    assignment: index,
                    name: assignment.name.to_string(),
                });
            };
            bound.polarity = if value.eq_ignore_ascii_case("NPN") {
                Polarity::Npn
            } else if value.eq_ignore_ascii_case("PNP") {
                Polarity::Pnp
            } else {
                return Err(Error::ModelType(value.to_string()));
            };
        } else if assignment.name.eq_ignore_ascii_case("npn")
            || assignment.name.eq_ignore_ascii_case("pnp")
        {
            if assignment.value != Value::Flag {
                return Err(Error::WrongKind {
                    assignment: index,
                    name: assignment.name.to_string(),
                });
            }
            bound.polarity = if assignment.name.eq_ignore_ascii_case("npn") {
                Polarity::Npn
            } else {
                Polarity::Pnp
            };
        } else {
            let id = parameters::Id::from_name(assignment.name)
                .or_else(|| {
                    assignment
                        .name
                        .eq_ignore_ascii_case("tref")
                        .then(|| parameters::Id::from_name("tnom"))
                        .flatten()
                })
                .ok_or_else(|| Error::UnknownParameter {
                    assignment: index,
                    name: assignment.name.to_string(),
                })?;
            let Value::Scalar(value) = assignment.value else {
                return Err(Error::WrongKind {
                    assignment: index,
                    name: assignment.name.to_string(),
                });
            };
            bound
                .assign_parameter(id, value)
                .map_err(|source| Error::InvalidValue {
                    assignment: index,
                    name: assignment.name.to_string(),
                    source,
                })?;
        }
    }
    Ok(bound)
}

/// Bind an instance of a prepared source model from Q-device settings.
/// Lexing, positional AREA normalization and IC-vector parsing belong
/// to the upstream SPICE parser and circuit elaborator.
///
/// # Errors
/// Invalid setting kinds/names preserve the original setter index;
/// invalid physical instance parameters are rejected before evaluation.
pub fn instance(
    model: &temperature::Nominal,
    global_ambient: Temperature,
    assignments: &[Assignment<'_>],
) -> Result<instance::Bound, Error> {
    let mut options = instance::Options::default();
    for (index, assignment) in assignments.iter().enumerate() {
        let name = assignment.name;
        if name.eq_ignore_ascii_case("area") {
            options.area = scalar(assignment, index)?;
        } else if name.eq_ignore_ascii_case("m") {
            options.multiplicity = scalar(assignment, index)?;
        } else if name.eq_ignore_ascii_case("temp") {
            options.temp_celsius = Some(scalar(assignment, index)?);
        } else if name.eq_ignore_ascii_case("dt") {
            options.dt_kelvin = scalar(assignment, index)?;
        } else if name.eq_ignore_ascii_case("off") {
            if assignment.value != Value::Flag {
                return Err(Error::WrongKind {
                    assignment: index,
                    name: name.to_string(),
                });
            }
            options.off = true;
        } else if name.eq_ignore_ascii_case("ic") {
            let Value::InitialVoltages(value) = assignment.value else {
                return Err(Error::WrongKind {
                    assignment: index,
                    name: name.to_string(),
                });
            };
            options.initial = value;
        } else {
            return Err(Error::UnknownParameter {
                assignment: index,
                name: name.to_string(),
            });
        }
    }
    instance::Bound::new(model, global_ambient, options).map_err(Error::Instance)
}

fn scalar(assignment: &Assignment<'_>, index: usize) -> Result<f64, Error> {
    let Value::Scalar(value) = assignment.value else {
        return Err(Error::WrongKind {
            assignment: index,
            name: assignment.name.to_string(),
        });
    };
    if !value.is_finite() {
        return Err(Error::InvalidValue {
            assignment: index,
            name: assignment.name.to_string(),
            source: ModelError::InvalidParameter("scalar"),
        });
    }
    Ok(value)
}
