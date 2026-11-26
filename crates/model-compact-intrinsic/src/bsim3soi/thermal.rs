//! Temperature-dependent size prelude of BSIM3SOI FD/DD and PD.
//! Copyright 1999/1990 Regents of the University of California. See package LICENSE.

use super::{EPS_OX, PreparedInstance, PreparedModel};
use crate::{
    ModelError, ModelVariant, MosPolarity,
    preparation::{integer, positive, real, slot},
};
mod channel;
mod channel_threshold;
mod channel_transport;
mod depleted;
pub use channel::{PdChannelEvaluation, PdCurrent, PdLeakage, PreparedPdChannel};
pub use depleted::{
    DepletedBias, DepletedChannelEvaluation, DepletedElectrostaticPoint, DepletedValue,
    PreparedDepletedChannel,
};
mod differential;
mod junction;
mod substrate;
pub use junction::{
    JunctionCharge, JunctionComponents, JunctionCurrent, JunctionEvaluation, PreparedJunctions,
};
pub use substrate::{PreparedSubstrateCharge, SubstrateCharge};

use std::sync::Arc;

const K_OVER_Q: f64 = 8.617_087e-5;
const EPS_SI: f64 = 1.03594e-10;
const CHARGE: f64 = 1.60219e-19;

/// Material quantities at the circuit and nominal temperatures.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TemperatureMaterial {
    /// Actual temperature in kelvin.
    pub kelvin: f64,
    /// Actual-to-nominal temperature ratio.
    pub ratio: f64,
    /// Thermal voltage.
    pub thermal_voltage: f64,
    /// Nominal band gap in electron volts.
    pub nominal_gap: f64,
    /// Actual band gap in electron volts.
    pub gap: f64,
    /// Intrinsic carrier density in inverse cubic centimeters.
    pub intrinsic_density: f64,
}

/// Temperature-adjusted channel and layout-dependent transport coefficients.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transport {
    /// Original UA, UB and UC for self-heating evaluation.
    pub nominal_mobility_coefficients: [f64; 3],
    /// Temperature-adjusted UA, UB and UC.
    pub mobility_coefficients: [f64; 3],
    /// Zero-field mobility in square meters per volt-second.
    pub nominal_mobility: f64,
    /// Mobility at actual temperature.
    pub mobility: f64,
    /// Saturation velocity in meters per second.
    pub saturation_velocity: f64,
    /// Width-dependent series-resistance denominator.
    pub resistance_denominator: f64,
    /// Intrinsic series resistance in ohms.
    pub series_resistance: f64,
    /// Thermal resistance before multiplicity stamping.
    pub thermal_resistance: f64,
    /// Thermal capacitance before multiplicity stamping.
    pub thermal_capacitance: f64,
    /// Internal body resistance.
    pub body_resistance: f64,
    /// PD quantum-oxide factor.
    pub oxide_ratio: Option<f64>,
}

/// Junction saturation densities before nonlinear bias evaluation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct JunctionDensities {
    /// Bipolar injection.
    pub bipolar: f64,
    /// Diffusion.
    pub diffusion: f64,
    /// Recombination.
    pub recombination: f64,
    /// Tunnelling.
    pub tunneling: f64,
    /// PD high-level injection coefficient.
    pub high_level_injection: Option<f64>,
}

/// Size-dependent electrostatic state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Electrostatics {
    /// Channel doping in inverse cubic centimeters.
    pub channel_doping: f64,
    /// Surface potential in volts.
    pub phi: f64,
    /// Zero-bias depletion depth in meters.
    pub depletion_depth: f64,
    /// Built-in potential in volts.
    pub built_in_potential: f64,
    /// Zero-bias depletion capacitance density.
    pub depletion_capacitance: f64,
    /// Characteristic lateral length in meters.
    pub lateral_length: f64,
    /// First-order body coefficient.
    pub k1: f64,
    /// Second-order body coefficient.
    pub k2: f64,
    /// Width-corrected K1 (identical to K1 for FD/DD).
    pub effective_k1: f64,
    /// Body-bias limit.
    pub body_bias_limit: f64,
    /// Front flat-band voltage.
    pub flatband: f64,
    /// Threshold voltage including polarity.
    pub threshold: f64,
    /// Backgate flat-band voltage.
    pub back_flatband: f64,
    /// Source/drain substrate flat-band voltage.
    pub substrate_flatband: f64,
    /// Source/drain substrate threshold voltage.
    pub substrate_threshold: f64,
    /// Minimum substrate capacitance density, instance local.
    pub substrate_minimum_capacitance: f64,
    /// Buried oxide capacitance density.
    pub buried_oxide_capacitance: f64,
    /// Short-channel threshold factor.
    pub short_channel_factor: f64,
    /// Output resistance DIBL coefficient.
    pub output_resistance_factor: f64,
}

