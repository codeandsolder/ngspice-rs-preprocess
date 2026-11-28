//! Prepared ngspice 47 VDMOS physics, without circuit topology or timestep integration.
//!
//! Inputs and outputs use SI units. The kernel performs no allocation, changes no
//! accepted/trial state, and has no dependence on the circuit solver. The caller
//! binds terminal voltages, integrates charge derivatives, limits diode voltage,
//! and stamps current and heat residuals.
//!
//! The equations are based on ngspice 47's vdmosload.c, vdmostemp.c,
//! vdmosset.c and `DevCapVDMOS` in devsup.c. The implementation differentiates
//! the equations themselves; it does not copy several inconsistent historical
//! C derivative approximations (most visibly the reverse diode conductance).
//!
//! BV/IBV/NBV avalanche is modeled natively; unsupported source options
//! fail at parameter compilation rather than substituting MOS1 or BSIM.

pub mod artifact;
mod breakdown;
pub mod observation;
mod parameters;

pub use parameters::{ParameterError, compile_ngspice};

use model_kernel_api::taylor::arithmetic::Scalar;
use std::cmp::Ordering;
use std::ops::{Add, Div, Mul, Neg, Sub};

/// Physical MOS channel polarity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Polarity {
    /// N-channel enhancement device.
    N,
    /// P-channel enhancement device.
    P,
}

impl Polarity {
    /// Physical channel/diode sign: +1 for N-channel and -1 for P-channel.
    /// The solver uses this once when binding polarity-normalized Qp states.
    #[must_use = "the polarity sign is needed for normalization"]
    pub const fn sign(self) -> f64 {
        match self {
            Self::N => 1.0,
            Self::P => -1.0,
        }
    }
}

/// Channel current, weak inversion and quasi-saturation fitting parameters.
#[derive(Clone, Copy, Debug)]
pub struct Channel {
    /// Signed SPICE threshold voltage at nominal temperature, volts.
    pub vth: f64,
    /// Zero-bias transconductance factor, A/V².
    pub kp: f64,
    /// Channel length modulation, 1/V.
    pub lambda: f64,
    /// Mobility field term, 1/V.
    pub theta: f64,
    /// Multiplier on the triode-region drain voltage.
    pub mtriode: f64,
    /// Weak-inversion gate-voltage shift, V.
    pub subshift: f64,
    /// Weak-inversion logarithmic voltage scale, V.
    pub ksubthres: f64,
    /// Linear temperature coefficient of weak-inversion slope, 1/K.
    pub tksubthres1: f64,
    /// Quadratic temperature coefficient of weak-inversion slope, 1/K².
    pub tksubthres2: f64,
    /// Linear threshold-temperature coefficient, V/K.
    pub tcvth: f64,
    /// Temperature exponent of transconductance.
    pub mu: f64,
}

impl Channel {
    fn defaults(p: Polarity) -> Self {
        Self {
            vth: 3.0 * p.sign(),
            kp: 5.0f64.mul_add(p.sign(), 15.0),
            lambda: 0.0,
            theta: 0.0,
            mtriode: 1.0,
            subshift: 0.0,
            ksubthres: 0.1,
            tksubthres1: 0.0,
            tksubthres2: 0.0,
            tcvth: 0.0,
            mu: -1.5,
        }
    }
}

/// Bias-dependent VDMOS gate capacitance parameters.
#[derive(Clone, Copy, Debug)]
pub struct GateCharge {
    /// Positive-bias limiting gate-drain capacitance, farads.
    pub cgdmax: f64,
    /// Negative-bias limiting gate-drain capacitance, farads.
    pub cgdmin: f64,
    /// Gate-drain capacitance transition sharpness, 1/V.
    pub a: f64,
    /// Constant gate-source capacitance, farads.
    pub cgs: f64,
}

impl Default for GateCharge {
    fn default() -> Self {
        Self {
            cgdmax: 2e-9,
            cgdmin: 2e-11,
            a: 1.0,
            cgs: 1.4e-9,
        }
    }
}

/// Body diode, junction and ngspice 47 soft-recovery parameters.
#[derive(Clone, Copy, Debug)]
pub struct BodyDiode {
    /// Saturation current at the nominal temperature, amperes.
    pub is: f64,
    /// Emission coefficient.
    pub n: f64,
    /// Zero-bias junction capacitance, farads.
    pub cjo: f64,
    /// Junction potential, volts.
    pub vj: f64,
    /// Junction grading exponent.
    pub m: f64,
    /// Forward-bias capacitance transition fraction.
    pub fc: f64,
    /// Diffusion carrier transit time, seconds.
    pub tt: f64,
    /// Recovery charge partition parameter. Zero disables the extra Qp node.
    pub vp: f64,
    /// Semiconductor activation energy, eV (numerically volts here).
    pub eg: f64,
    /// Saturation-current temperature exponent.
    pub xti: f64,
    /// Explicit BV/IBV/NBV avalanche junction parameters.
    pub breakdown: Option<Breakdown>,
    /// Linear temperature coefficient for diode series resistance.
    pub trb1: f64,
    /// Quadratic temperature coefficient for diode series resistance.
    pub trb2: f64,
    /// Linear temperature coefficient for transit time.
    pub ttt1: f64,
    /// Quadratic temperature coefficient for transit time.
    pub ttt2: f64,
    /// Linear temperature coefficient for grading exponent.
    pub tm1: f64,
    /// Quadratic temperature coefficient for grading exponent.
    pub tm2: f64,
}

impl Default for BodyDiode {
    fn default() -> Self {
        Self {
            is: 1e-14,
            n: 1.0,
            cjo: 5e-10,
            vj: 0.8,
            m: 0.5,
            fc: 0.5,
            tt: 0.0,
            vp: 0.0,
            eg: 1.11,
            xti: 3.0,
            breakdown: None,
            trb1: 0.0,
            trb2: 0.0,
            ttt1: 0.0,
            ttt2: 0.0,
            tm1: 0.0,
            tm2: 0.0,
        }
    }
}

/// Breakdown parameters (ngspice BV/IBV/NBV).
#[derive(Clone, Copy, Debug)]
pub struct Breakdown {
    /// Reverse breakdown voltage magnitude, V.
    pub bv: f64,
    /// Reverse breakdown test current, A.
    pub ibv: f64,
    /// Reverse breakdown emission coefficient.
    pub nbv: f64,
}

