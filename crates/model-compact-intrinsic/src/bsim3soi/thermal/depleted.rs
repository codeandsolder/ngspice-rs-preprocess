//! FD/DD 2.1 coupled body electrostatics and channel transport.
//! Copyright 1999 Regents of the University of California. See package LICENSE.

use super::{
    CHARGE, Context, EPS_SI, Electrostatics, K_OVER_Q, Transport,
    channel::intrinsic_density,
    channel_threshold::{
        Threshold, ThresholdBias, ThresholdGeometry, ThresholdPoint, overdrive, poly_depletion,
    },
    channel_transport::{
        BulkParameters, ChannelTransport, TransportInput, effective_bulk, one_plus_nine,
    },
    differential::Differential,
};
use crate::{ModelError, ModelVariant, preparation::positive};

mod leakage;
pub use leakage::DepletedLeakage;
use leakage::Leakage;

type D = Differential<6>;

/// Source-oriented, polarity-normalized terminal voltages and trial temperature.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DepletedBias {
    /// Normalized gate-to-source voltage.
    pub vgs: f64,
    /// Nonnegative normalized drain-to-source voltage.
    pub vds: f64,
    /// Normalized internal body-to-source voltage.
    pub vbs: f64,
    /// Normalized backgate-to-source voltage.
    pub ves: f64,
    /// Normalized body-contact-to-source voltage.
    pub vps: f64,
    /// Absolute trial device temperature.
    pub kelvin: f64,
}

/// A bias-local scalar and its analytic derivatives with respect to
/// normalized VGS, VDS, VBS, VES, VPS and absolute temperature, in that order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DepletedValue {
    /// Scalar value.
    pub value: f64,
    /// Gradient in the declared bias order.
    pub gradient: [f64; 6],
}

impl DepletedValue {
    fn from_d(d: D) -> Result<Self, ModelError> {
        if !d.finite() {
            return Err(ModelError::InvalidPhysicalParameter(
                "FD/DD constitutive evaluation",
            ));
        }
        Ok(Self {
            value: d.value,
            gradient: d.gradient,
        })
    }
}

/// Bias-dependent depletion state of FD/DD, prior to transport and charge.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DepletedElectrostaticPoint {
    /// Poly-depletion-corrected gate voltage.
    pub gate_effective: DepletedValue,
    /// Threshold evaluated at the fully depleted equilibrium.
    pub depleted_threshold: DepletedValue,
    /// Gate-dependent fully depleted equilibrium body voltage.
    pub equilibrium_body: DepletedValue,
    /// Smoothed internal diode voltage (FD equals the equilibrium).
    pub diode_body: DepletedValue,
    /// Effective channel body potential.
    pub effective_body: DepletedValue,
    /// Body/contact potential difference, after physical limiting.
    pub body_contact: DepletedValue,
    /// Channel/body coupling voltage.
    pub coupling: DepletedValue,
    /// Effective front threshold.
    pub threshold: DepletedValue,
    /// Effective gate overdrive.
    pub overdrive: DepletedValue,
    /// Depletion depth, in meters.
    pub depletion_depth: DepletedValue,
}

/// FD/DD intrinsic channel transport and complete depletion state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DepletedChannelEvaluation {
    /// Coupled frontgate/backgate electrostatics.
    pub electrostatics: DepletedElectrostaticPoint,
    /// Normalized intrinsic channel current in amperes, before M.
    pub current: DepletedValue,
    /// Effective channel width in meters.
    pub width: DepletedValue,
    /// Effective mobility in square meters per volt-second.
    pub mobility: DepletedValue,
    /// Saturation drain voltage.
    pub saturation_voltage: DepletedValue,
    /// Effective drain voltage.
    pub effective_drain: DepletedValue,
    /// Bias-dependent intrinsic resistance in ohms.
    pub resistance: DepletedValue,
    /// Effective bulk factor including FD/DD body coupling.
    pub bulk_factor: DepletedValue,
    /// Family-specific leakage and body-contact transport.
    pub leakage: DepletedLeakage,
}

/// FD/DD model-specific silicon-film and backgate coupling coefficients.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Film {
    cap: f64,
    charge: f64,
    effective_cap: f64,
    effective_charge: f64,
    box_cap: f64,
}

