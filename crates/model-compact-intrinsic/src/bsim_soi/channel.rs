//! Bias-dependent BSIMSOI 4.4 channel electrostatics.
//! Copyright 2010 Regents of the University of California. See package LICENSE.
mod current;
pub use current::{BsimSoiDrainChannelPoint, PreparedBsimSoiDrainChannel};
mod fd;
mod leakage;
pub use leakage::{BsimSoiGateInducedLeakage, PreparedBsimSoiGateInducedLeakage};
mod jet;
mod threshold;

use super::{
    BsimSoiElectrostaticBase, CHARGE, EPS_0, EPS_OX, EPS_SI, PreparedBsimSoiElectrostatics,
    PreparedBsimSoiSetup, PreparedBsimSoiSize, PreparedBsimSoiTemperature, binned, integer,
    positive, real,
};
use crate::{BoundParameters, ModelError, MosPolarity};
use fd::FullyDepleted;
pub use jet::BsimSoiChannelScalar;
use jet::Jet;
use threshold::Threshold;

/// Physical voltages relative to the source prime node and absolute temperature.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BsimSoiChannelBias {
    pub gate: f64,
    pub drain: f64,
    pub body: f64,
    pub backgate: f64,
    pub kelvin: f64,
}

/// Channel electrostatics in the source-defined normalized orientation.
/// All gradients refer to the original physical input coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BsimSoiChannelPoint {
    pub reversed: bool,
    pub drain_voltage: BsimSoiChannelScalar,
    pub gate_voltage: BsimSoiChannelScalar,
    pub gate_drain_voltage: BsimSoiChannelScalar,
    pub body_voltage: BsimSoiChannelScalar,
    pub body_mos: BsimSoiChannelScalar,
    pub body_limited: BsimSoiChannelScalar,
    pub body_effective: BsimSoiChannelScalar,
    pub surface_potential: BsimSoiChannelScalar,
    pub depletion_width: BsimSoiChannelScalar,
    pub threshold: BsimSoiChannelScalar,
    pub subthreshold_factor: BsimSoiChannelScalar,
    pub effective_overdrive: BsimSoiChannelScalar,
    pub noise_carrier_density: BsimSoiChannelScalar,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreparedBsimSoiChannel {
    electrostatic: PreparedBsimSoiElectrostatics,
    material: PreparedBsimSoiTemperature,
    threshold: Threshold,
    fd: Option<FullyDepleted>,
    polarity: f64,
    nominal: f64,
    poly_scale: Option<f64>,
    voff: f64,
}

struct Trial {
    drain: Jet,
    backgate: Jet,
    phi: Jet,
    vbi: Jet,
    depletion: Jet,
    vtm: Jet,
    temp_ratio: Jet,
    theta: Jet,
}

impl PreparedBsimSoiChannel {
    fn poly_gate(&self, gate: Jet, phi: Jet) -> Jet {
        let offset = gate - self.electrostatic.base().flatband - phi;
        self.poly_scale
            .filter(|_| offset.value > 0.)
            .map_or(gate, |scale| {
                let root = (1. + 2. * offset / scale).sqrt();
                let drop = 2. * offset / (root + 1.);
                let poly = 0.5 / scale * drop * drop;
                let band = 1.12 - poly - 0.05;
                gate - 1.12 + band.half_positive(Jet::constant(0.224))
            })
    }

    fn overdrive(&self, trial: &Trial, gate: Jet, threshold: Jet, slope: Jet, cdep: Jet) -> Jet {
        let mstar = self.electrostatic.base().effective_inversion;
        let raw = gate - threshold;
        let scale = slope * trial.vtm;
        let arg = mstar * raw / scale;
        let barrier = (self.voff - (1. - mstar) * raw) / scale;
        if arg.value > 100. {
            raw
        } else if barrier.value > 100. {
            trial.vtm * cdep / self.threshold.cox * ((raw - self.voff) / scale).exp()
        } else {
            scale * arg.exp().ln_1p() / (mstar + slope * self.threshold.cox / cdep * barrier.exp())
        }
    }

