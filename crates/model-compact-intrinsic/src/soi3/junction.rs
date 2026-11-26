//! STAG two-diode junction, parasitic BJT injection and depletion charge.

use super::{K_OVER_Q, PreparedModel, instance::TemperatureState};
use crate::{
    BoundParameters, ModelError,
    preparation::{integer, positive, real},
};

/// A polarity-normalized junction evaluation (all parallel devices included).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct JunctionResult {
    /// Total junction current, including circuit GMIN.
    pub current_a: f64,
    /// Native junction voltage tangent in siemens.
    pub conductance_s: f64,
    /// Native junction temperature tangent in A/K.
    pub thermal_derivative_a_per_k: f64,
    /// Depletion charge in coulombs.
    pub charge_c: f64,
    /// Differential depletion capacitance in farads.
    pub capacitance_f: f64,
    /// Parasitic BJT transported component, before terminal orientation.
    pub bipolar_current_a: f64,
    /// BJT voltage tangent in siemens.
    pub bipolar_conductance_s: f64,
    /// BJT temperature tangent in A/K.
    pub bipolar_thermal_derivative_a_per_k: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Diode {
    saturation: f64,
    activation: f64,
    ideality: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Current {
    value: f64,
    voltage: f64,
    temperature: f64,
}

/// Instance-local physical junction coefficients.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreparedJunctions {
    diodes: [Diode; 2],
    base_temperature: f64,
    dvt: f64,
    multiplicity: f64,
    bipolar_alpha: f64,
    potential: f64,
    grading: f64,
    forward_fraction: f64,
    zero_bias: [f64; 2],
}

pub(super) fn prepare(
    model: &PreparedModel,
    instance: &BoundParameters,
    state: TemperatureState,
    length: f64,
) -> Result<PreparedJunctions, ModelError> {
    let p = model.parameters();
    let width = real(instance, "w")?;
    let saturation = |density_name, current_name| -> Result<f64, ModelError> {
        let density = real(p, density_name)?;
        let base = if density == 0.0 {
            real(p, current_name)?
        } else {
            density * width
        };
        Ok(base * state.junction_current_factor)
    };
    let beta = real(p, "betabjt")? / length.powf(real(p, "betaexp")?);
    let alpha = beta / (1.0 + beta);
    let forward_fraction = real(p, "fc")?;
    if !alpha.is_finite() || beta < 0.0 || !(0.0..1.0).contains(&forward_fraction) {
        return Err(ModelError::InvalidPhysicalParameter(
            "junction coefficients",
        ));
    }
    Ok(PreparedJunctions {
        diodes: [
            Diode {
                saturation: saturation("js", "is")?,
                activation: state.junction_activation[0],
                ideality: positive(real(p, "etad")?, "etad")?,
            },
            Diode {
                saturation: saturation("js1", "is1")?,
                activation: state.junction_activation[1],
                ideality: positive(real(p, "etad1")?, "etad1")?,
            },
        ],
        base_temperature: state.kelvin,
        dvt: f64::from(integer(p, "dvt")?),
        multiplicity: real(instance, "m")?,
        bipolar_alpha: alpha,
        potential: state.junction_potential,
        grading: real(p, "mj")?,
        forward_fraction,
        zero_bias: state.zero_bias_capacitance,
    })
}

impl PreparedJunctions {
    fn diode(self, diode: Diode, voltage: f64, rise: f64) -> Result<Current, ModelError> {
        let actual_temp = positive(self.base_temperature + rise, "junction temperature")?;
        let thermal_temp = positive(
            self.dvt.mul_add(rise, self.base_temperature),
            "junction thermal voltage temperature",
        )?;
        let eta_vt = diode.ideality * K_OVER_Q * thermal_temp;
        let sat = diode.saturation
            * (diode.activation * rise / (self.base_temperature * actual_temp))
                .min(709.0)
                .exp();
        let exponent = (voltage / eta_vt).min(709.0);
        let ev = exponent.exp();
        let current = sat * exponent.exp_m1();
        let conductance = sat * ev / eta_vt;
        // The upstream Newton tangent intentionally retains its exponential
        // slope at the overflow guard; it is not the derivative of the cap.
        let temperature = current.mul_add(
            diode.activation / (actual_temp * actual_temp),
            -(conductance * voltage * self.dvt / thermal_temp),
        );
        Ok(Current {
            value: current,
            voltage: conductance,
            temperature,
        })
    }

