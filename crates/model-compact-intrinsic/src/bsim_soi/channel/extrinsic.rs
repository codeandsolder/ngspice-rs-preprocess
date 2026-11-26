//! Nonlinear gate overlap and bottom-film storage.
//! Copyright 2010 Regents of the University of California. See package LICENSE.
use super::super::{
    BsimSoiNode, CHARGE, EPS_OX, PreparedBsimSoiSetup, PreparedBsimSoiSize, binned, integer,
    positive, real, slot,
};
use super::{BsimSoiChannelScalar, Jet};
use crate::{BoundParameters, ModelError, MosPolarity, ParameterScope};

/// Physical voltages relative to source prime, with the overlap gate node.
/// Use the resolved gate-mid node for RGATEMOD=3.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BsimSoiExtrinsicChargeBias {
    pub overlap_gate: f64,
    pub drain: f64,
    pub backgate: f64,
}

/// Pairwise stored charge on the first named terminal.
/// The second terminal receives exactly the opposite charge. Gradient axes
/// follow overlap-gate, drain, unused body, backgate, unused temperature.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BsimSoiExtrinsicCharges {
    pub gate_drain: BsimSoiChannelScalar,
    pub gate_source: BsimSoiChannelScalar,
    pub gate_backgate: BsimSoiChannelScalar,
    pub drain_backgate: BsimSoiChannelScalar,
    pub source_backgate: BsimSoiChannelScalar,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Overlap {
    linear: f64,
    lateral: f64,
    kappa: f64,
}
impl Overlap {
    fn evaluate(self, voltage: Jet) -> Jet {
        let shifted = voltage + 0.02;
        let negative = -(-shifted).half_positive(Jet::constant(0.08));
        let root = (1. - 4. * negative / self.kappa).sqrt();
        // Rationalize T2 + 0.5*kappa*(sqrt(1-4*T2/kappa)-1)
        // to avoid cancellation near zero negative bias.
        let correction = negative * (1. - 2. / (root + 1.));
        (self.linear + self.lateral) * voltage - self.lateral * correction
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Transition {
    lower_voltage: f64,
    upper_voltage: f64,
    fraction: f64,
    lower_capacitance: f64,
    upper_capacitance: f64,
}

impl Transition {
    fn evaluate(self, voltage: Jet) -> Jet {
        let range = self.upper_voltage - self.lower_voltage;
        let change = self.lower_capacitance - self.upper_capacitance;
        let delta = voltage - self.lower_voltage;
        let normalized = delta / range;
        if voltage.value < self.lower_voltage {
            self.lower_capacitance * delta
        } else if normalized.value < self.fraction {
            delta
                * (self.lower_capacitance - change / (3. * self.fraction) * normalized * normalized)
        } else {
            let upper = voltage - self.upper_voltage;
            let total = range * change * (1. + self.fraction) / 3.;
            let base = self.upper_capacitance * delta + total;
            if voltage.value < self.upper_voltage {
                let distance = upper / range;
                base + change / (3. * (1. - self.fraction)) * upper * distance * distance
            } else {
                base
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Bottom {
    area: f64,
    fringe: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum BottomLaw {
    Linear(f64),
    Nonlinear(Transition),
}
impl BottomLaw {
    fn evaluate(self, side: Bottom, voltage: Jet) -> Jet {
        match self {
            Self::Linear(capacitance) => (capacitance.mul_add(side.area, side.fringe)) * voltage,
            Self::Nonlinear(transition) => {
                side.area * transition.evaluate(voltage) + side.fringe * voltage
            }
        }
    }
}

/// Prepared conservative extrinsic charges, independent of intrinsic channel storage.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreparedBsimSoiExtrinsicCharges {
    overlap_node: BsimSoiNode,
    source: Overlap,
    drain: Overlap,
    gate_backgate: f64,
    bottom: BottomLaw,
    source_bottom: Bottom,
    drain_bottom: Bottom,
    polarity: f64,
    multiplicity: f64,
}
impl PreparedBsimSoiExtrinsicCharges {
    /// Terminal on which the three overlap charges are stamped.
    #[must_use = "overlap storage is attached to the resolved gate node"]
    pub const fn overlap_node(&self) -> BsimSoiNode {
        self.overlap_node
    }

    /// Evaluate conservative pairwise charges in physical terminal order.
    /// # Errors
    /// Nonfinite physical bias or constitutive output.
    pub fn evaluate(
        &self,
        bias: BsimSoiExtrinsicChargeBias,
    ) -> Result<BsimSoiExtrinsicCharges, ModelError> {
        if ![bias.overlap_gate, bias.drain, bias.backgate]
            .into_iter()
            .all(f64::is_finite)
        {
            return Err(ModelError::InvalidPhysicalParameter(
                "extrinsic charge bias",
            ));
        }
        let gate = Jet::coordinate(bias.overlap_gate * self.polarity, 0, self.polarity);
        let drain = Jet::coordinate(bias.drain * self.polarity, 1, self.polarity);
        let backgate = Jet::coordinate(bias.backgate * self.polarity, 3, self.polarity);
        let scale = self.polarity * self.multiplicity;
        Ok(BsimSoiExtrinsicCharges {
            gate_drain: (self.drain.evaluate(gate - drain) * scale)
                .checked("gate drain overlap charge")?,
            gate_source: (self.source.evaluate(gate) * scale)
                .checked("gate source overlap charge")?,
            gate_backgate: (self.gate_backgate * (gate - backgate) * scale)
                .checked("gate backgate overlap charge")?,
            drain_backgate: (self
                .bottom
                .evaluate(self.drain_bottom, self.polarity * (drain - backgate))
                * self.multiplicity)
                .checked("drain bottom charge")?,
            source_backgate: (self
                .bottom
                .evaluate(self.source_bottom, -self.polarity * backgate)
                * self.multiplicity)
                .checked("source bottom charge")?,
        })
    }
}

fn overlap(
    model: &BoundParameters,
    size: &PreparedBsimSoiSize,
    width: f64,
    fingers: f64,
    names: [&str; 2],
) -> Result<Overlap, ModelError> {
    let scale = width * fingers;
    Ok(Overlap {
        linear: scale * (real(model, names[0])? + real(model, "cf")?),
        lateral: scale * binned(size, names[1])?,
        kappa: positive(binned(size, "ckappa")?, "overlap kappa")?,
    })
}

fn bottom_law(
    setup: &PreparedBsimSoiSetup,
    size: &PreparedBsimSoiSize,
    index: usize,
    ambient: f64,
    polarity: f64,
) -> Result<BottomLaw, ModelError> {
    let model = setup.model.parameters();
    let given = |name| slot(ParameterScope::Model, name).map(|id| setup.model.was_given(id));
    let cbox = EPS_OX / positive(real(model, "tbox")?, "buried oxide thickness")?;
    let thermal = setup.prepare_temperature(index, ambient)?;
    let material = thermal.ambient_point()?;
    let nsub = binned(size, "nsub")?;
    let ni = material.intrinsic_density.value;
    let vtm = material.thermal_voltage.value;
    let epssub = thermal.substrate_permittivity();
    let generalized = integer(model, "mtrlmod")? != 0;
    let phi = positive(
        2. * vtm * (nsub.abs() / ni).ln(),
        "bottom surface potential",
    )?;
    let minimum = if given("csdmin")? {
        real(model, "csdmin")?
    } else {
        let depletion = (2. * epssub * phi / (CHARGE * nsub.abs() * 1e6)).sqrt();
        let capacitance = epssub / depletion;
        1. / (1. / capacitance + 1. / cbox)
    };
    if minimum == 0. {
        return Ok(BottomLaw::Linear(cbox));
    }
    let flatband = if given("vsdfb")? {
        binned(size, "vsdfb")?
    } else if nsub > 0. {
        -polarity * vtm.mul_add((1e20 * nsub / ni / ni).ln(), -0.3)
    } else {
        -polarity * vtm.mul_add((-1e20 / nsub).ln(), 0.3)
    };
    let same = nsub * polarity > 0.;
    let threshold = if given("vsdth")? {
        binned(size, "vsdth")?
    } else {
        let coefficient = if generalized {
            (2. * 1e6 * CHARGE * epssub).sqrt()
        } else {
            5.753e-12
        };
        let offset = (coefficient * nsub.abs().sqrt() / cbox).mul_add(phi.sqrt(), phi);
        if same {
            flatband + offset
        } else {
            flatband - offset
        }
    };
    let (lower_voltage, upper_voltage, lower_capacitance, upper_capacitance) = if same {
        (flatband, threshold, cbox, minimum)
    } else {
        (threshold, flatband, minimum, cbox)
    };
    let fraction = real(model, "asd")?;
    positive(
        upper_voltage - lower_voltage,
        "bottom storage voltage range",
    )?;
    if !(0. < fraction && fraction < 1.) {
        return Err(ModelError::InvalidPhysicalParameter("bottom storage ASD"));
    }
    Ok(BottomLaw::Nonlinear(Transition {
        lower_voltage,
        upper_voltage,
        fraction,
        lower_capacitance,
        upper_capacitance,
    }))
}

impl PreparedBsimSoiSetup {
    /// Bind extrinsic overlap and buried-oxide pair charges.
    /// # Errors
    /// Missing instance data or invalid storage geometry.
    pub fn prepare_extrinsic_charges(
        &self,
        index: usize,
        ambient: f64,
    ) -> Result<PreparedBsimSoiExtrinsicCharges, ModelError> {
        let model = self.model.parameters();
        let size = self
            .sizes
            .get(index)
            .ok_or(ModelError::MismatchedPreparedContext)?;
        let instance = self
            .instances
            .get(index)
            .ok_or(ModelError::MismatchedPreparedContext)?;
        let topology = instance.topology();
        let p = instance.parameters();
        let geometry = size.geometry();
        let polarity = if self.model.polarity() == MosPolarity::Nmos {
            1.
        } else {
            -1.
        };
        let fingers = positive(real(p, "nf")?, "overlap finger count")?;
        let fringe = real(model, "csdesw")?
            * (real(model, "cfrcoeff")? * (1. + real(model, "tsi")? / real(model, "tbox")?)).ln();
        let width = real(p, "w")?;
        let bottom = |area, perimeter| -> Result<Bottom, ModelError> {
            Ok(Bottom {
                area: real(p, area)?,
                fringe: fringe * (real(p, perimeter)? - width).max(0.),
            })
        };
        Ok(PreparedBsimSoiExtrinsicCharges {
            overlap_node: if integer(p, "rgatemod")? == 3 {
                topology.gate_mid
            } else {
                topology.gate
            },
            source: overlap(
                model,
                size,
                geometry.source_diode_width_cv,
                fingers,
                ["cgso", "cgsl"],
            )?,
            drain: overlap(
                model,
                size,
                geometry.drain_diode_width_cv,
                fingers,
                ["cgdo", "cgdl"],
            )?,
            gate_backgate: real(model, "cgeo")? * geometry.leff_cv * fingers,
            bottom: bottom_law(self, size, index, ambient, polarity)?,
            source_bottom: bottom("as", "ps")?,
            drain_bottom: bottom("ad", "pd")?,
            polarity,
            multiplicity: positive(real(p, "m")?, "overlap multiplicity")?,
        })
    }
}
