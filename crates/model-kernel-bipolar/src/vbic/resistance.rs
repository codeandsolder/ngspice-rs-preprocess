//! VBIC 1.1.5 complete resistive branch laws, including RCI, RBI and RBP.
//!
//! The seven branch currents are Ircx, Irci, Irbx, Irbi, Ire, Irbp, Irs.
//! Nonlinear epitaxial collector resistance uses the exact VBIC Kbci/Kbcx
//! logarithmic correction, velocity saturation and high-current-limited
//! derivative. Internal and peripheral base resistances are modulated by
//! prepared qB and qBp including intrinsic cross derivatives.
//!
//! The circuit solver owns topology reduction for zero-value resistances,
//! nodal current injection, and accepted/trial state handling.

use super::{
    parasitic,
    temperature::{self, Coefficient as C},
    transport::{self, CheckedGradient, Gradient},
};
use crate::{EvaluationError, ModelError, Temperature, evaluate_lanes, validate_nonnegative};

#[derive(Clone, Copy, Debug)]
pub struct Parameters {
    /// External collector series resistance (RCX).
    pub rcx: f64,
    /// Epitaxial internal collector resistance (RCI).
    pub rci: f64,
    /// Internal collector velocity scale (VO).
    pub vo: f64,
    /// Doping factor for epitaxial Kbci and Kbcx (GAMM).
    pub gamm: f64,
    /// High-current collector resistance factor (HRCF).
    pub hrcf: f64,
    /// External base resistance (RBX).
    pub rbx: f64,
    /// Charge-modulated intrinsic base resistance (RBI).
    pub rbi: f64,
    /// Emitter resistance (RE).
    pub re: f64,
    /// Charge-modulated peripheral base resistance (RBP).
    pub rbp: f64,
    /// Substrate resistance (RS).
    pub rs: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct Bias {
    pub vrcx: f64,
    pub vrci: f64,
    pub vbci: f64,
    pub vbcx: f64,
    pub vrbx: f64,
    pub vrbi: f64,
    pub vre: f64,
    pub vrbp: f64,
    pub vrs: f64,
    pub transport: transport::Output,
    pub parasitic: parasitic::Output,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Current1 {
    pub amperes: f64,
    /// Local dI/dV in siemens.
    pub siemens: f64,
    pub d_temperature: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Current3 {
    pub amperes: f64,
    /// Local derivative wrt the branch's own voltage (R).
    pub d_resistor: f64,
    /// Local derivative wrt first coupled voltage.
    pub d_first: f64,
    /// Local derivative wrt second coupled voltage.
    pub d_second: f64,
    pub d_temperature: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Output {
    pub ircx: Current1,
    /// Irci: own Vrci, first Vbci, second Vbcx.
    pub irci: Current3,
    pub irbx: Current1,
    /// Irbi: own Vrbi, first Vbei, second Vbci.
    pub irbi: Current3,
    pub ire: Current1,
    /// Irbp: own Vrbp, first Vbep, second Vbci.
    pub irbp: Current3,
    pub irs: Current1,
}

#[derive(Clone, Copy, Debug)]
pub struct Prepared {
    p: Parameters,
    vt: Gradient,
    gamm: Gradient,
    rci: Gradient,
    inverse_rcx: Gradient,
    inverse_rci: Gradient,
    inverse_rbx: Gradient,
    inverse_rbi: Gradient,
    inverse_re: Gradient,
    inverse_rbp: Gradient,
    inverse_rs: Gradient,
    inverse_vo: Gradient,
    inverse_hrcf: f64,
}

impl Prepared {
    /// # Errors
    /// Rejects any invalid prepared resistance parameter before evaluation.
    pub fn new(p: Parameters, temperature: Temperature) -> Result<Self, ModelError> {
        for (name, value) in [
            ("rcx", p.rcx),
            ("rci", p.rci),
            ("vo", p.vo),
            ("gamm", p.gamm),
            ("hrcf", p.hrcf),
            ("rbx", p.rbx),
            ("rbi", p.rbi),
            ("re", p.re),
            ("rbp", p.rbp),
            ("rs", p.rs),
        ] {
            validate_nonnegative(name, value)?;
        }
        let reciprocal = |value: f64| if value == 0.0 { 0.0 } else { 1.0 / value };
        let result = Self {
            p,
            vt: Gradient::constant(temperature::thermal_voltage(temperature)),
            gamm: Gradient::constant(p.gamm),
            rci: Gradient::constant(p.rci),
            inverse_rcx: Gradient::constant(reciprocal(p.rcx)),
            inverse_rci: Gradient::constant(reciprocal(p.rci)),
            inverse_rbx: Gradient::constant(reciprocal(p.rbx)),
            inverse_rbi: Gradient::constant(reciprocal(p.rbi)),
            inverse_re: Gradient::constant(reciprocal(p.re)),
            inverse_rbp: Gradient::constant(reciprocal(p.rbp)),
            inverse_rs: Gradient::constant(reciprocal(p.rs)),
            inverse_vo: Gradient::constant(reciprocal(p.vo)),
            inverse_hrcf: reciprocal(p.hrcf),
        };
        if ![
            result.inverse_rcx.value,
            result.inverse_rci.value,
            result.inverse_rbx.value,
            result.inverse_rbi.value,
            result.inverse_re.value,
            result.inverse_rbp.value,
            result.inverse_rs.value,
            result.inverse_vo.value,
            result.inverse_hrcf,
        ]
        .iter()
        .all(|x| x.is_finite())
        {
            return Err(ModelError::InvalidParameter("resistance reciprocal"));
        }
        Ok(result)
    }

    pub(super) fn from_temperature(t: &temperature::Prepared) -> Result<Self, ModelError> {
        let mut prepared = Self::new(t.resistance_parameters(), t.temperature())?;
        prepared.vt = t.voltage_jet();
        prepared.gamm = t.jet(C::Gamm);
        prepared.rci = t.jet(C::Rci);
        prepared.inverse_rcx = temperature::inverse_enabled(t.jet(C::Rcx));
        prepared.inverse_rci = temperature::inverse_enabled(t.jet(C::Rci));
        prepared.inverse_rbx = temperature::inverse_enabled(t.jet(C::Rbx));
        prepared.inverse_rbi = temperature::inverse_enabled(t.jet(C::Rbi));
        prepared.inverse_re = temperature::inverse_enabled(t.jet(C::Re));
        prepared.inverse_rbp = temperature::inverse_enabled(t.jet(C::Rbp));
        prepared.inverse_rs = temperature::inverse_enabled(t.jet(C::Rs));
        prepared.inverse_vo = temperature::inverse_enabled(t.jet(C::Vo));
        Ok(prepared)
    }

    /// Evaluates all seven branch currents and local first partial derivatives.
    ///
    /// # Errors
    /// Returns a numerical error for nonfinite input or output, including
    /// supplied state from the transfer and parasitic charge subkernels.
    pub fn evaluate(&self, bias: &Bias) -> Result<Output, EvaluationError> {
        if ![
            bias.vrcx, bias.vrci, bias.vbci, bias.vbcx, bias.vrbx, bias.vrbi, bias.vre, bias.vrbp,
            bias.vrs,
        ]
        .iter()
        .all(|x| x.is_finite())
        {
            return Err(EvaluationError::NonFiniteBias);
        }
        let base_charge = bias.transport.qb;
        let peripheral_charge = bias.parasitic;
        if ![
            base_charge.value,
            base_charge.d_vbei,
            base_charge.d_vbci,
            peripheral_charge.qb,
            peripheral_charge.qb_vbep,
            peripheral_charge.qb_vbci,
        ]
        .iter()
        .all(|x| x.is_finite())
        {
            return Err(EvaluationError::NonFiniteBias);
        }
        if (self.p.rbi > 0.0 && base_charge.value <= 0.0)
            || (self.p.rbp > 0.0 && peripheral_charge.qb <= 0.0)
        {
            return Err(EvaluationError::InvalidPhysicalDomain);
        }
        let base = Gradient::new(
            base_charge.value,
            0.0,
            base_charge.d_vbei,
            base_charge.d_vbci,
        )
        .with_temperature(base_charge.d_temperature);
        let peripheral = Gradient::new(
            peripheral_charge.qb,
            0.0,
            peripheral_charge.qb_vbep,
            peripheral_charge.qb_vbci,
        )
        .with_temperature(peripheral_charge.qb_temperature);
        let internal_base = Gradient::emitter(bias.vrbi) * self.inverse_rbi * base;
        let peripheral_base = Gradient::emitter(bias.vrbp) * self.inverse_rbp * peripheral;
        let result = Output {
            ircx: linear(bias.vrcx, self.inverse_rcx)?,
            irci: self.epitaxial_current(bias.vrci, bias.vbci, bias.vbcx)?,
            irbx: linear(bias.vrbx, self.inverse_rbx)?,
            irbi: nonlinear(internal_base)?,
            ire: linear(bias.vre, self.inverse_re)?,
            irbp: nonlinear(peripheral_base)?,
            irs: linear(bias.vrs, self.inverse_rs)?,
        };
        Ok(result)
    }

    fn epitaxial_current(
        &self,
        resistor_voltage: f64,
        collector_intrinsic: f64,
        collector_external: f64,
    ) -> Result<Current3, EvaluationError> {
        if self.p.rci == 0.0 {
            return Ok(Current3::default());
        }
        let voltage = Gradient::emitter(resistor_voltage);
        let intrinsic = self.collector_factor(Gradient::collector(collector_intrinsic))?;
        let external = self.collector_factor(Gradient::charge(collector_external))?;
        let potential = intrinsic - external - (intrinsic.ln_1p() - external.ln_1p());
        let ohmic = self.inverse_rci * (self.vt * potential + voltage);
        let regularizer = (voltage * voltage + 0.01).checked_sqrt()?;
        let denominator = self.inverse_vo * (0.5 * self.inverse_hrcf) * regularizer + 1.0;
        let rolloff = self.inverse_vo * self.rci * ohmic / denominator;
        let norm = (rolloff * rolloff + 1.0).checked_sqrt()?;
        nonlinear(ohmic / norm)
    }

    fn collector_factor(&self, voltage: Gradient) -> Result<Gradient, EvaluationError> {
        if self.p.gamm == 0.0 {
            return Ok(Gradient::constant(1.0));
        }
        (self.gamm * (voltage / self.vt).exp() + 1.0).checked_sqrt()
    }

    /// Evaluate identical prepared parameters over a borrowed bias batch.
    ///
    /// All slice lengths are checked before any writes. Lanes run in order;
    /// the first error leaves that lane and the remaining outputs unchanged.
    ///
    /// # Errors
    /// Reports mismatched slices or any individual numerical failure.
    pub fn evaluate_batch(
        &self,
        inputs: &[Bias],
        outputs: &mut [Output],
    ) -> Result<(), EvaluationError> {
        evaluate_lanes(inputs.iter(), outputs, |bias| self.evaluate(bias))
    }
}

fn linear(voltage: f64, conductance: Gradient) -> Result<Current1, EvaluationError> {
    let value = Gradient::emitter(voltage) * conductance;
    if !value.is_finite() {
        return Err(EvaluationError::NonFiniteResult);
    }
    Ok(Current1 {
        amperes: value.value,
        siemens: value.be,
        d_temperature: value.temperature,
    })
}

fn nonlinear(value: Gradient) -> Result<Current3, EvaluationError> {
    if !value.is_finite() {
        return Err(EvaluationError::NonFiniteResult);
    }
    Ok(Current3 {
        amperes: value.value,
        d_resistor: value.be,
        d_first: value.bc,
        d_second: value.hole,
        d_temperature: value.temperature,
    })
}
