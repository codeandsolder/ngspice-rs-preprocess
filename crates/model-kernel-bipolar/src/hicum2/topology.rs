//! HICUM/L2 2.4.0 physical node topology, before circuit solver binding.
//!
//! The source's five physical resistors are *ideal voltage constraints*
//! whenever their **nominal** value is below 1 mOhm. Reduce these once;
//! never introduce enormous conductances to approximate absent resistors.
//! The four electrical terminals always map to indices 0..4 in C/B/E/S
//! order. The other five nodes receive stable consecutive indices only
//! when their source resistance makes them distinct.
//!
//! The source-prescribed branch incidences are metadata for the simulator
//! to assemble its nodal Jacobian and charge stamps; the solver still owns
//! matrix representation, branch integration and global unknown allocation.

use super::{temperature, thermal::MIN_SOURCE_RESISTANCE};
use crate::{ModelError, validate_nonnegative};

/// HICUM source nodes. Numeric order retains the four external terminals
/// first, then the five hidden source nodes (ci/ei/bp/bi/si).
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Node {
    C = 0,
    B = 1,
    E = 2,
    S = 3,
    Ci = 4,
    Ei = 5,
    Bp = 6,
    Bi = 7,
    Si = 8,
}

impl Node {
    pub const COUNT: usize = 9;
    pub const ALL: [Self; Self::COUNT] = [
        Self::C,
        Self::B,
        Self::E,
        Self::S,
        Self::Ci,
        Self::Ei,
        Self::Bp,
        Self::Bi,
        Self::Si,
    ];
    pub const TERMINALS: [Self; 4] = [Self::C, Self::B, Self::E, Self::S];

    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }
}

/// The source has five independent nominally gated physical resistors.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resistor {
    Rbx = 0,
    Rbi = 1,
    Rcx = 2,
    Re = 3,
    Rsu = 4,
}

impl Resistor {
    pub const ALL: [Self; 5] = [Self::Rbx, Self::Rbi, Self::Rcx, Self::Re, Self::Rsu];

    #[must_use]
    pub const fn nodes(self) -> (Node, Node) {
        match self {
            Self::Rbx => (Node::B, Node::Bp),
            Self::Rbi => (Node::Bp, Node::Bi),
            Self::Rcx => (Node::Ci, Node::C),
            Self::Re => (Node::Ei, Node::E),
            Self::Rsu => (Node::Si, Node::S),
        }
    }

    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }
}

/// All physical two-terminal source branches. Distinct contribution
/// equations may share a node pair, e.g. B-C reverse current and
/// generated avalanche current, but retain their physical orientation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Branch {
    Rbx,
    Rbi,
    Rcx,
    Re,
    Rsu,
    IntrinsicEmitter,
    IntrinsicCollector,
    PeripheralEmitter,
    ExternalEmitterIsolation,
    PeripheralEmitterIsolation,
    ExternalCollectorJunction,
    PeripheralCollector,
    TransferForward,
    TransferReverse,
    SubstrateJunction,
    PeripheralSubstrateJunction,
    SubstrateTransfer,
    Avalanche,
}

impl Branch {
    pub const ALL: [Self; 18] = [
        Self::Rbx,
        Self::Rbi,
        Self::Rcx,
        Self::Re,
        Self::Rsu,
        Self::IntrinsicEmitter,
        Self::IntrinsicCollector,
        Self::PeripheralEmitter,
        Self::ExternalEmitterIsolation,
        Self::PeripheralEmitterIsolation,
        Self::ExternalCollectorJunction,
        Self::PeripheralCollector,
        Self::TransferForward,
        Self::TransferReverse,
        Self::SubstrateJunction,
        Self::PeripheralSubstrateJunction,
        Self::SubstrateTransfer,
        Self::Avalanche,
    ];
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// Published Verilog-A branch declaration, from positive current
    /// terminal to negative terminal.
    #[must_use]
    pub const fn nodes(self) -> (Node, Node) {
        match self {
            Self::Rbx => (Node::B, Node::Bp),
            Self::Rbi => (Node::Bp, Node::Bi),
            Self::Rcx => (Node::Ci, Node::C),
            Self::Re => (Node::Ei, Node::E),
            Self::Rsu => (Node::Si, Node::S),
            Self::IntrinsicEmitter => (Node::Bi, Node::Ei),
            Self::IntrinsicCollector => (Node::Bi, Node::Ci),
            Self::PeripheralEmitter => (Node::Bp, Node::Ei),
            Self::ExternalEmitterIsolation => (Node::B, Node::E),
            Self::PeripheralEmitterIsolation => (Node::Bp, Node::E),
            Self::ExternalCollectorJunction => (Node::B, Node::Ci),
            Self::PeripheralCollector => (Node::Bp, Node::Ci),
            Self::TransferForward => (Node::Ci, Node::Ei),
            Self::TransferReverse => (Node::Ei, Node::Ci),
            Self::SubstrateJunction => (Node::Si, Node::Ci),
            Self::PeripheralSubstrateJunction => (Node::S, Node::C),
            Self::SubstrateTransfer => (Node::Bp, Node::Si),
            Self::Avalanche => (Node::Ci, Node::Bi),
        }
    }

    #[must_use]
    pub const fn resistor(self) -> Option<Resistor> {
        match self {
            Self::Rbx => Some(Resistor::Rbx),
            Self::Rbi => Some(Resistor::Rbi),
            Self::Rcx => Some(Resistor::Rcx),
            Self::Re => Some(Resistor::Re),
            Self::Rsu => Some(Resistor::Rsu),
            _ => None,
        }
    }
}

