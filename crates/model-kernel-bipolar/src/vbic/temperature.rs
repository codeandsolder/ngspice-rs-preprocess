//! Complete, source-indexed VBIC 1.1.5 temperature preparation.
//!
//! Follows the ngspice VBIC temperature source on all 108 coefficients.
//! The index enum is a compile-time bridge to the public VBIC parameter
//! names, not a per-Newton model-string lookup. This does not apply ngspice
//! parser defaults, minimum resistance clamps, or instance geometry scaling:
//! those must be source-aware stages before device binding.

use super::{avalanche, charge, depletion, noise, parasitic, resistance, transport};
use crate::{ModelError, Temperature, validate_positive};

/// All 108 VBIC temperature-preparation coefficients in source order.
///
/// Model-card defaults and source-aware parameter name resolution are
/// responsibilities of the upstream reader; model evaluation is type safe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Coefficient {
    Tnom,
    Rcx,
    Rci,
    Vo,
    Gamm,
    Hrcf,
    Rbx,
    Rbi,
    Re,
    Rs,
    Rbp,
    Is,
    Nf,
    Nr,
    Fc,
    Cbeo,
    Cje,
    Pe,
    Me,
    Aje,
    Cbco,
    Cjc,
    Qco,
    Cjep,
    Pc,
    Mc,
    Ajc,
    Cjcp,
    Ps,
    Ms,
    Ajs,
    Ibei,
    Wbe,
    Nei,
    Iben,
    Nen,
    Ibci,
    Nci,
    Ibcn,
    Ncn,
    Avc1,
    Avc2,
    Isp,
    Wsp,
    Nfp,
    Ibeip,
    Ibenp,
    Ibcip,
    Ncip,
    Ibcnp,
    Ncnp,
    Vef,
    Ver,
    Ikf,
    Ikr,
    Ikp,
    Tf,
    Qtf,
    Xtf,
    Vtf,
    Itf,
    Tr,
    Td,
    Kfn,
    Afn,
    Bfn,
    Xre,
    Xrbi,
    Xrci,
    Xrs,
    Xvo,
    Ea,
    Eaie,
    Eaic,
    Eais,
    Eane,
    Eanc,
    Eans,
    Xis,
    Xii,
    Xin,
    Tnf,
    Tavc,
    Rth,
    Cth,
    Vrt,
    Art,
    Ccso,
    Qbm,
    Nkf,
    Xikf,
    Xrcx,
    Xrbx,
    Xrbp,
    Isrr,
    Xisr,
    Dear,
    Eap,
    Vbbe,
    Nbbe,
    Ibbe,
    Tvbbe1,
    Tvbbe2,
    Tnbbe,
    Ebbe,
    Dtemp,
    Vers,
    Vref,
}

const COUNT: usize = 108;
const ELECTRICAL_K_OVER_Q: f64 = 1.380_662e-23 / 1.602_189e-19;

impl Coefficient {
    const fn index(self) -> usize {
        self as usize
    }
}

/// The complete VBIC nominal model-card/defaulted coefficients.
///
/// This is a compact immutable prepared-artifact boundary, not a solver
/// input layout. Temperature-independent values are copied without change.
#[derive(Clone, Copy, Debug)]
pub struct Nominal {
    values: [f64; COUNT],
}

#[derive(Clone, Copy, Debug)]
pub struct Prepared {
    values: [f64; COUNT],
    /// Temperature in kelvin including VBIC model local DTEMP.
    effective: Temperature,
}

