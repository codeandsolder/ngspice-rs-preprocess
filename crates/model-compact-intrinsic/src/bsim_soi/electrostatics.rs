//! Ambient electrostatic preparation and live short-channel coefficients.
//! Copyright 2010 Regents of the University of California. See package LICENSE.

use super::{
    BsimSoiStressOffsets, BsimSoiThermalScalar, CHARGE, EPS_0, EPS_OX, EPS_SI, K_OVER_Q,
    PreparedBsimSoiSetup, PreparedBsimSoiSize, PreparedBsimSoiTemperature, binned, integer,
    positive, real, slot, stress,
};
use crate::{BoundParameters, ModelError, MosPolarity, ParameterScope, PreparedBsimSoiModel};

/// Electrostatic coefficients frozen by the native ambient preparation.
/// K2, threshold and ETA0 include per-instance layout stress offsets.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BsimSoiElectrostaticBase {
    pub k1: f64,
    pub k2: f64,
    pub k1_effective: f64,
    pub k1_oxide: f64,
    pub k2_oxide: f64,
    pub flatband: f64,
    pub threshold: f64,
    pub eta0: f64,
    pub oxide_capacitance: f64,
    pub characteristic_length_factor: f64,
    pub effective_inversion: f64,
    pub effective_inversion_cv: f64,
    pub dits_factor: f64,
    pub debye_length: f64,
    pub mobility_flatband: f64,
}

/// Temperature-dependent short-channel electrostatic factors.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BsimSoiElectrostaticPoint {
    pub theta_body: BsimSoiThermalScalar,
    pub theta_output: BsimSoiThermalScalar,
}

/// Prepared native ambient coefficients and live short-channel temperature law.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreparedBsimSoiElectrostatics {
    base: BsimSoiElectrostaticBase,
    material: PreparedBsimSoiTemperature,
    ambient_scale: f64,
    live_scale: f64,
    body_exponent: f64,
    output_exponent: f64,
    pdibl1: f64,
    pdibl2: f64,
}

fn theta(exponent: f64, scale: f64, scale_slope: f64) -> BsimSoiThermalScalar {
    let argument = exponent / scale;
    let e = argument.exp();
    BsimSoiThermalScalar::new(
        e.mul_add(2. * e, e),
        (4.0_f64.mul_add(e, 1.)) * e * (-argument) * scale_slope / scale,
    )
}

impl PreparedBsimSoiElectrostatics {
    #[must_use = "the base values bind the ambient reference for the channel equations"]
    pub const fn base(&self) -> BsimSoiElectrostaticBase {
        self.base
    }

    /// Evaluate the native DIBL and output-resistance coefficients at a trial temperature.
    ///
    /// # Errors
    /// Invalid material temperature or nonfinite short-channel factor.
    pub fn evaluate(&self, kelvin: f64) -> Result<BsimSoiElectrostaticPoint, ModelError> {
        let point = self.material.evaluate(kelvin)?;
        let live = self.material.network().is_some();
        let oxide_scale = if live {
            self.live_scale
        } else {
            self.ambient_scale
        };
        let depletion = point.depletion_width;
        let scale = positive(
            (oxide_scale * depletion.value).sqrt(),
            "short-channel scale",
        )?;
        let derivative = 0.5 * scale * depletion.d_temperature / depletion.value;
        let body = theta(self.body_exponent, scale, derivative);
        let output = theta(self.output_exponent, scale, derivative);
        Ok(BsimSoiElectrostaticPoint {
            theta_body: body.checked("theta0vb0")?,
            theta_output: BsimSoiThermalScalar::new(
                self.pdibl1.mul_add(output.value, self.pdibl2),
                self.pdibl1 * output.d_temperature,
            )
            .checked("thetaRout")?,
        })
    }
}

#[derive(Clone, Copy)]
struct BodyDerivation {
    phi: f64,
    gamma1: f64,
    gamma2: f64,
    vbx: f64,
    vbm: f64,
}