/// Physical coefficients prepared from the size and temperature laws.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PhysicalTemperature {
    /// Temperature and material state.
    pub material: TemperatureMaterial,
    /// Channel and thermal transport.
    pub transport: Transport,
    /// Junction saturation densities.
    pub junctions: JunctionDensities,
    /// Bias-local junction and parasitic bipolar current evaluator.
    pub junction_currents: PreparedJunctions,
    /// Channel transport evaluator for the partially depleted family.
    pub pd_channel: Option<PreparedPdChannel>,
    /// Coupled depletion electrostatics and channel transport for the FD/DD families.
    pub depleted_channel: Option<PreparedDepletedChannel>,
    /// Electrostatic state.
    pub electrostatics: Electrostatics,
    /// Drain/source gate overlap and backgate overlap capacitances, before M.
    pub overlaps: [f64; 3],
    /// Drain/source extrinsic substrate-sidewall capacitance, before M.
    pub substrate_sidewall: [f64; 2],
    /// External drain/source series conductances, before M.
    pub series_conductances: [f64; 2],
    /// External body resistance.
    pub external_body_resistance: f64,
    /// Extrinsic drain/source-to-substrate charge laws.
    pub substrate_charge: PreparedSubstrateCharge,
}

struct Context<'a> {
    model: &'a PreparedModel,
    instance: &'a PreparedInstance,
    material: TemperatureMaterial,
}

impl Context<'_> {
    fn model(&self, name: &str) -> Result<f64, ModelError> {
        real(self.model.parameters(), name)
    }
    fn instance(&self, name: &str) -> Result<f64, ModelError> {
        real(self.instance.parameters(), name)
    }
    fn binned(&self, name: &str) -> Result<f64, ModelError> {
        let id = slot(self.model.parameters(), name)?;
        self.instance
            .raw_binned_slot(id)
            .map_or_else(|| self.model(name), |raw| Ok(raw.prelude_value()))
    }
    fn given(&self, name: &str) -> Result<bool, ModelError> {
        Ok(self
            .model
            .explicitly_given(slot(self.model.parameters(), name)?))
    }
    fn pd(&self) -> bool {
        self.model.parameters().variant() == ModelVariant::Bsim3SoiPd
    }
    fn checked(&self) -> Result<bool, ModelError> {
        Ok(integer(self.model.parameters(), "paramchk")? == 1)
    }
    fn sign(&self) -> f64 {
        if self.model.polarity() == MosPolarity::Nmos {
            1.0
        } else {
            -1.0
        }
    }
}

const fn finite(v: f64, name: &'static str) -> Result<f64, ModelError> {
    if v.is_finite() {
        Ok(v)
    } else {
        Err(ModelError::InvalidPhysicalParameter(name))
    }
}

fn bandgap(t: f64) -> f64 {
    (7.02e-4 * t / (t + 1108.0)).mul_add(-t, 1.16)
}

fn material(nominal: f64, kelvin: f64) -> Result<TemperatureMaterial, ModelError> {
    let kelvin = positive(kelvin, "device temperature")?;
    let ratio = positive(kelvin / nominal, "temperature ratio")?;
    let thermal_voltage = K_OVER_Q * kelvin;
    let gap = bandgap(kelvin);
    let tr = kelvin / 300.15;
    let intrinsic_density = positive(
        1.45e10 * tr * tr.sqrt() * (21.556_598_1 - gap / (thermal_voltage + thermal_voltage)).exp(),
        "intrinsic density",
    )?;
    Ok(TemperatureMaterial {
        kelvin,
        ratio,
        thermal_voltage,
        nominal_gap: bandgap(nominal),
        gap,
        intrinsic_density,
    })
}

