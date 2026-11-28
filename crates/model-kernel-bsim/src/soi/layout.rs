use model_compact_intrinsic::{BsimSoiNode, BsimSoiTopology};
use model_kernel_api::{Binding, Equation, EquationKind, Error, Result};

pub(super) const MAX_ROWS: usize = 15;

/// Physical nodes resolve to a single coordinate when aliased. Ground in a
/// BSIMSOI topology means an inactive optional physical branch, not a local row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Layout {
    pub(super) width: usize,
    terminals: u8,
    external_thermal: Option<u8>,
    hidden: [Option<usize>; 8],
    pub(super) drain: usize,
    pub(super) source: usize,
    pub(super) gate: usize,
    pub(super) overlap_gate: usize,
    pub(super) body: Option<usize>,
    pub(super) contact: Option<usize>,
    pub(super) drain_body: Option<usize>,
    pub(super) source_body: Option<usize>,
    pub(super) thermal: Option<usize>,
    pub(super) gate_mid: Option<usize>,
}

const HIDDEN: [(BsimSoiNode, &str); 8] = [
    (BsimSoiNode::DrainPrime, "drain_prime"),
    (BsimSoiNode::SourcePrime, "source_prime"),
    (BsimSoiNode::Gate, "gate_prime"),
    (BsimSoiNode::GateMid, "gate_mid"),
    (BsimSoiNode::Body, "body_internal"),
    (BsimSoiNode::DrainBody, "drain_body"),
    (BsimSoiNode::SourceBody, "source_body"),
    (BsimSoiNode::Temperature, "temperature_internal"),
];

impl Layout {
    pub(super) fn new(
        topology: BsimSoiTopology,
        terminals: u8,
        external_thermal: Option<u8>,
    ) -> Result<Self> {
        let nodes = [
            topology.drain_prime,
            topology.source_prime,
            topology.gate,
            topology.gate_mid,
            topology.body,
            topology.contact,
            topology.drain_body,
            topology.source_body,
            topology.temperature,
        ];
        let mut width = usize::from(terminals);
        let hidden = HIDDEN.map(|(node, _)| {
            nodes.contains(&node).then(|| {
                let row = width;
                width += 1;
                row
            })
        });
        if width > MAX_ROWS {
            return Err(Error::Shape("BSIMSOI row count"));
        }
        let row = |node| -> Result<Option<usize>> {
            match node {
                BsimSoiNode::Ground => Ok(None),
                BsimSoiNode::External(index) if index < terminals => Ok(Some(usize::from(index))),
                BsimSoiNode::External(_) => Err(Error::Shape("BSIMSOI terminal index")),
                _ => HIDDEN
                    .iter()
                    .position(|(role, _)| *role == node)
                    .and_then(|index| hidden[index])
                    .map(Some)
                    .ok_or(Error::Shape("BSIMSOI local node")),
            }
        };
        let required = |node| row(node)?.ok_or(Error::Shape("missing BSIMSOI channel node"));
        let gate = required(topology.gate)?;
        let gate_mid = if topology.gate_mid == BsimSoiNode::GateMid {
            row(topology.gate_mid)?
        } else {
            None
        };
        Ok(Self {
            width,
            terminals,
            external_thermal,
            hidden,
            drain: required(topology.drain_prime)?,
            source: required(topology.source_prime)?,
            gate,
            overlap_gate: gate_mid.unwrap_or(gate),
            body: row(topology.body)?,
            contact: row(topology.contact)?,
            drain_body: row(topology.drain_body)?,
            source_body: row(topology.source_body)?,
            thermal: row(topology.temperature)?,
            gate_mid,
        })
    }

    pub(super) fn equations(self) -> Vec<Equation> {
        let mut rows = Vec::with_capacity(self.width);
        for index in 0..self.terminals {
            let thermal = self.external_thermal == Some(index);
            let name = match index {
                0 => "drain",
                1 => "gate",
                2 => "source",
                3 => "backgate",
                _ if thermal => "temperature",
                4 => "contact",
                _ => "body",
            };
            rows.push(Equation {
                name: name.into(),
                binding: Binding::Terminal,
                kind: if thermal {
                    EquationKind::Thermal
                } else {
                    EquationKind::Electrical
                },
            });
        }
        for (index, (node, name)) in HIDDEN.into_iter().enumerate() {
            if self.hidden[index].is_some() {
                rows.push(Equation {
                    name: name.into(),
                    binding: Binding::Hidden,
                    kind: if node == BsimSoiNode::Temperature {
                        EquationKind::Thermal
                    } else {
                        EquationKind::Electrical
                    },
                });
            }
        }
        rows
    }
}
