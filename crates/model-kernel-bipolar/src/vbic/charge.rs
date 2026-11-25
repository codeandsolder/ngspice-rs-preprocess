//! VBIC 1.1.5 physical junction, transit and overlap charges.
//!
//! Implements Qbe, Qbex, Qbc, Qbcx, Qbep, Qbcp, Qbeo and Qbco from
//! ngspice VBIC `vbic_4T_it_cf_fj`, separately from the normalized depletion
//! integrals (qdbe, qdbc, ...) which have units of volts.
//! Outputs are **Coulombs** and analytic first derivatives in **Farads**;
//! the solver performs circuit charge integration and accepted-step isolation.

use super::{depletion, parasitic, transport};
use crate::{
    EvaluationError, ModelError, Temperature, validate_batch_lengths, validate_nonnegative,
};

use transport::Gradient;

/// Prepared physical capacitances and transit-time coefficients.
///
/// Holds associated family-typed transport/depletion model laws. Geometry and
/// temperature scaling belongs in instance preparation, not evaluation.
#[derive(Clone, Copy, Debug)]
pub struct Parameters {
    pub transport: transport::Parameters,
    pub parasitic: parasitic::Parameters,
    pub depletion: depletion::Parameters,
    pub substrate: depletion::SubstrateParameters,
    pub cje: f64,
    pub cjc: f64,
    pub cjep: f64,
    pub cjcp: f64,
    pub ccso: f64,
    pub cbeo: f64,
    pub cbco: f64,
    pub qco: f64,
    pub wbe: f64,
    /// Forward transit time.
    pub tf: f64,
    /// TF base-width modulation coefficient, may be signed.
    pub qtf: f64,
    /// TF bias modulation coefficient, may be signed.
    pub xtf: f64,
    /// TF collector-bias exponential scale (0 disables voltage dependence).
    pub vtf: f64,
    /// TF high-injection knee (0 disables its nonlinear factor).
    pub itf: f64,
    /// Reverse transit time.
    pub tr: f64,
    /// Epitaxial collector charge saturation parameter GAMM.
    pub gamm: f64,
}

/// Explicit, independent branch voltages; NPN/PNP orientation is the
/// caller's responsibility (identical to the other VBIC family subkernels).
#[derive(Clone, Copy, Debug)]
pub struct Bias {
    pub vbei: f64,
    pub vbex: f64,
    pub vbci: f64,
    pub vbcx: f64,
    pub vbep: f64,
    pub vbcp: f64,
    pub vbe: f64,
    pub vbc: f64,
}

/// One physical charge and its local incremental capacitance.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Charge1 {
    pub coulombs: f64,
    pub farads: f64,
}

/// Physical charge and partial derivatives wrt two explicitly named biases.
/// The two bias meanings depend on the branch and are documented below.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Charge2 {
    pub coulombs: f64,
    pub primary_farads: f64,
    pub coupled_farads: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Output {
    /// Qbe: primary Vbei, coupled Vbci.
    pub qbe: Charge2,
    /// Qbex: local Vbex.
    pub qbex: Charge1,
    /// Qbc: local Vbci.
    pub qbc: Charge1,
    /// Qbcx: local Vbcx.
    pub qbcx: Charge1,
    /// Qbep: primary Vbep, coupled Vbci.
    pub qbep: Charge2,
    /// Qbcp: local Vbcp.
    pub qbcp: Charge1,
    /// Qbeo: terminal Vbe.
    pub qbeo: Charge1,
    /// Qbco: terminal Vbc.
    pub qbco: Charge1,
}

#[derive(Clone, Copy, Debug)]
pub struct Prepared {
    p: Parameters,
    inverse_vtf: f64,
    inverse_itf: f64,
    vt: f64,
    transport: transport::Prepared,
    parasitic: parasitic::Prepared,
    depletion: depletion::ExtendedPrepared,
}

