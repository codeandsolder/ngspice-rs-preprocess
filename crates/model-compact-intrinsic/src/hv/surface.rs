//! Normal-mode HiSIM-HV depletion/inversion Poisson equation, common to
//! `hsmhveval.c` and `hsmhv2eval.c`. Accumulation and HV2 depletion-mode MOS
//! use different equations and must be dispatched by the channel evaluator.
use super::HiSimHvIntrinsicScalar as Jet;
use crate::{ModelError, differential::Differential};

/// Inputs to the source (`Ps0`) or drain (`Psl`) surface equation. Each
/// gradient is in normalized intrinsic Vgs, Vds, Vbs, kelvin order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvSurfaceInputs {
    /// Effective gate `Vgp`, including flatband, threshold and poly shifts.
    pub effective_gate: Jet,
    /// Confined body voltage `Vbscl`.
    pub confined_body: Jet,
    /// Quasi-Fermi voltage: zero for source, `Vdseff` for drain.
    pub quasi_fermi: Jet,
    /// Inverse thermal voltage `beta`.
    pub inverse_thermal_voltage: Jet,
    /// `fac1 = cnst0 / Cox`.
    pub charge_factor: Jet,
    /// Minority carrier coefficient `cnst1`.
    pub minority_coefficient: Jet,
}

/// Converged normal-mode surface potential and native charge factors.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvSurfacePoint {
    pub potential: Jet,
    pub chi: Jet,
    /// Bare depletion square root (`fb` or `sqrt(Chi - 1)`).
    pub depletion_root: Jet,
    /// Minority carrier squared term (`fs01` or `fsl1`).
    pub inversion_squared: Jet,
    /// Total Poisson charge factor (`fs02` or `fsl2`).
    pub total_root: Jet,
}

// Coefficients from the 5-degree native contact-at-Chi=5 approximation.
// First coefficient is sqrt(2)/2.
const DEPLETION: [f64; 5] = [
    std::f64::consts::FRAC_1_SQRT_2,
    -0.117_851_130_197_758,
    0.017_880_050_633_883_3,
    -0.001_637_301_627_791_91,
    6.369_649_188_663_52e-5,
];
const INVERSION: [f64; 3] = [
    0.296_931_548_557_71,
    -0.070_536_542_840_097_62,
    0.006_115_288_895_133_18,
];

fn polynomial<const N: usize>(x: Differential<N>, coefficients: &[f64]) -> Differential<N> {
    coefficients
        .iter()
        .rev()
        .fold(Differential::constant(0.), |value, coefficient| {
            value * x + *coefficient
        })
}
fn hypot<const N: usize>(left: Differential<N>, right: Differential<N>) -> Differential<N> {
    let value = left.value.hypot(right.value);
    if value == 0. {
        return Differential::default();
    }
    let gradient = std::array::from_fn(|axis| {
        (left.value / value).mul_add(
            left.gradient[axis],
            right.value / value * right.gradient[axis],
        )
    });
    Differential { value, gradient }
}
#[derive(Clone, Copy)]
struct ChargeTerms<const N: usize> {
    depletion: Differential<N>,
    inversion: Differential<N>,
    total: Differential<N>,
}
fn charge_terms<const N: usize>(chi: Differential<N>, base_log: Differential<N>) -> ChargeTerms<N> {
    if chi.value < 5. {
        let depletion_poly = polynomial(chi, &DEPLETION);
        let inversion_poly = polynomial(chi, &INVERSION);
        // Factor out Chi before taking the square root, so flatband has
        // the correct one-sided derivative rather than 0/0.
        let inversion_scaled = chi * chi * inversion_poly * (base_log * 0.5).exp();
        let depletion = chi * depletion_poly;
        let inversion_root = chi * inversion_scaled;
        ChargeTerms {
            depletion,
            inversion: inversion_root * inversion_root,
            total: chi * hypot(depletion_poly, inversion_scaled),
        }
    } else {
        let depletion = (chi - 1.).sqrt();
        // Algebraically cnst1*(exp(beta*(psi-qf))-exp(beta*(body-qf))).
        // Factor in log space before exponentiation, avoiding overflow in
        // exp(Chi) and cancellation of the two exponentials.
        let inversion_root = ((base_log + chi) * 0.5).exp() * (-(-chi).exp_m1()).sqrt();
        ChargeTerms {
            depletion,
            inversion: inversion_root * inversion_root,
            total: hypot(depletion, inversion_root),
        }
    }
}

fn root_equation(
    chi: f64,
    overdrive: f64,
    beta: f64,
    factor: f64,
    base_log: f64,
) -> Differential<1> {
    let chi = Differential::coordinate(chi, 0, 1.);
    overdrive - chi / beta - factor * charge_terms(chi, Differential::constant(base_log)).total
}

