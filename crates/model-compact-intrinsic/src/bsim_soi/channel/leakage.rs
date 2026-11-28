//! Gate-induced drain and source leakage, in physical terminal order.
//! Copyright 2010 Regents of the University of California. See package LICENSE.
use super::super::{PreparedBsimSoiSetup, PreparedBsimSoiSize, binned, integer, positive, real};
use super::{BsimSoiChannelBias, BsimSoiChannelScalar, Jet, PreparedBsimSoiChannel};
use crate::ModelError;

/// Signed current into each physical junction from its respective terminal.
/// Includes finger count and instance multiplicity.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BsimSoiGateInducedLeakage {
    pub drain_to_body: BsimSoiChannelScalar,
    pub source_to_body: BsimSoiChannelScalar,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Side {
    width: f64,
    a: f64,
    b: f64,
    c: f64,
    e: f64,
    r: f64,
    k: f64,
    f: f64,
}

impl Side {
    fn evaluate(
        &self,
        alternate: bool,
        gate: Jet,
        drain: Jet,
        body: Jet,
        oxide: f64,
        flatband: f64,
    ) -> Jet {
        let gate_factor = if alternate { self.r } else { 1. };
        let field = (drain - gate_factor * gate - self.e + flatband) / oxide;
        if self.a <= 0.
            || self.b <= 0.
            || self.c < 0.
            || field.value <= 0.
            || (!alternate && body.value > 0.)
        {
            return Jet::constant(0.);
        }
        let argument = -self.b / field;
        let current = self.width * self.a * field * argument.floor_exp();
        if alternate {
            let difference = body - self.f;
            let correction = if difference.value == 0. {
                Jet::constant(2.688_117_142e43)
            } else {
                let exponent = self.k / difference;
                if exponent.value >= 100. {
                    Jet::constant(2.688_117_142e43)
                } else {
                    exponent.exp()
                }
            };
            current * correction
        } else if self.c >= 3.720_075_976e-44 {
            let cube = -body * body * body;
            current * cube / (self.c + cube)
        } else {
            current
        }
    }
}

/// Bound source-defined GIDL/GISL law, including asymmetric binned inputs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreparedBsimSoiGateInducedLeakage {
    channel: PreparedBsimSoiChannel,
    source: Side,
    drain: Side,
    alternate: bool,
    oxide: f64,
    flatband: f64,
    scale: f64,
}

impl PreparedBsimSoiGateInducedLeakage {
    /// Evaluate the two gate-induced leakage branches.
    /// # Errors
    /// Nonfinite physical bias or constitutive result.
    pub fn evaluate(
        &self,
        bias: BsimSoiChannelBias,
    ) -> Result<BsimSoiGateInducedLeakage, ModelError> {
        let point = self.channel.evaluate(bias)?;
        if self.channel.fd.is_some_and(|fd| fd.ideal) {
            return Ok(BsimSoiGateInducedLeakage {
                drain_to_body: Jet::constant(0.),
                source_to_body: Jet::constant(0.),
            });
        }
        let (source, drain) = if point.reversed {
            (&self.drain, &self.source)
        } else {
            (&self.source, &self.drain)
        };
        let source_current = source.evaluate(
            self.alternate,
            point.gate_drain_voltage,
            -point.drain_voltage,
            point.body_voltage,
            self.oxide,
            self.flatband,
        );
        let drain_current = drain.evaluate(
            self.alternate,
            point.gate_voltage,
            point.drain_voltage,
            point.body_voltage - point.drain_voltage,
            self.oxide,
            self.flatband,
        );
        let (physical_source, physical_drain) = if point.reversed {
            (drain_current, source_current)
        } else {
            (source_current, drain_current)
        };
        Ok(BsimSoiGateInducedLeakage {
            drain_to_body: (physical_drain * self.scale).checked("gate induced drain leakage")?,
            source_to_body: (physical_source * self.scale)
                .checked("gate induced source leakage")?,
        })
    }
}

fn side(size: &PreparedBsimSoiSize, suffix: &str, width: f64) -> Result<Side, ModelError> {
    let get = |prefix: &str| binned(size, &format!("{prefix}{suffix}"));
    Ok(Side {
        width,
        a: get("a")?,
        b: get("b")?,
        c: get("c")?,
        e: get("e")?,
        r: get("r")?,
        k: get("k")?,
        f: get("f")?,
    })
}

impl PreparedBsimSoiSetup {
    /// Bind gate-induced junction leakage for an instance.
    /// # Errors
    /// Missing model inputs or invalid setup geometry.
    pub fn prepare_gate_induced_leakage(
        &self,
        index: usize,
        ambient: f64,
    ) -> Result<PreparedBsimSoiGateInducedLeakage, ModelError> {
        let channel = self.prepare_channel(index, ambient)?;
        let model = self.model.parameters();
        let size = self
            .sizes
            .get(index)
            .ok_or(ModelError::MismatchedPreparedContext)?;
        let instance = self
            .instances
            .get(index)
            .ok_or(ModelError::InvalidPhysicalParameter(
                "BSIMSOI instance index",
            ))?
            .parameters();
        let geometry = size.geometry();
        let generalized = integer(model, "mtrlmod")? != 0;
        let (oxide, flatband) = if generalized {
            (
                real(model, "epsrsub")? * channel.threshold.toxe / 3.9,
                self.prepare_transport(index, ambient)?
                    .source_drain_flatband(),
            )
        } else {
            (
                3. * 3.9 * channel.threshold.toxe / real(model, "epsrox")?,
                0.,
            )
        };
        Ok(PreparedBsimSoiGateInducedLeakage {
            source: side(size, "gisl", geometry.source_diode_width)?,
            drain: side(size, "gidl", geometry.drain_diode_width)?,
            alternate: integer(model, "gidlmod")? != 0,
            oxide: positive(oxide, "gate induced leakage oxide")?,
            flatband,
            scale: channel.polarity * real(instance, "nf")? * real(instance, "m")?,
            channel,
        })
    }
}
