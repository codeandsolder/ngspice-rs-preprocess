//! Complete, source-indexed VBIC 1.1.5 temperature preparation.
//!
//! Follows the ngspice VBIC temperature source on all 108 coefficients.
//! The index enum is a compile-time bridge to the public VBIC parameter
//! names, not a per-Newton model-string lookup. This does not apply ngspice
//! parser defaults, minimum resistance clamps, or instance geometry scaling:
//! those must be source-aware stages before device binding.

use super::{avalanche, charge, depletion, noise, parasitic, resistance, transport};
use crate::{ModelError, Temperature, hicum2::transport::jet::Jet, validate_positive};

/// All 108 VBIC temperature-preparation coefficients in source order.
///
/// Model-card defaults and source-aware parameter name resolution are
/// responsibilities of the upstream reader; model evaluation is type safe.
// Scalar names, native setup defaults and source slot order have one definition.
macro_rules! coefficients {
    ($($variant:ident => ($name:literal, $default:expr)),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        #[repr(u8)]
        pub enum Coefficient { $($variant),+ }

        impl Coefficient {
            /// Ordered source coefficient slots.
            pub const ALL: [Self; COUNT] = [$(Self::$variant),+];

            /// Canonical model-card keyword.
            #[must_use = "the keyword identifies a source model coefficient"]
            pub const fn name(self) -> &'static str {
                match self { $(Self::$variant => $name),+ }
            }

            /// Native ngspice default before explicit setters.
            #[must_use = "the default initializes an omitted model coefficient"]
            pub const fn source_default(self) -> f64 {
                match self { $(Self::$variant => $default),+ }
            }
        }
    };
}

coefficients! {
    Tnom => ("tnom", 27.0),
    Rcx => ("rcx", 0.0),
    Rci => ("rci", 0.1),
    Vo => ("vo", 0.0),
    Gamm => ("gamm", 0.0),
    Hrcf => ("hrcf", 1.0),
    Rbx => ("rbx", 0.0),
    Rbi => ("rbi", 0.1),
    Re => ("re", 0.0),
    Rs => ("rs", 0.0),
    Rbp => ("rbp", 0.1),
    Is => ("is", 1e-16),
    Nf => ("nf", 1.0),
    Nr => ("nr", 1.0),
    Fc => ("fc", 0.9),
    Cbeo => ("cbeo", 0.0),
    Cje => ("cje", 0.0),
    Pe => ("pe", 0.75),
    Me => ("me", 0.33),
    Aje => ("aje", -0.5),
    Cbco => ("cbco", 0.0),
    Cjc => ("cjc", 0.0),
    Qco => ("qco", 0.0),
    Cjep => ("cjep", 0.0),
    Pc => ("pc", 0.75),
    Mc => ("mc", 0.33),
    Ajc => ("ajc", -0.5),
    Cjcp => ("cjcp", 0.0),
    Ps => ("ps", 0.75),
    Ms => ("ms", 0.33),
    Ajs => ("ajs", -0.5),
    Ibei => ("ibei", 1e-18),
    Wbe => ("wbe", 1.0),
    Nei => ("nei", 1.0),
    Iben => ("iben", 0.0),
    Nen => ("nen", 2.0),
    Ibci => ("ibci", 1e-16),
    Nci => ("nci", 1.0),
    Ibcn => ("ibcn", 0.0),
    Ncn => ("ncn", 2.0),
    Avc1 => ("avc1", 0.0),
    Avc2 => ("avc2", 0.0),
    Isp => ("isp", 0.0),
    Wsp => ("wsp", 1.0),
    Nfp => ("nfp", 1.0),
    Ibeip => ("ibeip", 0.0),
    Ibenp => ("ibenp", 0.0),
    Ibcip => ("ibcip", 0.0),
    Ncip => ("ncip", 1.0),
    Ibcnp => ("ibcnp", 0.0),
    Ncnp => ("ncnp", 2.0),
    Vef => ("vef", 0.0),
    Ver => ("ver", 0.0),
    Ikf => ("ikf", 0.0),
    Ikr => ("ikr", 0.0),
    Ikp => ("ikp", 0.0),
    Tf => ("tf", 0.0),
    Qtf => ("qtf", 0.0),
    Xtf => ("xtf", 0.0),
    Vtf => ("vtf", 0.0),
    Itf => ("itf", 0.0),
    Tr => ("tr", 0.0),
    Td => ("td", 0.0),
    Kfn => ("kfn", 0.0),
    Afn => ("afn", 1.0),
    Bfn => ("bfn", 1.0),
    Xre => ("xre", 0.0),
    Xrbi => ("xrbi", 0.0),
    Xrci => ("xrci", 0.0),
    Xrs => ("xrs", 0.0),
    Xvo => ("xvo", 0.0),
    Ea => ("ea", 1.12),
    Eaie => ("eaie", 1.12),
    Eaic => ("eaic", 1.12),
    Eais => ("eais", 1.12),
    Eane => ("eane", 1.12),
    Eanc => ("eanc", 1.12),
    Eans => ("eans", 1.12),
    Xis => ("xis", 3.0),
    Xii => ("xii", 3.0),
    Xin => ("xin", 3.0),
    Tnf => ("tnf", 0.0),
    Tavc => ("tavc", 0.0),
    Rth => ("rth", 0.0),
    Cth => ("cth", 0.0),
    Vrt => ("vrt", 0.0),
    Art => ("art", 0.1),
    Ccso => ("ccso", 0.0),
    Qbm => ("qbm", 0.0),
    Nkf => ("nkf", 0.5),
    Xikf => ("xikf", 0.0),
    Xrcx => ("xrcx", 0.0),
    Xrbx => ("xrbx", 0.0),
    Xrbp => ("xrbp", 0.0),
    Isrr => ("isrr", 1.0),
    Xisr => ("xisr", 0.0),
    Dear => ("dear", 0.0),
    Eap => ("eap", 1.12),
    Vbbe => ("vbbe", 0.0),
    Nbbe => ("nbbe", 1.0),
    Ibbe => ("ibbe", 1e-06),
    Tvbbe1 => ("tvbbe1", 0.0),
    Tvbbe2 => ("tvbbe2", 0.0),
    Tnbbe => ("tnbbe", 0.0),
    Ebbe => ("ebbe", 0.0),
    Dtemp => ("dtemp", 0.0),
    Vers => ("vers", 1.2),
    Vref => ("vref", 0.0),
}