impl Nominal {
    /// # Errors
    /// Rejects incomplete/nonphysical numeric model coefficients rather than
    /// constructing a partly valid temperature profile.
    pub fn new(values: &[f64; COUNT]) -> Result<Self, ModelError> {
        if !values.iter().all(|value| value.is_finite()) {
            return Err(ModelError::InvalidParameter("VBIC nominal coefficient"));
        }
        if values[Coefficient::Tnom.index()] <= -273.15 {
            return Err(ModelError::InvalidTemperature);
        }
        for (name, param) in [
            ("nf", Coefficient::Nf),
            ("nr", Coefficient::Nr),
            ("nei", Coefficient::Nei),
            ("nen", Coefficient::Nen),
            ("nci", Coefficient::Nci),
            ("ncn", Coefficient::Ncn),
            ("nfp", Coefficient::Nfp),
            ("ncip", Coefficient::Ncip),
            ("ncnp", Coefficient::Ncnp),
            ("nbbe", Coefficient::Nbbe),
            ("pe", Coefficient::Pe),
            ("pc", Coefficient::Pc),
            ("ps", Coefficient::Ps),
        ] {
            validate_positive(name, values[param.index()])?;
        }
        Ok(Self { values: *values })
    }

    #[must_use]
    pub const fn value(&self, parameter: Coefficient) -> f64 {
        self.values[parameter.index()]
    }

    /// Prepare all VBIC parameters for the ambient device temperature.
    /// Includes native model-local DTEMP. The returned effective kelvin
    /// must be used by every nonlinear kernel from this preparation.
    ///
    /// # Errors
    /// Returns invalid temperature/parameter on a nonphysical profile or
    /// numerical singularity (for example emission coefficient <= 0).
    pub fn prepare(&self, ambient: Temperature) -> Result<Prepared, ModelError> {
        let v = &self.values;
        let ti = v[Coefficient::Tnom.index()] + 273.15;
        let td = ambient.kelvin() + v[Coefficient::Dtemp.index()];
        let effective = Temperature::new(td)?;
        let rt = td / ti;
        let dt = td - ti;
        let vt = ELECTRICAL_K_OVER_Q * td;
        let mut p = *v;

        scale_resistive_coefficients(v, &mut p, rt);
        scale_saturation_currents(v, &mut p, rt, vt);
        scale_direct_coefficients(v, &mut p, dt);
        scale_potentials(v, &mut p, rt, vt)?;
        scale_capacitances(v, &mut p);
        // Temperature-adjusted epitaxial doping and collector drift scale.
        p[Coefficient::Gamm.index()] = v[Coefficient::Gamm.index()]
            * rt.powf(v[Coefficient::Xis.index()])
            * (v[Coefficient::Ea.index()] * (rt - 1.0) / vt).exp();
        p[Coefficient::Vo.index()] =
            v[Coefficient::Vo.index()] * rt.powf(v[Coefficient::Xvo.index()]);
        p[Coefficient::Ebbe.index()] =
            (-p[Coefficient::Vbbe.index()] / (p[Coefficient::Nbbe.index()] * vt)).exp();
        p[Coefficient::Tnom.index()] = ambient.kelvin() - 273.15 + p[Coefficient::Dtemp.index()];

        if !p.iter().all(|value| value.is_finite()) {
            return Err(ModelError::InvalidParameter("VBIC transformed coefficient"));
        }
        if [
            Coefficient::Pe,
            Coefficient::Pc,
            Coefficient::Ps,
            Coefficient::Nf,
            Coefficient::Nr,
            Coefficient::Nbbe,
        ]
        .iter()
        .any(|parameter| p[parameter.index()] <= 0.0)
        {
            return Err(ModelError::InvalidParameter("VBIC transformed denominator"));
        }
        Ok(Prepared {
            values: p,
            effective,
        })
    }
}

fn scale_resistive_coefficients(v: &[f64; COUNT], p: &mut [f64; COUNT], rt: f64) {
    // Eight temperature-scaled resistor and knee parameters.
    for (destination, exponent) in [
        (Coefficient::Ikf, Coefficient::Xikf),
        (Coefficient::Rcx, Coefficient::Xrcx),
        (Coefficient::Rci, Coefficient::Xrci),
        (Coefficient::Rbx, Coefficient::Xrbx),
        (Coefficient::Rbi, Coefficient::Xrbi),
        (Coefficient::Re, Coefficient::Xre),
        (Coefficient::Rs, Coefficient::Xrs),
        (Coefficient::Rbp, Coefficient::Xrbp),
    ] {
        p[destination.index()] = v[destination.index()] * rt.powf(v[exponent.index()]);
    }
}

