//! Family-specific size preparation. Coefficients are in the reference
//! equation units; no name lookup occurs after instance preparation.
use super::{LegacyBsimGeometry, PreparedLegacyBsimModel, finite, positive};
use crate::{ModelError, ModelVariant};

/// BSIM1 normalized channel coefficients.
#[derive(Clone, Debug, PartialEq)]
pub struct Bsim1Coefficients {
    /// Flat-band voltage.
    pub vfb: f64,
    /// Surface potential (lower-bounded at 0.1 V).
    pub phi: f64,
    /// First body-effect coefficient (nonnegative).
    pub k1: f64,
    /// Second body-effect coefficient (nonnegative).
    pub k2: f64,
    /// Drain-induced threshold coefficients: constant, body, drain.
    pub eta: [f64; 3],
    /// Zero-drain transconductance factor: constant, body.
    pub beta_zero: [f64; 2],
    /// VDD transconductance factor: constant, body, drain.
    pub beta_vdd: [f64; 3],
    /// Gate mobility degradation: constant, body.
    pub ugs: [f64; 2],
    /// Drain mobility degradation: constant, body, drain.
    pub uds: [f64; 3],
    /// Subthreshold slope: constant, body, drain.
    pub subthreshold: [f64; 3],
    /// VDD interpolation anchor, volts.
    pub vdd: f64,
    /// True selects 0/100; false selects 40/60 channel-charge partition.
    pub partition_zero_drain: bool,
}

/// BSIM2 normalized channel coefficients. The beta2 series is not multiplied
/// by oxide capacitance, unlike beta0, betas, beta3 and beta4.
#[derive(Clone, Debug, PartialEq)]
pub struct Bsim2Coefficients {
    /// Flat-band voltage.
    pub vfb: f64,
    /// Surface potential.
    pub phi: f64,
    /// First body-effect coefficient.
    pub k1: f64,
    /// Second body-effect coefficient.
    pub k2: f64,
    /// Drain-induced threshold coefficients: constant, body.
    pub eta: [f64; 2],
    /// Zero-drain transconductance factor: constant, body.
    pub beta_zero: [f64; 2],
    /// Saturation transconductance factor: constant, body.
    pub beta_saturation: [f64; 2],
    /// Hyperbolic-tangent mobility coefficients: constant, body, gate.
    pub beta2: [f64; 3],
    /// Beta3 coefficients: constant, body, gate.
    pub beta3: [f64; 3],
    /// Beta4 coefficients: constant, body, gate.
    pub beta4: [f64; 3],
    /// Vertical mobility degradation coefficients: constant, body.
    pub ua: [f64; 2],
    /// Quadratic vertical mobility degradation: constant, body.
    pub ub: [f64; 2],
    /// Velocity-saturation coefficients: constant, body, drain.
    pub u1: [f64; 3],
    /// Subthreshold slope: constant, body, drain.
    pub subthreshold: [f64; 3],
    /// Weak-inversion offset: constant, body, drain.
    pub offset: [f64; 3],
    /// Impact-ionization coefficients A: constant, body.
    pub impact_a: [f64; 2],
    /// Impact-ionization coefficients B: constant, body.
    pub impact_b: [f64; 2],
    /// High gate-overdrive interpolation bound.
    pub gate_high: f64,
    /// Low gate-overdrive interpolation bound.
    pub gate_low: f64,
    /// Model thermal voltage from the model TEMP (not circuit ambient).
    pub thermal_voltage: f64,
    /// Reference voltage anchors VDD, VGG, VBB.
    pub voltage_anchors: [f64; 3],
    /// Square root of surface potential.
    pub sqrt_phi: f64,
    /// Surface potential to the 3/2 power.
    pub phi_three_halves: f64,
    /// Beta body interpolation coefficient.
    pub beta_body_argument: f64,
}

/// Distinct physical families; no shared fallback MOS equation.
#[derive(Clone, Debug, PartialEq)]
pub enum LegacyBsimChannel {
    /// BSIM1 size-dependent parameters.
    Bsim1(Bsim1Coefficients),
    /// BSIM2 size-dependent parameters.
    Bsim2(Bsim2Coefficients),
}

impl LegacyBsimChannel {
    pub(super) fn threshold(&self) -> Result<f64, ModelError> {
        let (vfb, phi, k1, k2) = match self {
            Self::Bsim1(c) => (c.vfb, c.phi, c.k1, c.k2),
            Self::Bsim2(c) => (c.vfb, c.phi, c.k1, c.k2),
        };
        finite(
            k2.mul_add(-phi, k1.mul_add(phi.sqrt(), vfb + phi)),
            "zero-bias threshold",
        )
    }
}

