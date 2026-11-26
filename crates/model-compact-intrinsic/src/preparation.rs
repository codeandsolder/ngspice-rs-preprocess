//! Shared typed parameter preparation operations.

use crate::{BoundParameters, ModelError, ParameterValue};

pub fn slot(p: &BoundParameters, name: &str) -> Result<u16, ModelError> {
    p.variant()
        .parameter(p.scope(), name)
        .map(|spec| spec.slot)
        .ok_or_else(|| ModelError::UnknownParameter(name.to_owned()))
}

pub fn value<'a>(p: &'a BoundParameters, name: &str) -> Result<&'a ParameterValue, ModelError> {
    let id = slot(p, name)?;
    p.get_slot(id)
        .ok_or(ModelError::MissingDefaultDependency(id))
}

pub fn real(p: &BoundParameters, name: &str) -> Result<f64, ModelError> {
    if let ParameterValue::Real(v) = value(p, name)? {
        Ok(*v)
    } else {
        Err(ModelError::MissingDefaultDependency(slot(p, name)?))
    }
}

pub fn integer(p: &BoundParameters, name: &str) -> Result<i32, ModelError> {
    if let ParameterValue::Integer(v) = value(p, name)? {
        Ok(*v)
    } else {
        Err(ModelError::MissingDefaultDependency(slot(p, name)?))
    }
}

pub fn given(p: &BoundParameters, name: &str) -> Result<bool, ModelError> {
    Ok(p.get_slot(slot(p, name)?).is_some())
}

pub fn flag(p: &BoundParameters, name: &str) -> Result<bool, ModelError> {
    Ok(matches!(
        p.get_slot(slot(p, name)?),
        Some(ParameterValue::Flag(true))
    ))
}

pub fn assign(p: &mut BoundParameters, name: &str, v: ParameterValue) -> Result<(), ModelError> {
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

pub fn default(p: &mut BoundParameters, name: &str, v: ParameterValue) -> Result<(), ModelError> {
    if !given(p, name)? {
        assign(p, name, v)?;
    }
    Ok(())
}

pub fn positive(v: f64, name: &'static str) -> Result<f64, ModelError> {
    if v.is_finite() && v > 0.0 {
        Ok(v)
    } else {
        Err(ModelError::InvalidPhysicalParameter(name))
    }
}
