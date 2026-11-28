//! Normalized VBIC depletion charge laws qdbe and qdbc.
//!
//! These are voltage-normalized integrals of depletion capacitance, not
//! physical charge in coulombs. Both feed the Early effect whether or not
//! the physical CJE / CJC capacitance is nonzero.

use super::temperature::{self, Coefficient as C};
use super::transport::{CheckedGradient, Depletion, Gradient};
use crate::{EvaluationError, ModelError, validate_nonnegative, validate_positive};

#[derive(Clone, Copy, Debug)]
pub struct Parameters {
    /// Forward-bias depletion continuation point as a fraction of potential.
    pub fc: f64,
    pub pe: f64,
    pub me: f64,
    /// B-E junction smoothing (nonpositive means piecewise continuation).
    pub aje: f64,
    pub pc: f64,
    pub mc: f64,
    /// B-C junction smoothing (nonpositive means piecewise continuation).
    pub ajc: f64,
    /// B-C reverse reach-through voltage, SPICE model keyword VRT; zero disables.
    pub vrt: f64,
    /// B-C second smoothing in punch-through mode, zero disables.
    pub art: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct Prepared {
    params: Parameters,
    pe: Gradient,
    pc: Gradient,
}

impl Prepared {
    /// # Errors
    /// Rejects invalid junction model parameters.
    pub fn new(parameters: Parameters) -> Result<Self, ModelError> {
        for (name, value) in [("pe", parameters.pe), ("pc", parameters.pc)] {
            validate_positive(name, value)?;
        }
        for (name, value) in [
            ("me", parameters.me),
            ("mc", parameters.mc),
            ("vrt", parameters.vrt),
            ("art", parameters.art),
        ] {
            validate_nonnegative(name, value)?;
        }
        for (name, value) in [("aje", parameters.aje), ("ajc", parameters.ajc)] {
            if !value.is_finite() {
                return Err(ModelError::InvalidParameter(name));
            }
        }
        if !parameters.fc.is_finite() || !(0.0..1.0).contains(&parameters.fc) {
            return Err(ModelError::InvalidParameter("fc"));
        }
        Ok(Self {
            params: parameters,
            pe: Gradient::constant(parameters.pe),
            pc: Gradient::constant(parameters.pc),
        })
    }

