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

use super::{parasitic, transport};
use crate::{
    EvaluationError, ModelError, Temperature, validate_batch_lengths, validate_nonnegative,
};

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
    vt: f64,
    inverse_rcx: f64,
    inverse_rci: f64,
    inverse_rbx: f64,
    inverse_rbi: f64,
    inverse_re: f64,
    inverse_rbp: f64,
    inverse_rs: f64,
    inverse_vo: f64,
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
            vt: (1.380_662e-23 / 1.602_189e-19) * temperature.kelvin(),
            inverse_rcx: reciprocal(p.rcx),
            inverse_rci: reciprocal(p.rci),
            inverse_rbx: reciprocal(p.rbx),
            inverse_rbi: reciprocal(p.rbi),
            inverse_re: reciprocal(p.re),
            inverse_rbp: reciprocal(p.rbp),
            inverse_rs: reciprocal(p.rs),
            inverse_vo: reciprocal(p.vo),
            inverse_hrcf: reciprocal(p.hrcf),
        };
        if ![
            result.inverse_rcx,
            result.inverse_rci,
            result.inverse_rbx,
            result.inverse_rbi,
            result.inverse_re,
            result.inverse_rbp,
            result.inverse_rs,
            result.inverse_vo,
            result.inverse_hrcf,
        ]
        .iter()
        .all(|x| x.is_finite())
        {
            return Err(ModelError::InvalidParameter("resistance reciprocal"));
        }
        Ok(result)
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
        let base_scale = bias.vrbi * self.inverse_rbi;
        let peripheral_scale = bias.vrbp * self.inverse_rbp;
        let result = Output {
            ircx: linear(bias.vrcx, self.inverse_rcx),
            irci: self.epitaxial_current(bias.vrci, bias.vbci, bias.vbcx)?,
            irbx: linear(bias.vrbx, self.inverse_rbx),
            irbi: Current3 {
                amperes: base_scale * base_charge.value,
                d_resistor: self.inverse_rbi * base_charge.value,
                d_first: base_scale * base_charge.d_vbei,
                d_second: base_scale * base_charge.d_vbci,
            },
            ire: linear(bias.vre, self.inverse_re),
            irbp: Current3 {
                amperes: peripheral_scale * peripheral_charge.qb,
                d_resistor: self.inverse_rbp * peripheral_charge.qb,
                d_first: peripheral_scale * peripheral_charge.qb_vbep,
                d_second: peripheral_scale * peripheral_charge.qb_vbci,
            },
            irs: linear(bias.vrs, self.inverse_rs),
        };
        if ![
            result.ircx.amperes,
            result.ircx.siemens,
            result.irci.amperes,
            result.irci.d_resistor,
            result.irci.d_first,
            result.irci.d_second,
            result.irbx.amperes,
            result.irbx.siemens,
            result.irbi.amperes,
            result.irbi.d_resistor,
            result.irbi.d_first,
            result.irbi.d_second,
            result.ire.amperes,
            result.ire.siemens,
            result.irbp.amperes,
            result.irbp.d_resistor,
            result.irbp.d_first,
            result.irbp.d_second,
            result.irs.amperes,
            result.irs.siemens,
        ]
        .iter()
        .all(|x| x.is_finite())
        {
            return Err(EvaluationError::NonFiniteResult);
        }
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
        let p = self.p;
        let vt = self.vt;
        let (intrinsic_factor, intrinsic_slope) = self.collector_factor(collector_intrinsic);
        let (external_factor, external_slope) = self.collector_factor(collector_external);
        let intrinsic_phi = intrinsic_factor * intrinsic_slope / (intrinsic_factor + 1.0);
        let external_phi = -external_factor * external_slope / (external_factor + 1.0);
        let logarithm = intrinsic_factor.ln_1p() - external_factor.ln_1p();
        let potential = intrinsic_factor - external_factor - logarithm;
        let ohmic = self.inverse_rci * (vt.mul_add(potential, resistor_voltage));
        let ohmic_resistor_slope = self.inverse_rci;
        let ohmic_first_slope = self.inverse_rci * vt * intrinsic_phi;
        let ohmic_second_slope = self.inverse_rci * vt * external_phi;

        let voltage_ratio = self.inverse_vo * p.rci;
        let regularizer = resistor_voltage.hypot(0.1);
        let saturation_factor = 0.5 * self.inverse_vo * self.inverse_hrcf;
        let denominator = saturation_factor.mul_add(regularizer, 1.0);
        let numerator = voltage_ratio * ohmic;
        let rolloff = numerator / denominator;
        let rolloff_resistor_slope = voltage_ratio.mul_add(
            ohmic_resistor_slope,
            -(rolloff * saturation_factor * resistor_voltage / regularizer),
        ) / denominator;
        let rolloff_first_slope = voltage_ratio * ohmic_first_slope / denominator;
        let rolloff_second_slope = voltage_ratio * ohmic_second_slope / denominator;
        let norm = rolloff.hypot(1.0);
        let norm_cubed = norm * norm * norm;
        let derivative = |ohmic_slope: f64, rolloff_slope: f64| {
            ohmic_slope / norm - ohmic * rolloff * rolloff_slope / norm_cubed
        };
        let result = Current3 {
            amperes: ohmic / norm,
            d_resistor: derivative(ohmic_resistor_slope, rolloff_resistor_slope),
            d_first: derivative(ohmic_first_slope, rolloff_first_slope),
            d_second: derivative(ohmic_second_slope, rolloff_second_slope),
        };
        if ![
            result.amperes,
            result.d_resistor,
            result.d_first,
            result.d_second,
        ]
        .iter()
        .all(|x| x.is_finite())
        {
            return Err(EvaluationError::NonFiniteResult);
        }
        Ok(result)
    }

    fn collector_factor(&self, voltage: f64) -> (f64, f64) {
        if self.p.gamm == 0.0 {
            return (1.0, 0.0);
        }
        let exponential = (voltage / self.vt).exp();
        let factor = self.p.gamm.mul_add(exponential, 1.0).sqrt();
        let slope = self.p.gamm * exponential / (2.0 * factor * self.vt);
        (factor, slope)
    }

    /// Evaluate identical prepared parameters over a borrowed bias batch.
    ///
    /// # Errors
    /// Reports mismatched slices or any individual numerical failure.
    pub fn evaluate_batch(
        &self,
        inputs: &[Bias],
        outputs: &mut [Output],
    ) -> Result<(), EvaluationError> {
        validate_batch_lengths(inputs.len(), outputs.len())?;
        for (bias, output) in inputs.iter().zip(outputs) {
            *output = self.evaluate(bias)?;
        }
        Ok(())
    }
}

const fn linear(voltage: f64, conductance: f64) -> Current1 {
    Current1 {
        amperes: voltage * conductance,
        siemens: conductance,
    }
}