/// Ohmic elements and optional quasi-saturation series drain resistance.
#[derive(Clone, Copy, Debug)]
pub struct Series {
    /// Drain resistance, ohms.
    pub rd: f64,
    /// Source resistance, ohms.
    pub rs: f64,
    /// Gate resistance, ohms.
    pub rg: f64,
    /// Body-diode resistance, ohms.
    pub rb: f64,
    /// Drain-source shunt resistance, ohms.
    pub rds: f64,
    /// Either both RQ/VQ are set or neither is.
    pub quasi_saturation: Option<QuasiSaturation>,
    /// Drain-resistance temperature exponent; overrides TRD1/TRD2 when set.
    pub texp0: Option<f64>,
    /// Quasi-saturation resistance temperature exponent.
    pub texp1: f64,
    /// Linear/quadratic temperature coefficients.
    pub trd1: f64,
    /// Quadratic drain resistance temperature coefficient.
    pub trd2: f64,
    /// Linear gate resistance temperature coefficient.
    pub trg1: f64,
    /// Quadratic gate resistance temperature coefficient.
    pub trg2: f64,
    /// Linear source resistance temperature coefficient.
    pub trs1: f64,
    /// Quadratic source resistance temperature coefficient.
    pub trs2: f64,
}

impl Default for Series {
    fn default() -> Self {
        Self {
            rd: 0.0,
            rs: 0.0,
            rg: 0.0,
            rb: 0.0,
            rds: 1e15,
            quasi_saturation: None,
            texp0: None,
            texp1: 0.3,
            trd1: 0.0,
            trd2: 0.0,
            trg1: 0.0,
            trg2: 0.0,
            trs1: 0.0,
            trs2: 0.0,
        }
    }
}

/// Additional drain series resistance following the VDMOS quasi-saturation law.
#[derive(Clone, Copy, Debug)]
pub struct QuasiSaturation {
    /// Resistance increment RQ, ohms.
    pub rq: f64,
    /// Voltage scale VQ, volts.
    pub vq: f64,
}

/// Parameters of the junction-to-case thermal equivalent circuit.
#[derive(Clone, Copy, Debug)]
pub struct Thermal {
    /// Junction-to-case resistance, K/W.
    pub rthjc: f64,
    /// Case-to-ambient resistance, K/W.
    pub rthca: f64,
    /// Junction heat capacitance, J/K.
    pub cthj: f64,
}

impl Default for Thermal {
    fn default() -> Self {
        Self {
            rthjc: 1.0,
            rthca: 23.0,
            cthj: 10e-6,
        }
    }
}

/// Intrinsic VDMOS noise parameters, matching the ngspice 47 noise law.
#[derive(Clone, Copy, Debug)]
pub struct Noise {
    /// Flicker-current coefficient KF.
    pub kf: f64,
    /// Flicker-current exponent AF.
    pub af: f64,
}

impl Default for Noise {
    fn default() -> Self {
        Self { kf: 0.0, af: 1.0 }
    }
}

/// Independent branch noise current source densities, in A²/Hz.
///
/// The simulator applies each source's own topology and small-signal
/// transfer function when accumulating noise at a requested observation.
#[derive(Clone, Copy, Debug)]
pub struct NoiseDensity {
    /// Thermal noise of the drain ohmic resistor.
    pub drain: f64,
    /// Thermal noise of the source ohmic resistor.
    pub source: f64,
    /// Channel thermal current noise (2/3 factor).
    pub channel: f64,
    /// Channel flicker current noise.
    pub flicker: f64,
}

/// Typed, pre-parse ngspice VDMOS model parameters, before instance binding.
#[derive(Clone, Copy, Debug)]
pub struct Model {
    /// MOS polarity.
    pub polarity: Polarity,
    /// Nominal model temperature, K.
    pub tnom: f64,
    /// Channel law.
    pub channel: Channel,
    /// Intrinsic gate capacitance law.
    pub gate: GateCharge,
    /// Body diode.
    pub diode: BodyDiode,
    /// Series/ohmic elements.
    pub series: Series,
    /// Thermal circuit.
    pub thermal: Thermal,
    /// Intrinsic noise law.
    pub noise: Noise,
    /// Nominal surface potential for distortion setup (V).
    pub phi: f64,
    /// Diagnostic-only safe operating area declarations.
    pub soa: observation::SoaLimits,
}

impl Model {
    /// ngspice 47 defaults for the chosen device type.
    #[must_use = "the returned model contains polarity-specific parameters"]
    pub fn defaults(polarity: Polarity) -> Self {
        Self {
            polarity,
            tnom: 300.15,
            channel: Channel::defaults(polarity),
            gate: GateCharge::default(),
            diode: BodyDiode::default(),
            series: Series::default(),
            thermal: Thermal::default(),
            noise: Noise::default(),
            phi: 0.6,
            soa: observation::SoaLimits::default(),
        }
    }

    /// Validate and prepare a shared model. No string lookups in evaluation.
    ///
    /// # Errors
    ///
    /// Fails for unsupported physical modes or invalid parameter domains.
    pub fn prepare(self) -> Result<PreparedModel, ModelError> {
        let channel = self.channel;
        let diode = self.diode;
        let gate = self.gate;
        let series = self.series;
        let thermal = self.thermal;

        self.validate_finite()?;
        self.soa.validate()?;
        if let Some(exponent) = series.texp0
            && !exponent.is_finite()
        {
            return Err(ModelError::Invalid("texp0"));
        }
        if let Some(quasi) = series.quasi_saturation
            && (!quasi.rq.is_finite() || quasi.rq < 0.0 || !quasi.vq.is_finite() || quasi.vq == 0.0)
        {
            return Err(ModelError::Invalid("rq/vq"));
        }
        if let Some(breakdown) = diode.breakdown
            && (!breakdown.bv.is_finite()
                || breakdown.bv == 0.0
                || !breakdown.ibv.is_finite()
                || breakdown.ibv < 0.0
                || !breakdown.nbv.is_finite()
                || breakdown.nbv <= 0.0)
        {
            return Err(ModelError::Invalid("bv/ibv/nbv"));
        }
        for (name, invalid) in [
            ("tnom", self.tnom <= 0.0),
            ("phi", self.phi <= 0.0),
            ("kp", channel.kp < 0.0),
            ("ksubthres", channel.ksubthres < 0.0),
            ("mtriode", channel.mtriode <= 0.0),
            ("cgdmin", gate.cgdmin < 0.0),
            ("cgdmax", gate.cgdmax < 0.0),
            ("a", gate.a < 0.0),
            ("cgs", gate.cgs < 0.0),
            ("is", diode.is <= 0.0),
            ("n", diode.n <= 0.0),
            ("cjo", diode.cjo < 0.0),
            ("vj", diode.vj <= 0.0),
            ("fc", !(0.0..1.0).contains(&diode.fc)),
            ("tt", diode.tt < 0.0),
            ("eg", diode.eg <= 0.0),
            ("vp", !(0.0..=1.0).contains(&diode.vp)),
            ("rd", series.rd < 0.0),
            ("rs", series.rs < 0.0),
            ("rg", series.rg < 0.0),
            ("rb", series.rb < 0.0),
            ("rds", series.rds <= 0.0),
            ("rthjc", thermal.rthjc <= 0.0),
            ("rthca", thermal.rthca <= 0.0),
            ("cthj", thermal.cthj < 0.0),
            ("kf", self.noise.kf < 0.0),
        ] {
            if invalid {
                return Err(ModelError::Invalid(name));
            }
        }
        let nominal_pbfact = pbfact(Jet::<f64>::constant(self.tnom)).v;
        let pbo = (self.diode.vj - nominal_pbfact) / (self.tnom / REFERENCE_T);
        if !pbo.is_finite() || pbo <= 0.0 {
            return Err(ModelError::Invalid("diode nominal junction potential"));
        }
        Ok(PreparedModel { model: self, pbo })
    }
}

