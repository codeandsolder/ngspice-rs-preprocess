//! HV2 CODEP=1 coupled finite-layer Poisson equations.
use super::HiSimHvIntrinsicScalar as Jet;
use crate::{ModelError, differential::Differential};

const Q: f64 = 1.602_191_8e-19;
const ESI: f64 = 1.034_943e-10;
const GATE_OFFSET: f64 = 1e8 * f64::EPSILON;
const CONVERGENCE: f64 = 4e-14;
const ERR: ModelError =
    ModelError::InvalidPhysicalParameter("HiSIM-HV2 coupled depletion Poisson solution");

/// Local gate, body, quasi-Fermi and temperature state for a depletion endpoint.
/// Gradients use the intrinsic G/D/B/T convention, while concentrations and
/// layer thickness are size-dependent constants.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvDepletionSurfaceInputs {
    pub effective_gate: Jet,
    pub confined_body: Jet,
    pub quasi_fermi: Jet,
    pub inverse_thermal_voltage: Jet,
    pub oxide_capacitance: Jet,
    pub intrinsic_carrier_density: Jet,
    pub depletion_doping: f64,
    pub substrate_doping: f64,
    pub layer_thickness_m: f64,
}

/// Converged coupled Poisson potentials before the native postsolve
/// accumulation-region smoothing, and signed physical sheet charge densities.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvDepletionSurfacePoint {
    pub surface: Jet,
    pub buried: Jet,
    pub junction: Jet,
    pub surface_charge_density: Jet,
    pub buried_charge_density: Jet,
    pub substrate_charge_density: Jet,
}

type Local = Differential<6>;
#[derive(Clone, Copy)]
struct State {
    gate: Local,
    body: Local,
    fermi: Local,
    beta: Local,
    oxide: Local,
    poisson: Local,
    minority: Local,
    base: Local,
    doping: f64,
    substrate: f64,
    thickness: f64,
    layer_potential: f64,
}
#[derive(Clone, Copy)]
struct Charges {
    junction: Local,
    surface: Local,
    buried: Local,
    substrate: Local,
}
fn promote(x: Jet) -> Local {
    let [g, d, b, t] = x.gradient;
    Local {
        value: x.value,
        gradient: [g, d, b, t, 0., 0.],
    }
}

// Native Fn_SU_CP/Fn_SL_CP. Evaluate the distance from the limit, not
// limit - delta + a rounded-to-delta power cap. The latter loses depletion
// widths when the input is far outside the soft limit.
fn distance<const N: usize>(ratio: Differential<N>, power: f64) -> Differential<N> {
    if ratio.value <= 1. {
        1. - ratio / (1. + ratio.positive_pow(power)).positive_pow(1. / power)
    } else {
        -(-(1. / ratio).positive_pow(power).ln_1p() / power).exp_m1()
    }
}
fn upper<const N: usize>(
    x: Differential<N>,
    limit: Differential<N>,
    delta: f64,
    power: f64,
) -> Differential<N> {
    if x.value <= limit.value - delta {
        x
    } else {
        limit - delta * distance((x - limit + delta) / delta, power)
    }
}
fn lower<const N: usize>(x: Differential<N>, delta: f64) -> Differential<N> {
    -upper(-x, Differential::constant(0.), delta, 4.)
}

fn exponential_remainder(x: Local) -> Local {
    let v = x.value;
    let value = if v.abs() < 0.01 {
        v * v
            * (0.5 + v * (1. / 6. + v * (1. / 24. + v * (1. / 120. + v * (1. / 720. + v / 5040.)))))
    } else {
        v.exp_m1() - v
    };
    x.map(value, v.exp_m1())
}