pub const COUNT: usize = 108;
const ELECTRICAL_K_OVER_Q: f64 = 1.380_662e-23 / 1.602_189e-19;

impl Coefficient {
    /// Resolve canonical names and documented aliases once at the source boundary.
    #[must_use = "the optional slot distinguishes unknown source keywords"]
    pub fn from_name(name: &str) -> Option<Self> {
        let alias = match name.to_ascii_lowercase().as_str() {
            "tref" => Some(Self::Tnom),
            "v0" => Some(Self::Vo),
            "cbe0" => Some(Self::Cbeo),
            "cbc0" => Some(Self::Cbco),
            "qc0" => Some(Self::Qco),
            "xv0" => Some(Self::Xvo),
            "xrc" => Some(Self::Xrcx),
            "xrb" => Some(Self::Xrbx),
            _ => None,
        };
        alias.or_else(|| {
            Self::ALL
                .into_iter()
                .find(|slot| slot.name().eq_ignore_ascii_case(name))
        })
    }

    #[must_use = "the index identifies the source coefficient slot"]
    pub const fn index(self) -> usize {
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
    slopes: [f64; COUNT],
    /// Temperature in kelvin including VBIC model local DTEMP.
    effective: Temperature,
}

impl Nominal {
    /// Native default coefficients before explicit setters.
    #[must_use = "the defaults form a nominal model"]
    pub fn source_defaults() -> Self {
        Self {
            values: Coefficient::ALL.map(Coefficient::source_default),
        }
    }

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

    #[must_use = "the coefficient value is returned without modifying the parameter set"]
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
        transform(
            &self.values,
            Temperature::new(ambient.kelvin() + self.value(Coefficient::Dtemp))?,
        )
    }
}

