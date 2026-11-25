//! Native VBIC topology, electrothermal residual, and source excess-phase states.

use super::{
    avalanche, binding, charge, noise, resistance,
    temperature::{self, Coefficient},
};
use crate::{EvaluationError, Temperature};
use model_kernel_api::{
    Error, Point,
    noise::{Injection, Sink, Source},
};

/// Physical source nodes, before nominal exact-short reduction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum Node {
    C,
    B,
    E,
    S,
    Cx,
    Ci,
    Bx,
    Bi,
    Bp,
    Ei,
    Si,
}

impl Node {
    pub const ALL: [Self; 11] = [
        Self::C,
        Self::B,
        Self::E,
        Self::S,
        Self::Cx,
        Self::Ci,
        Self::Bx,
        Self::Bi,
        Self::Bp,
        Self::Ei,
        Self::Si,
    ];
    #[must_use = "the source name identifies the electrical coordinate"]
    pub const fn name(self) -> &'static str {
        match self {
            Self::C => "c",
            Self::B => "b",
            Self::E => "e",
            Self::S => "s",
            Self::Cx => "cx",
            Self::Ci => "ci",
            Self::Bx => "bx",
            Self::Bi => "bi",
            Self::Bp => "bp",
            Self::Ei => "ei",
            Self::Si => "si",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    slots: [usize; 11],
    electrical: usize,
    phase: bool,
    thermal: bool,
}

impl Layout {
    /// The native three internal nodes are always present. Four outer series
    /// resistors collapse only when their nominal value is exactly zero.
    #[must_use = "the layout determines all local coordinate bindings"]
    pub fn new(model: &binding::Model) -> Self {
        let mut slots = [0; 11];
        let mut next = 4;
        for node in Node::ALL {
            slots[node as usize] = if (node as usize) < 4 {
                node as usize
            } else {
                let outer = match node {
                    Node::Cx => Some((Coefficient::Rcx, Node::C)),
                    Node::Bx => Some((Coefficient::Rbx, Node::B)),
                    Node::Ei => Some((Coefficient::Re, Node::E)),
                    Node::Si => Some((Coefficient::Rs, Node::S)),
                    _ => None,
                };
                if let Some((parameter, terminal)) = outer
                    && model.nominal().value(parameter) == 0.0
                {
                    terminal as usize
                } else {
                    let row = next;
                    next += 1;
                    row
                }
            };
        }
        Self {
            slots,
            electrical: next,
            phase: model.nominal().value(Coefficient::Td) > 0.0,
            thermal: model.self_heating(),
        }
    }

    #[must_use = "the index maps a source node to its reduced electrical row"]
    pub const fn node(self, node: Node) -> usize {
        self.slots[node as usize]
    }

    #[must_use = "the count sizes local residual and Jacobian buffers"]
    pub const fn count(self) -> usize {
        self.electrical + if self.phase { 2 } else { 0 } + if self.thermal { 1 } else { 0 }
    }

    #[must_use = "the optional state rows are needed for excess-phase integration"]
    pub const fn phase_rows(self) -> Option<[usize; 2]> {
        if self.phase {
            Some([self.electrical, self.electrical + 1])
        } else {
            None
        }
    }