impl Prepared {
    /// # Errors
    /// Validates physical model coefficients and every dependent family kernel.
    pub fn new(p: &Parameters, temperature: Temperature) -> Result<Self, ModelError> {
        let p = *p;
        for (name, value) in [
            ("cje", p.cje),
            ("cjc", p.cjc),
            ("cjep", p.cjep),
            ("cjcp", p.cjcp),
            ("ccso", p.ccso),
            ("cbeo", p.cbeo),
            ("cbco", p.cbco),
            ("qco", p.qco),
            ("tf", p.tf),
            ("tr", p.tr),
            ("vtf", p.vtf),
            ("itf", p.itf),
            ("gamm", p.gamm),
        ] {
            validate_nonnegative(name, value)?;
        }
        for (name, value) in [("qtf", p.qtf), ("xtf", p.xtf)] {
            if !value.is_finite() {
                return Err(ModelError::InvalidParameter(name));
            }
        }
        if !p.wbe.is_finite() || !(0.0..=1.0).contains(&p.wbe) {
            return Err(ModelError::InvalidParameter("wbe"));
        }
        let reciprocal = |x: f64| if x == 0.0 { 0.0 } else { 1.0 / x };
        let collector_voltage_reciprocal = reciprocal(p.vtf);
        let high_injection_reciprocal = reciprocal(p.itf);
        if !collector_voltage_reciprocal.is_finite() || !high_injection_reciprocal.is_finite() {
            return Err(ModelError::InvalidParameter("transit time reciprocal"));
        }
        Ok(Self {
            p,
            inverse_vtf: collector_voltage_reciprocal,
            inverse_itf: high_injection_reciprocal,
            vt: (1.380_662e-23 / 1.602_189e-19) * temperature.kelvin(),
            transport: transport::Prepared::new(p.transport, temperature)?,
            parasitic: parasitic::Prepared::new(p.parasitic, temperature)?,
            depletion: depletion::ExtendedPrepared::new(p.depletion, p.substrate)?,
        })
    }

    /// Evaluates all eight VBIC charge outputs. No history is mutated.
    /// The caller integrates Q and dQ/dV using its own time discretization.
    ///
    /// # Errors
    /// Returns an invalid voltage/domain or numerical overflow error.
    pub fn evaluate(&self, bias: Bias) -> Result<Output, EvaluationError> {
        if ![
            bias.vbei, bias.vbex, bias.vbci, bias.vbcx, bias.vbep, bias.vbcp, bias.vbe, bias.vbc,
        ]
        .iter()
        .all(|v| v.is_finite())
        {
            return Err(EvaluationError::NonFiniteBias);
        }
        let p = self.p;
        let dep = self.depletion.evaluate(depletion::ExtendedBias {
            emitter_intrinsic: bias.vbei,
            emitter_external: bias.vbex,
            collector_intrinsic: bias.vbci,
            collector_peripheral: bias.vbep,
            substrate_collector: bias.vbcp,
        })?;
        let trans = self.transport.evaluate(transport::Bias {
            vbei: bias.vbei,
            vbci: bias.vbci,
            depletion: dep.intrinsic,
        })?;
        let para = self.parasitic.evaluate(parasitic::Bias {
            vbep: bias.vbep,
            vbci: bias.vbci,
            vbcp: bias.vbcp,
        })?;

        let q1 = gradient(trans.q1);
        let injected = gradient(trans.ideal_forward);
        let forward = gradient(trans.forward);
        let reverse_injection = gradient(trans.ideal_reverse);
        let transit = self.forward_time(q1, injected, bias.vbci)?;
        let emitter_depletion = Gradient::new(dep.intrinsic.qdbe, dep.intrinsic.cbe, 0.0);
        let collector_depletion = Gradient::new(dep.intrinsic.qdbc, 0.0, dep.intrinsic.cbc);
        let qbe = emitter_depletion * (p.cje * p.wbe) + transit * forward;
        let qbc = collector_depletion * p.cjc
            + reverse_injection * p.tr
            + self.epitaxial_factor(bias.vbci, false)? * p.qco;
        let qbcx = self.epitaxial_factor(bias.vbcx, true)? * p.qco;
        let ifp = Gradient::new(
            para.ideal_forward,
            para.ideal_forward_vbep,
            para.ideal_forward_vbci,
        );
        let q_parasitic = Gradient::new(
            dep.peripheral_collector.volts * p.cjep,
            dep.peripheral_collector.derivative * p.cjep,
            0.0,
        ) + ifp * p.tr;
        let output = Output {
            qbe: two(qbe),
            qbex: one(
                dep.external_emitter.volts * p.cje * (1.0 - p.wbe),
                dep.external_emitter.derivative * p.cje * (1.0 - p.wbe),
            ),
            qbc: one(qbc.value, qbc.collector_slope),
            qbcx: one(qbcx.value, qbcx.emitter_slope),
            qbep: two(q_parasitic),
            qbcp: one(
                dep.substrate_collector
                    .volts
                    .mul_add(p.cjcp, bias.vbcp * p.ccso),
                dep.substrate_collector.derivative.mul_add(p.cjcp, p.ccso),
            ),
            qbeo: one(bias.vbe * p.cbeo, p.cbeo),
            qbco: one(bias.vbc * p.cbco, p.cbco),
        };
        if ![
            output.qbe.coulombs,
            output.qbe.primary_farads,
            output.qbe.coupled_farads,
            output.qbex.coulombs,
            output.qbex.farads,
            output.qbc.coulombs,
            output.qbc.farads,
            output.qbcx.coulombs,
            output.qbcx.farads,
            output.qbep.coulombs,
            output.qbep.primary_farads,
            output.qbep.coupled_farads,
            output.qbcp.coulombs,
            output.qbcp.farads,
            output.qbeo.coulombs,
            output.qbeo.farads,
            output.qbco.coulombs,
            output.qbco.farads,
        ]
        .iter()
        .all(|v| v.is_finite())
        {
            return Err(EvaluationError::NonFiniteResult);
        }
        Ok(output)
    }

