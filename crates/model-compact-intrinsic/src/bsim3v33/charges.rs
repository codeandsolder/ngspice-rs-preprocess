// BSIM3v3.3.0 numerical extraction from codeandsolder/ngspice-rs-fast
// revision 439ea0903380e7f5e2a651653f595150bad61e9e. See package LICENSE.
//! BSIM3v3.3 charge (C-V) model, a port of `b3ld.c`'s charge block.
//!
//! The supported charge path is BSIM3 `capMod == 3`.
//!
//! This ports the `capMod == 3` path (C's default, the "charge thickness"
//! model): the C-V preamble (`VbseffCV`, `CoxWL`, the `noff`/`voffcv`
//! `Vgsteff`), the CTM charge/capacitance block, bias-dependent overlap
//! capacitances for `capMod >= 2`, and source/drain junction C-V. `capMod` 0,
//! 1 and 2 and every NQS mode are explicit refusals: each is a different
//! equation set, not a tuning of this one.
//!
//! Every output keeps C's sign convention: charges on the gate, bulk and
//! drain and the nine `c*xb` capacitance derivatives, before the multiplicity
//! multiplier and before the overlap-charge adjustment the caller performs.
#![expect(
    clippy::similar_names,
    reason = "C-translated equation symbols keep their original distinct names for source-level audit"
)]
#![expect(
    clippy::float_cmp,
    reason = "C device branches compare floating-point values exactly, including nonfinite cases"
)]
#![expect(
    clippy::too_many_lines,
    reason = "a complete translated C numerical routine remains contiguous for equation parity review"
)]

use super::dc::{Bias, DcEvaluation};
use super::defaults::ModelDefaults;
use super::setup::InstanceSetup;
use super::temperature::TemperatureModel;
use super::{Error, Result};

const EXP_THRESHOLD: f64 = 34.0;
const MIN_EXP: f64 = 1.713_908_431e-15;
const MAX_EXP: f64 = 5.834_617_425e14;
/// `b3ld.c` `EPSSI` (rounded, not the platform's permittivity).
const EPS_SI: f64 = 1.035_94e-10;
/// `b3ld.c` `DELTA_1`.
const DELTA_1: f64 = 0.02;
/// `b3ld.c` `DELTA_3`.
const DELTA_3: f64 = 0.02;
/// `b3ld.c` `DELTA_4`.
const DELTA_4: f64 = 0.02;

/// Intrinsic charges, the nine capacitance derivatives and the junction C-V
/// of one `b3ld.c` charge evaluation at a fixed bias.
///
/// Charges are C's instance values before the multiplicity multiplier and
/// before the overlap-charge (`qgdo`/`qgso`/`cgbo`) adjustment: `qgate`,
/// `qbulk` and `qdrn` are the CTM results, exactly as C has them when the
/// `capMod == 3` block ends. `qbs`/`qbd` are C's `CKTstate0` junction charges.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChargeEvaluation {
    /// Intrinsic gate charge (C `qgate` at the end of the CTM block).
    pub qgate: f64,
    /// Intrinsic bulk charge (C `qbulk`).
    pub qbulk: f64,
    /// Intrinsic drain charge (C `qdrn`).
    pub qdrn: f64,
    /// Intrinsic source charge (C `qsrc`, from the XPART partition).
    pub qsrc: f64,
    /// C `here->BSIM3cggb`.
    pub cggb: f64,
    /// C `here->BSIM3cgdb`.
    pub cgdb: f64,
    /// C `here->BSIM3cgsb`.
    pub cgsb: f64,
    /// C `here->BSIM3cdgb`.
    pub cdgb: f64,
    /// C `here->BSIM3cddb`.
    pub cddb: f64,
    /// C `here->BSIM3cdsb`.
    pub cdsb: f64,
    /// C `here->BSIM3cbgb`.
    pub cbgb: f64,
    /// C `here->BSIM3cbdb`.
    pub cbdb: f64,
    /// C `here->BSIM3cbsb`.
    pub cbsb: f64,
    /// Bias-dependent gate-drain overlap capacitance (C `here->BSIM3cgdo`).
    pub cgdo: f64,
    /// Bias-dependent gate-source overlap capacitance (C `here->BSIM3cgso`).
    pub cgso: f64,
    /// Gate-bulk overlap capacitance (C `pParam->BSIM3cgbo`).
    pub cgbo: f64,
    /// Gate-drain overlap charge (C `qgdo`), for the gate/drain adjustment.
    pub qgdo: f64,
    /// Gate-source overlap charge (C `qgso`).
    pub qgso: f64,
    /// Source junction capacitance (C `here->BSIM3capbs`).
    pub capbs: f64,
    /// Drain junction capacitance (C `here->BSIM3capbd`).
    pub capbd: f64,
    /// Source junction charge (C `CKTstate0[here->BSIM3qbs]`).
    pub qbs: f64,
    /// Drain junction charge (C `CKTstate0[here->BSIM3qbd]`).
    pub qbd: f64,
    /// Inversion charge for the noise model (C `here->BSIM3qinv`).
    pub qinv: f64,
}

