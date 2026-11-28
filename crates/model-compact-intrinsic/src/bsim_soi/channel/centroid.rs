//! BSIMSOI CAPMOD=3 charge-thickness model.
//! Copyright 2010 Regents of the University of California. See package LICENSE.
use super::super::{
    BsimSoiTemperaturePoint, CHARGE, EPS_0, EPS_OX, K_OVER_Q, PreparedBsimSoiSize, binned, integer,
    positive, real,
};
use super::{Jet, PreparedBsimSoiChannel, threshold::Threshold};
use crate::{BoundParameters, ModelError};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Centroid {
    oxide: f64,
    cox: f64,
    epssub: f64,
    debye: f64,
    acde: f64,
    moin: f64,
    k1: f64,
    ambient_root: f64,
    ambient_flatband: f64,
    nominal: f64,
    live: bool,
}

#[derive(Clone, Copy)]
pub(super) struct CentroidTrial {
    pub(super) flatband: Jet,
    phi: Jet,
    vtm: Jet,
}
impl Centroid {
    pub(super) fn prepare(
        model: &BoundParameters,
        size: &PreparedBsimSoiSize,
        channel: &PreparedBsimSoiChannel,
        unstressed_threshold: f64,
        oxides: [f64; 2],
    ) -> Result<Self, ModelError> {
        let [oxide, size_oxide] = oxides;
        let generalized = integer(model, "mtrlmod")? != 0;
        let cox = if generalized {
            3.9 * EPS_0 / oxide
        } else {
            EPS_OX / oxide
        };
        let ambient = channel.material.ambient_point()?;
        let phi = ambient.surface_potential.value;
        let base = channel.electrostatic.base();
        let nominal = channel.nominal;
        let ambient_threshold = zero_threshold(
            &channel.threshold,
            ambient,
            size_oxide,
            unstressed_threshold,
            nominal,
        )
        .value;
        let ambient_flatband = base.k1.mul_add(-phi.sqrt(), ambient_threshold - phi);
        let [doping, _] = channel.material.doping();
        let acde = binned(size, "acde")? * (doping / 2e16).powf(-0.25);
        Ok(Self {
            oxide,
            cox,
            epssub: channel.threshold.epssub,
            debye: base.debye_length,
            acde,
            moin: positive(binned(size, "moin")?, "charge thickness MOIN")?,
            k1: channel.threshold.k1ox,
            ambient_root: phi.sqrt(),
            ambient_flatband,
            nominal,
            live: channel.material.network().is_some(),
        })
    }
    pub(super) fn trial(
        self,
        channel: &PreparedBsimSoiChannel,
        material: BsimSoiTemperaturePoint,
        offset: f64,
        ideal: bool,
    ) -> CentroidTrial {
        let phi = Jet::thermal(material.surface_potential);
        let flatband = if ideal {
            Jet::constant(0.)
        } else if self.live {
            zero_threshold(
                &channel.threshold,
                material,
                channel.threshold.toxe,
                channel.threshold.vth0,
                self.nominal,
            ) - phi
                - channel.threshold.k1 * phi.sqrt()
                + offset
        } else {
            Jet::constant(self.ambient_flatband + offset)
        };
        CentroidTrial {
            flatband,
            phi,
            vtm: Jet::thermal(material.thermal_voltage),
        }
    }
    fn oxide_fraction(self, centroid: Jet) -> Jet {
        1. / (1. + self.cox / self.epssub * centroid)
    }
    pub(super) fn accumulation_factor(self, gate: Jet, body: Jet, flatband: Jet) -> Jet {
        let field = (gate - body - flatband) / (1e8 * self.oxide);
        let raw = self.debye * (self.acde * field).bounded_exp();
        let link = 1e-3 * self.oxide;
        // Rationalize Debye - half-positive(Debye - raw - link)
        // in weak accumulation to preserve a small centroid.
        let difference = self.debye - raw - link;
        let root = (difference * difference + 4. * link * self.debye).sqrt();
        let centroid = if difference.value >= 0. {
            2. * self.debye * raw / (self.debye + raw + link + root)
        } else {
            self.debye - difference.half_positive(Jet::constant(4. * link * self.debye))
        };
        self.oxide_fraction(if centroid.value < 1e-15 {
            Jet::constant(1e-15)
        } else {
            centroid
        })
    }
    pub(super) fn inversion_factor(
        self,
        trial: CentroidTrial,
        threshold: Jet,
        overdrive: Jet,
    ) -> Jet {
        let field_shift =
            (4. * (threshold - trial.flatband - trial.phi)).half_positive(Jet::constant(1e-4));
        let field = (overdrive + field_shift) / (2e8 * self.oxide);
        let centroid = 1.9e-9 / (1. + (0.7 * field.ln()).exp());
        self.oxide_fraction(centroid)
    }
    pub(super) fn effective_overdrive(self, trial: CentroidTrial, overdrive: Jet) -> Jet {
        let (denominator, root) = if self.k1 <= 0. {
            (0.25 * self.moin * trial.vtm, 0.5 * self.ambient_root)
        } else {
            (
                self.moin * self.k1 * self.k1 * trial.vtm,
                self.k1 * self.ambient_root,
            )
        };
        let shift = trial.vtm * (overdrive * (2. * root + overdrive) / denominator).ln_1p();
        overdrive - shift
    }
}

