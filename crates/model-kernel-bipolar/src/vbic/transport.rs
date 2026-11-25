//! VBIC 1.1.5 forward/reverse transport and high-injection base-charge laws.
//!
//! Implements Ifi, Iri, q1, q2, qb, Itzf and Itzr from the ngspice VBIC
//! model. Depletion charges and derivatives must be computed by their own
//! physical laws, not silently omitted for nonzero model junction capacitance.

use crate::{
    EvaluationError, ModelError, Temperature, validate_batch_lengths, validate_nonnegative,
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
    vt: f64,
    inverse_vef: f64,
    inverse_ver: f64,
    inverse_ikf: f64,
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
            vt: (1.380_662e-23 / 1.602_189e-19) * temperature.kelvin(),
            inverse_vef: reciprocal(parameters.vef),
            inverse_ver: reciprocal(parameters.ver),
            inverse_ikf: reciprocal(parameters.ikf),
            inverse_ikr: reciprocal(parameters.ikr),
        };
        if [
            prepared.inverse_vef,
            prepared.inverse_ver,
            prepared.inverse_ikf,
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
        ]
        .iter()
        .all(|v| v.is_finite())
        {
            return Err(EvaluationError::NonFiniteBias);
        }
        let p = self.params;
        let emitter_voltage = Gradient::new(bias.vbei, 1.0, 0.0);
        let collector_voltage = Gradient::new(bias.vbci, 0.0, 1.0);
        let forward = (emitter_voltage / (p.nf * self.vt)).exp_m1() * p.is;
        let reverse = (collector_voltage / (p.nr * self.vt)).exp_m1() * (p.is * p.isrr);
        let be = Gradient::new(depletion.qdbe, depletion.cbe, 0.0);
        let bc = Gradient::new(depletion.qdbc, 0.0, depletion.cbc);

        let q1z = Gradient::constant(1.0) + be * self.inverse_ver + bc * self.inverse_vef;
        let centered = q1z - Gradient::constant(1e-4);
        let q1 = ((centered * centered + Gradient::constant(1e-8)).sqrt()? + centered) * 0.5
            + Gradient::constant(1e-4);
        let q2 = forward * self.inverse_ikf + reverse * self.inverse_ikr;
        let qb = match p.base_charge_form {
            BaseChargeForm::Coupled => {
                let radicand = q1.powf(1.0 / p.nkf)? + q2 * 4.0;
                (q1 + radicand.powf(p.nkf)?) * 0.5
            }
            BaseChargeForm::Factorized => {
                let injection = (Gradient::constant(1.0) + q2 * 4.0).powf(p.nkf)?;
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
            output.ideal_forward.value,
            output.ideal_forward.d_vbei,
            output.ideal_forward.d_vbci,
            output.ideal_reverse.value,
            output.ideal_reverse.d_vbei,
            output.ideal_reverse.d_vbci,
            output.q1.value,
            output.q1.d_vbei,
            output.q1.d_vbci,
            output.forward.value,
            output.forward.d_vbei,
            output.forward.d_vbci,
            output.reverse.value,
            output.reverse.d_vbei,
            output.reverse.d_vbci,
            output.net.value,
            output.net.d_vbei,
            output.net.d_vbci,
            output.qb.value,
            output.qb.d_vbei,
            output.qb.d_vbci,
        ]
        .iter()
        .all(|v| v.is_finite())
        {
            return Err(EvaluationError::NonFiniteResult);
        }
        Ok(output)
    }

    /// # Errors
    /// Reports mismatched input/output lengths or any invalid device evaluation.
    pub fn evaluate_batch(
        &self,
        inputs: &[Bias],
        outputs: &mut [Output],
    ) -> Result<(), EvaluationError> {
        validate_batch_lengths(inputs.len(), outputs.len())?;
        for (bias, output) in inputs.iter().zip(outputs) {
            *output = self.evaluate(*bias)?;
        }
        Ok(())
    }
}