fn scale_saturation_currents(v: &[f64; COUNT], p: &mut [f64; COUNT], rt: f64, vt: f64) {
    // The reference deliberately uses NOMINAL ideality coefficients for
    // saturation scaling, then applies TNF to NF/NR. This is not a typo.
    for (destination, exponent, activation, ideality) in [
        (
            Coefficient::Is,
            Coefficient::Xis,
            Coefficient::Ea,
            Coefficient::Nf,
        ),
        (
            Coefficient::Isrr,
            Coefficient::Xisr,
            Coefficient::Dear,
            Coefficient::Nr,
        ),
        (
            Coefficient::Isp,
            Coefficient::Xis,
            Coefficient::Eap,
            Coefficient::Nfp,
        ),
        (
            Coefficient::Ibei,
            Coefficient::Xii,
            Coefficient::Eaie,
            Coefficient::Nei,
        ),
        (
            Coefficient::Iben,
            Coefficient::Xin,
            Coefficient::Eane,
            Coefficient::Nen,
        ),
        (
            Coefficient::Ibci,
            Coefficient::Xii,
            Coefficient::Eaic,
            Coefficient::Nci,
        ),
        (
            Coefficient::Ibcn,
            Coefficient::Xin,
            Coefficient::Eanc,
            Coefficient::Ncn,
        ),
        (
            Coefficient::Ibeip,
            Coefficient::Xii,
            Coefficient::Eaic,
            Coefficient::Nci,
        ),
        (
            Coefficient::Ibenp,
            Coefficient::Xin,
            Coefficient::Eanc,
            Coefficient::Ncn,
        ),
        (
            Coefficient::Ibcip,
            Coefficient::Xii,
            Coefficient::Eais,
            Coefficient::Ncip,
        ),
        (
            Coefficient::Ibcnp,
            Coefficient::Xin,
            Coefficient::Eans,
            Coefficient::Ncnp,
        ),
    ] {
        p[destination.index()] = v[destination.index()]
            * (rt.powf(v[exponent.index()]) * (v[activation.index()] * (rt - 1.0) / vt).exp())
                .powf(1.0 / v[ideality.index()]);
    }
}

fn scale_direct_coefficients(v: &[f64; COUNT], p: &mut [f64; COUNT], dt: f64) {
    p[Coefficient::Nf.index()] =
        v[Coefficient::Nf.index()] * dt.mul_add(v[Coefficient::Tnf.index()], 1.0);
    p[Coefficient::Nr.index()] =
        v[Coefficient::Nr.index()] * dt.mul_add(v[Coefficient::Tnf.index()], 1.0);
    p[Coefficient::Avc2.index()] =
        v[Coefficient::Avc2.index()] * dt.mul_add(v[Coefficient::Tavc.index()], 1.0);
    p[Coefficient::Vbbe.index()] = v[Coefficient::Vbbe.index()]
        * dt.mul_add(
            dt.mul_add(
                v[Coefficient::Tvbbe2.index()],
                v[Coefficient::Tvbbe1.index()],
            ),
            1.0,
        );
    p[Coefficient::Nbbe.index()] =
        v[Coefficient::Nbbe.index()] * dt.mul_add(v[Coefficient::Tnbbe.index()], 1.0);
}

fn scale_potentials(
    v: &[f64; COUNT],
    p: &mut [f64; COUNT],
    rt: f64,
    vt: f64,
) -> Result<(), ModelError> {
    for (destination, activation) in [
        (Coefficient::Pe, Coefficient::Eaie),
        (Coefficient::Pc, Coefficient::Eaic),
        (Coefficient::Ps, Coefficient::Eais),
    ] {
        p[destination.index()] =
            transformed_potential(v[destination.index()], v[activation.index()], rt, vt)?;
    }

    Ok(())
}

