//! HICUM/L2 2.4.0 nonlinear intrinsic base resistance and HF shunt Qrbi.
//!
//! Native source HICRBI uses Qjei+Qjci+Qf conductivity modulation, intrinsic
//! Ibei crowding, and FQI-dependent minority charge partition. Its companion
//! Qrbi uses the *source Cjei/Cjci*, low-current transport capacitances,
//! forward/reverse current and the independent internal base branch voltage.
//!
//! No topology reduction or accepted-step ddt happens inside this kernel.

use super::{
    junction,
    thermal::MIN_SOURCE_RESISTANCE,
    transport::{self, jet::Jet},
};
use crate::{
    Branch, EvaluationError, ModelError, Temperature, validate_batch_lengths, validate_nonnegative,
    validate_positive,
};

const SOURCE_K: f64 = 1.380_650_3e-23;
const SOURCE_Q: f64 = 1.602_176_462e-19;

#[derive(Clone, Copy, Debug)]
pub struct Parameters {
    /// Source nominal RBI0 in ohms determines whether the intrinsic
    /// resistor exists. The Verilog-A nominal gate is RBI0 >= 1 mOhm.
    pub nominal_rbi0: f64,
    /// Effective RBI0(Tdev) used to evaluate the physical resistance.
    pub rbi0: f64,
    /// Effective Qp0 (C), from the same prepared HICCR model.
    pub qp0: f64,
    pub fdqr0: f64,
    pub fgeo: f64,
    pub fqi: f64,
    pub fcrbi: f64,
    /// Reverse storage time for Qrbi AC/current-crowding capacitor.
    pub tr: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct Bias {
    /// Internal peripheral-base to intrinsic-base voltage Vbpbi.
    pub vbpbi: f64,
    pub emitter: junction::Junction,
    pub collector: junction::Junction,
    pub transport: transport::Output,
    /// Intrinsic B-E source HICDIO Ibei, with native siemens derivative.
    pub intrinsic_diode: Branch,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Output {
    /// True if the source nominal RBI0=0 requires exact short topology.
    pub shorted: bool,
    /// Nonlinear RBI resistance (ohms) and B-E/B-C derivatives (ohms/V).
    pub resistance: transport::Value2,
    /// Vbpbi/RBI (A) and derivatives in intrinsic junction voltages.
    pub resistor_current: transport::Value2,
    /// Source ∂I/∂Vbpbi, independent of junction derivatives (S).
    pub branch_conductance: f64,
    /// Source Crbi*Vbpbi (C) with B-E/B-C gradients (F).
    pub hf_shunt_charge: transport::Value2,
    /// Source ∂Qrbi/∂Vbpbi, a separate capacitance (F).
    pub hf_branch_capacitance: f64,
}

/// Exact source intrinsic-base temperature-dependent parameter
/// gradients prepared once from RBI0(Tdev) and QP0(Tdev).
#[derive(Clone, Copy, Debug, Default)]
pub struct TemperatureSlopes {
    pub rbi0: f64,
    pub qp0: f64,
}

/// Coupled source input slopes from the already evaluated junction,
/// implicit HICCR and intrinsic HICDIO physical families.
#[derive(Clone, Copy, Debug, Default)]
pub struct TemperatureInputs {
    pub emitter: junction::ForwardSensitivity,
    pub collector: junction::CollectorSensitivity,
    pub intrinsic_diode_current: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TemperatureSensitivity {
    pub resistance: f64,
    pub resistor_current: f64,
    pub branch_conductance: f64,
    pub hf_shunt_charge: f64,
    pub hf_branch_capacitance: f64,
}

impl TemperatureSensitivity {
    /// Exact source FLSH=2 nonlinear intrinsic-base resistor
    /// contribution to fixed-voltage thermal power slope, in W/K.
    /// Other source heat families remain independent contributors.
    #[must_use]
    pub fn base_dissipation_slope(self, vbpbi: f64) -> f64 {
        vbpbi * self.resistor_current
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Prepared {
    p: Parameters,
    vt: f64,
    effective_kelvin: f64,
    temperature_slopes: TemperatureSlopes,
}

impl Prepared {
    /// # Errors
    /// Rejects nonphysical source intrinsic-base parameters or temperature.
    pub fn new(p: Parameters, temperature: Temperature) -> Result<Self, ModelError> {
        Self::with_temperature_slopes(p, temperature, TemperatureSlopes::default())
    }

    /// Source HICRBI nominal-to-effective RBI0 and QP0 thermal slopes.
    ///
    /// # Errors
    /// Rejects invalid source coefficients or nonfinite slopes.
    pub fn with_temperature_slopes(
        p: Parameters,
        temperature: Temperature,
        temperature_slopes: TemperatureSlopes,
    ) -> Result<Self, ModelError> {
        if ![temperature_slopes.rbi0, temperature_slopes.qp0]
            .iter()
            .all(|v| v.is_finite())
        {
            return Err(ModelError::InvalidParameter("rbi_temperature_slopes"));
        }
        validate_nonnegative("rbi0_nominal", p.nominal_rbi0)?;
        validate_nonnegative("rbi0", p.rbi0)?;
        if p.nominal_rbi0 >= MIN_SOURCE_RESISTANCE && p.rbi0 == 0.0 {
            return Err(ModelError::InvalidParameter("rbi0"));
        }
        validate_positive("qp0", p.qp0)?;
        if !p.fdqr0.is_finite() || p.fdqr0 < -0.5 || p.fdqr0 > 100.0 {
            return Err(ModelError::InvalidParameter("fdqr0"));
        }
        validate_nonnegative("fgeo", p.fgeo)?;
        for (name, value) in [("fqi", p.fqi), ("fcrbi", p.fcrbi)] {
            if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                return Err(ModelError::InvalidParameter(name));
            }
        }
        validate_nonnegative("tr", p.tr)?;
        Ok(Self {
            p,
            vt: SOURCE_K / SOURCE_Q * temperature.kelvin(),
            effective_kelvin: temperature.kelvin(),
            temperature_slopes,
        })
    }

    /// Source resistor current and Qrbi; the caller must short the two
    /// base nodes when `Output::shorted` is true.
    ///
    /// # Errors
    /// Rejects nonphysical charge/capacitance and numerical overflow.
    pub fn evaluate(&self, bias: &Bias) -> Result<Output, EvaluationError> {
        self.evaluate_source(bias, TemperatureInputs::default())
            .map(|(operating, _)| operating)
    }

    /// Exact native HICRBI thermal current, resistance and HF shunt
    /// charge derivatives at fixed normalized source branch voltages.
    /// Input thermal slopes must correspond to the same physical
    /// operating point; prepared source model owns RBI0 and QP0 slopes.
    ///
    /// # Errors
    /// Rejects nonfinite source input slopes and physical source overflow.
    pub fn temperature_sensitivity(
        &self,
        bias: &Bias,
        inputs: TemperatureInputs,
    ) -> Result<TemperatureSensitivity, EvaluationError> {
        self.evaluate_source(bias, inputs)
            .map(|(_, sensitivity)| sensitivity)
    }

    fn evaluate_source(
        &self,
        bias: &Bias,
        inputs: TemperatureInputs,
    ) -> Result<(Output, TemperatureSensitivity), EvaluationError> {
        if ![
            inputs.emitter.charge,
            inputs.emitter.capacitance,
            inputs.collector.charge,
            inputs.collector.capacitance,
            inputs.collector.source_capacitance,
            inputs.intrinsic_diode_current,
        ]
        .iter()
        .all(|v| v.is_finite())
        {
            return Err(EvaluationError::NonFiniteBias);
        }
        if ![
            bias.vbpbi,
            bias.emitter.coulombs,
            bias.emitter.farads,
            bias.emitter.source_capacitance,
            bias.emitter.d_source_capacitance,
            bias.collector.coulombs,
            bias.collector.farads,
            bias.collector.source_capacitance,
            bias.collector.d_source_capacitance,
            bias.intrinsic_diode.amperes,
            bias.intrinsic_diode.siemens,
        ]
        .iter()
        .all(|value| value.is_finite())
        {
            return Err(EvaluationError::NonFiniteBias);
        }
        if self.p.nominal_rbi0 < MIN_SOURCE_RESISTANCE {
            return Ok((
                Output {
                    shorted: true,
                    ..Output::default()
                },
                TemperatureSensitivity::default(),
            ));
        }
        let (resistance, crbi) = self.source_physical_jets(bias, inputs)?;
        let conductance = 1.0 / resistance.value;
        let current = Jet::constant(bias.vbpbi) / resistance;
        let shunt = crbi * bias.vbpbi;
        if !current.is_finite()
            || !crbi.is_finite()
            || !shunt.is_finite()
            || !conductance.is_finite()
        {
            return Err(EvaluationError::NonFiniteResult);
        }
        let thermal = TemperatureSensitivity {
            resistance: resistance.temperature,
            resistor_current: current.temperature,
            branch_conductance: -resistance.temperature * conductance * conductance,
            hf_shunt_charge: shunt.temperature,
            hf_branch_capacitance: crbi.temperature,
        };
        if ![
            thermal.resistance,
            thermal.resistor_current,
            thermal.branch_conductance,
            thermal.hf_shunt_charge,
            thermal.hf_branch_capacitance,
        ]
        .iter()
        .all(|v| v.is_finite())
        {
            return Err(EvaluationError::NonFiniteResult);
        }
        Ok((
            Output {
                shorted: false,
                resistance: from_jet(resistance),
                resistor_current: from_jet(current),
                branch_conductance: conductance,
                hf_shunt_charge: from_jet(shunt),
                hf_branch_capacitance: crbi.value,
            },
            thermal,
        ))
    }

    /// Common HICRBI physical charge modulation / resistance and HF
    /// capacitance, evaluated with source voltage and temperature AD.
    fn source_physical_jets(
        &self,
        bias: &Bias,
        inputs: TemperatureInputs,
    ) -> Result<(Jet, Jet), EvaluationError> {
        let p = self.p;
        let slopes = self.temperature_slopes;
        let vt = Jet::thermal_constant(self.vt, self.vt / self.effective_kelvin);
        let emitter_charge = Jet::new(bias.emitter.coulombs, bias.emitter.farads, 0.0, 0.0)
            .with_temperature(inputs.emitter.charge);
        let collector_charge = Jet::new(bias.collector.coulombs, 0.0, bias.collector.farads, 0.0)
            .with_temperature(inputs.collector.charge);
        let forward = to_jet(bias.transport.forward_diffusion_charge).with_temperature(
            bias.transport
                .temperature_sensitivity
                .forward_diffusion_charge,
        );
        let divisor = Jet::thermal_constant((1.0 + p.fdqr0) * p.qp0, (1.0 + p.fdqr0) * slopes.qp0);
        let normalized =
            Jet::constant(1.0) + (emitter_charge + collector_charge + forward) / divisor;
        let smoothed = (normalized + (normalized * normalized + 0.01).sqrt()) * 0.5;
        let mut resistance = Jet::thermal_constant(p.rbi0, slopes.rbi0) / smoothed;
        if bias.intrinsic_diode.amperes > 0.0 && p.fgeo > 0.0 {
            let diode = Jet::new(
                bias.intrinsic_diode.amperes,
                bias.intrinsic_diode.siemens,
                0.0,
                0.0,
            )
            .with_temperature(inputs.intrinsic_diode_current);
            let eta = resistance * diode * (Jet::constant(p.fgeo) / vt);
            let crowding = if eta.value < 1e-6 {
                Jet::constant(1.0) - eta * 0.5
            } else {
                eta.ln_1p() / eta
            };
            resistance = resistance * crowding;
        }
        if forward.value > 0.0 {
            let denominator = emitter_charge + forward;
            if denominator.value == 0.0 {
                return Err(EvaluationError::InvalidPhysicalDomain);
            }
            resistance = resistance * ((emitter_charge + forward * p.fqi) / denominator);
        }
        if !resistance.is_finite() || resistance.value <= 0.0 {
            return Err(EvaluationError::InvalidPhysicalDomain);
        }
        // For the HF base shunt, the source uses explicitly reported
        // Cjei/Cjci, not the second derivative of their physical charges.
        let emitter_cap = Jet::new(
            bias.emitter.source_capacitance,
            bias.emitter.d_source_capacitance,
            0.0,
            0.0,
        )
        .with_temperature(inputs.emitter.capacitance);
        let collector_cap = Jet::new(
            bias.collector.source_capacitance,
            0.0,
            bias.collector.d_source_capacitance,
            0.0,
        )
        .with_temperature(inputs.collector.source_capacitance);
        let source = bias.transport.temperature_sensitivity;
        let low_time = to_jet(bias.transport.low_current_transit_time)
            .with_temperature(source.low_current_transit_time);
        let transfer =
            to_jet(bias.transport.forward_current).with_temperature(source.forward_current);
        let reverse =
            to_jet(bias.transport.reverse_current).with_temperature(source.reverse_current);
        let crbi = (emitter_cap
            + collector_cap
            + low_time * transfer / vt
            + reverse * (Jet::constant(p.tr) / vt))
            * p.fcrbi;
        Ok((resistance, crbi))
    }

    /// # Errors
    /// Reports a batch mismatch or an invalid HICUM internal base state.
    pub fn evaluate_batch(
        &self,
        inputs: &[Bias],
        outputs: &mut [Output],
    ) -> Result<(), EvaluationError> {
        validate_batch_lengths(inputs.len(), outputs.len())?;
        for (input, output) in inputs.iter().zip(outputs) {
            *output = self.evaluate(input)?;
        }
        Ok(())
    }
}

const fn to_jet(value: transport::Value2) -> Jet {
    Jet::new(value.value, value.d_vbiei, value.d_vbici, 0.0)
}

const fn from_jet(value: Jet) -> transport::Value2 {
    transport::Value2 {
        value: value.value,
        d_vbiei: value.be,
        d_vbici: value.bc,
    }
}