fn thermal_layout(c: &Context<'_>) -> Result<[f64; 3], ModelError> {
    let g = c.instance.geometry();
    let body = c.model("rbody")?;
    let (rth, cth, rbody) = if c.pd() {
        let width = positive(g.width_m + c.model("wth0")?, "thermal width")?;
        let segments = positive(c.instance("nseg")?, "nseg")?;
        let halo = c.model("rhalo")?;
        let resistance = if body == 0.0 {
            0.0
        } else {
            c.instance("frbody")? * body * halo / halo.mul_add(g.length_m, body + body) * g.width_m
                / segments
        };
        (
            c.instance("rth0")? / width * segments,
            c.instance("cth0")? * width / segments,
            resistance,
        )
    } else {
        let film = positive(c.model("tsi")?, "tsi")?;
        (
            c.instance("rth0")? * (positive(c.model("tbox")?, "tbox")? / film).sqrt() / g.width_m,
            c.instance("cth0")? * film,
            body * g.width_m / g.length_m,
        )
    };
    Ok([
        finite(rth, "rth")?,
        finite(cth, "cth")?,
        finite(rbody, "rbody")?,
    ])
}

fn transport(c: &Context<'_>) -> Result<Transport, ModelError> {
    let delta = c.material.ratio - 1.0;
    let nominal = [c.binned("ua")?, c.binned("ub")?, c.binned("uc")?];
    let adjusted = [
        c.binned("ua1")?.mul_add(delta, nominal[0]),
        c.binned("ub1")?.mul_add(delta, nominal[1]),
        c.binned("uc1")?.mul_add(delta, nominal[2]),
    ];
    let u0 = c.binned("u0")?;
    let nominal_mobility = if u0 > 1.0 { u0 * 1e-4 } else { u0 };
    let mobility = positive(
        nominal_mobility * c.material.ratio.powf(c.binned("ute")?),
        "mobility",
    )?;
    let saturation_velocity = positive(c.binned("at")?.mul_add(-delta, c.binned("vsat")?), "vsat")?;
    let resistance_denominator = positive(
        (c.instance.geometry().width_m * 1e6).powf(c.binned("wr")?),
        "rds denominator",
    )?;
    let rdsw = c.binned("rdsw")?;
    let raw = c.binned("prt")?.mul_add(delta, rdsw) / resistance_denominator;
    let series_resistance = if c.checked()? && (rdsw < 0.0 || (raw > 0.0 && raw < 0.001)) {
        0.0
    } else {
        finite(raw, "rds0")?
    };
    let [thermal_resistance, thermal_capacitance, body_resistance] = thermal_layout(c)?;
    let oxide_ratio = if c.pd() {
        let tox = positive(c.model("toxqm")?, "toxqm")?;
        Some(finite(
            (c.model("toxref")? / tox).powf(c.model("ntox")?) / tox / tox,
            "oxide ratio",
        )?)
    } else {
        None
    };
    for value in adjusted {
        finite(value, "mobility coefficient")?;
    }
    Ok(Transport {
        nominal_mobility_coefficients: nominal,
        mobility_coefficients: adjusted,
        nominal_mobility,
        mobility,
        saturation_velocity,
        resistance_denominator,
        series_resistance,
        thermal_resistance,
        thermal_capacitance,
        body_resistance,
        oxide_ratio,
    })
}

struct Substrate {
    back_flatband: f64,
    flatband: f64,
    threshold: f64,
    minimum_capacitance: f64,
    oxide_capacitance: f64,
}

fn substrate(c: &Context<'_>, doping: f64) -> Result<Substrate, ModelError> {
    let ni = c.material.intrinsic_density;
    let vt = c.material.thermal_voltage;
    let nsub = c.binned("nsub")?;
    let abs_nsub = positive(nsub.abs(), "nsub")?;
    let sign = c.sign();
    let cbox = EPS_OX / positive(c.model("tbox")?, "tbox")?;
    let back_flatband = -sign
        * vt
        * if nsub > 0.0 {
            (doping / nsub).ln()
        } else {
            (doping * abs_nsub / ni / ni).ln()
        };
    let flatband = if c.given("vsdfb")? {
        c.binned("vsdfb")?
    } else if nsub > 0.0 {
        -sign * (vt.mul_add((1e20 * nsub / ni / ni).ln(), -0.3))
    } else {
        -sign * vt.mul_add((1e20 / abs_nsub).ln(), 0.3)
    };
    let phi = positive(2.0 * vt * (abs_nsub / ni).ln(), "substrate phi")?;
    let gamma = 5.753e-12 * abs_nsub.sqrt() / cbox;
    let threshold = if c.given("vsdth")? {
        c.binned("vsdth")?
    } else {
        (sign * nsub.signum()).mul_add(gamma.mul_add(phi.sqrt(), phi), flatband)
    };
    let minimum_capacitance = if c.given("csdmin")? {
        c.model("csdmin")?
    } else {
        let depth = (2.0 * EPS_SI * phi / (CHARGE * abs_nsub * 1e6)).sqrt();
        let cdep = EPS_SI / depth;
        cdep * cbox / (cdep + cbox)
    };
    Ok(Substrate {
        back_flatband: finite(back_flatband, "vfbb")?,
        flatband: finite(flatband, "vsdfb")?,
        threshold: finite(threshold, "vsdth")?,
        minimum_capacitance: finite(minimum_capacitance, "csdmin")?,
        oxide_capacitance: cbox,
    })
}

