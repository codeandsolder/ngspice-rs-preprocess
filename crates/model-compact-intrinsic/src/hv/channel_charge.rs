//! Normal-mode HiSIM-HV endpoint electrostatics and channel charge integrals.
//! The transport mobility law consumes Idd, not an approximated drain current.
use super::{
    HiSimHvIntrinsicScalar as Jet, HiSimHvSurfaceInputs, HiSimHvSurfacePoint,
    evaluate_hisim_hv_surface,
};
use crate::ModelError;

const EPS: f64 = 10. * f64::EPSILON;
const MIN_INVERSION: f64 = 1e-12;

/// The physical channel region selected by the native surface equations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HiSimHvChannelRegion {
    Accumulation,
    NoInversion,
    WeakInversion,
    Transition,
    StrongInversion,
}

/// Independent local material and electrostatic inputs to the normal channel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvChannelChargeInputs {
    pub effective_gate: Jet,
    pub confined_body: Jet,
    pub effective_drain: Jet,
    pub inverse_thermal_voltage: Jet,
    pub oxide_capacitance: Jet,
    pub poisson_coefficient: Jet,
    pub minority_coefficient: Jet,
}

/// Intrinsic endpoint quantities and the native unscaled drift/diffusion
/// integral, area charge densities and drain/source partition.
///
/// No overlap, junction, mobility, multiplicity or terminal polarity is applied here.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvChannelChargePoint {
    pub region: HiSimHvChannelRegion,
    pub source_surface: Jet,
    pub drain_surface: Jet,
    pub surface_difference: Jet,
    pub source_bulk_density: Jet,
    pub source_inversion_density: Jet,
    pub inversion_voltage: Jet,
    pub drift_diffusion_integral: Jet,
    pub bulk_density: Jet,
    pub inversion_density: Jet,
    pub drain_fraction: Jet,
    pub transition_weight: Jet,
}

#[derive(Clone, Copy)]
struct DepletionPowers {
    square: Jet,
    root: Jet,
    cube: Jet,
}
impl DepletionPowers {
    fn from_surface(point: &HiSimHvSurfacePoint) -> Self {
        let root = point.depletion_root;
        if point.chi.value < 5. {
            Self {
                square: root * root + EPS,
                root: root + EPS,
                cube: root * root * root + EPS,
            }
        } else {
            let square = point.chi - 1.;
            Self {
                square,
                root,
                cube: square * root,
            }
        }
    }
}

fn transition_weight(chi: Jet) -> Jet {
    if chi.value < 3. {
        return Jet::constant(0.);
    }
    if chi.value >= 5. {
        return Jet::constant(1.);
    }
    let x = 0.5 * (chi - 3.);
    x * x * x * (10. + x * (-15. + 6. * x))
}

fn nonnegative(value: Jet) -> Jet {
    if value.value < 0. {
        Jet::constant(0.)
    } else {
        value
    }
}

#[derive(Clone, Copy)]
struct IntegralFactors {
    f00: Jet,
    f10: Jet,
    f11: Jet,
    f10_delta: Jet,
}
fn integral_factors(
    source: &HiSimHvSurfacePoint,
    drain: &HiSimHvSurfacePoint,
    pds: Jet,
    beta: Jet,
) -> IntegralFactors {
    let xi = DepletionPowers::from_surface(source);
    let xil = DepletionPowers::from_surface(drain);
    let eta = beta * pds / xi.square;
    let root = (1. + eta).sqrt();
    // Native F10/F30 use ratios of differences of powers. Cancelling
    // (r-1) algebraically gives finite, accurate zero-drain limits.
    let r2 = root * root;
    let f00 = 1. / ((1. + root) * xi.root);
    let f10_delta = xi.root * eta * (2. * root + 1.) / (3. * (root + 1.) * (root + 1.));
    let f10 = xi.root + f10_delta;
    let f30 = (4. / 15.) / beta * xi.cube * (r2 * (r2 + root + 1.) + root + 1.) / (root + 1.);
    let f11 = source.potential * f10 + (2. / 3.) / beta * xil.cube - f30;
    IntegralFactors {
        f00,
        f10,
        f11,
        f10_delta,
    }
}

impl HiSimHvChannelChargePoint {
    fn validate(self) -> Result<Self, ModelError> {
        for value in [
            self.source_surface,
            self.drain_surface,
            self.surface_difference,
            self.source_bulk_density,
            self.source_inversion_density,
            self.inversion_voltage,
            self.drift_diffusion_integral,
            self.bulk_density,
            self.inversion_density,
            self.drain_fraction,
            self.transition_weight,
        ] {
            value.checked("HiSIM-HV channel charge integral")?;
        }
        Ok(self)
    }
}

