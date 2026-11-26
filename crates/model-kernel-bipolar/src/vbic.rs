//! VBIC 1.1.5 prepared bipolar device and constitutive laws.
//!
//! Based on the publicly documented VBIC pseudo-code and ngspice's VBIC
//! level-9 equations. The binding module resolves native model and instance
//! cards, device assembles the coupled residual and Jacobian, and shared
//! exports the prepared-model ABI. Constitutive-law inputs are temperature
//! adjusted unless explicitly identified as nominal.

pub mod avalanche;
pub mod binding;
pub mod charge;
pub mod depletion;
pub mod device;
pub mod noise;
pub mod parasitic;
pub mod resistance;
pub mod shared;
pub mod temperature;
pub mod transport;

use temperature::Coefficient as C;
use transport::Gradient;

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
    /// Temperature derivatives of the five junction currents in A/K.
    pub temperature: JunctionSlopes,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct JunctionSlopes {
    pub ibe: f64,
    pub ibex: f64,
    pub ibcj: f64,
    pub ibep: f64,
    pub ibcp: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct Prepared {
    parameters: Parameters,
    vt: Gradient,
    junctions: [Gradient; 8],
    vbbe: Gradient,
    nbbe: Gradient,
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
            vt: Gradient::constant(temperature::thermal_voltage(temperature)),
            junctions: [
                parameters.ibei,
                parameters.iben,
                parameters.ibci,
                parameters.ibcn,
                parameters.ibeip,
                parameters.ibenp,
                parameters.ibcip,
                parameters.ibcnp,
            ]
            .map(Gradient::constant),
            vbbe: Gradient::constant(parameters.vbbe),
            nbbe: Gradient::constant(parameters.nbbe),
        })
    }

    pub(super) fn from_temperature(t: &temperature::Prepared) -> Result<Self, ModelError> {
        let mut prepared = Self::new(t.branch_parameters(), t.temperature())?;
        prepared.vt = t.voltage_jet();
        prepared.junctions = [
            C::Ibei,
            C::Iben,
            C::Ibci,
            C::Ibcn,
            C::Ibeip,
            C::Ibenp,
            C::Ibcip,
            C::Ibcnp,
        ]
        .map(|c| t.jet(c));
        prepared.vbbe = t.jet(C::Vbbe);
        prepared.nbbe = t.jet(C::Nbbe);
        Ok(prepared)
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
        let intrinsic = self.be_junction(bias.vbei) * p.wbe;
        let external = self.be_junction(bias.vbex) * (1.0 - p.wbe);
        let [
            _,
            _,
            ideal_collector,
            recombination_collector,
            ideal_peripheral,
            recombination_peripheral,
            ideal_substrate,
            recombination_substrate,
        ] = self.junctions;
        let bc = junction(ideal_collector, p.nci, self.vt, bias.vbci)
            + junction(recombination_collector, p.ncn, self.vt, bias.vbci);
        let bep = junction(ideal_peripheral, p.nci, self.vt, bias.vbep)
            + junction(recombination_peripheral, p.ncn, self.vt, bias.vbep);
        let bcp = junction(ideal_substrate, p.ncip, self.vt, bias.vbcp)
            + junction(recombination_substrate, p.ncnp, self.vt, bias.vbcp);
        Ok(Currents {
            ibe: branch(intrinsic)?,
            ibex: branch(external)?,
            ibcj: branch(bc)?,
            ibep: branch(bep)?,
            ibcp: branch(bcp)?,
            temperature: JunctionSlopes {
                ibe: intrinsic.temperature,
                ibex: external.temperature,
                ibcj: bc.temperature,
                ibep: bep.temperature,
                ibcp: bcp.temperature,
            },
        })
    }

    fn be_junction(&self, voltage: f64) -> Gradient {
        let p = &self.parameters;
        let [ideal, recombination, ..] = self.junctions;
        let sum = junction(ideal, p.nei, self.vt, voltage)
            + junction(recombination, p.nen, self.vt, voltage);
        if p.vbbe == 0.0 || p.ibbe == 0.0 {
            return sum;
        }
        let vt = self.nbbe * self.vt;
        // Reverse breakdown anchored at zero bias, with exact thermal derivative.
        let exp = ((-self.vbbe - Gradient::emitter(voltage)) / vt).exp();
        let zero = (-self.vbbe / vt).exp();
        sum - (exp - zero) * p.ibbe
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

fn junction(saturation: Gradient, ideality: f64, vt: Gradient, voltage: f64) -> Gradient {
    if saturation.value == 0.0 {
        return Gradient::constant(0.0);
    }
    saturation * (Gradient::emitter(voltage) / (vt * ideality)).exp_m1()
}

fn branch(value: Gradient) -> Result<Branch, EvaluationError> {
    if !value.is_finite() {
        return Err(EvaluationError::NonFiniteResult);
    }
    Branch::validated(value.value, value.be)
}
