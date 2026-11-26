//! Substrate-source/drain junction current and charge laws from `HiSIM2` 2.80.
//! Copyright (C) 2014 Hiroshima University & STARC; see package LICENSE.

use super::{
    PreparedInstance, PreparedModel, PreparedTemperature, positive, prepare_physical,
    prepare_temperature, real, slot,
};
use crate::{BoundParameters, ModelError, ParameterScope};

const EXP_THRESHOLD: f64 = 34.0;

/// Polarity-normalized voltages across the physical junctions (bulk minus diffusion).
/// For PMOS the physical voltage differences are multiplied by -1 before evaluation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct JunctionBias {
    /// Drain-side junction voltage.
    pub drain_v: f64,
    /// Source-side junction voltage.
    pub source_v: f64,
}

/// One junction's source-normalized current, stored charge and analytic derivatives.
///
/// Values include the device multiplier. The caller applies polarity to current
/// and charge when stamping physical terminals; derivatives are polarity invariant.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct JunctionResult {
    /// Current in amperes, including the supplied junction GMIN.
    pub current_a: f64,
    /// Current derivative with respect to normalized junction voltage.
    pub conductance_s: f64,
    /// Depletion charge in coulombs.
    pub charge_c: f64,
    /// Charge derivative with respect to normalized junction voltage.
    pub capacitance_f: f64,
}