fn integrate(
    inputs: &HiSimHvChannelChargeInputs,
    source: &HiSimHvSurfacePoint,
    drain: &HiSimHvSurfacePoint,
    mut point: HiSimHvChannelChargePoint,
) -> Result<HiSimHvChannelChargePoint, ModelError> {
    let beta = inputs.inverse_thermal_voltage;
    let cox = inputs.oxide_capacitance;
    let coefficient = inputs.poisson_coefficient;
    let pds = point.surface_difference;
    let factors = integral_factors(source, drain, pds, beta);
    let fdd = beta
        * (cox * (inputs.effective_gate + 1. / beta - source.potential - 0.5 * pds)
            + coefficient * (factors.f00 - factors.f10));
    point.drift_diffusion_integral = pds * fdd;
    if point.region == HiSimHvChannelRegion::WeakInversion {
        return point.validate();
    }
    let xi = DepletionPowers::from_surface(source);
    let numerator = coefficient
        * (coefficient * (0.5 - xi.square - 0.5 * beta * pds)
            + cox * ((inputs.effective_gate + 1. / beta) * factors.f10 - factors.f11));
    let bulk = beta * numerator / fdd;
    let achi = pds + 2. * coefficient / cox * factors.f10_delta;
    let clamped = super::smoothing::unit_ceiling_power4(achi / point.inversion_voltage);
    let alpha = 1. - clamped;
    let qinm = 1. + alpha * (1. + alpha);
    let denominator = 1. + alpha;
    let denominator = if denominator.value < EPS {
        Jet::constant(EPS)
    } else {
        denominator
    };
    let inversion = (2. / 3.) * point.source_inversion_density * qinm / denominator;
    // Factor Qdrat - 0.5 to preserve the exact zero-drain value
    // and its right-hand derivative even in weak inversion.
    let partition = 0.5 - 0.1 * clamped * (alpha * (alpha + 3.) + 1.) / (denominator * qinm);
    let partition = if partition.value > 0.5 {
        Jet::constant(0.5)
    } else {
        partition
    };
    let weight = point.transition_weight;
    if point.region == HiSimHvChannelRegion::Transition {
        point.bulk_density = nonnegative(weight * bulk + (1. - weight) * point.source_bulk_density);
        point.inversion_density =
            nonnegative(weight * inversion + (1. - weight) * point.source_inversion_density);
        point.drain_fraction = weight * partition + (1. - weight) * 0.5;
    } else {
        point.bulk_density = bulk;
        point.inversion_density = inversion;
        point.drain_fraction = partition;
    }
    point.validate()
}

/// Solve both normal-mode surface endpoints and integrate native D1/D2/D3
/// charge and drift/diffusion expressions, including the no-inversion cutoff.
///
/// At zero drain bias the endpoint values coincide but their Jacobians do not.
/// # Errors
/// Invalid physical inputs, nonfinite output or failed Poisson convergence.
pub fn evaluate_hisim_hv_channel_charge(
    inputs: &HiSimHvChannelChargeInputs,
) -> Result<HiSimHvChannelChargePoint, ModelError> {
    for value in [
        inputs.effective_drain,
        inputs.oxide_capacitance,
        inputs.poisson_coefficient,
    ] {
        value.checked("HiSIM-HV channel charge input")?;
    }
    if inputs.effective_drain.value < 0.
        || inputs.oxide_capacitance.value <= 0.
        || inputs.poisson_coefficient.value <= 0.
    {
        return Err(ModelError::InvalidPhysicalParameter(
            "HiSIM-HV channel charge input",
        ));
    }
    let surface = HiSimHvSurfaceInputs {
        effective_gate: inputs.effective_gate,
        confined_body: inputs.confined_body,
        quasi_fermi: Jet::constant(0.),
        inverse_thermal_voltage: inputs.inverse_thermal_voltage,
        charge_factor: inputs.poisson_coefficient / inputs.oxide_capacitance,
        minority_coefficient: inputs.minority_coefficient,
    };
    let source = evaluate_hisim_hv_surface(&surface)?;
    let xi = DepletionPowers::from_surface(&source);
    let bulk = inputs.poisson_coefficient * xi.root;
    let inversion =
        inputs.poisson_coefficient * source.inversion_squared / (source.total_root + xi.root);
    let inversion_voltage = inversion / inputs.oxide_capacitance;
    let region = if inversion_voltage.value <= MIN_INVERSION {
        HiSimHvChannelRegion::NoInversion
    } else if source.chi.value < 3. {
        HiSimHvChannelRegion::WeakInversion
    } else if source.chi.value < 5. {
        HiSimHvChannelRegion::Transition
    } else {
        HiSimHvChannelRegion::StrongInversion
    };
    let mut point = HiSimHvChannelChargePoint {
        region,
        source_surface: source.potential,
        drain_surface: source.potential,
        surface_difference: Jet::constant(0.),
        source_bulk_density: bulk,
        source_inversion_density: inversion,
        inversion_voltage,
        drift_diffusion_integral: Jet::constant(0.),
        bulk_density: bulk,
        inversion_density: inversion,
        drain_fraction: Jet::constant(0.5),
        transition_weight: transition_weight(source.chi),
    };
    if region == HiSimHvChannelRegion::NoInversion {
        point.inversion_density = Jet::constant(0.);
        return point.validate();
    }
    let (drain, difference) = super::surface::evaluate_drain_from_source(
        &HiSimHvSurfaceInputs {
            quasi_fermi: inputs.effective_drain,
            ..surface
        },
        &source,
    )?;
    point.drain_surface = drain.potential;
    point.surface_difference = difference;
    integrate(inputs, &source, &drain, point)
}
