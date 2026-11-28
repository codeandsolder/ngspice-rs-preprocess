//! VBIC 1.1.5 uncorrelated device-noise source densities.
//!
//! Source: ngspice vbicnoise.c / const.h. The output is source-referred
//! A^2/Hz; the circuit solver must apply the frequency-dependent transfer
//! gain for each branch and integrate across the spectrum itself.
//!
//! The native reference has seven thermal resistor sources, four shot-current
//! sources and two 1/f sources. Its total accumulator accidentally omits the
//! ICCP shot source despite calculating it; our physical total includes it.

use crate::{EvaluationError, ModelError, Temperature, evaluate_lanes, validate_nonnegative};

// These historical ngspice noise-analysis constants differ from VBIC's
// internal current/charge/thermal-voltage constants.
const BOLTZMANN_NOISE: f64 = 1.380_622_6e-23;
const ELEMENTARY_CHARGE_NOISE: f64 = 1.602_191_8e-19;
const N_MINLOG: f64 = 1e-38;

/// VBIC KFN, AFN and BFN noise parameters; no frequency-specific work here.
#[derive(Clone, Copy, Debug)]
pub struct Parameters {
    /// KFN coefficient in the native 1/f law.
    pub kfn: f64,
    /// AFN current dependence exponent.
    pub afn: f64,
    /// BFN frequency dependence exponent.
    pub bfn: f64,
}

/// Input resistor conductances, temperature-adjusted and scaled by model
/// geometry/multiplicity. All are intrinsic S (not nodal stamp coefficients).
#[derive(Clone, Copy, Debug, Default)]
pub struct Conductances {
    pub rc: f64,
    pub rci: f64,
    pub rb: f64,
    pub rbi: f64,
    pub re: f64,
    pub rbp: f64,
    pub rs: f64,
}

