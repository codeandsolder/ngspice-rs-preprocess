//! VBIC parasitic substrate transistor transfer-current law (Iccp).
//!
//! Source: VBIC 1.1.5 / ngspice vbicload.c, ISP/WSP/NFP/IKP equations.
//! It is a distinct parasitic physics path with three voltage derivatives,
//! not a substitution of the intrinsic BJT's base-charge transport.

use crate::{
    EvaluationError, ModelError, Temperature, validate_batch_lengths, validate_nonnegative,
    validate_positive,
};

#[derive(Clone, Copy, Debug)]
pub struct Parameters {
    /// Temperature- and geometry-adjusted parasitic saturation current (A).
    pub isp: f64,
    /// Distribution between Vbep and Vbci forward injection.
    pub wsp: f64,
    /// Parasitic forward emission coefficient.
    pub nfp: f64,
    /// Parasitic knee current (A); zero disables high-current rolloff.
    pub ikp: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct Bias {
    pub vbep: f64,
    pub vbci: f64,
    pub vbcp: f64,
}

/// Iccp and its complete local partial derivatives in A and S.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Output {
    /// Unnormalized forward parasitic source Ifp, for reverse-time diffusion charge.
    pub ideal_forward: f64,
    /// Partial of Ifp wrt Vbep.
    pub ideal_forward_vbep: f64,
    /// Partial of Ifp wrt intrinsic Vbci.
    pub ideal_forward_vbci: f64,
    pub iccp: f64,
    pub d_vbep: f64,
    pub d_vbci: f64,
    pub d_vbcp: f64,
    pub qb: f64,
    /// d(qbp)/d(Vbep), used by the nonlinear peripheral base resistor.
    pub qb_vbep: f64,
    /// d(qbp)/d(Vbci), used by the nonlinear peripheral base resistor.
    pub qb_vbci: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct Prepared {
    params: Parameters,
    inverse_voltage: f64,
    inverse_ikp: f64,
}

impl Prepared {
    /// # Errors
    /// Rejects any invalid prepared parasitic-model parameter.
    pub fn new(parameters: Parameters, temperature: Temperature) -> Result<Self, ModelError> {
        validate_nonnegative("isp", parameters.isp)?;
        validate_nonnegative("ikp", parameters.ikp)?;
        validate_positive("nfp", parameters.nfp)?;
        if !parameters.wsp.is_finite() || !(0.0..=1.0).contains(&parameters.wsp) {
            return Err(ModelError::InvalidParameter("wsp"));
        }
        let inv = 1.0 / ((1.380_662e-23 / 1.602_189e-19) * temperature.kelvin() * parameters.nfp);
        let inv_ikp = if parameters.ikp == 0.0 {
            0.0
        } else {
            1.0 / parameters.ikp
        };
        if !inv.is_finite() || !inv_ikp.is_finite() {
            return Err(ModelError::InvalidParameter("parasitic reciprocal"));
        }
        Ok(Self {
            params: parameters,
            inverse_voltage: inv,
            inverse_ikp: inv_ikp,
        })
    }

    /// # Errors
    /// Rejects nonfinite input/output or a nonphysical base-charge radicand.
    pub fn evaluate(&self, bias: Bias) -> Result<Output, EvaluationError> {
        if ![bias.vbep, bias.vbci, bias.vbcp]
            .iter()
            .all(|value| value.is_finite())
        {
            return Err(EvaluationError::NonFiniteBias);
        }
        let p = self.params;
        if p.isp == 0.0 {
            return Ok(Output {
                qb: 1.0,
                ..Output::default()
            });
        }
        let scale = self.inverse_voltage;
        let (emitter, emitter_slope) = exponential_branch(p.isp * p.wsp, bias.vbep * scale, scale);
        let (collector, collector_slope) =
            exponential_branch(p.isp * (1.0 - p.wsp), bias.vbci * scale, scale);
        let forward = emitter + collector;
        let (reverse, reverse_slope) = exponential_branch(p.isp, bias.vbcp * scale, scale);
        let radical = 4.0_f64.mul_add(forward * self.inverse_ikp, 1.0);
        if radical <= 0.0 {
            return Err(EvaluationError::InvalidPhysicalDomain);
        }
        let root = radical.sqrt();
        let qb = 1.0_f64.midpoint(root);
        let numerator = forward - reverse;
        let iccp = numerator / qb;
        let q_slope = self.inverse_ikp / root;
        let transfer_slope = (-iccp).mul_add(q_slope, 1.0) / qb;
        let output = Output {
            ideal_forward: forward,
            ideal_forward_vbep: emitter_slope,
            ideal_forward_vbci: collector_slope,
            iccp,
            d_vbep: emitter_slope * transfer_slope,
            d_vbci: collector_slope * transfer_slope,
            d_vbcp: -reverse_slope / qb,
            qb,
            qb_vbep: q_slope * emitter_slope,
            qb_vbci: q_slope * collector_slope,
        };
        if ![
            output.ideal_forward,
            output.ideal_forward_vbep,
            output.ideal_forward_vbci,
            output.iccp,
            output.d_vbep,
            output.d_vbci,
            output.d_vbcp,
            output.qb,
            output.qb_vbep,
            output.qb_vbci,
        ]
        .iter()
        .all(|value| value.is_finite())
        {
            return Err(EvaluationError::NonFiniteResult);
        }
        Ok(output)
    }

    /// # Errors
    /// Rejects unmatched buffers or invalid individual evaluations.
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

fn exponential_branch(saturation: f64, exponent: f64, inverse_voltage: f64) -> (f64, f64) {
    if saturation == 0.0 {
        return (0.0, 0.0);
    }
    (
        saturation * exponent.exp_m1(),
        saturation * exponent.exp() * inverse_voltage,
    )
}
