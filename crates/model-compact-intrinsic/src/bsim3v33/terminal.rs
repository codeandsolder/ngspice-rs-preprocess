//! Conservative intrinsic terminal equations in physical D/G/S/B order.

use super::{Bias, PreparedInstance, Result};

/// Static currents, stored charges and their voltage Jacobians.
///
/// All values include instance multiplicity. Rows and columns use the physical
/// intrinsic drain, gate, source, bulk order, regardless of conduction mode.
#[derive(Clone, Debug, PartialEq)]
pub struct TerminalPoint {
    pub current: [f64; 4],
    pub charge: [f64; 4],
    pub conductance: [[f64; 4]; 4],
    pub capacitance: [[f64; 4]; 4],
}

fn branch(
    rows: &mut [f64; 4],
    matrix: &mut [[f64; 4]; 4],
    a: usize,
    b: usize,
    value: f64,
    gradient: [f64; 4],
) {
    rows[a] += value;
    rows[b] -= value;
    for (column, derivative) in gradient.into_iter().enumerate() {
        matrix[a][column] += derivative;
        matrix[b][column] -= derivative;
    }
}

fn oriented_gradient([high, low]: [usize; 2], drain: f64, gate: f64, bulk: f64) -> [f64; 4] {
    let mut result = [0.; 4];
    result[high] = drain;
    result[1] = gate;
    result[3] = bulk;
    result[low] = -(drain + gate + bulk);
    result
}

impl PreparedInstance {
    /// Assemble the complete intrinsic terminal residual and charge.
    ///
    /// Coordinates are physical volts, not polarity-normalized. Series
    /// resistance branches are external to these four intrinsic terminals.
    ///
    /// # Errors
    /// Nonphysical inputs, unsupported constitutive mode or nonfinite result.
    pub fn terminal_point(&self, voltage: [f64; 4], gmin: f64) -> Result<TerminalPoint> {
        let [drain, gate, source, bulk] = voltage;
        let sign = f64::from(self.setup().polarity);
        let m = self.setup().multiplier;
        let bias = Bias {
            vds: sign * (drain - source),
            vgs: sign * (gate - source),
            vbs: sign * (bulk - source),
        };
        let point = self.evaluate(bias, gmin)?;
        let d = &point.dc;
        let c = &point.charge;
        let [high, low] = if d.mode >= 0 { [0, 2] } else { [2, 0] };
        let mut result = TerminalPoint {
            current: [0.; 4],
            charge: [0.; 4],
            conductance: [[0.; 4]; 4],
            capacitance: [[0.; 4]; 4],
        };
        for (end, current, derivative) in [
            (low, d.ids, [d.gds, d.gm, d.gmbs]),
            (3, d.isub, [d.gbds, d.gbgs, d.gbbs]),
        ] {
            branch(
                &mut result.current,
                &mut result.conductance,
                high,
                end,
                sign * m * current,
                oriented_gradient([high, low], derivative[0], derivative[1], derivative[2])
                    .map(|v| m * v),
            );
        }
        for (terminal, current, conductance) in [(0, d.cbd, d.gbd), (2, d.cbs, d.gbs)] {
            let mut gradient = [0.; 4];
            gradient[3] = m * conductance;
            gradient[terminal] = -gradient[3];
            branch(
                &mut result.current,
                &mut result.conductance,
                3,
                terminal,
                sign * m * current,
                gradient,
            );
        }
        let overlap = c.with_overlap(d.mode, bias.vgs - bias.vbs);
        result.charge = [
            overlap.qd,
            overlap.qg,
            -(overlap.qd + overlap.qg + overlap.qb),
            overlap.qb,
        ]
        .map(|q| sign * m * q);
        // Native channel derivatives are oriented to the conducting drain;
        // derive the fourth column and fourth row from conservation, then
        // permute the two channel terminals together for reverse mode.
        for (row, gate, drain, source) in [
            (1, c.cggb, c.cgdb, c.cgsb),
            (high, c.cdgb, c.cddb, c.cdsb),
            (3, c.cbgb, c.cbdb, c.cbsb),
        ] {
            let mut gradient = [0.; 4];
            gradient[1] = m * gate;
            gradient[high] = m * drain;
            gradient[low] = m * source;
            gradient[3] = -(gradient[1] + gradient[high] + gradient[low]);
            result.capacitance[row] = gradient;
        }
        for column in 0..4 {
            result.capacitance[low][column] = -(result.capacitance[high][column]
                + result.capacitance[1][column]
                + result.capacitance[3][column]);
        }
        for (a, b, capacitance) in [
            (1, 0, c.cgdo),
            (1, 2, c.cgso),
            (1, 3, c.cgbo),
            (3, 0, c.capbd),
            (3, 2, c.capbs),
        ] {
            let derivative = m * capacitance;
            result.capacitance[a][a] += derivative;
            result.capacitance[b][b] += derivative;
            result.capacitance[a][b] -= derivative;
            result.capacitance[b][a] -= derivative;
        }
        if result
            .current
            .iter()
            .chain(&result.charge)
            .chain(result.conductance.iter().flatten())
            .chain(result.capacitance.iter().flatten())
            .any(|x| !x.is_finite())
        {
            return Err(super::Error::Domain(
                "nonfinite physical terminal residual or Jacobian".into(),
            ));
        }
        Ok(result)
    }
}
