//! VBIC 1.1.5 physical junction, transit and overlap charges.
//!
//! Implements Qbe, Qbex, Qbc, Qbcx, Qbep, Qbcp, Qbeo and Qbco from
//! ngspice VBIC `vbic_4T_it_cf_fj`, separately from the normalized depletion
//! integrals (qdbe, qdbc, ...) which have units of volts.
//! Outputs are **Coulombs** and analytic first derivatives in **Farads**;
//! the solver performs circuit charge integration and accepted-step isolation.

use super::{
    depletion, parasitic,
    temperature::{self, Coefficient as C},
    transport,
};
use crate::{EvaluationError, ModelError, Temperature, evaluate_lanes, validate_nonnegative};

use transport::{CheckedGradient, Gradient};

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
    pub d_temperature: f64,
}

/// Physical charge and partial derivatives wrt two explicitly named biases.
/// The two bias meanings depend on the branch and are documented below.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Charge2 {
    pub coulombs: f64,
    pub primary_farads: f64,
    pub coupled_farads: f64,
    pub d_temperature: f64,
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

pub(super) struct Physics {
    pub charge: Output,
    pub transport: transport::Output,
    pub parasitic: parasitic::Output,
}

#[derive(Clone, Copy, Debug)]
pub struct Prepared {
    p: Parameters,
    inverse_vtf: f64,
    inverse_itf: f64,
    vt: Gradient,
    gamm: Gradient,
    cje: Gradient,
    cjc: Gradient,
    cjep: Gradient,
    cjcp: Gradient,
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
            vt: Gradient::constant(temperature::thermal_voltage(temperature)),
            gamm: Gradient::constant(p.gamm),
            cje: Gradient::constant(p.cje),
            cjc: Gradient::constant(p.cjc),
            cjep: Gradient::constant(p.cjep),
            cjcp: Gradient::constant(p.cjcp),
            transport: transport::Prepared::new(p.transport, temperature)?,
            parasitic: parasitic::Prepared::new(p.parasitic, temperature)?,
            depletion: depletion::ExtendedPrepared::new(p.depletion, p.substrate)?,
        })
    }

    pub(super) fn from_temperature(t: &temperature::Prepared) -> Result<Self, ModelError> {
        let mut prepared = Self::new(&t.charge_parameters(), t.temperature())?;
        prepared.vt = t.voltage_jet();
        prepared.gamm = t.jet(C::Gamm);
        prepared.cje = t.jet(C::Cje);
        prepared.cjc = t.jet(C::Cjc);
        prepared.cjep = t.jet(C::Cjep);
        prepared.cjcp = t.jet(C::Cjcp);
        prepared.transport = transport::Prepared::from_temperature(t)?;
        prepared.parasitic = parasitic::Prepared::from_temperature(t)?;
        prepared.depletion = depletion::ExtendedPrepared::from_temperature(t)?;
        Ok(prepared)
    }

    /// Evaluates all eight VBIC charge outputs. No history is mutated.
    /// The caller integrates Q and dQ/dV using its own time discretization.
    ///
    /// # Errors
    /// Returns an invalid voltage/domain or numerical overflow error.
    pub fn evaluate(&self, bias: Bias) -> Result<Output, EvaluationError> {
        Ok(self.evaluate_physics(bias)?.charge)
    }

    // Charge, transport, and parasitic branches share the same evaluated
    // depletion integrals and minority-carrier state.
    pub(super) fn evaluate_physics(&self, bias: Bias) -> Result<Physics, EvaluationError> {
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
        let emitter_depletion = Gradient::new(dep.intrinsic.qdbe, dep.intrinsic.cbe, 0.0, 0.0)
            .with_temperature(dep.intrinsic.d_qdbe_temperature);
        let collector_depletion = Gradient::new(dep.intrinsic.qdbc, 0.0, dep.intrinsic.cbc, 0.0)
            .with_temperature(dep.intrinsic.d_qdbc_temperature);
        let intrinsic_emitter = emitter_depletion * self.cje * p.wbe + transit * forward;
        let intrinsic_collector = collector_depletion * self.cjc
            + reverse_injection * p.tr
            + self.epitaxial_factor(bias.vbci, false)? * p.qco;
        let epitaxial_external = self.epitaxial_factor(bias.vbcx, true)? * p.qco;
        let ifp = Gradient::new(
            para.ideal_forward,
            para.ideal_forward_vbep,
            para.ideal_forward_vbci,
            0.0,
        )
        .with_temperature(para.ideal_forward_temperature);
        let peripheral_collector =
            depletion_gradient(dep.peripheral_collector) * self.cjep + ifp * p.tr;
        let external_emitter = depletion_gradient(dep.external_emitter) * self.cje * (1.0 - p.wbe);
        let substrate_charge = depletion_gradient(dep.substrate_collector) * self.cjcp
            + Gradient::emitter(bias.vbcp) * p.ccso;
        let emitter_overlap = Gradient::emitter(bias.vbe) * p.cbeo;
        let collector_overlap = Gradient::emitter(bias.vbc) * p.cbco;
        if ![
            intrinsic_emitter,
            intrinsic_collector,
            epitaxial_external,
            peripheral_collector,
            external_emitter,
            substrate_charge,
            emitter_overlap,
            collector_overlap,
        ]
        .into_iter()
        .all(Gradient::is_finite)
        {
            return Err(EvaluationError::NonFiniteResult);
        }
        let output = Output {
            qbe: two(intrinsic_emitter),
            qbex: one(external_emitter, external_emitter.be),
            qbc: one(intrinsic_collector, intrinsic_collector.bc),
            qbcx: one(epitaxial_external, epitaxial_external.be),
            qbep: two(peripheral_collector),
            qbcp: one(substrate_charge, substrate_charge.be),
            qbeo: one(emitter_overlap, emitter_overlap.be),
            qbco: one(collector_overlap, collector_overlap.be),
        };
        Ok(Physics {
            charge: output,
            transport: trans,
            parasitic: para,
        })
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
                0.0,
            )
            .exp()
        };
        let forward_modulation =
            Gradient::constant(1.0) + bias_exponential * (p.xtf * sign) * active;
        let time = (Gradient::constant(1.0) + q1 * p.qtf) * forward_modulation * p.tf;
        if !time.is_finite() {
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
        let driving = if first_axis {
            Gradient::emitter(voltage)
        } else {
            Gradient::collector(voltage)
        } / self.vt;
        (Gradient::constant(1.0) + driving.exp() * self.gamm).checked_sqrt()
    }

    /// Evaluate a homogeneous batch into caller-owned outputs.
    ///
    /// All slice lengths are checked before any writes. Lanes run in order;
    /// the first error leaves that lane and the remaining outputs unchanged.
    ///
    /// # Errors
    /// Reports incompatible buffers or any invalid numerical device state.
    pub fn evaluate_batch(
        &self,
        inputs: &[Bias],
        outputs: &mut [Output],
    ) -> Result<(), EvaluationError> {
        evaluate_lanes(inputs.iter().copied(), outputs, |bias| self.evaluate(bias))
    }
}

const fn gradient(g: transport::ValueAndGradient) -> Gradient {
    Gradient::new(g.value, g.d_vbei, g.d_vbci, 0.0).with_temperature(g.d_temperature)
}

const fn two(g: Gradient) -> Charge2 {
    Charge2 {
        coulombs: g.value,
        primary_farads: g.be,
        coupled_farads: g.bc,
        d_temperature: g.temperature,
    }
}

const fn one(g: Gradient, farads: f64) -> Charge1 {
    Charge1 {
        coulombs: g.value,
        farads,
        d_temperature: g.temperature,
    }
}

const fn depletion_gradient(d: depletion::Normalized) -> Gradient {
    Gradient::new(d.volts, d.derivative, 0.0, 0.0).with_temperature(d.d_temperature)
}