/// Separate drain and source results, even in reverse channel operation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct JunctionResults {
    /// Drain-side junction.
    pub drain: JunctionResult,
    /// Source-side junction.
    pub source: JunctionResult,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct CurrentCoefficients {
    saturation: f64,
    secondary: f64,
    inverse_thermal: f64,
    transition_v: f64,
    transition_exp: f64,
    reverse: f64,
    reverse_exponent: f64,
    back: f64,
    back_exponent: f64,
    linear: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ChargeComponent {
    zero_cap: f64,
    potential: f64,
    grading: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Side {
    current: CurrentCoefficients,
    charge: [ChargeComponent; 3],
}

/// Voltage-independent junction quantities prepared once for a specific instance.
/// Evaluation does not perform model lookup, geometry preparation or exponent
/// calculations that depend only on temperature.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreparedJunctions {
    drain: Side,
    source: Side,
    multiplicity: f64,
}

const fn finite(v: f64) -> Result<f64, ModelError> {
    if v.is_finite() {
        Ok(v)
    } else {
        Err(ModelError::InvalidPhysicalParameter("HiSIM2 junction"))
    }
}

fn binned(i: &PreparedInstance, name: &str) -> Result<f64, ModelError> {
    let id = slot(ParameterScope::Model, name)?;
    i.binned_model_slot(id)
        .ok_or(ModelError::MissingDefaultDependency(id))
}

struct SaturationDensity {
    area: f64,
    perimeter: f64,
    area_secondary: f64,
    perimeter_secondary: f64,
    inverse_thermal: f64,
}

fn densities(
    m: &BoundParameters,
    i: &PreparedInstance,
    t: PreparedTemperature,
) -> Result<SaturationDensity, ModelError> {
    let nj = positive(binned(i, "nj")?, "nj")?;
    let njsw = positive(real(m, "njsw")?, "njsw")?;
    let band = t.activation_exponent();
    let scale = |xti: &str| -> Result<(f64, f64), ModelError> {
        let exponent = real(m, xti)?.mul_add(t.ratio.ln(), band);
        Ok((
            finite(binned(i, "js0")? * (exponent / nj).exp())?,
            finite(binned(i, "js0sw")? * (exponent / njsw).exp())?,
        ))
    };
    let (area, perimeter) = scale("xti")?;
    let (area_secondary, perimeter_secondary) = scale("xti2")?;
    Ok(SaturationDensity {
        area,
        perimeter,
        area_secondary,
        perimeter_secondary,
        inverse_thermal: t.beta / nj,
    })
}

fn current_coefficients(
    m: &BoundParameters,
    i: &PreparedInstance,
    t: PreparedTemperature,
    d: &SaturationDensity,
    area: f64,
    perimeter: f64,
) -> Result<CurrentCoefficients, ModelError> {
    let saturation = finite(area.mul_add(d.area, perimeter * d.perimeter))?;
    let secondary = finite(area.mul_add(d.area_secondary, perimeter * d.perimeter_secondary))?;
    let transition_v = finite(
        (binned(i, "vdiffj")? * t.ratio * t.ratio / (saturation + 1e-50)).ln_1p()
            / d.inverse_thermal,
    )?;
    let transition_exp = finite((transition_v * d.inverse_thermal).exp())?;
    Ok(CurrentCoefficients {
        saturation,
        secondary,
        inverse_thermal: d.inverse_thermal,
        transition_v,
        transition_exp,
        reverse: finite(real(m, "cisb")? * ((t.ratio - 1.0) * real(m, "ctemp")?).exp())?,
        reverse_exponent: finite(real(m, "cvb")? * d.inverse_thermal)?,
        back: binned(i, "cisbk")?,
        back_exponent: finite(real(m, "cvbk")? * d.inverse_thermal)?,
        linear: finite(real(m, "divx")? * secondary)?,
    })
}

fn component(
    m: &BoundParameters,
    dimension: f64,
    dt: f64,
    names: [&str; 4],
) -> Result<ChargeComponent, ModelError> {
    let [cap, temperature_coefficient, potential, grading] = names;
    Ok(ChargeComponent {
        zero_cap: finite(
            real(m, cap)? * dimension * real(m, temperature_coefficient)?.mul_add(dt, 1.0),
        )?,
        potential: positive(real(m, potential)?, "junction potential")?,
        grading: real(m, grading)?,
    })
}

fn charge_components(
    m: &BoundParameters,
    area: f64,
    perimeter: f64,
    width: f64,
    dt: f64,
    drain: bool,
) -> Result<[ChargeComponent; 3], ModelError> {
    let (tc_area, tc_wall, tc_gate) = if drain {
        ("tcjbd", "tcjbdsw", "tcjbdswg")
    } else {
        ("tcjbs", "tcjbssw", "tcjbsswg")
    };
    let gate = perimeter.min(width);
    let wall = (perimeter - width).max(0.0);
    Ok([
        component(m, area, dt, ["cj", tc_area, "pb", "mj"])?,
        component(m, wall, dt, ["cjsw", tc_wall, "pbsw", "mjsw"])?,
        component(m, gate, dt, ["cjswg", tc_gate, "pbswg", "mjswg"])?,
    ])
}

fn prepare_side(
    m: &BoundParameters,
    i: &PreparedInstance,
    t: PreparedTemperature,
    d: &SaturationDensity,
    width: f64,
    drain: bool,
) -> Result<Side, ModelError> {
    let (area_name, perimeter_name) = if drain { ("ad", "pd") } else { ("as", "ps") };
    let area = real(i.parameters(), area_name)?;
    let perimeter = real(i.parameters(), perimeter_name)?;
    let dt = i.evaluator_temperature_kelvin() - t.nominal_kelvin;
    Ok(Side {
        current: current_coefficients(m, i, t, d, area, perimeter)?,
        charge: charge_components(m, area, perimeter, width, dt, drain)?,
    })
}

/// Prepare the two independent source-derived junction laws.
///
/// # Errors
/// Inconsistent model/instance, invalid geometry, temperature or junction coefficients.
pub fn prepare_junctions(
    model: &PreparedModel,
    instance: &PreparedInstance,
) -> Result<PreparedJunctions, ModelError> {
    let t = prepare_temperature(model, instance)?;
    let width = prepare_physical(model, instance)?.geometry.total_width_m;
    let m = model.parameters();
    let d = densities(m, instance, t)?;
    Ok(PreparedJunctions {
        drain: prepare_side(m, instance, t, &d, width, true)?,
        source: prepare_side(m, instance, t, &d, width, false)?,
        multiplicity: real(instance.parameters(), "m")?,
    })
}

fn clipped_exp(argument: f64, clip: bool) -> Result<f64, ModelError> {
    if clip && argument < -EXP_THRESHOLD {
        Ok(0.0)
    } else {
        finite(argument.exp())
    }
}

fn current(
    c: CurrentCoefficients,
    v: f64,
    gmin: f64,
    source: bool,
) -> Result<(f64, f64), ModelError> {
    let (forward, conductance) = if v < c.transition_v {
        let x = v * c.inverse_thermal;
        let exp = clipped_exp(x, true)?;
        let increment = if x < -EXP_THRESHOLD { -1.0 } else { x.exp_m1() };
        (
            c.saturation * increment,
            c.saturation * exp * c.inverse_thermal,
        )
    } else {
        let slope = c.saturation * c.inverse_thermal * c.transition_exp;
        (
            slope.mul_add(v - c.transition_v, c.saturation * (c.transition_exp - 1.0)),
            slope,
        )
    };
    // The source side clips reverse exponential arguments at -34, unlike drain.
    let reverse = clipped_exp(-v * c.reverse_exponent, source)?;
    let back = clipped_exp(-v * c.back_exponent, source)?;
    let amplitude = c.secondary * c.reverse;
    let linear = c.linear + gmin;
    Ok((
        linear.mul_add(
            v,
            c.back
                .mul_add(back - 1.0, amplitude.mul_add(reverse - 1.0, forward)),
        ),
        linear
            + c.back.mul_add(
                -back * c.back_exponent,
                amplitude.mul_add(-reverse * c.reverse_exponent, conductance),
            ),
    ))
}

fn charge(c: ChargeComponent, v: f64) -> Result<(f64, f64), ModelError> {
    if v < 0.0 {
        if c.zero_cap <= 0.0 {
            return Ok((0.0, 0.0));
        }
        let power = 1.0 - c.grading;
        let log = (-v / c.potential).ln_1p();
        // Analytic integral, including its finite logarithmic grading=1 limit.
        let integral = if power == 0.0 {
            -log
        } else {
            -(power * log).exp_m1() / power
        };
        Ok((
            finite(c.potential * c.zero_cap * integral)?,
            finite(c.zero_cap * (-c.grading * log).exp())?,
        ))
    } else {
        let curvature = v * c.grading / c.potential;
        Ok((
            finite(v * c.zero_cap * 0.5_f64.mul_add(curvature, 1.0))?,
            finite(c.zero_cap * (1.0 + curvature))?,
        ))
    }
}

fn evaluate(
    side: Side,
    v: f64,
    gmin: f64,
    multiplicity: f64,
    source: bool,
) -> Result<JunctionResult, ModelError> {
    finite(v)?;
    let (current, conductance) = current(side.current, v, gmin, source)?;
    let mut q = 0.0;
    let mut cap = 0.0;
    for c in side.charge {
        let (dq, dc) = charge(c, v)?;
        q += dq;
        cap += dc;
    }
    Ok(JunctionResult {
        current_a: finite(current * multiplicity)?,
        conductance_s: finite(conductance * multiplicity)?,
        charge_c: finite(q * multiplicity)?,
        capacitance_f: finite(cap * multiplicity)?,
    })
}

impl PreparedJunctions {
    /// Evaluate both junctions at polarity-normalized voltages with analytic voltage derivatives.
    ///
    /// GMIN is the circuit's per-device minimum conductance; it is multiplied by M
    /// exactly as in the native junction evaluator.
    ///
    /// # Errors
    /// Non-finite voltages or results, or invalid GMIN.
    pub fn evaluate(&self, bias: JunctionBias, gmin: f64) -> Result<JunctionResults, ModelError> {
        if !gmin.is_finite() || gmin < 0.0 {
            return Err(ModelError::InvalidPhysicalParameter("junction gmin"));
        }
        Ok(JunctionResults {
            drain: evaluate(self.drain, bias.drain_v, gmin, self.multiplicity, false)?,
            source: evaluate(self.source, bias.source_v, gmin, self.multiplicity, true)?,
        })
    }
}