fn solve_chi(overdrive: f64, beta: f64, factor: f64, base_log: f64) -> Result<f64, ModelError> {
    if overdrive == 0. {
        return Ok(0.);
    }
    let mut lower = 0.;
    let mut upper = overdrive * beta;
    if !upper.is_finite() {
        return Err(ModelError::InvalidPhysicalParameter(
            "HiSIM-HV surface bracket",
        ));
    }
    // For Chi >= 5 the inversion term is at least
    // exp(base_log + Chi) * (1-exp(-5)). This gives an upper bound
    // even when a large gate bias would make the naive bracket enormous.
    let inversion_bound = (2_f64.mul_add(overdrive.ln() - factor.ln(), -base_log)
        - (-(-5_f64).exp()).ln_1p())
    .max(5.);
    if inversion_bound.is_finite() {
        upper = upper.min(inversion_bound);
    }
    // Newton from the flatband tangent, then safeguard each step with the
    // exact monotone bracket [0, beta*(Vgp-Vbscl)].
    let tangent = root_equation(0., overdrive, beta, factor, base_log);
    let mut chi = -tangent.value / tangent.gradient[0];
    if !(chi > lower && chi < upper) {
        chi = f64::midpoint(lower, upper);
    }
    for _ in 0..128 {
        let residual = root_equation(chi, overdrive, beta, factor, base_log);
        if residual.value.abs() <= 4e-15 * overdrive.max(1.) {
            return Ok(chi);
        }
        if residual.value > 0. {
            lower = chi;
        } else {
            upper = chi;
        }
        let next = chi - residual.value / residual.gradient[0];
        if next > lower && next < upper && next.is_finite() {
            chi = next;
        } else {
            chi = f64::midpoint(lower, upper);
        }
        if !(chi > lower && chi < upper) {
            let endpoint = root_equation(chi, overdrive, beta, factor, base_log);
            if endpoint.value.abs() <= 1e-12 * overdrive.max(1.) {
                return Ok(chi);
            }
            break;
        }
    }
    Err(ModelError::InvalidPhysicalParameter(
        "HiSIM-HV surface convergence",
    ))
}

/// Solve the exact source normal-mode Poisson relation in the depletion or
/// inversion regions.
///
/// Returns the implicit Jacobian of the converged root;
/// the initial guess and iteration count are not differentiated.
///
/// # Errors
/// Rejects invalid inputs, accumulation-region bias, nonfinite results, or a
/// root that does not converge. Accumulation has its own native analytical law.
pub fn evaluate_hisim_hv_surface(
    inputs: &HiSimHvSurfaceInputs,
) -> Result<HiSimHvSurfacePoint, ModelError> {
    let HiSimHvSurfaceInputs {
        effective_gate,
        confined_body,
        quasi_fermi,
        inverse_thermal_voltage: beta,
        charge_factor: factor,
        minority_coefficient: minority,
    } = *inputs;
    for input in [
        effective_gate,
        confined_body,
        quasi_fermi,
        beta,
        factor,
        minority,
    ] {
        input.checked("HiSIM-HV surface input")?;
    }
    if effective_gate.value < confined_body.value
        || beta.value <= 0.
        || factor.value <= 0.
        || minority.value <= 0.
        || quasi_fermi.value < 0.
    {
        return Err(ModelError::InvalidPhysicalParameter(
            "HiSIM-HV normal-mode surface input",
        ));
    }
    let overdrive = effective_gate - confined_body;
    let base_log = minority.ln() + beta * (confined_body - quasi_fermi);
    base_log.checked("HiSIM-HV minority logarithm")?;
    let root = solve_chi(overdrive.value, beta.value, factor.value, base_log.value)?;
    let fixed = Jet::constant(root);
    let residual = overdrive - fixed / beta - factor * charge_terms(fixed, base_log).total;
    let slope = root_equation(
        root,
        overdrive.value,
        beta.value,
        factor.value,
        base_log.value,
    )
    .gradient[0];
    let chi = Jet {
        value: root,
        gradient: residual.gradient.map(|partial| -partial / slope),
    }
    .checked("HiSIM-HV surface chi")?;
    let terms = charge_terms(chi, base_log);
    Ok(HiSimHvSurfacePoint {
        potential: (confined_body + chi / beta).checked("HiSIM-HV surface potential")?,
        chi,
        depletion_root: terms.depletion.checked("HiSIM-HV surface depletion")?,
        inversion_squared: terms.inversion.checked("HiSIM-HV surface inversion")?,
        total_root: terms.total.checked("HiSIM-HV surface total")?,
    })
}
