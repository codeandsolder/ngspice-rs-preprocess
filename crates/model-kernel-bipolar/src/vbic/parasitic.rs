//! VBIC parasitic substrate transistor transfer-current law (Iccp).
//!
//! Source: VBIC 1.1.5 / ngspice vbicload.c, ISP/WSP/NFP/IKP equations.
//! It is a distinct parasitic physics path with three voltage derivatives,
//! not a substitution of the intrinsic BJT's base-charge transport.

use super::{
    temperature::{self, Coefficient as C},
    transport::{CheckedGradient, Gradient},
};
use crate::{
    EvaluationError, ModelError, Temperature, evaluate_lanes, validate_nonnegative,
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
    pub ideal_forward_temperature: f64,
    pub iccp: f64,
    pub d_vbep: f64,
    pub d_vbci: f64,
    pub d_vbcp: f64,
    pub d_temperature: f64,
    pub qb: f64,
    /// d(qbp)/d(Vbep), used by the nonlinear peripheral base resistor.
    pub qb_vbep: f64,
    /// d(qbp)/d(Vbci), used by the nonlinear peripheral base resistor.
    pub qb_vbci: f64,
    pub qb_temperature: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct Prepared {
    params: Parameters,
    inverse_voltage: Gradient,
    saturation: Gradient,
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
            inverse_voltage: Gradient::constant(inv),
            saturation: Gradient::constant(parameters.isp),
            inverse_ikp: inv_ikp,
        })
    }

    pub(super) fn from_temperature(t: &temperature::Prepared) -> Result<Self, ModelError> {
        let mut prepared = Self::new(t.parasitic_parameters(), t.temperature())?;
        prepared.inverse_voltage =
            Gradient::constant(1.0) / (t.voltage_jet() * prepared.params.nfp);
        prepared.saturation = t.jet(C::Isp);
        Ok(prepared)
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
        let emitter = exponential_branch(
            self.saturation * p.wsp,
            Gradient::emitter(bias.vbep) * scale,
        );
        let collector = exponential_branch(
            self.saturation * (1.0 - p.wsp),
            Gradient::collector(bias.vbci) * scale,
        );
        let forward = emitter + collector;
        let reverse = exponential_branch(self.saturation, Gradient::charge(bias.vbcp) * scale);
        let qb = ((forward * (4.0 * self.inverse_ikp) + 1.0).checked_sqrt()? + 1.0) * 0.5;
        let iccp = (forward - reverse) / qb;
        let output = Output {
            ideal_forward: forward.value,
            ideal_forward_vbep: forward.be,
            ideal_forward_vbci: forward.bc,
            ideal_forward_temperature: forward.temperature,
            iccp: iccp.value,
            d_vbep: iccp.be,
            d_vbci: iccp.bc,
            d_vbcp: iccp.hole,
            d_temperature: iccp.temperature,
            qb: qb.value,
            qb_vbep: qb.be,
            qb_vbci: qb.bc,
            qb_temperature: qb.temperature,
        };
        if ![forward, reverse, qb, iccp]
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
    /// Rejects unmatched buffers or invalid individual evaluations.
    pub fn evaluate_batch(
        &self,
        inputs: &[Bias],
        outputs: &mut [Output],
    ) -> Result<(), EvaluationError> {
        evaluate_lanes(inputs.iter().copied(), outputs, |bias| self.evaluate(bias))
    }
}

fn exponential_branch(saturation: Gradient, exponent: Gradient) -> Gradient {
    if saturation.value == 0.0 {
        return Gradient::constant(0.0);
    }
    saturation * exponent.exp_m1()
}
