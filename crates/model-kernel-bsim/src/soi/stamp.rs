use super::layout::{Layout, MAX_ROWS};
use model_compact_intrinsic::{
    BsimSoiChannelScalar, BsimSoiJunctionCharge, BsimSoiJunctionCurrent, BsimSoiSeriesConductance,
};
use model_kernel_api::{Error, Point, Result};
use std::ops::{Add, Mul, Sub};

/// Fixed-size physical derivative for all supported SOI coordinate layouts.
/// Each voltage is a difference of two resolved, possibly aliased, nodes.
#[derive(Clone, Copy, Debug)]
pub(super) struct Differential {
    pub(super) value: f64,
    gradient: [f64; MAX_ROWS],
}

impl Differential {
    pub(super) const fn constant(value: f64) -> Self {
        Self {
            value,
            gradient: [0.; MAX_ROWS],
        }
    }

    pub(super) fn voltage(coordinates: &[f64], positive: usize, negative: usize) -> Self {
        let mut point = Self::constant(coordinates[positive] - coordinates[negative]);
        point.gradient[positive] += 1.;
        point.gradient[negative] -= 1.;
        point
    }

    pub(super) fn temperature(coordinates: &[f64], row: Option<usize>, ambient: f64) -> Self {
        let mut point = Self::constant(row.map_or(ambient, |row| coordinates[row]));
        if let Some(row) = row {
            point.gradient[row] = 1.;
        }
        point
    }

    pub(super) fn lift(value: f64, derivatives: impl IntoIterator<Item = (Self, f64)>) -> Self {
        let mut point = Self::constant(value);
        for (axis, slope) in derivatives {
            for (entry, derivative) in point.gradient.iter_mut().zip(axis.gradient) {
                *entry = slope.mul_add(derivative, *entry);
            }
        }
        point
    }

    pub(super) fn channel(point: BsimSoiChannelScalar, axes: &[Self; 5]) -> Self {
        Self::lift(point.value, axes.iter().copied().zip(point.gradient))
    }

    pub(super) fn junction(
        point: BsimSoiJunctionCurrent,
        voltage: Self,
        temperature: Self,
    ) -> Self {
        Self::lift(
            point.current,
            [
                (voltage, point.conductance),
                (temperature, point.d_temperature),
            ],
        )
    }

    pub(super) fn junction_charge(
        point: BsimSoiJunctionCharge,
        voltage: Self,
        temperature: Self,
    ) -> Self {
        Self::lift(
            point.charge,
            [
                (voltage, point.capacitance),
                (temperature, point.d_temperature),
            ],
        )
    }

    pub(super) fn series(
        point: BsimSoiSeriesConductance,
        gate: Self,
        body: Self,
        temperature: Self,
    ) -> Self {
        Self::lift(
            point.conductance,
            [
                (gate, point.d_gate),
                (body, point.d_body),
                (temperature, point.d_temperature),
            ],
        )
    }
}

impl Add for Differential {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        Self {
            value: self.value + rhs.value,
            gradient: std::array::from_fn(|i| self.gradient[i] + rhs.gradient[i]),
        }
    }
}
impl Sub for Differential {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        Self {
            value: self.value - rhs.value,
            gradient: std::array::from_fn(|i| self.gradient[i] - rhs.gradient[i]),
        }
    }
}
impl Mul for Differential {
    type Output = Self;
    fn mul(self, rhs: Self) -> Self {
        Self {
            value: self.value * rhs.value,
            gradient: std::array::from_fn(|i| {
                self.value
                    .mul_add(rhs.gradient[i], rhs.value * self.gradient[i])
            }),
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct Biases {
    pub(super) axes: [Differential; 5],
    pub(super) contact: Differential,
    pub(super) junctions: [Differential; 2],
    pub(super) overlap_gate: Differential,
}

impl Biases {
    pub(super) fn new(layout: &Layout, x: &[f64], ambient: f64) -> Self {
        let relative = |row| Differential::voltage(x, row, layout.source);
        let body = layout.body.map_or(Differential::constant(0.), relative);
        let drain = relative(layout.drain);
        Self {
            axes: [
                relative(layout.gate),
                drain,
                body,
                relative(3),
                Differential::temperature(x, layout.thermal, ambient),
            ],
            contact: layout.contact.map_or(Differential::constant(0.), relative),
            junctions: [
                layout
                    .source_body
                    .map_or(Differential::constant(0.), relative),
                layout.drain_body.map_or(Differential::constant(0.), |row| {
                    Differential::voltage(x, row, layout.drain)
                }),
            ],
            overlap_gate: relative(layout.overlap_gate),
        }
    }

    pub(super) fn channel(&self) -> model_compact_intrinsic::BsimSoiChannelBias {
        let [gate, drain, body, backgate, kelvin] = self.axes.map(|axis| axis.value);
        model_compact_intrinsic::BsimSoiChannelBias {
            gate,
            drain,
            body,
            backgate,
            kelvin,
        }
    }
}

/// Shared conservative row/branch stamp for residuals and stored quantities.
pub(super) struct Stamp<'a> {
    values: &'a mut [f64],
    jacobian: &'a mut [f64],
}

impl<'a> Stamp<'a> {
    pub(super) const fn current(out: &'a mut Point<'_>) -> Self {
        Self {
            values: out.current,
            jacobian: out.conductance,
        }
    }
    pub(super) const fn charge(out: &'a mut Point<'_>) -> Self {
        Self {
            values: out.charge,
            jacobian: out.capacitance,
        }
    }

    pub(super) fn row(&mut self, row: usize, value: &Differential, scale: f64) {
        let n = self.values.len();
        self.values[row] = value.value.mul_add(scale, self.values[row]);
        for (entry, derivative) in self.jacobian[row * n..(row + 1) * n]
            .iter_mut()
            .zip(value.gradient)
        {
            *entry = derivative.mul_add(scale, *entry);
        }
    }

    pub(super) fn pair(
        &mut self,
        positive: Option<usize>,
        negative: Option<usize>,
        value: &Differential,
    ) -> Result<()> {
        if positive == negative {
            return Ok(());
        }
        match (positive, negative) {
            (Some(a), Some(b)) => {
                self.row(a, value, 1.);
                self.row(b, value, -1.);
                Ok(())
            }
            _ if value.value == 0. && value.gradient.iter().all(|&derivative| derivative == 0.) => {
                Ok(())
            }
            _ => Err(Error::Domain(
                "active BSIMSOI branch has no physical terminal".into(),
            )),
        }
    }
}
