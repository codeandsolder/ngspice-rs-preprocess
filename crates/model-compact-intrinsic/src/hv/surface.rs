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

// Polynomial value at x and exact divided difference between x and x+delta.
// This Horner recurrence does not subtract nearly identical potentials.
fn polynomial_difference<const N: usize>(
    x: Differential<N>,
    delta: Differential<N>,
    coefficients: &[f64],
) -> (Differential<N>, Differential<N>) {
    coefficients.iter().rev().fold(
        (Differential::constant(0.), Differential::constant(0.)),
        |(value, difference), coefficient| {
            (value * x + *coefficient, difference * (x + delta) + value)
        },
    )
}

fn polynomial_charge_difference<const N: usize>(
    chi: Differential<N>,
    delta: Differential<N>,
    base_log: Differential<N>,
    quasi_fermi: Differential<N>,
) -> Differential<N> {
    let next = chi + delta;
    let (dep, dep_difference) = polynomial_difference(chi, delta, &DEPLETION);
    let depletion = chi * dep;
    let depletion_delta = delta * (dep + next * dep_difference);
    let (inv, inv_difference) = polynomial_difference(chi, delta, &INVERSION);
    let inversion = chi * chi * chi * inv;
    let chi_square = chi * chi;
    let cubic_divided_difference = next * (next + chi) + chi_square;
    let inversion_delta =
        delta * (cubic_divided_difference * inv + next * next * next * inv_difference);
    let negative = -quasi_fermi;
    let inversion_squared_delta = base_log.exp()
        * (negative.exp() * inversion_delta * (2. * inversion + inversion_delta)
            + negative.exp_m1() * inversion * inversion);
    depletion_delta * (2. * depletion + depletion_delta) + inversion_squared_delta
}

fn charge_squared_difference<const N: usize>(
    chi: Differential<N>,
    delta: Differential<N>,
    base_log: Differential<N>,
    quasi_fermi: Differential<N>,
) -> Differential<N> {
    if chi.value >= 5. {
        delta + (base_log + chi).exp() * (delta - quasi_fermi).exp_m1()
            - base_log.exp() * (-quasi_fermi).exp_m1()
    } else if chi.value + delta.value < 5. {
        polynomial_charge_difference(chi, delta, base_log, quasi_fermi)
    } else {
        // A branch transition is not a small perturbation of a single
        // polynomial. Use the actual native values on both sides of it.
        let source = charge_terms(chi, base_log);
        let drain = charge_terms(chi + delta, base_log - quasi_fermi);
        (drain.depletion - source.depletion) * (drain.depletion + source.depletion)
            + drain.inversion
            - source.inversion
    }
}
#[derive(Clone, Copy)]
struct DifferenceEquation<const N: usize> {
    chi: Differential<N>,
    beta: Differential<N>,
    factor: Differential<N>,
    base_log: Differential<N>,
    quasi_fermi: Differential<N>,
}
impl<const N: usize> DifferenceEquation<N> {
    fn evaluate(self, delta: Differential<N>) -> Differential<N> {
        let source = charge_terms(self.chi, self.base_log);
        let drain = charge_terms(self.chi + delta, self.base_log - self.quasi_fermi);
        let difference =
            charge_squared_difference(self.chi, delta, self.base_log, self.quasi_fermi)
                / (source.total + drain.total);
        delta / self.beta + self.factor * difference
    }
}
impl DifferenceEquation<4> {
    fn scalar(self, delta: f64) -> Differential<1> {
        DifferenceEquation {
            chi: Differential::constant(self.chi.value),
            beta: Differential::constant(self.beta.value),
            factor: Differential::constant(self.factor.value),
            base_log: Differential::constant(self.base_log.value),
            quasi_fermi: Differential::constant(self.quasi_fermi.value),
        }
        .evaluate(Differential::coordinate(delta, 0, 1.))
    }
    fn solve(self) -> Result<Jet, ModelError> {
        let initial = self.scalar(0.);
        let mut delta = 0.;
        if self.quasi_fermi.value > 0. && initial.value < 0. {
            let mut lower = 0.;
            let mut upper = self.quasi_fermi.value;
            delta = (-initial.value / initial.gradient[0]).min(upper);
            let tolerance = 8. * f64::EPSILON * initial.value.abs();
            let mut converged = false;
            for _ in 0..128 {
                let residual = self.scalar(delta);
                if residual.value.abs() <= tolerance {
                    converged = true;
                    break;
                }
                if residual.value > 0. {
                    upper = delta;
                } else {
                    lower = delta;
                }
                let candidate = delta - residual.value / residual.gradient[0];
                delta = if candidate > lower && candidate < upper {
                    candidate
                } else {
                    f64::midpoint(lower, upper)
                };
                if !(delta > lower && delta < upper) {
                    converged = self.scalar(delta).value.abs() <= 64. * tolerance;
                    break;
                }
            }
            if !converged {
                return Err(ModelError::InvalidPhysicalParameter(
                    "HiSIM-HV drain difference convergence",
                ));
            }
        }
        let residual = self.evaluate(Jet::constant(delta));
        let slope = self.scalar(delta).gradient[0];
        Jet {
            value: delta,
            gradient: residual.gradient.map(|partial| -partial / slope),
        }
        .checked("HiSIM-HV drain potential difference")
    }
}

/// Coupled drain endpoint and independently resolved surface difference.
/// The difference equation subtracts the source Poisson equation algebraically
/// before solving; it therefore preserves exponentially small drain response.
pub(super) fn evaluate_drain_from_source(
    inputs: &HiSimHvSurfaceInputs,
    source: &HiSimHvSurfacePoint,
) -> Result<(HiSimHvSurfacePoint, Jet), ModelError> {
    let beta = inputs.inverse_thermal_voltage;
    let base_log = inputs.minority_coefficient.ln() + beta * inputs.confined_body;
    let quasi_fermi = beta * inputs.quasi_fermi;
    let equation = DifferenceEquation {
        chi: source.chi,
        beta,
        factor: inputs.charge_factor,
        base_log,
        quasi_fermi,
    };
    let delta = equation.solve()?;
    let difference = (delta / beta).checked("HiSIM-HV drain surface difference")?;
    let chi = (source.chi + delta).checked("HiSIM-HV drain chi")?;
    let terms = charge_terms(chi, base_log - quasi_fermi);
    Ok((
        HiSimHvSurfacePoint {
            potential: (source.potential + difference).checked("HiSIM-HV drain surface")?,
            chi,
            depletion_root: terms.depletion.checked("HiSIM-HV drain depletion")?,
            inversion_squared: terms.inversion.checked("HiSIM-HV drain inversion")?,
            total_root: terms.total.checked("HiSIM-HV drain total root")?,
        },
        difference,
    ))
}
