//! HICUM/L2 2.4.0 local noise and correlated B-E/C-E shot spectra.
//!
//! Source: hicumL2V2p4p0.va local noise block. Emits individual local source
//! PSDs (A²/Hz), and a Hermitian B-E/C-E shot spectral covariance when
//! FLCONO is enabled. Nodal gain, integration and topology are solver-owned.

use crate::{
    EvaluationError, ModelError, Temperature, validate_batch_lengths, validate_nonnegative,
    validate_positive,
};

const SOURCE_K: f64 = 1.380_650_3e-23;
const SOURCE_Q: f64 = 1.602_176_462e-19;
const TWO_PI: f64 = 2.0 * std::f64::consts::PI;

/// CFBE: placement of the one shared B-E flicker source.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FlickerLocation {
    #[default]
    Intrinsic,
    Peripheral,
}

#[derive(Clone, Copy, Debug)]
pub struct Parameters {
    /// KF*|Ibei+Ibep|^AF/f.
    pub kf: f64,
    pub af: f64,
    pub flicker_location: FlickerLocation,
    /// Emitter-series flicker power from the resistor current.
    pub kfre: f64,
    pub afre: f64,
    /// FLCONO, independent of whether the transport NQS network is enabled.
    pub correlated: bool,
    pub alit: f64,
    pub alqf: f64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Conductances {
    /// Effective noise conductance of an *existing* resistor, or zero when
    /// the nominal resistor is absent/shorted in the device topology.
    pub rbx: f64,
    pub rbi: f64,
    pub rcx: f64,
    pub re: f64,
    pub rsu: f64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Currents {
    pub it: f64,
    pub ibei: f64,
    pub ibep: f64,
    pub ibci: f64,
    pub ijbcx: f64,
    pub ijsc: f64,
    /// Generated avalanche current must be nonnegative.
    pub iavl: f64,
    /// Current through RE, not the intrinsic emitter junction current.
    pub emitter_resistor: f64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct OperatingPoint {
    pub conductances: Conductances,
    pub currents: Currents,
    /// Source Tf (seconds), not T0 or stored minority charge.
    pub forward_transit_time: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Thermal {
    pub rbx: f64,
    pub rbi: f64,
    pub rcx: f64,
    pub re: f64,
    pub rsu: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Shot {
    pub iavl: f64,
    pub ibci: f64,
    pub ibep: f64,
    pub ijbcx: f64,
    pub ijsc: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Flicker {
    pub be: f64,
    pub emitter_resistor: f64,
}

/// Hermitian B-E/C-E shot-noise covariance, source-referred in A²/Hz.
/// Off-diagonal spectrum is purely imaginary with positive omega; its
/// conjugate occupies the transposed matrix element.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ShotCovariance {
    pub base: f64,
    pub collector: f64,
    pub cross_imaginary: f64,
    /// Independent base contribution, before coherent collector injection.
    pub base_independent: f64,
    /// Imaginary base-branch injection per unit collector noise current.
    pub collector_to_base_imaginary: f64,
    pub correlated: bool,
    pub n1_seconds: f64,
    pub n2_seconds: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Output {
    pub thermal: Thermal,
    pub shot: Shot,
    pub flicker: Flicker,
    pub flicker_location: FlickerLocation,
    pub be_ce_shot: ShotCovariance,
}

#[derive(Clone, Copy, Debug)]
pub struct Prepared {
    parameters: Parameters,
    four_kt: f64,
}

impl Prepared {
    /// # Errors
    /// Returns the offending nonphysical source coefficient or temperature.
    pub fn new(parameters: Parameters, temperature: Temperature) -> Result<Self, ModelError> {
        for (name, value) in [("kf", parameters.kf), ("kfre", parameters.kfre)] {
            validate_nonnegative(name, value)?;
        }
        validate_positive("af", parameters.af)?;
        validate_positive("afre", parameters.afre)?;
        for (name, value) in [("alit", parameters.alit), ("alqf", parameters.alqf)] {
            if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                return Err(ModelError::InvalidParameter(name));
            }
        }
        if parameters.af > 10.0 {
            return Err(ModelError::InvalidParameter("af"));
        }
        if parameters.afre > 10.0 {
            return Err(ModelError::InvalidParameter("afre"));
        }
        Ok(Self {
            parameters,
            four_kt: 4.0 * SOURCE_K * temperature.kelvin(),
        })
    }

    /// Emit full local HICUM spectra. Noise-current transfer and correlated
    /// matrix stamping belong to the simulator.
    ///
    /// # Errors
    /// Rejects invalid frequency, nonfinite operating point, unphysical
    /// conductance/avalanche, or finite-precision overflow.
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
        let conductances = [c.rbx, c.rbi, c.rcx, c.re, c.rsu];
        let currents = [
            i.it,
            i.ibei,
            i.ibep,
            i.ibci,
            i.ijbcx,
            i.ijsc,
            i.iavl,
            i.emitter_resistor,
            operating.forward_transit_time,
        ];
        if !conductances
            .iter()
            .chain(currents.iter())
            .all(|value| value.is_finite())
        {
            return Err(EvaluationError::NonFiniteBias);
        }
        if conductances.iter().any(|value| *value < 0.0)
            || i.iavl < 0.0
            || operating.forward_transit_time < 0.0
        {
            return Err(EvaluationError::InvalidPhysicalDomain);
        }
        let thermal = Thermal {
            rbx: self.four_kt * c.rbx,
            rbi: self.four_kt * c.rbi,
            rcx: self.four_kt * c.rcx,
            re: self.four_kt * c.re,
            rsu: self.four_kt * c.rsu,
        };
        let two_q = 2.0 * SOURCE_Q;
        let shot = Shot {
            iavl: two_q * i.iavl,
            ibci: two_q * i.ibci.abs(),
            ibep: two_q * i.ibep.abs(),
            ijbcx: two_q * i.ijbcx.abs(),
            ijsc: two_q * i.ijsc.abs(),
        };
        // One combined source, on the node chosen by CFBE. The emitter
        // flicker term is enabled by topology when RE is present.
        let flicker = Flicker {
            be: self.parameters.kf * (i.ibei + i.ibep).abs().powf(self.parameters.af)
                / frequency_hz,
            emitter_resistor: if c.re > 0.0 {
                self.parameters.kfre * i.emitter_resistor.abs().powf(self.parameters.afre)
                    / frequency_hz
            } else {
                0.0
            },
        };
        let be_ce_shot = self.correlated_shot(i, operating.forward_transit_time, frequency_hz);
        let out = Output {
            thermal,
            shot,
            flicker,
            flicker_location: self.parameters.flicker_location,
            be_ce_shot,
        };
        if ![
            thermal.rbx,
            thermal.rbi,
            thermal.rcx,
            thermal.re,
            thermal.rsu,
            shot.iavl,
            shot.ibci,
            shot.ibep,
            shot.ijbcx,
            shot.ijsc,
            flicker.be,
            flicker.emitter_resistor,
            be_ce_shot.base,
            be_ce_shot.collector,
            be_ce_shot.cross_imaginary,
            be_ce_shot.base_independent,
            be_ce_shot.collector_to_base_imaginary,
            be_ce_shot.n1_seconds,
            be_ce_shot.n2_seconds,
        ]
        .iter()
        .all(|value| value.is_finite())
        {
            return Err(EvaluationError::NonFiniteResult);
        }
        Ok(out)
    }

    fn correlated_shot(&self, currents: Currents, tf: f64, frequency_hz: f64) -> ShotCovariance {
        let be = 2.0 * SOURCE_Q * currents.ibei.abs();
        let ce = 2.0 * SOURCE_Q * currents.it.abs();
        let p = self.parameters;
        if !p.correlated || p.alit <= 0.0 || p.alqf <= 0.0 {
            return ShotCovariance {
                base: be,
                base_independent: be,
                collector: ce,
                ..ShotCovariance::default()
            };
        }
        let beta_dc = if currents.ibei > 0.0 {
            currents.it / currents.ibei
        } else {
            0.0
        };
        let squared_n2 = beta_dc * p.alit.mul_add(-p.alit, 2.0 * p.alqf);
        let n1 = tf * p.alit;
        let n2 = if squared_n2 > 0.0 {
            tf * squared_n2.sqrt()
        } else {
            0.0
        };
        let omega = TWO_PI * frequency_hz;
        let phase_base = omega * n2;
        let phase_collector = omega * n1;
        ShotCovariance {
            base: phase_base
                .mul_add(phase_base, 1.0)
                .mul_add(be, phase_collector * phase_collector * ce),
            collector: ce,
            cross_imaginary: phase_collector * ce,
            base_independent: phase_base.mul_add(phase_base, 1.0) * be,
            collector_to_base_imaginary: phase_collector,
            correlated: true,
            n1_seconds: n1,
            n2_seconds: n2,
        }
    }

    /// # Errors
    /// Rejects invalid frequencies, biases or buffer lengths.
    pub fn evaluate_batch(
        &self,
        operating: &[OperatingPoint],
        frequencies: &[f64],
        outputs: &mut [Output],
    ) -> Result<(), EvaluationError> {
        validate_batch_lengths(operating.len(), outputs.len())?;
        validate_batch_lengths(frequencies.len(), outputs.len())?;
        for ((op, frequency), out) in operating.iter().zip(frequencies).zip(outputs) {
            *out = self.evaluate(*op, *frequency)?;
        }
        Ok(())
    }
}
