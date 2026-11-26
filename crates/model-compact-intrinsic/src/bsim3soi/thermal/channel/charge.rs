//! PD 2.2.3 intrinsic charge, including the quantum centroid C-V mode.
//! Copyright 1999/1990 Regents of the University of California. See package LICENSE.

use super::super::{
    CHARGE, Context, EPS_SI, Electrostatics, K_OVER_Q, channel_threshold::ThresholdBias,
};
use super::{Bias, D, PreparedPdChannel, State, intrinsic_density};
use crate::{
    ModelError,
    preparation::{integer, positive},
};

/// Normalized per-instance terminal charge and its derivatives.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PdCharge {
    /// Charge in coulombs, before polarity and multiplicity stamping.
    pub charge_c: f64,
    /// Derivatives with respect to normalized VGS, VDS, VBS, VES and kelvin.
    pub gradient: [f64; 5],
}

/// Charge-conserving intrinsic PD channel terminal quantities.
/// Junction, overlap and extrinsic substrate charges are separate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PdTerminalCharges {
    /// Front gate.
    pub gate: PdCharge,
    /// Drain, in forward orientation.
    pub drain: PdCharge,
    /// Source, in forward orientation.
    pub source: PdCharge,
    /// Internal body.
    pub body: PdCharge,
    /// Back gate.
    pub backgate: PdCharge,
}

