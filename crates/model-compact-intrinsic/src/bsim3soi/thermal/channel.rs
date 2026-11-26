//! Partially depleted BSIM3SOI 2.2.3 channel transport.
//! Copyright 1999/1990 Regents of the University of California. See package LICENSE.

mod threshold;
mod transport;

use super::{Context, Electrostatics, K_OVER_Q, Transport, differential::Differential};
use crate::{
    ModelError,
    preparation::{integer, positive},
};
use threshold::Threshold;
use transport::ChannelTransport;

type D = Differential<4>;

/// Source-oriented channel current, threshold, and transport observables.
///
/// All values are normalized and per instance before multiplicity stamping.
/// Gradients are with respect to normalized VGS, VDS, VBS and absolute
/// temperature; temperature derivatives hold the accepted surface potential
/// fixed, as in the original self-heated load routine.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PdChannelEvaluation {
    /// Intrinsic channel current in amperes (excludes parasitic bipolar current).
    pub current_a: f64,
    /// Analytic VGS, VDS, VBS and temperature derivatives.
    pub current_gradient: [f64; 4],
    /// Threshold voltage.
    pub threshold_v: f64,
    /// Effective gate overdrive.
    pub overdrive_v: f64,
    /// Effective body voltage.
    pub body_effective_v: f64,
    /// Bias-dependent mobility in square meters per volt-second.
    pub mobility: f64,
    /// Saturation drain voltage.
    pub saturation_voltage_v: f64,
    /// Effective drain voltage.
    pub effective_drain_voltage_v: f64,
    /// Bias-dependent intrinsic source/drain series resistance.
    pub series_resistance_ohm: f64,
    /// Bulk charge factor.
    pub bulk_factor: f64,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Bias {
    gate: D,
    drain: D,
    body: D,
    thermal: D,
    ratio_delta: D,
    built_in: D,
    phi: f64,
}

#[derive(Clone, Copy)]
pub(super) struct State {
    body_high: D,
    body: D,
    root: D,
    depth: D,
    threshold: D,
    overdrive: D,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct TemperatureCoefficients {
    ambient: f64,
    nominal: f64,
    mobility_exponent: f64,
    velocity_slope: f64,
    resistance_slope: f64,
    resistance_base: f64,
    mobility_slopes: [f64; 3],
    selfheat: bool,
}

/// Prepared PD channel evaluator with layout, electrostatics, and model coefficients.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreparedPdChannel {
    electrostatics: Electrostatics,
    transport: Transport,
    threshold: Threshold,
    channel: ChannelTransport,
    temperature: TemperatureCoefficients,
    length: f64,
    width: f64,
    oxide: f64,
    tox: f64,
    sign: f64,
    voff: f64,
    segments: f64,
    body_contacts: f64,
}

impl PreparedPdChannel {
    pub(super) fn prepare(
        c: &Context<'_>,
        electrostatics: Electrostatics,
        transport: Transport,
    ) -> Result<Self, ModelError> {
        let geometry = c.instance.geometry();
        let mut resistance_base = c.binned("rdsw")?;
        if c.checked()? && resistance_base < 0.0 {
            resistance_base = 0.0;
        }
        Ok(Self {
            electrostatics,
            transport,
            threshold: Threshold::prepare(c)?,
            channel: ChannelTransport::prepare(c)?,
            temperature: TemperatureCoefficients {
                ambient: c.material.kelvin,
                nominal: positive(c.model("tnom")?, "tnom")?,
                mobility_exponent: c.binned("ute")?,
                velocity_slope: c.binned("at")?,
                resistance_slope: c.binned("prt")?,
                resistance_base,
                mobility_slopes: [c.binned("ua1")?, c.binned("ub1")?, c.binned("uc1")?],
                selfheat: integer(c.model.parameters(), "shmod")? == 1
                    && c.instance("rth0")? != 0.0,
            },
            length: geometry.length_m,
            width: geometry.width_m,
            oxide: c.model.oxide_capacitance_f_per_m2(),
            tox: positive(c.model("tox")?, "tox")?,
            sign: c.sign(),
            voff: c.binned("voff")?,
            segments: positive(c.instance("nseg")?, "nseg")?,
            body_contacts: c.instance("nbc")?,
        })
    }

    /// Surface potential initialized at ambient temperature. Self-heated
    /// solver integrations replace this with the accepted-step value.
    #[must_use]
    pub const fn initial_surface_potential(&self) -> f64 {
        self.electrostatics.phi
    }

    /// Evaluate a PD channel at forward-oriented normalized terminal bias.
    ///
    /// Supply the accepted surface potential when self-heating is enabled.
    /// Isothermal operation uses the prepared ambient coefficients and
    /// ignores the supplied temperature and accepted potential.
    ///
    /// # Errors
    /// Nonfinite or non-forward bias, invalid temperature/surface potential,
    /// or nonfinite transport results.
    pub fn evaluate(
        &self,
        vgs: f64,
        vds: f64,
        vbs: f64,
        kelvin: f64,
        accepted_phi: f64,
    ) -> Result<PdChannelEvaluation, ModelError> {
        if ![vgs, vds, vbs].into_iter().all(f64::is_finite) || vds < 0.0 {
            return Err(ModelError::InvalidPhysicalParameter(
                "PD normalized channel bias",
            ));
        }
        let temperature = if self.temperature.selfheat {
            D::variable(positive(kelvin, "channel temperature")?, 3)
        } else {
            D::constant(self.temperature.ambient)
        };
        let phi = if self.temperature.selfheat {
            positive(accepted_phi, "accepted phi")?
        } else {
            self.electrostatics.phi
        };
        let thermal = temperature.scale(K_OVER_Q);
        let built_in = if self.temperature.selfheat {
            built_in(temperature, thermal, self.electrostatics.channel_doping)
        } else {
            D::constant(self.electrostatics.built_in_potential)
        };
        let b = Bias {
            gate: D::variable(vgs, 0),
            drain: D::variable(vds, 1),
            body: D::variable(vbs, 2),
            thermal,
            ratio_delta: temperature.scale(self.temperature.nominal.recip()) - D::constant(1.0),
            built_in,
            phi,
        };
        let state = self.state(&b)?;
        self.channel_current(&b, &state)
    }
}

fn built_in(temp: D, thermal: D, doping: f64) -> D {
    let gap = D::constant(1.16) - (temp * temp).scale(7.02e-4) / (temp + D::constant(1108.0));
    let density = (temp * temp.sqrt()).scale(1.45e10 * 1.923_058_4e-4)
        * (D::constant(21.556_598_1) - gap / thermal.scale(2.0)).exp();
    thermal * (D::constant(1e20 * doping) / (density * density)).ln()
}