impl ChargeEvaluation {
    /// C's overlap-charge adjustment and `qd`/`qb` derivation, in C's order.
    ///
    /// `mode` is the DC mode (>= 0 normal, < 0 inverse). Returns
    /// `CKTstate0`'s `qg`, `qd` and `qb` (C's `qgate`/`qdrn` are updated in
    /// place because the terminal currents read them).
    #[must_use = "the returned charges include the overlap contributions and terminal-mode redistribution"]
    pub fn with_overlap(&self, mode: i8, vgb: f64) -> OverlapCharges {
        let qgd = self.qgdo;
        let qgs = self.qgso;
        let qgb = self.cgbo * vgb;
        let qgate = self.qgate + qgd + qgs + qgb;
        let qbulk = self.qbulk - qgb;
        let (qdrn, qsrc) = if mode >= 0 {
            let qdrn = self.qdrn - qgd;
            (qdrn, -(qgate + qbulk + qdrn))
        } else {
            let qsrc = self.qdrn - qgs;
            (-(qgate + qbulk + qsrc), qsrc)
        };
        OverlapCharges {
            qgate,
            qbulk,
            qdrn,
            qsrc,
            // C stores the state after the adjustment; the junction charges
            // are already on the bulk/drain nodes.
            qg: qgate,
            qd: qdrn - self.qbd,
            qb: qbulk + self.qbd + self.qbs,
        }
    }
}

/// C's `qgate`/`qbulk`/`qdrn`/`qsrc` after the overlap-charge adjustment and
/// the `CKTstate0` charge values `b3ask.c` reports.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OverlapCharges {
    /// Gate charge including overlaps and gate-bulk.
    pub qgate: f64,
    /// Bulk charge including gate-bulk.
    pub qbulk: f64,
    /// Drain charge including the gate-drain overlap.
    pub qdrn: f64,
    /// Source charge (C solves it from the node-charge sum).
    pub qsrc: f64,
    /// C `CKTstate0[here->BSIM3qg]`.
    pub qg: f64,
    /// C `CKTstate0[here->BSIM3qd]`.
    pub qd: f64,
    /// C `CKTstate0[here->BSIM3qb]`.
    pub qb: f64,
}

/// The BSIM3 charge core for one defaulted model and one set-up instance.
#[derive(Debug, Clone, Copy)]
pub struct ChargeModel<'a> {
    /// The defaulted model (`b3set.c`).
    pub model: &'a ModelDefaults,
    /// The model-level temperature pass (`b3temp.c`).
    pub temperature: &'a TemperatureModel,
    /// This instance's effective parameters (`b3temp.c`).
    pub setup: &'a InstanceSetup,
}