impl PdCharge {
    fn from_d(q: D, backgate_derivative: f64) -> Result<Self, ModelError> {
        if !q.finite() || !backgate_derivative.is_finite() {
            return Err(ModelError::InvalidPhysicalParameter("PD intrinsic charge"));
        }
        let [gate, drain, body, thermal] = q.gradient;
        Ok(Self {
            charge_c: q.value,
            gradient: [gate, drain, body, backgate_derivative, thermal],
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct ChargeParameters {
    mode: i32,
    partition: f64,
    gate_area: f64,
    body_area: f64,
    backgate_cap: f64,
    bulk_factor: f64,
    cv_tox: f64,
    delvt: f64,
    acde: f64,
    debye_length: f64,
    moin: f64,
    nsub: f64,
}

impl ChargeParameters {
    pub(super) fn prepare(c: &Context<'_>, e: Electrostatics) -> Result<Self, ModelError> {
        let g = c.instance.geometry();
        let pd =
            g.pd.ok_or(ModelError::InvalidPhysicalParameter("PD C-V geometry"))?;
        let width = g.cv_width_m / positive(c.instance("nseg")?, "nseg")?;
        let body = c.model("fbody")?;
        let contact = c.instance("agbcp")?;
        let mode = integer(c.model.parameters(), "capmod")?;
        let tox = c.model("tox")?;
        let cv_tox = if mode == 3 {
            positive(tox - c.model("dtoxcv")?, "C-V oxide thickness")?
        } else {
            tox
        };
        let debye_length =
            (EPS_SI * K_OVER_Q * c.model("tnom")? / (CHARGE * e.channel_doping * 1e6)).sqrt() / 3.0;
        Ok(Self {
            mode,
            partition: c.model("xpart")?,
            gate_area: width.mul_add(g.cv_length_m, contact),
            body_area: body * width.mul_add(pd.body_cv_length, contact),
            backgate_cap: c.binned("kb1")?
                * body
                * e.buried_oxide_capacitance
                * width.mul_add(pd.backgate_cv_length, c.instance("aebcp")?),
            bulk_factor: g.abulk_cv_factor,
            cv_tox,
            delvt: c.binned("delvt")?,
            acde: c.binned("acde")? * (e.channel_doping / 2e16).powf(-0.25),
            debye_length,
            moin: c.binned("moin")?,
            nsub: c.binned("nsub")?,
        })
    }
}

/// Numerically stable softplus, including the extreme-inversion region.
fn softplus(x: D) -> D {
    if x.value > 0.0 {
        x + (-x).exp().ln_1p()
    } else {
        x.exp().ln_1p()
    }
}

fn cv_overdrive(b: &Bias, s: &State, delvt: f64, voff: f64) -> D {
    let nvt = s.n * b.thermal;
    let vgst = s.gate_effective - s.threshold;
    let scale = nvt.scale(2.0);
    let x = vgst / scale;
    if x.value.abs() < 100.0 {
        let y = (D::constant(2.0 * voff) - vgst) / scale;
        let log_exp = if y.value > 100.0 {
            (vgst - D::constant(voff)).scale(2.0) / nvt
        } else {
            vgst / nvt
        };
        nvt * softplus(log_exp - D::constant(delvt) / nvt)
    } else {
        s.overdrive
    }
}

fn smooth_flatband(fb: D, gate: D, body: D, delta: f64) -> D {
    let v = fb - gate + body - D::constant(delta);
    let abs_fb = if fb.value <= 0.0 { -fb } else { fb };
    fb - (v + (v * v + abs_fb.scale(4.0 * delta)).sqrt()).scale(0.5)
}

fn depletion(k1: f64, gate_delta: D) -> D {
    if k1 == 0.0 {
        D::constant(0.0)
    } else if gate_delta.value < 0.0 {
        gate_delta
    } else {
        let half = 0.5 * k1;
        ((D::constant(half * half) + gate_delta).sqrt() - D::constant(half)).scale(k1)
    }
}

fn oxide_centroid(oxide: f64, centroid: D) -> D {
    (D::constant(oxide.recip()) + centroid.scale(EPS_SI.recip())).reciprocal()
}

fn smooth_saturation(vsat: D, drain: D) -> D {
    let delta = D::constant(0.02);
    let v = vsat - drain - delta;
    let root = (v * v + vsat.scale(0.08)).sqrt();
    (vsat * drain).scale(2.0) / (vsat + drain + delta + root)
}

fn source_partition(
    partition: f64,
    drive: D,
    drop: D,
    denominator: D,
    gate_cap: D,
    half_charge: D,
) -> D {
    if partition > 0.5 {
        -gate_cap * (drive.scale(0.5) + drop.scale(0.25) - drop * drop / denominator.scale(2.0))
    } else if partition < 0.5 {
        let u = denominator.scale(1.0 / 12.0);
        let polynomial = drive
            * ((drop * drop).scale(2.0 / 3.0) + drive * (drive - drop.scale(4.0 / 3.0)))
            - (drop * drop * drop).scale(2.0 / 15.0);
        -gate_cap.scale(0.5) * polynomial / (u * u)
    } else {
        half_charge.scale(-0.5)
    }
}

struct Inversion {
    gate: D,
    body: D,
    source: D,
}

impl ChargeParameters {
    fn inversion(&self, mode3: bool, drive: D, drain: D, bulk: D, cox: D) -> Inversion {
        let vd = smooth_saturation(drive / bulk, drain);
        let drop = bulk * vd;
        let denominator = (drive - drop.scale(0.5) + D::constant(1e-20)).scale(12.0);
        let gate_cap = cox.scale(self.gate_area);
        let body_cap = cox.scale(self.body_area);
        let body = body_cap * (D::constant(1.0) - bulk) * (vd.scale(0.5) - drop * vd / denominator);
        let gate = if mode3 {
            gate_cap * (drive - drop * (D::constant(0.5) - drop / denominator))
        } else {
            gate_cap * (drive - vd.scale(0.5) + drop * vd / denominator)
        };
        let half_charge = if mode3 { gate } else { gate + body };
        let source = source_partition(
            self.partition,
            drive,
            drop,
            denominator,
            gate_cap,
            half_charge,
        );
        Inversion {
            gate: if mode3 { gate - body } else { gate },
            body,
            source,
        }
    }

    fn centroid_accumulation(&self, gate: D, body: D, flatband: D, oxide: f64) -> D {
        let x = (gate - body - flatband).scale(self.acde / (1e8 * self.cv_tox));
        let raw = if x.value.abs() < 100.0 {
            x.exp().scale(self.debye_length)
        } else {
            D::constant(x.value.clamp(-100.0, 100.0).exp() * self.debye_length)
        };
        let link = 1e-3 * self.cv_tox;
        let v = D::constant(self.debye_length - link) - raw;
        let centroid = D::constant(self.debye_length)
            - (v + (v * v + D::constant(4.0 * link * self.debye_length)).sqrt()).scale(0.5);
        oxide_centroid(oxide, centroid)
    }

    fn centroid_inversion(&self, b: &Bias, s: &State, flatband: D, drive: D, oxide: f64) -> D {
        let field = (drive + (s.threshold - flatband - D::constant(b.phi)).scale(4.0))
            .scale(0.5e-8 / self.cv_tox);
        let field = if field.value <= 1e-20 {
            D::constant(1e-20)
        } else {
            field
        };
        let centroid = D::constant(1.9e-9) / (D::constant(1.0) + field.powf(0.7));
        oxide_centroid(oxide, centroid)
    }
}

impl PreparedPdChannel {
    fn cv_flatband(&self, b: &Bias, s: &State) -> D {
        let k1 = self.electrostatics.effective_k1;
        let p = self.charge;
        let fb = if p.mode == 3 {
            self.threshold.zero_body_flatband(
                &self.threshold_geometry,
                &ThresholdBias {
                    phi: b.phi,
                    body: D::constant(0.0),
                    drain: D::constant(0.0),
                    built_in: b.built_in,
                    ratio_delta: b.ratio_delta,
                    root_adjustment: D::constant(0.0),
                },
                if self.channel.temperature.selfheat {
                    self.tox
                } else {
                    p.cv_tox
                },
            )
        } else {
            s.threshold - D::constant(b.phi) - s.root.scale(k1)
        };
        fb + D::constant(p.delvt)
    }

    fn backgate_charge(&self, b: &Bias, ves: f64) -> D {
        let p = self.charge;
        let fb = if self.channel.temperature.selfheat {
            let doping = self.electrostatics.channel_doping;
            if p.nsub > 0.0 {
                b.thermal.scale(-self.sign * (doping / p.nsub).ln())
            } else {
                let ni = intrinsic_density(b.thermal.scale(K_OVER_Q.recip()), b.thermal);
                b.thermal
                    * (D::constant(-doping * p.nsub) / (ni * ni))
                        .ln()
                        .scale(-self.sign)
            }
        } else {
            D::constant(self.electrostatics.back_flatband)
        };
        (D::constant(ves) - fb - b.body).scale(p.backgate_cap)
    }

    fn quantum_shift(&self, b: &Bias, drive: D) -> D {
        let k1 = self.electrostatics.effective_k1;
        let root_phi = self.electrostatics.phi.sqrt();
        let (root, factor) = if k1 <= 0.0 {
            (0.5 * root_phi, 0.25)
        } else {
            (k1 * root_phi, k1 * k1)
        };
        b.thermal
            * ((D::constant(2.0 * root) + drive) * drive
                / b.thermal.scale(self.charge.moin * factor))
            .ln_1p()
    }

    /// Evaluate intrinsic PD charge for the selected C-V mode and partition.
    /// Bias and temperature follow the same conventions as `evaluate`.
    ///
    /// # Errors
    /// Invalid bias, temperature, accepted surface potential or charge.
    pub fn evaluate_charge(
        &self,
        vgs: f64,
        vds: f64,
        vbs: f64,
        ves: f64,
        kelvin: f64,
        accepted_phi: f64,
    ) -> Result<PdTerminalCharges, ModelError> {
        let b = self.trial_bias(vgs, vds, vbs, kelvin, accepted_phi)?;
        if !ves.is_finite() {
            return Err(ModelError::InvalidPhysicalParameter("PD backgate bias"));
        }
        if self.charge.partition < 0.0 {
            let zero = PdCharge::from_d(D::constant(0.0), 0.0)?;
            return Ok(PdTerminalCharges {
                gate: zero,
                drain: zero,
                source: zero,
                body: zero,
                backgate: zero,
            });
        }
        let s = self.state(&b)?;
        self.charge_at(&b, &s, ves)
    }

    fn charge_at(&self, b: &Bias, s: &State, ves: f64) -> Result<PdTerminalCharges, ModelError> {
        let p = self.charge;
        let mode3 = p.mode == 3;
        let drive = cv_overdrive(b, s, p.delvt, self.voff);
        let flatband = self.cv_flatband(b, s);
        let fbeff = smooth_flatband(
            flatband,
            s.gate_effective,
            s.body,
            if mode3 { 0.02 } else { 0.08 },
        );
        let oxide = self.oxide * self.tox / p.cv_tox;
        let accumulation_cox = if mode3 {
            p.centroid_accumulation(s.gate_effective, s.body, flatband, oxide)
        } else {
            D::constant(oxide)
        };
        let accumulation = accumulation_cox.scale(p.body_area) * (fbeff - flatband);
        let depleted = accumulation_cox.scale(p.body_area)
            * depletion(
                self.electrostatics.effective_k1,
                s.gate_effective - fbeff - s.body - drive,
            );
        let bulk = self.bulk_at(s, D::constant(0.0)).scale(p.bulk_factor);
        let inversion_cox = if mode3 {
            p.centroid_inversion(b, s, flatband, drive, oxide)
        } else {
            D::constant(oxide)
        };
        let inversion_drive = if mode3 {
            drive - self.quantum_shift(b, drive)
        } else {
            drive
        };
        let inv = p.inversion(mode3, inversion_drive, b.drain, bulk, inversion_cox);
        let gate = inv.gate + accumulation + depleted;
        let backgate = self.backgate_charge(b, ves);
        let body = inv.body - accumulation - depleted - backgate;
        let drain = -(gate + body + inv.source + backgate);
        Ok(PdTerminalCharges {
            gate: PdCharge::from_d(gate, 0.0)?,
            drain: PdCharge::from_d(drain, 0.0)?,
            source: PdCharge::from_d(inv.source, 0.0)?,
            body: PdCharge::from_d(body, -p.backgate_cap)?,
            backgate: PdCharge::from_d(backgate, p.backgate_cap)?,
        })
    }
}