impl BodyDerivation {
    fn coefficients(self) -> Result<[f64; 2], ModelError> {
        let phi = self.phi;
        let vbx = -self.vbx.abs();
        let vbm = -self.vbm.abs();
        // Use equivalent difference-of-roots expressions to avoid cancellation
        // for small body potentials. The native denominator is negative.
        let sqrt_phi = phi.sqrt();
        let transition_root = (phi - vbx).sqrt();
        let limit_root = (phi - vbm).sqrt();
        let numerator = (self.gamma1 - self.gamma2) * (-vbx) / (transition_root + sqrt_phi);
        let denominator = -(vbm / (limit_root + sqrt_phi)).powi(2);
        if denominator == 0. {
            return Err(ModelError::InvalidPhysicalParameter("body coefficient vbm"));
        }
        let k2 = numerator / denominator;
        let k1 = (-2. * k2).mul_add(limit_root, self.gamma2);
        Ok([k1, k2])
    }
}

impl PreparedBsimSoiSetup {
    /// Bind the ambient electrostatic coefficients and live DIBL factors.
    ///
    /// # Errors
    /// Missing model inputs or invalid electrostatic/material domain.
    pub fn prepare_electrostatics(
        &self,
        index: usize,
        ambient: f64,
    ) -> Result<PreparedBsimSoiElectrostatics, ModelError> {
        let instance = self
            .instances
            .get(index)
            .ok_or(ModelError::InvalidPhysicalParameter(
                "BSIMSOI instance index",
            ))?;
        let size = self
            .sizes
            .get(index)
            .ok_or(ModelError::MismatchedPreparedContext)?;
        let model = self.model.parameters();
        let get = |name| binned(size, name);
        let given = |name| slot(ParameterScope::Model, name).map(|id| self.model.was_given(id));
        let material = self.prepare_temperature(index, ambient)?;
        let point = material.ambient_point()?;
        let phi = point.surface_potential.value;
        let [nch, _] = material.doping();
        let epssub = material.substrate_permittivity();
        let oxide = Oxide::prepare(model, epssub)?;
        let [k1, mut k2] = body_coefficients(&self.model, size, material, oxide, phi)?;
        let geometry = size.geometry();
        let k1eff = k1 * (1. + get("k1w1")? / (geometry.weff + get("k1w2")?).max(1e-8));
        let polarity: f64 = if self.model.polarity() == MosPolarity::Nmos {
            1.
        } else {
            -1.
        };
        let flatband = if given("vfb")? {
            get("vfb")?
        } else if given("vth0")? {
            (-k1eff).mul_add(phi.sqrt(), polarity.mul_add(get("vth0")?, -phi))
        } else {
            -1.
        };
        let mut threshold = if given("vth0")? {
            get("vth0")?
        } else {
            polarity * k1eff.mul_add(phi.sqrt(), flatband + phi)
        };
        let BsimSoiStressOffsets {
            threshold: dvth,
            body_coefficient: dk2,
            dibl: deta,
        } = stress::prepare(model, instance.parameters(), size)?
            .map_or_else(BsimSoiStressOffsets::default, |value| value.offsets);
        let delvto = real(instance.parameters(), "delvto")?;
        k2 += dk2;
        threshold += dvth + delvto;
        let flatband = polarity.mul_add(delvto, flatband);
        let mobility_flatband = ((polarity.mul_add(threshold, -flatband) - phi)
            * if polarity > 0. { 2. } else { 2.5 })
        .max(0.);
        let nominal = positive(real(model, "tnom")?, "tnom")?;
        let inversion = |value: f64| 0.5 + value.atan() / std::f64::consts::PI;
        let base = BsimSoiElectrostaticBase {
            k1,
            k2,
            k1_effective: k1eff,
            k1_oxide: k1eff * oxide.physical / oxide.reference,
            k2_oxide: k2 * oxide.physical / oxide.reference,
            flatband,
            threshold,
            eta0: get("eta0")? + deta,
            oxide_capacitance: oxide.capacitance,
            characteristic_length_factor: oxide.factor,
            effective_inversion: inversion(get("minv")?),
            effective_inversion_cv: inversion(get("minvcv")?),
            dits_factor: get("dvtp2")? / geometry.leff.powf(get("dvtp3")?),
            debye_length: (epssub * K_OVER_Q * nominal / (CHARGE * nch * 1e6)).sqrt() / 3.,
            mobility_flatband,
        };
        base.validate()?;
        Ok(PreparedBsimSoiElectrostatics {
            base,
            material,
            ambient_scale: oxide.ambient_scale,
            live_scale: oxide.live_scale,
            body_exponent: -0.5 * get("dsub")? * geometry.leff,
            output_exponent: -0.5 * get("drout")? * geometry.leff,
            pdibl1: get("pdiblc1")?,
            pdibl2: get("pdiblc2")?,
        })
    }
}

