//! Gate-sidewall depletion and bipolar diffusion charge of BSIM3SOI.
//! Copyright 1999/1990 Regents of the University of California. See package LICENSE.

use super::{Bias, Components, D};
use crate::{ModelError, ModelVariant, bsim3soi::thermal::Context, preparation::positive};

/// Charge on the body side of one intrinsic junction and its analytic tangents.
///
/// All coordinates are normalized to the forward source orientation. The
/// opposite diffusion terminal receives the negative of this charge. Apply
/// polarity and instance multiplicity when constructing physical stamps.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct JunctionCharge {
    /// Stored charge in coulombs.
    pub charge_c: f64,
    /// Partial derivative with respect to normalized VBS.
    pub body_capacitance_f: f64,
    /// Partial derivative with respect to normalized VDS.
    pub drain_capacitance_f: f64,
    /// Partial derivative with respect to absolute temperature.
    pub temperature_derivative_c_per_k: f64,
}

impl JunctionCharge {
    fn from_d(value: D) -> Result<Self, ModelError> {
        if !value.finite() {
            return Err(ModelError::InvalidPhysicalParameter(
                "junction charge or tangent",
            ));
        }
        Ok(Self {
            charge_c: value.value,
            body_capacitance_f: value.gradient[0],
            drain_capacitance_f: value.gradient[1],
            temperature_derivative_c_per_k: value.gradient[2],
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum ChargeLaw {
    Fd,
    Dd(Parameters),
    Pd(Parameters),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Parameters {
    potential: f64,
    grading: f64,
    capacitance: [f64; 2],
    transit_time: f64,
    nominal_temperature: f64,
    potential_temperature: f64,
    capacitance_temperature: f64,
}

pub(super) fn prepare(c: &Context<'_>) -> Result<ChargeLaw, ModelError> {
    let family = c.model.parameters().variant();
    if family == ModelVariant::Bsim3SoiFd {
        return Ok(ChargeLaw::Fd);
    }
    let g = c.instance.geometry();
    let width = g.pd.map_or([g.width_m; 2], |p| {
        [p.source_diode_cv_width, p.drain_diode_cv_width]
    });
    let density = c.model("cjswg")? * positive(c.model("tsi")?, "tsi")? / 1e-7;
    let params = Parameters {
        potential: c.model("pbswg")?.max(0.1),
        grading: c.model("mjswg")?,
        capacitance: width.map(|w| density * w),
        transit_time: c.model("tt")?,
        nominal_temperature: positive(c.model("tnom")?, "tnom")?,
        potential_temperature: if c.pd() { -c.model("tpbswg")? } else { 0.0 },
        capacitance_temperature: if c.pd() { c.model("tcjswg")? } else { 0.0 },
    };
    Ok(if c.pd() {
        ChargeLaw::Pd(params)
    } else {
        ChargeLaw::Dd(params)
    })
}

fn depletion(voltage: D, potential: D, grading: f64) -> D {
    let log = (D::constant(1.0) - voltage / potential).ln();
    // The exact logarithmic limit is well-defined at unity grading.
    let integral = if grading == 1.0 {
        -log
    } else {
        -log.scale(1.0 - grading)
            .exp_m1()
            .scale((1.0 - grading).recip())
    };
    potential * integral
}

impl Parameters {
    fn dd(self, b: &Bias, side: Components, index: usize) -> Result<JunctionCharge, ModelError> {
        let voltage = b.junction[index];
        let potential = D::constant(self.potential);
        let primitive = if voltage.value < 0.0 {
            depletion(voltage, potential, self.grading)
        } else {
            voltage * (D::constant(1.0) + voltage.scale(0.5 * self.grading / self.potential))
        };
        JunctionCharge::from_d(
            primitive.scale(self.capacitance[index]) + side.diffusion.scale(self.transit_time),
        )
    }

    fn pd(self, b: &Bias, side: Components, index: usize) -> Result<JunctionCharge, ModelError> {
        let voltage = b.junction[index];
        let delta = b.temperature - D::constant(self.nominal_temperature);
        let potential = D::constant(self.potential) + delta.scale(self.potential_temperature);
        positive(potential.value, "temperature-adjusted junction potential")?;
        let capacitance = (D::constant(1.0) + delta.scale(self.capacitance_temperature))
            .scale(self.capacitance[index]);
        let limit = potential.scale(0.9);
        let primitive = if voltage.value > limit.value {
            let slope = 0.1_f64.powf(-self.grading);
            depletion(limit, potential, self.grading) + (voltage - limit).scale(slope)
        } else {
            depletion(voltage, potential, self.grading)
        };
        JunctionCharge::from_d(capacitance * primitive + side.stored.scale(self.transit_time))
    }
}

impl ChargeLaw {
    pub(super) fn evaluate(
        self,
        bias: &Bias,
        side: Components,
        index: usize,
    ) -> Result<JunctionCharge, ModelError> {
        match self {
            Self::Fd => Ok(JunctionCharge::default()),
            Self::Dd(p) => p.dd(bias, side, index),
            Self::Pd(p) => p.pd(bias, side, index),
        }
    }
}