impl Model {
    fn validate_finite(self) -> Result<(), ModelError> {
        let channel = self.channel;
        let diode = self.diode;
        let gate = self.gate;
        let series = self.series;
        let thermal = self.thermal;
        for (name, value) in [
            ("tnom", self.tnom),
            ("phi", self.phi),
            ("kp", channel.kp),
            ("vth", channel.vth),
            ("lambda", channel.lambda),
            ("theta", channel.theta),
            ("mtriode", channel.mtriode),
            ("subshift", channel.subshift),
            ("ksubthres", channel.ksubthres),
            ("tksubthres1", channel.tksubthres1),
            ("tksubthres2", channel.tksubthres2),
            ("tcvth", channel.tcvth),
            ("mu", channel.mu),
            ("cgdmin", gate.cgdmin),
            ("cgdmax", gate.cgdmax),
            ("a", gate.a),
            ("cgs", gate.cgs),
            ("is", diode.is),
            ("n", diode.n),
            ("cjo", diode.cjo),
            ("vj", diode.vj),
            ("m", diode.m),
            ("fc", diode.fc),
            ("tt", diode.tt),
            ("vp", diode.vp),
            ("eg", diode.eg),
            ("xti", diode.xti),
            ("trb1", diode.trb1),
            ("trb2", diode.trb2),
            ("ttt1", diode.ttt1),
            ("ttt2", diode.ttt2),
            ("tm1", diode.tm1),
            ("tm2", diode.tm2),
            ("rd", series.rd),
            ("rs", series.rs),
            ("rg", series.rg),
            ("rb", series.rb),
            ("rds", series.rds),
            ("texp1", series.texp1),
            ("trd1", series.trd1),
            ("trd2", series.trd2),
            ("trg1", series.trg1),
            ("trg2", series.trg2),
            ("trs1", series.trs1),
            ("trs2", series.trs2),
            ("rthjc", thermal.rthjc),
            ("rthca", thermal.rthca),
            ("cthj", thermal.cthj),
            ("kf", self.noise.kf),
            ("af", self.noise.af),
        ] {
            if !value.is_finite() {
                return Err(ModelError::Invalid(name));
            }
        }
        Ok(())
    }
}

pub use model_kernel_api::ModelError;

/// Validated, immutable model shared by independent device instances.
#[derive(Clone, Copy, Debug)]
pub struct PreparedModel {
    model: Model,
    pbo: f64,
}

/// Prepared geometry and independently bound instance switches.
#[derive(Clone, Copy, Debug)]
pub struct Instance {
    /// Number of parallel devices, > 0.
    pub multiplier: f64,
    /// Whether self-heating participates in the external thermal network.
    pub self_heating: bool,
}

impl Default for Instance {
    fn default() -> Self {
        Self {
            multiplier: 1.0,
            self_heating: false,
        }
    }
}

/// Immutable, geometry-bound instance kernel.
#[derive(Clone, Copy, Debug)]
pub struct PreparedInstance<'a> {
    model: &'a PreparedModel,
    multiplier: f64,
    self_heating: bool,
}

/// Biases bound by the simulator, all in physical (not polarity-normalized) volts.
///
/// The body diode is oriented source-to-drain for N-channel VDMOS and
/// drain-to-source for P-channel VDMOS. The caller supplies its intrinsic
/// junction voltage separately from the external drain-source voltage.
#[derive(Clone, Copy, Debug, Default)]
pub struct Bias<S = f64> {
    /// Intrinsic gate-source bias, V.
    pub vgs: S,
    /// Intrinsic drain-source bias, V.
    pub vds: S,
    /// External drain-source bias used in quasi-saturation, V.
    pub vds_external: S,
    /// Intrinsic body diode voltage in its physical polarity, V.
    pub vdiode: S,
    /// Polarity-normalized ngspice-47 soft-recovery internal node coordinate.
    /// Its actual integrated charge is TT(T) times this coordinate.
    pub recovery_node: S,
    /// Instantaneous device temperature, K.
    pub temperature: S,
}

/// Scalar current and exact derivatives with respect to Vgs, Vds and T.
#[derive(Clone, Copy, Debug, Default)]
pub struct Current<S = f64> {
    /// Current, A.
    pub value: S,
    /// Partial derivative with respect to the physical gate-source voltage.
    pub d_vgs: S,
    /// Partial derivative with respect to the physical drain-source voltage.
    pub d_vds: S,
    /// Partial derivative with respect to Kelvin temperature.
    pub d_temperature: S,
}

/// Charge, differential capacitance and temperature derivative.
#[derive(Clone, Copy, Debug, Default)]
pub struct Charge<S = f64> {
    /// Stored charge, C.
    pub value: S,
    /// Partial derivative with respect to associated physical voltage, F.
    pub d_voltage: S,
    /// Partial derivative with respect to Kelvin temperature, C/K.
    pub d_temperature: S,
}

/// Compiled channel, gate and body diode equations at a single bias point.
#[derive(Clone, Copy, Debug)]
pub struct Evaluation<S = f64> {
    /// Temperature at which this evaluation was prepared, K.
    pub temperature: S,
    /// Intrinsic drain-to-source channel conduction.
    pub channel: Current<S>,
    /// Gate-source charge, on gate plate.
    pub qgs: Charge<S>,
    /// Gate-drain charge, on gate plate.
    pub qgd: Charge<S>,
    /// Body diode conduction, positive in its forward physical direction.
    pub diode: DiodeEvaluation<S>,
    /// Thermal source from intrinsic resistive/channel dissipation.
    pub heat: Heat<S>,
    /// Series resistances and voltage-dependent conductances.
    pub series: SeriesEvaluation<S>,
}