impl State {
    fn new(i: HiSimHvDepletionSurfaceInputs) -> Result<Self, ModelError> {
        if [
            i.depletion_doping,
            i.substrate_doping,
            i.layer_thickness_m,
            i.oxide_capacitance.value,
            i.inverse_thermal_voltage.value,
            i.intrinsic_carrier_density.value,
        ]
        .into_iter()
        .any(|v| !v.is_finite() || v <= 0.)
        {
            return Err(ERR);
        }
        let beta = promote(i.inverse_thermal_voltage);
        let ni = promote(i.intrinsic_carrier_density);
        let minority = ni / i.depletion_doping;
        let built_in = ((i.depletion_doping / ni).ln() + (i.substrate_doping / ni).ln()) / beta;
        let state = Self {
            gate: promote(i.effective_gate) + GATE_OFFSET,
            body: promote(i.confined_body),
            fermi: promote(i.quasi_fermi),
            beta,
            oxide: promote(i.oxide_capacitance),
            poisson: (2. * ESI * Q * i.depletion_doping / beta).sqrt(),
            minority: minority * minority,
            base: promote(i.confined_body) - built_in,
            doping: i.depletion_doping,
            substrate: i.substrate_doping,
            thickness: i.layer_thickness_m,
            layer_potential: Q * i.depletion_doping * i.layer_thickness_m.powi(2) / (2. * ESI),
        };
        if state.fermi.value <= state.base.value {
            return Err(ERR);
        }
        Ok(state)
    }
    fn charges(self, surface: Local, buried: Local) -> Charges {
        let junction =
            self.base + (buried - self.base) * (self.doping / (self.doping + self.substrate));
        let width = (2. * ESI / (Q * self.doping) * lower(buried - junction, 0.1)).sqrt();
        let capped = upper(width, Local::constant(self.thickness), 1e-8, 4.);
        let substrate_width = (2. * ESI / (Q * self.substrate) * (junction - self.base)).sqrt();
        let chi = self.beta * (surface - buried);
        let majority = exponential_remainder(chi) + 1e-15;
        let surface_charge = if chi.value >= 0. {
            -self.poisson * majority.sqrt()
        } else {
            let holes = self.minority * (-self.beta * (buried - self.body)).exp() * (-chi).exp_m1();
            self.poisson * (majority + holes).sqrt()
        };
        Charges {
            junction,
            surface: surface_charge,
            buried: Q * self.doping * capped,
            substrate: -Q * self.substrate * substrate_width,
        }
    }
    fn residuals(self, surface: Local, buried: Local) -> ([Local; 2], Charges) {
        let q = self.charges(surface, buried);
        let z = surface - q.junction + self.layer_potential;
        let reference = q.junction + z * z / (4. * self.layer_potential);
        let residual_b = buried - upper(reference, self.fermi, 0.12, 8.);
        let residual_s = self.gate - surface + (q.surface + q.buried + q.substrate) / self.oxide;
        ([residual_b, residual_s], q)
    }
    fn surface(self, buried: f64) -> Result<f64, ModelError> {
        // At fixed buried potential charge neutrality is strictly decreasing
        // in surface potential. Bracket by expanding only as necessary.
        let mut lo = self.gate.value.min(buried).min(self.base.value) - 1.;
        let mut hi = self.gate.value.max(buried) + 1.;
        let eval = |x| {
            self.residuals(Local::coordinate(x, 4, 1.), Local::constant(buried))
                .0[1]
        };
        for _ in 0..16 {
            if eval(lo).value >= 0. && eval(hi).value <= 0. {
                return solve(lo, hi, |x| {
                    let v = eval(x);
                    (-v.value, -v.gradient[4])
                });
            }
            lo -= hi - lo;
            hi += hi - lo;
        }
        Err(ERR)
    }
    fn solve(self) -> Result<HiSimHvDepletionSurfacePoint, ModelError> {
        // The buried potential is between the built-in junction floor and
        // the local quasi-Fermi ceiling. Solve the reduced residual with
        // the exact Schur derivative, keeping all trial states physical.
        let buried = solve(self.base.value, self.fermi.value, |b| {
            let Ok(s) = self.surface(b) else {
                return (f64::NAN, f64::NAN);
            };
            let ([rb, rs], _) =
                self.residuals(Local::coordinate(s, 4, 1.), Local::coordinate(b, 5, 1.));
            (
                rb.value,
                rb.gradient[5] - rb.gradient[4] * rs.gradient[5] / rs.gradient[4],
            )
        })?;
        let surface = self.surface(buried)?;
        self.finish(surface, buried)
    }
    fn finish(self, s: f64, b: f64) -> Result<HiSimHvDepletionSurfacePoint, ModelError> {
        let ([rb, rs], _) =
            self.residuals(Local::coordinate(s, 4, 1.), Local::coordinate(b, 5, 1.));
        if rb.value.abs() > 1e-9 || rs.value.abs() > 1e-7 {
            return Err(ERR);
        }
        let (a, c, d, e) = (
            rb.gradient[4],
            rb.gradient[5],
            rs.gradient[4],
            rs.gradient[5],
        );
        let determinant = a.mul_add(e, -c * d);
        let mut surface = Local::constant(s);
        let mut buried = Local::constant(b);
        for axis in 0..4 {
            surface.gradient[axis] =
                c.mul_add(rs.gradient[axis], -e * rb.gradient[axis]) / determinant;
            buried.gradient[axis] =
                d.mul_add(rb.gradient[axis], -a * rs.gradient[axis]) / determinant;
        }
        let q = self.charges(surface, buried);
        Ok(HiSimHvDepletionSurfacePoint {
            surface: demote(surface)?,
            buried: demote(buried)?,
            junction: demote(q.junction)?,
            surface_charge_density: demote(q.surface)?,
            buried_charge_density: demote(q.buried)?,
            substrate_charge_density: demote(q.substrate)?,
        })
    }
}
fn demote(x: Local) -> Result<Jet, ModelError> {
    let [g, d, b, t, _, _] = x.gradient;
    Jet {
        value: x.value,
        gradient: [g, d, b, t],
    }
    .checked("HiSIM-HV2 depletion endpoint")
}

// Safeguarded scalar Newton with a sign bracket. A nonfinite Newton slope
// selects the bracket midpoint; an invalid residual is a reported failure.
fn solve(
    mut lo: f64,
    mut hi: f64,
    mut f: impl FnMut(f64) -> (f64, f64),
) -> Result<f64, ModelError> {
    let mut x = f64::midpoint(lo, hi);
    for _ in 0..128 {
        let (residual, slope) = f(x);
        if residual.is_nan() {
            return Err(ERR);
        }
        if residual.abs() <= CONVERGENCE {
            return Ok(x);
        }
        if residual > 0. {
            hi = x;
        } else {
            lo = x;
        }
        if hi - lo <= CONVERGENCE * hi.abs().max(lo.abs()).max(1.) {
            return Ok(f64::midpoint(lo, hi));
        }
        let candidate = x - residual / slope;
        let margin = 0.05 * (hi - lo);
        x = if candidate > lo + margin && candidate < hi - margin {
            candidate
        } else {
            f64::midpoint(lo, hi)
        };
    }
    Err(ERR)
}

/// Solve the native coupled Poisson equations of the finite depletion layer.
/// Derivatives are obtained from the converged implicit two-by-two Jacobian.
/// # Errors
/// Invalid physical inputs or failure to resolve the coupled equations.
pub fn evaluate_hisim_hv_depletion_surface(
    inputs: HiSimHvDepletionSurfaceInputs,
) -> Result<HiSimHvDepletionSurfacePoint, ModelError> {
    State::new(inputs)?.solve()
}