    fn forward_time(
        &self,
        q1: Gradient,
        injected: Gradient,
        collector_bias: f64,
    ) -> Result<Gradient, EvaluationError> {
        let p = self.p;
        if p.tf == 0.0 {
            return Ok(Gradient::constant(0.0));
        }
        let sign = f64::from(injected.value > 0.0);
        let rolloff = injected * (sign * self.inverse_itf);
        let ratio = rolloff / (Gradient::constant(1.0) + rolloff);
        let active = if p.itf > 0.0 {
            ratio * ratio
        } else {
            Gradient::constant(1.0)
        };
        let bias_exponential = if p.xtf == 0.0 || sign == 0.0 {
            Gradient::constant(0.0)
        } else {
            Gradient::new(
                collector_bias * (self.inverse_vtf / 1.44),
                0.0,
                self.inverse_vtf / 1.44,
            )
            .exp()
        };
        let forward_modulation =
            Gradient::constant(1.0) + bias_exponential * (p.xtf * sign) * active;
        let time = (Gradient::constant(1.0) + q1 * p.qtf) * forward_modulation * p.tf;
        if ![time.value, time.emitter_slope, time.collector_slope]
            .iter()
            .all(|value| value.is_finite())
        {
            return Err(EvaluationError::NonFiniteResult);
        }
        Ok(time)
    }

    fn epitaxial_factor(
        &self,
        voltage: f64,
        first_axis: bool,
    ) -> Result<Gradient, EvaluationError> {
        if self.p.qco == 0.0 {
            return Ok(Gradient::constant(0.0));
        }
        if self.p.gamm == 0.0 {
            // The source law sqrt(1 + GAMM*exp(V/Vt)) equals 1 for
            // GAMM=0 at all finite bias; avoid spurious infinity*0.
            return Ok(Gradient::constant(1.0));
        }
        let inverse_vt = 1.0 / self.vt;
        let driving = if first_axis {
            Gradient::new(voltage * inverse_vt, inverse_vt, 0.0)
        } else {
            Gradient::new(voltage * inverse_vt, 0.0, inverse_vt)
        };
        (Gradient::constant(1.0) + driving.exp() * self.p.gamm).sqrt()
    }

    /// Evaluate a homogeneous batch into caller-owned outputs.
    ///
    /// # Errors
    /// Reports incompatible buffers or any invalid numerical device state.
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

const fn gradient(g: transport::ValueAndGradient) -> Gradient {
    Gradient::new(g.value, g.d_vbei, g.d_vbci)
}

const fn two(g: Gradient) -> Charge2 {
    Charge2 {
        coulombs: g.value,
        primary_farads: g.emitter_slope,
        coupled_farads: g.collector_slope,
    }
}

const fn one(coulombs: f64, farads: f64) -> Charge1 {
    Charge1 { coulombs, farads }
}