fn body_coefficients(c: &Context<'_>, doping: f64, phi: f64) -> Result<[f64; 3], ModelError> {
    let mut vbm = c.binned("vbm")?;
    let (k1, k2) = if c.given("k1")? || c.given("k2")? {
        (
            if c.given("k1")? {
                c.binned("k1")?
            } else {
                0.53
            },
            if c.given("k2")? {
                c.binned("k2")?
            } else {
                -0.0186
            },
        )
    } else {
        let vbx = if c.given("vbx")? {
            c.binned("vbx")?
        } else {
            (7.7348e-4 * doping * c.binned("xt")?).mul_add(-c.binned("xt")?, phi)
        };
        let vbx = -vbx.abs();
        vbm = -vbm.abs();
        let cox = c.model.oxide_capacitance_f_per_m2();
        let gamma1 = if c.given("gamma1")? {
            c.binned("gamma1")?
        } else {
            5.753e-12 * doping.sqrt() / cox
        };
        let gamma2 = if c.given("gamma2")? {
            c.binned("gamma2")?
        } else {
            5.753e-12 * positive(c.binned("nsub")?, "gamma2 substrate doping")?.sqrt() / cox
        };
        let d1 = (phi - vbx).sqrt() - phi.sqrt();
        let d2 = (phi * (phi - vbm)).sqrt() - phi;
        let k2 = (gamma1 - gamma2) * d1 / 2.0_f64.mul_add(d2, vbm);
        ((-2.0 * k2).mul_add((phi - vbm).sqrt(), gamma2), k2)
    };
    let limit = if k2 < 0.0 {
        let t = 0.5 * k1 / k2;
        (0.9 * t.mul_add(-t, phi)).clamp(-30.0, -3.0)
    } else {
        -30.0
    };
    Ok([
        finite(k1, "k1")?,
        finite(k2, "k2")?,
        finite(limit.min(vbm), "vbsc")?,
    ])
}

fn electrostatics(c: &Context<'_>) -> Result<Electrostatics, ModelError> {
    let cox = c.model.oxide_capacitance_f_per_m2();
    let doping = positive(
        if !c.given("nch")? && c.given("gamma1")? {
            let g = c.binned("gamma1")? * cox;
            3.021e22 * g * g
        } else {
            c.binned("nch")?
        },
        "channel doping",
    )?;
    let vt = c.material.thermal_voltage;
    let ni = c.material.intrinsic_density;
    let phi = positive(2.0 * vt * (doping / ni).ln(), "channel phi")?;
    let charge_density = CHARGE * doping * 1e6;
    let depletion_depth = (2.0 * EPS_SI * phi / charge_density).sqrt();
    let depletion_capacitance = (charge_density * EPS_SI / (2.0 * phi)).sqrt();
    let tox = positive(c.model("tox")?, "tox")?;
    let lateral_length = (3.0 * c.model("xj")? * tox).sqrt();
    let [k1, k2, body_bias_limit] = body_coefficients(c, doping, phi)?;
    let width = c.instance.geometry().width_m;
    let effective_k1 = if c.pd() {
        k1 * (1.0 + c.binned("k1w1")? / (width + c.binned("k1w2")?).max(1e-8))
    } else {
        k1
    };
    let threshold_offset = effective_k1.mul_add(phi.sqrt(), phi);
    let (flatband, threshold) = if c.given("vth0")? {
        let vth = c.binned("vth0")?;
        (c.sign().mul_add(vth, -threshold_offset), vth)
    } else {
        (-1.0, c.sign() * (threshold_offset - 1.0))
    };
    let characteristic = (EPS_SI / EPS_OX * tox * depletion_depth).sqrt();
    let factor = |name| -> Result<f64, ModelError> {
        let e = (-0.5 * c.binned(name)? * c.instance.geometry().length_m / characteristic).exp();
        finite((e + e).mul_add(e, e), "short-channel factor")
    };
    let sub = substrate(c, doping)?;
    Ok(Electrostatics {
        channel_doping: doping,
        phi,
        depletion_depth: positive(depletion_depth, "Xdep0")?,
        built_in_potential: finite(vt * (1e20 * doping / (ni * ni)).ln(), "vbi")?,
        depletion_capacitance: positive(depletion_capacitance, "cdep0")?,
        lateral_length: positive(lateral_length, "litl")?,
        k1,
        k2,
        effective_k1: finite(effective_k1, "k1eff")?,
        body_bias_limit,
        flatband: finite(flatband, "vfb")?,
        threshold: finite(threshold, "vth0")?,
        back_flatband: sub.back_flatband,
        substrate_flatband: sub.flatband,
        substrate_threshold: sub.threshold,
        substrate_minimum_capacitance: sub.minimum_capacitance,
        buried_oxide_capacitance: sub.oxide_capacitance,
        short_channel_factor: factor("dsub")?,
        output_resistance_factor: finite(
            c.binned("pdiblc1")?
                .mul_add(factor("drout")?, c.binned("pdiblc2")?),
            "thetaRout",
        )?,
    })
}