impl Film {
    fn prepare(c: &Context<'_>, e: Electrostatics) -> Result<Self, ModelError> {
        // Setup uses MODEL doping and VBSA, not the binned values. This is
        // distinct from the instance-specific channel electrostatic prelude.
        let doping = positive(c.model("nch")?, "film doping")?;
        let film = positive(c.model("tsi")?, "tsi")?;
        let charge_density = CHARGE * doping * 1e6;
        let correction = 2.0 * EPS_SI * c.model("vbsa")? / charge_density;
        let square = film * film;
        let effective = if square < correction {
            film
        } else {
            positive((square - correction).sqrt(), "effective film thickness")?
        };
        Ok(Self {
            cap: EPS_SI / film,
            charge: charge_density * film,
            effective_cap: EPS_SI / effective,
            effective_charge: charge_density * effective,
            box_cap: e.buried_oxide_capacitance,
        })
    }
}

/// FD/DD accumulation/depletion mixing law, not the PD bulk equation.
#[derive(Clone, Copy, Debug, PartialEq)]
struct DepletedBulk {
    common: BulkParameters,
    abp: f64,
    mxc: f64,
    adice0: f64,
}

impl DepletedBulk {
    fn prepare(c: &Context<'_>) -> Result<Self, ModelError> {
        Ok(Self {
            common: BulkParameters::prepare(c)?,
            abp: c.binned("abp")?,
            mxc: c.binned("mxc")?,
            adice0: c.model("adice0")?,
        })
    }

    fn evaluate(
        &self,
        prepared: &PreparedDepletedChannel,
        bias: &Trial,
        body: &InternalBody,
        point: &ThresholdPoint<6>,
        drive: D,
    ) -> D {
        let raw = if self.common.a0 == 0.0 {
            D::constant(0.0)
        } else {
            self.common
                .base(&prepared.geometry, point.depth, drive)
                .scale(0.5 * prepared.electrostatics.k1 / bias.phi.sqrt())
        };
        let abulk = D::constant(1.0)
            + effective_bulk(raw) / one_plus_nine(body.effective.scale(self.common.keta));
        let coupling = body.coupling / (drive + bias.thermal.scale(2.0)).scale(self.abp);
        let mix = D::constant(1.0) - smooth_positive(D::constant(0.8) - coupling, 0.04);
        let x = (mix * mix).scale(self.mxc) + mix.scale(1.0 - self.mxc);
        let cbox = prepared.film.box_cap;
        let csi = prepared.film.cap;
        let adice = self.adice0 / (1.0 + cbox * csi / (cbox + csi) / prepared.geometry.oxide);
        x * abulk + (D::constant(1.0) - x).scale(adice)
    }
}

/// Prepared fully/dynamically depleted channel electrostatics.
///
/// FD and DD share the source's coupled backgate threshold equations, but
/// retain different internal diode-body potential laws.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreparedDepletedChannel {
    electrostatics: Electrostatics,
    threshold: Threshold,
    geometry: ThresholdGeometry,
    film: Film,
    transport: ChannelTransport,
    bulk: DepletedBulk,
    leakage: Leakage,
    fd: bool,
    nsub: f64,
    voff: f64,
    dvbd: [f64; 2],
    kb: [f64; 2],
    vbsa: f64,
    delp: f64,
}

struct Trial {
    gate: D,
    drain: D,
    body: D,
    backgate: D,
    contact: D,
    thermal: D,
    ratio_delta: D,
    built_in: D,
    back_flatband: D,
    phi: f64,
}

struct Equilibrium {
    thermal_body: D,
    body: D,
    threshold: D,
    mos: D,
}

struct InternalBody {
    equilibrium: D,
    diode: D,
    effective: D,
    contact: D,
    coupling: D,
    contact_coupling: D,
}

fn smooth_positive(x: D, square: f64) -> D {
    let root = (x * x + D::constant(square)).sqrt();
    if x.value >= 0.0 {
        (x + root).scale(0.5)
    } else {
        D::constant(0.5 * square) / (root - x)
    }
}

fn limit_upper(value: D, limit: f64, square: f64) -> D {
    D::constant(limit) - smooth_positive(D::constant(limit - 0.005) - value, square)
}

impl PreparedDepletedChannel {
    pub(super) fn prepare(
        c: &Context<'_>,
        electrostatics: Electrostatics,
        transport: Transport,
    ) -> Result<Self, ModelError> {
        Ok(Self {
            electrostatics,
            threshold: Threshold::prepare(c)?,
            geometry: ThresholdGeometry::prepare(c, electrostatics)?,
            film: Film::prepare(c, electrostatics)?,
            transport: ChannelTransport::prepare(c, transport)?,
            bulk: DepletedBulk::prepare(c)?,
            leakage: Leakage::prepare(c, transport)?,
            fd: c.model.parameters().variant() == ModelVariant::Bsim3SoiFd,
            nsub: c.binned("nsub")?,
            voff: c.binned("voff")?,
            dvbd: [c.binned("dvbd0")?, c.binned("dvbd1")?],
            kb: [c.binned("kb1")?, c.binned("kb3")?],
            vbsa: c.binned("vbsa")?,
            delp: c.binned("delp")?,
        })
    }