impl BsimSoiElectrostaticBase {
    fn validate(&self) -> Result<(), ModelError> {
        if ![
            self.k1,
            self.k2,
            self.k1_effective,
            self.k1_oxide,
            self.k2_oxide,
            self.flatband,
            self.threshold,
            self.eta0,
            self.oxide_capacitance,
            self.characteristic_length_factor,
            self.effective_inversion,
            self.effective_inversion_cv,
            self.dits_factor,
            self.debye_length,
            self.mobility_flatband,
        ]
        .into_iter()
        .all(f64::is_finite)
        {
            return Err(ModelError::InvalidPhysicalParameter(
                "electrostatic preparation",
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Copy)]
struct Oxide {
    physical: f64,
    reference: f64,
    capacitance: f64,
    ambient_scale: f64,
    live_scale: f64,
    factor: f64,
    generalized: bool,
}
impl Oxide {
    fn prepare(model: &BoundParameters, epssub: f64) -> Result<Self, ModelError> {
        let generalized = integer(model, "mtrlmod")? != 0;
        let physical = positive(real(model, "tox")?, "tox")?;
        let reference = positive(real(model, "toxm")?, "toxm")?;
        let equivalent = if generalized {
            positive(real(model, "eot")?, "eot")?
        } else {
            physical
        };
        let epsrox = if generalized {
            3.9
        } else {
            positive(real(model, "epsrox")?, "epsrox")?
        };
        let capacitance = if generalized {
            epsrox * EPS_0 / equivalent
        } else {
            EPS_OX / physical
        };
        let ambient_scale = if generalized {
            epssub * physical / (epsrox * EPS_0)
        } else {
            EPS_SI * physical / EPS_OX
        };
        let live_scale = epssub * equivalent / (epsrox * EPS_0);
        let factor = (if generalized {
            live_scale
        } else {
            ambient_scale
        })
        .sqrt();
        Ok(Self {
            physical,
            reference,
            capacitance,
            ambient_scale,
            live_scale,
            factor,
            generalized,
        })
    }
}
fn body_coefficients(
    prepared: &PreparedBsimSoiModel,
    size: &PreparedBsimSoiSize,
    material: PreparedBsimSoiTemperature,
    oxide: Oxide,
    phi: f64,
) -> Result<[f64; 2], ModelError> {
    let model = prepared.parameters();
    let get = |name| binned(size, name);
    let given = |name| slot(ParameterScope::Model, name).map(|id| prepared.was_given(id));
    let [nch, nsub] = material.doping();
    let epssub = material.substrate_permittivity();
    let generalized = oxide.generalized;
    let cox = oxide.capacitance;
    Ok(if given("k1")? || given("k2")? {
        [
            if given("k1")? { get("k1")? } else { 0.53 },
            if given("k2")? { get("k2")? } else { -0.0186 },
        ]
    } else {
        let gamma_factor = if generalized {
            (2. * 1e6 * CHARGE * epssub).sqrt()
        } else {
            5.753e-12
        };
        let gamma1 = if given("gamma1")? {
            real(model, "gamma1")?
        } else {
            gamma_factor * nch.sqrt() / cox
        };
        let gamma2 = if given("gamma2")? {
            real(model, "gamma2")?
        } else {
            gamma_factor * positive(nsub, "gamma2 substrate doping")?.sqrt() / cox
        };
        let vbx = if given("vbx")? {
            real(model, "vbx")?
        } else {
            let factor = if generalized {
                CHARGE * 1e6 / (2. * epssub)
            } else {
                7.7348e-4
            };
            (-factor * nch).mul_add(real(model, "xt")?.powi(2), phi)
        };
        BodyDerivation {
            phi,
            gamma1,
            gamma2,
            vbx,
            vbm: real(model, "vbm")?,
        }
        .coefficients()?
    })
}