/// Already-scaled branch currents from the accepted or trial operating point.
#[derive(Clone, Copy, Debug, Default)]
pub struct Currents {
    /// Collector transfer current (delayed forward minus reverse).
    pub ic: f64,
    pub ibe: f64,
    pub ibep: f64,
    pub iccp: f64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct OperatingPoint {
    pub conductances: Conductances,
    pub currents: Currents,
}

/// The output entries are individual independent source spectra in A^2/Hz.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Thermal {
    pub rc: f64,
    pub rci: f64,
    pub rb: f64,
    pub rbi: f64,
    pub re: f64,
    pub rbp: f64,
    pub rs: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Shot {
    pub ic: f64,
    pub ibe: f64,
    pub ibep: f64,
    pub iccp: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Flicker {
    pub ibe: f64,
    pub ibep: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Output {
    pub thermal: Thermal,
    pub shot: Shot,
    pub flicker: Flicker,
    /// Unweighted sum of *all* local source PSDs; this is not output noise.
    /// Includes ICCP shot PSD, unlike the ngspice 47 total-source accumulator.
    pub total_source: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct Prepared {
    p: Parameters,
    four_kt: f64,
    twice_charge: f64,
    /// The VBIC instance M scaling; total branch current is already scaled.
    multiplicity: f64,
}

impl Prepared {
    /// # Errors
    /// Reports invalid noise model, temperature or multiplicity.
    pub fn new(
        parameters: Parameters,
        temperature: Temperature,
        multiplicity: f64,
    ) -> Result<Self, ModelError> {
        validate_nonnegative("kfn", parameters.kfn)?;
        if !parameters.afn.is_finite() {
            return Err(ModelError::InvalidParameter("afn"));
        }
        if !parameters.bfn.is_finite() {
            return Err(ModelError::InvalidParameter("bfn"));
        }
        if !multiplicity.is_finite() || multiplicity <= 0.0 {
            return Err(ModelError::InvalidParameter("m"));
        }
        let four_kt = 4.0 * BOLTZMANN_NOISE * temperature.kelvin();
        let twice_charge = 2.0 * ELEMENTARY_CHARGE_NOISE;
        if !four_kt.is_finite() {
            return Err(ModelError::InvalidTemperature);
        }
        Ok(Self {
            p: parameters,
            four_kt,
            twice_charge,
            multiplicity,
        })
    }

    /// Evaluate local device-noise source spectra at the supplied frequency.
    /// Circuit gain, correlated sources and output integration belong to the
    /// simulator; this method intentionally computes none of those.
    ///
    /// # Errors
    /// Returns invalid physical domain for zero/negative frequency, invalid
    /// operating point for negative conductance, or numerical overflow.
    pub fn evaluate(
        &self,
        operating: OperatingPoint,
        frequency_hz: f64,
    ) -> Result<Output, EvaluationError> {
        if !frequency_hz.is_finite() || frequency_hz <= 0.0 {
            return Err(EvaluationError::InvalidPhysicalDomain);
        }
        let c = operating.conductances;
        let i = operating.currents;
        let conductances = [c.rc, c.rci, c.rb, c.rbi, c.re, c.rbp, c.rs];
        let currents = [i.ic, i.ibe, i.ibep, i.iccp];
        if !conductances
            .iter()
            .chain(currents.iter())
            .all(|value| value.is_finite())
        {
            return Err(EvaluationError::NonFiniteBias);
        }
        if conductances.iter().any(|value| *value < 0.0) {
            return Err(EvaluationError::InvalidPhysicalDomain);
        }

        let scale = self.four_kt;
        let thermal = Thermal {
            rc: scale * c.rc,
            rci: scale * c.rci,
            rb: scale * c.rb,
            rbi: scale * c.rbi,
            re: scale * c.re,
            rbp: scale * c.rbp,
            rs: scale * c.rs,
        };
        let shot = Shot {
            ic: self.twice_charge * i.ic.abs(),
            ibe: self.twice_charge * i.ibe.abs(),
            ibep: self.twice_charge * i.ibep.abs(),
            iccp: self.twice_charge * i.iccp.abs(),
        };
        let flicker = if self.p.kfn == 0.0 {
            Flicker::default()
        } else {
            let base = self.multiplicity * self.p.kfn / frequency_hz.powf(self.p.bfn);
            Flicker {
                ibe: base
                    * (i.ibe / self.multiplicity)
                        .abs()
                        .max(N_MINLOG)
                        .powf(self.p.afn),
                ibep: base
                    * (i.ibep / self.multiplicity)
                        .abs()
                        .max(N_MINLOG)
                        .powf(self.p.afn),
            }
        };
        let sum = [
            thermal.rc,
            thermal.rci,
            thermal.rb,
            thermal.rbi,
            thermal.re,
            thermal.rbp,
            thermal.rs,
            shot.ic,
            shot.ibe,
            shot.ibep,
            shot.iccp,
            flicker.ibe,
            flicker.ibep,
        ]
        .into_iter()
        .sum();
        let out = Output {
            thermal,
            shot,
            flicker,
            total_source: sum,
        };
        if ![
            thermal.rc,
            thermal.rci,
            thermal.rb,
            thermal.rbi,
            thermal.re,
            thermal.rbp,
            thermal.rs,
            shot.ic,
            shot.ibe,
            shot.ibep,
            shot.iccp,
            flicker.ibe,
            flicker.ibep,
            out.total_source,
        ]
        .iter()
        .all(|value| value.is_finite())
        {
            return Err(EvaluationError::NonFiniteResult);
        }
        Ok(out)
    }

    /// Evaluate a fixed-frequency batch without allocations.
    ///
    /// All slice lengths are checked before any writes. Lanes run in order;
    /// the first error leaves that lane and the remaining outputs unchanged.
    ///
    /// # Errors
    /// Returns mismatched buffer size or any invalid operating point.
    pub fn evaluate_batch(
        &self,
        operating: &[OperatingPoint],
        outputs: &mut [Output],
        frequency_hz: f64,
    ) -> Result<(), EvaluationError> {
        evaluate_lanes(operating.iter().copied(), outputs, |op| {
            self.evaluate(op, frequency_hz)
        })
    }
}