    /// The initial surface potential at ambient temperature.
    #[must_use]
    pub const fn initial_surface_potential(&self) -> f64 {
        self.electrostatics.phi
    }

    /// Calculate the next accepted predictor surface potential.
    ///
    /// # Errors
    /// Invalid temperature or surface potential.
    pub fn accepted_surface_potential(&self, kelvin: f64) -> Result<f64, ModelError> {
        if !self.transport.temperature.selfheat {
            return Ok(self.electrostatics.phi);
        }
        let temperature = D::constant(positive(kelvin, "accepted temperature")?);
        let thermal = temperature.scale(K_OVER_Q);
        let ni = intrinsic_density(temperature, thermal);
        positive(
            2.0 * thermal.value * (self.electrostatics.channel_doping / ni.value).ln(),
            "accepted phi",
        )
    }

    fn trial(&self, bias: DepletedBias, accepted_phi: f64) -> Result<Trial, ModelError> {
        if ![bias.vgs, bias.vds, bias.vbs, bias.ves, bias.vps]
            .into_iter()
            .all(f64::is_finite)
            || bias.vds < 0.0
        {
            return Err(ModelError::InvalidPhysicalParameter(
                "FD/DD normalized bias",
            ));
        }
        let temperature = if self.transport.temperature.selfheat {
            D::variable(positive(bias.kelvin, "trial temperature")?, 5)
        } else {
            D::constant(self.transport.temperature.ambient)
        };
        let phi = if self.transport.temperature.selfheat {
            positive(accepted_phi, "accepted phi")?
        } else {
            self.electrostatics.phi
        };
        let thermal = temperature.scale(K_OVER_Q);
        let (built_in, back_flatband) = if self.transport.temperature.selfheat {
            let ni = intrinsic_density(temperature, thermal);
            let doping = self.electrostatics.channel_doping;
            let built_in = thermal * (D::constant(1e20 * doping) / (ni * ni)).ln();
            let back = if self.nsub > 0.0 {
                thermal.scale(-self.geometry.sign * (doping / self.nsub).ln())
            } else {
                (D::constant(-doping * self.nsub) / (ni * ni)).ln()
                    * thermal.scale(-self.geometry.sign)
            };
            (built_in, back)
        } else {
            (
                D::constant(self.electrostatics.built_in_potential),
                D::constant(self.electrostatics.back_flatband),
            )
        };
        let gate = poly_depletion(
            D::variable(bias.vgs, 0),
            self.electrostatics.flatband + phi,
            self.threshold.poly_doping(),
            self.geometry.oxide,
        );
        Ok(Trial {
            gate,
            drain: D::variable(bias.vds, 1),
            body: D::variable(bias.vbs, 2),
            backgate: D::variable(bias.ves, 3),
            contact: D::variable(bias.vps, 4),
            thermal,
            ratio_delta: temperature.scale(self.transport.temperature.nominal.recip())
                - D::constant(1.0),
            built_in,
            back_flatband,
            phi,
        })
    }

    fn threshold_at(&self, bias: &Trial, body: D) -> ThresholdPoint<6> {
        self.threshold.evaluate(
            &self.geometry,
            &ThresholdBias {
                phi: bias.phi,
                body,
                drain: bias.drain,
                built_in: bias.built_in,
                ratio_delta: bias.ratio_delta,
                root_adjustment: D::constant(0.0),
            },
            false,
        )
    }

    fn equilibrium(&self, bias: &Trial) -> Equilibrium {
        let exponent = -self.dvbd[1] * self.geometry.length / self.electrostatics.lateral_length;
        let shift = self.dvbd[0] * 2.0_f64.mul_add(exponent.exp(), (0.5 * exponent).exp());
        let thermal_body =
            D::constant(bias.phi - 0.5 * self.film.charge / self.film.cap + self.vbsa)
                + (bias.built_in - D::constant(bias.phi)).scale(shift);
        let coupling = self.kb[0] / (1.0 + self.film.effective_cap / self.film.box_cap);
        let body =
            thermal_body - (thermal_body - bias.backgate + bias.back_flatband).scale(coupling);
        let body = limit_upper(body, bias.phi - self.delp, 0.02);
        let transition =
            smooth_positive(thermal_body - body - D::constant(0.005), 0.005_f64.powi(2));
        let mos = body
            - (transition * transition)
                .scale(0.5 * self.film.effective_cap / self.film.effective_charge);
        let threshold = self.threshold_at(bias, mos).threshold;
        Equilibrium {
            thermal_body,
            body,
            threshold,
            mos,
        }
    }

