//! Allocation-free source-to-reduced-node KCL and charge Jacobian assembly.
//!
//! This module owns physical branch incidence only. The solver owns global
//! matrix allocation, transient charge history and external noise injection.
//! Two physical chain rules (bias-dependent Tf and device temperature)
//! remain explicit until the caller supplies their derivatives.

use super::{
    Polarity, device, stamps,
    topology::{self, Branch, Node},
};
use crate::EvaluationError;

pub const MAX_UNKNOWN: usize = Node::COUNT + 3 + 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    topology: topology::Prepared,
    count: usize,
    nqs_start: usize,
    thermal_slot: Option<usize>,
}

impl Layout {
    #[must_use]
    pub const fn new(topology: topology::Prepared) -> Self {
        let nqs_start = topology.count();
        let after_nqs = nqs_start + topology.nqs_state_count();
        let thermal_slot = if topology.thermal_shorted() {
            None
        } else {
            Some(after_nqs)
        };
        let count = if thermal_slot.is_some() {
            after_nqs + 1
        } else {
            after_nqs
        };
        Self {
            topology,
            count,
            nqs_start,
            thermal_slot,
        }
    }

    #[must_use]
    pub const fn count(self) -> usize {
        self.count
    }

    #[must_use]
    pub const fn electrical_count(self) -> usize {
        self.nqs_start
    }

    #[must_use]
    pub const fn node(self, name: Node) -> usize {
        self.topology.index(name) as usize
    }

    #[must_use]
    pub const fn nqs_slot(self, state: usize) -> Option<usize> {
        if state < self.topology.nqs_state_count() {
            Some(self.nqs_start + state)
        } else {
            None
        }
    }

    /// None means the thermal port is an exact source ground constraint.
    #[must_use]
    pub const fn thermal_slot(self) -> Option<usize> {
        self.thermal_slot
    }

    #[must_use]
    pub const fn topology(self) -> topology::Prepared {
        self.topology
    }

    /// Reduce all source branches with the same signs as their Verilog-A
    /// incidences. The physical solver owns the global node mapping.
    ///
    /// # Errors
    /// Rejects active state columns without source NQS slots,
    /// inconsistent nominal shorts and finite-precision overflow.
    pub fn reduce(self, source: &stamps::Output) -> Result<Partials, EvaluationError> {
        let mut out = Partials::new(self);
        for branch in Branch::ALL {
            let data = source.branch(branch);
            if let Some(resistor) = branch.resistor()
                && !self.topology.present(resistor)
                && (data.current.value != 0.0 || data.charge.value != 0.0)
            {
                return Err(EvaluationError::InvalidPhysicalDomain);
            }
            let (from, to) = self.topology.ends(branch);
            if from == to {
                continue;
            }
            out.add_source(usize::from(from), data.current, data.charge, 1.0)?;
            out.add_source(usize::from(to), data.current, data.charge, -1.0)?;
        }
        if self.topology.nqs_state_count() == 3 {
            for (index, auxiliary) in source.auxiliary.iter().enumerate() {
                out.add_source(
                    self.nqs_start + index,
                    auxiliary.current,
                    auxiliary.charge,
                    1.0,
                )?;
            }
        }
        if let Some(row) = self.thermal_slot {
            out.add_source(row, source.thermal.current, source.thermal.charge, 1.0)?;
            out.conductance[row][row] += source.thermal.d_rise_at_fixed_electrical;
            out.capacitance[row][row] += source.thermal.heat_capacitance;
        }
        if !out.is_finite() {
            return Err(EvaluationError::NonFiniteResult);
        }
        Ok(out)
    }
}

/// Physical nodal partials at fixed Tf and fixed effective temperature.
/// Complete the rank-one Tf and electrothermal columns before solving.
#[derive(Clone, Copy, Debug)]
pub struct Partials {
    pub layout: Layout,
    pub current: [f64; MAX_UNKNOWN],
    pub charge: [f64; MAX_UNKNOWN],
    pub conductance: [[f64; MAX_UNKNOWN]; MAX_UNKNOWN],
    pub capacitance: [[f64; MAX_UNKNOWN]; MAX_UNKNOWN],
    pub d_current_d_tf: [f64; MAX_UNKNOWN],
    pub d_charge_d_tf: [f64; MAX_UNKNOWN],
}