    /// Evaluates normalized depletion charges, with their first derivatives.
    ///
    /// # Errors
    /// Reports nonfinite bias, nonphysical branch domains or overflow.
    pub fn evaluate(
        &self,
        emitter_bias: f64,
        collector_bias: f64,
    ) -> Result<Depletion, EvaluationError> {
        if !emitter_bias.is_finite() || !collector_bias.is_finite() {
            return Err(EvaluationError::NonFiniteBias);
        }
        let p = self.params;
        let emitter = Gradient::emitter(emitter_bias);
        let collector = Gradient::collector(collector_bias);
        let be = smooth_depletion(emitter, self.pe, p.me, p.fc, p.aje)?;
        let bc = bc_depletion(collector, self.pc, &p)?;
        let output = Depletion {
            qdbe: be.value,
            cbe: be.be,
            qdbc: bc.value,
            cbc: bc.bc,
            d_qdbe_temperature: be.temperature,
            d_qdbc_temperature: bc.temperature,
        };
        if !be.is_finite() || !bc.is_finite() {
            return Err(EvaluationError::NonFiniteResult);
        }
        Ok(output)
    }
}

/// Integral of the normalized abrupt depletion capacitance.
fn integral(
    voltage: Gradient,
    potential: Gradient,
    grading: f64,
) -> Result<Gradient, EvaluationError> {
    let ratio = (Gradient::constant(1.0) - voltage / potential).checked_ln()?;
    let exponent = 1.0 - grading;
    if exponent == 0.0 {
        Ok(ratio * -potential)
    } else {
        Ok((ratio * exponent).exp_m1() * (-potential / exponent))
    }
}

/// VBIC's abrupt transition at FC * P with quadratic continuation.
fn hard_depletion(
    voltage: Gradient,
    potential: Gradient,
    grading: f64,
    fc: f64,
    vrt: f64,
) -> Result<Gradient, EvaluationError> {
    if voltage.value > potential.value * fc {
        let crossing = potential * fc;
        let excess = voltage - crossing;
        let slope = (1.0 - fc).powf(-1.0 - grading);
        let tail = excess
            * ((Gradient::constant(1.0 - fc)
                + excess * (Gradient::constant(0.5 * grading) / potential))
                * slope);
        Ok(integral(crossing, potential, grading)? + tail)
    } else if vrt > 0.0 && voltage.value < -vrt {
        let crossing = -vrt;
        let slope = (Gradient::constant(vrt) / potential + 1.0).checked_powf(-grading)?;
        Ok(integral(Gradient::constant(crossing), potential, grading)?
            + (voltage - Gradient::constant(crossing)) * slope)
    } else {
        integral(voltage, potential, grading)
    }
}

/// Smooth normalized charge at the FC continuation, VBIC AJE/AJC > 0.
fn smooth_depletion(
    voltage: Gradient,
    potential: Gradient,
    grading: f64,
    fc: f64,
    smoothing: f64,
) -> Result<Gradient, EvaluationError> {
    if smoothing <= 0.0 {
        return hard_depletion(voltage, potential, grading, fc, 0.0);
    }
    let dv0 = -potential * fc;
    let mv0 = (dv0 * dv0 + 4.0 * smoothing * smoothing).checked_sqrt()?;
    let vl0 = (dv0 + mv0) * -0.5;
    let dv = voltage + dv0;
    let mv = (dv * dv + Gradient::constant(4.0 * smoothing * smoothing)).checked_sqrt()?;
    let vl = (dv - mv) * 0.5 - dv0;
    Ok(
        integral(vl, potential, grading)? + (voltage - vl + vl0) * (1.0 - fc).powf(-grading)
            - integral(vl0, potential, grading)?,
    )
}

/// Full VBIC B-C two-ended smooth punch-through (AJC, ART and VPT > 0).
fn double_smooth_depletion(
    voltage: Gradient,
    pc: Gradient,
    p: &Parameters,
) -> Result<Gradient, EvaluationError> {
    let dv0 = -pc * p.fc;
    let denominator = Gradient::constant(p.vrt) - dv0;
    let normalized = |voltage: Gradient| (voltage * 2.0 + (dv0 + p.vrt)) / denominator;
    let normalized_smoother = |vn: Gradient| -> Result<Gradient, EvaluationError> {
        let minus = vn - Gradient::constant(1.0);
        let plus = vn + Gradient::constant(1.0);
        let left = (minus * minus + Gradient::constant(4.0 * p.ajc * p.ajc)).checked_sqrt()?;
        let right = (plus * plus + Gradient::constant(4.0 * p.art * p.art)).checked_sqrt()?;
        Ok((vn * 2.0) / (left + right))
    };
    let bias_smoother = |voltage: Gradient| -> Result<Gradient, EvaluationError> {
        let vn = normalized_smoother(normalized(voltage))?;
        Ok(vn * (denominator * 0.5) - ((dv0 + p.vrt) * 0.5))
    };
    let vl0 = bias_smoother(Gradient::constant(0.0))?;
    let vl = bias_smoother(voltage)?;
    let vnl = normalized_smoother(normalized(voltage))?;
    let sel = (vnl + Gradient::constant(1.0)) * 0.5;
    let crt = (Gradient::constant(p.vrt) / pc + 1.0).checked_powf(-p.mc)?;
    let cmx = (1.0 - p.fc).powf(-p.mc);
    let slope = (Gradient::constant(1.0) - sel) * crt + sel * cmx;
    Ok(integral(vl, pc, p.mc)? + (voltage - vl + vl0) * slope - integral(vl0, pc, p.mc)?)
}

/// VBIC substrate-collector depletion junction, with its independent
/// zero-bias potential PS, grading MS and AJS smoothing.
#[derive(Clone, Copy, Debug)]
pub struct SubstrateParameters {
    pub ps: f64,
    pub ms: f64,
    pub ajs: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct ExtendedBias {
    pub emitter_intrinsic: f64,
    pub emitter_external: f64,
    pub collector_intrinsic: f64,
    pub collector_peripheral: f64,
    pub substrate_collector: f64,
}

/// One normalized junction integral in V and its dimensionless derivative.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Normalized {
    pub volts: f64,
    pub derivative: f64,
    pub d_temperature: f64,
}

/// All five normalized junction integrals used by VBIC physical charges.
#[derive(Clone, Copy, Debug, Default)]
pub struct ExtendedOutput {
    pub intrinsic: Depletion,
    pub external_emitter: Normalized,
    pub peripheral_collector: Normalized,
    pub substrate_collector: Normalized,
}

/// Prepared normalized depletion evaluator with the common FC and independent
/// intrinsic, peripheral and substrate-collector grading/smoothing.
#[derive(Clone, Copy, Debug)]
pub struct ExtendedPrepared {
    central: Prepared,
    substrate: SubstrateParameters,
    ps: Gradient,
}

impl ExtendedPrepared {
    /// # Errors
    /// Validates the full junction parameter set before evaluation.
    pub fn new(central: Parameters, substrate: SubstrateParameters) -> Result<Self, ModelError> {
        let prepared = Prepared::new(central)?;
        validate_positive("ps", substrate.ps)?;
        validate_nonnegative("ms", substrate.ms)?;
        if !substrate.ajs.is_finite() {
            return Err(ModelError::InvalidParameter("ajs"));
        }
        Ok(Self {
            central: prepared,
            substrate,
            ps: Gradient::constant(substrate.ps),
        })
    }