/// Stable source topology, with a unique deterministic index per
/// non-shorted electrical node. All names that are ideal-shorted share
/// the exact same index; the four device pins keep independent indices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Prepared {
    index: [u8; Node::COUNT],
    node_count: u8,
    present: [bool; 5],
    thermal_shorted: bool,
    nqs_auxiliaries: bool,
}

impl Prepared {
    /// Construct a source-consistent immutable topology before model
    /// temperature preparation or any trial Newton voltages.
    ///
    /// # Errors
    /// Rejects invalid nominal resistance and thermal mode.
    pub fn new(p: &temperature::Nominal) -> Result<Self, ModelError> {
        let r = p.auxiliary;
        let nominal = [r.rbx, r.rbi0, r.rcx, r.re, r.rsu];
        let names = ["rbx", "rbi0", "rcx", "re", "rsu"];
        for (name, value) in names.into_iter().zip(nominal) {
            validate_nonnegative(name, value)?;
        }
        if r.flsh > 2 {
            return Err(ModelError::InvalidParameter("flsh"));
        }
        validate_nonnegative("rth", r.rth)?;
        let present = nominal.map(|r| r >= MIN_SOURCE_RESISTANCE);
        let mut parent = [0usize; Node::COUNT];
        for node in Node::ALL {
            parent[node.index()] = node.index();
        }
        for resistor in Resistor::ALL {
            if !present[resistor.index()] {
                let (a, b) = resistor.nodes();
                merge(&mut parent, a.index(), b.index());
            }
        }
        let mut index = [u8::MAX; Node::COUNT];
        // External terminal IDs are the layout's non-negotiable public
        // order; internal roots inherit a terminal ID if shorted to it.
        for terminal in Node::TERMINALS {
            index[root(&parent, terminal.index())] = terminal as u8;
        }
        let mut count = 4u8;
        for node in Node::ALL {
            let root_index = root(&parent, node.index());
            if index[root_index] == u8::MAX {
                index[root_index] = count;
                count += 1;
            }
        }
        for node in Node::ALL {
            index[node.index()] = index[root(&parent, node.index())];
        }
        // The source's physical NQS activation is bias-dependent (Tf=0 can
        // suppress it at an operating point). Reserve the auxiliary state
        // nodes for every model that can activate at any bias, not just
        // for the present Newton iterate.
        let legacy = p.transport.flcomp == 0.0 || p.transport.flcomp.to_bits() == 2.1f64.to_bits();
        // Tf can be generated by low-current transport time, its
        // voltage-dependent corrections, emitter transit, or collector
        // high-injection delay. If all five source time coefficients
        // vanish, HICQFF returns Tf=0 at every bias and NQS can never
        // activate. Do not allocate three permanently idle nodes.
        let possible_forward_delay = [
            p.transport.t0,
            p.transport.dt0h,
            p.transport.tbvl,
            p.transport.tef0,
            p.transport.thcs,
        ]
        .into_iter()
        .any(|value| value != 0.0);
        let nqs_auxiliaries = (r.flnqs || legacy)
            && (p.noise.alit > 0.0 || p.noise.alqf > 0.0)
            && possible_forward_delay;
        Ok(Self {
            index,
            node_count: count,
            present,
            thermal_shorted: r.flsh == 0 || r.rth < MIN_SOURCE_RESISTANCE,
            nqs_auxiliaries,
        })
    }

    #[must_use]
    pub const fn index(self, node: Node) -> u8 {
        self.index[node.index()]
    }

    #[must_use]
    pub const fn count(self) -> usize {
        self.node_count as usize
    }

    #[must_use]
    pub const fn present(self, resistor: Resistor) -> bool {
        self.present[resistor.index()]
    }

    #[must_use]
    pub const fn thermal_shorted(self) -> bool {
        self.thermal_shorted
    }

    /// The source has exactly three auxiliary NQS state nodes whenever
    /// FLNQS/FLcomp and ALIT/ALQF can enable them. Their trial values
    /// remain solver-owned and can be temporarily inactive at a bias.
    #[must_use]
    pub const fn nqs_state_count(self) -> usize {
        if self.nqs_auxiliaries { 3 } else { 0 }
    }

    /// Source-prescribed branch endpoints after all ideal resistor shorts.
    /// The owner simulator can stamp directly into its allocated node map.
    #[must_use]
    pub const fn ends(self, branch: Branch) -> (u8, u8) {
        let (a, b) = branch.nodes();
        (self.index(a), self.index(b))
    }
}

const fn root(parent: &[usize; Node::COUNT], mut index: usize) -> usize {
    while parent[index] != index {
        index = parent[index];
    }
    index
}

const fn merge(parent: &mut [usize; Node::COUNT], a: usize, b: usize) {
    let left = root(parent, a);
    let right = root(parent, b);
    if left != right {
        parent[right] = left;
    }
}

impl temperature::Nominal {
    /// Resolve the physical HICUM source short constraints once, before
    /// preparing temperature-dependent model kernels.
    ///
    /// # Errors
    /// Rejects an invalid nominal source resistance or self-heating mode.
    pub fn physical_topology(&self) -> Result<Prepared, ModelError> {
        Prepared::new(self)
    }
}