    /// Evaluate the PD/FD body potentials, threshold and effective gate overdrive.
    /// # Errors
    /// Nonfinite bias or a nonphysical constitutive domain.
    pub fn evaluate(&self, bias: BsimSoiChannelBias) -> Result<BsimSoiChannelPoint, ModelError> {
        let values = [bias.gate, bias.drain, bias.body, bias.backgate];
        if !values.into_iter().all(f64::is_finite) {
            return Err(ModelError::InvalidPhysicalParameter("channel bias"));
        }
        let [mut gate, mut drain, mut body, mut backgate] =
            std::array::from_fn(|i| Jet::coordinate(values[i] * self.polarity, i, self.polarity));
        let reversed = drain.value < 0.;
        if reversed {
            gate = gate - drain;
            body = body - drain;
            backgate = backgate - drain;
            drain = -drain;
        }
        let material = self.material.evaluate(bias.kelvin)?;
        let electro = self.electrostatic.evaluate(bias.kelvin)?;
        let temp_slope = if self.material.network().is_some() {
            1. / self.nominal
        } else {
            0.
        };
        let trial = Trial {
            drain,
            backgate: backgate - Jet::thermal(material.backgate_flatband),
            phi: Jet::thermal(material.surface_potential),
            vbi: Jet::thermal(material.built_in_potential),
            depletion: Jet::thermal(material.depletion_width),
            vtm: Jet::thermal(material.thermal_voltage),
            temp_ratio: Jet::coordinate(material.kelvin / self.nominal - 1., 4, temp_slope),
            theta: Jet::thermal(electro.theta_body),
        };
        let gate_drain = self.poly_gate(gate - drain, trial.phi);
        let gate = self.poly_gate(gate, trial.phi);
        let (mos, body) = if let Some(fd) = &self.fd {
            fd.evaluate(&trial, &self.threshold, gate, body)?
        } else {
            (body, body)
        };
        let floor = -5. + (mos + 4.999).half_positive(Jet::constant(0.02));
        let limited = 1.5 - (1.498 - floor).half_positive(Jet::constant(0.012));
        let cap = 0.95 * trial.phi;
        let effective = cap - (cap - limited - 0.002).half_positive(0.008 * cap);
        let surface = trial.phi - effective;
        let root = surface.sqrt();
        let extended = root - 2.2361 / trial.phi.sqrt() * (limited - effective);
        let (threshold, slope, depletion) =
            self.threshold.evaluate(&trial, effective, root, extended)?;
        let overdrive = self.overdrive(
            &trial,
            gate,
            threshold,
            slope,
            Jet::thermal(material.depletion_capacitance),
        );
        let density = trial.vtm / CHARGE
            * (self.threshold.cox + self.threshold.epssub / depletion + self.threshold.cit);
        Ok(BsimSoiChannelPoint {
            reversed,
            drain_voltage: drain,
            gate_voltage: gate.checked("poly gate voltage")?,
            gate_drain_voltage: gate_drain.checked("poly gate drain voltage")?,
            body_voltage: body.checked("body voltage")?,
            body_mos: mos.checked("MOS body voltage")?,
            body_limited: limited.checked("limited body voltage")?,
            body_effective: effective.checked("effective body voltage")?,
            surface_potential: surface.checked("channel surface potential")?,
            depletion_width: depletion,
            threshold,
            subthreshold_factor: slope,
            effective_overdrive: overdrive.checked("effective gate overdrive")?,
            noise_carrier_density: density.checked("noise carrier density")?,
        })
    }
}

fn threshold(
    size: &PreparedBsimSoiSize,
    base: BsimSoiElectrostaticBase,
    polarity: f64,
    epssub: f64,
    toxe: f64,
) -> Result<Threshold, ModelError> {
    let g = size.geometry();
    let get = |name| binned(size, name);
    Ok(Threshold {
        vth0: polarity * base.threshold,
        k1: base.k1_effective,
        k1ox: base.k1_oxide,
        k2ox: base.k2_oxide,
        eta0: base.eta0,
        etab: get("etab")?,
        cox: base.oxide_capacitance,
        epssub,
        factor1: base.characteristic_length_factor,
        length: g.leff,
        width: g.weff,
        toxe,
        w0: get("w0")?,
        dvt: [get("dvt0")?, get("dvt1")?, get("dvt2")?],
        dvtw: [get("dvt0w")?, get("dvt1w")?, get("dvt2w")?],
        cdsc: [get("cdsc")?, get("cdscb")?, get("cdscd")?],
        nfactor: get("nfactor")?,
        cit: get("cit")?,
        dvtp: [
            get("dvtp0")?,
            get("dvtp1")?,
            base.dits_factor,
            get("dvtp4")?,
        ],
        lpe: [
            (1. + get("lpe0")? / g.leff).sqrt() - 1.,
            (1. + get("lpeb")? / g.leff).sqrt(),
        ],
        kt: [get("kt1")? + get("kt1l")? / g.leff, get("kt2")?],
        k3: [get("k3")?, get("k3b")?],
    })
}