fn zero_threshold(
    threshold: &Threshold,
    material: BsimSoiTemperaturePoint,
    oxide: f64,
    base: f64,
    nominal: f64,
) -> Jet {
    let phi = Jet::thermal(material.surface_potential);
    let depletion = Jet::thermal(material.depletion_width);
    let vbi = Jet::thermal(material.built_in_potential);
    let scale = threshold.factor1 * depletion.sqrt();
    let theta = |coefficients: [f64; 3], length: f64| {
        let exponential = (-0.5 * coefficients[1] * length / scale).floor_exp();
        coefficients[0] * exponential * (1. + 2. * exponential)
    };
    let short = theta(threshold.dvt, threshold.length)
        + theta(threshold.dvtw, threshold.length * threshold.width);
    let ratio = Jet::coordinate(
        material.kelvin / nominal - 1.,
        4,
        material.thermal_voltage.d_temperature / crate::bsim_soi::K_OVER_Q / nominal,
    );
    base - short * (vbi - phi)
        + threshold.k3[0] * oxide / (threshold.width + threshold.w0) * phi
        + threshold.k1ox * threshold.lpe[0] * phi.sqrt()
        + threshold.kt[0] * ratio
}

// B4SOItemp derives TOXP even when the card explicitly supplies it. The
// reference EOT bias uses ambient carrier density but TEMPEOT thermal voltage.
pub(super) fn physical_oxide(
    model: &BoundParameters,
    size: &PreparedBsimSoiSize,
    channel: &PreparedBsimSoiChannel,
) -> Result<f64, ModelError> {
    if integer(model, "mtrlmod")? == 0 {
        return positive(
            real(model, "tox")? - real(model, "dtoxcv")?,
            "CV physical oxide",
        );
    }
    let threshold = &channel.threshold;
    let ambient = channel.material.ambient_point()?;
    let base = channel.electrostatic.base();
    let [doping, _] = channel.material.doping();
    let density = ambient.intrinsic_density.value;
    let temperature = positive(real(model, "tempeot")?, "EOT temperature")?;
    let vtm = K_OVER_Q * temperature;
    let phi = 2. * vtm * (doping / density).ln();
    let vbi = vtm * (1e20 * doping / density.powi(2)).ln();
    let gate = reference_gate(model, size, channel, phi)?;
    let length = positive(real(model, "leffeot")?, "EOT length")?;
    let width = positive(real(model, "weffeot")?, "EOT width")?;
    let depletion = ambient.depletion_width.value;
    let scale = threshold.factor1 * depletion.sqrt();
    let theta = |coefficients: [f64; 3], distance: f64| {
        let e = Jet::constant(-0.5 * coefficients[1] * distance / scale)
            .floor_exp()
            .value;
        e * 2_f64.mul_add(e, 1.)
    };
    let short = theta(threshold.dvt, length);
    let narrow = theta(threshold.dvtw, length * width);
    let slope = (threshold.cdsc[0]
        .mul_add(short, threshold.nfactor * threshold.epssub / depletion)
        + threshold.cit)
        / threshold.cox;
    let n = if slope >= -0.5 {
        1. + slope
    } else {
        3_f64.mul_add(slope, 1.) / 8_f64.mul_add(slope, 3.)
    };
    let dits = if threshold.dvtp[0] > 0. {
        n * vtm * (length / (2_f64.mul_add(threshold.dvtp[0], length))).ln()
    } else {
        0.
    };
    let lpe = (1. + binned(size, "lpe0")? / length).sqrt();
    let body_lpe = (1. + binned(size, "lpeb")? / length).sqrt();
    let kt = binned(size, "kt1")? + binned(size, "kt1l")? / length;
    let root = phi.sqrt();
    let body = ((threshold.k1ox - threshold.k1) * root).mul_add(body_lpe, threshold.vth0);
    let short_shift = threshold.dvtw[0].mul_add(narrow, threshold.dvt[0] * short);
    let narrow_shift = threshold.k3[0] * threshold.toxe * phi / (width + threshold.w0);
    let vth = short_shift.mul_add(phi - vbi, body) + narrow_shift;
    let vth = (threshold.k1ox * (lpe - 1.)).mul_add(root, vth);
    let vth = kt.mul_add(temperature / channel.nominal - 1., vth) - dits;
    let overdrive = reference_overdrive(
        gate - vth,
        n,
        vtm,
        channel,
        ambient.depletion_capacitance.value,
    );
    let flatband = (4. * (threshold.vth0 - base.flatband - phi)).max(0.);
    let amplitude = real(model, "ados")? * 1.9e-9 * 3.9
        / positive(real(model, "epsrsub")?, "EOT substrate permittivity")?;
    let exponent = 0.7 * real(model, "bdos")?;
    let mut oxide = threshold.toxe;
    for _ in 0..5 {
        let field = (overdrive + flatband) / (2e8 * oxide);
        let next = positive(
            threshold.toxe - amplitude / (1. + (exponent * field.ln()).exp()),
            "CV physical oxide",
        )?;
        let converged = (next - oxide).abs() <= 1e-12;
        oxide = next;
        if converged {
            break;
        }
    }
    Ok(oxide)
}