fn scale_capacitances(v: &[f64; COUNT], p: &mut [f64; COUNT]) {
    for (destination, nominal_potential, actual_potential, grading) in [
        (
            Coefficient::Cje,
            Coefficient::Pe,
            Coefficient::Pe,
            Coefficient::Me,
        ),
        (
            Coefficient::Cjc,
            Coefficient::Pc,
            Coefficient::Pc,
            Coefficient::Mc,
        ),
        (
            Coefficient::Cjep,
            Coefficient::Pc,
            Coefficient::Pc,
            Coefficient::Mc,
        ),
        (
            Coefficient::Cjcp,
            Coefficient::Ps,
            Coefficient::Ps,
            Coefficient::Ms,
        ),
    ] {
        p[destination.index()] = v[destination.index()]
            * (v[nominal_potential.index()] / p[actual_potential.index()]).powf(v[grading.index()]);
    }
}
/// Reference VBIC potential mapping with numerically stable ln(2*sinh(x)).
fn transformed_potential(
    nominal: f64,
    energy: f64,
    ratio: f64,
    vt: f64,
) -> Result<f64, ModelError> {
    let x = 0.5 * nominal * ratio / vt;
    let log_sinh = x + (-(-2.0 * x).exp()).ln_1p();
    let nominal_intrinsic = 2.0 * vt / ratio * log_sinh;
    let actual_intrinsic = energy.mul_add(
        -(ratio - 1.0),
        nominal_intrinsic.mul_add(ratio, -3.0 * vt * ratio.ln()),
    );
    let root = (-actual_intrinsic / vt).exp().mul_add(4.0, 1.0).sqrt();
    let result = (2.0 * vt).mul_add(root.midpoint(1.0).ln(), actual_intrinsic);
    if !result.is_finite() || result <= 0.0 {
        return Err(ModelError::InvalidParameter("VBIC junction potential"));
    }
    Ok(result)
}

impl Prepared {
    #[must_use]
    pub const fn value(&self, parameter: Coefficient) -> f64 {
        self.values[parameter.index()]
    }

    #[must_use]
    pub const fn temperature(self) -> Temperature {
        self.effective
    }

    #[must_use]
    pub const fn transport_parameters(&self) -> transport::Parameters {
        transport::Parameters {
            is: self.value(Coefficient::Is),
            nf: self.value(Coefficient::Nf),
            nr: self.value(Coefficient::Nr),
            isrr: self.value(Coefficient::Isrr),
            vef: self.value(Coefficient::Vef),
            ver: self.value(Coefficient::Ver),
            ikf: self.value(Coefficient::Ikf),
            ikr: self.value(Coefficient::Ikr),
            nkf: self.value(Coefficient::Nkf),
            base_charge_form: if self.value(Coefficient::Qbm) < 0.5 {
                transport::BaseChargeForm::Coupled
            } else {
                transport::BaseChargeForm::Factorized
            },
        }
    }

    #[must_use]
    pub const fn depletion_parameters(&self) -> depletion::Parameters {
        depletion::Parameters {
            fc: self.value(Coefficient::Fc),
            pe: self.value(Coefficient::Pe),
            me: self.value(Coefficient::Me),
            aje: self.value(Coefficient::Aje),
            pc: self.value(Coefficient::Pc),
            mc: self.value(Coefficient::Mc),
            ajc: self.value(Coefficient::Ajc),
            vrt: self.value(Coefficient::Vrt),
            art: self.value(Coefficient::Art),
        }
    }

    #[must_use]
    pub const fn substrate_parameters(&self) -> depletion::SubstrateParameters {
        depletion::SubstrateParameters {
            ps: self.value(Coefficient::Ps),
            ms: self.value(Coefficient::Ms),
            ajs: self.value(Coefficient::Ajs),
        }
    }

