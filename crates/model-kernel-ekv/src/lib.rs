//! EKV 2.6 revision 15 intrinsic MOS equations.
//!
//! Based on the EKV 2.6 mathematical model and the independently executable
//! Verilog-A reference (ekv26/model, revision 137e7779c66d282113ce5802843f546869cb8c05).
//! See NOTICE for attribution. Reimplemented with shared scalar arithmetic,
//! SI preparation and conservative terminal assembly. This is the intrinsic
//! channel only; source-specific parasitics and parameter derivation are separate.
#![forbid(unsafe_code)]

pub mod artifact;
mod channel;
mod parameters;

pub use parameters::{Geometry, Model, Polarity, Prepared};

use model_kernel_api::{
    Error, Result,
    taylor::arithmetic::{Scalar, Series},
};

/// Terminal order: drain, gate, source, bulk. Currents enter the device.
#[derive(Clone, Copy, Debug)]
pub struct Intrinsic<S = f64> {
    pub current: [S; 4],
    pub charge: [S; 4],
    pub channel_current: S,
    pub thermal_conductance: S,
}

/// Intrinsic residuals and analytic voltage Jacobians in terminal order.
#[derive(Clone, Copy, Debug)]
pub struct Linearization {
    pub point: Intrinsic,
    pub conductance: [[f64; 4]; 4],
    pub capacitance: [[f64; 4]; 4],
    pub channel_transconductance: f64,
}

impl Prepared {
    /// Evaluate a point or normalized local Taylor series.
    ///
    /// # Errors
    /// Nonfinite input or output, including an out-of-domain physical bias.
    pub fn evaluate<S: Scalar>(&self, voltage: [S; 4]) -> Result<Intrinsic<S>> {
        if voltage.iter().any(|v| !v.is_finite()) {
            return Err(Error::Domain("nonfinite EKV bias".into()));
        }
        let result = channel::evaluate(self, voltage);
        if result
            .current
            .into_iter()
            .chain(result.charge)
            .chain([result.channel_current, result.thermal_conductance])
            .all(Scalar::is_finite)
        {
            Ok(result)
        } else {
            Err(Error::Domain("nonfinite EKV intrinsic response".into()))
        }
    }

    /// Differentiate the very same constitutive law used for residuals.
    ///
    /// # Errors
    /// Invalid physical bias or nonfinite derivative.
    pub fn linearize(&self, voltage: [f64; 4]) -> Result<Linearization> {
        let point = self.evaluate(voltage)?;
        let mut conductance = [[0.; 4]; 4];
        let mut capacitance = [[0.; 4]; 4];
        let mut channel_transconductance = 0.;
        for column in 0..4 {
            let mut input = [Series::default(); 4];
            for (row, value) in voltage.into_iter().enumerate() {
                input[row] = Series::from_coefficients([value, f64::from(row == column)])?;
            }
            let derivative = self.evaluate(input)?;
            if column == 1 {
                channel_transconductance = derivative.channel_current.coefficient(1);
            }
            for row in 0..4 {
                conductance[row][column] = derivative.current[row].coefficient(1);
                capacitance[row][column] = derivative.charge[row].coefficient(1);
            }
        }
        Ok(Linearization {
            point,
            conductance,
            capacitance,
            channel_transconductance,
        })
    }
}