struct Binning<'a> {
    model: &'a PreparedLegacyBsimModel,
    length_um: f64,
    width_um: f64,
    beta_scale: f64,
}

impl Binning<'_> {
    fn value(&self, names: [&str; 3]) -> Result<f64, ModelError> {
        let [base, l, w] = names;
        let value = self.model.real(base)?;
        let length = self.model.real(l)?;
        let width = self.model.real(w)?;
        let result = if self.model.variant() == ModelVariant::Bsim1 {
            value + length / self.length_um + width / self.width_um
        } else {
            value + width / self.width_um + length / self.length_um
        };
        finite(result, "size-dependent model coefficient")
    }

    fn series<const N: usize>(&self, names: [[&str; 3]; N]) -> Result<[f64; N], ModelError> {
        let mut values = [0.; N];
        for (value, names) in values.iter_mut().zip(names) {
            *value = self.value(names)?;
        }
        Ok(values)
    }

    fn mobility<const N: usize>(&self, values: [f64; N]) -> Result<[f64; N], ModelError> {
        let mut result = values;
        for value in &mut result {
            *value = finite(*value * self.beta_scale, "size-dependent mobility")?;
        }
        Ok(result)
    }
}

fn bsim1(bin: &Binning<'_>) -> Result<Bsim1Coefficients, ModelError> {
    let [vfb, phi, k1, k2] = bin.series([
        ["vfb", "lvfb", "wvfb"],
        ["phi", "lphi", "wphi"],
        ["k1", "lk1", "wk1"],
        ["k2", "lk2", "wk2"],
    ])?;
    let mut beta_vdd = bin.mobility(bin.series([
        ["mus", "lmus", "wmus"],
        ["x2ms", "lx2ms", "wx2ms"],
        ["x3ms", "lx3ms", "wx3ms"],
    ])?)?;
    beta_vdd[2] = beta_vdd[2].max(0.);
    let partition = super::flag_input(&bin.model.values, "xpart")?;
    Ok(Bsim1Coefficients {
        vfb,
        phi: phi.max(0.1),
        k1: k1.max(0.),
        k2: k2.max(0.),
        eta: bin.series([
            ["eta", "leta", "weta"],
            ["x2e", "lx2e", "wx2e"],
            ["x3e", "lx3e", "wx3e"],
        ])?,
        beta_zero: bin.mobility([
            bin.model.real("muz")?,
            bin.value(["x2mz", "lx2mz", "wx2mz"])?,
        ])?,
        beta_vdd,
        ugs: bin.series([["u0", "lu0", "wu0"], ["x2u0", "lx2u0", "wx2u0"]])?,
        uds: bin.series([
            ["u1", "lu1", "wu1"],
            ["x2u1", "lx2u1", "wx2u1"],
            ["x3u1", "lx3u1", "wx3u1"],
        ])?,
        subthreshold: bin.series([
            ["n0", "ln0", "wn0"],
            ["nb", "lnb", "wnb"],
            ["nd", "lnd", "wnd"],
        ])?,
        vdd: positive(bin.model.real("vdd")?, "vdd")?,
        partition_zero_drain: partition,
    })
}

struct Bsim2Mobility {
    beta_zero: [f64; 2],
    beta_saturation: [f64; 2],
    beta3: [f64; 3],
    beta4: [f64; 3],
    beta_body_argument: f64,
}

fn bsim2_mobility(bin: &Binning<'_>, vdd: f64, vbb: f64) -> Result<Bsim2Mobility, ModelError> {
    let beta_zero = [
        bin.model.real("mu0")?,
        bin.value(["mu0b", "lmu0b", "wmu0b"])?,
    ];
    let mut beta_saturation =
        bin.series([["mus0", "lmus0", "wmus0"], ["musb", "lmusb", "wmusb"]])?;
    beta_saturation[0] = beta_saturation[0].max(1.01 * beta_zero[0]);
    let tmp = beta_zero[1].mul_add(-vbb, beta_saturation[0] - beta_zero[0]);
    if -beta_saturation[1] * vbb > tmp {
        beta_saturation[1] = finite(-tmp / vbb, "saturation mobility body coefficient")?;
    }
    let beta_zero = bin.mobility(beta_zero)?;
    let beta_saturation = bin.mobility(beta_saturation)?;
    let beta3 = bin.mobility(bin.series([
        ["mu30", "lmu30", "wmu30"],
        ["mu3b", "lmu3b", "wmu3b"],
        ["mu3g", "lmu3g", "wmu3g"],
    ])?)?;
    let beta4 = bin.mobility(bin.series([
        ["mu40", "lmu40", "wmu40"],
        ["mu4b", "lmu4b", "wmu4b"],
        ["mu4g", "lmu4g", "wmu4g"],
    ])?)?;
    let arg = finite(
        vdd.mul_add(
            -vdd.mul_add(-beta4[1], beta3[1]),
            beta_saturation[1] - beta_zero[1],
        ),
        "beta body argument",
    )?;
    Ok(Bsim2Mobility {
        beta_zero,
        beta_saturation,
        beta3,
        beta4,
        beta_body_argument: arg,
    })
}

