//! Complete fixed-width source branch residuals and charge derivatives.
//!
//! Physical quantities are in the published Verilog-A branch orientation,
//! before exact ideal-short node reduction and without solver Gmin.
//! The three auxiliary NQS residuals are independent of the 18 electrical
//! branches. d/dTf remains explicit for exact transit-time chain rules.
//! The circuit solver owns timestep integration and global nodal assembly.

use super::{
    Polarity, device, external, internal_base, junction, nqs, substrate, thermal,
    topology::{Branch, Node},
    transport, tunneling,
};
use crate::{Branch as Diode, EvaluationError};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Gradient {
    pub nodes: [f64; Node::COUNT],
    pub states: nqs::State,
    /// Partial derivative with respect to effective Tf held as a
    /// separate HICCR dependency, not its intrinsic-voltage gradients.
    pub d_tf: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Quantity {
    pub value: f64,
    pub gradient: Gradient,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PhysicalBranch {
    pub current: Quantity,
    pub charge: Quantity,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Auxiliary {
    pub current: Quantity,
    pub charge: Quantity,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Output {
    /// Fixed order exactly equal to the published branch metadata table.
    pub branches: [PhysicalBranch; Branch::ALL.len()],
    /// Source branches (xf1,0), (xf2,0), (xf,0), in that order.
    pub auxiliary: [Auxiliary; 3],
    /// Thermal source-to-ground row. Electrical derivatives are exact
    /// at fixed effective device temperature, with rise partial separate.
    pub thermal: ThermalStamp,
}

/// Full electrical cross-Jacobian for the thermal KCL. The source heat
/// capacity is a dynamic rise-to-ground charge, not a device voltage charge.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ThermalStamp {
    pub current: Quantity,
    pub charge: Quantity,
    /// d(KCL)/d(rise) with electrical currents and resistances held fixed.
    /// The solver must add their total temperature derivatives separately.
    pub d_rise_at_fixed_electrical: f64,
    pub heat_capacitance: f64,
}

impl Output {
    #[must_use = "the returned physical contribution is required for branch stamping"]
    pub const fn branch(&self, branch: Branch) -> PhysicalBranch {
        self.branches[branch.index()]
    }

    const fn set(&mut self, branch: Branch, current: Quantity, charge: Quantity) {
        self.branches[branch.index()] = PhysicalBranch { current, charge };
    }

    fn is_finite(&self) -> bool {
        let finite = |q: Quantity| {
            q.value.is_finite()
                && q.gradient.nodes.iter().all(|v| v.is_finite())
                && [
                    q.gradient.states.current_first,
                    q.gradient.states.current_second,
                    q.gradient.states.stored_charge,
                    q.gradient.d_tf,
                ]
                .iter()
                .all(|v| v.is_finite())
        };
        self.branches
            .iter()
            .all(|b| finite(b.current) && finite(b.charge))
            && self
                .auxiliary
                .iter()
                .all(|a| finite(a.current) && finite(a.charge))
            && finite(self.thermal.current)
            && finite(self.thermal.charge)
            && self.thermal.d_rise_at_fixed_electrical.is_finite()
            && self.thermal.heat_capacitance.is_finite()
    }
}

/// Map all physical source currents and charges to their exact branch
/// incidence and analytic nine-node plus NQS local derivatives.
///
/// The electrical-node columns are fixed-effective-temperature partials.
/// The separate Tf partial belongs to the solver's HICCR transit-time
/// dependency, including its variation with the intrinsic bias.
///
/// # Errors
/// Detects overflow when combining individually finite source branches.
pub fn assemble(
    polarity: Polarity,
    tunneling_location: tunneling::Location,
    op: &device::Output,
) -> Result<Output, EvaluationError> {
    let polarity_sign = sign(polarity);
    let mut out = Output::default();
    out.fill_resistors(op, polarity_sign);
    out.fill_emitters(op, polarity_sign, tunneling_location);
    out.fill_collector(op, polarity_sign);
    out.fill_transport_substrate(op, polarity_sign);
    let delay = op.nqs;
    out.thermal = thermal_row(polarity_sign, op);
    out.auxiliary = [
        auxiliary(delay.first_current, delay.first_charge, polarity_sign),
        auxiliary(delay.second_current, delay.second_charge, polarity_sign),
        auxiliary(
            delay.charge_current,
            delay.charge_filter_charge,
            polarity_sign,
        ),
    ];
    if !out.is_finite() {
        return Err(EvaluationError::NonFiniteResult);
    }
    Ok(out)
}

impl Output {
    fn fill_resistors(&mut self, op: &device::Output, polarity_sign: f64) {
        let ext = op.external;
        self.set(
            Branch::Rbx,
            resistor(ext.external_base, Node::B, Node::Bp),
            Quantity::default(),
        );
        self.set(
            Branch::Rbi,
            base_current(polarity_sign, op.internal_base),
            base_charge(polarity_sign, op.internal_base),
        );
        self.set(
            Branch::Rcx,
            resistor(ext.external_collector, Node::Ci, Node::C),
            Quantity::default(),
        );
        self.set(
            Branch::Re,
            resistor(ext.emitter, Node::Ei, Node::E),
            Quantity::default(),
        );
        self.set(
            Branch::Rsu,
            resistor(ext.substrate, Node::Si, Node::S),
            capacitor(ext.substrate_shunt, Node::Si, Node::S),
        );
    }

    fn fill_emitters(
        &mut self,
        op: &device::Output,
        polarity_sign: f64,
        tunneling_location: tunneling::Location,
    ) {
        let diodes = op.diode;
        let junctions = op.junction;
        let transfer = op.transfer;
        let delay = op.nqs;
        let be = diode(polarity_sign, diodes.ibei, Node::Bi, Node::Ei)
            + diode(polarity_sign, diodes.irei, Node::Bi, Node::Ei)
            + intrinsic(polarity_sign, transfer.barrier_base_current);
        let bpe = diode(polarity_sign, diodes.ibep, Node::Bp, Node::Ei)
            + diode(polarity_sign, diodes.irep, Node::Bp, Node::Ei);
        let tunneled = op.tunneling;
        let tunnel_current = tunnel_quantity(polarity_sign, tunneled);
        self.set(
            Branch::IntrinsicEmitter,
            be + if matches!(tunneling_location, tunneling::Location::Intrinsic) {
                tunnel_current
            } else {
                Quantity::default()
            },
            depletion(
                polarity_sign,
                junctions.intrinsic_emitter,
                Node::Bi,
                Node::Ei,
            ) + intrinsic(polarity_sign, delay.delayed_charge)
                .with_states(delay.delayed_charge_d_state, polarity_sign),
        );
        self.set(
            Branch::PeripheralEmitter,
            bpe + if matches!(tunneling_location, tunneling::Location::Peripheral) {
                tunnel_current
            } else {
                Quantity::default()
            },
            depletion(
                polarity_sign,
                junctions.peripheral_emitter,
                Node::Bp,
                Node::Ei,
            ),
        );
    }

    fn fill_collector(&mut self, op: &device::Output, polarity_sign: f64) {
        let diodes = op.diode;
        let junctions = op.junction;
        let transfer = op.transfer;
        let ext = op.external;
        self.set(
            Branch::IntrinsicCollector,
            diode(polarity_sign, diodes.ibci, Node::Bi, Node::Ci),
            depletion(
                polarity_sign,
                junctions.intrinsic_collector,
                Node::Bi,
                Node::Ci,
            ) + intrinsic(polarity_sign, transfer.reverse_diffusion_charge),
        );
        self.set(
            Branch::ExternalEmitterIsolation,
            Quantity::default(),
            capacitor(ext.external_emitter_isolation, Node::B, Node::E),
        );
        self.set(
            Branch::PeripheralEmitterIsolation,
            Quantity::default(),
            capacitor(ext.peripheral_emitter_isolation, Node::Bp, Node::E),
        );
        self.set(
            Branch::ExternalCollectorJunction,
            Quantity::default(),
            depletion(
                polarity_sign,
                junctions.external_collector,
                Node::B,
                Node::Ci,
            ) + capacitor(ext.external_collector_isolation, Node::B, Node::Ci),
        );
        self.set(
            Branch::PeripheralCollector,
            diode(polarity_sign, diodes.ijbcx, Node::Bp, Node::Ci),
            depletion(
                polarity_sign,
                junctions.peripheral_collector,
                Node::Bp,
                Node::Ci,
            ) + substrate(polarity_sign, op.substrate.diffusion_charge)
                + capacitor(ext.peripheral_collector_isolation, Node::Bp, Node::Ci),
        );
    }

    fn fill_transport_substrate(&mut self, op: &device::Output, polarity_sign: f64) {
        let diodes = op.diode;
        let junctions = op.junction;
        let transfer = op.transfer;
        let delay = op.nqs;
        self.set(
            Branch::TransferForward,
            intrinsic(polarity_sign, delay.delayed_current)
                .with_states(delay.delayed_current_d_state, polarity_sign),
            Quantity::default(),
        );
        self.set(
            Branch::TransferReverse,
            intrinsic(polarity_sign, transfer.reverse_current),
            Quantity::default(),
        );
        self.set(
            Branch::SubstrateJunction,
            diode(polarity_sign, diodes.ijsc, Node::Si, Node::Ci),
            depletion(polarity_sign, junctions.substrate, Node::Si, Node::Ci),
        );
        self.set(
            Branch::PeripheralSubstrateJunction,
            Quantity::default(),
            depletion(
                polarity_sign,
                junctions.peripheral_substrate,
                Node::S,
                Node::C,
            ),
        );
        self.set(
            Branch::SubstrateTransfer,
            substrate(polarity_sign, op.substrate.current),
            Quantity::default(),
        );
        self.set(
            Branch::Avalanche,
            intrinsic(polarity_sign, op.avalanche.generation),
            Quantity::default(),
        );
    }
}

impl Quantity {
    #[must_use = "the updated quantity is returned rather than modifying the original"]
    fn with_states(mut self, state: nqs::State, scale: f64) -> Self {
        self.gradient.states = nqs::State {
            current_first: scale * state.current_first,
            current_second: scale * state.current_second,
            stored_charge: scale * state.stored_charge,
        };
        self
    }
}

impl std::ops::Add for Quantity {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        let mut nodes = [0.0; Node::COUNT];
        for node in Node::ALL {
            nodes[node.index()] =
                self.gradient.nodes[node.index()] + other.gradient.nodes[node.index()];
        }
        Self {
            value: self.value + other.value,
            gradient: Gradient {
                nodes,
                states: nqs::State {
                    current_first: self.gradient.states.current_first
                        + other.gradient.states.current_first,
                    current_second: self.gradient.states.current_second
                        + other.gradient.states.current_second,
                    stored_charge: self.gradient.states.stored_charge
                        + other.gradient.states.stored_charge,
                },
                d_tf: self.gradient.d_tf + other.gradient.d_tf,
            },
        }
    }
}

const fn sign(polarity: Polarity) -> f64 {
    match polarity {
        Polarity::Npn => 1.0,
        Polarity::Pnp => -1.0,
    }
}

fn add_endpoints(mut quantity: Quantity, from: Node, to: Node, slope: f64) -> Quantity {
    quantity.gradient.nodes[from.index()] += slope;
    quantity.gradient.nodes[to.index()] -= slope;
    quantity
}

fn resistor(branch: external::Resistor, from: Node, to: Node) -> Quantity {
    add_endpoints(
        Quantity {
            value: branch.amperes,
            ..Quantity::default()
        },
        from,
        to,
        branch.siemens,
    )
}

fn capacitor(branch: external::Capacitor, from: Node, to: Node) -> Quantity {
    add_endpoints(
        Quantity {
            value: branch.coulombs,
            ..Quantity::default()
        },
        from,
        to,
        branch.farads,
    )
}

fn diode(sign: f64, branch: Diode, from: Node, to: Node) -> Quantity {
    add_endpoints(
        Quantity {
            value: sign * branch.amperes,
            ..Quantity::default()
        },
        from,
        to,
        branch.siemens,
    )
}

fn depletion(sign: f64, branch: junction::Junction, from: Node, to: Node) -> Quantity {
    add_endpoints(
        Quantity {
            value: sign * branch.coulombs,
            ..Quantity::default()
        },
        from,
        to,
        branch.farads,
    )
}

fn intrinsic(sign: f64, value: transport::Value2) -> Quantity {
    let mut output = Quantity {
        value: sign * value.value,
        ..Quantity::default()
    };
    // The source contribution and the normalized source voltage both
    // multiply by TYPE; their signs cancel in the physical node Jacobian.
    output = add_endpoints(output, Node::Bi, Node::Ei, value.d_vbiei);
    add_endpoints(output, Node::Bi, Node::Ci, value.d_vbici)
}

fn substrate(sign: f64, value: substrate::Value2) -> Quantity {
    let mut output = Quantity {
        value: sign * value.value,
        ..Quantity::default()
    };
    output = add_endpoints(output, Node::Bp, Node::Ci, value.d_vbpci);
    add_endpoints(output, Node::Si, Node::Ci, value.d_vsici)
}

fn base_current(sign: f64, output: internal_base::Output) -> Quantity {
    let mut current = Quantity {
        value: output.resistor_current.value,
        ..Quantity::default()
    };
    current = add_endpoints(current, Node::Bp, Node::Bi, output.branch_conductance);
    current = add_endpoints(
        current,
        Node::Bi,
        Node::Ei,
        sign * output.resistor_current.d_vbiei,
    );
    add_endpoints(
        current,
        Node::Bi,
        Node::Ci,
        sign * output.resistor_current.d_vbici,
    )
}

fn base_charge(sign: f64, output: internal_base::Output) -> Quantity {
    let mut charge = Quantity {
        value: output.hf_shunt_charge.value,
        ..Quantity::default()
    };
    charge = add_endpoints(charge, Node::Bp, Node::Bi, output.hf_branch_capacitance);
    charge = add_endpoints(
        charge,
        Node::Bi,
        Node::Ei,
        sign * output.hf_shunt_charge.d_vbiei,
    );
    add_endpoints(
        charge,
        Node::Bi,
        Node::Ci,
        sign * output.hf_shunt_charge.d_vbici,
    )
}

fn tunnel_quantity(sign: f64, output: tunneling::Output) -> Quantity {
    let mut current = Quantity {
        value: -sign * output.amperes,
        ..Quantity::default()
    };
    // Source Ibet is subtracted from the selected B-E branch. The
    // two source voltage derivatives are independent of the chosen
    // injection branch, and TYPE cancels in physical node derivatives.
    current = add_endpoints(current, Node::Bi, Node::Ei, -output.d_vbiei);
    add_endpoints(current, Node::Bp, Node::Ei, -output.d_vbpei)
}

fn auxiliary(residual: nqs::Residual, charge: nqs::DynamicCharge, sign: f64) -> Auxiliary {
    let mut current = Quantity {
        value: residual.current.value,
        ..Quantity::default()
    };
    // Auxiliary states are not multiplied by TYPE, unlike physical
    // current branches. Their source-voltage derivatives must retain it.
    current = add_endpoints(current, Node::Bi, Node::Ei, sign * residual.current.d_vbiei);
    current = add_endpoints(current, Node::Bi, Node::Ci, sign * residual.current.d_vbici);
    current.gradient.states = residual.d_state;
    current.gradient.d_tf = residual.d_tf;
    Auxiliary {
        current,
        charge: Quantity {
            value: charge.coulombs,
            gradient: Gradient {
                states: charge.d_state,
                ..Gradient::default()
            },
        },
    }
}

// Return a quantity with every local derivative scaled, not just its value.
impl Quantity {
    fn scale(mut self, factor: f64) -> Self {
        self.value *= factor;
        for node in Node::ALL {
            self.gradient.nodes[node.index()] *= factor;
        }
        self.gradient.states.current_first *= factor;
        self.gradient.states.current_second *= factor;
        self.gradient.states.stored_charge *= factor;
        self.gradient.d_tf *= factor;
        self
    }
}

fn physical_voltage(sign: f64, voltage: f64, from: Node, to: Node) -> Quantity {
    add_endpoints(
        Quantity {
            value: voltage,
            ..Quantity::default()
        },
        from,
        to,
        sign,
    )
}

// This is the model's internally normalized current. A source physical
// branch uses TYPE * I(model), but the published power law uses I(model).
fn source_intrinsic(sign: f64, value: transport::Value2) -> Quantity {
    intrinsic(sign, value).scale(sign)
}

fn source_diode(sign: f64, branch: Diode, from: Node, to: Node) -> Quantity {
    diode(sign, branch, from, to).scale(sign)
}

fn product_gradient(voltage: Quantity, current: Quantity, term: thermal::ProductPower) -> Quantity {
    voltage.scale(term.d_voltage) + current.scale(term.d_current)
}

fn resistor_power_gradient(
    voltage: Quantity,
    resistance_gradient: Quantity,
    term: thermal::ResistorPower,
) -> Quantity {
    voltage.scale(term.d_voltage) + resistance_gradient.scale(term.d_resistance)
}

// The intrinsic base resistance is source Rbi(Vbiei,Vbici).
// Its local gradient is *not* the branch current gradient. Source RBI
// also has a separate raw Vbpbi drive, and the power chain needs dR/dV.
fn internal_resistance_gradient(sign: f64, b: internal_base::Output) -> Quantity {
    let mut result = Quantity {
        value: b.resistance.value,
        ..Quantity::default()
    };
    result = add_endpoints(result, Node::Bi, Node::Ei, sign * b.resistance.d_vbiei);
    add_endpoints(result, Node::Bi, Node::Ci, sign * b.resistance.d_vbici)
}

fn thermal_product_terms(sign: f64, op: &device::Output) -> Quantity {
    let p = op.thermal.electrical;
    let v = op.voltage;
    let d = op.diode;
    let mut total = product_gradient(
        physical_voltage(sign, v.vciei, Node::Ci, Node::Ei),
        source_intrinsic(sign, op.transfer.net_transfer_current),
        p.transport,
    );
    let avalanche_headroom = p.avalanche.d_current;
    total = total
        + product_gradient(
            physical_voltage(-sign, avalanche_headroom, Node::Bi, Node::Ci),
            source_intrinsic(sign, op.avalanche.generation),
            p.avalanche,
        );
    for (voltage, current, term, from, to) in [
        (v.vbiei, d.ibei, p.intrinsic_emitter, Node::Bi, Node::Ei),
        (v.vbici, d.ibci, p.intrinsic_collector, Node::Bi, Node::Ci),
        (v.vbpei, d.ibep, p.peripheral_emitter, Node::Bp, Node::Ei),
        (v.vbpci, d.ijbcx, p.peripheral_collector, Node::Bp, Node::Ci),
        (v.vsici, d.ijsc, p.substrate, Node::Si, Node::Ci),
    ] {
        total = total
            + product_gradient(
                physical_voltage(sign, voltage, from, to),
                source_diode(sign, current, from, to),
                term,
            );
    }
    total
}

fn thermal_resistor_terms(sign: f64, op: &device::Output) -> Quantity {
    let p = op.thermal.electrical;
    let n = op.nodes;
    let raw = |voltage, from, to| physical_voltage(1.0, voltage, from, to);
    let zero = Quantity::default();
    let mut total = resistor_power_gradient(
        raw(n.bp - n.bi, Node::Bp, Node::Bi),
        internal_resistance_gradient(sign, op.internal_base),
        p.intrinsic_base,
    );
    // RBX/RCX/RE resistances depend on the prepared temperature,
    // but not on their electrical branch voltages at fixed Tdev.
    total = total
        + resistor_power_gradient(raw(n.ei - n.e, Node::Ei, Node::E), zero, p.external_emitter);
    total = total
        + resistor_power_gradient(
            raw(n.c - n.ci, Node::C, Node::Ci),
            zero,
            p.external_collector,
        );
    total + resistor_power_gradient(raw(n.b - n.bp, Node::B, Node::Bp), zero, p.external_base)
}

fn thermal_row(sign: f64, op: &device::Output) -> ThermalStamp {
    let heat = op.thermal;
    let power_gradient = thermal_product_terms(sign, op) + thermal_resistor_terms(sign, op);
    ThermalStamp {
        current: Quantity {
            value: heat.thermal_current,
            gradient: power_gradient.scale(-1.0).gradient,
        },
        charge: Quantity {
            value: heat.heat_charge,
            ..Quantity::default()
        },
        d_rise_at_fixed_electrical: heat.d_thermal_current_d_rise_at_fixed_electrical,
        heat_capacitance: heat.heat_capacitance,
    }
}
