//! Common source-derived BSIM3SOI threshold and overdrive primitives.
//! Copyright 1999/1990 Regents of the University of California. See package LICENSE.
use super::{CHARGE, Context, EPS_OX, EPS_SI, Electrostatics, differential::Differential};
use crate::{ModelError, preparation::positive};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Threshold {
    poly_doping: f64,
    dvt: [f64; 3],
    dvtw: [f64; 3],
    temperature: [f64; 3],
    lateral: f64,
    narrow: [f64; 3],
    dibl: [f64; 2],
    subthreshold: [f64; 5],
}

impl Threshold {
    pub(super) fn prepare(c: &Context<'_>) -> Result<Self, ModelError> {
        Ok(Self {
            poly_doping: c.binned("ngate")?,
            dvt: [c.binned("dvt0")?, c.binned("dvt1")?, c.binned("dvt2")?],
            dvtw: [c.binned("dvt0w")?, c.binned("dvt1w")?, c.binned("dvt2w")?],
            temperature: [c.binned("kt1")?, c.binned("kt1l")?, c.binned("kt2")?],
            lateral: c.binned("nlx")?,
            narrow: [c.binned("k3")?, c.binned("k3b")?, c.binned("w0")?],
            dibl: [c.binned("eta0")?, c.binned("etab")?],
            subthreshold: [
                c.binned("nfactor")?,
                c.binned("cdsc")?,
                c.binned("cdscb")?,
                c.binned("cdscd")?,
                c.binned("cit")?,
            ],
        })
    }
}

pub(super) fn one_plus<const N: usize>(x: Differential<N>) -> Differential<N> {
    if x.value >= -0.5 {
        Differential::constant(1.0) + x
    } else {
        (Differential::constant(1.0) + x.scale(3.0)) / (Differential::constant(3.0) + x.scale(8.0))
    }
}

pub(super) fn poly_depletion<const N: usize>(
    gate: Differential<N>,
    onset: f64,
    doping: f64,
    oxide: f64,
) -> Differential<N> {
    if doping <= 1e18 || doping >= 1e25 || gate.value <= onset {
        return gate;
    }
    let coefficient = 1e6 * CHARGE * EPS_SI * doping / oxide / oxide;
    let voltage = gate - Differential::constant(onset);
    let root = (Differential::constant(1.0) + voltage.scale(2.0 / coefficient)).sqrt();
    let t = voltage.scale(2.0) / (root + Differential::constant(1.0));
    let poly = (t * t).scale(0.5 / coefficient);
    let remainder = Differential::constant(1.07) - poly;
    gate - Differential::constant(1.12)
        + (remainder + (remainder * remainder + Differential::constant(0.224)).sqrt()).scale(0.5)
}

pub(super) fn overdrive<const N: usize>(
    vgst: Differential<N>,
    n: Differential<N>,
    thermal: Differential<N>,
    voff: f64,
    cdep: f64,
    cox: f64,
    cutoff: f64,
) -> Differential<N> {
    let scale = (n * thermal).scale(2.0);
    let x = vgst / scale;
    let y = (Differential::constant(2.0 * voff) - vgst) / scale;
    if x.value > cutoff {
        vgst
    } else if y.value > cutoff {
        thermal.scale(cdep / cox) * ((vgst - Differential::constant(voff)) / (n * thermal)).exp()
    } else {
        scale * x.exp().ln_1p()
            / (Differential::constant(1.0) + n.scale(2.0 * cox / cdep) * y.exp())
    }
}

/// Constants shared by all three threshold calculations.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct ThresholdGeometry {
    pub length: f64,
    pub width: f64,
    pub oxide: f64,
    pub tox: f64,
    pub sign: f64,
    pub(super) electrostatics: Electrostatics,
}

impl ThresholdGeometry {
    pub(super) fn prepare(
        c: &Context<'_>,
        electrostatics: Electrostatics,
    ) -> Result<Self, ModelError> {
        let g = c.instance.geometry();
        Ok(Self {
            length: g.length_m,
            width: g.width_m,
            oxide: c.model.oxide_capacitance_f_per_m2(),
            tox: positive(c.model("tox")?, "tox")?,
            sign: c.sign(),
            electrostatics,
        })
    }
}