impl ChargeModel<'_> {
    /// Evaluate the charge model at `bias`, reusing the DC evaluation's
    /// intermediates.
    ///
    /// # Errors
    /// A `capMod` other than 3 or any NQS mode (`NotYetPorted` naming the C
    /// file), or a nonfinite result.
    pub fn evaluate(&self, dc: &DcEvaluation, bias: Bias) -> Result<ChargeEvaluation> {
        let cap_mod = self.model.values.capmod.value;
        if cap_mod != 3 {
            return Err(Error::unsupported(
                format!(
                    "BSIM3 capMod={cap_mod} charge model (only capMod=3 is ported; \
                     set capMod=3 or omit it)"
                ),
                "src/spicelib/devices/bsim3/b3ld.c (capMod 0/1/2 charge blocks)",
            ));
        }
        if self.setup.nqs_mod != 0 || self.setup.acnqs_mod != 0 {
            return Err(Error::unsupported(
                "BSIM3 NQS charge model (nqsMod/acnqsMod)",
                "src/spicelib/devices/bsim3/b3ld.c (NQS charge partition and qdef)",
            ));
        }
        let p = self.setup;
        let m = self.model;
        let t = self.temperature;
        let cv = &dc.cv;
        // C's junction C-V and overlap charges use the raw terminal bias;
        // the CTM block uses the mode-selected magnitudes (`Vgs`, `Vds`).
        let vbs = bias.vbs;
        let vgs = bias.vgs;
        let vds = bias.vds;
        let vds_mode = dc.vds;

        // ---- b3ld.c C-V preamble: VbseffCV, CoxWL and the noff VgsteffCV.
        let (vbseff_cv, dvbseff_cv_dvb) = if cv.vbseff < 0.0 {
            (cv.vbseff, 1.0)
        } else {
            // C `VbseffCV = phi - Phis`.
            (cv.phi - cv.phis, -cv.d_phis_dvb)
        };
        let cox = m.oxide_capacitance;
        let cox_wl = cox * p.weff_cv * p.leff_cv;

        let noff = cv.n * p.binned.noff;
        let dnoff_dvd = p.binned.noff * cv.d_n_dvd;
        let dnoff_dvb = p.binned.noff * cv.d_n_dvb;
        let t0 = cv.vtm * noff;
        let voffcv = p.binned.voffcv;
        let vgst_nvt = (cv.vgst - voffcv) / t0;
        let (vgsteff, dvgsteff_dvg, dvgsteff_dvd, dvgsteff_dvb) = if vgst_nvt > EXP_THRESHOLD {
            (
                cv.vgst - voffcv,
                cv.d_vgs_eff_dvg,
                -cv.d_vth_dvd,
                -cv.d_vth_dvb,
            )
        } else if vgst_nvt < -EXP_THRESHOLD {
            let vgsteff = t0 * MIN_EXP.ln_1p();
            let dvd = vgsteff / noff;
            (vgsteff, 0.0, dvd * dnoff_dvd, dvd * dnoff_dvb)
        } else {
            let exp_vgst = vgst_nvt.exp();
            let vgsteff = t0 * exp_vgst.ln_1p();
            let dvg = exp_vgst / (1.0 + exp_vgst);
            let dvd = (vgsteff / noff).mul_add(
                dnoff_dvd,
                -dvg * ((cv.vgst - voffcv) / noff).mul_add(dnoff_dvd, cv.d_vth_dvd),
            );
            let dvb = (vgsteff / noff).mul_add(
                dnoff_dvb,
                -dvg * ((cv.vgst - voffcv) / noff).mul_add(dnoff_dvb, cv.d_vth_dvb),
            );
            (vgsteff, dvg * cv.d_vgs_eff_dvg, dvd, dvb)
        };

        // ---- capMod == 3, the charge-thickness model (CTM).
        let vfbzb = p.vfbzb;
        let v3 = vfbzb - cv.vgs_eff + vbseff_cv - DELTA_3;
        let discriminant_addend = 4.0 * DELTA_3 * vfbzb.abs();
        let t0 = v3.mul_add(v3, discriminant_addend).sqrt();
        let (flatband_drop, t1) = if v3 < 0.0 {
            let conjugate = t0 - v3;
            let drop = 0.5 * discriminant_addend / conjugate;
            (drop, drop / t0)
        } else {
            (f64::midpoint(v3, t0), f64::midpoint(1.0, v3 / t0))
        };
        let vfbeff = vfbzb - flatband_drop;
        let dvfbeff_dvg = t1 * cv.d_vgs_eff_dvg;
        let dvfbeff_dvb = -t1 * dvbseff_cv_dvb;

        let tox = m.values.tox.value;
        // C `Tox = 1.0e8 * model->BSIM3tox` (centimetres for the ACDE exponent).
        let mut tox_cm = 1.0e8 * tox;
        let t0 = (cv.vgs_eff - vbseff_cv - vfbzb) / tox_cm;
        let dt0_dvg = cv.d_vgs_eff_dvg / tox_cm;
        let dt0_dvb = -dvbseff_cv_dvb / tox_cm;

        let acde = p.binned.acde;
        let ldeb = p.derived.ldeb;
        let tmp = t0 * acde;
        let (tcen, mut dtcen_dvg, mut dtcen_dvb) = if tmp > -EXP_THRESHOLD && tmp < EXP_THRESHOLD {
            let tcen = ldeb * tmp.exp();
            let mut dvg = acde * tcen;
            let dvb = dvg * dt0_dvb;
            dvg *= dt0_dvg;
            (tcen, dvg, dvb)
        } else if tmp <= -EXP_THRESHOLD {
            (ldeb * MIN_EXP, 0.0, 0.0)
        } else {
            (ldeb * MAX_EXP, 0.0, 0.0)
        };

        let link = 1.0e-3 * tox;
        // C reuses V3/V4/T1 here, after the Vfb-eff computation.
        let v3 = ldeb - tcen - link;
        let v4 = (4.0 * link).mul_add(ldeb, v3 * v3).sqrt();
        let tcen = if ldeb > 0.0 && tcen >= 0.0 {
            tcen * (2.0 * (ldeb / (ldeb + tcen + link + v4)))
        } else {
            ldeb - f64::midpoint(v3, v4)
        };
        let t1 = f64::midpoint(1.0, v3 / v4);
        dtcen_dvg *= t1;
        dtcen_dvb *= t1;

        let ccen = EPS_SI / tcen;
        let t2 = cox / (cox + ccen);
        let coxeff = t2 * ccen;
        let t3 = -ccen / tcen;
        let mut dcoxeff_dvg = t2 * t2 * t3;
        let dcoxeff_dvb = dcoxeff_dvg * dtcen_dvb;
        dcoxeff_dvg *= dtcen_dvg;
        let cox_wl_cen = cox_wl * coxeff / cox;

        let qac0 = -cox_wl_cen * flatband_drop;
        let qov_cox = qac0 / coxeff;
        let dqac0_dvg = qov_cox.mul_add(dcoxeff_dvg, cox_wl_cen * dvfbeff_dvg);
        let dqac0_dvb = qov_cox.mul_add(dcoxeff_dvb, cox_wl_cen * dvfbeff_dvb);

        // Poly-gate depletion share of the bulk charge.
        let k1ox = p.derived.k1ox;
        let t0 = 0.5 * k1ox;
        let t3 = cv.vgs_eff - vfbeff - vbseff_cv - vgsteff;
        let (t1, t2) = if k1ox == 0.0 {
            (0.0, 0.0)
        } else if t3 < 0.0 {
            (t0 + t3 / k1ox, cox_wl_cen)
        } else {
            let t1 = t0.mul_add(t0, t3).sqrt();
            (t1, cox_wl_cen * t0 / t1)
        };

        let potential_drop = if k1ox == 0.0 {
            0.0
        } else if t3 < 0.0 {
            t3 / k1ox
        } else if t0 >= 0.0 {
            t3 / (t1 + t0)
        } else {
            t1 - t0
        };
        let qsub0 = cox_wl_cen * k1ox * potential_drop;
        let qov_cox = qsub0 / coxeff;
        let dqsub0_dvg = qov_cox.mul_add(
            dcoxeff_dvg,
            t2 * (cv.d_vgs_eff_dvg - dvfbeff_dvg - dvgsteff_dvg),
        );
        let dqsub0_dvd = -t2 * dvgsteff_dvd;
        let dqsub0_dvb = qov_cox.mul_add(
            dcoxeff_dvb,
            -t2 * (dvfbeff_dvb + dvbseff_cv_dvb + dvgsteff_dvb),
        );

        // Gate-bias-dependent surface potential (C's "Delta Phis").
        let (denomi, t0) = if k1ox <= 0.0 {
            (0.25 * p.binned.moin * cv.vtm, 0.5 * p.derived.sqrtphi)
        } else {
            (
                p.binned.moin * cv.vtm * k1ox * k1ox,
                k1ox * p.derived.sqrtphi,
            )
        };
        let t1 = 2.0f64.mul_add(t0, vgsteff);
        let delta_phi = cv.vtm * (t1 * vgsteff / denomi).ln_1p();
        let ddelta_phi_dvg = 2.0 * cv.vtm * (t1 - t0) / f64::mul_add(t1, vgsteff, denomi);

        // Effective gate overdrive, `VgDP`.
        let t0 = vgsteff - delta_phi - 0.001;
        let dt0_dvg = 1.0 - ddelta_phi_dvg;
        let t1 = f64::mul_add(vgsteff, 0.004, t0 * t0).sqrt();
        let vgdp = if t0 < 0.0 {
            0.002 * vgsteff / (t1 - t0)
        } else {
            f64::midpoint(t0, t1)
        };
        let dvgdp_dvg = f64::midpoint(dt0_dvg, t0.mul_add(dt0_dvg, 0.002) / t1);

        // Bias-dependent channel thickness for the inversion charge.
        let t3 = 4.0 * (cv.vth - vfbzb - cv.phi);
        tox_cm += tox_cm;
        let (t0, dt0_dvd, dt0_dvb) = if t3 >= 0.0 {
            (
                (vgsteff + t3) / tox_cm,
                4.0f64.mul_add(cv.d_vth_dvd, dvgsteff_dvd) / tox_cm,
                4.0f64.mul_add(cv.d_vth_dvb, dvgsteff_dvb) / tox_cm,
            )
        } else {
            (
                (vgsteff + 1.0e-20) / tox_cm,
                dvgsteff_dvd / tox_cm,
                dvgsteff_dvb / tox_cm,
            )
        };
        let tmp = (0.7 * t0.ln()).exp();
        let t1 = 1.0 + tmp;
        let t2 = 0.7 * tmp / (t0 * tox_cm);
        let tcen = 1.9e-9 / t1;
        let mut dtcen_dvg2 = -1.9e-9 * t2 / t1 / t1;
        let mut dtcen_dvd = tox_cm * dtcen_dvg2;
        let dtcen_dvb2 = dtcen_dvd * dt0_dvb;
        dtcen_dvd *= dt0_dvd;
        dtcen_dvg2 *= dvgsteff_dvg;

        let ccen = EPS_SI / tcen;
        let t0 = cox / (cox + ccen);
        let coxeff = t0 * ccen;
        let t1 = -ccen / tcen;
        let mut dcoxeff_dvg = t0 * t0 * t1;
        let dcoxeff_dvd = dcoxeff_dvg * dtcen_dvd;
        let dcoxeff_dvb = dcoxeff_dvg * dtcen_dvb2;
        dcoxeff_dvg *= dtcen_dvg2;
        let cox_wl_cen = cox_wl * coxeff / cox;

        // Effective drain voltage for C-V.
        let abulk_cv = cv.abulk0 * p.thermal.abulkcvfactor;
        let dabulk_cv_dvb = p.thermal.abulkcvfactor * cv.d_abulk0_dvb;
        let vdsat_cv = vgdp / abulk_cv;
        let t0 = vdsat_cv - vds_mode - DELTA_4;
        let dt0_dvg = dvgdp_dvg / abulk_cv;
        let dt0_dvb = -vdsat_cv * dabulk_cv_dvb / abulk_cv;
        let t1 = (4.0 * DELTA_4).mul_add(vdsat_cv, t0 * t0).sqrt();
        let dt1_dvg0 = (t0 + DELTA_4 + DELTA_4) / t1;
        let dt1_dvd = -t0 / t1;
        let dt1_dvb0 = dt1_dvg0 * dt0_dvb;
        let dt1_dvg = dt1_dvg0 * dt0_dvg;
        let (mut vdseff_cv, mut dvdseff_cv_dvg, dvdseff_cv_dvd, mut dvdseff_cv_dvb) = if t0 >= 0.0 {
            (
                if vdsat_cv > 0.0 {
                    vds_mode * (2.0 * (vdsat_cv / (vdsat_cv + vds_mode + DELTA_4 + t1)))
                } else {
                    vdsat_cv - f64::midpoint(t0, t1)
                },
                0.5 * (dt0_dvg - dt1_dvg),
                0.5 * (1.0 - dt1_dvd),
                0.5 * (dt0_dvb - dt1_dvb0),
            )
        } else {
            let twice_delta = DELTA_4 + DELTA_4;
            let t3 = twice_delta / (t1 - t0);
            let t4 = 1.0 - t3;
            let t5 = vdsat_cv * t3 / (t1 - t0);
            (
                vdsat_cv * t4,
                t5.mul_add(dt1_dvg - dt0_dvg, dt0_dvg * t4),
                t5 * (dt1_dvd + 1.0),
                t5.mul_add(dt1_dvb0, dt0_dvb * (1.0 - t5)),
            )
        };
        // C zeroes VdseffCV's gate/body derivatives at Vds = 0 (not the drain
        // one, which the published code leaves alone).
        if vds_mode == 0.0 {
            vdseff_cv = 0.0;
            dvdseff_cv_dvg = 0.0;
            dvdseff_cv_dvb = 0.0;
        }

        // Gate charge and its derivatives.
        let t0 = abulk_cv * vdseff_cv;
        let t1 = vgdp;
        let t2 = 12.0 * (0.5f64.mul_add(-t0, t1) + 1.0e-20);
        let t3 = t0 / t2;
        let t4 = (12.0 * t3).mul_add(-t3, 1.0);
        let t5 = abulk_cv * (6.0 * t0 * 4.0f64.mul_add(t1, -t0) / (t2 * t2) - 0.5);
        let t6 = t5 * vdseff_cv / abulk_cv;

        let qgate = cox_wl_cen * t0.mul_add(-(0.5 - t3), t1);
        let qinoi = qgate;
        let qov_cox = qgate / coxeff;
        let mut cgg1 = cox_wl_cen * t5.mul_add(dvdseff_cv_dvg, t4 * dvgdp_dvg);
        let cgd1 = qov_cox.mul_add(
            dcoxeff_dvd,
            cgg1.mul_add(dvgsteff_dvd, cox_wl_cen * t5 * dvdseff_cv_dvd),
        );
        let cgb1 = qov_cox.mul_add(
            dcoxeff_dvb,
            cgg1.mul_add(
                dvgsteff_dvb,
                cox_wl_cen * t6.mul_add(dabulk_cv_dvb, t5 * dvdseff_cv_dvb),
            ),
        );
        cgg1 = qov_cox.mul_add(dcoxeff_dvg, cgg1 * dvgsteff_dvg);

        // Bulk charge and its derivatives.
        let t7 = 1.0 - abulk_cv;
        let t8 = t2 * t2;
        let t9 = 12.0 * t7 * t0 * t0 / (t8 * abulk_cv);
        let t10 = t9 * dvgdp_dvg;
        let t11 = -t7 * t5 / abulk_cv;
        let t12 = -f64::mul_add(vdseff_cv, 0.5 - t0 / t2, t9 * t1 / abulk_cv);

        let qbulk = cox_wl_cen * t7 * 0.5f64.mul_add(vdseff_cv, -(t0 * vdseff_cv / t2));
        let qov_cox = qbulk / coxeff;
        let mut cbg1 = cox_wl_cen * t11.mul_add(dvdseff_cv_dvg, t10);
        let cbd1 = qov_cox.mul_add(
            dcoxeff_dvd,
            cbg1.mul_add(dvgsteff_dvd, cox_wl_cen * t11 * dvdseff_cv_dvd),
        );
        let cbb1 = qov_cox.mul_add(
            dcoxeff_dvb,
            cbg1.mul_add(
                dvgsteff_dvb,
                cox_wl_cen * t12.mul_add(dabulk_cv_dvb, t11 * dvdseff_cv_dvb),
            ),
        );
        cbg1 = qov_cox.mul_add(dcoxeff_dvg, cbg1 * dvgsteff_dvg);

        // Source charge partition (XPART: > 0.5 = 0/100, < 0.5 = 40/60).
        let xpart = m.values.xpart.value;
        let (qsrc, csg, csd, mut csb);
        if xpart > 0.5 {
            let qsrc0 = -cox_wl_cen * (t1 / 2.0 + t0 / 4.0 - 0.5 * t0 * t0 / t2);
            let qov_cox = qsrc0 / coxeff;
            let t2 = t2 + t2;
            let t3 = t2 * t2;
            let t7p = -(0.25 - 12.0 * t0 * 4.0f64.mul_add(t1, -t0) / t3);
            let t4p = -(0.5 + 24.0 * t0 * t0 / t3) * dvgdp_dvg;
            let t5p = t7p * abulk_cv;
            let t6p = t7p * vdseff_cv;
            let csg0 = cox_wl_cen * t5p.mul_add(dvdseff_cv_dvg, t4p);
            let csd0 = qov_cox.mul_add(
                dcoxeff_dvd,
                csg0.mul_add(dvgsteff_dvd, cox_wl_cen * t5p * dvdseff_cv_dvd),
            );
            let csb0 = qov_cox.mul_add(
                dcoxeff_dvb,
                csg0.mul_add(
                    dvgsteff_dvb,
                    cox_wl_cen * t6p.mul_add(dabulk_cv_dvb, t5p * dvdseff_cv_dvb),
                ),
            );
            let csg1 = qov_cox.mul_add(dcoxeff_dvg, csg0 * dvgsteff_dvg);
            qsrc = qsrc0;
            csg = csg1;
            csd = csd0;
            csb = csb0;
        } else if xpart < 0.5 {
            // 40/60 partition, C's default (`b3set.c` xpart = 0).
            let t2 = t2 / 12.0;
            let t3 = 0.5 * cox_wl_cen / (t2 * t2);
            let t4 = f64::mul_add(
                t1,
                f64::mul_add(t1, t1 - 4.0 * t0 / 3.0, 2.0 * t0 * t0 / 3.0),
                -(2.0 * t0 * t0 * t0 / 15.0),
            );
            let qsrc0 = -t3 * t4;
            let qov_cox = qsrc0 / coxeff;
            let t8 = (0.4 * t0).mul_add(t0, 4.0 / 3.0 * t1 * (t1 - t0));
            let t5 = t3.mul_add(
                -f64::mul_add(
                    t1,
                    3.0f64.mul_add(t1, -(8.0 * t0 / 3.0)),
                    2.0 * t0 * t0 / 3.0,
                ),
                -2.0 * qsrc0 / t2,
            );
            let t6 = abulk_cv * t3.mul_add(t8, qsrc0 / t2);
            let t7 = t6 * vdseff_cv / abulk_cv;
            let csg0 = t6.mul_add(dvdseff_cv_dvg, t5 * dvgdp_dvg);
            let csd0 =
                qov_cox.mul_add(dcoxeff_dvd, t6.mul_add(dvdseff_cv_dvd, csg0 * dvgsteff_dvd));
            let csb0 = qov_cox.mul_add(
                dcoxeff_dvb,
                t7.mul_add(
                    dabulk_cv_dvb,
                    t6.mul_add(dvdseff_cv_dvb, csg0 * dvgsteff_dvb),
                ),
            );
            let csg1 = qov_cox.mul_add(dcoxeff_dvg, csg0 * dvgsteff_dvg);
            qsrc = qsrc0;
            csg = csg1;
            csd = csd0;
            csb = csb0;
        } else {
            let qsrc0 = -0.5 * qgate;
            qsrc = qsrc0;
            csg = -0.5 * cgg1;
            csd = -0.5 * cgd1;
            csb = -0.5 * cgb1;
        }

        // Fold the accumulation/subthreshold charges into gate and bulk.
        let qgate = qgate + qac0 + qsub0 - qbulk;
        let qbulk = qbulk - (qac0 + qsub0);
        let qdrn = -(qgate + qbulk + qsrc);

        let cbg = cbg1 - dqac0_dvg - dqsub0_dvg;
        let cbd = cbd1 - dqsub0_dvd;
        let mut cbb = cbb1 - dqac0_dvb - dqsub0_dvb;

        let cgg = cgg1 - cbg;
        let cgd = cgd1 - cbd;
        let mut cgb = cgb1 - cbb;
        cgb *= cv.d_vbseff_dvb;
        cbb *= cv.d_vbseff_dvb;
        csb *= cv.d_vbseff_dvb;

        let cggb = cgg;
        let cgsb = -(cgg + cgd + cgb);
        let cgdb = cgd;
        let cdgb = -(cgg + cbg + csg);
        let cdsb = cgg + cgd + cgb + cbg + cbd + cbb + csg + csd + csb;
        let cddb = -(cgd + cbd + csd);
        let cbgb = cbg;
        let cbsb = -(cbg + cbd + cbb);
        let cbdb = cbd;

        // ---- Overlap capacitances (capMod 3 uses the capMod >= 2 branch).
        let cgdo_param = p.cgdo;
        let cgso_param = p.cgso;
        let ck = p.binned.ckappa;
        let vgd = vgs - vds;

        let (cgdo, qgdo) = overlap_cv(vgd, cgdo_param, p.weff_cv * p.binned.cgdl, ck);
        let (cgso, qgso) = overlap_cv(vgs, cgso_param, p.weff_cv * p.binned.cgsl, ck);

        let cgbo = p.derived.cgbo;

        // ---- Junction C-V (b3ld.c "bulk-drain and bulk-source depletion
        // capacitances"). C uses the un-temperature-scaled gate-sidewall field
        // (`BSIM3unitLengthGateSidewallJctCap`, not `...TempJctCap`); that
        // quirk is reproduced.
        let (czbd, czbdsw, czbdswg) =
            junction_zero_bias(t, m, p.drain_area, p.drain_perimeter, p.weff);
        let (czbs, czbssw, czbsswg) =
            junction_zero_bias(t, m, p.source_area, p.source_perimeter, p.weff);
        let mj = m.values.mj.value;
        let mjsw = m.values.mjsw.value;
        let mjswg = m.values.mjswg.value;
        let phib = t.phi_b;
        let phibsw = t.phi_bsw;
        let phibswg = t.phi_bswg;

        let vbd = vbs - vds;
        let (qbs, capbs) = junction_cv(
            vbs, czbs, czbssw, czbsswg, mj, mjsw, mjswg, phib, phibsw, phibswg,
        );
        let (qbd, capbd) = junction_cv(
            vbd, czbd, czbdsw, czbdswg, mj, mjsw, mjswg, phib, phibsw, phibswg,
        );

        let result = ChargeEvaluation {
            qgate,
            qbulk,
            qdrn,
            qsrc,
            cggb,
            cgdb,
            cgsb,
            cdgb,
            cddb,
            cdsb,
            cbgb,
            cbdb,
            cbsb,
            cgdo,
            cgso,
            cgbo,
            qgdo,
            qgso,
            capbs,
            capbd,
            qbs,
            qbd,
            qinv: -qinoi,
        };
        for (name, value) in [
            ("qgate", result.qgate),
            ("qbulk", result.qbulk),
            ("qdrn", result.qdrn),
            ("qsrc", result.qsrc),
            ("qbs", result.qbs),
            ("qbd", result.qbd),
            ("capbs", result.capbs),
            ("capbd", result.capbd),
            ("cggb", result.cggb),
            ("cgdb", result.cgdb),
            ("cgsb", result.cgsb),
            ("cdgb", result.cdgb),
            ("cddb", result.cddb),
            ("cdsb", result.cdsb),
            ("cbgb", result.cbgb),
            ("cbdb", result.cbdb),
            ("cbsb", result.cbsb),
            ("cgdo", result.cgdo),
            ("cgso", result.cgso),
        ] {
            if !value.is_finite() {
                return Err(Error::Numerical {
                    context: "BSIM3 charge evaluation (bsim3/b3ld.c)".into(),
                    message: format!("{name} is not finite"),
                });
            }
        }
        Ok(result)
    }
}

