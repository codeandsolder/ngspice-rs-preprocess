//! VBIC 1.1.5 forward/reverse transport and high-injection base-charge laws.
//!
//! Implements Ifi, Iri, q1, q2, qb, Itzf and Itzr from the ngspice VBIC
//! model. Depletion charges and derivatives must be computed by their own
//! physical laws, not silently omitted for nonzero model junction capacitance.

use super::temperature::{self, Coefficient as C};
use crate::{
    EvaluationError, ModelError, Temperature, evaluate_lanes, validate_nonnegative,
    validate_positive,
};

/// QBM selects one of two VBIC base-charge equations (ngspice model keyword).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BaseChargeForm {
    Coupled,
    Factorized,
}

/// Temperature-adjusted transport parameters. Zero VEF/VER/IKF/IKR disables
/// the corresponding term; ISRR controls reverse transport independently.
#[derive(Clone, Copy, Debug)]
pub struct Parameters {
    pub is: f64,
    pub nf: f64,
    pub nr: f64,
    pub isrr: f64,
    pub vef: f64,
    pub ver: f64,
    pub ikf: f64,
    pub ikr: f64,
    pub nkf: f64,
    pub base_charge_form: BaseChargeForm,
}

/// Current charges and their voltage derivatives must be supplied together.
#[derive(Clone, Copy, Debug, Default)]
pub struct Depletion {
    /// Normalized B-E junction integral, units of volts, NOT coulombs.
    pub qdbe: f64,
    /// Derivative of qdbe wrt intrinsic Vbei, dimensionless.
    pub cbe: f64,
    /// Normalized B-C junction integral, units of volts, NOT coulombs.
    pub qdbc: f64,
    /// Derivative of qdbc wrt intrinsic Vbci, dimensionless.
    pub cbc: f64,
    pub d_qdbe_temperature: f64,
    pub d_qdbc_temperature: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct Bias {
    pub vbei: f64,
    pub vbci: f64,
    pub depletion: Depletion,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ValueAndGradient {
    pub value: f64,
    pub d_vbei: f64,
    pub d_vbci: f64,
    pub d_temperature: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Output {
    /// Unnormalized minority-carrier source Ifi, used by diffusion charge.
    pub ideal_forward: ValueAndGradient,
    /// Unnormalized minority-carrier source Iri, used by reverse diffusion charge.
    pub ideal_reverse: ValueAndGradient,
    /// Smoothed Early-effect depletion factor q1, before high injection.
    pub q1: ValueAndGradient,
    pub forward: ValueAndGradient,
    pub reverse: ValueAndGradient,
    pub net: ValueAndGradient,
    pub qb: ValueAndGradient,
}

#[derive(Clone, Copy, Debug)]
pub struct Prepared {
    params: Parameters,
    vt: Gradient,
    saturation: Gradient,
    nf: Gradient,
    nr: Gradient,
    isrr: Gradient,
    inverse_vef: f64,
    inverse_ver: f64,
    inverse_ikf: Gradient,
    inverse_ikr: f64,
}

impl Prepared {
    /// # Errors
    /// Rejects invalid transport model parameters before runtime evaluation.
    pub fn new(parameters: Parameters, temperature: Temperature) -> Result<Self, ModelError> {
        for (name, value) in [
            ("is", parameters.is),
            ("isrr", parameters.isrr),
            ("vef", parameters.vef),
            ("ver", parameters.ver),
            ("ikf", parameters.ikf),
            ("ikr", parameters.ikr),
        ] {
            validate_nonnegative(name, value)?;
        }
        for (name, value) in [
            ("nf", parameters.nf),
            ("nr", parameters.nr),
            ("nkf", parameters.nkf),
        ] {
            validate_positive(name, value)?;
        }
        let reciprocal = |value: f64| if value == 0.0 { 0.0 } else { 1.0 / value };
        let prepared = Self {
            params: parameters,
            vt: Gradient::constant(temperature::thermal_voltage(temperature)),
            saturation: Gradient::constant(parameters.is),
            nf: Gradient::constant(parameters.nf),
            nr: Gradient::constant(parameters.nr),
            isrr: Gradient::constant(parameters.isrr),
            inverse_vef: reciprocal(parameters.vef),
            inverse_ver: reciprocal(parameters.ver),
            inverse_ikf: Gradient::constant(reciprocal(parameters.ikf)),
            inverse_ikr: reciprocal(parameters.ikr),
        };
        if [
            prepared.inverse_vef,
            prepared.inverse_ver,
            prepared.inverse_ikf.value,
            prepared.inverse_ikr,
        ]
        .iter()
        .any(|value| !value.is_finite())
        {
            return Err(ModelError::InvalidParameter(
                "reciprocal transport parameter",
            ));
        }
        Ok(prepared)
    }

    pub(super) fn from_temperature(t: &temperature::Prepared) -> Result<Self, ModelError> {
        let mut prepared = Self::new(t.transport_parameters(), t.temperature())?;
        prepared.vt = t.voltage_jet();
        prepared.saturation = t.jet(C::Is);
        prepared.nf = t.jet(C::Nf);
        prepared.nr = t.jet(C::Nr);
        prepared.isrr = t.jet(C::Isrr);
        prepared.inverse_ikf = temperature::inverse_enabled(t.jet(C::Ikf));
        Ok(prepared)
    }

    /// # Errors
    /// Rejects nonfinite input or output and nonphysical base-charge domains.
    pub fn evaluate(&self, bias: Bias) -> Result<Output, EvaluationError> {
        let depletion = bias.depletion;
        if ![
            bias.vbei,
            bias.vbci,
            depletion.qdbe,
            depletion.cbe,
            depletion.qdbc,
            depletion.cbc,
            depletion.d_qdbe_temperature,
            depletion.d_qdbc_temperature,
        ]
        .iter()
        .all(|v| v.is_finite())
        {
            return Err(EvaluationError::NonFiniteBias);
        }
        let p = self.params;
        let emitter_voltage = Gradient::emitter(bias.vbei);
        let collector_voltage = Gradient::collector(bias.vbci);
        let forward = (emitter_voltage / (self.nf * self.vt)).exp_m1() * self.saturation;
        let reverse =
            (collector_voltage / (self.nr * self.vt)).exp_m1() * (self.saturation * self.isrr);
        let be = Gradient::new(depletion.qdbe, depletion.cbe, 0.0, 0.0)
            .with_temperature(depletion.d_qdbe_temperature);
        let bc = Gradient::new(depletion.qdbc, 0.0, depletion.cbc, 0.0)
            .with_temperature(depletion.d_qdbc_temperature);

        let q1z = Gradient::constant(1.0) + be * self.inverse_ver + bc * self.inverse_vef;
        let centered = q1z - Gradient::constant(1e-4);
        let q1 = ((centered * centered + Gradient::constant(1e-8)).checked_sqrt()? + centered)
            * 0.5
            + Gradient::constant(1e-4);
        let q2 = forward * self.inverse_ikf + reverse * self.inverse_ikr;
        let qb = match p.base_charge_form {
            BaseChargeForm::Coupled => {
                let radicand = q1.checked_powf(1.0 / p.nkf)? + q2 * 4.0;
                (q1 + radicand.checked_powf(p.nkf)?) * 0.5
            }
            BaseChargeForm::Factorized => {
                let injection = (Gradient::constant(1.0) + q2 * 4.0).checked_powf(p.nkf)?;
                q1 * (Gradient::constant(1.0) + injection) * 0.5
            }
        };
        if qb.value <= 0.0 {
            return Err(EvaluationError::InvalidPhysicalDomain);
        }
        let forward_current = forward / qb;
        let reverse_current = reverse / qb;
        let net = forward_current - reverse_current;
        let output = Output {
            ideal_forward: forward.into(),
            ideal_reverse: reverse.into(),
            q1: q1.into(),
            forward: forward_current.into(),
            reverse: reverse_current.into(),
            net: net.into(),
            qb: qb.into(),
        };
        if ![
            forward,
            reverse,
            q1,
            forward_current,
            reverse_current,
            net,
            qb,
        ]
        .into_iter()
        .all(Gradient::is_finite)
        {
            return Err(EvaluationError::NonFiniteResult);
        }
        Ok(output)
    }

    /// All slice lengths are checked before any writes. Lanes run in order;
    /// the first error leaves that lane and the remaining outputs unchanged.
    ///
    /// # Errors
    /// Reports mismatched input/output lengths or any invalid device evaluation.
    pub fn evaluate_batch(
        &self,
        inputs: &[Bias],
        outputs: &mut [Output],
    ) -> Result<(), EvaluationError> {
        evaluate_lanes(inputs.iter().copied(), outputs, |bias| self.evaluate(bias))
    }
}

pub(super) use crate::hicum2::transport::jet::Jet as Gradient;

pub(super) trait CheckedGradient: Sized {
    fn checked_ln(self) -> Result<Self, EvaluationError>;
    fn checked_sqrt(self) -> Result<Self, EvaluationError>;
    fn checked_powf(self, exponent: f64) -> Result<Self, EvaluationError>;
}

impl CheckedGradient for Gradient {
    fn checked_ln(self) -> Result<Self, EvaluationError> {
        if self.value <= 0.0 {
            return Err(EvaluationError::InvalidPhysicalDomain);
        }
        Ok(self.ln())
    }
    fn checked_sqrt(self) -> Result<Self, EvaluationError> {
        if self.value <= 0.0 {
            return Err(EvaluationError::InvalidPhysicalDomain);
        }
        Ok(self.sqrt())
    }
    fn checked_powf(self, exponent: f64) -> Result<Self, EvaluationError> {
        if self.value <= 0.0 {
            return Err(EvaluationError::InvalidPhysicalDomain);
        }
        Ok(self.powf(exponent))
    }
}

impl From<Gradient> for ValueAndGradient {
    fn from(value: Gradient) -> Self {
        Self {
            value: value.value,
            d_vbei: value.be,
            d_vbci: value.bc,
            d_temperature: value.temperature,
        }
    }
}