impl Partials {
    const fn new(layout: Layout) -> Self {
        Self {
            layout,
            current: [0.0; MAX_UNKNOWN],
            charge: [0.0; MAX_UNKNOWN],
            conductance: [[0.0; MAX_UNKNOWN]; MAX_UNKNOWN],
            capacitance: [[0.0; MAX_UNKNOWN]; MAX_UNKNOWN],
            d_current_d_tf: [0.0; MAX_UNKNOWN],
            d_charge_d_tf: [0.0; MAX_UNKNOWN],
        }
    }

    fn add_source(
        &mut self,
        row: usize,
        current: stamps::Quantity,
        charge: stamps::Quantity,
        sign: f64,
    ) -> Result<(), EvaluationError> {
        self.current[row] = sign.mul_add(current.value, self.current[row]);
        self.charge[row] = sign.mul_add(charge.value, self.charge[row]);
        self.d_current_d_tf[row] = sign.mul_add(current.gradient.d_tf, self.d_current_d_tf[row]);
        self.d_charge_d_tf[row] = sign.mul_add(charge.gradient.d_tf, self.d_charge_d_tf[row]);
        self.add_gradient(row, current.gradient, sign, true)?;
        self.add_gradient(row, charge.gradient, sign, false)
    }

    fn add_gradient(
        &mut self,
        row: usize,
        gradient: stamps::Gradient,
        sign: f64,
        dc: bool,
    ) -> Result<(), EvaluationError> {
        let matrix = if dc {
            &mut self.conductance
        } else {
            &mut self.capacitance
        };
        for node in Node::ALL {
            let col = self.layout.node(node);
            matrix[row][col] = sign.mul_add(gradient.nodes[node.index()], matrix[row][col]);
        }
        let states = [
            gradient.states.current_first,
            gradient.states.current_second,
            gradient.states.stored_charge,
        ];
        for (position, value) in states.into_iter().enumerate() {
            if let Some(col) = self.layout.nqs_slot(position) {
                matrix[row][col] = sign.mul_add(value, matrix[row][col]);
            } else if value != 0.0 {
                return Err(EvaluationError::InvalidPhysicalDomain);
            }
        }
        Ok(())
    }

    fn is_finite(&self) -> bool {
        let n = self.layout.count();
        [
            &self.current,
            &self.charge,
            &self.d_current_d_tf,
            &self.d_charge_d_tf,
        ]
        .iter()
        .all(|x| x[..n].iter().all(|v| v.is_finite()))
            && [&self.conductance, &self.capacitance].iter().all(|matrix| {
                matrix[..n]
                    .iter()
                    .all(|row| row[..n].iter().all(|v| v.is_finite()))
            })
    }

    /// Complete with the exact HICCR forward-transit-time derivative
    /// carried by the actual source physical operating point. This
    /// automatically handles TYPE NPN/PNP, intrinsic BE/BC polarity and
    /// cross-couplings, without caller-provided numerical differences.
    ///
    /// The solver still supplies thermal derivatives for a live
    /// electrothermal node; the isothermal path is fully complete.
    ///
    /// # Errors
    /// Reports missing electrothermal coupling or overflow.
    pub fn complete_from_operating(
        self,
        operating: &device::Output,
        temperature: Option<TemperatureCoupling>,
    ) -> Result<Complete, EvaluationError> {
        let gradient = Self::transit_time_gradient(
            operating.polarity,
            operating.transfer.forward_transit_time_gradient,
        );
        self.complete(Some(gradient), temperature)
    }

    /// Exact normalized HICCR Tf voltage gradient -> physical named
    /// source-node gradient. The resistor-raw voltage axes are not
    /// polarity multiplied; only the intrinsic BE and BC contribute.
    #[must_use]
    pub fn transit_time_gradient(polarity: Polarity, intrinsic: [f64; 2]) -> [f64; Node::COUNT] {
        let sign = if matches!(polarity, Polarity::Npn) {
            1.0
        } else {
            -1.0
        };
        let mut gradient = [0.0; Node::COUNT];
        gradient[Node::Bi.index()] = sign * (intrinsic[0] + intrinsic[1]);
        gradient[Node::Ei.index()] = -sign * intrinsic[0];
        gradient[Node::Ci.index()] = -sign * intrinsic[1];
        gradient
    }