    pub(super) fn from_temperature(t: &temperature::Prepared) -> Result<Self, ModelError> {
        let mut prepared = Self::new(t.depletion_parameters(), t.substrate_parameters())?;
        prepared.central.pe = t.jet(C::Pe);
        prepared.central.pc = t.jet(C::Pc);
        prepared.ps = t.jet(C::Ps);
        Ok(prepared)
    }

    /// # Errors
    /// Propagates nonfinite/invalid source voltages or physical-domain errors.
    pub fn evaluate(&self, bias: ExtendedBias) -> Result<ExtendedOutput, EvaluationError> {
        if ![
            bias.emitter_intrinsic,
            bias.emitter_external,
            bias.collector_intrinsic,
            bias.collector_peripheral,
            bias.substrate_collector,
        ]
        .iter()
        .all(|voltage| voltage.is_finite())
        {
            return Err(EvaluationError::NonFiniteBias);
        }
        let p = self.central.params;
        let s = self.substrate;
        let intrinsic = self
            .central
            .evaluate(bias.emitter_intrinsic, bias.collector_intrinsic)?;
        let external_emitter = smooth_depletion(
            Gradient::emitter(bias.emitter_external),
            self.central.pe,
            p.me,
            p.fc,
            p.aje,
        )?;
        let peripheral_collector = bc_depletion(
            Gradient::emitter(bias.collector_peripheral),
            self.central.pc,
            &p,
        )?;
        let substrate_collector = smooth_depletion(
            Gradient::emitter(bias.substrate_collector),
            self.ps,
            s.ms,
            p.fc,
            s.ajs,
        )?;
        let result = ExtendedOutput {
            intrinsic,
            external_emitter: Normalized {
                volts: external_emitter.value,
                derivative: external_emitter.be,
                d_temperature: external_emitter.temperature,
            },
            peripheral_collector: Normalized {
                volts: peripheral_collector.value,
                derivative: peripheral_collector.be,
                d_temperature: peripheral_collector.temperature,
            },
            substrate_collector: Normalized {
                volts: substrate_collector.value,
                derivative: substrate_collector.be,
                d_temperature: substrate_collector.temperature,
            },
        };
        if ![external_emitter, peripheral_collector, substrate_collector]
            .into_iter()
            .all(Gradient::is_finite)
        {
            return Err(EvaluationError::NonFiniteResult);
        }
        Ok(result)
    }
}

fn bc_depletion(
    voltage: Gradient,
    pc: Gradient,
    p: &Parameters,
) -> Result<Gradient, EvaluationError> {
    if p.ajc > 0.0 && p.vrt > 0.0 && p.art > 0.0 {
        double_smooth_depletion(voltage, pc, p)
    } else if p.ajc > 0.0 {
        smooth_depletion(voltage, pc, p.mc, p.fc, p.ajc)
    } else {
        hard_depletion(voltage, pc, p.mc, p.fc, p.vrt)
    }
}