pub(super) struct ThresholdBias<const N: usize> {
    pub phi: f64,
    pub body: Differential<N>,
    pub drain: Differential<N>,
    pub built_in: Differential<N>,
    pub ratio_delta: Differential<N>,
    // PD additionally extends the surface root above its body clamp.
    pub root_adjustment: Differential<N>,
}

pub(super) struct ThresholdPoint<const N: usize> {
    pub root: Differential<N>,
    pub depth: Differential<N>,
    pub threshold: Differential<N>,
    pub n: Differential<N>,
}

fn theta<const N: usize>(x: Differential<N>, pd: bool) -> Differential<N> {
    let (limit, minimum) = if pd {
        (-100.0, 3.720_075_976e-44)
    } else {
        (-34.0, 1.713_908_431e-15)
    };
    let e = if x.value > limit {
        x.exp()
    } else {
        Differential::constant(minimum)
    };
    e * (Differential::constant(1.0) + e.scale(2.0))
}

impl Threshold {
    pub(super) const fn poly_doping(self) -> f64 {
        self.poly_doping
    }

    pub(super) fn evaluate<const N: usize>(
        &self,
        g: &ThresholdGeometry,
        b: &ThresholdBias<N>,
        pd: bool,
    ) -> ThresholdPoint<N> {
        let root_phi = b.phi.sqrt();
        let root = (Differential::constant(b.phi) - b.body).sqrt();
        let depth =
            root.scale((2.0 * EPS_SI / (CHARGE * g.electrostatics.channel_doping * 1e6)).sqrt());
        let scale = depth.sqrt().scale((EPS_SI * g.tox / EPS_OX).sqrt());
        let short = theta(
            Differential::constant(-0.5 * self.dvt[1] * g.length)
                / (scale * one_plus(b.body.scale(self.dvt[2]))),
            pd,
        );
        let narrow = theta(
            Differential::constant(-0.5 * self.dvtw[1] * g.width * g.length)
                / (scale * one_plus(b.body.scale(self.dvtw[2]))),
            pd,
        );
        let v0 = b.built_in - Differential::constant(b.phi);
        let k1 = g.electrostatics.effective_k1;
        let temp_shift =
            Differential::constant(k1 * ((1.0 + self.lateral / g.length).sqrt() - 1.0) * root_phi)
                + (Differential::constant(self.temperature[0] + self.temperature[1] / g.length)
                    + b.body.scale(self.temperature[2]))
                    * b.ratio_delta;
        let narrow_shift = (Differential::constant(self.narrow[0]) + b.body.scale(self.narrow[1]))
            .scale(g.tox * b.phi / (g.width + self.narrow[2]));
        let eta = Differential::constant(self.dibl[0]) + b.body.scale(self.dibl[1]);
        let eta = if eta.value < 1e-4 {
            (Differential::constant(2e-4) - eta) / (Differential::constant(3.0) - eta.scale(2e4))
        } else {
            eta
        };
        let dibl = eta * b.drain.scale(g.electrostatics.short_channel_factor);
        let threshold = Differential::constant(g.sign * g.electrostatics.threshold)
            + (root - b.root_adjustment - Differential::constant(root_phi)).scale(k1)
            - b.body.scale(g.electrostatics.k2)
            - short.scale(self.dvt[0]) * v0
            - narrow.scale(self.dvtw[0]) * v0
            + narrow_shift
            + temp_shift
            - dibl;
        let n = one_plus(
            (Differential::constant(self.subthreshold[0] * EPS_SI) / depth
                + (Differential::constant(self.subthreshold[1])
                    + b.body.scale(self.subthreshold[2])
                    + b.drain.scale(self.subthreshold[3]))
                    * short
                + Differential::constant(self.subthreshold[4]))
            .scale(g.oxide.recip()),
        );
        ThresholdPoint {
            root,
            depth,
            threshold,
            n,
        }
    }
}
