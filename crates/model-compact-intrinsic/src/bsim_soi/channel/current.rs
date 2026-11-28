//! Native BSIMSOI channel conduction, separate from junction and leakage currents.
//! Copyright 2010 Regents of the University of California. See package LICENSE.
use super::super::{
    BsimSoiThermalScalar, K_OVER_Q, PreparedBsimSoiSetup, PreparedBsimSoiSize,
    PreparedBsimSoiTransport, binned, integer, positive, real,
};
use super::{BsimSoiChannelBias, BsimSoiChannelPoint, Jet, PreparedBsimSoiChannel};
use crate::{BoundParameters, ModelError};

/// Intrinsic channel-only conduction, excluding bipolar and leakage branches.
///
/// Current is positive into the physical drain; other outputs are normalized
/// to the native source/drain orientation. Gradients use physical bias axes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BsimSoiDrainChannelPoint {
    pub current: Jet,
    pub effective_width: Jet,
    pub bulk_charge_factor: Jet,
    pub effective_mobility: Jet,
    pub saturation_voltage: Jet,
    pub effective_drain_voltage: Jet,
    pub early_voltage: Jet,
    pub channel_conductance: Jet,
    /// Effective beta, including finger and instance multiplicity (A/V²).
    pub beta: Jet,
    /// Internal access resistance of the full parallel instance.
    pub internal_resistance: Jet,
    /// IDS / VDSEFF, evaluated without dividing by VDSEFF at zero drain bias.
    pub output_conductance_ratio: Jet,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreparedBsimSoiDrainChannel {
    channel: PreparedBsimSoiChannel,
    transport: PreparedBsimSoiTransport,
    geometry: Geometry,
    mobility: Mobility,
    saturation: Saturation,
    output: OutputResistance,
}

const HUGE: f64 = 2.688_117_142e43;
const TINY: f64 = 3.720_075_976e-44;

