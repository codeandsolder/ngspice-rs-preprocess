//! Extrinsic source/drain-to-backgate charge, distinct from the intrinsic channel.
//! Copyright 1999/1990 Regents of the University of California. See package LICENSE.

use super::{Context, Electrostatics, finite};
use crate::ModelError;

/// A physical two-terminal charge and its voltage derivative, including M.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SubstrateCharge {
    /// Charge on the source or drain terminal, in coulombs.
    pub charge_c: f64,
    /// Differential capacitance with respect to terminal-minus-backgate voltage.
    pub capacitance_f: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Transition {
    lower: f64,
    span: f64,
    split: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Law {
    Linear,
    Transition(Transition),
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Side {
    low_capacitance: f64,
    high_capacitance: f64,
    fringing: f64,
}

/// Prepared extrinsic source/drain charge laws, bound to device layout and temperature.
///
/// Evaluation takes physical, not polarity-normalized voltages. Each branch
/// represents a charge on its diffusion terminal and the opposite charge
/// on the backgate, and includes all parallel devices.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreparedSubstrateCharge {
    law: Law,
    drain: Side,
    source: Side,
}

pub(super) fn prepare(
    c: &Context<'_>,
    e: Electrostatics,
    fringing: [f64; 2],
) -> Result<PreparedSubstrateCharge, ModelError> {
    let same_type = c.sign() * c.binned("nsub")? > 0.0;
    let linear = c.pd() && e.substrate_minimum_capacitance == 0.0;
    let (lower, upper) = if same_type {
        (e.substrate_flatband, e.substrate_threshold)
    } else {
        (e.substrate_threshold, e.substrate_flatband)
    };
    let law = if linear {
        Law::Linear
    } else {
        let split = c.model("asd")?;
        let span = upper - lower;
        if !(0.0..=1.0).contains(&split) || !span.is_finite() || span < 0.0 {
            return Err(ModelError::InvalidPhysicalParameter(
                "substrate charge transition",
            ));
        }
        Law::Transition(Transition { lower, span, split })
    };
    let m = c.instance("m")?;
    let side = |name, edge| -> Result<Side, ModelError> {
        let area = c.instance(name)? * m;
        if area < 0.0 {
            return Err(ModelError::InvalidPhysicalParameter("diffusion area"));
        }
        let oxide = finite(
            area * e.buried_oxide_capacitance,
            "buried oxide capacitance",
        )?;
        let minimum = finite(
            area * e.substrate_minimum_capacitance,
            "minimum substrate capacitance",
        )?;
        let (low_capacitance, high_capacitance) = if linear || same_type {
            (oxide, minimum)
        } else {
            (minimum, oxide)
        };
        Ok(Side {
            low_capacitance,
            high_capacitance,
            fringing: finite(edge * m, "substrate fringing")?,
        })
    };
    Ok(PreparedSubstrateCharge {
        law,
        drain: side("ad", fringing[0])?,
        source: side("as", fringing[1])?,
    })
}

impl Transition {
    fn evaluate(self, side: Side, voltage: f64) -> (f64, f64) {
        let offset = voltage - self.lower;
        let delta = side.low_capacitance - side.high_capacitance;
        if offset <= 0.0 {
            return (side.low_capacitance * offset, side.low_capacitance);
        }
        let q_high = self.span * delta.mul_add((1.0 + self.split) / 3.0, side.high_capacitance);
        if offset >= self.span {
            return (
                side.high_capacitance.mul_add(offset - self.span, q_high),
                side.high_capacitance,
            );
        }
        let x = offset / self.span;
        if x < self.split {
            let correction = delta * x * x / self.split;
            (
                offset * correction.mul_add(-1.0 / 3.0, side.low_capacitance),
                side.low_capacitance - correction,
            )
        } else {
            let u = x - 1.0;
            let correction = delta * u * u / (1.0 - self.split);
            let capacitance = side.high_capacitance + correction;
            (
                (offset - self.span)
                    .mul_add(correction.mul_add(1.0 / 3.0, side.high_capacitance), q_high),
                capacitance,
            )
        }
    }
}

impl PreparedSubstrateCharge {
    fn side(self, side: Side, voltage: f64) -> Result<SubstrateCharge, ModelError> {
        finite(voltage, "substrate bias")?;
        let (charge, capacitance) = match self.law {
            Law::Linear => (side.low_capacitance * voltage, side.low_capacitance),
            Law::Transition(t) => t.evaluate(side, voltage),
        };
        Ok(SubstrateCharge {
            charge_c: finite(side.fringing.mul_add(voltage, charge), "substrate charge")?,
            capacitance_f: finite(capacitance + side.fringing, "substrate capacitance")?,
        })
    }

    /// Evaluate drain and source charges for physical diffusion-minus-backgate voltages.
    ///
    /// The corresponding backgate charge is the negative sum of these charges.
    ///
    /// # Errors
    /// Nonfinite bias or result.
    pub fn evaluate(self, drain_v: f64, source_v: f64) -> Result<[SubstrateCharge; 2], ModelError> {
        Ok([
            self.side(self.drain, drain_v)?,
            self.side(self.source, source_v)?,
        ])
    }
}
