//! Partially depleted BSIM3SOI 2.2.3 channel transport.
//! Copyright 1999/1990 Regents of the University of California. See package LICENSE.

mod leakage;
mod threshold;
mod transport;

use super::channel_threshold::{Threshold, ThresholdGeometry};
use super::channel_transport::{BulkParameters, ChannelTransport};
use super::{Context, Electrostatics, K_OVER_Q, Transport, differential::Differential};
use crate::{ModelError, preparation::positive};
use leakage::Leakage;
pub use leakage::{PdCurrent, PdLeakage};

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
    /// Gate-induced, gate-body, and impact-ionization contributions.
    pub leakage: PdLeakage,
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
    gate_effective: D,
    body_high: D,
    body: D,
    root: D,
    depth: D,
    threshold: D,
    overdrive: D,
}

/// Prepared PD channel evaluator with layout, electrostatics, and model coefficients.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreparedPdChannel {
    electrostatics: Electrostatics,
    threshold: Threshold,
    threshold_geometry: ThresholdGeometry,
    channel: ChannelTransport,
    leakage: Leakage,
    bulk_params: BulkParameters,
    ketas: f64,
    length: f64,
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
        Ok(Self {
            electrostatics,
            threshold: Threshold::prepare(c)?,
            threshold_geometry: ThresholdGeometry::prepare(c, electrostatics)?,
            channel: ChannelTransport::prepare(c, transport)?,
            leakage: Leakage::prepare(c, transport.oxide_ratio)?,
            bulk_params: BulkParameters::prepare(c)?,
            ketas: c.binned("ketas")?,
            length: geometry.length_m,
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

    /// Calculate the accepted/predictor surface potential from the last
    /// solved device temperature, using the native load-time material law.
    /// The solver owns when to commit this value; trial evaluation does not
    /// mutate it.
    ///
    /// # Errors
    /// Nonpositive temperature or surface potential.
    pub fn accepted_surface_potential(&self, kelvin: f64) -> Result<f64, ModelError> {
        if !self.channel.temperature.selfheat {
            return Ok(self.electrostatics.phi);
        }
        let temperature = D::constant(positive(kelvin, "accepted temperature")?);
        let thermal = temperature.scale(K_OVER_Q);
        let ni = intrinsic_density(temperature, thermal);
        positive(
            2.0 * thermal.value * (self.electrostatics.channel_doping / ni.value).ln(),
            "accepted phi",
        )
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
        let temperature = if self.channel.temperature.selfheat {
            D::variable(positive(kelvin, "channel temperature")?, 3)
        } else {
            D::constant(self.channel.temperature.ambient)
        };
        let phi = if self.channel.temperature.selfheat {
            positive(accepted_phi, "accepted phi")?
        } else {
            self.electrostatics.phi
        };
        let thermal = temperature.scale(K_OVER_Q);
        let built_in = if self.channel.temperature.selfheat {
            built_in(temperature, thermal, self.electrostatics.channel_doping)
        } else {
            D::constant(self.electrostatics.built_in_potential)
        };
        let b = Bias {
            gate: D::variable(vgs, 0),
            drain: D::variable(vds, 1),
            body: D::variable(vbs, 2),
            thermal,
            ratio_delta: temperature.scale(self.channel.temperature.nominal.recip())
                - D::constant(1.0),
            built_in,
            phi,
        };
        let state = self.state(&b)?;
        self.channel_current(&b, &state)
    }
}

pub(super) fn intrinsic_density<const N: usize>(
    temp: Differential<N>,
    thermal: Differential<N>,
) -> Differential<N> {
    let gap = Differential::constant(1.16)
        - (temp * temp).scale(7.02e-4) / (temp + Differential::constant(1108.0));
    (temp * temp.sqrt()).scale(1.45e10 * 1.923_058_4e-4)
        * (Differential::constant(21.556_598_1) - gap / thermal.scale(2.0)).exp()
}

fn built_in(temp: D, thermal: D, doping: f64) -> D {
    let density = intrinsic_density(temp, thermal);
    thermal * (D::constant(1e20 * doping) / (density * density)).ln()
}
