//! FD/DD-specific impact ionization, GIDL and body-contact transport.
//! Copyright 1999 Regents of the University of California. See package LICENSE.

use super::super::{
    Context, Transport, channel_threshold::ThresholdGeometry, channel_transport::TransportPoint,
};
use super::{D, DepletedValue, InternalBody, Trial};
use crate::ModelError;

/// Normalized leakage and body-contact constitutive terms of FD/DD.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DepletedLeakage {
    /// Impact-ionization current from drain to body.
    pub impact: DepletedValue,
    /// Gate-induced drain-to-body leakage.
    pub drain_gidl: DepletedValue,
    /// Gate-induced source-to-body leakage.
    pub source_gidl: DepletedValue,
    /// Internal body-to-contact current. Stamp only for a distinct resistive
    /// body-contact branch; floating and ideal-tie topology omit this branch.
    pub body_contact: DepletedValue,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Leakage {
    alpha: f64,
    beta: f64,
    impact: [f64; 3],
    custom_impact: bool,
    delta: f64,
    gidl: [f64; 3],
    body_resistance: f64,
    external_resistance: f64,
    resistive_contact: bool,
}

const MIN_EXP: f64 = 1.713_908_431e-15;

impl Leakage {
    pub(super) fn prepare(c: &Context<'_>, transport: Transport) -> Result<Self, ModelError> {
        Ok(Self {
            alpha: c
                .binned("alpha0")?
                .mul_add(c.instance.geometry().length_m.recip(), c.binned("alpha1")?),
            beta: c.binned("beta0")?,
            impact: [
                c.binned("bii")?
                    .mul_add(c.instance.geometry().length_m.recip(), c.binned("aii")?),
                c.binned("cii")?,
                c.binned("dii")?,
            ],
            custom_impact: c.binned("aii")? > 0.0,
            delta: c.binned("delta")?,
            gidl: [c.binned("agidl")?, c.binned("bgidl")?, c.binned("ngidl")?],
            body_resistance: transport.body_resistance,
            external_resistance: c.instance("nrb")? * c.model("rbsh")?,
            resistive_contact: c.model("rbody")? != 0.0 || c.model("rbsh")? != 0.0,
        })
    }

    fn impact(&self, trial: &Trial, drive: D, t: &TransportPoint<6>) -> D {
        if self.alpha <= 0.0 || self.beta <= 0.0 {
            return D::constant(0.0);
        }
        let [aii, cii, dii] = self.impact;
        let vsat = if self.custom_impact {
            let coefficient = if cii == 0.0 {
                D::constant(1.0)
            } else {
                let limit = cii / 3.0_f64.sqrt() + dii;
                let v = trial.drain - D::constant(limit + 0.1);
                let bounded =
                    D::constant(limit - dii) + (v + (v * v + D::constant(0.4)).sqrt()).scale(0.5);
                let ratio = D::constant(cii) / bounded;
                D::constant(1.0) + ratio * ratio
            };
            t.field_length * drive / (coefficient * drive + t.field_length.scale(aii))
        } else {
            t.saturation
        };
        let v = vsat - trial.drain - D::constant(self.delta);
        let root = (v * v + vsat.scale(4.0 * self.delta)).sqrt();
        let effective = vsat - (v + root).scale(0.5);
        let excess = trial.drain - effective;
        let exp = if excess.value > self.beta / 34.0 {
            (D::constant(-self.beta) / excess).exp()
        } else {
            D::constant(MIN_EXP)
        };
        t.current * excess * exp.scale(self.alpha)
    }

    fn gidl(&self, voltage: D, geometry: &ThresholdGeometry) -> D {
        let [a, b, n] = self.gidl;
        let field = (voltage - D::constant(n)).scale((3.0 * geometry.tox).recip());
        if a <= 0.0 || b <= 0.0 || field.value <= 0.0 {
            return D::constant(0.0);
        }
        let exponent = D::constant(-b) / field;
        let exp = if exponent.value > -34.0 {
            exponent.exp()
        } else {
            D::constant(MIN_EXP)
        };
        (field * exp).scale(geometry.width * a)
    }

    fn body_contact(&self, body: &InternalBody) -> D {
        if self.body_resistance < 1e-30 {
            return body
                .contact
                .scale(self.external_resistance.max(1e-30).recip());
        }
        let carrier = if body.contact.value >= 0.0 {
            body.coupling
        } else {
            body.contact_coupling
        };
        let conductance = carrier.sqrt().scale(self.body_resistance.recip());
        body.contact * conductance
            / (D::constant(1.0) + conductance.scale(self.external_resistance))
    }

    pub(super) fn evaluate(
        &self,
        fd: bool,
        geometry: &ThresholdGeometry,
        trial: &Trial,
        body: &InternalBody,
        drive: D,
        t: &TransportPoint<6>,
    ) -> Result<DepletedLeakage, ModelError> {
        let zero = DepletedValue::from_d(D::constant(0.0))?;
        if fd {
            return Ok(DepletedLeakage {
                impact: zero,
                drain_gidl: zero,
                source_gidl: zero,
                body_contact: zero,
            });
        }
        Ok(DepletedLeakage {
            impact: DepletedValue::from_d(self.impact(trial, drive, t))?,
            drain_gidl: DepletedValue::from_d(self.gidl(trial.drain - trial.gate, geometry))?,
            source_gidl: DepletedValue::from_d(self.gidl(-trial.gate, geometry))?,
            body_contact: if self.resistive_contact {
                DepletedValue::from_d(self.body_contact(body))?
            } else {
                zero
            },
        })
    }
}
