use model_kernel_api::{Error, Result};

const EPS_SI: f64 = 11.7 * 8.854_187_923_944_2e-12;
// CODATA 2010, as used by the revision-15 OpenVAF reference.
const K_OVER_Q: f64 = 1.380_648_8e-23 / 1.602_176_565e-19;

/// Physical channel type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Polarity {
    N,
    P,
}

impl Polarity {
    pub(crate) const fn sign(self) -> f64 {
        match self {
            Self::N => 1.,
            Self::P => -1.,
        }
    }
}

/// Intrinsic model parameters in SI units; no source-dialect defaults implied.
#[derive(Clone, Copy, Debug)]
pub struct Model {
    pub polarity: Polarity,
    pub cox: f64,
    pub junction_depth: f64,
    pub threshold: f64,
    pub threshold_temperature: f64,
    pub gamma: f64,
    pub phi: f64,
    pub kp: f64,
    pub mobility_exponent: f64,
    pub theta: f64,
    pub mobility_field: f64,
    pub critical_field: f64,
    pub critical_field_exponent: f64,
    pub lambda: f64,
    pub dl: f64,
    pub dw: f64,
    pub narrow: f64,
    pub short: f64,
    pub reverse_short_charge: f64,
    pub reverse_short_length: f64,
    pub impact_a: f64,
    pub impact_b: f64,
    pub impact_temperature: f64,
    pub impact_saturation: f64,
    pub mismatch_threshold: f64,
    pub mismatch_gain: f64,
    pub mismatch_gamma: f64,
    pub nominal_temperature: f64,
}

impl Model {
    /// Defaults of the revision-15 Verilog-A reference, not `PSpice`.
    #[must_use]
    pub const fn reference_defaults(polarity: Polarity) -> Self {
        Self {
            polarity,
            cox: 2e-3,
            junction_depth: 300e-9,
            threshold: 0.5 * polarity.sign(),
            threshold_temperature: 1e-3,
            gamma: 0.7,
            phi: 0.5,
            kp: 150e-6,
            mobility_exponent: -1.5,
            theta: 0.,
            mobility_field: 1e8,
            critical_field: 2e6,
            critical_field_exponent: 0.8,
            lambda: 0.8,
            dl: -0.01e-6,
            dw: -0.01e-6,
            narrow: 0.2,
            short: 0.3,
            reverse_short_charge: 230e-6,
            reverse_short_length: 0.4e-6,
            impact_a: 5e8,
            impact_b: 4e8,
            impact_temperature: 9e-4,
            impact_saturation: 1.,
            mismatch_threshold: 1e-6,
            mismatch_gain: 1e-6,
            mismatch_gamma: 1e-6,
            nominal_temperature: 298.15,
        }
    }
}

/// Drawn geometry and channel segmentation; external circuit multiplicity
/// is deliberately separate from the intrinsic narrow/short-channel terms.
#[derive(Clone, Copy, Debug)]
pub struct Geometry {
    pub length: f64,
    pub width: f64,
    pub parallel: f64,
    pub series: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct Prepared {
    pub(crate) sign: f64,
    pub(crate) vt: f64,
    pub(crate) phi: f64,
    pub(crate) gamma: f64,
    pub(crate) threshold: f64,
    pub(crate) kp_width: f64,
    pub(crate) length: f64,
    pub(crate) area_cox: f64,
    pub(crate) critical_field: f64,
    pub(crate) characteristic_length: f64,
    pub(crate) lambda: f64,
    pub(crate) narrow: f64,
    pub(crate) short: f64,
    pub(crate) flatband_shift: f64,
    pub(crate) theta: f64,
    pub(crate) mobility_factor: Option<f64>,
    pub(crate) impact_ratio: f64,
    pub(crate) impact_field_length: f64,
    pub(crate) impact_saturation: f64,
}

fn bandgap(t: f64) -> f64 {
    1.16 - 7.02e-4 * t * t / (t + 1108.)
}

impl Model {
    fn validate(self, g: Geometry, t: f64) -> Result<()> {
        let positive = [
            self.cox,
            self.junction_depth,
            self.phi,
            self.kp,
            self.critical_field,
            self.reverse_short_length,
            self.impact_b,
            self.nominal_temperature,
            g.length,
            g.width,
            g.parallel,
            g.series,
            t,
        ];
        let nonnegative = [
            self.gamma,
            self.theta,
            self.mobility_field,
            self.lambda,
            self.narrow,
            self.short,
            self.reverse_short_charge,
            self.impact_a,
            self.impact_saturation,
            self.mismatch_threshold,
            self.mismatch_gain,
            self.mismatch_gamma,
        ];
        let signed = [
            self.threshold,
            self.threshold_temperature,
            self.mobility_exponent,
            self.critical_field_exponent,
            self.dl,
            self.dw,
            self.impact_temperature,
        ];
        if positive.iter().any(|v| !v.is_finite() || *v <= 0.)
            || nonnegative.iter().any(|v| !v.is_finite() || *v < 0.)
            || signed.iter().any(|v| !v.is_finite())
            || self.phi < 0.2
        {
            return Err(Error::Domain("invalid EKV model or geometry".into()));
        }
        Ok(())
    }

