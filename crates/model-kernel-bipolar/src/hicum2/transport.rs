//! HICUM/L2 2.4.0 GICCR minority-charge and current transport.
//!
//! Implements HICCR/HICICK/HICQFF/HICQFC without substituting a BJT law.
//! Fixed-size derivatives capture B-E, B-C and implicit hole charge axes;
//! solver owns accepted/trial state and branch charge integration.

pub(crate) mod jet;
mod lateral;

use self::jet::{Jet, Mixed, Scalar};
use super::{differentiation::Dual, junction};
use crate::{
    EvaluationError, ModelError, Temperature, evaluate_lanes, validate_nonnegative,
    validate_positive,
};

const SMOOTHING: f64 = 1.921_812;
const CHARGE_TOLERANCE: f64 = 1.0e-5;
const MAX_NEWTON_ITERATIONS: usize = 101;
const MAX_RELATIVE_NEWTON_STEP: f64 = 0.3;

/// Effective-temperature model coefficients. Fields ending in nominal
/// distinguish mode selectors from temperature-adjusted branch coefficients.
#[derive(Clone, Copy, Debug)]
pub struct Parameters {
    pub junction: junction::Parameters,
    /// Temperature-adjusted GICCR c10 (A^2*s).
    pub c10: f64,
    /// Temperature-adjusted zero-bias hole charge qp0 (C).
    pub qp0: f64,
    pub ich: f64,
    pub mcf: f64,
    pub hjei: f64,
    pub ahjei_nominal: f64,
    pub ahjei: f64,
    pub hjei_zero_bias: f64,
    pub rhjei: f64,
    pub hjci: f64,
    pub hf0: f64,
    pub hfe: f64,
    pub hfc: f64,
    pub t0: f64,
    pub dt0h: f64,
    pub tbvl: f64,
    pub tef0: f64,
    pub gtfe: f64,
    pub thcs: f64,
    pub ahc: f64,
    pub fthc: f64,
    pub rci0: f64,
    pub vlim: f64,
    pub vces: f64,
    pub vpt: f64,
    pub aick: f64,
    pub delck: f64,
    pub tr: f64,
    /// tbhrec, B-C barrier excess-base recombination time (s), zero disables.
    pub tbhrec: f64,
    pub vcbar: f64,
    pub icbar: f64,
    pub acbar: f64,
    pub latb: f64,
    pub latl: f64,
    /// HICUM source coefficient selecting the ICCR collector charge weight.
    pub flcomp: f64,
}

/// Exact preparation-time temperature slopes of HICCR source
/// collector critical-current and forward low-delay parameters.
#[derive(Clone, Copy, Debug, Default)]
pub struct TemperatureSlopes {
    pub rci0: f64,
    pub vlim: f64,
    pub vces: f64,
    pub t0: f64,
    pub c10: f64,
    pub qp0: f64,
    pub ahjei: f64,
    pub hjei_zero_bias: f64,
    pub hf0: f64,
    pub hfe: f64,
    pub hfc: f64,
    pub tef0: f64,
    pub thcs: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct Bias {
    pub vbiei: f64,
    pub vbici: f64,
}

/// A source quantity and its two local intrinsic voltage derivatives.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Value2 {
    pub value: f64,
    pub d_vbiei: f64,
    pub d_vbici: f64,
}

/// Exact source HICCR parameter-and-bias thermal derivatives at
/// fixed normalized physical voltages, after implicit hole-charge
/// differentiation. Every entry is d(source quantity)/dTdev.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TemperatureSensitivity {
    pub forward_current: f64,
    pub reverse_current: f64,
    pub net_transfer_current: f64,
    pub critical_current: f64,
    pub equilibrium_hole_charge: f64,
    pub total_hole_charge: f64,
    pub forward_diffusion_charge: f64,
    pub weighted_forward_charge: f64,
    pub reverse_diffusion_charge: f64,
    pub excess_base_charge: f64,
    pub barrier_base_current: f64,
    pub low_current_transit_time: f64,
    pub forward_transit_time: f64,
    pub weighted_transit_time: f64,
}