    /// Supply exact dTf/d(named node) and full electrothermal coupling.
    ///
    /// The caller must supply the electrical rows' fixed-Tf thermal
    /// derivatives and the thermal row's electrical-power temperature
    /// derivative separately; otherwise this would not be a complete
    /// Newton matrix. The thermal kernel already owns the temperature
    /// dependence of cooling through Rth(Tdev).
    ///
    /// # Errors
    /// Missing required coupling, nonfinite input or summed overflow.
    pub fn complete(
        mut self,
        d_tf_d_nodes: Option<[f64; Node::COUNT]>,
        temperature: Option<TemperatureCoupling>,
    ) -> Result<Complete, EvaluationError> {
        let n = self.layout.count();
        let needs_tf =
            (0..n).any(|row| self.d_current_d_tf[row] != 0.0 || self.d_charge_d_tf[row] != 0.0);
        if needs_tf && d_tf_d_nodes.is_none() {
            return Err(EvaluationError::MissingCoupling("transit_time"));
        }
        if self.layout.thermal_slot().is_some() && temperature.is_none() {
            return Err(EvaluationError::MissingCoupling("electrothermal"));
        }
        if let Some(gradient) = d_tf_d_nodes {
            if !gradient.iter().all(|v| v.is_finite()) {
                return Err(EvaluationError::NonFiniteBias);
            }
            for node in Node::ALL {
                let col = self.layout.node(node);
                for row in 0..n {
                    self.conductance[row][col] = self.d_current_d_tf[row]
                        .mul_add(gradient[node.index()], self.conductance[row][col]);
                    self.capacitance[row][col] = self.d_charge_d_tf[row]
                        .mul_add(gradient[node.index()], self.capacitance[row][col]);
                }
            }
        }
        if let Some(thermal) = temperature {
            if let Some(col) = self.layout.thermal_slot() {
                if !thermal.d_tf_d_rise.is_finite()
                    || !thermal.d_thermal_current_d_rise_from_power.is_finite()
                    || !thermal.current[..col].iter().all(|v| v.is_finite())
                    || !thermal.charge[..col].iter().all(|v| v.is_finite())
                {
                    return Err(EvaluationError::NonFiniteBias);
                }
                for row in 0..col {
                    self.conductance[row][col] +=
                        self.d_current_d_tf[row].mul_add(thermal.d_tf_d_rise, thermal.current[row]);
                    self.capacitance[row][col] +=
                        self.d_charge_d_tf[row].mul_add(thermal.d_tf_d_rise, thermal.charge[row]);
                }
                self.conductance[col][col] += thermal.d_thermal_current_d_rise_from_power;
            } else {
                return Err(EvaluationError::InvalidPhysicalDomain);
            }
        }
        if !self.is_finite() {
            return Err(EvaluationError::NonFiniteResult);
        }
        Ok(Complete {
            layout: self.layout,
            current: self.current,
            charge: self.charge,
            conductance: self.conductance,
            capacitance: self.capacitance,
        })
    }
}

/// Complete electrothermal source chain-rule data for a single thermal trial.
///
/// The input electrical row vectors contain their temperature derivatives
/// at fixed Tf. The thermal row scalar contains the extra power-law
/// derivative missing from the cooling-only Rth prepared derivative.
#[derive(Clone, Copy, Debug)]
pub struct TemperatureCoupling {
    pub current: [f64; MAX_UNKNOWN],
    pub charge: [f64; MAX_UNKNOWN],
    pub d_tf_d_rise: f64,
    /// d(-P_electrical)/dRise, excluding the fixed-electrical d(Rise/Rth)/dRise
    /// already stamped by the prepared thermal kernel.
    pub d_thermal_current_d_rise_from_power: f64,
}

/// Complete physical local DC and charge Jacobians, ready for global
/// sparse insertion and solver-owned AC/transient time integration.
#[derive(Clone, Copy, Debug)]
pub struct Complete {
    pub layout: Layout,
    pub current: [f64; MAX_UNKNOWN],
    pub charge: [f64; MAX_UNKNOWN],
    pub conductance: [[f64; MAX_UNKNOWN]; MAX_UNKNOWN],
    pub capacitance: [[f64; MAX_UNKNOWN]; MAX_UNKNOWN],
}