/// Body-diode conduction, charge and extra recovery state equation.
#[derive(Clone, Copy, Debug)]
pub struct DiodeEvaluation<S = f64> {
    /// Junction DC current in the diode forward direction.
    pub current: ChargeCurrent<S>,
    /// Depletion charge only; no solver history is included.
    pub depletion: Charge<S>,
    /// Diffusion charge when VP=0; absent when Qp relaxation is active.
    pub diffusion: Charge<S>,
    /// Relaxation state source and coefficients when VP != 0 and TT != 0.
    pub recovery: Option<Recovery<S>>,
}

/// Junction conduction with voltage and thermal sensitivities.
#[derive(Clone, Copy, Debug)]
pub struct ChargeCurrent<S = f64> {
    /// Junction DC current, A.
    pub value: S,
    /// Junction differential conductance, S.
    pub d_voltage: S,
    /// Temperature coefficient, A/K.
    pub d_temperature: S,
}

/// ODE physics exposed as charge and residual terms; time integration belongs
/// to the solver.
///
/// The additional node coordinate `qp` has stored charge `Qs=TT(T)*qp`.
/// The charge-based state equation is
/// `dQs/dt + (qp - TT(T)*Id)/VP = 0`.
/// In the physical body-diode direction (source to drain for N-channel,
/// drain to source for P-channel), the current contribution is
/// `polarity*(1-VP)/TT(T) * dQs/dt`, including temperature-sensitive TT.
/// A drain-to-source channel-current convention has the opposite sign.
#[derive(Clone, Copy, Debug)]
pub struct Recovery<S = f64> {
    /// Algebraic residual excluding the time derivative of the stored charge.
    pub residual: S,
    /// Jacobian of the algebraic residual with respect to the Qp node coordinate.
    pub d_node: S,
    /// Jacobian of the residual with respect to physical diode junction voltage.
    pub d_voltage: S,
    /// Jacobian of the residual with respect to Kelvin temperature.
    pub d_temperature: S,
    /// Charge on the independent recovery node; `d_voltage` here means dQs/dqp.
    pub stored_charge: Charge<S>,
    /// Physical diode current coefficient on dQs/dt.
    pub diode_charge_rate_gain: S,
    /// Temperature derivative of the diode charge-rate coefficient.
    pub d_gain_d_temperature: S,
}

/// Power entering junction thermal node; the simulator stamps the heat source.
#[derive(Clone, Copy, Debug)]
pub struct Heat<S = f64> {
    /// Intrinsic channel conduction power, W.
    pub channel: S,
    /// Body diode conduction power, W.
    pub diode: S,
    /// Total intrinsic power, W, excluding separately bound ohmic branch losses.
    pub intrinsic: S,
    /// Partial derivative dP/dVgs, A.
    pub d_vgs: S,
    /// Partial derivative dP/dVds, A.
    pub d_vds: S,
    /// Partial derivative dP/dVdiode, A.
    pub d_vdiode: S,
    /// Partial derivative dP/dT, W/K.
    pub d_temperature: S,
    /// Junction thermal storage capacitance, J/K.
    pub junction_capacitance: S,
    /// Junction-to-case thermal conductance, W/K.
    pub junction_to_case: S,
    /// Case-to-ambient thermal conductance, W/K.
    pub case_to_ambient: S,
}

/// Explicit, optional series conductances; zero denotes a shorted branch
/// that the solver must eliminate from topology before numerical stamping.
#[derive(Clone, Copy, Debug)]
pub struct SeriesEvaluation<S = f64> {
    /// Drain effective conductance (S).
    pub drain: Conductance<S>,
    /// Source effective conductance (S).
    pub source: Conductance<S>,
    /// Gate effective conductance (S).
    pub gate: Conductance<S>,
    /// Body diode effective conductance (S).
    pub body: Conductance<S>,
    /// Drain-source shunt conductance (S).
    pub shunt: S,
}

/// Ohmic power, optionally including terminal- and temperature-dependent resistance.
#[derive(Clone, Copy, Debug, Default)]
pub struct OhmicPower {
    /// Dissipated power, watts.
    pub value: f64,
    /// Partial derivative of power with respect to the element voltage drop.
    pub d_voltage_drop: f64,
    /// Partial derivative of power with respect to drain-source external voltage.
    pub d_vds_external: f64,
    /// Partial derivative of power with respect to temperature.
    pub d_temperature: f64,
}

/// Temperature- and possibly voltage-dependent conductance.
#[derive(Clone, Copy, Debug, Default)]
pub struct Conductance<S = f64> {
    /// S; None represents a zero-ohm element requiring topology coalescing.
    pub value: Option<S>,
    /// S/V, derivative with respect to physical external drain-source voltage.
    pub d_vds_external: S,
    /// S/K, temperature derivative.
    pub d_temperature: S,
}

impl Conductance {
    /// Evaluate branch dissipation G·V² and derivatives. A shorted/coalesced
    /// branch has zero voltage drop and contributes no heat.
    #[must_use = "power and derivatives contribute to the thermal balance"]
    pub fn ohmic_power(self, voltage_drop: f64) -> OhmicPower {
        let g = self.value.unwrap_or(0.0);
        let v2 = voltage_drop * voltage_drop;
        OhmicPower {
            value: g * v2,
            d_voltage_drop: 2.0 * g * voltage_drop,
            d_vds_external: self.d_vds_external * v2,
            d_temperature: self.d_temperature * v2,
        }
    }
}

impl PreparedModel {
    /// The validated source parameters used for this prepared model.
    /// Reading them belongs to setup/inspection, not to per-device dispatch.
    #[must_use = "the returned reference contains the prepared parameters"]
    pub const fn parameters(&self) -> &Model {
        &self.model
    }

    /// Bind an independent instance with a parallel multiplier.
    ///
    /// # Errors
    ///
    /// Fails on nonpositive or nonfinite instance multiplicity.
    pub fn bind(&self, instance: Instance) -> Result<PreparedInstance<'_>, ModelError> {
        if !instance.multiplier.is_finite() || instance.multiplier <= 0.0 {
            return Err(ModelError::Invalid("multiplier"));
        }
        Ok(PreparedInstance {
            model: self,
            multiplier: instance.multiplier,
            self_heating: instance.self_heating,
        })
    }
}