impl TemperatureSensitivity {
    fn is_finite(self) -> bool {
        [
            self.forward_current,
            self.reverse_current,
            self.net_transfer_current,
            self.critical_current,
            self.equilibrium_hole_charge,
            self.total_hole_charge,
            self.forward_diffusion_charge,
            self.weighted_forward_charge,
            self.reverse_diffusion_charge,
            self.excess_base_charge,
            self.barrier_base_current,
            self.low_current_transit_time,
            self.forward_transit_time,
            self.weighted_transit_time,
        ]
        .iter()
        .all(|v| v.is_finite())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Output {
    /// Itf (A) and derivatives (S).
    pub forward_current: Value2,
    /// Itr (A) and derivatives (S).
    pub reverse_current: Value2,
    pub net_transfer_current: Value2,
    pub critical_current: Value2,
    /// Q0 (C) and derivatives (F).
    pub equilibrium_hole_charge: Value2,
    /// `QpT` (C) and derivatives (F).
    pub total_hole_charge: Value2,
    /// Qf (C) and derivatives (F).
    pub forward_diffusion_charge: Value2,
    /// `QfT` used in the ICCR charge balance (C).
    pub weighted_forward_charge: Value2,
    pub reverse_diffusion_charge: Value2,
    pub excess_base_charge: Value2,
    /// `Ibh_rec = Q_bf / tbhrec`, source HICCR-excess barrier recombination.
    pub barrier_base_current: Value2,
    pub low_current_transit_time: Value2,
    /// Analytic dQf/dItf in seconds with Ick and Tf0 held fixed.
    pub forward_transit_time: f64,
    /// Exact mixed derivatives dTf/dVbiei and dTf/dVbici (s/V),
    /// obtained by second-order forward AD of the source Qf law.
    pub forward_transit_time_gradient: [f64; 2],
    /// Analytic dQfT/dItf in seconds with Ick and Tf0 held fixed.
    pub weighted_transit_time: f64,
    /// Exact full implicit source physical dI/dTdev and dQ/dTdev.
    pub temperature_sensitivity: TemperatureSensitivity,
    pub iterations: u16,
}

impl Output {
    fn is_finite(&self) -> bool {
        self.temperature_sensitivity.is_finite()
            && [
                self.forward_transit_time,
                self.forward_transit_time_gradient[0],
                self.forward_transit_time_gradient[1],
                self.weighted_transit_time,
                self.forward_current.value,
                self.forward_current.d_vbiei,
                self.forward_current.d_vbici,
                self.reverse_current.value,
                self.reverse_current.d_vbiei,
                self.reverse_current.d_vbici,
                self.net_transfer_current.value,
                self.net_transfer_current.d_vbiei,
                self.net_transfer_current.d_vbici,
                self.critical_current.value,
                self.critical_current.d_vbiei,
                self.critical_current.d_vbici,
                self.equilibrium_hole_charge.value,
                self.equilibrium_hole_charge.d_vbiei,
                self.equilibrium_hole_charge.d_vbici,
                self.total_hole_charge.value,
                self.total_hole_charge.d_vbiei,
                self.total_hole_charge.d_vbici,
                self.forward_diffusion_charge.value,
                self.forward_diffusion_charge.d_vbiei,
                self.forward_diffusion_charge.d_vbici,
                self.weighted_forward_charge.value,
                self.weighted_forward_charge.d_vbiei,
                self.weighted_forward_charge.d_vbici,
                self.reverse_diffusion_charge.value,
                self.reverse_diffusion_charge.d_vbiei,
                self.reverse_diffusion_charge.d_vbici,
                self.excess_base_charge.value,
                self.excess_base_charge.d_vbiei,
                self.excess_base_charge.d_vbici,
                self.barrier_base_current.value,
                self.barrier_base_current.d_vbiei,
                self.barrier_base_current.d_vbici,
                self.low_current_transit_time.value,
                self.low_current_transit_time.d_vbiei,
                self.low_current_transit_time.d_vbici,
            ]
            .iter()
            .all(|value| value.is_finite())
    }
}

impl jet::Scalar for Dual {
    fn constant(value: f64) -> Self {
        Self::new(value, 0.0)
    }
    fn temperature_constant(value: f64, slope: f64) -> Self {
        Self::new(value, slope)
    }
    fn scalar(self) -> f64 {
        self.value
    }
    fn exp(self) -> Self {
        let value = self.value.exp();
        Self::new(value, value * self.slope)
    }
    fn exp_m1(self) -> Self {
        Self::new(self.value.exp_m1(), self.value.exp() * self.slope)
    }
    fn ln(self) -> Self {
        Self::new(self.value.ln(), self.slope / self.value)
    }
    fn ln_1p(self) -> Self {
        Self::new(self.value.ln_1p(), self.slope / (1.0 + self.value))
    }
    fn sqrt(self) -> Self {
        let value = self.value.sqrt();
        Self::new(value, self.slope * 0.5 / value)
    }
    fn powf(self, exponent: f64) -> Self {
        Self::new(
            self.value.powf(exponent),
            self.value.powf(exponent - 1.0) * exponent * self.slope,
        )
    }
    fn is_finite(self) -> bool {
        self.value.is_finite() && self.slope.is_finite()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Prepared {
    p: Parameters,
    junction: junction::Prepared,
    vt: f64,
    inverse_ich: f64,
    legacy_weighting: bool,
    effective_kelvin: f64,
    temperature_slopes: TemperatureSlopes,
}

#[derive(Clone, Copy, Debug)]
struct ForwardStorage<S> {
    actual: S,
    weighted: S,
    base_excess: S,
}

#[derive(Clone, Copy, Debug)]
struct TransitGradient {
    voltage: [f64; 2],
    thermal: f64,
    weighted_thermal: f64,
}

#[derive(Clone, Copy, Debug)]
struct State {
    forward: Jet,
    reverse: Jet,
    storage: ForwardStorage<Jet>,
    reverse_charge: Jet,
    residual: Jet,
}

impl Prepared {
    /// # Errors
    /// Validates all coefficients and the HICUM physical junction model.
    pub fn new(p: &Parameters, temperature: Temperature) -> Result<Self, ModelError> {
        Self::with_temperature_slopes(
            p,
            temperature,
            TemperatureSlopes::default(),
            junction::ParameterSlopes::default(),
        )
    }

    /// Source coefficients and QJMODF are temperature prepared once;
    /// all per-bias HICCR numerical work uses their fixed compiled form.
    ///
    /// # Errors
    /// Rejects unphysical source parameters or nonfinite thermal slopes.
    pub fn with_temperature_slopes(
        p: &Parameters,
        temperature: Temperature,
        temperature_slopes: TemperatureSlopes,
        junction_slopes: junction::ParameterSlopes,
    ) -> Result<Self, ModelError> {
        for (name, value) in [
            ("rci0_temperature_slope", temperature_slopes.rci0),
            ("vlim_temperature_slope", temperature_slopes.vlim),
            ("vces_temperature_slope", temperature_slopes.vces),
            ("t0_temperature_slope", temperature_slopes.t0),
            ("c10_temperature_slope", temperature_slopes.c10),
            ("qp0_temperature_slope", temperature_slopes.qp0),
            ("ahjei_temperature_slope", temperature_slopes.ahjei),
            (
                "hjei_zero_bias_temperature_slope",
                temperature_slopes.hjei_zero_bias,
            ),
            ("hf0_temperature_slope", temperature_slopes.hf0),
            ("hfe_temperature_slope", temperature_slopes.hfe),
            ("hfc_temperature_slope", temperature_slopes.hfc),
            ("tef0_temperature_slope", temperature_slopes.tef0),
            ("thcs_temperature_slope", temperature_slopes.thcs),
        ] {
            if !value.is_finite() {
                return Err(ModelError::InvalidParameter(name));
            }
        }
        let p = *p;
        for (name, value) in [
            ("qp0", p.qp0),
            ("mcf", p.mcf),
            ("rhjei", p.rhjei),
            ("gtfe", p.gtfe),
            ("ahc", p.ahc),
            ("rci0", p.rci0),
            ("vlim", p.vlim),
            ("vpt", p.vpt),
            ("aick", p.aick),
            ("delck", p.delck),
            ("acbar", p.acbar),
        ] {
            validate_positive(name, value)?;
        }
        for (name, value) in [
            ("c10", p.c10),
            ("ich", p.ich),
            ("hjei", p.hjei),
            ("ahjei_nominal", p.ahjei_nominal),
            ("ahjei", p.ahjei),
            ("hjei_zero_bias", p.hjei_zero_bias),
            ("hjci", p.hjci),
            ("hf0", p.hf0),
            ("hfe", p.hfe),
            ("hfc", p.hfc),
            ("t0", p.t0),
            ("tef0", p.tef0),
            ("thcs", p.thcs),
            ("vces", p.vces),
            ("tr", p.tr),
            ("tbhrec", p.tbhrec),
            ("vcbar", p.vcbar),
            ("icbar", p.icbar),
            ("latb", p.latb),
            ("latl", p.latl),
        ] {
            validate_nonnegative(name, value)?;
        }
        validate_nonnegative("flcomp", p.flcomp)?;
        for (name, value) in [("dt0h", p.dt0h), ("tbvl", p.tbvl)] {
            if !value.is_finite() {
                return Err(ModelError::InvalidParameter(name));
            }
        }
        if !p.fthc.is_finite() || !(0.0..=1.0).contains(&p.fthc) {
            return Err(ModelError::InvalidParameter("fthc"));
        }
        let junction =
            junction::Prepared::with_temperature_slopes(p.junction, temperature, junction_slopes)?;
        let inverse_ich = if p.ich == 0.0 { 0.0 } else { 1.0 / p.ich };
        if !inverse_ich.is_finite() {
            return Err(ModelError::InvalidParameter("ich"));
        }
        Ok(Self {
            p,
            junction,
            vt: (1.380_650_3e-23 / 1.602_176_462e-19) * temperature.kelvin(),
            inverse_ich,
            legacy_weighting: p.flcomp == 0.0 || p.flcomp.to_bits() == 2.1_f64.to_bits(),
            effective_kelvin: temperature.kelvin(),
            temperature_slopes,
        })
    }

    /// Evaluate coupled HICCR transport with both intrinsic voltage partials.
    ///
    /// # Errors
    /// Reports nonphysical charge/current or failure of the implicit solve.
    pub fn evaluate(&self, bias: Bias) -> Result<Output, EvaluationError> {
        if !bias.vbiei.is_finite() || !bias.vbici.is_finite() {
            return Err(EvaluationError::NonFiniteBias);
        }
        let emitter = Jet::emitter(bias.vbiei);
        let collector = Jet::collector(bias.vbici);
        let depletion = self
            .junction
            .intrinsic_for_transport(bias.vbiei, bias.vbici)?;
        let equilibrium = self.equilibrium_hole_charge(emitter, depletion, bias)?;
        let initial_time = self.low_current_transit_time(depletion, bias.vbici)?;
        if initial_time.value < 0.0 {
            return Err(EvaluationError::InvalidPhysicalDomain);
        }
        let thermal_voltage = Jet::thermal_constant(self.vt, self.vt / self.effective_kelvin);
        let source_c10 = Jet::thermal_constant(self.p.c10, self.temperature_slopes.c10);
        let i0f = (emitter / (thermal_voltage * self.p.mcf)).exp() * source_c10;
        let i0r = (collector / thermal_voltage).exp() * source_c10;
        let critical = self.critical_current(emitter - collector)?;
        if ![i0f, i0r, critical].iter().all(|x| x.is_finite()) || critical.value <= 0.0 {
            return Err(EvaluationError::InvalidPhysicalDomain);
        }

        let initial = self.initial_hole_charge(equilibrium, i0f, i0r, initial_time)?;
        let state = self.state(initial, equilibrium, i0f, i0r, initial_time, critical)?;
        let high_injection = state.storage.actual.value > CHARGE_TOLERANCE * initial.value
            || self.inverse_ich * (i0f.value / initial.value) > CHARGE_TOLERANCE;
        let (hole_charge, iterations) = if high_injection {
            let first_guess = equilibrium.value
                + (initial_time.value * state.forward.value * state.storage.weighted.value).sqrt()
                + state.reverse_charge.value;
            let (root, iterations) =
                self.solve_hole_charge(first_guess, equilibrium, i0f, i0r, initial_time, critical)?;
            let with_partial = self.state(
                Jet::charge(root),
                equilibrium,
                i0f,
                i0r,
                initial_time,
                critical,
            )?;
            let denominator = with_partial.residual.hole;
            if !denominator.is_finite() || denominator == 0.0 {
                return Err(EvaluationError::InvalidPhysicalDomain);
            }
            (
                Jet::new(
                    root,
                    -with_partial.residual.be / denominator,
                    -with_partial.residual.bc / denominator,
                    0.0,
                )
                .with_temperature(-with_partial.residual.temperature / denominator),
                iterations,
            )
        } else {
            (initial, 0)
        };
        let final_state = self.state(hole_charge, equilibrium, i0f, i0r, initial_time, critical)?;
        let unit_time = self.forward_storage(
            Jet::charge(final_state.forward.value),
            Jet::constant(critical.value),
            Jet::constant(initial_time.value),
        )?;
        let tf_gradient =
            self.transit_time_gradient(final_state.forward, critical, initial_time)?;
        let output = Output {
            forward_current: into_value(final_state.forward),
            reverse_current: into_value(final_state.reverse),
            net_transfer_current: into_value(final_state.forward - final_state.reverse),
            critical_current: into_value(critical),
            equilibrium_hole_charge: into_value(equilibrium),
            total_hole_charge: into_value(hole_charge),
            forward_diffusion_charge: into_value(final_state.storage.actual),
            weighted_forward_charge: into_value(final_state.storage.weighted),
            reverse_diffusion_charge: into_value(final_state.reverse_charge),
            excess_base_charge: into_value(final_state.storage.base_excess),
            barrier_base_current: if self.p.tbhrec > 0.0 {
                into_value(final_state.storage.base_excess / self.p.tbhrec)
            } else {
                Value2::default()
            },
            low_current_transit_time: into_value(initial_time),
            forward_transit_time: unit_time.actual.hole,
            forward_transit_time_gradient: tf_gradient.voltage,
            weighted_transit_time: unit_time.weighted.hole,
            temperature_sensitivity: self.evaluate_temperature_sensitivity(
                &final_state,
                equilibrium,
                hole_charge,
                critical,
                initial_time,
                tf_gradient,
            ),
            iterations,
        };
        if !output.is_finite() {
            return Err(EvaluationError::NonFiniteResult);
        }
        Ok(output)
    }

    fn initial_hole_charge(
        &self,
        equilibrium: Jet,
        i0f: Jet,
        i0r: Jet,
        initial_time: Jet,
    ) -> Result<Jet, EvaluationError> {
        let half = equilibrium * 0.5;
        let initial = if initial_time.value > 0.0 || self.p.tr > 0.0 {
            let quadratic = half * half;
            half + (quadratic + initial_time * i0f + i0r * self.p.tr).sqrt()
        } else {
            equilibrium
        };
        if !initial.is_finite() || initial.value <= 0.0 {
            return Err(EvaluationError::InvalidPhysicalDomain);
        }
        Ok(initial)
    }

    fn evaluate_temperature_sensitivity(
        &self,
        state: &State,
        equilibrium: Jet,
        hole_charge: Jet,
        critical: Jet,
        initial_time: Jet,
        tf_gradient: TransitGradient,
    ) -> TemperatureSensitivity {
        TemperatureSensitivity {
            forward_current: state.forward.temperature,
            reverse_current: state.reverse.temperature,
            net_transfer_current: state.forward.temperature - state.reverse.temperature,
            critical_current: critical.temperature,
            equilibrium_hole_charge: equilibrium.temperature,
            total_hole_charge: hole_charge.temperature,
            forward_diffusion_charge: state.storage.actual.temperature,
            weighted_forward_charge: state.storage.weighted.temperature,
            reverse_diffusion_charge: state.reverse_charge.temperature,
            excess_base_charge: state.storage.base_excess.temperature,
            barrier_base_current: if self.p.tbhrec > 0.0 {
                state.storage.base_excess.temperature / self.p.tbhrec
            } else {
                0.0
            },
            low_current_transit_time: initial_time.temperature,
            forward_transit_time: tf_gradient.thermal,
            weighted_transit_time: tf_gradient.weighted_thermal,
        }
    }

    fn equilibrium_hole_charge(
        &self,
        emitter: Jet,
        dep: junction::IntrinsicForTransport,
        bias: Bias,
    ) -> Result<Jet, EvaluationError> {
        let p = self.p;
        let slopes = self.temperature_slopes;
        let vt = Jet::thermal_constant(self.vt, self.vt / self.effective_kelvin);
        let (emitter_thermal, collector_thermal) = self
            .junction
            .intrinsic_hiccr_temperature_sensitivity(bias.vbiei, bias.vbici)?;
        let hjei = if p.ahjei_nominal == 0.0 {
            Jet::constant(p.hjei)
        } else {
            let potential = Jet::thermal_constant(
                p.junction.intrinsic_emitter.potential,
                self.junction
                    .temperature_slopes()
                    .intrinsic_emitter
                    .potential,
            );
            let grading = p.junction.intrinsic_emitter.grading;
            let scaled = (potential - emitter) / (vt * p.rhjei);
            let smoothed = potential
                - (scaled + (scaled * scaled + SMOOTHING).sqrt()) * (vt * (0.5 * p.rhjei));
            let offset = (smoothed - vt) / vt;
            let corrected = (offset + (offset * offset + SMOOTHING).sqrt()) * (vt * 0.5) + vt;
            let z = (Jet::constant(1.0)
                - (Jet::constant(1.0) - corrected / potential).powf(grading))
                * Jet::thermal_constant(p.ahjei, slopes.ahjei);
            exprel(z) * Jet::thermal_constant(p.hjei_zero_bias, slopes.hjei_zero_bias)
        };
        let emitter_charge = Jet::new(dep.emitter.coulombs, dep.emitter.farads, 0.0, 0.0)
            .with_temperature(emitter_thermal.charge);
        let collector_charge = Jet::new(dep.collector.coulombs, 0.0, dep.collector.farads, 0.0)
            .with_temperature(collector_thermal.charge);
        let raw = Jet::thermal_constant(p.qp0, slopes.qp0)
            + hjei * emitter_charge
            + collector_charge * p.hjci;
        let floor = Jet::thermal_constant(0.05 * p.qp0, 0.05 * slopes.qp0);
        let scaled = raw / floor - 1.0;
        let result =
            (Jet::constant(1.0) + (scaled + (scaled * scaled + SMOOTHING).sqrt()) * 0.5) * floor;
        if !result.is_finite() || result.value <= 0.0 {
            return Err(EvaluationError::InvalidPhysicalDomain);
        }
        Ok(result)
    }

    fn low_current_transit_time(
        &self,
        dep: junction::IntrinsicForTransport,
        collector_voltage: f64,
    ) -> Result<Jet, EvaluationError> {
        let p = self.p;
        let slopes = self.temperature_slopes;
        let t0 = Jet::thermal_constant(p.t0, slopes.t0);
        let cap_zero = p.junction.intrinsic_collector.zero_bias_capacitance;
        if cap_zero == 0.0 {
            return Ok(t0);
        }
        let cap = Jet::new(
            dep.transit_collector_capacitance,
            0.0,
            dep.d_transit_collector_capacitance_vbici,
            0.0,
        )
        .with_temperature(
            self.junction
                .transit_collector_temperature_sensitivity(collector_voltage)?,
        );
        let ratio = cap
            / Jet::thermal_constant(
                cap_zero,
                self.junction
                    .temperature_slopes()
                    .intrinsic_collector
                    .zero_bias_capacitance,
            );
        let inv_ratio = Jet::constant(1.0) / ratio;
        let time = t0 + (inv_ratio - 1.0) * p.dt0h + (ratio - 1.0) * p.tbvl;
        if !time.is_finite() {
            return Err(EvaluationError::NonFiniteResult);
        }
        Ok(time)
    }

    fn critical_current(&self, vcei: Jet) -> Result<Jet, EvaluationError> {
        self.critical_current_source(
            vcei,
            Jet::thermal_constant(self.p.rci0, self.temperature_slopes.rci0),
            Jet::thermal_constant(self.p.vlim, self.temperature_slopes.vlim),
            Jet::thermal_constant(self.p.vces, self.temperature_slopes.vces),
            Jet::thermal_constant(self.vt, self.vt / self.effective_kelvin),
        )
    }

    /// Complete source critical current dIck/dTdev at fixed normalized
    /// collector-emitter voltage. Includes all source velocity, series
    /// resistance, knee and VT temperature dependencies.
    ///
    /// # Errors
    /// Rejects nonfinite source bias and numerical overflow.
    pub fn critical_current_temperature_sensitivity(
        &self,
        vcei: f64,
    ) -> Result<f64, EvaluationError> {
        if !vcei.is_finite() {
            return Err(EvaluationError::NonFiniteBias);
        }
        let s = self.temperature_slopes;
        let source = self.critical_current_source(
            Dual::new(vcei, 0.0),
            Dual::new(self.p.rci0, s.rci0),
            Dual::new(self.p.vlim, s.vlim),
            Dual::new(self.p.vces, s.vces),
            Dual::new(self.vt, self.vt / self.effective_kelvin),
        )?;
        Ok(source.slope)
    }

    /// Complete source low-current transit delay dTf0/dTdev at fixed
    /// intrinsic B-C voltage, including separate unpunched HICCR
    /// Cjcit(T), intrinsic Cjci0(T), T0(T), DT0H and TBVL branches.
    ///
    /// # Errors
    /// Rejects invalid collector bias or nonfinite derivative results.
    pub fn low_current_transit_time_temperature_sensitivity(
        &self,
        collector_voltage: f64,
    ) -> Result<f64, EvaluationError> {
        if !collector_voltage.is_finite() {
            return Err(EvaluationError::NonFiniteBias);
        }
        let c0 = self.p.junction.intrinsic_collector.zero_bias_capacitance;
        if c0 == 0.0 {
            return Ok(self.temperature_slopes.t0);
        }
        let trans = self
            .junction
            .intrinsic_for_transport(0.0, collector_voltage)?;
        let cap = trans.transit_collector_capacitance;
        let cap_slope = self
            .junction
            .transit_collector_temperature_sensitivity(collector_voltage)?;
        let c0_slope = self
            .junction
            .temperature_slopes()
            .intrinsic_collector
            .zero_bias_capacitance;
        let ratio = cap / c0;
        let ratio_slope = ratio.mul_add(-c0_slope, cap_slope) / c0;
        let result = self.temperature_slopes.t0
            + ratio_slope.mul_add(self.p.tbvl - self.p.dt0h / (ratio * ratio), 0.0);
        if !result.is_finite() {
            return Err(EvaluationError::NonFiniteResult);
        }
        Ok(result)
    }

    /// One HICICK source law statically instantiated by the original
    /// intrinsic-voltage Jet and a one-lane temperature Dual.
    fn critical_current_source<S: jet::Scalar>(
        &self,
        collector_emitter: S,
        rci0: S,
        vlim: S,
        saturation_voltage: S,
        vt: S,
    ) -> Result<S, EvaluationError> {
        let p = self.p;
        let a = (collector_emitter - saturation_voltage) / vt - 1.0;
        let effective = (a + (a * a + SMOOTHING).sqrt()) * (vt * 0.5) + vt;
        let ratio = effective / vlim;
        let rolloff = (S::constant(1.0) + ratio.powf(p.delck)).powf(1.0 / p.delck);
        let knee = (effective - vlim) / p.vpt;
        let raised = (knee + (knee * knee + p.aick).sqrt()) * 0.5 + 1.0;
        let result = effective / (rolloff * rci0) * raised;
        if !result.is_finite() || result.scalar() <= 0.0 {
            return Err(EvaluationError::InvalidPhysicalDomain);
        }
        Ok(result)
    }

    fn transit_time_gradient(
        &self,
        forward: Jet,
        critical: Jet,
        initial_time: Jet,
    ) -> Result<TransitGradient, EvaluationError> {
        // Qf=Tf0*Itf below the low-injection threshold; weighted
        // storage equals actual source charge in that branch.
        if forward.value < 1.0e-6 * critical.value {
            return Ok(TransitGradient {
                voltage: [initial_time.be, initial_time.bc],
                thermal: initial_time.temperature,
                weighted_thermal: initial_time.temperature,
            });
        }
        // Above the threshold with no source excess storage, Qf=Tf0*Itf
        // while weighted QfT=HF0*Tf0*Itf. Keep source HF0(T) exact.
        if self.p.tef0 == 0.0 && self.p.thcs == 0.0 {
            return Ok(TransitGradient {
                voltage: [initial_time.be, initial_time.bc],
                thermal: initial_time.temperature,
                weighted_thermal: initial_time
                    .temperature
                    .mul_add(self.p.hf0, initial_time.value * self.temperature_slopes.hf0),
            });
        }
        // Mixed second derivatives of the *same source forward-charge
        // law*: ∂²Qf/(∂Itf∂Vbe, ∂Itf∂Vbc, ∂Itf∂Tdev).
        // The effective Itf, Ick and Tf0 temperature gradients and
        // all explicit HICQFF/HICQFC temperature coefficients propagate
        // without a numerical perturbation or duplicated source law.
        let mixed = self.forward_storage(
            Mixed::new(forward.value, forward.be, forward.bc, 1.0)
                .with_temperature(forward.temperature),
            Mixed::new(critical.value, critical.be, critical.bc, 0.0)
                .with_temperature(critical.temperature),
            Mixed::new(initial_time.value, initial_time.be, initial_time.bc, 0.0)
                .with_temperature(initial_time.temperature),
        )?;
        Ok(TransitGradient {
            voltage: [mixed.actual.d_hole_be, mixed.actual.d_hole_bc],
            thermal: mixed.actual.d_hole_temperature,
            weighted_thermal: mixed.weighted.d_hole_temperature,
        })
    }

    fn forward_storage<S: Scalar>(
        &self,
        itf: S,
        critical: S,
        initial_time: S,
    ) -> Result<ForwardStorage<S>, EvaluationError> {
        let p = self.p;
        let slopes = self.temperature_slopes;
        let vt = S::temperature_constant(self.vt, self.vt / self.effective_kelvin);
        let thcs = S::temperature_constant(p.thcs, slopes.thcs);
        let low = initial_time * itf;
        if itf.scalar() < 1.0e-6 * critical.scalar() {
            return Ok(ForwardStorage {
                actual: low,
                weighted: low,
                base_excess: S::constant(0.0),
            });
        }
        let ratio = itf / critical;
        let emitter_time =
            ratio.powf(p.gtfe) * S::temperature_constant(p.tef0, self.temperature_slopes.tef0);
        let emitter_charge = emitter_time * itf / (1.0 + p.gtfe);
        let barrier = if p.icbar < 0.05 * (p.vlim / p.rci0) {
            S::constant(0.0)
        } else {
            let normalized = (itf - critical) / p.icbar;
            let limited = if normalized.scalar() < -1.0e10 {
                S::constant(-1.0e10)
            } else {
                normalized
            };
            let root = (limited * limited + p.acbar).sqrt();
            let spread = if limited.scalar() < 0.0 {
                S::constant(p.acbar) / ((root - limited) * 2.0)
            } else {
                (limited + root) * 0.5
            };
            if -1.0 / spread.scalar() < -745.0 {
                S::constant(0.0)
            } else {
                (S::constant(-1.0) / spread).exp() * p.vcbar
            }
        };
        let base_part = thcs * (1.0 - p.fthc);
        let base_barrier = itf * base_part * (barrier / vt).exp_m1();
        let spreading = S::constant(1.0) - critical / itf;
        let smoothing = (spreading * spreading + p.ahc).sqrt();
        let width = (spreading + smoothing) / (1.0 + (1.0 + p.ahc).sqrt());
        let collector_common = itf * thcs * width * width * ((barrier - p.vcbar) / vt).exp();
        let base_collector = collector_common * (1.0 - p.fthc);
        let (collector_actual, collector_weighted) = if p.latb == 0.0 && p.latl == 0.0 {
            let value = collector_common * p.fthc;
            (value, value)
        } else if p.fthc == 0.0 {
            (S::constant(0.0), S::constant(0.0))
        } else {
            let (actual_factor, weighted_factor) =
                lateral::factors(itf, critical, p.ahc, p.latb, p.latl, self.legacy_weighting)?;
            let common = itf * (thcs * p.fthc) * ((barrier - p.vcbar) / vt).exp();
            (common * actual_factor, common * weighted_factor)
        };
        let excess_base = base_barrier + base_collector;
        let actual = low + excess_base + emitter_charge + collector_actual;
        let weighted = low * S::temperature_constant(p.hf0, slopes.hf0)
            + excess_base
            + emitter_charge * S::temperature_constant(p.hfe, slopes.hfe)
            + collector_weighted * S::temperature_constant(p.hfc, slopes.hfc);
        if !actual.is_finite() || !weighted.is_finite() || !excess_base.is_finite() {
            return Err(EvaluationError::NonFiniteResult);
        }
        Ok(ForwardStorage {
            actual,
            weighted,
            base_excess: excess_base,
        })
    }

    fn state(
        &self,
        qp: Jet,
        q0: Jet,
        i0f: Jet,
        i0r: Jet,
        initial_time: Jet,
        critical: Jet,
    ) -> Result<State, EvaluationError> {
        if qp.value <= 0.0 {
            return Err(EvaluationError::InvalidPhysicalDomain);
        }
        let low_forward = i0f / qp;
        let injection = low_forward * self.inverse_ich;
        let forward = low_forward * (injection + 1.0);
        let reverse = i0r / qp;
        let storage = self.forward_storage(forward, critical, initial_time)?;
        let reverse_charge = reverse * self.p.tr;
        let residual = qp - q0 - storage.weighted - reverse_charge;
        if ![
            forward,
            reverse,
            storage.actual,
            storage.weighted,
            storage.base_excess,
            reverse_charge,
            residual,
        ]
        .iter()
        .all(|x| x.is_finite())
        {
            return Err(EvaluationError::NonFiniteResult);
        }
        Ok(State {
            forward,
            reverse,
            storage,
            reverse_charge,
            residual,
        })
    }

    fn solve_hole_charge(
        &self,
        initial: f64,
        q0: Jet,
        i0f: Jet,
        i0r: Jet,
        initial_time: Jet,
        critical: Jet,
    ) -> Result<(f64, u16), EvaluationError> {
        if !initial.is_finite() || initial <= 0.0 {
            return Err(EvaluationError::InvalidPhysicalDomain);
        }
        let mut qp = initial;
        for index in 0..MAX_NEWTON_ITERATIONS {
            let state = self.state(Jet::charge(qp), q0, i0f, i0r, initial_time, critical)?;
            let gradient = state.residual.hole;
            if gradient == 0.0 || !gradient.is_finite() {
                return Err(EvaluationError::InvalidPhysicalDomain);
            }
            let step = -state.residual.value / gradient;
            let bounded = step.clamp(
                -MAX_RELATIVE_NEWTON_STEP * qp,
                MAX_RELATIVE_NEWTON_STEP * qp,
            );
            qp += bounded;
            if !qp.is_finite() || qp <= 0.0 {
                return Err(EvaluationError::InvalidPhysicalDomain);
            }
            if bounded.abs() < CHARGE_TOLERANCE * qp {
                let iterations =
                    u16::try_from(index + 1).map_err(|_| EvaluationError::InvalidPhysicalDomain)?;
                return Ok((qp, iterations));
            }
        }
        Err(EvaluationError::NonConverged)
    }

    /// Evaluate a homogeneous HICCR batch without allocation.
    ///
    /// All slice lengths are checked before any writes. Lanes run in order;
    /// the first error leaves that lane and the remaining outputs unchanged.
    ///
    /// # Errors
    /// Rejects incompatible buffers or a physical/numerical solve error.
    pub fn evaluate_batch(
        &self,
        inputs: &[Bias],
        outputs: &mut [Output],
    ) -> Result<(), EvaluationError> {
        evaluate_lanes(inputs.iter().copied(), outputs, |input| {
            self.evaluate(input)
        })
    }
}

fn exprel(value: Jet) -> Jet {
    if value.value.abs() < 1.0e-4 {
        let x2 = value * value;
        Jet::constant(1.0)
            + value * 0.5
            + x2 / 6.0
            + x2 * value / 24.0
            + x2 * x2 / 120.0
            + x2 * x2 * value / 720.0
    } else {
        value.exp_m1() / value
    }
}

const fn into_value(jet: Jet) -> Value2 {
    Value2 {
        value: jet.value,
        d_vbiei: jet.be,
        d_vbici: jet.bc,
    }
}
