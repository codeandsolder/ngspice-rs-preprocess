//! HICUM/L2 2.4.0 self-heating power and thermal RC branch.
//!
//! Reproduces source FLSH=0/1/2, nominal RTH branch threshold 1mohm,
//! Tdev-adjusted Rth, and explicit thermal-node charge heat capacity times rise.
//! All individual electrical power and resistor sensitivities are
//! returned separately so the simulator can chain coupled Jacobians.
//! The solver owns thermal-node allocation and integration.

use crate::{
    EvaluationError, ModelError, Temperature, validate_batch_lengths, validate_nonnegative,
    validate_positive,
};

/// Source minimum resistor threshold used for thermal and series branches.
pub const MIN_SOURCE_RESISTANCE: f64 = 0.001;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Disabled,
    /// FLSH=1: transfer + avalanche generation heating only.
    TransferAvalanche,
    /// FLSH=2: include five diode terms and four resistive branches.
    FullElectrical,
}

#[derive(Clone, Copy, Debug)]
pub struct Parameters {
    pub mode: Mode,
    /// Source nominal RTH selects whether the *thermal network exists*.
    pub nominal_resistance: f64,
    /// Effective RTH(Tdev) computed by shared temperature preparation.
    pub effective_resistance: f64,
    /// Physical heat capacity; energy state is capacity times thermal rise.
    pub heat_capacity: f64,
    /// Current Tdev after source clamps.
    pub effective_temperature: Temperature,
    pub nominal_temperature: Temperature,
    pub zetarth: f64,
    pub alrth: f64,
    /// Current voltage on the thermal-node branch (kelvin).
    pub thermal_rise: f64,
    /// dTdev/dVthermal is one within the source Tdev limits and zero
    /// when clamped. Includes source FLSH/RTH physical branch gate.
    pub temperature_slope: f64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Terminal {
    /// Terminal voltage across the source current branch (V).
    pub voltage: f64,
    /// Source branch current (A), with native sign, not abs(current).
    pub current: f64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Avalanche {
    /// Source effective internal B-C built-in potential from the source temperature update (V).
    pub collector_potential: f64,
    /// Source intrinsic B-C voltage Vbici (V).
    pub collector_voltage: f64,
    /// Nonnegative generated HICAVL current (A).
    pub current: f64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Series {
    pub voltage: f64,
    /// *Effective* nonlinear RBI or constant effective RE/RCX/RBX.
    /// Subthreshold resistances are excluded from dissipated power.
    pub resistance: f64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ElectricalBias {
    /// Vciei * It, with source net transfer It=Itf-Itr, not delayed Itxf.
    pub transport: Terminal,
    /// Collector built-in potential minus B-C bias, multiplied by avalanche current.
    pub avalanche: Avalanche,
    /// Source FLSH=2 *only* Ibei,Ibci,Ibep,Ijbcx,Ijsc; no substitute
    /// reconstruction from terminal currents or excess recombination.
    pub intrinsic_emitter: Terminal,
    pub intrinsic_collector: Terminal,
    pub peripheral_emitter: Terminal,
    pub peripheral_collector: Terminal,
    pub substrate: Terminal,
    pub intrinsic_base: Series,
    pub external_emitter: Series,
    pub external_collector: Series,
    pub external_base: Series,
}

/// A V*I power term, with exact independent partials in watts/volt
/// and watts/ampere. Callers chain actual current/voltage Jacobians.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ProductPower {
    pub watts: f64,
    pub d_voltage: f64,
    pub d_current: f64,
}

/// A V²/R power term, with exact independent partials including dP/dR.
/// A source-absent or shorted branch has no dissipated power.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ResistorPower {
    pub watts: f64,
    pub d_voltage: f64,
    pub d_resistance: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ElectricalPower {
    pub watts: f64,
    pub transport: ProductPower,
    /// The voltage derivative is with respect to avalanche voltage headroom.
    pub avalanche: ProductPower,
    pub intrinsic_emitter: ProductPower,
    pub intrinsic_collector: ProductPower,
    pub peripheral_emitter: ProductPower,
    pub peripheral_collector: ProductPower,
    pub substrate: ProductPower,
    pub intrinsic_base: ResistorPower,
    pub external_emitter: ResistorPower,
    pub external_collector: ResistorPower,
    pub external_base: ResistorPower,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Output {
    /// Disabled heating or nominal resistance below threshold: ideal short,
    /// not an arbitrarily large conductance.
    pub shorted: bool,
    /// Source pterm in watts. Exact individual powers/partials above.
    pub electrical: ElectricalPower,
    /// Source thermal KCL static current = rise/RTH(Tdev)-pterm (W).
    pub thermal_current: f64,
    /// At *fixed electrical branch values*, exact partial derivative
    /// with respect to thermal node, including dRTH(Tdev)/dTnode.
    /// The full coupled Jacobian must also chain dP/dTdev through
    /// the device electrical models (not implicitly assumed zero).
    pub d_thermal_current_d_rise_at_fixed_electrical: f64,
    /// Dynamic thermal energy, capacity times local temperature rise (J).
    pub heat_charge: f64,
    pub heat_capacitance: f64,
    pub d_tdev_d_rise: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct Prepared {
    p: Parameters,
    enabled: bool,
    d_rth_d_rise: f64,
}

impl Prepared {
    /// # Errors
    /// Rejects physically invalid RTH/CTH and nonfinite temperature
    /// derivatives, not just the nominal thermal network values.
    pub fn new(p: Parameters) -> Result<Self, ModelError> {
        validate_nonnegative("rth", p.nominal_resistance)?;
        validate_nonnegative("cth", p.heat_capacity)?;
        if ![
            p.effective_resistance,
            p.zetarth,
            p.alrth,
            p.thermal_rise,
            p.temperature_slope,
        ]
        .iter()
        .all(|value| value.is_finite())
        {
            return Err(ModelError::InvalidParameter("thermal"));
        }
        if !(0.0..=1.0).contains(&p.temperature_slope) {
            return Err(ModelError::InvalidParameter("temperature_slope"));
        }
        let enabled = p.mode != Mode::Disabled && p.nominal_resistance >= MIN_SOURCE_RESISTANCE;
        let d_rth_d_rise = if enabled {
            validate_positive("rth_t", p.effective_resistance)?;
            let nominal_kelvin = p.nominal_temperature.kelvin();
            let effective_kelvin = p.effective_temperature.kelvin();
            let ratio = effective_kelvin / nominal_kelvin;
            let delta = effective_kelvin - nominal_kelvin;
            let resistance_scale = p.nominal_resistance * ratio.powf(p.zetarth);
            let linear_factor = p.alrth.mul_add(delta, 1.0);
            let gradient = resistance_scale
                * ((p.zetarth / effective_kelvin).mul_add(linear_factor, p.alrth))
                * p.temperature_slope;
            if !gradient.is_finite() {
                return Err(ModelError::InvalidParameter("rth_derivative"));
            }
            gradient
        } else {
            0.0
        };
        Ok(Self {
            p,
            enabled,
            d_rth_d_rise,
        })
    }

    /// # Errors
    /// Invalid nonfinite electrical sources, negative resistor values,
    /// negative avalanche generation, or thermal numerical overflow.
    pub fn evaluate(&self, bias: ElectricalBias) -> Result<Output, EvaluationError> {
        if !self.enabled {
            return Ok(Output {
                shorted: true,
                ..Output::default()
            });
        }
        let input = [
            bias.transport.voltage,
            bias.transport.current,
            bias.avalanche.collector_potential,
            bias.avalanche.collector_voltage,
            bias.avalanche.current,
            bias.intrinsic_emitter.voltage,
            bias.intrinsic_emitter.current,
            bias.intrinsic_collector.voltage,
            bias.intrinsic_collector.current,
            bias.peripheral_emitter.voltage,
            bias.peripheral_emitter.current,
            bias.peripheral_collector.voltage,
            bias.peripheral_collector.current,
            bias.substrate.voltage,
            bias.substrate.current,
            bias.intrinsic_base.voltage,
            bias.intrinsic_base.resistance,
            bias.external_emitter.voltage,
            bias.external_emitter.resistance,
            bias.external_collector.voltage,
            bias.external_collector.resistance,
            bias.external_base.voltage,
            bias.external_base.resistance,
        ];
        if !input.iter().all(|value| value.is_finite()) {
            return Err(EvaluationError::NonFiniteBias);
        }
        if bias.avalanche.current < 0.0
            || [
                bias.intrinsic_base.resistance,
                bias.external_emitter.resistance,
                bias.external_collector.resistance,
                bias.external_base.resistance,
            ]
            .iter()
            .any(|value| *value < 0.0)
        {
            return Err(EvaluationError::InvalidPhysicalDomain);
        }
        let electrical = self.power(bias);
        let cooling_conductance = 1.0 / self.p.effective_resistance;
        let d_inverse = -self.d_rth_d_rise * cooling_conductance * cooling_conductance;
        let out = Output {
            shorted: false,
            electrical,
            thermal_current: self
                .p
                .thermal_rise
                .mul_add(cooling_conductance, -electrical.watts),
            d_thermal_current_d_rise_at_fixed_electrical: self
                .p
                .thermal_rise
                .mul_add(d_inverse, cooling_conductance),
            heat_charge: self.p.heat_capacity * self.p.thermal_rise,
            heat_capacitance: self.p.heat_capacity,
            d_tdev_d_rise: self.p.temperature_slope,
        };
        if !out.is_finite() {
            return Err(EvaluationError::NonFiniteResult);
        }
        Ok(out)
    }

    fn power(&self, bias: ElectricalBias) -> ElectricalPower {
        let transport = product(bias.transport);
        let avalanche = product(Terminal {
            voltage: bias.avalanche.collector_potential - bias.avalanche.collector_voltage,
            current: bias.avalanche.current,
        });
        let mut out = ElectricalPower {
            watts: transport.watts + avalanche.watts,
            transport,
            avalanche,
            ..ElectricalPower::default()
        };
        if self.p.mode == Mode::FullElectrical {
            out.intrinsic_emitter = product(bias.intrinsic_emitter);
            out.intrinsic_collector = product(bias.intrinsic_collector);
            out.peripheral_emitter = product(bias.peripheral_emitter);
            out.peripheral_collector = product(bias.peripheral_collector);
            out.substrate = product(bias.substrate);
            out.intrinsic_base = series_power(bias.intrinsic_base);
            out.external_emitter = series_power(bias.external_emitter);
            out.external_collector = series_power(bias.external_collector);
            out.external_base = series_power(bias.external_base);
            out.watts += [
                out.intrinsic_emitter.watts,
                out.intrinsic_collector.watts,
                out.peripheral_emitter.watts,
                out.peripheral_collector.watts,
                out.substrate.watts,
                out.intrinsic_base.watts,
                out.external_emitter.watts,
                out.external_collector.watts,
                out.external_base.watts,
            ]
            .into_iter()
            .sum::<f64>();
        }
        out
    }

    /// # Errors
    /// Reports mismatched buffers or invalid source power/thermal state.
    pub fn evaluate_batch(
        &self,
        inputs: &[ElectricalBias],
        outputs: &mut [Output],
    ) -> Result<(), EvaluationError> {
        validate_batch_lengths(inputs.len(), outputs.len())?;
        for (input, output) in inputs.iter().zip(outputs) {
            *output = self.evaluate(*input)?;
        }
        Ok(())
    }
}

const fn product(branch: Terminal) -> ProductPower {
    ProductPower {
        watts: branch.voltage * branch.current,
        d_voltage: branch.current,
        d_current: branch.voltage,
    }
}

fn series_power(branch: Series) -> ResistorPower {
    if branch.resistance < MIN_SOURCE_RESISTANCE {
        return ResistorPower::default();
    }
    let conductance = 1.0 / branch.resistance;
    let power = branch.voltage * branch.voltage * conductance;
    ResistorPower {
        watts: power,
        d_voltage: 2.0 * branch.voltage * conductance,
        d_resistance: -power * conductance,
    }
}

impl Output {
    fn is_finite(self) -> bool {
        let electrical = self.electrical;
        let term = electrical.transport;
        let avalanche = electrical.avalanche;
        let intrinsic_emitter = electrical.intrinsic_emitter;
        let intrinsic_collector = electrical.intrinsic_collector;
        let peripheral_emitter = electrical.peripheral_emitter;
        let peripheral_collector = electrical.peripheral_collector;
        let substrate = electrical.substrate;
        let rbi = electrical.intrinsic_base;
        let re = electrical.external_emitter;
        let rcx = electrical.external_collector;
        let rbx = electrical.external_base;
        let powers = [
            electrical.watts,
            self.thermal_current,
            self.d_thermal_current_d_rise_at_fixed_electrical,
            self.heat_charge,
            self.heat_capacitance,
            self.d_tdev_d_rise,
            term.watts,
            term.d_voltage,
            term.d_current,
            avalanche.watts,
            avalanche.d_voltage,
            avalanche.d_current,
            intrinsic_emitter.watts,
            intrinsic_emitter.d_voltage,
            intrinsic_emitter.d_current,
            intrinsic_collector.watts,
            intrinsic_collector.d_voltage,
            intrinsic_collector.d_current,
            peripheral_emitter.watts,
            peripheral_emitter.d_voltage,
            peripheral_emitter.d_current,
            peripheral_collector.watts,
            peripheral_collector.d_voltage,
            peripheral_collector.d_current,
            substrate.watts,
            substrate.d_voltage,
            substrate.d_current,
        ];
        let resistors = [
            rbi.watts,
            rbi.d_voltage,
            rbi.d_resistance,
            re.watts,
            re.d_voltage,
            re.d_resistance,
            rcx.watts,
            rcx.d_voltage,
            rcx.d_resistance,
            rbx.watts,
            rbx.d_voltage,
            rbx.d_resistance,
        ];
        powers
            .iter()
            .chain(resistors.iter())
            .all(|value| value.is_finite())
    }
}