fn fully_depleted(
    model: &BoundParameters,
    size: &PreparedBsimSoiSize,
    mode: i32,
    epssub: f64,
    toxe: f64,
) -> Result<Option<FullyDepleted>, ModelError> {
    if mode == 0 {
        return Ok(None);
    }
    let generalized = integer(model, "mtrlmod")? != 0;
    let epsrox = if generalized {
        3.9
    } else {
        real(model, "epsrox")?
    };
    let tsi = positive(real(model, "tsi")?, "TSI")?;
    let csi = EPS_SI
        / positive(
            if generalized {
                real(model, "etsi")?
            } else {
                tsi
            },
            "film thickness",
        )?;
    let cbox = EPS_OX / positive(real(model, "tbox")?, "TBOX")?;
    let xj = binned(size, "xj")?;
    let litl = positive(
        if generalized {
            (epssub * xj * toxe / (epsrox * EPS_0)).sqrt()
        } else {
            (3. * 3.9 / epsrox * xj * toxe).sqrt()
        },
        "FD characteristic length",
    )?;
    let length = size.geometry().leff;
    let decay = |name| -> Result<f64, ModelError> {
        let e = (-0.5 * real(model, name)? * length / litl).exp();
        Ok(e.mul_add(2. * e, e))
    };
    let qsi = positive(
        CHARGE * real(model, "nch")? * (1. + binned(size, "lpe0")? / length) * 1e6 * tsi,
        "FD film charge",
    )?;
    Ok(Some(FullyDepleted {
        ideal: mode == 2,
        alternate: integer(model, "fdmod")? != 0,
        csi,
        cbox,
        qsi,
        cdsbs: real(model, "cdsbs")?,
        vbsa: real(model, "vbsa")?,
        vsce: real(model, "vsce")?,
        lowering: real(model, "dvbd0")? * decay("dvbd1")?,
        backgate_coupling: real(model, "k2b")?.mul_add(-decay("dk2b")?, real(model, "k1b")?)
            / (1. + csi / cbox),
        noff: real(model, "nofffd")?,
        voff: real(model, "vofffd")?,
        moin: real(model, "moinfd")?,
    }))
}

impl PreparedBsimSoiSetup {
    /// Bind one instance's PD/FD channel electrostatics and bias law.
    /// # Errors
    /// Missing instance or model parameter, or invalid physical setup domain.
    pub fn prepare_channel(
        &self,
        index: usize,
        ambient: f64,
    ) -> Result<PreparedBsimSoiChannel, ModelError> {
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
        let material = self.prepare_temperature(index, ambient)?;
        let electrostatic = self.prepare_electrostatics(index, ambient)?;
        let base = electrostatic.base();
        let polarity = if self.model.polarity() == MosPolarity::Nmos {
            1.
        } else {
            -1.
        };
        let generalized = integer(model, "mtrlmod")? != 0;
        let toxe = positive(
            real(model, if generalized { "eot" } else { "tox" })?,
            "equivalent oxide thickness",
        )?;
        let epssub = material.substrate_permittivity();
        let epsgate = if generalized {
            real(model, "epsrgate")? * EPS_0
        } else {
            epssub
        };
        let ngate = binned(size, "ngate")?;
        let poly_scale = if ngate > 1e18 && ngate < 1e25 && epsgate != 0. {
            Some(positive(
                1e6 * CHARGE * epsgate * ngate / base.oxide_capacitance.powi(2),
                "poly depletion scale",
            )?)
        } else {
            None
        };
        Ok(PreparedBsimSoiChannel {
            electrostatic,
            material,
            polarity,
            nominal: positive(real(model, "tnom")?, "tnom")?,
            threshold: threshold(size, base, polarity, epssub, toxe)?,
            fd: fully_depleted(
                model,
                size,
                integer(instance.parameters(), "soimod")?,
                epssub,
                toxe,
            )?,
            poly_scale,
            voff: binned(size, "voff")?,
        })
    }
}