    #[must_use = "the optional thermal row identifies live self-heating"]
    pub const fn thermal_row(self) -> Option<usize> {
        if self.thermal {
            Some(self.count() - 1)
        } else {
            None
        }
    }
}

#[derive(Clone, Copy)]
struct Pair(Node, Node);
impl Pair {
    fn voltage(self, layout: Layout, x: &[f64], sign: f64) -> f64 {
        sign * (x[layout.node(self.0)] - x[layout.node(self.1)])
    }
    const fn slots(self, layout: Layout) -> (usize, usize) {
        (layout.node(self.0), layout.node(self.1))
    }
}

use Node::{B, Bi, Bp, Bx, C, Ci, Cx, E, Ei, S, Si};
const BEI: Pair = Pair(Bi, Ei);
const BEX: Pair = Pair(Bx, Ei);
const BCI: Pair = Pair(Bi, Ci);
const BCX: Pair = Pair(Bi, Cx);
const BEP: Pair = Pair(Bx, Bp);
const BCP: Pair = Pair(Si, Bp);
const BE: Pair = Pair(B, E);
const BC: Pair = Pair(B, C);
const CEI: Pair = Pair(Ci, Ei);
const CCP: Pair = Pair(Bx, Si);
const RCX: Pair = Pair(C, Cx);
const RCI: Pair = Pair(Cx, Ci);
const RBX: Pair = Pair(B, Bx);
const RBI: Pair = Pair(Bx, Bi);
const RE: Pair = Pair(E, Ei);
const RBP: Pair = Pair(Bp, Cx);
const RS: Pair = Pair(S, Si);

/// Prepared nominal source equations and fixed geometry.
/// Thermal coordinates use absolute kelvin; the native temperature-rise voltage
/// is the difference from the instance effective ambient temperature.
#[derive(Clone, Copy, Debug)]
pub struct Prepared {
    layout: Layout,
    baseline: temperature::Prepared,
    family: Family,
    noise: noise::Prepared,
    scale: f64,
    sign: f64,
    delay: f64,
    rth: f64,
    cth: f64,
}

#[derive(Clone, Copy, Debug)]
struct Family {
    diode: super::Prepared,
    charge: charge::Prepared,
    resistance: resistance::Prepared,
    avalanche: avalanche::Prepared,
}

impl Family {
    fn new(t: &temperature::Prepared) -> Result<Self, Error> {
        Ok(Self {
            diode: super::Prepared::from_temperature(t).map_err(domain)?,
            charge: charge::Prepared::from_temperature(t).map_err(domain)?,
            resistance: resistance::Prepared::from_temperature(t).map_err(domain)?,
            avalanche: avalanche::Prepared::from_temperature(t).map_err(domain)?,
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StampKind {
    Current,
    CurrentWithoutHeat,
    Charge,
}

struct Contribution<'a> {
    value: f64,
    slopes: &'a [(Pair, f64)],
    temperature: f64,
}

struct Stamps<'a, 'b> {
    layout: Layout,
    scale: f64,
    sign: f64,
    point: &'a mut Point<'b>,
    coordinates: &'a [f64],
}

impl Stamps<'_, '_> {
    fn branch(&mut self, pair: Pair, contribution: Contribution<'_>, kind: StampKind) {
        let (from, to) = pair.slots(self.layout);
        if from == to {
            return;
        }
        if kind == StampKind::Current {
            self.heat(pair, &contribution);
        }
        let width = self.layout.count();
        let (values, matrix) = if kind == StampKind::Charge {
            (&mut self.point.charge, &mut self.point.capacitance)
        } else {
            (&mut self.point.current, &mut self.point.conductance)
        };
        let signed_scale = self.sign * self.scale;
        let value = signed_scale * contribution.value;
        values[from] += value;
        values[to] -= value;
        for &(pair, slope) in contribution.slopes {
            let (positive, negative) = pair.slots(self.layout);
            if positive == negative {
                continue;
            }
            let slope = self.scale * slope;
            matrix[from * width + positive] += slope;
            matrix[from * width + negative] -= slope;
            matrix[to * width + positive] -= slope;
            matrix[to * width + negative] += slope;
        }
        if let Some(thermal) = self.layout.thermal_row() {
            let slope = signed_scale * contribution.temperature;
            matrix[from * width + thermal] += slope;
            matrix[to * width + thermal] -= slope;
        }
    }

    // Negative source heat is part of the heat-balance residual. This uses
    // undelayed transport even when an excess-phase state is present, as
    // specified by the native VBIC heat-generation law.
    fn heat(&mut self, pair: Pair, contribution: &Contribution<'_>) {
        let Some(row) = self.layout.thermal_row() else {
            return;
        };
        let (from, to) = pair.slots(self.layout);
        if from == to {
            return;
        }
        let width = self.layout.count();
        let voltage = pair.voltage(self.layout, self.coordinates, self.sign);
        let power_scale = -self.scale * voltage;
        self.point.current[row] = power_scale.mul_add(contribution.value, self.point.current[row]);
        let gradient = &mut self.point.conductance[row * width..(row + 1) * width];
        let direct = -self.scale * self.sign * contribution.value;
        gradient[from] += direct;
        gradient[to] -= direct;
        for &(control, slope) in contribution.slopes {
            let (positive, negative) = control.slots(self.layout);
            if positive != negative {
                let derivative = power_scale * self.sign * slope;
                gradient[positive] += derivative;
                gradient[negative] -= derivative;
            }
        }
        gradient[row] = power_scale.mul_add(contribution.temperature, gradient[row]);
    }

    fn single(&mut self, pair: Pair, value: f64, slope: f64, temperature: f64, kind: StampKind) {
        self.branch(
            pair,
            Contribution {
                value,
                slopes: &[(pair, slope)],
                temperature,
            },
            kind,
        );
    }

    fn thermal_storage(&mut self, baseline: f64, rth: f64, cth: f64) {
        if let Some(row) = self.layout.thermal_row() {
            let width = self.layout.count();
            let rise = self.coordinates[row] - baseline;
            let conductance = self.scale / rth;
            let capacity = self.scale * cth;
            self.point.current[row] = conductance.mul_add(rise, self.point.current[row]);
            self.point.conductance[row * width + row] += conductance;
            self.point.charge[row] = capacity * rise;
            self.point.capacitance[row * width + row] += capacity;
        }
    }

    fn phase(
        &mut self,
        rows: [usize; 2],
        coordinates: &[f64],
        transport: super::transport::Output,
        td: f64,
    ) {
        let [first, second] = rows;
        let width = self.layout.count();
        // Internal states are normalized forward currents, not terminal voltages.
        self.point.current[first] += self
            .scale
            .mul_add(-transport.forward.value, coordinates[second]);
        self.point.current[second] += coordinates[second] - coordinates[first];
        self.point.charge[first] = td.mul_add(coordinates[first], self.point.charge[first]);
        self.point.charge[second] =
            (td / 3.0).mul_add(coordinates[second], self.point.charge[second]);
        self.point.conductance[first * width + second] += 1.0;
        self.point.conductance[second * width + second] += 1.0;
        self.point.conductance[second * width + first] -= 1.0;
        self.point.capacitance[first * width + first] += td;
        self.point.capacitance[second * width + second] += td / 3.0;
        for (pair, derivative) in [
            (BEI, transport.forward.d_vbei),
            (BCI, transport.forward.d_vbci),
        ] {
            let (positive, negative) = pair.slots(self.layout);
            let slope = self.scale * self.sign * derivative;
            self.point.conductance[first * width + positive] -= slope;
            self.point.conductance[first * width + negative] += slope;
        }
        if let Some(thermal) = self.layout.thermal_row() {
            let index = first * width + thermal;
            self.point.conductance[index] = (-self.scale).mul_add(
                transport.forward.d_temperature,
                self.point.conductance[index],
            );
        }
        let (collector, emitter) = CEI.slots(self.layout);
        self.point.current[collector] = self
            .sign
            .mul_add(coordinates[second], self.point.current[collector]);
        self.point.current[emitter] = self
            .sign
            .mul_add(-coordinates[second], self.point.current[emitter]);
        self.point.conductance[collector * width + second] += self.sign;
        self.point.conductance[emitter * width + second] -= self.sign;
    }
}

struct Electrical {
    phys: charge::Physics,
    diode: super::Currents,
    avalanche: avalanche::Output,
    resistive: resistance::Output,
}

impl Electrical {
    fn operating(&self, scale: f64, net: f64) -> noise::OperatingPoint {
        noise::OperatingPoint {
            conductances: noise::Conductances {
                rc: scale * self.resistive.ircx.siemens,
                rci: scale * self.resistive.irci.d_resistor,
                rb: scale * self.resistive.irbx.siemens,
                rbi: scale * self.resistive.irbi.d_resistor,
                re: scale * self.resistive.ire.siemens,
                rbp: scale * self.resistive.irbp.d_resistor,
                rs: scale * self.resistive.irs.siemens,
            },
            currents: noise::Currents {
                ic: net,
                ibe: scale * self.diode.ibe.amperes,
                ibep: scale * self.diode.ibep.amperes,
                iccp: scale * self.phys.parasitic.iccp,
            },
        }
    }
}

impl Stamps<'_, '_> {
    fn electrical(&mut self, source: &Electrical, x: &[f64], delay: f64) -> f64 {
        let thermal = source.diode.temperature;
        for (pair, branch, slope) in [
            (BEI, source.diode.ibe, thermal.ibe),
            (BEX, source.diode.ibex, thermal.ibex),
            (BEP, source.diode.ibep, thermal.ibep),
            (BCP, source.diode.ibcp, thermal.ibcp),
        ] {
            self.single(
                pair,
                branch.amperes,
                branch.siemens,
                slope,
                StampKind::Current,
            );
        }
        let av = source.avalanche;
        self.branch(
            BCI,
            Contribution {
                value: av.ibc,
                slopes: &[(BEI, av.d_vbei), (BCI, av.d_vbci)],
                temperature: av.d_temperature,
            },
            StampKind::Current,
        );
        let transport = source.phys.transport;
        let net_branch = Contribution {
            value: transport.net.value,
            slopes: &[(BEI, transport.net.d_vbei), (BCI, transport.net.d_vbci)],
            temperature: transport.net.d_temperature,
        };
        let net = if let Some(rows) = self.layout.phase_rows() {
            self.phase(rows, x, transport, delay);
            self.heat(CEI, &net_branch);
            self.branch(
                CEI,
                Contribution {
                    value: -transport.reverse.value,
                    slopes: &[
                        (BEI, -transport.reverse.d_vbei),
                        (BCI, -transport.reverse.d_vbci),
                    ],
                    temperature: -transport.reverse.d_temperature,
                },
                StampKind::CurrentWithoutHeat,
            );
            self.scale.mul_add(-transport.reverse.value, x[rows[1]])
        } else {
            self.branch(CEI, net_branch, StampKind::Current);
            self.scale * transport.net.value
        };
        let parasitic = source.phys.parasitic;
        self.branch(
            CCP,
            Contribution {
                value: parasitic.iccp,
                slopes: &[
                    (BEP, parasitic.d_vbep),
                    (BCI, parasitic.d_vbci),
                    (BCP, parasitic.d_vbcp),
                ],
                temperature: parasitic.d_temperature,
            },
            StampKind::Current,
        );
        for (pair, branch) in [
            (RCX, source.resistive.ircx),
            (RBX, source.resistive.irbx),
            (RE, source.resistive.ire),
            (RS, source.resistive.irs),
        ] {
            self.single(
                pair,
                branch.amperes,
                branch.siemens,
                branch.d_temperature,
                StampKind::Current,
            );
        }
        for (pair, first, second, branch) in [
            (RCI, BCI, BCX, source.resistive.irci),
            (RBI, BEI, BCI, source.resistive.irbi),
            (RBP, BEP, BCI, source.resistive.irbp),
        ] {
            self.branch(
                pair,
                Contribution {
                    value: branch.amperes,
                    slopes: &[
                        (pair, branch.d_resistor),
                        (first, branch.d_first),
                        (second, branch.d_second),
                    ],
                    temperature: branch.d_temperature,
                },
                StampKind::Current,
            );
        }
        net
    }

    fn storage(&mut self, charges: charge::Output) {
        for (pair, branch) in [
            (BEX, charges.qbex),
            (BCI, charges.qbc),
            (BCX, charges.qbcx),
            (BCP, charges.qbcp),
            (BE, charges.qbeo),
            (BC, charges.qbco),
        ] {
            self.single(
                pair,
                branch.coulombs,
                branch.farads,
                branch.d_temperature,
                StampKind::Charge,
            );
        }
        for (pair, branch) in [(BEI, charges.qbe), (BEP, charges.qbep)] {
            self.branch(
                pair,
                Contribution {
                    value: branch.coulombs,
                    slopes: &[(pair, branch.primary_farads), (BCI, branch.coupled_farads)],
                    temperature: branch.d_temperature,
                },
                StampKind::Charge,
            );
        }
    }
}

impl Prepared {
    /// Validate and prepare the complete source branch set.
    ///
    /// # Errors
    /// Invalid physical parameters or temperature transformation.
    pub fn new(
        model: &binding::Model,
        instance: binding::Instance,
        ambient: Temperature,
    ) -> Result<Self, Error> {
        let layout = Layout::new(model);
        let t = model
            .nominal()
            .prepare(instance.ambient(ambient).map_err(domain)?)
            .map_err(domain)?;
        let effective = t.temperature();
        Ok(Self {
            layout,
            baseline: t,
            family: Family::new(&t)?,
            noise: noise::Prepared::new(t.noise_parameters(), effective, instance.multiplicity())
                .map_err(domain)?,
            scale: instance.scale(),
            sign: model.polarity().sign(),
            delay: t.value(Coefficient::Td),
            rth: t.value(Coefficient::Rth),
            cth: t.value(Coefficient::Cth),
        })
    }

    #[must_use = "the layout identifies the local output rows"]
    pub const fn layout(&self) -> Layout {
        self.layout
    }

    /// Fill all reduced electrical, electrothermal and excess-phase residuals and Jacobians,
    /// returning the same operating-point quantities used for noise.
    ///
    /// # Errors
    /// Incorrect output shape, invalid local point or numerical failure.
    pub fn evaluate(
        &self,
        x: &[f64],
        point: &mut Point<'_>,
    ) -> Result<noise::OperatingPoint, Error> {
        let n = self.layout.count();
        if x.len() != n
            || point.current.len() != n
            || point.charge.len() != n
            || point.conductance.len() != n * n
            || point.capacitance.len() != n * n
        {
            return Err(Error::Shape("VBIC point"));
        }
        if !x.iter().all(|x| x.is_finite()) {
            return Err(domain(EvaluationError::NonFiniteBias));
        }
        point.current.fill(0.0);
        point.charge.fill(0.0);
        point.conductance.fill(0.0);
        point.capacitance.fill(0.0);
        let baseline = self.baseline.temperature().kelvin();
        let thermal_family;
        let family = if let Some(row) = self.layout.thermal_row() {
            let profile = self.baseline.at_rise(x[row] - baseline).map_err(domain)?;
            thermal_family = Family::new(&profile)?;
            &thermal_family
        } else {
            &self.family
        };
        let source = self.evaluate_branches(family, x)?;
        let mut stamp = Stamps {
            layout: self.layout,
            scale: self.scale,
            sign: self.sign,
            point,
            coordinates: x,
        };
        let net = stamp.electrical(&source, x, self.delay);
        stamp.storage(source.phys.charge);
        stamp.thermal_storage(baseline, self.rth, self.cth);
        if !point
            .current
            .iter()
            .chain(point.charge.iter())
            .chain(point.conductance.iter())
            .chain(point.capacitance.iter())
            .all(|value| value.is_finite())
        {
            return Err(domain(EvaluationError::NonFiniteResult));
        }
        Ok(source.operating(self.scale, net))
    }

    fn evaluate_branches(&self, family: &Family, x: &[f64]) -> Result<Electrical, Error> {
        let voltage = |pair: Pair| pair.voltage(self.layout, x, self.sign);
        let bias = charge::Bias {
            vbei: voltage(BEI),
            vbex: voltage(BEX),
            vbci: voltage(BCI),
            vbcx: voltage(BCX),
            vbep: voltage(BEP),
            vbcp: voltage(BCP),
            vbe: voltage(BE),
            vbc: voltage(BC),
        };
        let phys = family.charge.evaluate_physics(bias).map_err(domain)?;
        let di = family
            .diode
            .evaluate(super::Bias {
                vbei: bias.vbei,
                vbex: bias.vbex,
                vbci: bias.vbci,
                vbep: bias.vbep,
                vbcp: bias.vbcp,
            })
            .map_err(domain)?;
        let av = family
            .avalanche
            .evaluate_thermal(
                avalanche::Bias {
                    vbci: bias.vbci,
                    transport: phys.transport,
                    ibcj: di.ibcj,
                },
                di.temperature.ibcj,
            )
            .map_err(domain)?;
        let resistive = family
            .resistance
            .evaluate(&resistance::Bias {
                vrcx: voltage(RCX),
                vrci: voltage(RCI),
                vbci: bias.vbci,
                vbcx: bias.vbcx,
                vrbx: voltage(RBX),
                vrbi: voltage(RBI),
                vre: voltage(RE),
                vrbp: voltage(RBP),
                vrs: voltage(RS),
                transport: phys.transport,
                parasitic: phys.parasitic,
            })
            .map_err(domain)?;
        Ok(Electrical {
            phys,
            diode: di,
            avalanche: av,
            resistive,
        })
    }

    /// Evaluate physical local noise from the same source operating point.
    /// # Errors
    /// Invalid source spectrum or frequency.
    pub fn noise(
        &self,
        operating: noise::OperatingPoint,
        frequency: f64,
    ) -> Result<noise::Output, EvaluationError> {
        self.noise.evaluate(operating, frequency)
    }
}

/// Emit independent physical noise sources with the same branch incidence
/// used by the residual. Collapsed branches cannot inject into the circuit.
/// # Errors
/// Consumer rejection or invalid source spectrum.
pub(super) fn inject_noise(
    layout: Layout,
    spectrum: noise::Output,
    sink: &mut dyn Sink,
) -> Result<(), Error> {
    let thermal = spectrum.thermal;
    let shot = spectrum.shot;
    let flicker = spectrum.flicker;
    for (pair, name, density) in [
        (RCX, "rcx", thermal.rc),
        (RCI, "rci", thermal.rci),
        (RBX, "rbx", thermal.rb),
        (RBI, "rbi", thermal.rbi),
        (RE, "re", thermal.re),
        (RBP, "rbp", thermal.rbp),
        (RS, "rs", thermal.rs),
        (CEI, "ic", shot.ic),
        (BEI, "ibe", shot.ibe),
        (BEP, "ibep", shot.ibep),
        (CCP, "iccp", shot.iccp),
        (BEI, "flicker_be", flicker.ibe),
        (BEP, "flicker_bep", flicker.ibep),
    ] {
        let (positive, negative) = pair.slots(layout);
        if positive != negative && density != 0. {
            sink.push(Source {
                name,
                density,
                injections: &[
                    Injection {
                        row: positive,
                        re: 1.,
                        im: 0.,
                    },
                    Injection {
                        row: negative,
                        re: -1.,
                        im: 0.,
                    },
                ],
            })?;
        }
    }
    Ok(())
}

fn domain(error: impl std::fmt::Display) -> Error {
    Error::Domain(error.to_string())
}