fn reference_gate(
    model: &BoundParameters,
    size: &PreparedBsimSoiSize,
    channel: &PreparedBsimSoiChannel,
    phi: f64,
) -> Result<f64, ModelError> {
    let gate = channel.polarity * real(model, "vddeot")?;
    let epsgate = real(model, "epsrgate")? * EPS_0;
    let doping = binned(size, "ngate")?;
    if !(1e18..1e25).contains(&doping)
        || doping <= 1e18
        || gate <= channel.electrostatic.base().flatband + phi
        || epsgate == 0.
    {
        return Ok(gate);
    }
    // The native EOT-specific equation subtracts EPSGATE, not VFB + PHI.
    let scale = 1e6 * CHARGE * channel.threshold.epssub * doping / channel.threshold.cox.powi(2);
    let delta = gate - epsgate;
    let drop = 2. * delta / ((1. + 2. * delta / scale).sqrt() + 1.);
    let band = 1.12 - 0.5 * drop.powi(2) / scale - 0.05;
    Ok(gate - 1.12 + band.midpoint(band.mul_add(band, 0.224).sqrt()))
}

fn reference_overdrive(
    raw: f64,
    n: f64,
    vtm: f64,
    channel: &PreparedBsimSoiChannel,
    cdep: f64,
) -> f64 {
    let mstar = channel.electrostatic.base().effective_inversion;
    let scale = n * vtm;
    let argument = mstar * raw / scale;
    let barrier = (1. - mstar).mul_add(-raw, channel.voff) / scale;
    if argument > 100. {
        raw
    } else if barrier > 100. {
        vtm * cdep / channel.threshold.cox * ((raw - channel.voff) / scale).exp()
    } else {
        scale * argument.exp().ln_1p()
            / (n * channel.threshold.cox / cdep).mul_add(barrier.exp(), mstar)
    }
}