    #[must_use]
    pub const fn parasitic_parameters(&self) -> parasitic::Parameters {
        parasitic::Parameters {
            isp: self.value(Coefficient::Isp),
            wsp: self.value(Coefficient::Wsp),
            nfp: self.value(Coefficient::Nfp),
            ikp: self.value(Coefficient::Ikp),
        }
    }

    #[must_use]
    pub const fn branch_parameters(&self) -> super::Parameters {
        super::Parameters {
            ibei: self.value(Coefficient::Ibei),
            iben: self.value(Coefficient::Iben),
            nei: self.value(Coefficient::Nei),
            nen: self.value(Coefficient::Nen),
            wbe: self.value(Coefficient::Wbe),
            ibci: self.value(Coefficient::Ibci),
            ibcn: self.value(Coefficient::Ibcn),
            nci: self.value(Coefficient::Nci),
            ncn: self.value(Coefficient::Ncn),
            ibeip: self.value(Coefficient::Ibeip),
            ibenp: self.value(Coefficient::Ibenp),
            ibcip: self.value(Coefficient::Ibcip),
            ibcnp: self.value(Coefficient::Ibcnp),
            ncip: self.value(Coefficient::Ncip),
            ncnp: self.value(Coefficient::Ncnp),
            vbbe: self.value(Coefficient::Vbbe),
            nbbe: self.value(Coefficient::Nbbe),
            ibbe: self.value(Coefficient::Ibbe),
        }
    }

    #[must_use]
    pub const fn avalanche_parameters(&self) -> avalanche::Parameters {
        avalanche::Parameters {
            avc1: self.value(Coefficient::Avc1),
            avc2: self.value(Coefficient::Avc2),
            pc: self.value(Coefficient::Pc),
            mc: self.value(Coefficient::Mc),
        }
    }

    #[must_use]
    pub const fn resistance_parameters(&self) -> resistance::Parameters {
        resistance::Parameters {
            rcx: self.value(Coefficient::Rcx),
            rci: self.value(Coefficient::Rci),
            vo: self.value(Coefficient::Vo),
            gamm: self.value(Coefficient::Gamm),
            hrcf: self.value(Coefficient::Hrcf),
            rbx: self.value(Coefficient::Rbx),
            rbi: self.value(Coefficient::Rbi),
            re: self.value(Coefficient::Re),
            rbp: self.value(Coefficient::Rbp),
            rs: self.value(Coefficient::Rs),
        }
    }

    #[must_use]
    pub const fn charge_parameters(&self) -> charge::Parameters {
        charge::Parameters {
            transport: self.transport_parameters(),
            parasitic: self.parasitic_parameters(),
            depletion: self.depletion_parameters(),
            substrate: self.substrate_parameters(),
            cje: self.value(Coefficient::Cje),
            cjc: self.value(Coefficient::Cjc),
            cjep: self.value(Coefficient::Cjep),
            cjcp: self.value(Coefficient::Cjcp),
            ccso: self.value(Coefficient::Ccso),
            cbeo: self.value(Coefficient::Cbeo),
            cbco: self.value(Coefficient::Cbco),
            qco: self.value(Coefficient::Qco),
            wbe: self.value(Coefficient::Wbe),
            tf: self.value(Coefficient::Tf),
            qtf: self.value(Coefficient::Qtf),
            xtf: self.value(Coefficient::Xtf),
            vtf: self.value(Coefficient::Vtf),
            itf: self.value(Coefficient::Itf),
            tr: self.value(Coefficient::Tr),
            gamm: self.value(Coefficient::Gamm),
        }
    }

    #[must_use]
    pub const fn noise_parameters(&self) -> noise::Parameters {
        noise::Parameters {
            kfn: self.value(Coefficient::Kfn),
            afn: self.value(Coefficient::Afn),
            bfn: self.value(Coefficient::Bfn),
        }
    }
}