fn overlaps(c: &Context<'_>) -> Result<[f64; 3], ModelError> {
    let g = c.instance.geometry();
    let (dw, sw) = g.pd.map_or((g.cv_width_m, g.cv_width_m), |p| {
        (p.drain_diode_cv_width, p.source_diode_cv_width)
    });
    let cf = c.binned("cf")?;
    let checked = c.checked()?;
    let overlap = |name, width| -> Result<f64, ModelError> {
        let base = c.model(name)?;
        finite(
            ((if checked { base.max(0.0) } else { base }) + cf) * width,
            "overlap",
        )
    };
    Ok([
        overlap("cgdo", dw)?,
        overlap("cgso", sw)?,
        finite(c.model("cgeo")? * g.cv_length_m, "cgeo")?,
    ])
}

fn extrinsic(c: &Context<'_>) -> Result<([f64; 2], [f64; 2], f64), ModelError> {
    let width = if c.pd() {
        c.instance("w")?
    } else {
        c.instance.geometry().width_m
    };
    let scale = c.model("csdesw")? * (c.model("tsi")? / c.model("tbox")?).ln_1p();
    let sidewall = |name| -> Result<f64, ModelError> {
        finite(
            scale * (c.instance(name)? - width).max(0.0),
            "substrate sidewall",
        )
    };
    let sheet = c.model("rsh")?;
    let conductance = |name| -> Result<f64, ModelError> {
        let r = sheet * c.instance(name)?;
        if r > 0.0 {
            finite(r.recip(), "series conductance")
        } else {
            Ok(0.0)
        }
    };
    Ok((
        [sidewall("pd")?, sidewall("ps")?],
        [conductance("nrd")?, conductance("nrs")?],
        finite(
            c.instance("nrb")? * c.model("rbsh")?,
            "external body resistance",
        )?,
    ))
}

/// Complete size-dependent temperature prelude without evaluating terminal bias.
///
/// # Errors
/// Model/instance identity mismatch, nonpositive temperature or geometry,
/// or invalid/nonfinite physical coefficients.
pub fn prepare_temperature(
    model: &PreparedModel,
    instance: &PreparedInstance,
    kelvin: f64,
) -> Result<PhysicalTemperature, ModelError> {
    if !Arc::ptr_eq(&model.identity, &instance.model_identity) {
        return Err(ModelError::MismatchedPreparedContext);
    }
    let material = material(real(model.parameters(), "tnom")?, kelvin)?;
    let c = Context {
        model,
        instance,
        material,
    };
    let (substrate_sidewall, series_conductances, external_body_resistance) = extrinsic(&c)?;
    let electrostatics = electrostatics(&c)?;
    let substrate_charge = substrate::prepare(&c, electrostatics, substrate_sidewall)?;
    let density = junction::DensityLaw::prepare(&c)?;
    let junction_currents = junction::prepare(&c, density)?;
    let transport = transport(&c)?;
    let pd_channel = if c.pd() {
        Some(PreparedPdChannel::prepare(&c, electrostatics, transport)?)
    } else {
        None
    };
    let depleted_channel = if c.pd() {
        None
    } else {
        Some(PreparedDepletedChannel::prepare(
            &c,
            electrostatics,
            transport,
        )?)
    };
    Ok(PhysicalTemperature {
        material,
        transport,
        junctions: density.ambient(&c)?,
        junction_currents,
        pd_channel,
        depleted_channel,
        electrostatics,
        substrate_charge,
        overlaps: overlaps(&c)?,
        substrate_sidewall,
        series_conductances,
        external_body_resistance,
    })
}