/// `b3ld.c`'s zero-bias junction capacitances for one junction, split into the
/// bottom, sidewall and gate-sidewall parts.
fn junction_zero_bias(
    temperature: &TemperatureModel,
    model: &ModelDefaults,
    area: f64,
    perimeter: f64,
    weff: f64,
) -> (f64, f64, f64) {
    let czb = temperature.unit_area_temp_jct_cap * area;
    let sidewall = temperature.unit_length_sidewall_temp_jct_cap;
    // C reads the *model* field here (no CJSWG temperature scaling).
    let gate_sidewall = model.values.cjswg.value;
    if perimeter < weff {
        (czb, 0.0, gate_sidewall * perimeter)
    } else {
        (czb, sidewall * (perimeter - weff), gate_sidewall * weff)
    }
}

/// Bias-dependent overlap charge and its derivative. Rationalized
/// square-root offsets retain the small correction for large positive bias.
fn overlap_cv(v: f64, base: f64, extra: f64, kappa: f64) -> (f64, f64) {
    let offset = v + DELTA_1;
    let root = offset.mul_add(offset, 4.0 * DELTA_1).sqrt();
    let negative_half = if offset > 0.0 {
        -2.0 * DELTA_1 / (offset + root)
    } else {
        0.5 * (offset - root)
    };
    let ratio = -4.0 * negative_half / kappa;
    let secondary = (1.0 + ratio).sqrt();
    let secondary_offset = ratio / (secondary + 1.0);
    let correction = secondary_offset / secondary * (-negative_half / root);
    let capacitance = extra.mul_add(-correction, base + extra);
    let charge = extra.mul_add(
        -negative_half * (secondary_offset / (secondary + 1.0)),
        (base + extra) * v,
    );
    (capacitance, charge)
}