    /// Prepare geometry and temperature once, before the nonlinear evaluation.
    ///
    /// # Errors
    /// Nonphysical or nonfinite model, geometry, or absolute temperature.
    pub fn prepare(self, g: Geometry, temperature: f64) -> Result<Prepared> {
        self.validate(g, temperature)?;
        let length = g.length + self.dl;
        let width = g.width + self.dw;
        if length <= 0. || width <= 0. {
            return Err(Error::Domain("nonpositive EKV effective geometry".into()));
        }
        let vt = K_OVER_Q * temperature;
        let ratio = temperature / self.nominal_temperature;
        let delta_t = temperature - self.nominal_temperature;
        let raw_phi = bandgap(self.nominal_temperature)
            .mul_add(-ratio, (3. * vt).mul_add(-ratio.ln(), self.phi * ratio))
            + bandgap(temperature);
        let phi = 0.2 + f64::midpoint(raw_phi - 0.2, (raw_phi - 0.2).hypot(vt));
        let area_scale = (width * length).sqrt().recip();
        let gamma = (self.mismatch_gamma - 1e-6).mul_add(area_scale, self.gamma);
        let kp = self.kp
            * ratio.powf(self.mobility_exponent)
            * (self.mismatch_gain - 1e-6).mul_add(area_scale, 1.);
        let critical_field = self.critical_field * ratio.powf(self.critical_field_exponent);
        let impact_b = self.impact_b * self.impact_temperature.mul_add(delta_t, 1.);
        if gamma < 0. || kp <= 0. || critical_field <= 0. || impact_b <= 0. {
            return Err(Error::Domain(
                "nonphysical temperature or mismatch-adjusted EKV model".into(),
            ));
        }
        let eps_cox = EPS_SI / self.cox;
        let characteristic_length = (eps_cox * self.junction_depth).sqrt();
        let reverse = 0.28 * (length / (self.reverse_short_length * g.series) - 0.1);
        let short_factor =
            (1. + f64::midpoint(reverse, reverse.hypot(1.936e-3_f64.sqrt()))).recip();
        Ok(Prepared {
            sign: self.polarity.sign(),
            vt,
            phi,
            gamma,
            threshold: self.polarity.sign()
                * ((self.mismatch_threshold - 1e-6).mul_add(
                    area_scale,
                    self.threshold_temperature.mul_add(-delta_t, self.threshold),
                )),
            kp_width: kp * width,
            length,
            area_cox: length * width * self.cox,
            critical_field,
            characteristic_length,
            lambda: self.lambda,
            narrow: 3. * eps_cox * self.narrow * g.parallel / width,
            short: eps_cox * self.short * g.series / length,
            flatband_shift: 2. * self.reverse_short_charge / self.cox * short_factor * short_factor,
            theta: self.theta,
            mobility_factor: (self.mobility_field > 0.)
                .then_some(self.cox / (EPS_SI * self.mobility_field)),
            impact_ratio: self.impact_a / impact_b,
            impact_field_length: characteristic_length * impact_b,
            impact_saturation: self.impact_saturation,
        })
    }
}