/// Apply the same temperature law for initial ambient preparation and the
/// native evaluator's subsequent temperature rise from that prepared baseline.
/// All transformations carry exact derivatives using the common first-order AD.
fn transform(v: &[f64; COUNT], effective: Temperature) -> Result<Prepared, ModelError> {
    let ti = v[Coefficient::Tnom.index()] + 273.15;
    let td = Jet::thermal_constant(effective.kelvin(), 1.0);
    let rt = td / ti;
    let dt = td - ti;
    let vt = td * ELECTRICAL_K_OVER_Q;
    let mut p = v.map(Jet::constant);

    scale_resistive_coefficients(v, &mut p, rt);
    scale_saturation_currents(v, &mut p, rt, vt);
    scale_direct_coefficients(v, &mut p, dt);
    scale_potentials(v, &mut p, rt, vt)?;
    scale_capacitances(v, &mut p);
    p[Coefficient::Gamm.index()] = rt.powf(v[Coefficient::Xis.index()])
        * ((rt - 1.0) / vt * v[Coefficient::Ea.index()]).exp()
        * v[Coefficient::Gamm.index()];
    p[Coefficient::Vo.index()] = rt.powf(v[Coefficient::Xvo.index()]) * v[Coefficient::Vo.index()];
    let breakdown = -p[Coefficient::Vbbe.index()];
    let emission_voltage = p[Coefficient::Nbbe.index()] * vt;
    // Preserve correctly rounded scalar division before the very sensitive
    // exponential; the AD quotient already provides the derivative.
    p[Coefficient::Ebbe.index()] = (breakdown / emission_voltage)
        .with_value(breakdown.value / emission_voltage.value)
        .exp();
    p[Coefficient::Tnom.index()] = td - 273.15;

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
    .any(|parameter| p[parameter.index()].value <= 0.0)
    {
        return Err(ModelError::InvalidParameter("VBIC transformed denominator"));
    }
    Ok(Prepared {
        values: p.map(|coefficient| coefficient.value),
        slopes: p.map(|coefficient| coefficient.temperature),
        effective,
    })
}

fn scale_resistive_coefficients(v: &[f64; COUNT], p: &mut [Jet; COUNT], rt: Jet) {
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
        p[destination.index()] = rt.powf(v[exponent.index()]) * v[destination.index()];
    }
}

fn scale_saturation_currents(v: &[f64; COUNT], p: &mut [Jet; COUNT], rt: Jet, vt: Jet) {
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
        p[destination.index()] = (rt.powf(v[exponent.index()])
            * ((rt - 1.0) / vt * v[activation.index()]).exp())
        .powf(1.0 / v[ideality.index()])
            * v[destination.index()];
    }
}

fn scale_direct_coefficients(v: &[f64; COUNT], p: &mut [Jet; COUNT], dt: Jet) {
    for (destination, coefficient) in [
        (Coefficient::Nf, Coefficient::Tnf),
        (Coefficient::Nr, Coefficient::Tnf),
        (Coefficient::Avc2, Coefficient::Tavc),
        (Coefficient::Nbbe, Coefficient::Tnbbe),
    ] {
        p[destination.index()] = (dt * v[coefficient.index()] + 1.0) * v[destination.index()];
    }
    p[Coefficient::Vbbe.index()] =
        (dt * (dt * v[Coefficient::Tvbbe2.index()] + v[Coefficient::Tvbbe1.index()]) + 1.0)
            * v[Coefficient::Vbbe.index()];
}

fn scale_potentials(
    v: &[f64; COUNT],
    p: &mut [Jet; COUNT],
    rt: Jet,
    vt: Jet,
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

fn scale_capacitances(v: &[f64; COUNT], p: &mut [Jet; COUNT]) {
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
        p[destination.index()] = (Jet::constant(v[nominal_potential.index()])
            / p[actual_potential.index()])
        .powf(v[grading.index()])
            * v[destination.index()];
    }
}
/// Reference VBIC potential mapping with numerically stable ln(2*sinh(x)).
fn transformed_potential(
    nominal: f64,
    energy: f64,
    ratio: Jet,
    vt: Jet,
) -> Result<Jet, ModelError> {
    let x = ratio / vt * (0.5 * nominal);
    let log_sinh = x + (-(-x * 2.0).exp()).ln_1p();
    let nominal_intrinsic = vt / ratio * log_sinh * 2.0;
    let actual_intrinsic =
        nominal_intrinsic * ratio - vt * ratio.ln() * 3.0 - (ratio - 1.0) * energy;
    let root = ((-actual_intrinsic / vt).exp() * 4.0 + 1.0).sqrt();
    let result = vt * 2.0 * ((root + 1.0) * 0.5).ln() + actual_intrinsic;
    if !result.is_finite() || result.value <= 0.0 {
        return Err(ModelError::InvalidParameter("VBIC junction potential"));
    }
    Ok(result)
}

