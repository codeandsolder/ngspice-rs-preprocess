//! VBIC 1.1.5 B-C impact-ionization correction.
//!
//! Implements the native avalanche multiplier and Ibc = Ibcj - Igc law;
//! Igc = (Itzf - Itzr - Ibcj) * avalf.
//! Source: ngspice VBIC `vbic_4T_it_cf_fj` (AVC1, AVC2, PC, MC).
//! Currents and partials use local intrinsic Vbei / Vbci coordinates.

use super::{
    temperature::{self, Coefficient as C},
    transport::{self, CheckedGradient, Gradient},
};
use crate::{Branch, EvaluationError, ModelError, validate_nonnegative, validate_positive};

#[derive(Clone, Copy, Debug)]
pub struct Parameters {
    pub avc1: f64,
    /// Temperature-adjusted avalanche exponential factor.
    pub avc2: f64,
    /// Temperature-adjusted B-C depletion potential.
    pub pc: f64,
    /// B-C grading coefficient.
    pub mc: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct Bias {
    pub vbci: f64,
    pub transport: transport::Output,
    /// Intrinsic ideal/nonideal B-C diode current and local conductance,
    /// before avalanche correction.
    pub ibcj: Branch,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Output {
    /// Total B-C current after avalanche injection.
    pub ibc: f64,
    /// Partial d(Ibc)/d(Vbei) in siemens.
    pub d_vbei: f64,
    /// Partial d(Ibc)/d(Vbci) in siemens.
    pub d_vbci: f64,
    pub d_temperature: f64,
    /// Avalanche generation current; subtract from unmodified diode.
    pub igc: f64,
    /// Dimensionless source avalanche factor.
    pub avalanche_factor: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct Prepared {
    p: Parameters,
    pc: Gradient,
    avc2: Gradient,
}

impl Prepared {
    /// # Errors
    /// Reports invalid prepared physical model parameters.
    pub fn new(p: Parameters) -> Result<Self, ModelError> {
        validate_nonnegative("avc1", p.avc1)?;
        validate_nonnegative("avc2", p.avc2)?;
        validate_positive("pc", p.pc)?;
        validate_nonnegative("mc", p.mc)?;
        Ok(Self {
            p,
            pc: Gradient::constant(p.pc),
            avc2: Gradient::constant(p.avc2),
        })
    }

    pub(super) fn from_temperature(t: &temperature::Prepared) -> Result<Self, ModelError> {
        let mut prepared = Self::new(t.avalanche_parameters())?;
        prepared.pc = t.jet(C::Pc);
        prepared.avc2 = t.jet(C::Avc2);
        Ok(prepared)
    }

    /// Evaluate the native B-C avalanche law and both partials.
    ///
    /// # Errors
    /// Rejects nonfinite voltage, source current/partials or numerical overflow.
    pub fn evaluate(&self, bias: Bias) -> Result<Output, EvaluationError> {
        self.evaluate_thermal(bias, 0.0)
    }

    pub(super) fn evaluate_thermal(
        &self,
        bias: Bias,
        junction_temperature: f64,
    ) -> Result<Output, EvaluationError> {
        if !bias.vbci.is_finite() {
            return Err(EvaluationError::NonFiniteBias);
        }
        let transfer = bias.transport.net;
        let net = Gradient::new(transfer.value, transfer.d_vbei, transfer.d_vbci, 0.0)
            .with_temperature(transfer.d_temperature);
        let junction = Gradient::new(bias.ibcj.amperes, 0.0, bias.ibcj.siemens, 0.0)
            .with_temperature(junction_temperature);
        if !net.is_finite() || !junction.is_finite() {
            return Err(EvaluationError::NonFiniteBias);
        }
        if self.p.avc1 == 0.0 {
            return Ok(Output {
                ibc: junction.value,
                d_vbci: junction.bc,
                d_temperature: junction.temperature,
                ..Output::default()
            });
        }
        let distance = self.pc - Gradient::collector(bias.vbci);
        let smooth = (distance * distance + 0.01).checked_sqrt()?;
        let potential = (smooth + distance) * 0.5;
        let power = potential.checked_powf(self.p.mc - 1.0)?;
        let factor = potential * (-self.avc2 * power).exp() * self.p.avc1;
        let igc = (net - junction) * factor;
        let ibc = junction - igc;
        let output = Output {
            ibc: ibc.value,
            d_vbei: ibc.be,
            d_vbci: ibc.bc,
            d_temperature: ibc.temperature,
            igc: igc.value,
            avalanche_factor: factor.value,
        };
        if ![ibc, igc, factor].into_iter().all(Gradient::is_finite) {
            return Err(EvaluationError::NonFiniteResult);
        }
        Ok(output)
    }

    /// # Errors
    /// Returns a numerical evaluation error for any invalid branch.
    pub fn evaluate_batch(
        &self,
        inputs: &[Bias],
        outputs: &mut [Output],
    ) -> Result<(), EvaluationError> {
        crate::validate_batch_lengths(inputs.len(), outputs.len())?;
        for (bias, output) in inputs.iter().zip(outputs) {
            *output = self.evaluate(*bias)?;
        }
        Ok(())
    }
}
