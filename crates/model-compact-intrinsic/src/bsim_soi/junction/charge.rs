//! Gate-sidewall depletion and neutral-body diffusion charge from `B4SOIload`.
//! Copyright 2010 Regents of the University of California. See package LICENSE.

use super::{Bias, BsimSoiJunctionCurrent, BsimSoiNeutralBody};
use crate::bsim_soi::{PreparedBsimSoiSize, positive, real};
use crate::{BoundParameters, ModelError};

/// A conservative body-to-terminal charge and its voltage/temperature partials.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BsimSoiJunctionCharge {
    pub charge: f64,
    pub capacitance: f64,
    pub d_temperature: f64,
}

impl BsimSoiJunctionCharge {
    const fn plus(self, other: Self) -> Self {
        Self {
            charge: self.charge + other.charge,
            capacitance: self.capacitance + other.capacitance,
            d_temperature: self.d_temperature + other.d_temperature,
        }
    }

    fn checked(self) -> Result<Self, ModelError> {
        if [self.charge, self.capacitance, self.d_temperature]
            .into_iter()
            .all(f64::is_finite)
        {
            Ok(self)
        } else {
            Err(ModelError::InvalidPhysicalParameter(
                "BSIMSOI junction charge",
            ))
        }
    }
}

/// Distinct depletion and diffusion storage for a single physical junction.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BsimSoiJunctionStorage {
    pub depletion: BsimSoiJunctionCharge,
    pub diffusion: BsimSoiJunctionCharge,
}

impl BsimSoiJunctionStorage {
    #[must_use = "the total is the conserved body-to-terminal charge"]
    pub const fn total(self) -> BsimSoiJunctionCharge {
        self.depletion.plus(self.diffusion)
    }
}

/// Source/drain charges in physical terminal order.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BsimSoiJunctionCharges {
    pub source: BsimSoiJunctionStorage,
    pub drain: BsimSoiJunctionStorage,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct SideStorage {
    potential: f64,
    potential_slope: f64,
    capacitance: f64,
    cap_relative_slope: f64,
    grading: f64,
}

impl SideStorage {
    fn depletion(self, bias: Bias, nominal: f64) -> Result<BsimSoiJunctionCharge, ModelError> {
        if self.capacitance == 0. {
            return Ok(BsimSoiJunctionCharge::default());
        }
        let delta = bias.temperature - nominal;
        let potential = positive(
            self.potential_slope.mul_add(delta, self.potential),
            "junction depletion potential",
        )?;
        let capacitance = self.capacitance * self.cap_relative_slope.mul_add(delta, 1.);
        let x = bias.voltage / potential;
        let (shape, slope) = depletion_shape(x, self.grading);
        // Q=C*phi*F(V/phi). This chain rule also differentiates
        // the linear extension above 0.9*phi, not the native tangent
        // evaluated at the moving cutoff with omitted extension terms.
        let temperature_slope = if bias.live {
            (capacitance * self.potential_slope).mul_add(
                (-x).mul_add(slope, shape),
                self.capacitance * self.cap_relative_slope * potential * shape,
            )
        } else {
            0.
        };
        BsimSoiJunctionCharge {
            charge: bias.polarity * capacitance * potential * shape,
            capacitance: capacitance * slope,
            d_temperature: bias.polarity * temperature_slope,
        }
        .checked()
    }
}

fn depletion_shape(x: f64, grading: f64) -> (f64, f64) {
    let clamped = x.min(0.9);
    let log = (-clamped).ln_1p();
    let exponent = 1. - grading;
    let shape = if exponent == 0. {
        -log
    } else {
        -(exponent * log).exp_m1() / exponent
    };
    let slope = (-grading * log).exp();
    (slope.mul_add(x - clamped, shape), slope)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct JunctionChargeLaw {
    sides: [SideStorage; 2],
    transit_time: f64,
}

impl JunctionChargeLaw {
    fn side(
        self,
        index: usize,
        bias: Bias,
        nominal: f64,
        diffusion: BsimSoiJunctionCurrent,
    ) -> Result<BsimSoiJunctionStorage, ModelError> {
        Ok(BsimSoiJunctionStorage {
            depletion: self.sides[index].depletion(bias, nominal)?,
            diffusion: BsimSoiJunctionCharge {
                charge: self.transit_time * diffusion.current,
                capacitance: self.transit_time * diffusion.conductance,
                d_temperature: self.transit_time * diffusion.d_temperature,
            }
            .checked()?,
        })
    }

    pub(super) fn evaluate(
        self,
        biases: [Bias; 2],
        nominal: f64,
        neutral: BsimSoiNeutralBody,
        disabled: bool,
    ) -> Result<BsimSoiJunctionCharges, ModelError> {
        if disabled {
            return Ok(BsimSoiJunctionCharges::default());
        }
        Ok(BsimSoiJunctionCharges {
            source: self.side(0, biases[0], nominal, neutral.source_diffusion)?,
            drain: self.side(1, biases[1], nominal, neutral.drain_diffusion)?,
        })
    }
}

fn side(
    model: &BoundParameters,
    scale: f64,
    names: [&'static str; 5],
) -> Result<SideStorage, ModelError> {
    let [
        potential,
        thermal_potential,
        capacitance,
        thermal_capacitance,
        grading,
    ] = names;
    Ok(SideStorage {
        potential: real(model, potential)?.max(0.1),
        potential_slope: -real(model, thermal_potential)?,
        capacitance: scale * real(model, capacitance)?,
        cap_relative_slope: real(model, thermal_capacitance)?,
        grading: real(model, grading)?,
    })
}

pub(super) fn prepare(
    model: &BoundParameters,
    size: &PreparedBsimSoiSize,
    area_per_width: f64,
) -> Result<JunctionChargeLaw, ModelError> {
    let scale = area_per_width / 1e-7;
    let geometry = size.geometry();
    Ok(JunctionChargeLaw {
        sides: [
            side(
                model,
                scale * geometry.source_diode_width_cv,
                ["pbswg", "tpbswg", "cjswg", "tcjswg", "mjswg"],
            )?,
            side(
                model,
                scale * geometry.drain_diode_width_cv,
                ["pbswgd", "tpbswgd", "cjswgd", "tcjswgd", "mjswgd"],
            )?,
        ],
        transit_time: real(model, "tt")?,
    })
}