impl Prepared {
    pub(super) const fn jet(&self, coefficient: Coefficient) -> Jet {
        Jet::thermal_constant(self.value(coefficient), self.slope(coefficient))
    }

    pub(super) fn voltage_jet(&self) -> Jet {
        Jet::thermal_constant(thermal_voltage(self.effective), ELECTRICAL_K_OVER_Q)
    }

    /// Native second-stage transform: the initial instance temperature becomes
    /// the new measurement reference and model DTEMP is not applied twice.
    /// # Errors
    /// Invalid effective temperature or transformed physical coefficients.
    pub fn at_rise(&self, rise: f64) -> Result<Self, ModelError> {
        transform(
            &self.values,
            Temperature::new(self.effective.kelvin() + rise)?,
        )
    }

    /// Exact derivative of the transformed coefficient with respect to the
    /// effective junction temperature, in coefficient units per kelvin.
    #[must_use = "the slope contributes to the electrothermal Jacobian"]
    pub const fn slope(&self, coefficient: Coefficient) -> f64 {
        self.slopes[coefficient.index()]
    }

    #[must_use = "the coefficient value is returned without modifying the parameter set"]
    pub const fn value(&self, parameter: Coefficient) -> f64 {
        self.values[parameter.index()]
    }

    #[must_use = "the effective temperature is required by subsequent physical laws"]
    pub const fn temperature(self) -> Temperature {
        self.effective
    }

    #[must_use = "the returned coefficients are inputs to the transport equations"]
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

    #[must_use = "the returned coefficients are inputs to the depletion equations"]
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

    #[must_use = "the returned coefficients are inputs to the substrate depletion equations"]
    pub const fn substrate_parameters(&self) -> depletion::SubstrateParameters {
        depletion::SubstrateParameters {
            ps: self.value(Coefficient::Ps),
            ms: self.value(Coefficient::Ms),
            ajs: self.value(Coefficient::Ajs),
        }
    }

    #[must_use = "the returned coefficients are inputs to the parasitic transistor equations"]
    pub const fn parasitic_parameters(&self) -> parasitic::Parameters {
        parasitic::Parameters {
            isp: self.value(Coefficient::Isp),
            wsp: self.value(Coefficient::Wsp),
            nfp: self.value(Coefficient::Nfp),
            ikp: self.value(Coefficient::Ikp),
        }
    }

    #[must_use = "the returned coefficients are inputs to the branch equations"]
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

    #[must_use = "the returned coefficients are inputs to avalanche current evaluation"]
    pub const fn avalanche_parameters(&self) -> avalanche::Parameters {
        avalanche::Parameters {
            avc1: self.value(Coefficient::Avc1),
            avc2: self.value(Coefficient::Avc2),
            pc: self.value(Coefficient::Pc),
            mc: self.value(Coefficient::Mc),
        }
    }

    #[must_use = "the returned coefficients are inputs to the resistance equations"]
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

    #[must_use = "the returned coefficients are inputs to charge evaluation"]
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

    #[must_use = "the returned coefficients are inputs to noise evaluation"]
    pub const fn noise_parameters(&self) -> noise::Parameters {
        noise::Parameters {
            kfn: self.value(Coefficient::Kfn),
            afn: self.value(Coefficient::Afn),
            bfn: self.value(Coefficient::Bfn),
        }
    }
}

pub(super) fn thermal_voltage(temperature: Temperature) -> f64 {
    ELECTRICAL_K_OVER_Q * temperature.kelvin()
}

pub(super) fn inverse_enabled(value: Jet) -> Jet {
    if value.value == 0.0 {
        Jet::constant(0.0)
    } else {
        Jet::constant(1.0) / value
    }
}
