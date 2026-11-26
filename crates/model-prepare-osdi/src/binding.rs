//! Bind explicit parameter values once to compiler-defined ABI ordinals.

use crate::schema::{ModuleSchema, Parameter, ParameterKind, ValueType};
use anyhow::{Context, Result, bail, ensure};
use std::collections::{BTreeMap, btree_map::Entry};

/// Model or instance assignment context. Operating-point slots are read-only.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Scope {
    Model,
    Instance,
}

/// A scalar in the ABI representation (integer is signed 32-bit).
#[derive(Clone, Debug, PartialEq)]
pub enum Scalar {
    Real(f64),
    Integer(i32),
    String(String),
}

/// Keep scalar and vector shape distinct, including single-element vectors.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Scalar(Scalar),
    Array(Vec<Scalar>),
}

/// User-specified value with its original name or alias.
#[derive(Clone, Debug, PartialEq)]
pub struct Assignment {
    pub name: String,
    pub value: Value,
}

/// Bound, validated values. Omitted slots are left for compiler setup defaults.
#[derive(Clone, Debug, PartialEq)]
pub struct BoundParameters {
    digest: [u8; 32],
    descriptor: u32,
    scope: Scope,
    values: BTreeMap<u32, Value>,
}

impl BoundParameters {
    /// Identity of the exact binary that defines these ordinals.
    #[must_use = "the digest identifies the exact compiled artifact"]
    pub const fn digest(&self) -> &[u8; 32] {
        &self.digest
    }

    /// Ordinal of the selected device descriptor in the artifact.
    #[must_use = "the ordinal selects the bound descriptor"]
    pub const fn descriptor(&self) -> u32 {
        self.descriptor
    }

    /// Model or instance assignment namespace.
    #[must_use = "the namespace determines which setup operation receives the values"]
    pub const fn scope(&self) -> Scope {
        self.scope
    }

    /// Ordered parameter ordinals and values for one setup operation.
    pub fn values(&self) -> impl Iterator<Item = (u32, &Value)> {
        self.values.iter().map(|(&id, value)| (id, value))
    }
}

fn validate_scalar(scalar: &Scalar, kind: ValueType) -> Result<()> {
    match (scalar, kind) {
        (Scalar::Real(real), ValueType::Real) => {
            ensure!(real.is_finite(), "nonfinite OSDI real parameter");
        }
        (Scalar::Integer(_), ValueType::Integer) => {}
        (Scalar::String(string), ValueType::String) => {
            ensure!(!string.contains('\0'), "OSDI string contains NUL");
        }
        _ => bail!("OSDI parameter value type mismatch"),
    }
    Ok(())
}

fn validate_value(value: &Value, parameter: &Parameter) -> Result<()> {
    match value {
        Value::Scalar(scalar) => {
            ensure!(parameter.array_len == 0, "OSDI parameter requires an array");
            validate_scalar(scalar, parameter.value_type)?;
        }
        Value::Array(array) => {
            ensure!(
                parameter.array_len != 0 && array.len() == usize::try_from(parameter.array_len)?,
                "OSDI parameter array length mismatch"
            );
            for scalar in array {
                validate_scalar(scalar, parameter.value_type)?;
            }
        }
    }
    Ok(())
}

fn accessible(kind: ParameterKind, scope: Scope) -> bool {
    match scope {
        Scope::Model => kind == ParameterKind::Model,
        Scope::Instance => kind == ParameterKind::Instance,
    }
}

impl ModuleSchema {
    /// Bind names and aliases to stable ordinals, without guessing defaults.
    ///
    /// Name matching uses SPICE case insensitivity; ambiguous aliases fail.
    ///
    /// # Errors
    /// Unknown descriptor or parameter, ambiguous name, duplicate ordinal,
    /// inaccessible parameter, invalid representation or array shape.
    pub fn bind(
        &self,
        descriptor: u32,
        scope: Scope,
        assignments: Vec<Assignment>,
    ) -> Result<BoundParameters> {
        let device = self
            .devices()
            .get(usize::try_from(descriptor)?)
            .context("unknown OSDI descriptor")?;
        // Multiple spellings of the same slot are unambiguous; collisions
        // between distinct slots are retained as an explicit error.
        let mut names = BTreeMap::<String, Option<&Parameter>>::new();
        for parameter in device
            .parameters
            .iter()
            .filter(|p| accessible(p.kind, scope))
        {
            for name in &parameter.names {
                match names.entry(name.to_ascii_lowercase()) {
                    Entry::Vacant(entry) => {
                        entry.insert(Some(parameter));
                    }
                    Entry::Occupied(mut entry) => {
                        if entry.get().is_some_and(|other| other.id != parameter.id) {
                            entry.insert(None);
                        }
                    }
                }
            }
        }
        let mut values = BTreeMap::new();
        for Assignment { name, value } in assignments {
            let parameter = names
                .get(&name.to_ascii_lowercase())
                .with_context(|| format!("unknown or inaccessible OSDI parameter {name:?}"))?
                .with_context(|| format!("ambiguous OSDI parameter {name:?}"))?;
            validate_value(&value, parameter)
                .with_context(|| format!("binding OSDI parameter {name:?}"))?;
            ensure!(
                values.insert(parameter.id, value).is_none(),
                "duplicate OSDI parameter {name:?}"
            );
        }
        Ok(BoundParameters {
            digest: self.envelope().sha256,
            descriptor,
            scope,
            values,
        })
    }
}