impl PreparedInstance<'_> {
    /// Evaluate VDMOS intrinsic thermal and flicker-current noise densities.
    ///
    /// The returned values are branch-local sources; the circuit solver
    /// transforms them to the observation via the AC transfer function.
    ///
    /// # Errors
    ///
    /// Uses the temperature stored in the supplied evaluation, preventing
    /// inconsistent temperature/noise operating points. Returns an error
    /// for nonpositive frequency or nonfinite noise sources.
    pub fn noise_density(
        &self,
        evaluation: &Evaluation,
        frequency: f64,
    ) -> Result<NoiseDensity, ModelError> {
        if !frequency.is_finite()
            || frequency <= 0.0
            || !evaluation.temperature.is_finite()
            || evaluation.temperature <= 0.0
        {
            return Err(ModelError::Invalid("noise frequency/temperature"));
        }
        let thermal = 4.0 * BOLTZ * evaluation.temperature;
        let drain = thermal * evaluation.series.drain.value.unwrap_or(0.0);
        let source = thermal * evaluation.series.source.value.unwrap_or(0.0);
        let channel = thermal * (2.0 / 3.0) * evaluation.channel.d_vgs.abs();
        let noise = self.model.model.noise;
        let flicker = if noise.kf == 0.0 {
            0.0
        } else {
            const COX_SQUARED: f64 =
                (3.9 * 8.854_214_871e-12 / 1e-7) * (3.9 * 8.854_214_871e-12 / 1e-7);
            noise.kf * evaluation.channel.value.abs().max(1e-38).powf(noise.af)
                / (frequency * COX_SQUARED)
        };
        let result = NoiseDensity {
            drain,
            source,
            channel,
            flicker,
        };
        if ![drain, source, channel, flicker]
            .iter()
            .all(|v| v.is_finite())
        {
            return Err(ModelError::Invalid("nonfinite noise density"));
        }
        Ok(result)
    }

    /// Evaluate the complete supported constitutive laws, with analytic
    /// first derivatives. No internal state is mutated or integrated.
    ///
    /// # Errors
    ///
    /// Fails on invalid temperature/bias or a singular/nonfinite model state.
    pub fn evaluate(&self, bias: Bias) -> Result<Evaluation, ModelError> {
        self.evaluate_physical(bias)
    }

    pub(crate) fn evaluate_physical<S: Scalar>(
        &self,
        bias: Bias<S>,
    ) -> Result<Evaluation<S>, ModelError> {
        if ![
            bias.vgs,
            bias.vds,
            bias.vds_external,
            bias.vdiode,
            bias.recovery_node,
            bias.temperature,
        ]
        .into_iter()
        .all(Scalar::is_finite)
            || bias.temperature.value() <= 0.0
        {
            return Err(ModelError::Invalid("bias"));
        }
        let m = &self.model.model;
        let polarity = m.polarity.sign();
        let t = Jet::temperature(bias.temperature);
        let dt = t - m.tnom;
        let relative = t / m.tnom;

        let ids = channel_at(m, self.multiplier, bias, t)?;
        let qgs = Charge {
            value: bias.vgs.scale(m.gate.cgs * self.multiplier),
            d_voltage: S::constant(m.gate.cgs * self.multiplier),
            d_temperature: S::constant(0.0),
        };
        let normalized_vgd = (bias.vgs - bias.vds).scale(polarity);
        let (qgd_normal, cgd) = gate_charge(normalized_vgd, m.gate, self.multiplier);
        let qgd = Charge {
            value: qgd_normal.scale(polarity),
            d_voltage: cgd,
            d_temperature: S::constant(0.0),
        };

        let vd = Jet::voltage(bias.vdiode) * polarity;
        let diode = diode_at(self.model, self.multiplier, t, vd, bias.recovery_node)?;

        let series = series_at(
            m,
            self.multiplier,
            relative,
            dt,
            bias.vds_external,
            bias.vds,
            polarity,
        )?;
        let channel_power = ids * Jet::drain(bias.vds);
        let diode_power = diode.power;
        let intrinsic = channel_power + diode_power;
        let heat = Heat {
            channel: channel_power.v,
            diode: diode_power.v,
            intrinsic: intrinsic.v,
            d_vgs: channel_power.dx,
            d_vds: channel_power.dy,
            d_vdiode: diode_power.dx,
            d_temperature: intrinsic.dt,
            junction_capacitance: S::constant(if self.self_heating {
                m.thermal.cthj * self.multiplier
            } else {
                0.0
            }),
            junction_to_case: S::constant(if self.self_heating {
                self.multiplier / m.thermal.rthjc
            } else {
                0.0
            }),
            case_to_ambient: S::constant(if self.self_heating {
                self.multiplier / m.thermal.rthca
            } else {
                0.0
            }),
        };
        let result = Evaluation {
            temperature: bias.temperature,
            channel: Current {
                value: ids.v,
                d_vgs: ids.dx,
                d_vds: ids.dy,
                d_temperature: ids.dt,
            },
            qgs,
            qgd,
            diode: diode.evaluation,
            heat,
            series,
        };
        validate_output(&result)?;
        Ok(result)
    }
}

fn validate_output<S: Scalar>(result: &Evaluation<S>) -> Result<(), ModelError> {
    if ![
        result.channel.value,
        result.channel.d_vgs,
        result.channel.d_vds,
        result.channel.d_temperature,
        result.qgd.value,
        result.qgd.d_voltage,
        result.diode.current.value,
        result.diode.current.d_voltage,
        result.diode.current.d_temperature,
        result.diode.depletion.value,
        result.diode.depletion.d_voltage,
        result.heat.intrinsic,
    ]
    .iter()
    .all(|value| value.is_finite())
    {
        return Err(ModelError::Invalid("nonfinite evaluation"));
    }
    Ok(())
}

fn channel_at<S: Scalar>(
    m: &Model,
    multiplier: f64,
    bias: Bias<S>,
    t: Jet<S>,
) -> Result<Jet<S>, ModelError> {
    let dt = t - m.tnom;
    let relative = t / m.tnom;
    let polarity = m.polarity.sign();
    let beta = multiplier * m.channel.kp * relative.powf(m.channel.mu);
    let threshold = m.channel.vth + m.channel.tcvth * dt;
    let slope =
        m.channel.ksubthres * (1.0 + m.channel.tksubthres1 * dt + m.channel.tksubthres2 * dt * dt);
    if slope.v.compare(0.) == Some(Ordering::Less) {
        return Err(ModelError::Invalid("temperature-adjusted ksubthres"));
    }
    let vgs = Jet::gate(bias.vgs) * polarity;
    let vds = Jet::drain(bias.vds) * polarity;
    let mode = if vds.v.compare(0.) == Some(Ordering::Less) {
        -1.0
    } else {
        1.0
    };
    let overdrive = if mode > 0.0 { vgs } else { vgs - vds } - threshold * polarity;
    let vdsat = overdrive.max_zero();
    let gate_drive = (overdrive - m.channel.subshift).scaled_softplus(slope);
    let beta_effective = beta * (1.0 + m.channel.lambda * vds) / (1.0 + m.channel.theta * vdsat);
    let vdss = vds * (mode * m.channel.mtriode);
    let channel_normal = if gate_drive.v.compare_to(vdss.v) != Some(Ordering::Greater) {
        beta_effective * gate_drive * gate_drive * 0.5
    } else {
        beta_effective * vdss * (gate_drive - 0.5 * vdss)
    };
    let ids = channel_normal * (polarity * mode);

    Ok(ids)
}