fn lower_one(x: Jet) -> Jet {
    if x.value >= -0.9 {
        1. + x
    } else {
        (0.8 + x) / (17. + 20. * x)
    }
}
fn bulk_limit(x: Jet) -> Jet {
    if x.value < 0.01 {
        (0.02 - x) / (3. - 200. * x)
    } else {
        x
    }
}
const fn thermal(value: BsimSoiThermalScalar) -> Jet {
    Jet::thermal(value)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Geometry {
    width: f64,
    length: f64,
    width_scale: f64,
    width_gate: f64,
    width_body: f64,
    xj: f64,
    litl: f64,
    k1_lpe: f64,
    bulk: [f64; 4],
    keta: f64,
    ketas: f64,
    parallel: f64,
    segments: f64,
}

impl Geometry {
    fn width(&self, point: &BsimSoiChannelPoint, root: Jet) -> Jet {
        let width = self.width
            - self.width_scale
                * (self.width_gate * point.effective_overdrive + self.width_body * root);
        if width.value < 2e-8 {
            2e-8 * (4e-8 - width) / (6e-8 - 2. * width)
        } else {
            width
        }
    }

    pub(super) fn bulk_factors(&self, point: &BsimSoiChannelPoint, phi: Jet) -> (Jet, Jet) {
        let [a0, ags, b0, b1] = self.bulk;
        if a0 == 0. {
            return (Jet::constant(1.), Jet::constant(1.));
        }
        let body = point.body_limited;
        let corrected = body / lower_one(self.keta * body) / (phi + self.ketas);
        let factor = if corrected.value < 0.96 {
            1. / (1. - corrected).sqrt()
        } else {
            (6.016_949_152_5 - 6.355_932_203_4 * corrected) / (1. - 1.059_322_033_9 * corrected)
        };
        let factor = self.k1_lpe * factor / (phi + self.ketas).sqrt();
        let length = self.length / (self.length + 2. * (self.xj * point.depletion_width).sqrt());
        let base = 1. + factor * (a0 * length + b0 / (self.width + b1));
        (
            bulk_limit(base),
            bulk_limit(
                base - factor * ags * a0 * length * length * length * point.effective_overdrive,
            ),
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Mobility {
    mode: i32,
    oxide: f64,
    field_oxide: f64,
    material_offset: Option<f64>,
    polarity: f64,
    flatband: f64,
    reference_overdrive: f64,
    ucs: f64,
    ucste: f64,
    ud: f64,
    ud1: f64,
    eu: f64,
}

impl Mobility {
    fn evaluate(
        &self,
        point: &BsimSoiChannelPoint,
        coefficients: &super::super::BsimSoiTransportPoint,
        bandgap: Jet,
        ratio: Jet,
    ) -> Jet {
        let [ua, ub, uc] = [coefficients.ua, coefficients.ub, coefficients.uc].map(thermal);
        let vgst = point.effective_overdrive;
        let body = point.body_effective;
        let material = self
            .material_offset
            .map_or(Jet::constant(0.), |offset| offset - self.polarity * bandgap);
        let field = (vgst + 2. * point.threshold - material) / self.field_oxide;
        let degradation = match self.mode {
            1 => field * (ua + uc * body + ub * field),
            2 => {
                let field = (vgst - material) / self.oxide;
                field * (ua + uc * body + ub * field)
            }
            3 => field * (ua + ub * field) * (1. + uc * body),
            _ => {
                let field = (vgst + self.flatband) * 1e-8 / (6. * self.oxide);
                let field_term = if field.value == 0. && self.eu > 0. {
                    Jet::constant(0.)
                } else {
                    (self.eu * field.ln()).exp()
                };
                let log_ratio = ratio.ln();
                let ucs = self.ucs * (self.ucste * log_ratio).exp();
                let ud = self.ud * (self.ud1 * log_ratio).exp();
                let coulomb = ud / (ucs * (vgst / self.reference_overdrive).ln_1p()).exp();
                field_term * (ua + uc * body) + coulomb
            }
        };
        let denominator = if degradation.value >= -0.8 {
            1. + degradation
        } else {
            (0.6 + degradation) / (7. + 10. * degradation)
        };
        thermal(coefficients.mobility) / denominator
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Saturation {
    a1: f64,
    a2: f64,
    delta: f64,
}

struct SaturationPoint {
    voltage: Jet,
    effective: Jet,
    field_length: Jet,
    width_velocity_resistance: Jet,
    lambda: Jet,
}

impl Saturation {
    fn lambda(&self, overdrive: Jet) -> Jet {
        if self.a1 == 0. {
            Jet::constant(self.a2)
        } else if self.a1 > 0. {
            let headroom = 1. - self.a2;
            1. - (headroom - self.a1 * overdrive - 0.0001)
                .half_positive(Jet::constant(0.0004 * headroom))
        } else {
            (self.a2 + self.a1 * overdrive - 0.0001).half_positive(Jet::constant(0.0004 * self.a2))
        }
    }

    fn evaluate(&self, point: &BsimSoiChannelPoint, drive: &Drive) -> SaturationPoint {
        let lambda = self.lambda(point.effective_overdrive);
        let bulk = drive.bulk;
        let overdrive = drive.overdrive_thermal;
        let field = drive.field_length;
        let resistance = drive.width_velocity_resistance;
        // Rationalized quadratic root also handles the linear limit
        // Rds=0, Lambda=1 without a singular special case.
        let t0 = 2. * bulk * (bulk * resistance - 1. + 1. / lambda);
        let t1 = overdrive * (2. / lambda - 1.) + bulk * field + 3. * overdrive * bulk * resistance;
        let t2 = overdrive * (field + 2. * overdrive * resistance);
        let voltage = 2. * t2 / (t1 + (t1 * t1 - 2. * t0 * t2).sqrt());
        let drain = point.drain_voltage;
        let difference = voltage - drain - self.delta;
        let sum = voltage + drain + self.delta;
        let effective = 2. * voltage * drain
            / (sum + (difference * difference + 4. * self.delta * voltage).sqrt());
        let effective = if effective.value > drain.value {
            drain
        } else {
            effective
        };
        SaturationPoint {
            voltage,
            effective,
            field_length: field,
            width_velocity_resistance: resistance,
            lambda,
        }
    }
}

struct Drive {
    bulk: Jet,
    overdrive_thermal: Jet,
    field_length: Jet,
    width_velocity_resistance: Jet,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct OutputResistance {
    pclm: f64,
    pdiblb: f64,
    pdits: f64,
    pditsd: f64,
    pditsl: f64,
    pvag: f64,
    fprout: f64,
}

impl OutputResistance {
    fn early_voltage(
        &self,
        point: &BsimSoiChannelPoint,
        saturation: &SaturationPoint,
        drive: &Drive,
        geometry: &Geometry,
        theta: Jet,
    ) -> Jet {
        let vgst = point.effective_overdrive;
        let bulk = drive.bulk;
        let thermal = drive.overdrive_thermal;
        let diff = point.drain_voltage - saturation.effective;
        let field = saturation.field_length;
        let saturation_voltage = saturation.voltage;
        let resistance = saturation.width_velocity_resistance;
        let vasat = (field
            + saturation_voltage
            + 2. * resistance * vgst * (1. - 0.5 * bulk * saturation_voltage / thermal))
            / (2. / saturation.lambda - 1. + resistance * bulk);
        let vaclm = if self.pclm > 0. && diff.value > 1e-10 {
            geometry.length * (bulk + vgst / field) * diff / (self.pclm * bulk * geometry.litl)
        } else {
            Jet::constant(HUGE)
        };
        let vadibl = if theta.value > 0. {
            let t = bulk * saturation_voltage;
            thermal * (thermal / (thermal + t))
                / theta
                / lower_one(self.pdiblb * point.body_effective)
        } else {
            Jet::constant(HUGE)
        };
        let vadits = if self.pdits > TINY {
            let argument = self.pditsd * point.drain_voltage;
            let exponential = if argument.value > 100. {
                Jet::constant(HUGE)
            } else {
                argument.exp()
            };
            let fp = if self.fprout > 0. {
                1. / (1. + self.fprout * geometry.length.sqrt() / thermal)
            } else {
                Jet::constant(1.)
            };
            fp * (1. + self.pditsl.mul_add(geometry.length, 1.) * exponential) / self.pdits
        } else {
            Jet::constant(HUGE)
        };
        let combined = 1. / (1. / vaclm + 1. / vadibl + 1. / vadits);
        vasat + lower_one(self.pvag * vgst / field) * combined
    }
}

impl PreparedBsimSoiDrainChannel {
    /// Evaluate intrinsic drain conduction with analytic five-coordinate derivatives.
    /// # Errors
    /// Invalid physical bias, material state or nonfinite channel equation.
    pub fn evaluate(
        &self,
        bias: BsimSoiChannelBias,
    ) -> Result<BsimSoiDrainChannelPoint, ModelError> {
        let point = self.channel.evaluate(bias)?;
        let material = self.channel.material.evaluate(bias.kelvin)?;
        let electro = self.channel.electrostatic.evaluate(bias.kelvin)?;
        let transport = self.transport.evaluate(bias.kelvin)?;
        let phi = thermal(material.surface_potential);
        let ratio = Jet::coordinate(
            material.kelvin / self.channel.nominal,
            4,
            if self.channel.material.network().is_some() {
                1. / self.channel.nominal
            } else {
                0.
            },
        );
        let root = point.surface_potential.sqrt() - phi.sqrt();
        let width = self.geometry.width(&point, root);
        let bulk = self.geometry.bulk_factors(&point, phi).1;
        let mobility = self
            .mobility
            .evaluate(&point, &transport, thermal(material.bandgap), ratio);
        let r = self.transport.internal_resistance(
            bias.kelvin,
            point.effective_overdrive.value,
            root.value,
        )?;
        let resistance = Jet {
            value: r.resistance,
            gradient: std::array::from_fn(|i| {
                r.d_overdrive.mul_add(
                    point.effective_overdrive.gradient[i],
                    r.d_root_potential * root.gradient[i],
                ) + if i == 4 { r.d_temperature } else { 0. }
            }),
        } * self.geometry.parallel;
        let velocity = thermal(transport.saturation_velocity);
        let cox = self.channel.threshold.cox;
        let thermal = point.effective_overdrive + 2. * Jet::thermal(material.thermal_voltage);
        let drive = Drive {
            bulk,
            overdrive_thermal: thermal,
            field_length: 2. * velocity * self.geometry.length / mobility,
            width_velocity_resistance: width * velocity * cox * resistance,
        };
        let saturation = self.saturation.evaluate(&point, &drive);
        let early = self.output.early_voltage(
            &point,
            &saturation,
            &drive,
            &self.geometry,
            Jet::thermal(electro.theta_output),
        );
        let beta = mobility * cox * width / self.geometry.length;
        let gche =
            beta * point.effective_overdrive * (1. - 0.5 * bulk * saturation.effective / thermal)
                / (1. + saturation.effective / drive.field_length);
        let channel_conductance = gche / (1. + gche * resistance);
        let ratio = channel_conductance
            * (1. + (point.drain_voltage - saturation.effective) / early)
            * (self.geometry.parallel / self.geometry.segments);
        let current = ratio
            * saturation.effective
            * self.channel.polarity
            * if point.reversed { -1. } else { 1. };
        Ok(BsimSoiDrainChannelPoint {
            current: current.checked("channel current")?,
            effective_width: width.checked("channel width")?,
            bulk_charge_factor: bulk.checked("bulk charge factor")?,
            effective_mobility: mobility.checked("effective mobility")?,
            saturation_voltage: saturation.voltage.checked("saturation voltage")?,
            effective_drain_voltage: saturation.effective.checked("effective drain voltage")?,
            early_voltage: early.checked("channel early voltage")?,
            beta: (beta * self.geometry.parallel).checked("channel beta")?,
            internal_resistance: (resistance / self.geometry.parallel)
                .checked("internal access resistance")?,
            output_conductance_ratio: ratio.checked("channel output ratio")?,
            channel_conductance: (channel_conductance
                * (self.geometry.parallel / self.geometry.segments))
                .checked("channel conductance")?,
        })
    }
}

pub(super) fn prepare_geometry(
    model: &BoundParameters,
    instance: &BoundParameters,
    size: &PreparedBsimSoiSize,
    channel: &PreparedBsimSoiChannel,
) -> Result<Geometry, ModelError> {
    let get = |name| binned(size, name);
    let dimensions = size.geometry();
    let xj = get("xj")?;
    let generalized = integer(model, "mtrlmod")? != 0;
    let epsrox = if generalized {
        3.9
    } else {
        real(model, "epsrox")?
    };
    let litl = positive(
        if generalized {
            (channel.threshold.epssub * xj * channel.threshold.toxe
                / (epsrox * super::super::EPS_0))
                .sqrt()
        } else {
            (3. * 3.9 / epsrox * xj * channel.threshold.toxe).sqrt()
        },
        "channel characteristic length",
    )?;
    Ok(Geometry {
        width: dimensions.weff,
        length: dimensions.leff,
        width_scale: 2. - real(instance, "nbc")?,
        width_gate: get("dwg")?,
        width_body: get("dwb")?,
        xj,
        litl,
        k1_lpe: 0.5 * channel.threshold.k1ox * channel.threshold.lpe[1],
        bulk: [get("a0")?, get("ags")?, get("b0")?, get("b1")?],
        keta: get("keta")?,
        ketas: get("ketas")?,
        parallel: positive(
            real(instance, "nf")? * real(instance, "m")?,
            "channel parallel factor",
        )?,
        segments: positive(real(instance, "nseg")?, "channel segments")?,
    })
}

fn prepare_mobility(
    model: &BoundParameters,
    size: &PreparedBsimSoiSize,
    channel: &PreparedBsimSoiChannel,
) -> Result<Mobility, ModelError> {
    let get = |name| binned(size, name);
    let mode = integer(model, "mobmod")?;
    let generalized = integer(model, "mtrlmod")? != 0;
    let oxide = channel.threshold.toxe;
    let field_oxide = if generalized {
        oxide * real(model, "epsrsub")? / 3.9
    } else {
        oxide
    };
    let material_offset = if generalized {
        Some(2. * channel.polarity * (real(model, "phig")? - real(model, "easub")? + 0.45))
    } else {
        None
    };
    let base = channel.electrostatic.base();
    let reference_overdrive = if matches!(mode, 1..=3) {
        1.
    } else {
        let ambient = channel.material.ambient_point()?;
        let depletion = ambient.depletion_width.value;
        let argument = channel.threshold.dvt[1] * channel.threshold.length
            / (channel.threshold.factor1 * depletion.sqrt());
        let theta = if argument < 100. {
            let e = argument.exp();
            let diff = argument.exp_m1();
            e / diff.mul_add(diff, 2. * e * TINY)
        } else {
            1. / (HUGE - 2.)
        };
        let n = (channel.threshold.cdsc[0].mul_add(
            theta,
            channel.threshold.nfactor * channel.threshold.epssub / depletion,
        ) + channel.threshold.cit)
            / base.oxide_capacitance;
        let n = if n >= -0.5 {
            1. + n
        } else {
            3.0_f64.mul_add(n, 1.) / 8.0_f64.mul_add(n, 3.)
        };
        let scale = n * K_OVER_Q * channel.nominal;
        let exp = (channel.voff / scale).clamp(-100., 100.).exp();
        positive(
            scale * std::f64::consts::LN_2
                / (base.effective_inversion
                    + n * base.oxide_capacitance * exp / ambient.depletion_capacitance.value),
            "mobility threshold overdrive",
        )?
    };
    Ok(Mobility {
        mode,
        oxide,
        field_oxide,
        material_offset,
        polarity: channel.polarity,
        flatband: base.mobility_flatband,
        reference_overdrive,
        ucs: get("ucs")?,
        ucste: get("ucste")?,
        ud: get("ud")?,
        ud1: get("ud1")?,
        eu: get("eu")?,
    })
}

impl PreparedBsimSoiSetup {
    /// Bind the intrinsic drain-channel current law for one resolved instance.
    /// # Errors
    /// Missing model inputs or invalid physical setup domain.
    pub fn prepare_drain_channel(
        &self,
        index: usize,
        ambient: f64,
    ) -> Result<PreparedBsimSoiDrainChannel, ModelError> {
        let channel = self.prepare_channel(index, ambient)?;
        let transport = self.prepare_transport(index, ambient)?;
        let model = self.model.parameters();
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
        let get = |name| binned(size, name);
        Ok(PreparedBsimSoiDrainChannel {
            geometry: prepare_geometry(model, instance.parameters(), size, &channel)?,
            mobility: prepare_mobility(model, size, &channel)?,
            saturation: Saturation {
                a1: get("a1")?,
                a2: get("a2")?,
                delta: get("delta")?,
            },
            output: OutputResistance {
                pclm: get("pclm")?,
                pdiblb: get("pdiblcb")?,
                pdits: get("pdits")?,
                pditsd: get("pditsd")?,
                pditsl: real(model, "pditsl")?,
                pvag: get("pvag")?,
                fprout: get("fprout")?,
            },
            channel,
            transport,
        })
    }
}
