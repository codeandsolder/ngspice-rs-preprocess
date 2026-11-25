//! VBIC 1.1.5 intrinsic and parasitic base-junction current laws.
//!
//! Based on the publicly documented VBIC pseudo-code and ngspice's VBIC
//! level-9 equations. All current parameters are *temperature-adjusted*
//! values, rather than uncorrected .MODEL-card values. This module does not
//! expose a complete VBIC device kernel.

pub mod avalanche;
pub mod binding;
pub mod charge;
pub mod depletion;
pub mod noise;
pub mod parasitic;
pub mod resistance;
pub mod temperature;
pub mod transport;

use crate::{
    Branch, EvaluationError, ModelError, Temperature, validate_batch_lengths, validate_nonnegative,
    validate_positive,
};

/// Temperature-adjusted VBIC 1.1.5 branch parameters. Geometry/multiplicity
/// scaling is the responsibility of instance preparation. All currents in A.
#[derive(Clone, Copy, Debug)]
pub struct Parameters {
    pub ibei: f64,
    pub iben: f64,
    pub nei: f64,
    pub nen: f64,
    /// Portion of base-emitter injection flowing through intrinsic base.
    pub wbe: f64,
    pub ibci: f64,
    pub ibcn: f64,
    pub nci: f64,
    pub ncn: f64,
    pub ibeip: f64,
    pub ibenp: f64,
    pub ibcip: f64,
    pub ibcnp: f64,
    pub ncip: f64,
    pub ncnp: f64,
    /// Reverse breakdown voltage; zero disables the breakdown source.
    pub vbbe: f64,
    pub nbbe: f64,
    pub ibbe: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct Bias {
    pub vbei: f64,
    pub vbex: f64,
    pub vbci: f64,
    pub vbep: f64,
    pub vbcp: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Currents {
    pub ibe: Branch,
    pub ibex: Branch,
    /// Intrinsic b-c diode, without collector-base avalanche.
    pub ibcj: Branch,
    pub ibep: Branch,
    pub ibcp: Branch,
}

#[derive(Clone, Copy, Debug)]
pub struct Prepared {
    parameters: Parameters,
    vt: f64,
}

impl Prepared {
    /// Prepare only the VBIC branch laws represented by this type.
    /// # Errors
    /// Returns the offending model parameter on nonfinite or invalid values.
    pub fn new(parameters: Parameters, temperature: Temperature) -> Result<Self, ModelError> {
        for (name, value) in [
            ("ibei", parameters.ibei),
            ("iben", parameters.iben),
            ("ibci", parameters.ibci),
            ("ibcn", parameters.ibcn),
            ("ibeip", parameters.ibeip),
            ("ibenp", parameters.ibenp),
            ("ibcip", parameters.ibcip),
            ("ibcnp", parameters.ibcnp),
            ("ibbe", parameters.ibbe),
            ("vbbe", parameters.vbbe),
        ] {
            validate_nonnegative(name, value)?;
        }
        for (name, value) in [
            ("nei", parameters.nei),
            ("nen", parameters.nen),
            ("nci", parameters.nci),
            ("ncn", parameters.ncn),
            ("ncip", parameters.ncip),
            ("ncnp", parameters.ncnp),
        ] {
            validate_positive(name, value)?;
        }
        if !parameters.wbe.is_finite() || !(0.0..=1.0).contains(&parameters.wbe) {
            return Err(ModelError::InvalidParameter("wbe"));
        }
        if parameters.vbbe > 0.0 && parameters.ibbe > 0.0 {
            validate_positive("nbbe", parameters.nbbe)?;
        } else if !parameters.nbbe.is_finite() {
            return Err(ModelError::InvalidParameter("nbbe"));
        }
        Ok(Self {
            parameters,
            vt: (1.380_662e-23 / 1.602_189e-19) * temperature.kelvin(),
        })
    }

    /// All diode derivatives are analytic. No state mutation or allocations.
    /// # Errors
    /// Returns a numerical error for nonfinite bias or current/derivative.
    pub fn evaluate(&self, bias: Bias) -> Result<Currents, EvaluationError> {
        if ![bias.vbei, bias.vbex, bias.vbci, bias.vbep, bias.vbcp]
            .iter()
            .all(|value| value.is_finite())
        {
            return Err(EvaluationError::NonFiniteBias);
        }
        let p = &self.parameters;
        let intrinsic = self.be_junction(bias.vbei)?;
        let external = self.be_junction(bias.vbex)?;
        Ok(Currents {
            ibe: scale(intrinsic, p.wbe),
            ibex: scale(external, 1.0 - p.wbe),
            ibcj: sum(
                junction(p.ibci, p.nci, self.vt, bias.vbci)?,
                junction(p.ibcn, p.ncn, self.vt, bias.vbci)?,
            )?,
            ibep: sum(
                junction(p.ibeip, p.nci, self.vt, bias.vbep)?,
                junction(p.ibenp, p.ncn, self.vt, bias.vbep)?,
            )?,
            ibcp: sum(
                junction(p.ibcip, p.ncip, self.vt, bias.vbcp)?,
                junction(p.ibcnp, p.ncnp, self.vt, bias.vbcp)?,
            )?,
        })
    }

    fn be_junction(&self, voltage: f64) -> Result<Branch, EvaluationError> {
        let p = &self.parameters;
        let ideal = junction(p.ibei, p.nei, self.vt, voltage)?;
        let nonideal = junction(p.iben, p.nen, self.vt, voltage)?;
        let sum = sum(ideal, nonideal)?;
        if p.vbbe == 0.0 || p.ibbe == 0.0 {
            return Ok(sum);
        }
        let vt = p.nbbe * self.vt;
        // Reverse-breakdown source, anchored to exactly zero at zero bias.
        let exp = ((-p.vbbe - voltage) / vt).exp();
        let zero = (-p.vbbe / vt).exp();
        Branch::validated(
            p.ibbe.mul_add(-(exp - zero), sum.amperes),
            sum.siemens + p.ibbe * exp / vt,
        )
    }

    /// Evaluate a homogeneous batch into caller-owned storage. Can be used on
    /// contiguous array-of-structures chunks; a future solver can lower them to structure-of-arrays.
    /// # Errors
    /// Returns a length error for incompatible buffers or an evaluation error.
    pub fn evaluate_batch(
        &self,
        inputs: &[Bias],
        outputs: &mut [Currents],
    ) -> Result<(), EvaluationError> {
        validate_batch_lengths(inputs.len(), outputs.len())?;
        for (input, output) in inputs.iter().zip(outputs) {
            *output = self.evaluate(*input)?;
        }
        Ok(())
    }
}

fn junction(
    saturation: f64,
    ideality: f64,
    vt: f64,
    voltage: f64,
) -> Result<Branch, EvaluationError> {
    if saturation == 0.0 {
        return Ok(Branch::default());
    }
    let exponent = voltage / (ideality * vt);
    let exponential = exponent.exp();
    Branch::validated(
        saturation * exponent.exp_m1(),
        saturation * exponential / (ideality * vt),
    )
}

fn sum(first: Branch, second: Branch) -> Result<Branch, EvaluationError> {
    Branch::validated(
        first.amperes + second.amperes,
        first.siemens + second.siemens,
    )
}

fn scale(branch: Branch, factor: f64) -> Branch {
    Branch {
        amperes: branch.amperes * factor,
        siemens: branch.siemens * factor,
    }
}