/// Two-dimensional analytic forward differentiation without a runtime tape.
#[derive(Clone, Copy, Debug)]
pub(super) struct Gradient {
    pub(super) value: f64,
    pub(super) emitter_slope: f64,
    pub(super) collector_slope: f64,
}

impl Gradient {
    pub(super) const fn new(value: f64, emitter_slope: f64, collector_slope: f64) -> Self {
        Self {
            value,
            emitter_slope,
            collector_slope,
        }
    }
    pub(super) const fn constant(value: f64) -> Self {
        Self::new(value, 0.0, 0.0)
    }
    pub(super) fn exp(self) -> Self {
        let value = self.value.exp();
        Self::new(
            value,
            value * self.emitter_slope,
            value * self.collector_slope,
        )
    }
    pub(super) fn exp_m1(self) -> Self {
        let slope = self.value.exp();
        Self::new(
            self.value.exp_m1(),
            slope * self.emitter_slope,
            slope * self.collector_slope,
        )
    }
    pub(super) fn ln(self) -> Result<Self, EvaluationError> {
        if self.value <= 0.0 {
            return Err(EvaluationError::InvalidPhysicalDomain);
        }
        let inv = 1.0 / self.value;
        Ok(Self::new(
            self.value.ln(),
            self.emitter_slope * inv,
            self.collector_slope * inv,
        ))
    }
    pub(super) fn sqrt(self) -> Result<Self, EvaluationError> {
        if self.value <= 0.0 {
            return Err(EvaluationError::InvalidPhysicalDomain);
        }
        let root = self.value.sqrt();
        Ok(Self::new(
            root,
            self.emitter_slope * (0.5 / root),
            self.collector_slope * (0.5 / root),
        ))
    }
    pub(super) fn powf(self, power: f64) -> Result<Self, EvaluationError> {
        if self.value <= 0.0 {
            return Err(EvaluationError::InvalidPhysicalDomain);
        }
        let value = self.value.powf(power);
        let slope = value * power / self.value;
        Ok(Self::new(
            value,
            slope * self.emitter_slope,
            slope * self.collector_slope,
        ))
    }
}

impl From<Gradient> for ValueAndGradient {
    fn from(value: Gradient) -> Self {
        Self {
            value: value.value,
            d_vbei: value.emitter_slope,
            d_vbci: value.collector_slope,
        }
    }
}

impl std::ops::Add for Gradient {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        Self::new(
            self.value + rhs.value,
            self.emitter_slope + rhs.emitter_slope,
            self.collector_slope + rhs.collector_slope,
        )
    }
}
impl std::ops::Sub for Gradient {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        Self::new(
            self.value - rhs.value,
            self.emitter_slope - rhs.emitter_slope,
            self.collector_slope - rhs.collector_slope,
        )
    }
}
impl std::ops::Mul for Gradient {
    type Output = Self;
    fn mul(self, rhs: Self) -> Self {
        Self::new(
            self.value * rhs.value,
            self.emitter_slope
                .mul_add(rhs.value, rhs.emitter_slope * self.value),
            self.collector_slope
                .mul_add(rhs.value, rhs.collector_slope * self.value),
        )
    }
}
impl std::ops::Mul<f64> for Gradient {
    type Output = Self;
    fn mul(self, rhs: f64) -> Self {
        Self::new(
            self.value * rhs,
            self.emitter_slope * rhs,
            self.collector_slope * rhs,
        )
    }
}
impl std::ops::Div<f64> for Gradient {
    type Output = Self;
    fn div(self, rhs: f64) -> Self {
        self * (1.0 / rhs)
    }
}
impl std::ops::Div for Gradient {
    type Output = Self;
    fn div(self, rhs: Self) -> Self {
        let inv = 1.0 / rhs.value;
        Self::new(
            self.value * inv,
            (self.value * inv).mul_add(-rhs.emitter_slope, self.emitter_slope) * inv,
            (self.value * inv).mul_add(-rhs.collector_slope, self.collector_slope) * inv,
        )
    }
}