    fn depletion(self, voltage: f64, zero: f64) -> (f64, f64) {
        let edge = self.forward_fraction * self.potential;
        let reverse = |v: f64| {
            let log = (-v / self.potential).ln_1p();
            let power = 1.0 - self.grading;
            let charge = if power == 0.0 {
                -self.potential * zero * log
            } else {
                -self.potential * zero * (power * log).exp_m1() / power
            };
            (charge, zero * (-self.grading * log).exp())
        };
        if voltage < edge {
            reverse(voltage)
        } else {
            let (edge_charge, edge_cap) = reverse(edge);
            let slope = edge_cap * self.grading / (self.potential - edge);
            let offset = voltage - edge;
            let charge = offset.mul_add((0.5 * slope).mul_add(offset, edge_cap), edge_charge);
            (charge, slope.mul_add(offset, edge_cap))
        }
    }

    fn side(
        self,
        voltage: f64,
        rise: f64,
        gmin: f64,
        zero: f64,
    ) -> Result<JunctionResult, ModelError> {
        let primary = self.diode(self.diodes[0], voltage, rise)?;
        let secondary = self.diode(self.diodes[1], voltage, rise)?;
        let (charge_c, capacitance_f) = self.depletion(voltage, zero);
        let m = self.multiplicity;
        let result = JunctionResult {
            current_a: m * gmin.mul_add(voltage, primary.value + secondary.value),
            conductance_s: m * (primary.voltage + secondary.voltage + gmin),
            thermal_derivative_a_per_k: m * (primary.temperature + secondary.temperature),
            charge_c,
            capacitance_f,
            bipolar_current_a: m * self.bipolar_alpha * primary.value,
            bipolar_conductance_s: m * self.bipolar_alpha * primary.voltage,
            bipolar_thermal_derivative_a_per_k: m * self.bipolar_alpha * primary.temperature,
        };
        if [
            result.current_a,
            result.conductance_s,
            result.thermal_derivative_a_per_k,
            result.charge_c,
            result.capacitance_f,
            result.bipolar_current_a,
            result.bipolar_conductance_s,
            result.bipolar_thermal_derivative_a_per_k,
        ]
        .iter()
        .all(|v| v.is_finite())
        {
            Ok(result)
        } else {
            Err(ModelError::InvalidPhysicalParameter("junction evaluation"))
        }
    }

    /// Evaluate both polarity-normalized junctions (drain, source), with
    /// thermal rise in kelvin and the circuit's per-device GMIN.
    ///
    /// The voltage and temperature tangents follow the native Newton
    /// linearization, including its capped-exponential guard. The stored
    /// depletion capacitance parameters remain at the prepared temperature.
    /// The solver supplies the limited thermal rise; the source load routine
    /// clamps negative thermal-node values to zero before this evaluation.
    ///
    /// # Errors
    /// Non-finite inputs/outputs, negative GMIN, or nonpositive actual
    /// junction/thermal-voltage temperature.
    pub fn evaluate(
        self,
        drain_v: f64,
        source_v: f64,
        thermal_rise: f64,
        gmin: f64,
    ) -> Result<[JunctionResult; 2], ModelError> {
        if ![drain_v, source_v, thermal_rise, gmin]
            .iter()
            .all(|v| v.is_finite())
            || gmin < 0.0
        {
            return Err(ModelError::InvalidPhysicalParameter("junction bias"));
        }
        Ok([
            self.side(drain_v, thermal_rise, gmin, self.zero_bias[0])?,
            self.side(source_v, thermal_rise, gmin, self.zero_bias[1])?,
        ])
    }
}