    fn internal_body(&self, bias: &Trial, eq: &Equilibrium) -> InternalBody {
        let transition = smooth_positive(
            eq.threshold - bias.gate - D::constant(0.02),
            0.02_f64.powi(2),
        );
        let thermal_body = eq.thermal_body - transition;
        let root = (D::constant(bias.phi) - eq.mos).sqrt();
        let k1 = self.electrostatics.k1;
        let nfb = (D::constant(1.0)
            + (D::constant(1.0)
                + (D::constant(bias.phi) + root.scale(k1) - eq.mos).scale(4.0 / (k1 * k1)))
            .sqrt()
            .scale(self.kb[1] * self.film.box_cap / self.geometry.oxide))
        .reciprocal();
        let equilibrium = eq.body - nfb * transition;
        let diode = if self.fd {
            equilibrium
        } else {
            equilibrium
                + D::constant(0.02)
                + smooth_positive(
                    bias.body - equilibrium - D::constant(0.03),
                    0.01_f64.powi(2),
                )
        };
        let transition =
            smooth_positive(thermal_body - diode - D::constant(0.005), 0.005_f64.powi(2));
        let mos = diode
            - (transition * transition)
                .scale(0.5 * self.film.effective_cap / self.film.effective_charge);
        let limit = bias.phi - self.delp;
        let effective = limit_upper(mos, limit, 0.02 * limit);
        let contact_coupling =
            smooth_positive(bias.contact - equilibrium + D::constant(1e-7), 1e-14);
        let contact = equilibrium + contact_coupling;
        InternalBody {
            equilibrium,
            diode,
            effective,
            contact: diode - contact,
            coupling: diode - equilibrium,
            contact_coupling,
        }
    }

    /// Evaluate all coupled front/backgate body and threshold potentials
    /// and intrinsic channel transport.
    ///
    /// Isothermal mode uses the prepared ambient coefficients; self-heated
    /// mode uses a frozen accepted surface potential with trial temperature.
    ///
    /// # Errors
    /// Invalid bias, temperature, surface potential, or nonfinite result.
    pub fn evaluate(
        &self,
        bias: DepletedBias,
        accepted_phi: f64,
    ) -> Result<DepletedChannelEvaluation, ModelError> {
        let trial = self.trial(bias, accepted_phi)?;
        let eq = self.equilibrium(&trial);
        let body = self.internal_body(&trial, &eq);
        let point = self.threshold_at(&trial, body.effective);
        let drive = overdrive(
            trial.gate - point.threshold,
            point.n,
            trial.thermal,
            self.voff,
            self.electrostatics.depletion_capacitance,
            self.geometry.oxide,
            34.0,
        );
        let bulk = self.bulk.evaluate(self, &trial, &body, &point, drive);
        let transport = self.transport.evaluate(
            &self.geometry,
            &TransportInput {
                drain: trial.drain,
                thermal: trial.thermal,
                ratio_delta: trial.ratio_delta,
                root_delta: point.root - D::constant(trial.phi.sqrt()),
                body: body.effective,
                overdrive: drive,
                threshold: point.threshold,
                bulk,
                width_factor: 2.0,
            },
        );
        let electrostatics = DepletedElectrostaticPoint {
            gate_effective: DepletedValue::from_d(trial.gate)?,
            depleted_threshold: DepletedValue::from_d(eq.threshold)?,
            equilibrium_body: DepletedValue::from_d(body.equilibrium)?,
            diode_body: DepletedValue::from_d(body.diode)?,
            effective_body: DepletedValue::from_d(body.effective)?,
            body_contact: DepletedValue::from_d(body.contact)?,
            coupling: DepletedValue::from_d(body.coupling)?,
            threshold: DepletedValue::from_d(point.threshold)?,
            overdrive: DepletedValue::from_d(drive)?,
            depletion_depth: DepletedValue::from_d(point.depth)?,
        };
        Ok(DepletedChannelEvaluation {
            electrostatics,
            current: DepletedValue::from_d(transport.current)?,
            width: DepletedValue::from_d(transport.width)?,
            mobility: DepletedValue::from_d(transport.mobility)?,
            saturation_voltage: DepletedValue::from_d(transport.saturation)?,
            effective_drain: DepletedValue::from_d(transport.effective_drain)?,
            resistance: DepletedValue::from_d(transport.resistance)?,
            bulk_factor: DepletedValue::from_d(bulk)?,
            leakage: self.leakage.evaluate(
                self.fd,
                &self.geometry,
                &trial,
                &body,
                drive,
                &transport,
            )?,
        })
    }
}
