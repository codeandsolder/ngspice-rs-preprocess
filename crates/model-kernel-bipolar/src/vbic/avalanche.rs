//! VBIC 1.1.5 B-C impact-ionization correction.
//!
//! Implements the native avalanche multiplier and Ibc = Ibcj - Igc law;
//! Igc = (Itzf - Itzr - Ibcj) * avalf.
//! Source: ngspice VBIC `vbic_4T_it_cf_fj` (AVC1, AVC2, PC, MC).
//! Currents and partials use local intrinsic Vbei / Vbci coordinates.

use super::transport;
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
    /// Avalanche generation current; subtract from unmodified diode.
    pub igc: f64,
    /// Dimensionless source avalanche factor.
    pub avalanche_factor: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct Prepared {
    p: Parameters,
}

impl Prepared {
    /// # Errors
    /// Reports invalid prepared physical model parameters.
    pub fn new(p: Parameters) -> Result<Self, ModelError> {
        validate_nonnegative("avc1", p.avc1)?;
        validate_nonnegative("avc2", p.avc2)?;
        validate_positive("pc", p.pc)?;
        validate_nonnegative("mc", p.mc)?;
        Ok(Self { p })
    }

    /// Evaluate the native B-C avalanche law and both partials.
    ///
    /// # Errors
    /// Rejects nonfinite voltage, source current/partials or numerical overflow.
    pub fn evaluate(&self, bias: Bias) -> Result<Output, EvaluationError> {
        if !bias.vbci.is_finite() {
            return Err(EvaluationError::NonFiniteBias);
        }
        let transfer = bias.transport.net;
        let junction = bias.ibcj;
        if ![
            transfer.value,
            transfer.d_vbei,
            transfer.d_vbci,
            junction.amperes,
            junction.siemens,
        ]
        .iter()
        .all(|value| value.is_finite())
        {
            return Err(EvaluationError::NonFiniteBias);
        }
        if self.p.avc1 == 0.0 {
            return Ok(Output {
                ibc: junction.amperes,
                d_vbci: junction.siemens,
                ..Output::default()
            });
        }
        let p = self.p;
        let distance = p.pc - bias.vbci;
        let smooth = distance.hypot(0.1);
        let potential = smooth.midpoint(distance);
        if potential <= 0.0 {
            return Err(EvaluationError::InvalidPhysicalDomain);
        }
        let gradient = -0.5 * (distance / smooth + 1.0);
        let power = potential.powf(p.mc - 1.0);
        let exponential = (-p.avc2 * power).exp();
        let factor = p.avc1 * potential * exponential;
        let derivative =
            p.avc1 * exponential * (p.avc2 * (p.mc - 1.0)).mul_add(-power, 1.0) * gradient;
        let source = transfer.value - junction.amperes;
        let igc = source * factor;
        let output = Output {
            ibc: junction.amperes - igc,
            d_vbei: -transfer.d_vbei * factor,
            d_vbci: junction.siemens
                - (transfer.d_vbci - junction.siemens).mul_add(factor, source * derivative),
            igc,
            avalanche_factor: factor,
        };
        if ![
            output.ibc,
            output.d_vbei,
            output.d_vbci,
            output.igc,
            output.avalanche_factor,
        ]
        .iter()
        .all(|value| value.is_finite())
        {
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