/// No heap allocation; caller provides independent input and output slots.
///
/// # Errors
///
/// Returns the first failed sample, leaving earlier output slots updated.
pub fn evaluate_batch(
    device: &PreparedInstance<'_>,
    biases: &[Bias],
    out: &mut [Evaluation],
) -> Result<(), ModelError> {
    if biases.len() != out.len() {
        return Err(ModelError::Invalid("batch length"));
    }
    for (bias, slot) in biases.iter().copied().zip(out.iter_mut()) {
        *slot = device.evaluate(bias)?;
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct Jet<S = f64> {
    v: S,
    dx: S,
    dy: S,
    dt: S,
}

impl<S: Scalar> Jet<S> {
    fn constant(v: f64) -> Self {
        Self::constant_value(S::constant(v))
    }
    fn constant_value(v: S) -> Self {
        Self {
            v,
            dx: S::constant(0.),
            dy: S::constant(0.),
            dt: S::constant(0.),
        }
    }
    fn gate(v: S) -> Self {
        Self {
            dx: S::constant(1.),
            ..Self::constant_value(v)
        }
    }
    fn drain(v: S) -> Self {
        Self {
            dy: S::constant(1.),
            ..Self::constant_value(v)
        }
    }
    fn voltage(v: S) -> Self {
        Self::gate(v)
    }
    fn temperature(v: S) -> Self {
        Self {
            dt: S::constant(1.),
            ..Self::constant_value(v)
        }
    }
    fn scale(self, scalar: f64) -> Self {
        Self {
            v: self.v.scale(scalar),
            dx: self.dx.scale(scalar),
            dy: self.dy.scale(scalar),
            dt: self.dt.scale(scalar),
        }
    }
    fn compose(self, value: S, slope: S) -> Self {
        Self {
            v: value,
            dx: self.dx * slope,
            dy: self.dy * slope,
            dt: self.dt * slope,
        }
    }
    fn exp(self) -> Self {
        let value = self.v.exp();
        self.compose(value, value)
    }
    fn ln(self) -> Self {
        self.compose(self.v.ln(), self.v.recip())
    }
    fn powf(self, exponent: f64) -> Self {
        self.compose(
            self.v.powf(exponent),
            self.v.powf(exponent - 1.).scale(exponent),
        )
    }
    fn max_zero(self) -> Self {
        if self.v.compare(0.) == Some(Ordering::Greater) {
            self
        } else {
            Self::constant(0.)
        }
    }
    // Evaluate s*log(1+exp(x/s)) and its partials without forming
    // 1/s² or differentiating a potentially infinite intermediate.
    fn scaled_softplus(self, slope: Self) -> Self {
        if slope.v.value() == 0. {
            // Jointly vanishing scale and argument can have path-dependent
            // partials. Do not assign arbitrary coefficients at that join.
            if !slope.v.is_zero() && self.v.value() == 0. {
                return Self::constant(f64::NAN);
            }
            return self.max_zero();
        }
        let ratio = self.v / slope.v;
        let magnitude = ratio.abs();
        let e = (-magnitude).exp();
        let tail = e.ln_1p();
        let positive = ratio.compare(0.) != Some(Ordering::Less);
        let sigmoid = if positive {
            e.offset(1.).recip()
        } else {
            e / e.offset(1.)
        };
        let ds = if magnitude.value().is_finite() {
            magnitude * e / e.offset(1.) + tail
        } else {
            S::constant(0.)
        };
        let maximum = if self.v.compare(0.) == Some(Ordering::Less) {
            S::constant(0.)
        } else {
            self.v
        };
        Self {
            v: slope.v.mul_add(tail, maximum),
            dx: slope.dx.mul_add(ds, self.dx * sigmoid),
            dy: slope.dy.mul_add(ds, self.dy * sigmoid),
            dt: slope.dt.mul_add(ds, self.dt * sigmoid),
        }
    }
}

impl<S: Scalar> From<f64> for Jet<S> {
    fn from(v: f64) -> Self {
        Self::constant(v)
    }
}
impl<S: Scalar> Add for Jet<S> {
    type Output = Self;
    fn add(self, other: Self) -> Self {
        Self {
            v: self.v + other.v,
            dx: self.dx + other.dx,
            dy: self.dy + other.dy,
            dt: self.dt + other.dt,
        }
    }
}
impl<S: Scalar> Sub for Jet<S> {
    type Output = Self;
    fn sub(self, other: Self) -> Self {
        Self {
            v: self.v - other.v,
            dx: self.dx - other.dx,
            dy: self.dy - other.dy,
            dt: self.dt - other.dt,
        }
    }
}
impl<S: Scalar> Mul for Jet<S> {
    type Output = Self;
    fn mul(self, other: Self) -> Self {
        Self {
            v: self.v * other.v,
            dx: self.v.mul_add(other.dx, self.dx * other.v),
            dy: self.v.mul_add(other.dy, self.dy * other.v),
            dt: self.v.mul_add(other.dt, self.dt * other.v),
        }
    }
}
impl<S: Scalar> Div for Jet<S> {
    type Output = Self;
    fn div(self, other: Self) -> Self {
        let inverse = (other.v * other.v).recip();
        Self {
            v: self.v / other.v,
            dx: self.v.mul_add(-other.dx, self.dx * other.v) * inverse,
            dy: self.v.mul_add(-other.dy, self.dy * other.v) * inverse,
            dt: self.v.mul_add(-other.dt, self.dt * other.v) * inverse,
        }
    }
}
impl<S: Scalar> Neg for Jet<S> {
    type Output = Self;
    fn neg(self) -> Self {
        self.scale(-1.)
    }
}
impl<S: Scalar> Add<f64> for Jet<S> {
    type Output = Self;
    fn add(self, other: f64) -> Self {
        self + Self::constant(other)
    }
}
impl<S: Scalar> Sub<f64> for Jet<S> {
    type Output = Self;
    fn sub(self, other: f64) -> Self {
        self - Self::constant(other)
    }
}
impl<S: Scalar> Mul<f64> for Jet<S> {
    type Output = Self;
    fn mul(self, other: f64) -> Self {
        self.scale(other)
    }
}
impl<S: Scalar> Div<f64> for Jet<S> {
    type Output = Self;
    fn div(self, other: f64) -> Self {
        self.scale(other.recip())
    }
}
impl<S: Scalar> Mul<Jet<S>> for f64 {
    type Output = Jet<S>;
    fn mul(self, other: Jet<S>) -> Jet<S> {
        other.scale(self)
    }
}
impl<S: Scalar> Add<Jet<S>> for f64 {
    type Output = Jet<S>;
    fn add(self, other: Jet<S>) -> Jet<S> {
        Jet::constant(self) + other
    }
}
impl<S: Scalar> Sub<Jet<S>> for f64 {
    type Output = Jet<S>;
    fn sub(self, other: Jet<S>) -> Jet<S> {
        Jet::constant(self) - other
    }
}
impl<S: Scalar> Div<Jet<S>> for f64 {
    type Output = Jet<S>;
    fn div(self, other: Jet<S>) -> Jet<S> {
        Jet::constant(self) / other
    }
}

fn log_cosh<S: Scalar>(v: S) -> S {
    v.abs() + v.abs().scale(-2.).exp().ln_1p() - S::constant(std::f64::consts::LN_2)
}

fn gate_charge<S: Scalar>(v: S, params: GateCharge, mult: f64) -> (S, S) {
    let s = (params.cgdmax - params.cgdmin) / (1.0 + std::f64::consts::FRAC_PI_2);
    let y = params.cgdmax - s;
    let av = v.scale(params.a);
    let positive = av.compare(0.) != Some(Ordering::Less);
    let base = if positive { av.tanh() } else { av.atan() };
    let antiderivative = if params.a == 0.0 {
        S::constant(0.)
    } else if positive {
        log_cosh(av) / S::constant(params.a)
    } else {
        v.mul_add(av.atan(), -(av * av).ln_1p() / S::constant(2. * params.a))
    };
    (
        S::constant(y)
            .mul_add(v, antiderivative.scale(s))
            .scale(mult),
        S::constant(s).mul_add(base, S::constant(y)).scale(mult),
    )
}

struct DiodeAt<S> {
    evaluation: DiodeEvaluation<S>,
    power: Jet<S>,
}

const K_OVER_Q: f64 = 8.617_333_262_145e-5;
const BOLTZ: f64 = 1.380_649e-23;
const CHARGE: f64 = 1.602_176_634e-19;
const REFERENCE_T: f64 = 300.15;

fn silicon_bandgap<S: Scalar>(t: Jet<S>) -> Jet<S> {
    1.16 - 7.02e-4 * t * t / (t + 1108.0)
}

fn pbfact<S: Scalar>(t: Jet<S>) -> Jet<S> {
    let reference = 1.115_087_7 / (BOLTZ * (REFERENCE_T + REFERENCE_T));
    let fact = t / REFERENCE_T;
    let arg = -silicon_bandgap(t) / (2.0 * BOLTZ * t) + reference;
    -2.0 * (K_OVER_Q * t) * (1.5 * fact.ln() + CHARGE * arg)
}

// Stable integral of x^(-m) with respect to voltage, including the
// m=1 logarithmic limit AND its temperature derivative through dm/dT.
fn depletion_primitive<S: Scalar>(x: Jet<S>, one_minus_m: Jet<S>) -> Jet<S> {
    let logarithm = x.ln();
    if one_minus_m.v.abs().compare(1e-4) == Some(Ordering::Less) {
        let z = one_minus_m;
        -logarithm
            - 0.5 * z * logarithm * logarithm
            - (z * z * logarithm * logarithm * logarithm) / 6.0
            - (z * z * z * logarithm * logarithm * logarithm * logarithm) / 24.0
    } else {
        (1.0 - (one_minus_m * logarithm).exp()) / one_minus_m
    }
}

fn depletion_charge<S: Scalar>(
    v: Jet<S>,
    potential: Jet<S>,
    cap_zero: Jet<S>,
    grading: Jet<S>,
    fc: f64,
) -> Jet<S> {
    let fc_v = potential * fc;
    let arg_fc = 1.0 - fc;
    let z = 1.0 - grading;
    let q_fc = potential * cap_zero * depletion_primitive(Jet::constant(arg_fc), z);
    if v.v.compare_to(fc_v.v) == Some(Ordering::Less) {
        let x = 1.0 - v / potential;
        potential * cap_zero * depletion_primitive(x, z)
    } else {
        let factor = cap_zero / ((1.0 + grading) * arg_fc.ln()).exp();
        let f3 = 1.0 - fc * (1.0 + grading);
        q_fc + factor * (f3 * (v - fc_v) + grading / (2.0 * potential) * (v * v - fc_v * fc_v))
    }
}

fn diode_current<S: Scalar>(
    vd: Jet<S>,
    saturation_current: Jet<S>,
    vte: Jet<S>,
    thermal_voltage: Jet<S>,
    avalanche: Option<Breakdown>,
) -> Result<Jet<S>, ModelError> {
    if vd.v.compare_to(vte.v.scale(-3.)) != Some(Ordering::Less) {
        return Ok(saturation_current * ((vd / vte).exp() - 1.0));
    }
    if let Some(parameters) = avalanche {
        let xbv = breakdown::effective_voltage(parameters, saturation_current, thermal_voltage)?;
        if vd.v.compare_to(-xbv.v) == Some(Ordering::Less) {
            return Ok(
                -saturation_current * (-(xbv + vd) / (parameters.nbv * thermal_voltage)).exp()
            );
        }
    }
    let arg = 3.0 * vte / (vd * std::f64::consts::E);
    Ok(-saturation_current * (1.0 + arg * arg * arg))
}

fn diode_at<S: Scalar>(
    prepared: &PreparedModel,
    mult: f64,
    temp: Jet<S>,
    vd: Jet<S>,
    qp: S,
) -> Result<DiodeAt<S>, ModelError> {
    let model = &prepared.model;
    let d = model.diode;
    let ratio = temp / model.tnom;
    let dt = temp - model.tnom;
    let grading = d.m * (1.0 + d.tm1 * dt + d.tm2 * dt * dt);
    let pbo = prepared.pbo;
    let gmaold = (d.vj - pbo) / pbo;
    let potential = pbfact(temp) + ratio * pbo * (model.tnom / REFERENCE_T);
    if potential.v.value() <= 0.0 {
        return Err(ModelError::Invalid("temperature-adjusted diode potential"));
    }
    let gmanew = (potential - pbo) / pbo;
    let cap = mult * d.cjo
        / (1.0 + grading * (400e-6f64.mul_add(model.tnom - REFERENCE_T, -gmaold)))
        * (1.0 + grading * (400e-6 * (temp - REFERENCE_T) - gmanew));
    let thermal_voltage = K_OVER_Q * temp;
    let vte = d.n * thermal_voltage;
    let scale = ((ratio - 1.0) * d.eg / vte + (d.xti / d.n) * ratio.ln()).exp();
    let isat = mult * d.is * scale;
    let current = diode_current(vd, isat, vte, thermal_voltage, d.breakdown)?;
    // ngspice 47 vdmostemp.c limits the *temperature-adjusted* junction
    // potential when FC*VJ exceeds 2.5 V. The zero-bias capacitance is
    // computed using the original potential, before applying this limit.
    // Its subsequent cutoff is 2.5 V (N times the limited potential), but
    // the original FC coefficients are stale and N >= 1 would permit the
    // integral to encounter its pole. Retain the limited potential and use
    // a consistent continuation at that cutoff, bounded by ngspice's own
    // maximum admissible FC. This reproduces the C law below the cutoff
    // while keeping charge and dQ/dV continuous and real above it.
    let (potential, fc) = if potential.v.scale(d.fc).compare(2.5) == Some(Ordering::Greater) {
        (Jet::constant(2.5 / d.n), d.n.min(0.95))
    } else {
        (potential, d.fc)
    };
    let qdep = depletion_charge(vd, potential, cap, grading, fc);
    // Fitted catalog grading exponents can be negative. Admit the real,
    // passive charge law where it is valid, but reject a negative
    // temperature-adjusted capacitance or extrapolated differential C.
    if cap.v.compare(0.) == Some(Ordering::Less)
        || qdep.dx.scale(model.polarity.sign()).compare(0.) == Some(Ordering::Less)
    {
        return Err(ModelError::Invalid("negative diode depletion capacitance"));
    }
    let transit = d.tt * (1.0 + d.ttt1 * dt + d.ttt2 * dt * dt);
    if transit.v.compare(0.) == Some(Ordering::Less) {
        return Err(ModelError::Invalid("temperature-adjusted transit time"));
    }
    let has_recovery = d.vp > 0.0 && transit.v.value() > 0.0;
    let (diffusion, recovery) = if has_recovery {
        let residual = (Jet::constant_value(qp) - transit * current) / d.vp;
        (
            Charge::default(),
            Some(Recovery {
                residual: residual.v,
                d_node: S::constant(1.0 / d.vp),
                d_voltage: residual.dx,
                d_temperature: residual.dt,
                stored_charge: Charge {
                    value: transit.v * qp,
                    d_voltage: transit.v,
                    d_temperature: transit.dt * qp,
                },
                diode_charge_rate_gain: S::constant(model.polarity.sign() * (1.0 - d.vp))
                    / transit.v,
                d_gain_d_temperature: transit.dt.scale(-model.polarity.sign() * (1.0 - d.vp))
                    / (transit.v * transit.v),
            }),
        )
    } else {
        let charge = transit * current;
        (
            Charge {
                value: charge.v.scale(model.polarity.sign()),
                d_voltage: charge.dx.scale(model.polarity.sign()),
                d_temperature: charge.dt.scale(model.polarity.sign()),
            },
            None,
        )
    };
    // vd is polarity-normalized; current and physical diode power are
    // both mapped through the same polarity, leaving I*V invariant.
    Ok(DiodeAt {
        evaluation: DiodeEvaluation {
            current: ChargeCurrent {
                value: current.v.scale(model.polarity.sign()),
                d_voltage: current.dx.scale(model.polarity.sign()),
                d_temperature: current.dt.scale(model.polarity.sign()),
            },
            depletion: Charge {
                value: qdep.v.scale(model.polarity.sign()),
                d_voltage: qdep.dx.scale(model.polarity.sign()),
                d_temperature: qdep.dt.scale(model.polarity.sign()),
            },
            diffusion,
            recovery,
        },
        power: current * vd,
    })
}

fn series_at<S: Scalar>(
    model: &Model,
    mult: f64,
    ratio: Jet<S>,
    dt: Jet<S>,
    vds_external: S,
    intrinsic_vds: S,
    polarity: f64,
) -> Result<SeriesEvaluation<S>, ModelError> {
    let s = model.series;
    let t_rd = s.texp0.map_or_else(
        || 1.0 + s.trd1 * dt + s.trd2 * dt * dt,
        |exp| ratio.powf(exp),
    );
    let drain_r = s.rd * t_rd / mult;
    let mut drain = conductance(drain_r)?;
    if let Some(quasi) = s.quasi_saturation {
        // Upstream uses external normalized Vds only in forward mode.
        let ext = Jet::drain(vds_external) * polarity;
        if intrinsic_vds.scale(polarity).compare(0.) != Some(Ordering::Less) {
            let qres = quasi.rq * ratio.powf(s.texp1) / mult;
            let fraction = ext / (ext + quasi.vq.abs());
            drain = conductance(drain_r + qres * fraction)?;
        }
    }
    let source = conductance(s.rs / mult * (1.0 + s.trs1 * dt + s.trs2 * dt * dt))?;
    let gate = conductance(s.rg / mult * (1.0 + s.trg1 * dt + s.trg2 * dt * dt))?;
    let body =
        conductance(s.rb / mult * (1.0 + model.diode.trb1 * dt + model.diode.trb2 * dt * dt))?;
    Ok(SeriesEvaluation {
        drain,
        source,
        gate,
        body,
        shunt: S::constant(mult / s.rds),
    })
}

fn conductance<S: Scalar>(r: Jet<S>) -> Result<Conductance<S>, ModelError> {
    if r.v.compare(0.) == Some(Ordering::Less) || !r.v.is_finite() {
        return Err(ModelError::Invalid(
            "temperature-adjusted series resistance",
        ));
    }
    if r.v.value() == 0.0 {
        if !r.v.is_zero() {
            return Err(ModelError::Invalid("series topology at Taylor join"));
        }
        return Ok(Conductance::default());
    }
    let g = 1.0 / r;
    Ok(Conductance {
        value: Some(g.v),
        d_vds_external: g.dy,
        d_temperature: g.dt,
    })
}

#[cfg(test)]
mod tests;
