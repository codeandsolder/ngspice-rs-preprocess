//! Compile already-normalized ngspice VDMOS model parameters into an immutable
//! prepared device, without any source-name lookup at the evaluation point.

use super::{Breakdown, Model, ModelError, Polarity, PreparedModel, QuasiSaturation};

/// Error compiling a normalized ngspice VDMOS `.model` parameter set.
#[derive(Debug, PartialEq, Eq)]
pub enum ParameterError {
    /// The source field is not recognized as a ngspice 47 VDMOS parameter.
    Unknown(String),
    /// A read-only model query cannot be supplied as an input field.
    ReadOnly(String),
    /// A pair/group of parameters must be specified together.
    Incomplete(&'static str),
    /// The compiled typed model failed its semantic validation.
    Invalid(ModelError),
}

impl std::fmt::Display for ParameterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown(name) => write!(f, "unknown VDMOS parameter: {name}"),
            Self::ReadOnly(name) => write!(f, "VDMOS parameter is read-only: {name}"),
            Self::Incomplete(name) => write!(f, "incomplete VDMOS parameter group: {name}"),
            Self::Invalid(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for ParameterError {}

impl From<ModelError> for ParameterError {
    fn from(error: ModelError) -> Self {
        Self::Invalid(error)
    }
}

/// Convert semantic source parameter pairs into a prepared native VDMOS model.
///
/// The caller is responsible for parsing `.model` grammar and resolving SPICE
/// numeric suffixes/expressions. This function owns the ngspice 47 parameter
/// **meanings**, aliases, temperature units and grouped-parameter activation.
/// Fields are case-insensitive. Explicit unknown or read-only parameters
/// fail instead of losing their physical effects silently.
///
/// # Errors
///
/// Rejects unknown, read-only, incomplete or physically invalid parameters.
pub fn compile_ngspice<'a>(
    polarity: Polarity,
    parameters: impl IntoIterator<Item = (&'a str, f64)>,
) -> Result<PreparedModel, ParameterError> {
    let mut model = Model::defaults(polarity);
    let mut grouped = GroupedParameters::default();
    for (original, value) in parameters {
        if !value.is_finite() {
            return Err(ParameterError::Invalid(ModelError::Invalid(
                "model parameter",
            )));
        }
        let name = original.to_ascii_lowercase();
        if name == "tnom" {
            model.soa.tnom_given = true;
        }
        if name == "phi" {
            model.phi = value;
            continue;
        }
        if model.soa.set(&name, value)
            || grouped.set(&name, value)
            || set_channel(&mut model, &name, value)
            || set_gate(&mut model, &name, value)
            || set_diode(&mut model, &name, value)
            || set_series(&mut model, &name, value)
            || set_thermal_and_noise(&mut model, &name, value)
        {
            continue;
        }
        if name == "type" {
            return Err(ParameterError::ReadOnly(original.to_owned()));
        }
        return Err(ParameterError::Unknown(original.to_owned()));
    }
    grouped.finish(&mut model)?;
    Ok(model.prepare()?)
}

#[derive(Default)]
struct GroupedParameters {
    rq: Option<f64>,
    vq: Option<f64>,
    bv: Option<f64>,
    ibv: Option<f64>,
    nbv: Option<f64>,
}

impl GroupedParameters {
    fn set(&mut self, name: &str, value: f64) -> bool {
        let destination = match name {
            "rq" => &mut self.rq,
            "vq" => &mut self.vq,
            "bv" => &mut self.bv,
            "ibv" => &mut self.ibv,
            "nbv" => &mut self.nbv,
            _ => return false,
        };
        *destination = Some(value);
        true
    }

    fn finish(self, model: &mut Model) -> Result<(), ParameterError> {
        match (self.rq, self.vq) {
            (Some(rq), Some(vq)) => {
                model.series.quasi_saturation = Some(QuasiSaturation { rq, vq });
            }
            (None, None) => {}
            _ => return Err(ParameterError::Incomplete("rq/vq")),
        }
        match self.bv {
            Some(bv) => {
                model.diode.breakdown = Some(Breakdown {
                    bv,
                    ibv: self.ibv.unwrap_or(1e-10),
                    nbv: self.nbv.unwrap_or(1.0),
                });
            }
            None if self.ibv.is_none() && self.nbv.is_none() => {}
            None => return Err(ParameterError::Incomplete("bv/ibv/nbv")),
        }
        Ok(())
    }
}

fn set_channel(model: &mut Model, name: &str, value: f64) -> bool {
    let target = match name {
        "vto" | "vth0" => &mut model.channel.vth,
        "kp" => &mut model.channel.kp,
        "lambda" => &mut model.channel.lambda,
        "theta" => &mut model.channel.theta,
        "mtriode" => &mut model.channel.mtriode,
        "subshift" => &mut model.channel.subshift,
        "ksubthres" => &mut model.channel.ksubthres,
        "tksubthres1" => &mut model.channel.tksubthres1,
        "tksubthres2" => &mut model.channel.tksubthres2,
        "tcvth" | "vtotc" => &mut model.channel.tcvth,
        "mu" | "bex" => &mut model.channel.mu,
        _ => return false,
    };
    *target = value;
    true
}

fn set_gate(model: &mut Model, name: &str, value: f64) -> bool {
    let target = match name {
        "cgdmin" => &mut model.gate.cgdmin,
        "cgdmax" => &mut model.gate.cgdmax,
        "cgs" => &mut model.gate.cgs,
        "a" => &mut model.gate.a,
        _ => return false,
    };
    *target = value;
    true
}

fn set_diode(model: &mut Model, name: &str, value: f64) -> bool {
    if name == "is" {
        // ngspice47 vdmosset floors *explicit* Is=0 (and all values
        // below CKTepsmin) to 1e-28 before instance preparation. The
        // physical kernel still validates its positive saturation current.
        // C references: vdmosset.c:250-252, cktinit.c:97.
        model.diode.is = value.max(1e-28);
        return true;
    }
    // The source compiler implements ngspice's explicit setup bounds.
    // Typed Model::prepare continues to validate direct physical input.
    if name == "eg" {
        model.diode.eg = value.max(0.1);
        return true;
    }
    if name == "fc" {
        model.diode.fc = value.min(0.95);
        return true;
    }
    let target = match name {
        "n" => &mut model.diode.n,
        "cjo" => &mut model.diode.cjo,
        "vj" => &mut model.diode.vj,
        "m" => &mut model.diode.m,
        "tt" => &mut model.diode.tt,
        "vp" => &mut model.diode.vp,
        "xti" => &mut model.diode.xti,
        "trb1" => &mut model.diode.trb1,
        "trb2" => &mut model.diode.trb2,
        _ => return false,
    };
    *target = value;
    true
}

fn set_series(model: &mut Model, name: &str, value: f64) -> bool {
    let target = match name {
        "rd" => &mut model.series.rd,
        "rs" => &mut model.series.rs,
        "rg" => &mut model.series.rg,
        "rb" => &mut model.series.rb,
        "rds" => &mut model.series.rds,
        "texp0" => {
            model.series.texp0 = Some(value);
            return true;
        }
        "texp1" => &mut model.series.texp1,
        "trd1" => &mut model.series.trd1,
        "trd2" => &mut model.series.trd2,
        "trg1" => &mut model.series.trg1,
        "trg2" => &mut model.series.trg2,
        "trs1" => &mut model.series.trs1,
        "trs2" => &mut model.series.trs2,
        _ => return false,
    };
    // ngspice 47 allocates a series-resistor internal node only for
    // strictly positive Rd/Rs/Rg/Rb. A nonpositive catalog value means
    // the branch is absent, not a negative physical resistor.
    *target = if matches!(name, "rd" | "rs" | "rg" | "rb") {
        value.max(0.0)
    } else {
        value
    };
    true
}

fn set_thermal_and_noise(model: &mut Model, name: &str, value: f64) -> bool {
    let target = match name {
        "tnom" => &mut model.tnom,
        "rthjc" => &mut model.thermal.rthjc,
        "rthca" => &mut model.thermal.rthca,
        "cthj" => &mut model.thermal.cthj,
        "kf" => &mut model.noise.kf,
        "af" => &mut model.noise.af,
        _ => return false,
    };
    *target = if name == "tnom" {
        value + 273.15
    } else {
        value
    };
    true
}