fn bsim2(bin: &Binning<'_>) -> Result<Bsim2Coefficients, ModelError> {
    let [vfb, phi, k1, k2] = bin.series([
        ["vfb", "lvfb", "wvfb"],
        ["phi", "lphi", "wphi"],
        ["k1", "lk1", "wk1"],
        ["k2", "lk2", "wk2"],
    ])?;
    let phi = positive(phi, "phi")?;
    let vdd = positive(bin.model.real("vdd")?, "vdd")?;
    let vgg = bin.model.real("vgg")?;
    let vbb = bin.model.real("vbb")?;
    for voltage in [vdd, vgg, vbb] {
        finite(2. * voltage, "voltage anchor")?;
    }
    let Bsim2Mobility {
        beta_zero,
        beta_saturation,
        beta3,
        beta4,
        beta_body_argument,
    } = bsim2_mobility(bin, vdd, vbb)?;
    let mut subthreshold = bin.series([
        ["n0", "ln0", "wn0"],
        ["nb", "lnb", "wnb"],
        ["nd", "lnd", "wnd"],
    ])?;
    subthreshold[0] = subthreshold[0].max(0.);
    let sqrt_phi = phi.sqrt();
    Ok(Bsim2Coefficients {
        vfb,
        phi,
        k1,
        k2,
        eta: bin.series([["eta0", "leta0", "weta0"], ["etab", "letab", "wetab"]])?,
        beta_zero,
        beta_saturation,
        beta2: bin.series([
            ["mu20", "lmu20", "wmu20"],
            ["mu2b", "lmu2b", "wmu2b"],
            ["mu2g", "lmu2g", "wmu2g"],
        ])?,
        beta3,
        beta4,
        ua: bin.series([["ua0", "lua0", "wua0"], ["uab", "luab", "wuab"]])?,
        ub: bin.series([["ub0", "lub0", "wub0"], ["ubb", "lubb", "wubb"]])?,
        u1: bin.series([
            ["u10", "lu10", "wu10"],
            ["u1b", "lu1b", "wu1b"],
            ["u1d", "lu1d", "wu1d"],
        ])?,
        subthreshold,
        offset: bin.series([
            ["vof0", "lvof0", "wvof0"],
            ["vofb", "lvofb", "wvofb"],
            ["vofd", "lvofd", "wvofd"],
        ])?,
        impact_a: bin.series([["ai0", "lai0", "wai0"], ["aib", "laib", "waib"]])?,
        impact_b: bin.series([["bi0", "lbi0", "wbi0"], ["bib", "lbib", "wbib"]])?,
        gate_high: bin.value(["vghigh", "lvghigh", "wvghigh"])?,
        gate_low: bin.value(["vglow", "lvglow", "wvglow"])?,
        thermal_voltage: positive(
            8.625e-5 * (bin.model.real("temp")? + 273.),
            "model thermal voltage",
        )?,
        voltage_anchors: [vdd, vgg, vbb],
        sqrt_phi,
        phi_three_halves: finite(sqrt_phi * phi, "phi^(3/2)")?,
        beta_body_argument,
    })
}

pub(super) fn prepare(
    model: &PreparedLegacyBsimModel,
    geometry: &LegacyBsimGeometry,
    beta_scale: f64,
) -> Result<LegacyBsimChannel, ModelError> {
    let bin = Binning {
        model,
        length_um: positive(
            geometry.effective_length * 1e6,
            "effective length in micrometres",
        )?,
        width_um: positive(
            geometry.effective_width * 1e6,
            "effective width in micrometres",
        )?,
        beta_scale,
    };
    match model.variant() {
        ModelVariant::Bsim1 => bsim1(&bin).map(LegacyBsimChannel::Bsim1),
        ModelVariant::Bsim2 => bsim2(&bin).map(LegacyBsimChannel::Bsim2),
        _ => Err(ModelError::WrongModelScope),
    }
}