/// `b3ld.c`'s source/drain bulk junction C-V for one junction, returning
/// `(charge, capacitance)`.
#[expect(
    clippy::too_many_arguments,
    reason = "keeps the positional interface explicit at the reference solver or device boundary"
)] // C's three junction regions.
fn junction_cv(
    v: f64,
    czb: f64,
    czsw: f64,
    czswg: f64,
    mj: f64,
    mjsw: f64,
    mjswg: f64,
    phib: f64,
    phibsw: f64,
    phibswg: f64,
) -> (f64, f64) {
    if v == 0.0 {
        (0.0, czb + czsw + czswg)
    } else if v < 0.0 {
        let mut charge = 0.0;
        let mut cap = 0.0;
        for (cz, m, phi) in [
            (czb, mj, phib),
            (czsw, mjsw, phibsw),
            (czswg, mjswg, phibswg),
        ] {
            if cz > 0.0 {
                let relative = -v / phi;
                let arg = 1.0 + relative;
                let (sarg, integral) = if m == 0.5 {
                    let root = arg.sqrt();
                    // 2*(1 - sqrt(1+x)) = -2*x/(1 + sqrt(1+x)).
                    (1.0 / root, -2.0 * (relative / (1.0 + root)))
                } else {
                    let log = relative.ln_1p();
                    let exponent = 1.0 - m;
                    ((-m * log).exp(), -(exponent * log).exp_m1() / exponent)
                };
                charge = f64::mul_add(phi * cz, integral, charge);
                cap = cz.mul_add(sarg, cap);
            }
        }
        (charge, cap)
    } else {
        let t0 = czb + czsw + czswg;
        let t1 = v * (czb * mj / phib + czsw * mjsw / phibsw + czswg * mjswg / phibswg);
        (v * 0.5f64.mul_add(t1, t0), t0 + t1)
    }
}
